//! Write-once exFAT images for ShadowMount Plus (`.exfat`), built from a folder without mounting
//! anything or needing administrator rights.
//!
//! The volume starts at byte 0 (no partition table) with 512-byte sectors and 64 KiB clusters, the
//! geometry ShadowMount's LVD path is tuned for. Everything is planned before the first byte is
//! written: the allocation bitmap, the up-case table and every directory come first, then each
//! file's data as one contiguous run with NoFatChain set (as Windows and exfat-fuse write them).
//! Directories, the bitmap and the up-case table use FAT chains. Output is deterministic for the
//! same source tree: entries are sorted, and timestamps come from the source files.
//! Structures follow Microsoft's exFAT specification; `verify` reopens the finished image.

mod upcase_table;
pub(crate) mod verify;

use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) const SECTOR: u64 = 512;
pub(crate) const CLUSTER: u64 = 64 * 1024;
pub(crate) const SECTOR_SHIFT: u8 = 9;
pub(crate) const CLUSTER_SHIFT: u8 = 7; // 128 sectors per cluster
pub(crate) const FAT_OFFSET_SECTORS: u32 = 2048; // 1 MiB
const ALIGN_SECTORS: u64 = 2048;
const ENTRY: usize = 32;
const COPY_BUFFER: usize = 8 * 1024 * 1024;

pub(crate) const ATTR_DIRECTORY: u16 = 0x10;
pub(crate) const ATTR_ARCHIVE: u16 = 0x20;
pub(crate) const FLAG_ALLOCATION_POSSIBLE: u8 = 0x01;
pub(crate) const FLAG_NO_FAT_CHAIN: u8 = 0x02;

/* ------------------------------------------------------------------ up-case table and checksums */

/// The table exactly as stored on disk (little-endian UTF-16, compressed).
pub(crate) fn upcase_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| upcase_table::UPCASE_COMPRESSED.iter().flat_map(|w| w.to_le_bytes()).collect())
}

/// The table expanded to one mapping per UTF-16 code unit.
pub(crate) fn upcase_map() -> &'static [u16] {
    static MAP: OnceLock<Vec<u16>> = OnceLock::new();
    MAP.get_or_init(|| expand_upcase(&upcase_table::UPCASE_COMPRESSED).expect("the built-in up-case table is complete"))
}

pub(crate) fn expand_upcase(words: &[u16]) -> Option<Vec<u16>> {
    let mut map = Vec::with_capacity(0x10000);
    let mut i = 0;
    while i < words.len() {
        if words[i] == 0xFFFF && i + 1 < words.len() {
            let run = words[i + 1] as usize;
            if map.len() + run > 0x10000 { return None; }
            let start = map.len();
            map.extend((start..start + run).map(|c| c as u16));
            i += 2;
        } else {
            map.push(words[i]);
            i += 1;
        }
    }
    // A table may end early; the remaining code units map to themselves.
    while map.len() < 0x10000 { let c = map.len() as u16; map.push(c); }
    (map.len() == 0x10000).then_some(map)
}

/// The 32-bit rotate-and-add checksum used by the boot region and the up-case table.
pub(crate) fn checksum32(bytes: &[u8], skip: impl Fn(usize) -> bool) -> u32 {
    bytes.iter().enumerate().fold(0u32, |sum, (i, &b)| if skip(i) { sum } else { sum.rotate_right(1).wrapping_add(b as u32) })
}

pub(crate) fn boot_checksum(first_eleven_sectors: &[u8]) -> u32 {
    checksum32(first_eleven_sectors, |i| i == 106 || i == 107 || i == 112)
}

/// SetChecksum over a whole entry set, skipping its own field (bytes 2 and 3).
pub(crate) fn set_checksum(set: &[u8]) -> u16 {
    set.iter().enumerate().fold(0u16, |sum, (i, &b)| if i == 2 || i == 3 { sum } else { sum.rotate_right(1).wrapping_add(b as u16) })
}

pub(crate) fn name_hash(name: &[u16]) -> u16 {
    let map = upcase_map();
    name.iter().fold(0u16, |sum, &unit| {
        let [lo, hi] = map[unit as usize].to_le_bytes();
        sum.rotate_right(1).wrapping_add(lo as u16).rotate_right(1).wrapping_add(hi as u16)
    })
}

