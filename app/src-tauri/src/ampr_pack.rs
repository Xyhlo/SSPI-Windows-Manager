//! Lizard: AMPR seekable asset packs, the compressed-asset format read by drakmor's ampr_emu
//! 0.4.2.1 pack-capable libSceAmpr (AMPRPAK4 index, AMPRDAT3 volumes of independent raw LZ4
//! blocks, AMPRCRC1 offline checksums). Ported from its tools/ampr_pack.py (pack tool 4.0).
//!
//! The game's own `ampr_emu.index` fixes the file IDs: the manifest holds one record per indexed
//! file, in index order, packed or loose. Without console traces the built-in profile is
//! deliberately conservative: game data is compressed in independent 64 KiB blocks, large archives
//! that barely compress stay loose, and executables, modules, `sce_sys`, fakelib, indexes and
//! media are never packed. Every packed block is decoded and CRC-checked before any original is
//! removed, and originals are only ever removed from the disposable staging copy.

use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const INDEX_NAME: &str = "ampr_assets.index";
const PACK_PATTERN: &str = "ampr_assets-{group}-lane{lane:02}-vol{volume:02}-{id:03}.pak";
const INDEX_HEADER: usize = 128;
const DATA_HEADER: u64 = 64;
const CRC_HEADER: usize = 48;
const FILE_RECORD: usize = 48;
const CHUNK_RECORD: usize = 12;
const PACK_RECORD: usize = 32;

const FILE_PACKED: u32 = 1;
const FILE_STORE_ONLY: u32 = 2;
const FILE_STREAMING: u32 = 4;
const FILE_HOT: u32 = 8;
const FILE_RANDOM: u32 = 16;
const CODEC_RAW: u8 = 0;
const CODEC_LZ4: u8 = 1;
const CHUNK_SHARED: u8 = 1;
const CHUNK_STREAMING: u8 = 2;
const CHUNK_PAGE_CONTAINED: u8 = 4;
const CHUNK_PAGE_ALIGNED: u8 = 8;
#[allow(dead_code)] // defined by the format; this packer writes unstriped volumes
const PACK_STRIPED: u32 = 1;
const PACK_IO_PAGE_LAYOUT: u32 = 2;
const CHUNK_ALIGNMENT: u64 = 64;
const IO_PAGE: u64 = 64 * 1024;

/* ------------------------------------------------------------------ the built-in profile */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action { Compress, Store, Loose }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Random comes from trace profiles; the built-in profile never selects it
enum Layout { Random, Mixed, Streaming }

#[derive(Clone, Debug)]
struct Rule {
    action: Action,
    include: &'static [&'static str],
    exclude: &'static [&'static str],
    block_shift: u8,
    group: &'static str,
    layout: Layout,
    hot: bool,
}

struct Group { name: &'static str, pack_count: usize, balanced: bool, max_pack_size: u64 }
const GROUPS: [Group; 2] = [
    Group { name: "assets", pack_count: 4, balanced: true, max_pack_size: 8 << 30 },
    Group { name: "bulk", pack_count: 4, balanced: false, max_pack_size: 16 << 30 },
];

// Never packed: the runtime, executables and anything the loader or console reads directly.
const NEVER: &[&str] = &[
    "eboot.bin", "*/eboot.bin", "*.prx", "*.sprx", "*.self", "*.elf", "*.exe", "*.dll",
    "sce_sys/*", "sce_module/*", "system/*", "mods/*", "save/*", "fakelib/*", "fakelib2/*",
    "ampr_emu.index", "ampr_assets.index", "ampr_assets.index.*", "ampr_assets-*.pak",
    "ampr_commands.bin", "apr_emu.log", "nptitle.dat", "param.sfo", "*/param.sfo",
];
// Already compressed media gain nothing and are often streamed through other paths.
const MEDIA: &[&str] = &[
    "*.bk2", "*.mp4", "*.m4v", "*.webm", "*.mov", "*.usm", "*.avi", "*.wem", "*.bnk", "*.ogg", "*.opus",
    "*.mp3", "*.at9", "*.xma", "*.fsb",
];
const ARCHIVES: &[&str] = &["*.pak", "*.ucas", "*.utoc", "*.bundle", "*.archive", "*.forge", "*.arc", "*.rpf", "*.big"];

fn rules() -> Vec<Rule> {
    vec![
        Rule { action: Action::Compress, include: &["*"], exclude: &[], block_shift: 16, group: "assets", layout: Layout::Mixed, hot: false },
        Rule { action: Action::Compress, include: ARCHIVES, exclude: &[], block_shift: 16, group: "bulk", layout: Layout::Mixed, hot: false },
        Rule { action: Action::Loose, include: MEDIA, exclude: &[], block_shift: 16, group: "assets", layout: Layout::Mixed, hot: false },
        Rule { action: Action::Loose, include: NEVER, exclude: &[], block_shift: 16, group: "assets", layout: Layout::Mixed, hot: false },
    ]
}
const LOOSE: Rule = Rule { action: Action::Loose, include: &[], exclude: &[], block_shift: 16, group: "assets", layout: Layout::Mixed, hot: false };

/// Python's fnmatch.fnmatchcase: `*` crosses `/`, `?` is one character, `[...]` a class.
fn fnmatch(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None, 0usize);
    let class = |at: usize, c: char| -> Option<(bool, usize)> {
        let mut i = at + 1;
        let negate = p.get(i).is_some_and(|&c| c == '!');
        if negate { i += 1; }
        let start = i;
        let mut matched = false;
        while i < p.len() && (p[i] != ']' || i == start) {
            if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' { if p[i] <= c && c <= p[i + 2] { matched = true; } i += 3; }
            else { if p[i] == c { matched = true; } i += 1; }
        }
        (i < p.len()).then_some((matched != negate, i + 1))
    };
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' { star = Some(pi); mark = ti; pi += 1; continue; }
        let step = if pi >= p.len() { None } else if p[pi] == '?' { Some(pi + 1) }
            else if p[pi] == '[' { match class(pi, t[ti]) { Some((true, next)) => Some(next), Some((false, _)) => None, None => (p[pi] == t[ti]).then_some(pi + 1) } }
            else { (p[pi] == t[ti]).then_some(pi + 1) };
        match step {
            Some(next) => { pi = next; ti += 1; }
            None => match star { Some(s) => { pi = s + 1; mark += 1; ti = mark; } None => return false },
        }
    }
    while pi < p.len() && p[pi] == '*' { pi += 1; }
    pi == p.len()
}
fn matches_any(patterns: &[&str], relative: &str) -> bool { patterns.iter().any(|p| fnmatch(p, relative)) }

