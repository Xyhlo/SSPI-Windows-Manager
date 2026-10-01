//! Reads an exFAT image back from disk and checks it against the exFAT specification and the
//! expected contents: both boot regions and their checksums, the up-case table, every entry set's
//! checksum and name hash, FAT chains, cluster ownership against the allocation bitmap and, when
//! hashes are supplied, every file's data. It parses the raw bytes itself and shares no layout
//! state with the writer.

use super::{boot_checksum, checksum32, expand_upcase, set_checksum, CLUSTER, FLAG_NO_FAT_CHAIN, SECTOR};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) path: String,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub(crate) entries: Vec<Entry>,
    pub(crate) label: String,
    pub(crate) cluster_count: u32,
    pub(crate) used_clusters: u64,
    pub(crate) hashed_bytes: u64,
}

struct Image {
    file: File,
    heap: u64,
    count: u32,
    fat: Vec<u32>,
    used: Vec<u8>, // 0 free, 1 owned; ownership is recorded while walking
}

fn le16(b: &[u8], at: usize) -> u16 { u16::from_le_bytes(b[at..at + 2].try_into().unwrap()) }
fn le32(b: &[u8], at: usize) -> u32 { u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) }
fn le64(b: &[u8], at: usize) -> u64 { u64::from_le_bytes(b[at..at + 8].try_into().unwrap()) }

impl Image {
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        self.file.seek(SeekFrom::Start(offset)).and_then(|_| self.file.read_exact(buffer))
            .map_err(|e| format!("Image read at {offset} failed: {e}"))
    }
    fn cluster_at(&self, cluster: u32) -> u64 { self.heap + (cluster as u64 - 2) * CLUSTER }

    /// Clusters of an allocation, from its FAT chain or as a contiguous run; each may be owned once.
    fn claim(&mut self, first: u32, length: u64, contiguous: bool, what: &str) -> Result<Vec<u32>, String> {
        let needed = length.div_ceil(CLUSTER);
        if needed == 0 { return if first == 0 { Ok(Vec::new()) } else { Err(format!("{what}: empty allocation names cluster {first}")) }; }
        let mut clusters = Vec::with_capacity(needed as usize);
        let mut cluster = first;
        for k in 0..needed {
            if cluster < 2 || cluster as u64 >= self.count as u64 + 2 { return Err(format!("{what}: cluster {cluster} is outside the heap")); }
            if self.used[cluster as usize - 2] != 0 { return Err(format!("{what}: cluster {cluster} is already in use")); }
            self.used[cluster as usize - 2] = 1;
            clusters.push(cluster);
            if k + 1 < needed {
                cluster = if contiguous { cluster + 1 } else {
                    let next = self.fat[cluster as usize];
                    if next < 2 || next >= 0xFFFF_FFF7 { return Err(format!("{what}: FAT chain ends after {} of {needed} clusters", k + 1)); }
                    next
                };
            } else if !contiguous && self.fat[cluster as usize] != 0xFFFF_FFFF {
                return Err(format!("{what}: FAT chain does not end where its length does"));
            }
        }
        Ok(clusters)
    }

    fn read_clusters(&mut self, clusters: &[u32], length: u64) -> Result<Vec<u8>, String> {
        let mut data = vec![0u8; clusters.len() * CLUSTER as usize];
        for (k, &c) in clusters.iter().enumerate() {
            let offset = self.cluster_at(c);
            self.read_at(offset, &mut data[k * CLUSTER as usize..(k + 1) * CLUSTER as usize])?;
        }
        data.truncate(length as usize);
        Ok(data)
    }
}