pub(crate) fn upcased(name: &[u16]) -> Vec<u16> {
    let map = upcase_map();
    name.iter().map(|&u| map[u as usize]).collect()
}

/* ------------------------------------------------------------------ timestamps */

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// exFAT timestamp (UTC), plus its 10 ms increment (0..199).
pub(crate) fn exfat_time(time: SystemTime) -> (u32, u8) {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() as i64;
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    let (year, month, day, rem, millis) = if year < 1980 { (1980, 1, 1, 0, 0) }
        else if year > 2107 { (2107, 12, 31, 86_399, 990) }
        else { (year, month, day, rem, since.subsec_millis()) };
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let stamp = ((year - 1980) as u32) << 25 | month << 21 | day << 16 | (hour as u32) << 11 | (minute as u32) << 5 | (second / 2) as u32;
    (stamp, ((second % 2) * 100 + i64::from(millis / 10)) as u8)
}
const UTC_OFFSET: u8 = 0x80; // OffsetValid, +00:00

/* ------------------------------------------------------------------ the plan */

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) name: Vec<u16>,
    pub(crate) path: String, // image path, "/" separated, "" for the root
    pub(crate) source: PathBuf,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
    pub(crate) modified: SystemTime,
    pub(crate) children: Vec<usize>,
    pub(crate) first_cluster: u32,
    pub(crate) clusters: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub(crate) nodes: Vec<Node>,
    pub(crate) label: Vec<u16>,
    pub(crate) cluster_count: u32,
    pub(crate) fat_length: u32,
    pub(crate) heap_offset: u32,
    pub(crate) bitmap_first: u32,
    pub(crate) bitmap_bytes: u64,
    pub(crate) upcase_first: u32,
    pub(crate) used_clusters: u64,
    pub(crate) serial: u32,
    pub(crate) payload_bytes: u64,
    pub(crate) files: u64,
    pub(crate) directories: u64,
    pub(crate) image_bytes: u64,
}

impl Plan {
    pub(crate) fn root(&self) -> &Node { &self.nodes[0] }
    /// Files in the order their data is written.
    pub(crate) fn file_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.nodes.len()).filter(|&i| !self.nodes[i].is_dir && self.nodes[i].size > 0).collect();
        order.sort_by_key(|&i| self.nodes[i].first_cluster);
        order
    }
}

fn ceil_div(value: u64, by: u64) -> u64 { value.div_ceil(by) }
fn align_up(value: u64, to: u64) -> u64 { value.div_ceil(to) * to }

/// Names Windows allows can still be invalid on exFAT; check the rules explicitly.
pub(crate) fn valid_name(name: &[u16]) -> bool {
    !name.is_empty() && name.len() <= 255
        && name.iter().all(|&u| u >= 0x20 && !matches!(u, 0x22 | 0x2A | 0x2F | 0x3A | 0x3C | 0x3E | 0x3F | 0x5C | 0x7C))
        && name != [0x2E] && name != [0x2E, 0x2E]
}

fn entry_slots(name_len: usize) -> u64 { 2 + ceil_div(name_len as u64, 15) }

fn scan(source: &Path, nodes: &mut Vec<Node>, index: usize, depth: usize) -> Result<(), String> {
    if depth > 64 { return Err(format!("{} is nested too deeply.", source.display())); }
    let mut entries = Vec::new();
    for entry in fs::read_dir(source).map_err(|e| format!("Cannot list {}: {e}", source.display()))? {
        let entry = entry.map_err(|e| format!("Cannot list {}: {e}", source.display()))?;
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| format!("Cannot read {}: {e}", entry.path().display()))?;
        if meta.file_type().is_symlink() { return Err(format!("{} is a link; images are built from real files only.", entry.path().display())); }
        let name = entry.file_name();
        let text = name.to_str().ok_or_else(|| format!("{} has a name that is not valid Unicode.", entry.path().display()))?.to_string();
        let units: Vec<u16> = text.encode_utf16().collect();
        if !valid_name(&units) { return Err(format!("\"{text}\" in {} cannot be stored on exFAT.", source.display())); }
        entries.push((units, text, entry.path(), meta));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut seen = HashSet::new();
    for (units, text, path, meta) in entries {
        if !seen.insert(upcased(&units)) { return Err(format!("{} holds two names that differ only in case (\"{text}\").", source.display())); }
        let child = nodes.len();
        let parent_path = nodes[index].path.clone();
        nodes.push(Node {
            name: units, path: if parent_path.is_empty() { text } else { format!("{parent_path}/{text}") },
            source: path.clone(), is_dir: meta.is_dir(), size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().unwrap_or(UNIX_EPOCH), children: Vec::new(), first_cluster: 0, clusters: 0,
        });
        nodes[index].children.push(child);
        if meta.is_dir() { scan(&path, nodes, child, depth + 1)?; }
        else if !meta.is_file() { return Err(format!("{} is not a regular file.", path.display())); }
    }
    Ok(())
}