fn select_rule(relative: &str) -> Rule {
    // Last matching rule wins, as upstream.
    rules().into_iter().filter(|r| matches_any(r.include, relative) && !matches_any(r.exclude, relative)).last().unwrap_or(LOOSE)
}

/* ------------------------------------------------------------------ AMPRIDX3 */

#[derive(Clone, Debug)]
pub(crate) struct IndexEntry { pub(crate) path: String, pub(crate) size: u64, pub(crate) mtime: i64 }

fn le32(b: &[u8], at: usize) -> u32 { u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) }
fn le64(b: &[u8], at: usize) -> u64 { u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) }

/// Reads `ampr_emu.index` records in file-ID order (ID = position + 1).
pub(crate) fn read_index(path: &Path) -> Result<Vec<IndexEntry>, String> {
    let data = fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if data.len() < 48 || &data[..8] != b"AMPRIDX3" || le32(&data, 8) != 3 || le32(&data, 12) != 24 {
        return Err("ampr_emu.index is not an AMPRIDX3 index.".into());
    }
    let count = le64(&data, 16) as usize;
    let path_bytes = le64(&data, 24) as usize;
    let paths = 48usize.checked_add(count.checked_mul(24).ok_or("ampr_emu.index is corrupt.")?).ok_or("ampr_emu.index is corrupt.")?;
    if count == 0 || count > 2_000_000 || paths.checked_add(path_bytes).is_none_or(|end| end > data.len()) { return Err("ampr_emu.index is truncated.".into()); }
    let table = &data[paths..paths + path_bytes];
    (0..count).map(|id| {
        let row = 48 + id * 24;
        let (at, len) = (le32(&data, row) as usize, le32(&data, row + 4) as usize);
        if at.checked_add(len).is_none_or(|end| end >= table.len()) || table[at + len] != 0 { return Err(format!("ampr_emu.index record {} is corrupt.", id + 1)); }
        let path = std::str::from_utf8(&table[at..at + len]).map_err(|_| format!("ampr_emu.index record {} is not UTF-8.", id + 1))?;
        if !path.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("/app0/")) || path.len() == 6 { return Err(format!("ampr_emu.index record {} is outside /app0.", id + 1)); }
        Ok(IndexEntry { path: path.to_string(), size: le64(&data, row + 8), mtime: le64(&data, row + 16) as i64 })
    }).collect()
}

fn fnv1a64(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for b in bytes { hash = (hash ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3); }
    hash.max(1)
}
fn fold(path: &str) -> impl Iterator<Item = u8> + '_ { path.bytes().map(|b| match b { b'\\' => b'/', b'A'..=b'Z' => b + 32, _ => b }) }

/* ------------------------------------------------------------------ records */

struct FileRecord { path_hash: u64, size: u64, mtime: i64, first_chunk: u32, chunk_count: u32, path_offset: u32, path_length: u32, flags: u32, block_shift: u8, class: u8 }
impl FileRecord {
    fn bytes(&self) -> [u8; FILE_RECORD] {
        let mut b = [0u8; FILE_RECORD];
        b[0..8].copy_from_slice(&self.path_hash.to_le_bytes());
        b[8..16].copy_from_slice(&self.size.to_le_bytes());
        b[16..24].copy_from_slice(&self.mtime.to_le_bytes());
        b[24..28].copy_from_slice(&self.first_chunk.to_le_bytes());
        b[28..32].copy_from_slice(&self.chunk_count.to_le_bytes());
        b[32..36].copy_from_slice(&self.path_offset.to_le_bytes());
        b[36..40].copy_from_slice(&self.path_length.to_le_bytes());
        b[40..44].copy_from_slice(&self.flags.to_le_bytes());
        b[44] = self.block_shift;
        b[45] = self.class;
        b
    }
}

#[derive(Clone, Copy)]
struct ChunkRecord { offset: u64, stored: u32, pack: u16, codec: u8, flags: u8 }
impl ChunkRecord {
    fn bytes(&self) -> [u8; CHUNK_RECORD] {
        let mut b = [0u8; CHUNK_RECORD];
        b[0..8].copy_from_slice(&(self.offset | (self.pack as u64) << 48).to_le_bytes());
        b[8..12].copy_from_slice(&((self.stored - 1) | (self.codec as u32) << 20 | (self.flags as u32) << 22).to_le_bytes());
        b
    }
}

struct PackRecord { payload_bytes: u64, file_size: u64, name_offset: u32, name_length: u32, flags: u32, io_page: u32 }
impl PackRecord {
    fn bytes(&self) -> [u8; PACK_RECORD] {
        let mut b = [0u8; PACK_RECORD];
        b[0..8].copy_from_slice(&self.payload_bytes.to_le_bytes());
        b[8..16].copy_from_slice(&self.file_size.to_le_bytes());
        b[16..20].copy_from_slice(&self.name_offset.to_le_bytes());
        b[20..24].copy_from_slice(&self.name_length.to_le_bytes());
        b[24..28].copy_from_slice(&self.flags.to_le_bytes());
        b[28..32].copy_from_slice(&self.io_page.to_le_bytes());
        b
    }
}

#[derive(Default)]
struct Strings { data: Vec<u8>, seen: HashMap<String, (u32, u32)> }
impl Strings {
    fn add(&mut self, value: &str) -> (u32, u32) {
        if let Some(&known) = self.seen.get(value) { return known; }
        let at = (self.data.len() as u32, value.len() as u32);
        self.data.extend_from_slice(value.as_bytes());
        self.data.push(0);
        self.seen.insert(value.to_string(), at);
        at
    }
}

fn crc32(bytes: &[u8]) -> u32 { crc32fast::hash(bytes) }
fn align_up(value: u64, to: u64) -> u64 { value.div_ceil(to) * to }

/* ------------------------------------------------------------------ pack volumes */

struct Volume {
    id: u16,
    name: String,
    path: PathBuf,
    out: io::BufWriter<fs::File>,
    position: u64,
    payload_offset: u64,
    flags: u32,
    max_size: u64,
}