/// `expected`: every file and folder the image must hold. `hashes`: SHA-256 per file path to
/// check the data too (skip the payload check with an empty map). `control` receives the bytes
/// hashed so far and the current file; returning an error stops the check.
pub(crate) fn verify(path: &Path, expected: &[Entry], hashes: &HashMap<String, [u8; 32]>,
                     control: &mut dyn FnMut(u64, &str) -> Result<(), String>) -> Result<Report, String> {
    let mut file = File::open(path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    let mut regions = vec![0u8; 24 * SECTOR as usize];
    file.read_exact(&mut regions).map_err(|e| format!("The image is too short for exFAT boot regions: {e}"))?;
    let (main, backup) = regions.split_at(12 * SECTOR as usize);
    if main != backup { return Err("The backup boot region differs from the main one.".into()); }
    let boot = &main[..SECTOR as usize];
    if boot[..3] != [0xEB, 0x76, 0x90] || &boot[3..11] != b"EXFAT   " || boot[510..512] != [0x55, 0xAA] {
        return Err("The boot sector is not exFAT.".into());
    }
    if boot[11..64].iter().any(|&b| b != 0) { return Err("The boot sector's MustBeZero field is not zero.".into()); }
    for sector in 1..=8 {
        let s = &main[sector * SECTOR as usize..(sector + 1) * SECTOR as usize];
        if s[508..512] != [0, 0, 0x55, 0xAA] { return Err(format!("Extended boot sector {sector} lacks its signature.")); }
    }
    let sum = boot_checksum(&main[..11 * SECTOR as usize]);
    if main[11 * SECTOR as usize..].chunks_exact(4).any(|w| le32(w, 0) != sum) { return Err("The boot checksum sector does not match.".into()); }
    if boot[108] != 9 || boot[109] != 7 || boot[110] != 1 || le16(boot, 104) != 0x0100 {
        return Err("The image does not use 512-byte sectors, 64 KiB clusters and one FAT.".into());
    }
    let (volume, fat_offset, fat_length, heap, count, root) = (le64(boot, 72), le32(boot, 80), le32(boot, 84), le32(boot, 88), le32(boot, 92), le32(boot, 96));
    if le64(boot, 64) != 0 || volume * SECTOR != length { return Err(format!("VolumeLength says {} bytes; the file has {length}.", volume * SECTOR)); }
    if fat_offset < 24 || (fat_length as u64) * SECTOR < (count as u64 + 2) * 4 || (heap as u64) < fat_offset as u64 + fat_length as u64
        || heap as u64 * SECTOR + count as u64 * CLUSTER > length || heap as u64 * SECTOR % CLUSTER != 0 {
        return Err("The FAT and cluster heap geometry is inconsistent.".into());
    }
    let mut fat_bytes = vec![0u8; (count as usize + 2) * 4];
    file.seek(SeekFrom::Start(fat_offset as u64 * SECTOR)).and_then(|_| file.read_exact(&mut fat_bytes)).map_err(|e| e.to_string())?;
    let fat: Vec<u32> = fat_bytes.chunks_exact(4).map(|w| le32(w, 0)).collect();
    if fat[0] != 0xFFFF_FFF8 || fat[1] != 0xFFFF_FFFF { return Err("The first two FAT entries are not the media and end markers.".into()); }
    let mut image = Image { file, heap: heap as u64 * SECTOR, count, fat, used: vec![0; count as usize] };

    // Root directory: its length is its FAT chain.
    let mut root_clusters = Vec::new();
    let mut cluster = root;
    loop {
        if cluster < 2 || cluster as u64 >= count as u64 + 2 || root_clusters.len() > count as usize { return Err("The root directory chain is broken.".into()); }
        root_clusters.push(cluster);
        let next = image.fat[cluster as usize];
        if next == 0xFFFF_FFFF { break; }
        cluster = next;
    }
    let root_claim = image.claim(root, root_clusters.len() as u64 * CLUSTER, false, "root directory")?;
    let root_data = image.read_clusters(&root_claim, root_claim.len() as u64 * CLUSTER)?;

    let (mut bitmap, mut upcase, mut label) = (None, None, None);
    for entry in root_data.chunks_exact(32) {
        match entry[0] {
            0x00 => break,
            0x81 => bitmap = Some((le32(entry, 20), le64(entry, 24))),
            0x82 => upcase = Some((le32(entry, 4), le32(entry, 20), le64(entry, 24))),
            0x83 => {
                let n = entry[1] as usize;
                if n > 11 { return Err("The volume label is longer than 11 characters.".into()); }
                label = Some(String::from_utf16_lossy(&(0..n).map(|j| le16(entry, 2 + 2 * j)).collect::<Vec<_>>()));
            }
            _ => {}
        }
    }
    let (bitmap_first, bitmap_bytes) = bitmap.ok_or("The root directory has no allocation bitmap entry.")?;
    let (table_sum, upcase_first, upcase_bytes) = upcase.ok_or("The root directory has no up-case table entry.")?;
    if bitmap_bytes < (count as u64).div_ceil(8) { return Err("The allocation bitmap is shorter than the cluster heap.".into()); }
    let bitmap_claim = image.claim(bitmap_first, bitmap_bytes, false, "allocation bitmap")?;
    let bitmap_data = image.read_clusters(&bitmap_claim, bitmap_bytes)?;
    let upcase_claim = image.claim(upcase_first, upcase_bytes, false, "up-case table")?;
    let upcase_data = image.read_clusters(&upcase_claim, upcase_bytes)?;
    if checksum32(&upcase_data, |_| false) != table_sum { return Err("The up-case table checksum does not match its data.".into()); }
    if upcase_data.len() % 2 != 0 { return Err("The up-case table has an odd length.".into()); }
    let map = expand_upcase(&upcase_data.chunks_exact(2).map(|w| le16(w, 0)).collect::<Vec<_>>()).ok_or("The up-case table is malformed.")?;

    let mut found = Vec::new();
    let mut hashed = 0u64;
    let mut pending = vec![(String::new(), root_data)];
    while let Some((prefix, data)) = pending.pop() {
        let mut i = 0;
        let mut names = std::collections::HashSet::new();
        while i + 32 <= data.len() {
            let kind = data[i];
            if kind == 0x00 { break; }
            if kind & 0x80 == 0 || kind != 0x85 { i += 32; continue; } // unused, system or benign entries
            let secondary = data[i + 1] as usize;
            if secondary < 2 || i + 32 * (secondary + 1) > data.len() { return Err(format!("{prefix}: an entry set runs past its directory")); }
            let set = &data[i..i + 32 * (secondary + 1)];
            if set_checksum(set) != le16(set, 2) { return Err(format!("{prefix}: entry set checksum mismatch")); }
            let stream = &set[32..64];
            if stream[0] != 0xC0 { return Err(format!("{prefix}: file entry not followed by a stream extension")); }
            let name_len = stream[3] as usize;
            if name_len == 0 || secondary - 1 != name_len.div_ceil(15) { return Err(format!("{prefix}: name length disagrees with the entry count")); }
            let mut name = Vec::with_capacity(name_len);
            for k in 0..secondary - 1 {
                let entry = &set[64 + 32 * k..96 + 32 * k];
                if entry[0] != 0xC1 { return Err(format!("{prefix}: missing file name entry")); }
                for j in 0..15 { if name.len() < name_len { name.push(le16(entry, 2 + 2 * j)); } }
            }
            let hash = name.iter().fold(0u16, |s, &u| { let [lo, hi] = map[u as usize].to_le_bytes(); s.rotate_right(1).wrapping_add(lo as u16).rotate_right(1).wrapping_add(hi as u16) });
            let text = String::from_utf16(&name).map_err(|_| format!("{prefix}: a name is not valid UTF-16"))?;
            if hash != le16(stream, 4) { return Err(format!("{prefix}/{text}: name hash mismatch")); }
            if !names.insert(name.iter().map(|&u| map[u as usize]).collect::<Vec<_>>()) { return Err(format!("{prefix}/{text}: duplicate name")); }
            let path = if prefix.is_empty() { text } else { format!("{prefix}/{}", text) };
            let is_dir = le16(set, 4) & super::ATTR_DIRECTORY != 0;
            let (flags, valid, first, size) = (stream[1], le64(stream, 8), le32(stream, 20), le64(stream, 24));
            if flags & 0x01 == 0 { return Err(format!("{path}: AllocationPossible is clear")); }
            if valid > size { return Err(format!("{path}: ValidDataLength exceeds DataLength")); }
            let contiguous = flags & FLAG_NO_FAT_CHAIN != 0;
            let clusters = image.claim(first, size, contiguous, &path)?;
            if is_dir {
                if size % CLUSTER != 0 || size == 0 { return Err(format!("{path}: directory length is not whole clusters")); }
                let child = image.read_clusters(&clusters, size)?;
                pending.push((path.clone(), child));
            } else if let Some(expected) = hashes.get(&path) {
                let mut digest = Sha256::new();
                let mut left = size;
                let mut buffer = vec![0u8; 8 * 1024 * 1024];
                let mut k = 0;
                while left > 0 {
                    control(hashed + size - left, &path)?;
                    // Read the longest run of consecutive clusters at once.
                    let mut run = 1;
                    while k + run < clusters.len() && clusters[k + run] == clusters[k] + run as u32 && (run + 1) * CLUSTER as usize <= buffer.len() { run += 1; }
                    let want = (run as u64 * CLUSTER).min(left) as usize;
                    let offset = image.cluster_at(clusters[k]);
                    image.read_at(offset, &mut buffer[..want])?;
                    digest.update(&buffer[..want]);
                    left -= want as u64;
                    k += run;
                }
                if <[u8; 32]>::from(digest.finalize()) != *expected { return Err(format!("{path}: data differs from the source file")); }
                hashed += size;
            }
            found.push(Entry { path, is_dir, size: if is_dir { 0 } else { size } });
            i += 32 * (secondary + 1);
        }
    }

    // Every owned cluster must be marked in the bitmap, and nothing else.
    for k in 0..count as usize {
        let bit = bitmap_data[k / 8] >> (k % 8) & 1;
        if bit != image.used[k] { return Err(format!("Allocation bitmap disagrees at cluster {}", k + 2)); }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    let mut wanted = expected.to_vec();
    wanted.sort_by(|a, b| a.path.cmp(&b.path));
    if found != wanted {
        let missing = wanted.iter().find(|e| !found.contains(e)).map(|e| e.path.clone());
        let extra = found.iter().find(|e| !wanted.contains(e)).map(|e| e.path.clone());
        return Err(format!("The image holds different files than expected (missing {missing:?}, unexpected {extra:?})."));
    }
    let used_clusters = image.used.iter().filter(|&&u| u != 0).count() as u64;
    Ok(Report { entries: found, label: label.unwrap_or_default(), cluster_count: count, used_clusters, hashed_bytes: hashed })
}

/// The expected listing for a plan.
pub(crate) fn expected(plan: &super::Plan) -> Vec<Entry> {
    plan.nodes.iter().skip(1).map(|n| Entry { path: n.path.clone(), is_dir: n.is_dir, size: n.size }).collect()
}