/// Scans `source` and lays out the whole volume. `label` is at most 11 UTF-16 units.
pub(crate) fn plan(source: &Path, label: &str) -> Result<Plan, String> {
    let root_meta = fs::metadata(source).map_err(|e| format!("Cannot read {}: {e}", source.display()))?;
    if !root_meta.is_dir() { return Err(format!("{} is not a folder.", source.display())); }
    let label: Vec<u16> = label.encode_utf16().take(11).collect();
    if !label.iter().all(|&u| u >= 0x20) { return Err("The volume label contains control characters.".into()); }
    let mut nodes = vec![Node {
        name: Vec::new(), path: String::new(), source: source.to_path_buf(), is_dir: true, size: 0,
        modified: root_meta.modified().unwrap_or(UNIX_EPOCH), children: Vec::new(), first_cluster: 0, clusters: 0,
    }];
    scan(source, &mut nodes, 0, 0)?;

    // Directory sizes: every child's entry set, the root's three system entries, and an end marker.
    for i in 0..nodes.len() {
        if !nodes[i].is_dir { continue; }
        let slots = 1 + if i == 0 { 3 } else { 0 } + nodes[i].children.iter().map(|&c| entry_slots(nodes[c].name.len())).sum::<u64>();
        nodes[i].clusters = ceil_div(slots * ENTRY as u64, CLUSTER).max(1);
    }
    for node in nodes.iter_mut().filter(|n| !n.is_dir) { node.clusters = ceil_div(node.size, CLUSTER); }

    let upcase_clusters = ceil_div(upcase_bytes().len() as u64, CLUSTER);
    let dir_clusters: u64 = nodes.iter().filter(|n| n.is_dir).map(|n| n.clusters).sum();
    let file_clusters: u64 = nodes.iter().filter(|n| !n.is_dir).map(|n| n.clusters).sum();
    let meta = upcase_clusters.checked_add(dir_clusters).and_then(|v| v.checked_add(file_clusters)).ok_or("The folder is too large for one exFAT image.")?;
    // The bitmap covers every cluster, including its own; iterate to a fixed point. Sixteen-cluster
    // steps keep the image a whole number of MiB.
    let mut count = align_up(meta + 1, 16);
    loop {
        let bitmap = ceil_div(ceil_div(count, 8), CLUSTER);
        let next = align_up(meta + bitmap, 16);
        if next <= count { break; }
        count = next;
    }
    if count > 0xFFFF_FFF5 { return Err("The folder is too large for one exFAT image.".into()); }
    let bitmap_bytes = ceil_div(count, 8);
    let bitmap_clusters = ceil_div(bitmap_bytes, CLUSTER);

    // Cluster numbering starts at 2: bitmap, up-case table, directories (depth first), then files.
    let mut next = 2u64;
    let mut take = |n: u64| { let first = next; next += n; first as u32 };
    let bitmap_first = take(bitmap_clusters);
    let upcase_first = take(upcase_clusters);
    let mut order = Vec::new();
    fn walk(nodes: &[Node], i: usize, out: &mut Vec<usize>) { out.push(i); for &c in &nodes[i].children { walk(nodes, c, out); } }
    walk(&nodes, 0, &mut order);
    let dirs: Vec<usize> = order.iter().copied().filter(|&i| nodes[i].is_dir).collect();
    let files: Vec<usize> = order.iter().copied().filter(|&i| !nodes[i].is_dir && nodes[i].size > 0).collect();
    for i in dirs.into_iter().chain(files) { nodes[i].first_cluster = take(nodes[i].clusters); }
    let used = next - 2;
    debug_assert!(used <= count);

    let fat_length = ceil_div((count + 2) * 4, SECTOR);
    let heap_offset = align_up(FAT_OFFSET_SECTORS as u64 + fat_length, ALIGN_SECTORS);
    let image_bytes = heap_offset * SECTOR + count * CLUSTER;

    // The serial is derived from the content, so rebuilding the same folder gives the same image.
    let mut digest = Sha256::new();
    for node in &nodes {
        digest.update(node.path.as_bytes()); digest.update([0]);
        digest.update(node.size.to_le_bytes());
        digest.update(exfat_time(node.modified).0.to_le_bytes());
    }
    let serial = u32::from_le_bytes(digest.finalize()[..4].try_into().unwrap());
    let files = nodes.iter().filter(|n| !n.is_dir).count() as u64;
    let payload_bytes = nodes.iter().map(|n| n.size).sum();
    Ok(Plan {
        directories: nodes.len() as u64 - files, files, payload_bytes, nodes, label, cluster_count: count as u32,
        fat_length: fat_length as u32, heap_offset: heap_offset as u32, bitmap_first, bitmap_bytes, upcase_first,
        used_clusters: used, serial, image_bytes,
    })
}