impl Volume {
    fn create(root: &Path, id: u16, name: String, max_size: u64) -> Result<Self, String> {
        let path = root.join(&name);
        let file = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("Cannot create {name}: {e}"))?;
        let mut out = io::BufWriter::with_capacity(4 << 20, file);
        let payload_offset = align_up(DATA_HEADER, IO_PAGE);
        out.write_all(&vec![0u8; payload_offset as usize]).map_err(|e| format!("Cannot write {name}: {e}"))?;
        Ok(Volume { id, name, path, out, position: payload_offset, payload_offset, flags: PACK_IO_PAGE_LAYOUT, max_size })
    }
    fn placement(&self, stored: u64, layout: Layout, extent_start: bool) -> (u64, u8) {
        let mut position = self.position;
        if extent_start { position = align_up(position, IO_PAGE); }
        let offset = if layout == Layout::Streaming { align_up(position, CHUNK_ALIGNMENT) }
            else if stored >= IO_PAGE { align_up(position, IO_PAGE) }
            else {
                let offset = align_up(position, CHUNK_ALIGNMENT);
                let page_end = align_up(offset + 1, IO_PAGE);
                if offset + stored > page_end { align_up(offset, IO_PAGE) } else { offset }
            };
        let mut flags = 0;
        if stored <= IO_PAGE && offset / IO_PAGE == (offset + stored - 1) / IO_PAGE { flags |= CHUNK_PAGE_CONTAINED; }
        if offset % IO_PAGE == 0 { flags |= CHUNK_PAGE_ALIGNED; }
        (offset, flags)
    }
    fn fits(&self, stored: u64, layout: Layout, extent_start: bool) -> bool {
        let (offset, _) = self.placement(stored, layout, extent_start);
        self.max_size == 0 || align_up(offset + stored, IO_PAGE) <= self.max_size
    }
    fn write(&mut self, payload: &[u8], layout: Layout, extent_start: bool) -> Result<(u64, u8), String> {
        let (offset, flags) = self.placement(payload.len() as u64, layout, extent_start);
        let io = |e: io::Error| format!("Writing {} failed: {e}", self.name);
        if offset > self.position { self.out.write_all(&vec![0u8; (offset - self.position) as usize]).map_err(io)?; }
        self.out.write_all(payload).map_err(|e| format!("Writing {} failed: {e}", self.name))?;
        self.position = offset + payload.len() as u64;
        Ok((offset, flags))
    }
    fn final_size(&self) -> u64 { align_up(self.position, IO_PAGE) }
    fn finish(mut self, build_id: &[u8; 16]) -> Result<(), String> {
        let final_size = self.final_size();
        let io = |e: io::Error| format!("Finishing {} failed: {e}", self.name);
        self.out.write_all(&vec![0u8; (final_size - self.position) as usize]).map_err(io)?;
        let mut header = [0u8; DATA_HEADER as usize];
        header[..8].copy_from_slice(b"AMPRDAT3");
        header[8..12].copy_from_slice(&3u32.to_le_bytes());
        header[12..16].copy_from_slice(&(DATA_HEADER as u32).to_le_bytes());
        header[16..20].copy_from_slice(&(self.id as u32).to_le_bytes());
        header[20..24].copy_from_slice(&self.flags.to_le_bytes());
        header[24..40].copy_from_slice(build_id);
        header[40..48].copy_from_slice(&self.payload_offset.to_le_bytes());
        header[48..56].copy_from_slice(&(final_size - self.payload_offset).to_le_bytes());
        let sum = crc32(&header);
        header[56..60].copy_from_slice(&sum.to_le_bytes());
        let mut file = self.out.into_inner().map_err(|e| format!("Finishing {} failed: {e}", self.name))?;
        file.seek(SeekFrom::Start(0)).and_then(|_| file.write_all(&header)).and_then(|_| file.sync_all()).map_err(|e| format!("Finishing {} failed: {e}", self.name))?;
        Ok(())
    }
}

/* ------------------------------------------------------------------ compression */

struct Block { stored: Vec<u8>, codec: u8, raw_len: u32, raw_crc: u32, fingerprint: [u8; 32] }

const MIN_SAVINGS_BYTES: usize = 64;
const MIN_SAVINGS_RATIO: f64 = 0.01;
const IO_NEUTRAL_BYTES: usize = 8 * 1024;
const IO_NEUTRAL_RATIO: f64 = 0.125;
/// LZ4 HC level. Upstream defaults to 12; on real game archives level 9 stores within 0.1% of
/// level 12 at about four times the speed, and the runtime decodes any level the same way.
const LZ4_HC_LEVEL: i32 = 9;

fn compress_block(raw: &[u8], rule: &Rule) -> Result<Block, String> {
    let raw_crc = crc32(raw);
    let fingerprint: [u8; 32] = Sha256::digest(raw).into();
    let raw_block = |raw: &[u8]| Block { stored: raw.to_vec(), codec: CODEC_RAW, raw_len: raw.len() as u32, raw_crc, fingerprint };
    if rule.action == Action::Store { return Ok(raw_block(raw)); }
    let compressed = lz4::block::compress(raw, Some(lz4::block::CompressionMode::HIGHCOMPRESSION(LZ4_HC_LEVEL)), false).map_err(|e| format!("LZ4 compression failed: {e}"))?;
    let saved = raw.len().saturating_sub(compressed.len());
    let ratio = saved as f64 / raw.len().max(1) as f64;
    let (mut need_bytes, mut need_ratio) = (MIN_SAVINGS_BYTES, MIN_SAVINGS_RATIO);
    // A cold random read costs a whole I/O page; demand a real gain when compression saves none.
    if rule.layout != Layout::Streaming && compressed.len().div_ceil(IO_PAGE as usize) >= raw.len().div_ceil(IO_PAGE as usize) {
        need_bytes = need_bytes.max(IO_NEUTRAL_BYTES);
        need_ratio = need_ratio.max(IO_NEUTRAL_RATIO);
    }
    if compressed.len() >= raw.len() || saved < need_bytes || ratio < need_ratio { return Ok(raw_block(raw)); }
    Ok(Block { stored: compressed, codec: CODEC_LZ4, raw_len: raw.len() as u32, raw_crc, fingerprint })
}

