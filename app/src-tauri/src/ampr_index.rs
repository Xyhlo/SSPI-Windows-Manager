//! AMPRIDX3 / AMPRPAK4 reconciliation in disposable package staging.
//! Authority: drakmor/ampr_emu cfa85df, build_ampr_index.py, ampr_pack_format.py,
//! src/ampr_emu_index.cpp and src/ampr_emu_pack.cpp (0.4.2.1 pack tools).

use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{Read, Write};
use std::path::Path;

const MAX_ENTRIES: usize = 2_000_000;
const MAX_INDEX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SLOTS: usize = 4_194_304;
const MAX_LOOKUP_PROBES: usize = 64_000_000;
const WARNING_LIMIT: usize = 8;

fn failure(message: impl std::fmt::Display) -> String { format!("AMPR index: {message}") }
fn linked(metadata: &Metadata) -> bool {
    #[cfg(windows)] { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))] { metadata.file_type().is_symlink() }
}

fn regular_path(root: &Path, relative: &str) -> Result<Option<Metadata>, String> {
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.split('/').collect();
    for (position, component) in components.iter().enumerate() {
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(failure(format!("cannot inspect {relative}: {error}"))),
        };
        if linked(&metadata) { return Err(failure(format!("linked or reparse path is unsupported: {relative}"))); }
        if position + 1 == components.len() {
            if !metadata.is_file() { return Err(failure(format!("indexed path is not a regular file: {relative}"))); }
            return Ok(Some(metadata));
        }
        if !metadata.is_dir() { return Err(failure(format!("indexed parent is not a directory: {relative}"))); }
    }
    Ok(None)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 { u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) }
fn u64_at(bytes: &[u8], offset: usize) -> u64 { u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) }
fn key(path: &str) -> Vec<u8> {
    path.bytes().map(|byte| match byte { b'\\' => b'/', b'A'..=b'Z' => byte + 32, _ => byte }).collect()
}
/// AMPRIDX3 hash slots. Upstream's build_ampr_index.py seeds FNV-1a with 1469598103934665603
/// (not the standard 14695981039346656037), and ampr_emu looks paths up the same way.
fn path_hash(path: &str) -> u64 { fnv1a64(1_469_598_103_934_665_603, path) }
/// AMPRPAK4 file records use the standard FNV-1a seed (ampr_pack_format.py `asset_path_hash`).
fn asset_path_hash(path: &str) -> u64 { fnv1a64(0xCBF2_9CE4_8422_2325, path) }
fn fnv1a64(seed: u64, path: &str) -> u64 {
    let mut hash = seed;
    for byte in key(path) { hash = (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211); }
    hash.max(1)
}
fn relative_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");
    if !normalized.as_bytes().get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"/app0/")) {
        return Err(failure(format!("path must be below /app0: {path}")));
    }
    let relative = &normalized[6..];
    if relative.is_empty() || relative.split('/').any(|part| part.is_empty() || matches!(part, "." | "..")
        || part.contains(':') || part.chars().any(char::is_control) || part.ends_with('.') || part.ends_with(' ')) {
        return Err(failure(format!("invalid or ambiguous indexed path: {path}")));
    }
    Ok(relative.to_string())
}

fn string_at(bytes: &[u8], offset: u64, length: u64) -> Result<String, String> {
    if length == 0 || length >= 4096 || offset.checked_add(length).is_none_or(|end| end >= bytes.len() as u64) {
        return Err(failure("invalid path bounds"));
    }
    let start = offset as usize; let end = start + length as usize;
    if bytes[end] != 0 || bytes[start..end].contains(&0) { return Err(failure("path is not a single NUL-terminated string")); }
    String::from_utf8(bytes[start..end].to_vec()).map_err(|_| failure("indexed path is not UTF-8"))
}

struct Record { path: String, size: u64 }
struct Index { bytes: Vec<u8>, records: Vec<Record> }

struct AssetRecord { path: String, size: u64, packed: bool }
struct Assets { bytes: Vec<u8>, records: Vec<AssetRecord> }

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, value) in table.iter_mut().enumerate() {
        *value = i as u32;
        for _ in 0..8 { *value = (*value >> 1) ^ (0xedb88320u32 & 0u32.wrapping_sub(*value & 1)); }
    }
    let mut crc = !0u32;
    for byte in bytes { crc = (crc >> 8) ^ table[((crc ^ u32::from(*byte)) & 255) as usize]; }
    !crc
}