/* ------------------------------------------------------------------ structures */

fn put16(b: &mut [u8], at: usize, v: u16) { b[at..at + 2].copy_from_slice(&v.to_le_bytes()); }
fn put32(b: &mut [u8], at: usize, v: u32) { b[at..at + 4].copy_from_slice(&v.to_le_bytes()); }
fn put64(b: &mut [u8], at: usize, v: u64) { b[at..at + 8].copy_from_slice(&v.to_le_bytes()); }

/// Main and backup boot regions are identical: 12 sectors each.
pub(crate) fn boot_region(plan: &Plan) -> Vec<u8> {
    let mut region = vec![0u8; 12 * SECTOR as usize];
    let boot = &mut region[..SECTOR as usize];
    boot[..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
    boot[3..11].copy_from_slice(b"EXFAT   ");
    put64(boot, 64, 0); // PartitionOffset
    put64(boot, 72, plan.image_bytes / SECTOR);
    put32(boot, 80, FAT_OFFSET_SECTORS);
    put32(boot, 84, plan.fat_length);
    put32(boot, 88, plan.heap_offset);
    put32(boot, 92, plan.cluster_count);
    put32(boot, 96, plan.root().first_cluster);
    put32(boot, 100, plan.serial);
    put16(boot, 104, 0x0100);
    put16(boot, 106, 0); // VolumeFlags
    boot[108] = SECTOR_SHIFT;
    boot[109] = CLUSTER_SHIFT;
    boot[110] = 1; // NumberOfFats
    boot[111] = 0x80; // DriveSelect
    boot[112] = (plan.used_clusters * 100 / plan.cluster_count.max(1) as u64) as u8;
    boot[510] = 0x55;
    boot[511] = 0xAA;
    for sector in 1..=8 { let end = (sector + 1) * SECTOR as usize; region[end - 2] = 0x55; region[end - 1] = 0xAA; }
    let sum = boot_checksum(&region[..11 * SECTOR as usize]);
    for word in region[11 * SECTOR as usize..].chunks_exact_mut(4) { word.copy_from_slice(&sum.to_le_bytes()); }
    region
}

pub(crate) fn fat(plan: &Plan) -> Vec<u8> {
    let mut fat = vec![0u8; plan.fat_length as usize * SECTOR as usize];
    put32(&mut fat, 0, 0xFFFF_FFF8);
    put32(&mut fat, 4, 0xFFFF_FFFF);
    let mut chain = |first: u32, clusters: u64| {
        for k in 0..clusters {
            let cluster = first as u64 + k;
            let value = if k + 1 == clusters { 0xFFFF_FFFF } else { cluster as u32 + 1 };
            put32(&mut fat, cluster as usize * 4, value);
        }
    };
    chain(plan.bitmap_first, ceil_div(plan.bitmap_bytes, CLUSTER));
    chain(plan.upcase_first, ceil_div(upcase_bytes().len() as u64, CLUSTER));
    for node in plan.nodes.iter().filter(|n| n.is_dir) { chain(node.first_cluster, node.clusters); }
    fat
}

pub(crate) fn bitmap(plan: &Plan) -> Vec<u8> {
    let mut bits = vec![0u8; ceil_div(plan.bitmap_bytes, CLUSTER) as usize * CLUSTER as usize];
    for k in 0..plan.used_clusters as usize { bits[k / 8] |= 1 << (k % 8); }
    bits
}

fn entry_set(node: &Node) -> Vec<u8> {
    let names = ceil_div(node.name.len() as u64, 15) as usize;
    let mut set = vec![0u8; ENTRY * (2 + names)];
    let (stamp, increment) = exfat_time(node.modified);
    set[0] = 0x85;
    set[1] = (1 + names) as u8;
    put16(&mut set, 4, if node.is_dir { ATTR_DIRECTORY } else { ATTR_ARCHIVE });
    for at in [8, 12, 16] { put32(&mut set, at, stamp); }
    set[20] = increment;
    set[21] = increment;
    set[22] = UTC_OFFSET;
    set[23] = UTC_OFFSET;
    set[24] = UTC_OFFSET;
    let stream = &mut set[ENTRY..2 * ENTRY];
    stream[0] = 0xC0;
    let (length, first, flags) = if node.is_dir { (node.clusters * CLUSTER, node.first_cluster, FLAG_ALLOCATION_POSSIBLE) }
        else if node.size == 0 { (0, 0, FLAG_ALLOCATION_POSSIBLE) }
        else { (node.size, node.first_cluster, FLAG_ALLOCATION_POSSIBLE | FLAG_NO_FAT_CHAIN) };
    stream[1] = flags;
    stream[3] = node.name.len() as u8;
    put16(stream, 4, name_hash(&node.name));
    put64(stream, 8, length);
    put32(stream, 20, first);
    put64(stream, 24, length);
    for (k, chunk) in node.name.chunks(15).enumerate() {
        let entry = &mut set[ENTRY * (2 + k)..ENTRY * (3 + k)];
        entry[0] = 0xC1;
        for (j, &unit) in chunk.iter().enumerate() { put16(entry, 2 + 2 * j, unit); }
    }
    let sum = set_checksum(&set);
    put16(&mut set, 2, sum);
    set
}

pub(crate) fn directory(plan: &Plan, index: usize) -> Vec<u8> {
    let node = &plan.nodes[index];
    let mut data = Vec::with_capacity((node.clusters * CLUSTER) as usize);
    if index == 0 {
        let mut label = [0u8; ENTRY];
        label[0] = 0x83;
        label[1] = plan.label.len() as u8;
        for (j, &unit) in plan.label.iter().enumerate() { put16(&mut label, 2 + 2 * j, unit); }
        let mut bitmap = [0u8; ENTRY];
        bitmap[0] = 0x81;
        put32(&mut bitmap, 20, plan.bitmap_first);
        put64(&mut bitmap, 24, plan.bitmap_bytes);
        let mut upcase = [0u8; ENTRY];
        upcase[0] = 0x82;
        put32(&mut upcase, 4, checksum32(upcase_bytes(), |_| false));
        put32(&mut upcase, 20, plan.upcase_first);
        put64(&mut upcase, 24, upcase_bytes().len() as u64);
        data.extend_from_slice(&label);
        data.extend_from_slice(&bitmap);
        data.extend_from_slice(&upcase);
    }
    for &child in &node.children { data.extend_from_slice(&entry_set(&plan.nodes[child])); }
    data.resize((node.clusters * CLUSTER) as usize, 0);
    data
}

/* ------------------------------------------------------------------ writing */

#[derive(Debug, Clone)]
pub(crate) struct Progress<'a> {
    pub(crate) written: u64,
    pub(crate) total: u64,
    pub(crate) files_done: u64,
    pub(crate) current: &'a str,
}