/// Runs `job` for 0..count on all workers, keeping the results in order.
fn compress_each(count: usize, workers: usize, job: impl Fn(usize) -> Result<Block, String> + Sync) -> Result<Vec<Block>, String> {
    if count <= 1 || workers <= 1 { return (0..count).map(&job).collect(); }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<Option<Result<Block, String>>> = (0..count).map(|_| None).collect();
    let slots: Vec<std::sync::Mutex<&mut Option<Result<Block, String>>>> = results.iter_mut().map(std::sync::Mutex::new).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers.min(count) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= count { break; }
                **slots[i].lock().unwrap() = Some(job(i));
            });
        }
    });
    drop(slots);
    results.into_iter().map(|r| r.unwrap()).collect()
}

/// Compresses blocks on all workers, keeping their order.
fn compress_all(blocks: &[Vec<u8>], rule: &Rule, workers: usize) -> Result<Vec<Block>, String> {
    compress_each(blocks.len(), workers, |i| compress_block(&blocks[i], rule))
}

/// Positioned read that fills `buffer` unless the file ends first; returns the bytes read.
fn read_full_at(file: &fs::File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(file, &mut buffer[filled..], offset + filled as u64);
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, &mut buffer[filled..], offset + filled as u64);
        match n {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Reads and compresses one file's blocks on a producer thread, a batch ahead of the thread that
/// places them, so the cores keep compressing during the serial volume writes. Batches arrive in
/// file order, so the packs are identical to a serial build. Dropping it stops and joins the
/// producer, so the source file is closed before `pack` returns.
struct Batches {
    receiver: Option<std::sync::mpsc::Receiver<Result<Vec<Block>, String>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Batches {
    /// About 32 MiB of raw blocks per batch, and at least four blocks per worker.
    const BATCH_BYTES: u64 = 32 << 20;

    fn spawn(item: &Selected, block: u64, workers: usize) -> Self {
        let (source, relative, size, rule) = (item.source.clone(), item.relative.clone(), item.size, item.rule.clone());
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let produce = || -> Result<(), String> {
                let file = fs::File::open(&source).map_err(|e| format!("Cannot read {relative}: {e}"))?;
                let batch_bytes = (Self::BATCH_BYTES / block).max(workers as u64 * 4) * block;
                // Windows serializes synchronous reads on one handle, so small parallel reads
                // crawl. A reader thread fetches the next batch in one large read while this
                // thread compresses the current one.
                let (raw_sender, raws) = std::sync::mpsc::sync_channel::<Result<Vec<u8>, String>>(1);
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        let read = || -> Result<(), String> {
                            let mut offset = 0u64;
                            while offset < size {
                                let mut data = vec![0u8; batch_bytes.min(size - offset) as usize];
                                let got = read_full_at(&file, &mut data, offset).map_err(|e| format!("Cannot read {relative}: {e}"))?;
                                if got < data.len() { return Err(format!("{relative} got shorter while packing.")); }
                                offset += got as u64;
                                if raw_sender.send(Ok(data)).is_err() { return Ok(()); }
                            }
                            let mut probe = [0u8; 1];
                            if read_full_at(&file, &mut probe, size).map_err(|e| e.to_string())? != 0 { return Err(format!("{relative} grew while packing.")); }
                            Ok(())
                        };
                        if let Err(error) = read() { let _ = raw_sender.send(Err(error)); }
                        drop(raw_sender);
                    });
                    let raws = raws; // dropped on any early return, which stops the reader
                    for data in raws.iter() {
                        let data = data?;
                        let blocks: Vec<&[u8]> = data.chunks(block as usize).collect();
                        let compressed = compress_each(blocks.len(), workers, |i| compress_block(blocks[i], &rule));
                        if sender.send(compressed).is_err() { return Ok(()); }
                    }
                    Ok(())
                })
            };
            if let Err(error) = produce() { let _ = sender.send(Err(error)); }
        });
        Self { receiver: Some(receiver), worker: Some(worker) }
    }

    fn next(&mut self) -> Option<Result<Vec<Block>, String>> { self.receiver.as_ref()?.recv().ok() }
}