fn parse_assets(root: &Path, mut bytes: Vec<u8>, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Assets, String> {
    let bad = |message: &str| failure(format!("ampr_assets.index: {message}; restore the matching manifest/pack set or regenerate it with AMPR pack tools"));
    if bytes.len() < 128 || &bytes[..8] != b"AMPRPAK4" || u32_at(&bytes, 8) != 4 {
        return Err(bad("unsupported or truncated asset manifest (expected AMPRPAK4)"));
    }
    if u32_at(&bytes, 12) != 128 || u32_at(&bytes, 16) != 0 || u32_at(&bytes, 20) != 0x01020304 || u64_at(&bytes, 120) != 0
        || u32_at(&bytes, 60) != 48 || u32_at(&bytes, 64) != 12 || u32_at(&bytes, 68) != 32 {
        return Err(bad("invalid header/record sizes"));
    }
    let header_crc = u32_at(&bytes, 116); bytes[116..120].fill(0);
    let valid_crc = crc32(&bytes[..128]) == header_crc && crc32(&bytes[128..]) == u32_at(&bytes, 112);
    bytes[116..120].copy_from_slice(&header_crc.to_le_bytes());
    if !valid_crc { return Err(bad("header or payload CRC mismatch")); }
    checkpoint()?;
    let file_count = u64_at(&bytes, 40); let chunk_count = u64_at(&bytes, 48); let pack_count = u32_at(&bytes, 56) as u64;
    if file_count > MAX_ENTRIES as u64 || chunk_count > MAX_INDEX_BYTES / 12 || pack_count > 65536 { return Err(bad("record count exceeds inspection limits")); }
    let chunks = 128 + file_count * 48; let packs = chunks + chunk_count * 12; let strings = packs + pack_count * 32;
    if u64_at(&bytes, 72) != 128 || u64_at(&bytes, 80) != chunks || u64_at(&bytes, 88) != packs || u64_at(&bytes, 96) != strings
        || strings.checked_add(u64_at(&bytes, 104)) != Some(bytes.len() as u64) { return Err(bad("invalid section layout")); }
    let chunks = chunks as usize; let packs = packs as usize; let strings = strings as usize;
    let mut pack_geometry = Vec::with_capacity(pack_count as usize);
    for id in 0..pack_count as usize {
        checkpoint()?;
        let row = packs + id * 32;
        let payload = u64_at(&bytes, row); let size = u64_at(&bytes, row + 8);
        let name = string_at(&bytes[strings..], u32_at(&bytes, row + 16) as u64, u32_at(&bytes, row + 20) as u64)?;
        let relative = relative_path(&format!("/app0/{name}"))?;
        let flags = u32_at(&bytes, row + 24); let page = u32_at(&bytes, row + 28) as u64;
        if flags & !3 != 0 || flags & 2 == 0 || !(4096..=1048576).contains(&page) || !page.is_power_of_two()
            || size < 64 || payload > size { return Err(bad("invalid pack flags, page size or length")); }
        let start = size - payload;
        if start < 64 || start % page != 0 || size % page != 0 { return Err(bad("invalid page-aware pack layout")); }
        let metadata = regular_path(root, &relative)?.ok_or_else(|| bad(&format!("missing pack {relative}")))?;
        if metadata.len() != size { return Err(bad(&format!("pack size mismatch: {relative}"))); }
        let mut header = [0; 64]; File::open(root.join(&relative)).map_err(failure)?.read_exact(&mut header).map_err(failure)?;
        let crc = u32_at(&header, 56); header[56..60].fill(0);
        if &header[..8] != b"AMPRDAT3" || u32_at(&header, 8) != 3 || u32_at(&header, 12) != 64
            || u32_at(&header, 16) != id as u32 || u32_at(&header, 20) != flags || header[24..40] != bytes[24..40]
            || u64_at(&header, 40) != start || u64_at(&header, 48) != payload || u32_at(&header, 60) != 0 || crc32(&header) != crc {
            return Err(bad(&format!("pack header/build ID/CRC mismatch: {relative}")));
        }
        pack_geometry.push((start, size, page));
    }
    let mut records = Vec::with_capacity(file_count as usize);
    let mut seen = HashSet::new();
    let mut visits = 0usize;
    for id in 0..file_count as usize {
        checkpoint()?;
        let row = 128 + id * 48;
        let path = string_at(&bytes[strings..], u32_at(&bytes, row + 32) as u64, u32_at(&bytes, row + 36) as u64)?;
        let relative = relative_path(&path)?;
        if path[6..] != relative || path.contains('\\') || !seen.insert(key(&path)) || asset_path_hash(&path) != u64_at(&bytes, row) {
            return Err(bad("non-canonical, duplicate or incorrectly hashed path"));
        }
        let size = u64_at(&bytes, row + 8); let first = u32_at(&bytes, row + 24) as u64; let count = u32_at(&bytes, row + 28) as u64;
        let flags = u32_at(&bytes, row + 40); let shift = bytes[row + 44]; let class = bytes[row + 45];
        if flags & !31 != 0 || bytes[row + 46..row + 48] != [0, 0] { return Err(bad("unknown file flags/reserved bits")); }
        let packed = flags & 1 != 0;
        if !packed {
            if flags != 0 || first != 0 || count != 0 || shift != 0 || class != 0 { return Err(bad("loose record references chunks")); }
        } else {
            if !(14..=20).contains(&shift) || flags & 20 == 20 || first + count > chunk_count { return Err(bad("invalid file chunk geometry")); }
            let block = 1u64 << shift;
            if count != size.div_ceil(block) { return Err(bad("logical size/chunk count mismatch")); }
            for local in 0..count {
                visits += 1;
                if visits > MAX_LOOKUP_PROBES { return Err(bad("overlapping chunk references exceed inspection limit")); }
                if visits % 1024 == 0 { checkpoint()?; }
                let chunk = chunks + (first + local) as usize * 12;
                let location = u64_at(&bytes, chunk); let descriptor = u32_at(&bytes, chunk + 8);
                let offset = location & ((1u64 << 48) - 1); let pack_id = (location >> 48) as usize;
                let stored = (descriptor & 0xfffff) as u64 + 1; let codec = (descriptor >> 20) & 3; let chunk_flags = (descriptor >> 22) & 255;
                let raw = block.min(size - local * block);
                if descriptor >> 30 != 0 || chunk_flags & !15 != 0 || codec > 1 || stored > block || (codec == 0 && stored != raw)
                    || ((flags & 4 != 0) != (chunk_flags & 2 != 0)) || (flags & 2 != 0 && codec != 0) || pack_id >= pack_geometry.len() {
                    return Err(bad("invalid chunk size, codec, flags or pack ID"));
                }
                let (start, end, page) = pack_geometry[pack_id];
                let contained = chunk_flags & 4 != 0; let aligned = chunk_flags & 8 != 0;
                let page_start = offset & !(page - 1); let page_end = (offset + stored + page - 1) & !(page - 1);
                if offset < start || offset + stored > end || offset % 64 != 0 || page_start < start || page_end > end
                    || (contained && (stored > page || page_start != (offset + stored - 1) & !(page - 1)))
                    || (aligned && offset % page != 0) || (stored <= page && aligned && !contained)
                    || ((flags & 16 != 0 || flags & 4 == 0) && !contained && !aligned) {
                    return Err(bad("invalid chunk range/alignment/page flags"));
                }
            }
        }
        records.push(AssetRecord { path, size, packed });
    }
    Ok(Assets { bytes, records })
}

fn parse_index(bytes: Vec<u8>, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Index, String> {
    if bytes.len() < 48 || &bytes[..8] != b"AMPRIDX3" { return Err(failure("truncated or unsupported index format (expected AMPRIDX3)")); }
    let count = u64_at(&bytes, 16); let path_bytes = u64_at(&bytes, 24);
    let hash_offset = u64_at(&bytes, 32); let slots = u32_at(&bytes, 44) as usize;
    if u32_at(&bytes, 8) != 3 || u32_at(&bytes, 12) != 24 || u32_at(&bytes, 40) != 16 {
        return Err(failure("invalid AMPRIDX3 version or record sizes"));
    }
    if count == 0 || count > MAX_ENTRIES as u64 || path_bytes == 0 || path_bytes > 256 * 1024 * 1024
        || slots < 2 || slots > MAX_SLOTS || !slots.is_power_of_two() || slots < count as usize {
        return Err(failure("invalid AMPRIDX3 record, path or hash-slot counts"));
    }
    let paths_start = 48 + count as usize * 24;
    let paths_end = paths_start + path_bytes as usize;
    if hash_offset < ((paths_end as u64 + 7) & !7) || hash_offset % 8 != 0
        || hash_offset.checked_add(slots as u64 * 16).is_none_or(|end| end > bytes.len() as u64) {
        return Err(failure("truncated or overlapping AMPRIDX3 sections"));
    }
    let mut records = Vec::with_capacity(count as usize);
    let mut hashes = Vec::with_capacity(count as usize);
    let mut unique = HashSet::new();
    for id in 0..count as usize {
        checkpoint()?;
        let row = 48 + id * 24;
        let path = string_at(&bytes[paths_start..paths_end], u32_at(&bytes, row) as u64, u32_at(&bytes, row + 4) as u64)?;
        relative_path(&path)?;
        if !unique.insert(key(&path)) { return Err(failure("duplicate case-insensitive application paths")); }
        hashes.push(path_hash(&path));
        records.push(Record { path, size: u64_at(&bytes, row + 8) });
    }
    let hash_offset = hash_offset as usize;
    let mut seen = vec![false; records.len()];
    for slot in 0..slots {
        if slot % 1024 == 0 { checkpoint()?; }
        let row = hash_offset + slot * 16;
        let hash = u64_at(&bytes, row); let id = u32_at(&bytes, row + 8) as usize; let flags = u32_at(&bytes, row + 12);
        if id == 0 {
            if hash != 0 || flags != 0 { return Err(failure("invalid empty hash slot")); }
        } else {
            if id > records.len() || flags & !1 != 0 || seen[id - 1] || hashes[id - 1] != hash {
                return Err(failure("invalid, duplicate or mismatched hash-slot file ID"));
            }
            seen[id - 1] = true;
        }
    }
    if seen.iter().any(|present| !present) { return Err(failure("hash table omits an indexed file ID")); }
    let mut probes = 0;
    for (id, hash) in hashes.iter().enumerate() {
        let mut position = *hash as usize & (slots - 1);
        loop {
            probes += 1;
            if probes > MAX_LOOKUP_PROBES { return Err(failure("hash chains exceed the bounded validation limit")); }
            if probes % 1024 == 0 { checkpoint()?; }
            let candidate = u32_at(&bytes, hash_offset + position * 16 + 8) as usize;
            if candidate == id + 1 { break; }
            if candidate == 0 { return Err(failure("broken hash lookup chain")); }
            // A colliding hash must force the runtime's full path comparison.
            if hashes[candidate - 1] == *hash && u32_at(&bytes, hash_offset + position * 16 + 12) & 1 == 0 {
                return Err(failure("hash collision is missing its path-comparison flag"));
            }
            position = (position + 1) & (slots - 1);
        }
    }
    Ok(Index { bytes, records })
}

fn read_index(root: &Path, name: &str, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Option<Vec<u8>>, String> {
    let Some(metadata) = regular_path(root, name)? else { return Ok(None); };
    if metadata.len() > MAX_INDEX_BYTES { return Err(failure(format!("{name} exceeds the 512 MiB inspection limit; regenerate it with the matching AMPR tools"))); }
    let mut input = File::open(root.join(name)).map_err(failure)?;
    let mut data = Vec::with_capacity(metadata.len() as usize);
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        checkpoint()?;
        let count = input.read(&mut buffer).map_err(failure)?;
        if count == 0 { break; }
        if data.len() + count > MAX_INDEX_BYTES as usize { return Err(failure("index grew beyond the inspection limit")); }
        data.extend_from_slice(&buffer[..count]);
    }
    Ok(Some(data))
}

// Upstream lookup_app0_index_key_view returns row+1 at runtime. Loose opens resolve
// the path through that row; no on-disk loose file stores this ID. However,
// resolve_packed_file in ampr_emu_pack.cpp indexes manifest.files[fileId-1] and
// compares logicalSize. Never drop/reorder valid records, even missing ones; keep
// their IDs with a warning. Rebuild invalid indexes only WITHOUT an asset manifest.
fn rebuild(root: &Path, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Index, String> {
    let mut rows = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((directory, depth)) = stack.pop() {
        for entry in fs::read_dir(directory).map_err(failure)? {
            checkpoint()?;
            let entry = entry.map_err(failure)?; let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(failure)?;
            if linked(&metadata) { return Err(failure("cannot rebuild across linked source paths")); }
            if metadata.is_dir() {
                if depth >= 32 { return Err(failure("rebuild directory depth exceeds 32")); }
                stack.push((path, depth + 1)); continue;
            }
            if !metadata.is_file() { return Err(failure("cannot index a non-regular file")); }
            let relative = path.strip_prefix(root).map_err(failure)?.to_str().ok_or_else(|| failure("non-UTF-8 file path"))?.replace('\\', "/");
            if matches!(relative.to_ascii_lowercase().as_str(), "ampr_emu.index" | "ampr_emu.index.tmp" | "ampr_commands.bin" | "apr_emu.log") { continue; }
            let name = format!("/app0/{relative}"); relative_path(&name)?;
            let mtime = metadata.modified().map_err(failure)?.duration_since(std::time::UNIX_EPOCH).map(|time| time.as_secs() as i64).unwrap_or(0);
            rows.push((name, metadata.len(), mtime));
            if rows.len() > MAX_ENTRIES { return Err(failure("rebuild file count exceeds inspection limit")); }
        }
    }
    // Match build_ampr_index.py: ASCII-folded UTF-8 sort, FNV slots at <=50% load,
    // <IIQq> records, NUL paths, 16-byte alignment and duplicate-hash flags.
    rows.sort_by_cached_key(|row| key(&row.0));
    if rows.is_empty() { return Err(failure("cannot rebuild an empty application index")); }
    let count = rows.len(); let slot_count = (count * 2).next_power_of_two().max(2);
    let mut data = vec![0u8; 48 + count * 24]; let mut strings = Vec::new();
    let mut slots = vec![(0u64, 0u32, 0u32); slot_count];
    for (id, (path, size, mtime)) in rows.iter().enumerate() {
        checkpoint()?;
        if id > 0 && key(path) == key(&rows[id - 1].0) { return Err(failure("cannot rebuild case-insensitive path collisions")); }
        let row = 48 + id * 24;
        data[row..row + 4].copy_from_slice(&(strings.len() as u32).to_le_bytes());
        data[row + 4..row + 8].copy_from_slice(&(path.len() as u32).to_le_bytes());
        data[row + 8..row + 16].copy_from_slice(&size.to_le_bytes());
        data[row + 16..row + 24].copy_from_slice(&mtime.to_le_bytes());
        strings.extend_from_slice(path.as_bytes()); strings.push(0);
        if strings.len() > 256 * 1024 * 1024 { return Err(failure("rebuild path table exceeds inspection limit")); }
        let hash = path_hash(path); let mut position = hash as usize & (slot_count - 1); let mut duplicate = 0;
        while slots[position].1 != 0 {
            if slots[position].0 == hash { slots[position].2 = 1; duplicate = 1; }
            position = (position + 1) & (slot_count - 1);
        }
        slots[position] = (hash, id as u32 + 1, duplicate);
    }
    let hash_offset = (data.len() + strings.len() + 15) & !15;
    data[..8].copy_from_slice(b"AMPRIDX3"); data[8..12].copy_from_slice(&3u32.to_le_bytes()); data[12..16].copy_from_slice(&24u32.to_le_bytes());
    data[16..24].copy_from_slice(&(count as u64).to_le_bytes()); data[24..32].copy_from_slice(&(strings.len() as u64).to_le_bytes());
    data[32..40].copy_from_slice(&(hash_offset as u64).to_le_bytes()); data[40..44].copy_from_slice(&16u32.to_le_bytes()); data[44..48].copy_from_slice(&(slot_count as u32).to_le_bytes());
    data.extend(strings); data.resize(hash_offset, 0);
    for (hash, id, flags) in slots { data.extend(hash.to_le_bytes()); data.extend(id.to_le_bytes()); data.extend(flags.to_le_bytes()); }
    parse_index(data, checkpoint)
}

/// A fresh AMPRIDX3 for `root`, in upstream order. Only for folders no asset manifest depends on.
#[cfg(test)]
pub(crate) fn rebuilt_index(root: &Path) -> Result<Vec<u8>, String> { rebuild(root, &|| Ok(())).map(|index| index.bytes) }

#[derive(Default)]
struct Plan { warnings: Vec<String>, replacements: Vec<(&'static str, Vec<u8>)> }

fn plan(root: &Path, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Plan, String> {
    checkpoint()?;
    let metadata = fs::symlink_metadata(root).map_err(failure)?;
    if !metadata.is_dir() || linked(&metadata) { return Err(failure("application root must be an ordinary directory")); }
    let mut assets = read_index(root, "ampr_assets.index", checkpoint)?.map(|bytes| parse_assets(root, bytes, checkpoint)).transpose()?;
    let Some(bytes) = read_index(root, "ampr_emu.index", checkpoint)? else {
        if assets.is_some() { return Err(failure("ampr_assets.index requires its matching ampr_emu.index; restore that index with the AMPR pack tools")); }
        return Ok(Plan::default());
    };
    let mut result = Plan::default();
    let mut rebuilt = false;
    let mut index = match parse_index(bytes, checkpoint) {
        Ok(index) => index,
        Err(error) if !error.starts_with("AMPR index: ") => return Err(error),
        Err(error) => {
            checkpoint()?;
            if assets.is_some() { return Err(failure(format!("{error}; cannot renumber asset-pack file IDs. Restore the matching ampr_emu.index or rebuild the complete asset set with AMPR tools"))); }
            let index = rebuild(root, checkpoint)?;
            result.warnings.push(format!("AMPR index rebuilt in staging from {} loose files ({error}); upstream AMPRIDX3 path ordering and hash algorithm used. No asset manifest depends on the old file IDs.", index.records.len()));
            rebuilt = true; index
        }
    };
    if assets.as_ref().is_some_and(|assets| assets.records.len() != index.records.len()) {
        return Err(failure("asset manifest file count differs from ampr_emu.index; restore the matching index/pack set"));
    }
    let mut refreshed = 0; let mut missing = 0; let mut assets_changed = false;
    for (id, record) in index.records.iter().enumerate() {
        checkpoint()?;
        let relative = relative_path(&record.path)?;
        if let Some(assets) = &assets {
            let asset = &assets.records[id];
            if key(&asset.path) != key(&record.path) { return Err(failure(format!("asset manifest file ID {} differs from {}", id + 1, record.path))); }
            if asset.packed {
                if asset.size != record.size { return Err(failure(format!("packed size mismatch for {}; restore the matching index/pack set", record.path))); }
                continue;
            }
        }
        let Some(metadata) = regular_path(root, &relative)? else {
            if assets.as_ref().is_some_and(|assets| assets.records[id].size != record.size) { return Err(failure(format!("missing loose file {} has conflicting index/manifest sizes", record.path))); }
            missing += 1;
            if missing <= WARNING_LIMIT { result.warnings.push(format!("AMPR index kept missing loose file {} (file ID {}); IDs and lookups are preserved. Restore the file from the matching dump if the game needs it.", record.path, id + 1)); }
            continue;
        };
        let manifest_drift = assets.as_ref().is_some_and(|assets| assets.records[id].size != metadata.len());
        if metadata.len() == record.size && !manifest_drift { continue; }
        let path = root.join(&relative);
        if relative.eq_ignore_ascii_case("eboot.bin") || crate::fpkg_doctor::is_module(&path, Path::new(&relative))? {
            crate::fpkg_doctor::validate_executable(&path).map_err(|error| failure(format!("cannot refresh {}: {error}; restore a validated executable backup", record.path)))?;
        }
        index.bytes[48 + id * 24 + 8..48 + id * 24 + 16].copy_from_slice(&metadata.len().to_le_bytes());
        if let Some(assets) = &mut assets {
            assets.bytes[128 + id * 48 + 8..128 + id * 48 + 16].copy_from_slice(&metadata.len().to_le_bytes());
            assets_changed |= manifest_drift;
        }
        refreshed += 1;
        if refreshed <= WARNING_LIMIT { result.warnings.push(format!("AMPR index refreshed loose file {}: {} -> {} bytes{}; file ID {}, paths, hash slots and timestamps preserved.", record.path, record.size, metadata.len(), if manifest_drift { " (asset manifest refreshed too)" } else { "" }, id + 1)); }
    }
    if refreshed > 0 { result.warnings.push(format!("AMPR index refreshed {refreshed} loose size records in total (showing up to {WARNING_LIMIT} repairs).")); }
    if missing > 0 { result.warnings.push(format!("AMPR index retained {missing} missing loose records in total (showing up to {WARNING_LIMIT}); no file IDs renumbered.")); }
    if let Some(mut assets) = assets {
        result.warnings.push(format!("AMPR asset manifest validated: {} packed records, pack headers/build IDs, CRCs and chunk bounds checked; compressed payloads were not decompressed.", assets.records.iter().filter(|r| r.packed).count()));
        if assets_changed {
            let payload_crc = crc32(&assets.bytes[128..]); assets.bytes[112..116].copy_from_slice(&payload_crc.to_le_bytes());
            assets.bytes[116..120].fill(0); let header_crc = crc32(&assets.bytes[..128]); assets.bytes[116..120].copy_from_slice(&header_crc.to_le_bytes());
            result.replacements.push(("ampr_assets.index", assets.bytes));
        }
    }
    if refreshed > 0 || rebuilt { result.replacements.push(("ampr_emu.index", index.bytes)); }
    Ok(result)
}

/// Read-only preview: deterministic repairs are warnings, not blockers.
pub fn validate(root: &Path, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Vec<String>, String> {
    Ok(plan(root, checkpoint)?.warnings.into_iter().map(|warning| format!("Staging preview: {warning}")).collect())
}

/// Prepare all replacements before renaming any. Never truncate a hard-linked input.
pub fn prepare_staged(root: &Path, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<Vec<String>, String> {
    let plan = plan(root, checkpoint)?;
    let mut temporary = Vec::new();
    let result: Result<(), String> = (|| {
        for (name, bytes) in &plan.replacements {
            checkpoint()?;
            crate::storage::guard_bytes(root, bytes.len() as u64, "AMPR index refresh")?;
            let path = root.join(format!(".sspi-ampr-{}.tmp", uuid::Uuid::new_v4()));
            let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(failure)?;
            temporary.push((path, root.join(name)));
            for chunk in bytes.chunks(1024 * 1024) { checkpoint()?; output.write_all(chunk).map_err(failure)?; }
            output.sync_all().map_err(failure)?;
        }
        checkpoint()?;
        for (temporary, destination) in &temporary { fs::rename(temporary, destination).map_err(failure)?; }
        Ok(())
    })();
    for (path, _) in temporary { let _ = fs::remove_file(path); }
    result?;
    Ok(plan.warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../Build-Output/Windows Manager/host-test-work/ampr-index")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }
        fn index(&self, entries: &[(&str, u64)]) -> Vec<u8> {
            let count = entries.len();
            let slot_count = (count * 2).next_power_of_two().max(2);
            let mut records = Vec::new();
            let mut paths = Vec::new();
            let mut slots = vec![(0u64, 0u32, 0u32); slot_count];
            for (index, (path, size)) in entries.iter().enumerate() {
                records.extend_from_slice(&(paths.len() as u32).to_le_bytes());
                records.extend_from_slice(&(path.len() as u32).to_le_bytes());
                records.extend_from_slice(&size.to_le_bytes());
                records.extend_from_slice(&0i64.to_le_bytes());
                paths.extend_from_slice(path.as_bytes());
                paths.push(0);
                let hash = path_hash(path);
                let mut position = hash as usize & (slot_count - 1);
                while slots[position].1 != 0 {
                    position = (position + 1) & (slot_count - 1);
                }
                slots[position] = (hash, index as u32 + 1, 0);
            }
            let hash_offset = (48 + records.len() + paths.len() + 15) & !15;
            let mut data = b"AMPRIDX3".to_vec();
            data.extend_from_slice(&3u32.to_le_bytes());
            data.extend_from_slice(&24u32.to_le_bytes());
            data.extend_from_slice(&(count as u64).to_le_bytes());
            data.extend_from_slice(&(paths.len() as u64).to_le_bytes());
            data.extend_from_slice(&(hash_offset as u64).to_le_bytes());
            data.extend_from_slice(&16u32.to_le_bytes());
            data.extend_from_slice(&(slot_count as u32).to_le_bytes());
            data.extend(records);
            data.extend(paths);
            data.resize(hash_offset, 0);
            for (hash, id, flags) in slots {
                data.extend_from_slice(&hash.to_le_bytes());
                data.extend_from_slice(&id.to_le_bytes());
                data.extend_from_slice(&flags.to_le_bytes());
            }
            fs::write(self.0.join("ampr_emu.index"), &data).unwrap();
            data
        }
        fn check(&self) -> Result<Vec<String>, String> {
            validate(&self.0, &|| Ok(()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn validates_paths_and_sizes_without_touching_the_index() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("Data.bin"), [1, 2, 3]).unwrap();
        let original = fixture.index(&[("/app0/Data.bin", 3)]);
        assert!(fixture.check().unwrap().is_empty());
        assert_eq!(
            fs::read(fixture.0.join("ampr_emu.index")).unwrap(),
            original
        );
    }

    #[test]
    fn previews_stale_sizes_and_missing_loose_files_without_writing() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("data"), [1, 2]).unwrap();
        fixture.index(&[("/app0/data", 3)]);
        assert!(fixture.check().unwrap().iter().any(|s| s.contains("3 -> 2")));
        fixture.index(&[("/app0/missing", 3)]);
        assert!(fixture.check().unwrap().iter().any(|s| s.contains("kept missing loose")));
    }

    #[test]
    fn rebuilds_unknown_and_truncated_indexes() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("data"), b"data").unwrap();
        fs::write(fixture.0.join("ampr_emu.index"), b"AMPRIDX2").unwrap();
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap()[0].contains("rebuilt"));
        assert!(fixture.check().unwrap().is_empty());
        fs::write(fixture.0.join("ampr_emu.index"), b"AMPRIDX3").unwrap();
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap()[0].contains("rebuilt"));
        assert!(fixture.check().unwrap().is_empty());
    }

    #[test]
    fn rejects_paths_escaping_the_application_root() {
        let fixture = Fixture::new();
        for path in [
            "/app0/../outside",
            "/app0/C:/outside",
            "/app1/data",
            "/app0/a//b",
            "/app0/a.",
        ] {
            fixture.index(&[(path, 1)]);
            assert!(fixture.check().is_err(), "{path}");
        }
    }

    #[test]
    fn catches_hash_corruption_and_duplicate_names() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("data"), [1]).unwrap();
        let mut index = fixture.index(&[("/app0/data", 1)]);
        let offset = u64_at(&index, 32) as usize;
        let occupied = (0..2)
            .find(|slot| u32_at(&index, offset + slot * 16 + 8) != 0)
            .unwrap();
        index[offset + occupied * 16] ^= 1;
        assert!(parse_index(index, &|| Ok(())).is_err());
        let index = fixture.index(&[("/app0/data", 1), ("/app0/data", 1)]);
        assert!(parse_index(index, &|| Ok(())).err().unwrap().contains("duplicate"));
    }

    #[test]
    fn refuses_invalid_asset_manifests_without_modifying_either_index() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("ampr_assets.index"), b"manifest").unwrap();
        let original = fixture.index(&[("/app0/packed.bin", 123)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("unsupported"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), original);
    }

    #[test]
    fn forwards_cancellation() {
        let fixture = Fixture::new();
        assert_eq!(
            validate(&fixture.0, &|| Err("cancelled".into())).unwrap_err(),
            "cancelled"
        );
    }

    fn executable(size: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; size];
        bytes[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
        bytes[16..18].copy_from_slice(&0xfe10u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes[72..80].copy_from_slice(&120u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&((size - 120) as u64).to_le_bytes());
        bytes[104..112].copy_from_slice(&((size - 120) as u64).to_le_bytes());
        bytes
    }

    #[test]
    fn refreshes_earthion_size_delta_without_modifying_executable_or_source_index() {
        let source = Fixture::new();
        let staged = Fixture::new();
        let executable = executable(2_292_157);
        fs::write(source.0.join("eboot.bin"), &executable).unwrap();
        fs::write(source.0.join("data"), [1, 2, 3]).unwrap();
        let original = source.index(&[("/app0/data", 3), ("/app0/eboot.bin", 2_292_413)]);
        crate::fpkg::stage_source(&source.0, &staged.0).unwrap();
        assert!(staged.check().unwrap().iter().any(|s| s.contains("2292413 -> 2292157")));
        let messages = prepare_staged(&staged.0, &|| Ok(())).unwrap();
        assert!(messages.iter().any(|message| message.contains("2292413 -> 2292157")));
        let mut expected = original.clone();
        expected[80..88].copy_from_slice(&2_292_157u64.to_le_bytes());
        assert_eq!(fs::read(staged.0.join("ampr_emu.index")).unwrap(), expected);
        assert_eq!(fs::read(source.0.join("ampr_emu.index")).unwrap(), original);
        assert_eq!(fs::read(staged.0.join("eboot.bin")).unwrap(), executable);
        assert_eq!(fs::read(source.0.join("eboot.bin")).unwrap(), executable);
        assert!(staged.check().unwrap().is_empty());
        assert!(prepare_staged(&staged.0, &|| Ok(())).unwrap().is_empty());
    }

    #[test]
    fn refuses_size_refresh_for_a_truncated_executable() {
        let fixture = Fixture::new();
        let mut truncated = executable(512);
        truncated.truncate(256);
        fs::write(fixture.0.join("eboot.bin"), truncated).unwrap();
        let original = fixture.index(&[("/app0/eboot.bin", 512)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("extends beyond the file"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), original);
    }

    #[test]
    fn refreshes_non_executable_size_drift_without_touching_source() {
        let source = Fixture::new();
        let fixture = Fixture::new();
        fs::create_dir(source.0.join("_DUPLEX_")).unwrap();
        fs::write(source.0.join("_DUPLEX_/duplex.nfo"), vec![b'\n'; 4660]).unwrap();
        let original = source.index(&[("/app0/_DUPLEX_/duplex.nfo", 4489)]);
        crate::fpkg::stage_source(&source.0, &fixture.0).unwrap();
        let messages = prepare_staged(&fixture.0, &|| Ok(())).unwrap();
        assert!(messages.iter().any(|s| s.contains("4489 -> 4660")));
        let mut expected = original.clone(); expected[56..64].copy_from_slice(&4660u64.to_le_bytes());
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), expected);
        assert_eq!(fs::read(source.0.join("ampr_emu.index")).unwrap(), original);
        assert_eq!(fs::read(source.0.join("_DUPLEX_/duplex.nfo")).unwrap(), vec![b'\n'; 4660]);
    }

    #[test]
    fn refreshes_binary_assets_without_misclassifying_them_as_executables() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("data.bin"), [1, 2]).unwrap();
        fixture.index(&[("/app0/data.bin", 3)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap()[0].contains("3 -> 2"));
    }

    #[test]
    fn retains_missing_file_ids_and_rebuilds_corrupt_hashes_without_packs() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("eboot.bin"), executable(128)).unwrap();
        let original = fixture.index(&[("/app0/eboot.bin", 384), ("/app0/missing", 1)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap().iter().any(|s| s.contains("file ID 2")));
        let mut expected = original.clone(); expected[56..64].copy_from_slice(&128u64.to_le_bytes());
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), expected);
        let mut corrupt = fixture.index(&[("/app0/eboot.bin", 384)]);
        let offset = u64_at(&corrupt, 32) as usize;
        let occupied = (0..2).find(|slot| u32_at(&corrupt, offset + slot * 16 + 8) != 0).unwrap();
        corrupt[offset + occupied * 16] ^= 1;
        fs::write(fixture.0.join("ampr_emu.index"), &corrupt).unwrap();
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap()[0].contains("rebuilt"));
        assert!(fixture.check().unwrap().is_empty());
    }

    #[test]
    fn cancellation_during_refresh_keeps_original_and_removes_temporary_file() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("eboot.bin"), executable(128)).unwrap();
        let original = fixture.index(&[("/app0/eboot.bin", 384)]);
        let result = prepare_staged(&fixture.0, &|| {
            if fs::read_dir(&fixture.0).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(".sspi-ampr-")) {
                Err("cancelled".into())
            } else { Ok(()) }
        });
        assert_eq!(result.unwrap_err(), "cancelled");
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), original);
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
    }

    #[test]
    fn hashes_only_ascii_case_folding() {
        assert_eq!(path_hash("/app0/File"), path_hash("/APP0/file"));
        assert_ne!(path_hash("/app0/Ä"), path_hash("/app0/ä"));
        // Upstream uses different FNV-1a seeds for the two files (build_ampr_index.py and
        // ampr_pack_format.py); both values come from those scripts.
        assert_eq!(path_hash("/app0/Content/Paks/b.pak"), 0x4854_91ef_cbe9_823a);
        assert_eq!(asset_path_hash("/app0/Content/Paks/b.pak"), 0xc7a4_d4e0_8f3d_c768);
    }

    fn asset_fixture(fixture: &Fixture, loose: u64, packed: u64) -> Vec<u8> {
        fixture.index(&[("/app0/readme.nfo", loose), ("/app0/packed.dat", packed)]);
        let paths = b"/app0/readme.nfo\0/app0/packed.dat\0ampr_assets-000.dat\0";
        let mut bytes = vec![0u8; 128 + 2 * 48 + 12 + 32];
        bytes[..8].copy_from_slice(b"AMPRPAK4");
        for (offset, value) in [(8, 4u32), (12, 128), (20, 0x01020304), (56, 1), (60, 48), (64, 12), (68, 32)] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[24..40].fill(42);
        for (offset, value) in [(40, 2u64), (48, 1), (72, 128), (80, 224), (88, 236), (96, 268), (104, paths.len() as u64)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        for (id, path, size, path_offset) in [(0, "/app0/readme.nfo", loose, 0u32), (1, "/app0/packed.dat", packed, 17)] {
            let row = 128 + id * 48;
            bytes[row..row + 8].copy_from_slice(&asset_path_hash(path).to_le_bytes());
            bytes[row + 8..row + 16].copy_from_slice(&size.to_le_bytes());
            bytes[row + 32..row + 36].copy_from_slice(&path_offset.to_le_bytes());
            bytes[row + 36..row + 40].copy_from_slice(&(path.len() as u32).to_le_bytes());
            if id == 1 { bytes[row + 28] = 1; bytes[row + 40] = 1; bytes[row + 44] = 14; }
        }
        bytes[224..232].copy_from_slice(&4096u64.to_le_bytes());
        bytes[232..236].copy_from_slice(&((packed as u32 - 1) | (12 << 22)).to_le_bytes());
        bytes[236..244].copy_from_slice(&4096u64.to_le_bytes());
        bytes[244..252].copy_from_slice(&8192u64.to_le_bytes());
        bytes[252..256].copy_from_slice(&34u32.to_le_bytes());
        bytes[256..260].copy_from_slice(&19u32.to_le_bytes());
        bytes[260..264].copy_from_slice(&2u32.to_le_bytes());
        bytes[264..268].copy_from_slice(&4096u32.to_le_bytes());
        bytes.extend(paths);
        asset_crcs(&mut bytes);
        fs::write(fixture.0.join("ampr_assets.index"), &bytes).unwrap();
        let mut pack = vec![0u8; 8192]; pack[..8].copy_from_slice(b"AMPRDAT3");
        pack[8..12].copy_from_slice(&3u32.to_le_bytes()); pack[12..16].copy_from_slice(&64u32.to_le_bytes());
        pack[20..24].copy_from_slice(&2u32.to_le_bytes()); pack[24..40].fill(42);
        pack[40..48].copy_from_slice(&4096u64.to_le_bytes()); pack[48..56].copy_from_slice(&4096u64.to_le_bytes());
        let crc = crc32(&pack[..64]); pack[56..60].copy_from_slice(&crc.to_le_bytes());
        fs::write(fixture.0.join("ampr_assets-000.dat"), pack).unwrap();
        bytes
    }

    fn asset_crcs(bytes: &mut [u8]) {
        let crc = crc32(&bytes[128..]); bytes[112..116].copy_from_slice(&crc.to_le_bytes());
        bytes[116..120].fill(0); let crc = crc32(&bytes[..128]); bytes[116..120].copy_from_slice(&crc.to_le_bytes());
    }

    #[test]
    fn asset_manifest_refreshes_loose_sizes_and_preserves_packed_ids_and_source_bytes() {
        let source = Fixture::new(); let staged = Fixture::new();
        let original = asset_fixture(&source, 3, 123);
        let old_index = fs::read(source.0.join("ampr_emu.index")).unwrap();
        fs::write(source.0.join("readme.nfo"), b"changed").unwrap();
        crate::fpkg::stage_source(&source.0, &staged.0).unwrap();
        let warnings = prepare_staged(&staged.0, &|| Ok(())).unwrap();
        assert!(warnings.iter().any(|s| s.contains("asset manifest refreshed too")));
        let mut expected = original.clone(); expected[136..144].copy_from_slice(&7u64.to_le_bytes()); asset_crcs(&mut expected);
        assert_eq!(fs::read(staged.0.join("ampr_assets.index")).unwrap(), expected);
        assert_eq!(fs::read(source.0.join("ampr_assets.index")).unwrap(), original);
        assert_eq!(fs::read(source.0.join("ampr_emu.index")).unwrap(), old_index);
        assert!(staged.check().unwrap().iter().all(|s| !s.contains("refreshed")));
    }

    #[test]
    fn packed_mismatches_and_invalid_index_never_trigger_id_rebuild() {
        let fixture = Fixture::new(); asset_fixture(&fixture, 3, 123);
        fs::write(fixture.0.join("readme.nfo"), b"new text").unwrap();
        let original = fixture.index(&[("/app0/readme.nfo", 3), ("/app0/packed.dat", 124)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("packed size mismatch"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), original);
        fs::write(fixture.0.join("ampr_emu.index"), b"invalid").unwrap();
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("cannot renumber"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), b"invalid");
    }

    #[test]
    fn asset_validation_checks_crc_chunk_bounds_and_pack_build_id() {
        let fixture = Fixture::new(); let original = asset_fixture(&fixture, 3, 123);
        let mut corrupt = original.clone(); corrupt[136] ^= 1;
        assert!(parse_assets(&fixture.0, corrupt, &|| Ok(())).err().unwrap().contains("CRC"));
        let mut corrupt = original.clone(); corrupt[224..232].copy_from_slice(&8192u64.to_le_bytes()); asset_crcs(&mut corrupt);
        assert!(parse_assets(&fixture.0, corrupt, &|| Ok(())).err().unwrap().contains("range"));
        let pack = fixture.0.join("ampr_assets-000.dat"); let mut data = fs::read(&pack).unwrap();
        data[24] ^= 1; data[56..60].fill(0); let crc = crc32(&data[..64]); data[56..60].copy_from_slice(&crc.to_le_bytes()); fs::write(pack, data).unwrap();
        assert!(parse_assets(&fixture.0, original, &|| Ok(())).err().unwrap().contains("build ID"));
    }

    #[test]
    fn missing_loose_asset_records_keep_ids_and_warn() {
        let fixture = Fixture::new(); let original = asset_fixture(&fixture, 3, 123);
        let warnings = prepare_staged(&fixture.0, &|| Ok(())).unwrap();
        assert!(warnings.iter().any(|s| s.contains("kept missing loose file /app0/readme.nfo (file ID 1)")));
        assert_eq!(fs::read(fixture.0.join("ampr_assets.index")).unwrap(), original);
    }

    #[test]
    fn repair_warnings_are_capped_with_totals() {
        let fixture = Fixture::new(); let names = (0..24).map(|i| format!("/app0/{i}.nfo")).collect::<Vec<_>>();
        for name in &names { fs::write(fixture.0.join(relative_path(name).unwrap()), b"changed").unwrap(); }
        fixture.index(&names.iter().map(|s| (s.as_str(), 1)).collect::<Vec<_>>());
        let warnings = prepare_staged(&fixture.0, &|| Ok(())).unwrap();
        assert_eq!(warnings.len(), WARNING_LIMIT + 1);
        assert!(warnings.last().unwrap().contains("24 loose size records in total"));
    }

    #[test]
    #[ignore = "requires SSPI_AMPR_DUMP; stages read-only D:/Games input in a disposable crew-* folder"]
    fn local_dump_staging_preflight_preserves_source_index() {
        use sha2::{Digest, Sha256};
        let source = PathBuf::from(std::env::var_os("SSPI_AMPR_DUMP").expect("SSPI_AMPR_DUMP"));
        let scratch_root = PathBuf::from("D:/Games/packaged");
        let staging = scratch_root.join(format!("crew-ampr-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&staging).unwrap();
        struct Cleanup(PathBuf, PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) {
            let path = fs::canonicalize(&self.0).unwrap(); let root = fs::canonicalize(&self.1).unwrap();
            assert!(path.starts_with(root) && path.file_name().unwrap().to_string_lossy().starts_with("crew-ampr-"));
            fs::remove_dir_all(path).unwrap();
        } }
        let _cleanup = Cleanup(staging.clone(), scratch_root);
        let before = Sha256::digest(fs::read(source.join("ampr_emu.index")).unwrap());
        let started = std::time::Instant::now();
        let doctor = crate::fpkg_doctor::inspect(&source, &|| Ok(())).unwrap();
        assert!(doctor.blockers().is_empty(), "{:?}", doctor.blockers());
        let staged = crate::fpkg::stage_source_with_repairs(&source, &staging, &doctor.repairs, &|| Ok(())).unwrap();
        let warnings = prepare_staged(&staging, &|| Ok(())).unwrap();
        let preflight = crate::fpkg::preflight_staged(&staging, 2_000_000, &staged).unwrap();
        assert!(preflight.ok(), "{:?}", preflight.blockers);
        for warning in warnings.iter().chain(preflight.warnings.iter()) { println!("{warning}"); }
        let after = Sha256::digest(fs::read(source.join("ampr_emu.index")).unwrap()); assert_eq!(before, after);
        println!("STAGING PASS: {} files, {} bytes, {:.3}s, {} Kraken workers; source index SHA256 {:x} unchanged", preflight.file_count, preflight.total_bytes, started.elapsed().as_secs_f64(), crate::fpkg::kraken_workers(), after);
    }
}