/// SHA-256 of every file as it was copied, keyed by image path.
pub(crate) type Hashes = Vec<(String, [u8; 32])>;

struct Counted<W: Write> { inner: W, written: u64 }
impl<W: Write> Counted<W> {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> { self.inner.write_all(bytes)?; self.written += bytes.len() as u64; Ok(()) }
    fn zeros(&mut self, mut count: u64) -> io::Result<()> {
        static ZERO: [u8; 1 << 16] = [0; 1 << 16];
        while count > 0 { let n = count.min(ZERO.len() as u64) as usize; self.put(&ZERO[..n])?; count -= n as u64; }
        Ok(())
    }
}

/// Writes the planned image to `output` (which must not exist) and returns each file's SHA-256.
/// `control` is called between chunks; returning an error stops the build and removes the file.
pub(crate) fn write(plan: &Plan, output: &Path, mut control: impl FnMut(Progress) -> Result<(), String>) -> Result<Hashes, String> {
    let file = fs::OpenOptions::new().write(true).create_new(true).open(output)
        .map_err(|e| format!("Cannot create {}: {e}", output.display()))?;
    let result = write_into(plan, &file, &mut control);
    let result = result.and_then(|hashes| file.sync_all().map(|_| hashes).map_err(|e| format!("Cannot finish {}: {e}", output.display())));
    drop(file);
    if result.is_err() { let _ = fs::remove_file(output); }
    result
}