impl Drop for Batches {
    fn drop(&mut self) {
        self.receiver.take();
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

/* ------------------------------------------------------------------ planning */

const AUTO_LOOSE_MIN_SIZE: u64 = 64 << 20;
const AUTO_LOOSE_SAMPLE_BLOCKS: u64 = 32;
const AUTO_LOOSE_SAMPLE_BYTES: u64 = 16 << 20;
const AUTO_LOOSE_MIN_SAVINGS: f64 = 0.05;
const AUTO_LOOSE_MAX_RAW: f64 = 0.90;

struct Selected { id: usize, entry: IndexEntry, relative: String, source: PathBuf, rule: Rule, size: u64, mtime: i64, lane: usize }

fn sample_indices(total: u64, samples: u64) -> Vec<u64> {
    if total == 0 || samples == 0 { return Vec::new(); }
    if total <= samples { return (0..total).collect(); }
    if samples == 1 { return vec![total / 2]; }
    let mut out: Vec<u64> = (0..samples).map(|s| s * (total - 1) / (samples - 1)).collect();
    out.dedup();
    out
}

/// Large files that barely compress stay loose: packing them only adds runtime work.
fn auto_loose(item: &Selected, workers: usize) -> Result<bool, String> {
    if item.rule.action != Action::Compress || item.rule.hot || item.size < AUTO_LOOSE_MIN_SIZE { return Ok(false); }
    let block = 1u64 << item.rule.block_shift;
    let total = item.size.div_ceil(block);
    let indices = sample_indices(total, AUTO_LOOSE_SAMPLE_BLOCKS.min((AUTO_LOOSE_SAMPLE_BYTES / block).max(1)));
    let mut file = fs::File::open(&item.source).map_err(|e| format!("Cannot read {}: {e}", item.relative))?;
    let mut raws = Vec::with_capacity(indices.len());
    for &index in &indices {
        let offset = index * block;
        let mut raw = vec![0u8; block.min(item.size - offset) as usize];
        file.seek(SeekFrom::Start(offset)).and_then(|_| file.read_exact(&mut raw)).map_err(|e| format!("Cannot sample {}: {e}", item.relative))?;
        raws.push(raw);
    }
    let blocks = compress_all(&raws, &item.rule, workers)?;
    let sampled: usize = raws.iter().map(Vec::len).sum();
    let stored: usize = blocks.iter().map(|b| b.stored.len()).sum();
    let raw_blocks = blocks.iter().filter(|b| b.codec == CODEC_RAW).count();
    let savings = 1.0 - stored as f64 / sampled.max(1) as f64;
    Ok(savings < AUTO_LOOSE_MIN_SAVINGS || raw_blocks as f64 / blocks.len().max(1) as f64 >= AUTO_LOOSE_MAX_RAW)
}

fn assign_lanes(items: &mut [Selected]) {
    for group in &GROUPS {
        let mut members: Vec<usize> = (0..items.len()).filter(|&i| items[i].rule.action != Action::Loose && items[i].rule.group == group.name).collect();
        if group.balanced {
            members.sort_by(|&a, &b| items[b].size.cmp(&items[a].size).then_with(|| items[a].relative.to_lowercase().cmp(&items[b].relative.to_lowercase())).then(items[a].id.cmp(&items[b].id)));
            let mut loads = vec![0u64; group.pack_count];
            for i in members {
                let lane = (0..group.pack_count).min_by_key(|&l| (loads[l], l)).unwrap();
                items[i].lane = lane;
                loads[lane] += items[i].size;
            }
        } else {
            for i in members { items[i].lane = (fnv1a64(fold(&items[i].relative)) % group.pack_count as u64) as usize; }
        }
    }
}

/* ------------------------------------------------------------------ the build */

#[derive(Debug, Clone, Default)]
pub(crate) struct Outcome {
    pub(crate) files_total: usize,
    pub(crate) files_packed: usize,
    pub(crate) files_auto_loose: usize,
    pub(crate) packed_bytes: u64,
    pub(crate) stored_bytes: u64,
    pub(crate) chunks: usize,
    pub(crate) shared_chunks: usize,
    pub(crate) packs: usize,
    pub(crate) removed_bytes: u64,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Progress<'a> { pub(crate) stage: &'a str, pub(crate) done: u64, pub(crate) total: u64, pub(crate) current: &'a str }

fn format_name(group: &str, lane: usize, volume: usize, id: usize) -> String {
    PACK_PATTERN.replace("{group}", group).replace("{lane:02}", &format!("{lane:02}")).replace("{volume:02}", &format!("{volume:02}")).replace("{id:03}", &format!("{id:03}"))
}

/// Packs the game at `root` in place (a disposable staging copy), verifies every chunk, then
/// removes the packed originals. The offline checksum sidecar is written to `crc_out`.
pub(crate) fn pack(root: &Path, crc_out: &Path, workers: usize, control: &mut dyn FnMut(Progress) -> Result<(), String>) -> Result<Outcome, String> {
    if root.join(INDEX_NAME).exists() { return Err("This dump already contains Lizard asset packs.".into()); }
    let entries = read_index(&root.join("ampr_emu.index"))?;
    let mut outcome = Outcome { files_total: entries.len(), ..Outcome::default() };

    // Plan: which rule applies, and which files actually exist to be packed.
    let mut items = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let relative = entry.path[6..].replace('\\', "/");
        control(Progress { stage: "plan", done: i as u64, total: entries.len() as u64, current: &relative })?;
        let mut rule = select_rule(&relative);
        let source = relative.split('/').fold(root.to_path_buf(), |p, part| p.join(part));
        let mut size = entry.size;
        let mut mtime = entry.mtime;
        if rule.action != Action::Loose {
            match fs::symlink_metadata(&source) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                    if meta.len() != entry.size { return Err(format!("ampr_emu.index says {} is {} bytes; the file has {}.", relative, entry.size, meta.len())); }
                    size = meta.len();
                    mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(entry.mtime);
                }
                _ => { outcome.warnings.push(format!("{relative} is indexed but missing; left loose.")); rule = LOOSE; }
            }
        }
        items.push(Selected { id: i + 1, entry: entry.clone(), relative, source, rule, size, mtime, lane: 0 });
    }
    for i in 0..items.len() {
        if items[i].size > 0 && auto_loose(&items[i], workers)? {
            outcome.files_auto_loose += 1;
            items[i].rule = Rule { action: Action::Loose, ..items[i].rule.clone() };
        }
        if items[i].size == 0 && items[i].rule.action != Action::Loose { items[i].rule = Rule { action: Action::Loose, ..items[i].rule.clone() }; }
    }
    assign_lanes(&mut items);
    let packed_total: u64 = items.iter().filter(|i| i.rule.action != Action::Loose).map(|i| i.size).sum();