fn write_into(plan: &Plan, file: &fs::File, control: &mut dyn FnMut(Progress) -> Result<(), String>) -> Result<Hashes, String> {
    let io = |e: io::Error| format!("Writing the image failed: {e}");
    let mut out = Counted { inner: io::BufWriter::with_capacity(COPY_BUFFER, file), written: 0 };
    let region = boot_region(plan);
    out.put(&region).map_err(io)?;
    out.put(&region).map_err(io)?;
    out.zeros(FAT_OFFSET_SECTORS as u64 * SECTOR - out.written).map_err(io)?;
    out.put(&fat(plan)).map_err(io)?;
    out.zeros(plan.heap_offset as u64 * SECTOR - out.written).map_err(io)?;
    out.put(&bitmap(plan)).map_err(io)?;
    let mut upcase = upcase_bytes().to_vec();
    upcase.resize(align_up(upcase.len() as u64, CLUSTER) as usize, 0);
    out.put(&upcase).map_err(io)?;
    let mut dirs: Vec<usize> = (0..plan.nodes.len()).filter(|&i| plan.nodes[i].is_dir).collect();
    dirs.sort_by_key(|&i| plan.nodes[i].first_cluster);
    for i in dirs {
        debug_assert_eq!(out.written, cluster_offset(plan, plan.nodes[i].first_cluster));
        out.put(&directory(plan, i)).map_err(io)?;
    }
    let mut hashes = Hashes::new();
    let mut buffer = vec![0u8; COPY_BUFFER];
    let mut files_done = plan.files - plan.file_order().len() as u64; // empty files need no data
    for i in plan.file_order() {
        let node = &plan.nodes[i];
        debug_assert_eq!(out.written, cluster_offset(plan, node.first_cluster));
        let mut source = fs::File::open(&node.source).map_err(|e| format!("Cannot open {}: {e}", node.source.display()))?;
        let mut digest = Sha256::new();
        let mut left = node.size;
        while left > 0 {
            control(Progress { written: out.written, total: plan.image_bytes, files_done, current: &node.path })?;
            let want = left.min(buffer.len() as u64) as usize;
            let got = read_full(&mut source, &mut buffer[..want]).map_err(|e| format!("Cannot read {}: {e}", node.source.display()))?;
            if got < want { return Err(format!("{} got shorter while the image was being written.", node.path)); }
            digest.update(&buffer[..got]);
            out.put(&buffer[..got]).map_err(io)?;
            left -= got as u64;
        }
        let mut probe = [0u8; 1];
        if source.read(&mut probe).map_err(|e| format!("Cannot read {}: {e}", node.source.display()))? != 0 {
            return Err(format!("{} grew while the image was being written.", node.path));
        }
        out.zeros(node.clusters * CLUSTER - node.size).map_err(io)?;
        hashes.push((node.path.clone(), digest.finalize().into()));
        files_done += 1;
    }
    out.zeros(plan.image_bytes - out.written).map_err(io)?;
    control(Progress { written: out.written, total: plan.image_bytes, files_done, current: "" })?;
    out.inner.flush().map_err(io)?;
    debug_assert_eq!(out.written, plan.image_bytes);
    Ok(hashes)
}

fn read_full(source: &mut fs::File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

pub(crate) fn cluster_offset(plan: &Plan, cluster: u32) -> u64 {
    plan.heap_offset as u64 * SECTOR + (cluster as u64 - 2) * CLUSTER
}

#[cfg(test)]
mod tests;