    let mut strings = Strings::default();
    let mut files = Vec::with_capacity(items.len());
    let mut chunks: Vec<ChunkRecord> = Vec::new();
    let mut crcs: Vec<u32> = Vec::new();
    let mut volumes: Vec<Volume> = Vec::new();
    let mut current: HashMap<(&'static str, usize), usize> = HashMap::new();
    let mut volume_numbers: HashMap<(&'static str, usize), usize> = HashMap::new();
    let mut dedupe: HashMap<(String, u8, u8, [u8; 32]), ChunkRecord> = HashMap::new();
    let class_of = |group: &str| { let mut names: Vec<&str> = GROUPS.iter().map(|g| g.name).collect(); names.sort(); names.iter().position(|&n| n == group).unwrap_or(0) as u8 };

    let result: Result<(), String> = (|| {
        let mut done = 0u64;
        for item in &items {
            let (path_offset, path_length) = strings.add(&item.entry.path);
            let path_hash = fnv1a64(fold(&item.entry.path));
            if item.rule.action == Action::Loose {
                files.push(FileRecord { path_hash, size: item.entry.size, mtime: item.entry.mtime, first_chunk: 0, chunk_count: 0, path_offset, path_length, flags: 0, block_shift: 0, class: 0 });
                continue;
            }
            let group = GROUPS.iter().find(|g| g.name == item.rule.group).unwrap();
            let mut flags = FILE_PACKED;
            if item.rule.action == Action::Store { flags |= FILE_STORE_ONLY; }
            match item.rule.layout { Layout::Streaming => flags |= FILE_STREAMING, Layout::Random => flags |= FILE_RANDOM, Layout::Mixed => {} }
            if item.rule.hot { flags |= FILE_HOT; }
            let block = 1u64 << item.rule.block_shift;
            let first_chunk = chunks.len() as u32;
            let mut index = 0usize;
            let mut batches = Batches::spawn(item, block, workers);
            while let Some(batch) = batches.next() {
                control(Progress { stage: "pack", done, total: packed_total, current: &item.relative })?;
                for compressed in batch? {
                    let lane = item.lane;
                    let domain = format!("{}:{lane}", item.rule.group);
                    let physical = if item.rule.layout == Layout::Streaming { 1 } else { 0 };
                    let key = (domain, physical, compressed.codec, compressed.fingerprint);
                    let dedupe_allowed = item.rule.layout != Layout::Streaming;
                    let record = if let Some(prior) = dedupe_allowed.then(|| dedupe.get(&key)).flatten() {
                        outcome.shared_chunks += 1;
                        ChunkRecord { flags: CHUNK_SHARED | (prior.flags & (CHUNK_PAGE_CONTAINED | CHUNK_PAGE_ALIGNED)) | if item.rule.layout == Layout::Streaming { CHUNK_STREAMING } else { 0 }, ..*prior }
                    } else {
                        let extent_start = item.rule.layout == Layout::Streaming && index == 0;
                        let stored = compressed.stored.len() as u64;
                        let slot = (group.name, lane);
                        let needs_new = match current.get(&slot) { Some(&v) => !volumes[v].fits(stored, item.rule.layout, extent_start), None => true };
                        if needs_new {
                            let id = volumes.len();
                            if id > 0xFFFF { return Err("Too many pack volumes.".into()); }
                            let number = volume_numbers.entry(slot).or_insert(0);
                            let volume = Volume::create(root, id as u16, format_name(group.name, lane, *number, id), group.max_pack_size)?;
                            *number += 1;
                            if !volume.fits(stored, item.rule.layout, true) { return Err("One block is larger than a pack volume.".into()); }
                            volumes.push(volume);
                            current.insert(slot, id);
                        }
                        let volume = &mut volumes[current[&slot]];
                        let (offset, placement) = volume.write(&compressed.stored, item.rule.layout, extent_start)?;
                        let flags = placement | if item.rule.layout == Layout::Streaming { CHUNK_STREAMING } else { 0 };
                        let safe = if stored <= IO_PAGE { placement & CHUNK_PAGE_CONTAINED != 0 } else { placement & CHUNK_PAGE_ALIGNED != 0 };
                        if item.rule.layout != Layout::Streaming && !safe { return Err("A chunk was not placed page-safely.".into()); }
                        let record = ChunkRecord { offset, stored: stored as u32, pack: volume.id, codec: compressed.codec, flags };
                        if dedupe_allowed { dedupe.insert(key, record); }
                        outcome.stored_bytes += stored;
                        record
                    };
                    chunks.push(record);
                    crcs.push(compressed.raw_crc);
                    done += compressed.raw_len as u64;
                    index += 1;
                }
            }
            drop(batches);
            if index as u64 != item.size.div_ceil(block) { return Err(format!("{} got shorter while packing.", item.relative)); }
            files.push(FileRecord { path_hash, size: item.size, mtime: item.mtime, first_chunk, chunk_count: index as u32, path_offset, path_length, flags, block_shift: item.rule.block_shift, class: class_of(item.rule.group) });
            outcome.files_packed += 1;
            outcome.packed_bytes += item.size;
        }
        Ok(())
    })();
    let cleanup = |volumes: &[PathBuf]| { for path in volumes { let _ = fs::remove_file(path); } };
    let volume_paths: Vec<PathBuf> = volumes.iter().map(|v| v.path.clone()).collect();
    if let Err(error) = result { drop(volumes); cleanup(&volume_paths); return Err(error); }

    // Names first, then a deterministic build ID over everything the runtime reads.
    control(Progress { stage: "finish", done: packed_total, total: packed_total, current: "" })?;
    let pack_records: Vec<PackRecord> = volumes.iter().map(|v| {
        let (name_offset, name_length) = strings.add(&v.name);
        PackRecord { payload_bytes: v.final_size() - v.payload_offset, file_size: v.final_size(), name_offset, name_length, flags: v.flags, io_page: IO_PAGE as u32 }
    }).collect();
    let crc_payload: Vec<u8> = crcs.iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut digest = Sha256::new();
    digest.update(b"AMPRPACK4\0sspi-lizard-default-v1");
    for part in [files.iter().flat_map(|f| f.bytes()).collect::<Vec<u8>>(), chunks.iter().flat_map(|c| c.bytes()).collect(), crc_payload.clone(),
                 pack_records.iter().flat_map(|p| p.bytes()).collect(), strings.data.clone()] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(&part);
    }
    let build_id: [u8; 16] = digest.finalize()[..16].try_into().unwrap();
    for volume in volumes { if let Err(e) = volume.finish(&build_id) { cleanup(&volume_paths); return Err(e); } }
    outcome.packs = pack_records.len();
    outcome.chunks = chunks.len();

    let manifest = manifest_bytes(&build_id, &files, &chunks, &pack_records, &strings.data);
    let finish = (|| -> Result<(), String> {
        fs::write(root.join(INDEX_NAME), &manifest).map_err(|e| format!("Cannot write {INDEX_NAME}: {e}"))?;
        let mut sidecar = crc_header(&build_id, crcs.len() as u64, crc32(&crc_payload)).to_vec();
        sidecar.extend_from_slice(&crc_payload);
        fs::write(crc_out, sidecar).map_err(|e| format!("Cannot write {}: {e}", crc_out.display()))?;
        verify(root, &manifest, &crcs, control)?;
        Ok(())
    })();
    if let Err(error) = finish { cleanup(&volume_paths); let _ = fs::remove_file(root.join(INDEX_NAME)); let _ = fs::remove_file(crc_out); return Err(error); }

    // Only now remove packed originals, and never anything the runtime or loader needs loose.
    for item in items.iter().filter(|i| i.rule.action != Action::Loose) {
        if matches_any(NEVER, &item.relative) { continue; }
        fs::remove_file(&item.source).map_err(|e| format!("Cannot remove packed original {}: {e}", item.relative))?;
        outcome.removed_bytes += item.size;
    }
    Ok(outcome)
}

fn manifest_bytes(build_id: &[u8; 16], files: &[FileRecord], chunks: &[ChunkRecord], packs: &[PackRecord], strings: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(files.len() * FILE_RECORD + chunks.len() * CHUNK_RECORD + packs.len() * PACK_RECORD + strings.len());
    for f in files { payload.extend_from_slice(&f.bytes()); }
    for c in chunks { payload.extend_from_slice(&c.bytes()); }
    for p in packs { payload.extend_from_slice(&p.bytes()); }
    payload.extend_from_slice(strings);
    let files_offset = INDEX_HEADER as u64;
    let chunks_offset = files_offset + (files.len() * FILE_RECORD) as u64;
    let packs_offset = chunks_offset + (chunks.len() * CHUNK_RECORD) as u64;
    let strings_offset = packs_offset + (packs.len() * PACK_RECORD) as u64;
    let mut h = [0u8; INDEX_HEADER];
    h[..8].copy_from_slice(b"AMPRPAK4");
    h[8..12].copy_from_slice(&4u32.to_le_bytes());
    h[12..16].copy_from_slice(&(INDEX_HEADER as u32).to_le_bytes());
    h[20..24].copy_from_slice(&0x0102_0304u32.to_le_bytes());
    h[24..40].copy_from_slice(build_id);
    h[40..48].copy_from_slice(&(files.len() as u64).to_le_bytes());
    h[48..56].copy_from_slice(&(chunks.len() as u64).to_le_bytes());
    h[56..60].copy_from_slice(&(packs.len() as u32).to_le_bytes());
    h[60..64].copy_from_slice(&(FILE_RECORD as u32).to_le_bytes());
    h[64..68].copy_from_slice(&(CHUNK_RECORD as u32).to_le_bytes());
    h[68..72].copy_from_slice(&(PACK_RECORD as u32).to_le_bytes());
    h[72..80].copy_from_slice(&files_offset.to_le_bytes());
    h[80..88].copy_from_slice(&chunks_offset.to_le_bytes());
    h[88..96].copy_from_slice(&packs_offset.to_le_bytes());
    h[96..104].copy_from_slice(&strings_offset.to_le_bytes());
    h[104..112].copy_from_slice(&(strings.len() as u64).to_le_bytes());
    h[112..116].copy_from_slice(&crc32(&payload).to_le_bytes());
    let header_crc = crc32(&h);
    h[116..120].copy_from_slice(&header_crc.to_le_bytes());
    let mut out = h.to_vec();
    out.extend_from_slice(&payload);
    out
}

fn crc_header(build_id: &[u8; 16], count: u64, payload_crc: u32) -> [u8; CRC_HEADER] {
    let mut h = [0u8; CRC_HEADER];
    h[..8].copy_from_slice(b"AMPRCRC1");
    h[8..12].copy_from_slice(&1u32.to_le_bytes());
    h[12..16].copy_from_slice(&(CRC_HEADER as u32).to_le_bytes());
    h[16..32].copy_from_slice(build_id);
    h[32..40].copy_from_slice(&count.to_le_bytes());
    h[40..44].copy_from_slice(&payload_crc.to_le_bytes());
    let sum = crc32(&h);
    h[44..48].copy_from_slice(&sum.to_le_bytes());
    h
}

/// Re-reads the manifest and every volume from disk, checks the format invariants upstream's
/// loader enforces, and decodes every physical chunk against its CRC.
pub(crate) fn verify(root: &Path, manifest: &[u8], crcs: &[u32], control: &mut dyn FnMut(Progress) -> Result<(), String>) -> Result<(), String> {
    let bad = |m: String| format!("Lizard verification failed: {m}");
    if manifest.len() < INDEX_HEADER || &manifest[..8] != b"AMPRPAK4" { return Err(bad("manifest header".into())); }
    let mut zeroed = manifest[..INDEX_HEADER].to_vec();
    zeroed[116..120].fill(0);
    if crc32(&zeroed) != le32(manifest, 116) || crc32(&manifest[INDEX_HEADER..]) != le32(manifest, 112) { return Err(bad("manifest CRC".into())); }
    let (file_count, chunk_count, pack_count) = (le64(manifest, 40) as usize, le64(manifest, 48) as usize, le32(manifest, 56) as usize);
    let (files_at, chunks_at, packs_at, strings_at) = (le64(manifest, 72) as usize, le64(manifest, 80) as usize, le64(manifest, 88) as usize, le64(manifest, 96) as usize);
    if chunk_count != crcs.len() { return Err(bad("chunk count".into())); }
    let build_id = &manifest[24..40];
    let strings = &manifest[strings_at..];
    let string = |at: u32, len: u32| -> Result<String, String> {
        let (at, len) = (at as usize, len as usize);
        if at + len >= strings.len() || strings[at + len] != 0 { return Err(bad("string table".into())); }
        String::from_utf8(strings[at..at + len].to_vec()).map_err(|_| bad("string table".into()))
    };
    let mut volumes = Vec::with_capacity(pack_count);
    let mut volume_paths = Vec::with_capacity(pack_count);
    for p in 0..pack_count {
        let r = &manifest[packs_at + p * PACK_RECORD..packs_at + (p + 1) * PACK_RECORD];
        let (payload, size, flags, page) = (le64(r, 0), le64(r, 8), le32(r, 24), le32(r, 28) as u64);
        let name = string(le32(r, 16), le32(r, 20))?;
        let mut file = fs::File::open(root.join(&name)).map_err(|e| bad(format!("{name}: {e}")))?;
        let mut header = [0u8; DATA_HEADER as usize];
        file.read_exact(&mut header).map_err(|e| bad(format!("{name}: {e}")))?;
        let mut zero = header;
        zero[56..60].fill(0);
        if &header[..8] != b"AMPRDAT3" || le32(&header, 16) != p as u32 || &header[24..40] != build_id || le32(&header, 20) != flags
            || crc32(&zero) != le32(&header, 56) || le64(&header, 40) + le64(&header, 48) != size || le64(&header, 48) != payload
            || file.metadata().map(|m| m.len()).unwrap_or(0) != size || flags & PACK_IO_PAGE_LAYOUT == 0 || size % page != 0 {
            return Err(bad(format!("{name} header")));
        }
        volumes.push((size - payload, size, page));
        volume_paths.push(root.join(&name));
    }
    // Structure is checked here in one pass; the data of every distinct chunk is decoded and
    // CRC-checked afterwards on all cores.
    let mut seen: HashMap<(u16, u64), u32> = HashMap::new();
    let mut checks: Vec<ChunkCheck> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for f in 0..file_count {
        let r = &manifest[files_at + f * FILE_RECORD..files_at + (f + 1) * FILE_RECORD];
        let path = string(le32(r, 32), le32(r, 36))?;
        if fnv1a64(fold(&path)) != le64(r, 0) { return Err(bad(format!("{path} hash"))); }
        let flags = le32(r, 40);
        if flags & FILE_PACKED == 0 { continue; }
        control(Progress { stage: "verify", done: 0, total: 0, current: &path[6..] })?;
        let (size, first, count, shift) = (le64(r, 8), le32(r, 24) as usize, le32(r, 28) as usize, r[44]);
        paths.push(path.clone());
        let block = 1u64 << shift;
        let mut total = 0u64;
        for local in 0..count {
            let c = &manifest[chunks_at + (first + local) * CHUNK_RECORD..chunks_at + (first + local + 1) * CHUNK_RECORD];
            let location = le64(c, 0);
            let descriptor = le32(c, 8);
            let (offset, pack) = (location & ((1 << 48) - 1), (location >> 48) as u16);
            let stored = (descriptor & 0xFFFFF) + 1;
            let codec = (descriptor >> 20 & 3) as u8;
            let chunk_flags = (descriptor >> 22 & 0xFF) as u8;
            let raw = block.min(size - total) as usize;
            let (payload_offset, pack_size, page) = volumes.get_mut(pack as usize).ok_or_else(|| bad(format!("{path} pack id")))?;
            if offset < *payload_offset || offset + stored as u64 > *pack_size || offset % CHUNK_ALIGNMENT != 0 { return Err(bad(format!("{path} chunk range"))); }
            let contained = chunk_flags & CHUNK_PAGE_CONTAINED != 0;
            let aligned = chunk_flags & CHUNK_PAGE_ALIGNED != 0;
            if (contained && offset / *page != (offset + stored as u64 - 1) / *page) || (aligned && offset % *page != 0) || (flags & FILE_STREAMING == 0 && !(contained || aligned)) {
                return Err(bad(format!("{path} chunk page layout")));
            }
            if (flags & FILE_STREAMING != 0) != (chunk_flags & CHUNK_STREAMING != 0) { return Err(bad(format!("{path} streaming flag"))); }
            let crc = crcs[first + local];
            if let Some(&prior) = seen.get(&(pack, offset)) {
                if prior != crc { return Err(bad(format!("{path} shared chunk conflict"))); }
            } else {
                if codec != CODEC_LZ4 && !(codec == CODEC_RAW && stored as usize == raw) { return Err(bad(format!("{path} codec"))); }
                checks.push(ChunkCheck { pack, offset, stored, raw: raw as u32, codec, crc, file: paths.len() - 1 });
                seen.insert((pack, offset), crc);
            }
            total += raw as u64;
        }
        if total != size { return Err(bad(format!("{path} size"))); }
    }
    check_chunk_data(&volume_paths, &mut checks, &paths, control).map_err(|e| if e == "cancelled" || e == "Cancelled" { e } else { bad(e) })
}

struct ChunkCheck { pack: u16, offset: u64, stored: u32, raw: u32, codec: u8, crc: u32, file: usize }

/// Decodes and CRC-checks every distinct chunk. Chunks are read in pack order in spans of up to
/// 8 MiB, each worker with its own file handles (Windows serializes reads on one handle).
fn check_chunk_data(volumes: &[PathBuf], checks: &mut [ChunkCheck], paths: &[String], control: &mut dyn FnMut(Progress) -> Result<(), String>) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    const SPAN: u64 = 8 << 20;
    checks.sort_by_key(|c| (c.pack, c.offset));
    let mut spans = Vec::new();
    let mut start = 0;
    for i in 1..=checks.len() {
        if i == checks.len() || checks[i].pack != checks[start].pack || checks[i].offset + checks[i].stored as u64 - checks[start].offset > SPAN {
            if start < i { spans.push(start..i); }
            start = i;
        }
    }
    let total: u64 = checks.iter().map(|c| c.stored as u64).sum();
    let (next, done, stop, finished) = (AtomicUsize::new(0), AtomicU64::new(0), AtomicBool::new(false), AtomicUsize::new(0));
    let failure = std::sync::Mutex::new(None::<String>);
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let checks = &*checks;
    let spawned = workers.min(spans.len());
    std::thread::scope(|scope| {
        for _ in 0..spawned {
            scope.spawn(|| {
                let mut files: HashMap<u16, fs::File> = HashMap::new();
                let mut buffer = Vec::new();
                let run = |files: &mut HashMap<u16, fs::File>, buffer: &mut Vec<u8>| -> Result<(), String> {
                    loop {
                        if stop.load(Ordering::Relaxed) { return Ok(()); }
                        let Some(span) = spans.get(next.fetch_add(1, Ordering::Relaxed)) else { return Ok(()) };
                        let (first, last) = (&checks[span.start], &checks[span.end - 1]);
                        let file = match files.entry(first.pack) {
                            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                            std::collections::hash_map::Entry::Vacant(e) => e.insert(fs::File::open(&volumes[first.pack as usize]).map_err(|err| format!("{}: {err}", paths[first.file]))?),
                        };
                        let length = (last.offset + last.stored as u64 - first.offset) as usize;
                        buffer.resize(length, 0);
                        if read_full_at(file, buffer, first.offset).map_err(|err| format!("{}: {err}", paths[first.file]))? != length {
                            return Err(format!("{} chunk range", paths[first.file]));
                        }
                        for check in &checks[span.clone()] {
                            let at = (check.offset - first.offset) as usize;
                            let data = &buffer[at..at + check.stored as usize];
                            let path = &paths[check.file];
                            let crc = if check.codec == CODEC_LZ4 {
                                let decoded = lz4::block::decompress(data, Some(check.raw as i32)).map_err(|_| format!("{path} LZ4 block"))?;
                                if decoded.len() != check.raw as usize { return Err(format!("{path} data CRC")); }
                                crc32(&decoded)
                            } else { crc32(data) };
                            if crc != check.crc { return Err(format!("{path} data CRC")); }
                        }
                        done.fetch_add(length as u64, Ordering::Relaxed);
                    }
                };
                if let Err(error) = run(&mut files, &mut buffer) {
                    stop.store(true, Ordering::Relaxed);
                    failure.lock().unwrap().get_or_insert(error);
                }
                finished.fetch_add(1, Ordering::Release);
            });
        }
        // Progress and cancellation stay on the calling thread.
        while finished.load(Ordering::Acquire) < spawned && !stop.load(Ordering::Relaxed) {
            if let Err(error) = control(Progress { stage: "verify", done: done.load(Ordering::Relaxed).min(total), total, current: "" }) {
                stop.store(true, Ordering::Relaxed);
                failure.lock().unwrap().get_or_insert(error);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
    if let Some(error) = failure.into_inner().unwrap() { return Err(error); }
    control(Progress { stage: "verify", done: total, total, current: "" })
}

#[cfg(test)]
mod tests;
