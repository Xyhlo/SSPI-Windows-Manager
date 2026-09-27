//! AMPRIDX3 validation and metadata reconciliation in disposable package staging.
//! Format: drakmor/ampr_emu cfa85df, tools/build_ampr_index.py and src/ampr_emu_index.cpp.

use std::fs::{self, File, Metadata};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

const HEADER_BYTES: u64 = 48;
const RECORD_BYTES: u64 = 24;
const SLOT_BYTES: u64 = 16;
const MAX_ENTRIES: u64 = 2_000_000;
const MAX_PATH_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SLOTS: u64 = 4_194_304;
const MAX_LOOKUP_PROBES: u64 = 64_000_000;

fn failure(message: impl std::fmt::Display) -> String {
    format!("AMPR index: {message}")
}

fn linked(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
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
        if linked(&metadata) {
            return Err(failure(format!(
                "linked or reparse path is unsupported: {relative}"
            )));
        }
        if position + 1 == components.len() {
            if !metadata.is_file() {
                return Err(failure(format!(
                    "indexed path is not a regular file: {relative}"
                )));
            }
            return Ok(Some(metadata));
        }
        if !metadata.is_dir() {
            return Err(failure(format!(
                "indexed parent is not a directory: {relative}"
            )));
        }
    }
    Ok(None)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn read<const N: usize>(reader: &mut impl Read) -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes).map_err(failure)?;
    Ok(bytes)
}

fn key(path: &str) -> Vec<u8> {
    path.bytes()
        .map(|byte| match byte {
            b'\\' => b'/',
            b'A'..=b'Z' => byte + 32,
            _ => byte,
        })
        .collect()
}

fn path_hash(path: &str) -> u64 {
    let mut hash = 1_469_598_103_934_665_603u64;
    for byte in key(path) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211);
    }
    hash.max(1)
}

fn relative_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");
    if !normalized
        .as_bytes()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"/app0/"))
    {
        return Err(failure(format!("path must be below /app0: {path}")));
    }
    let relative = &normalized[6..];
    if relative.is_empty()
        || relative.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.contains(':')
                || part.chars().any(char::is_control)
                || part.ends_with('.')
                || part.ends_with(' ')
        })
    {
        return Err(failure(format!(
            "invalid or ambiguous indexed path: {path}"
        )));
    }
    Ok(relative.to_string())
}

struct Record {
    path_offset: u64,
    path_length: usize,
    hash: u64,
}

fn record_path(reader: &mut File, record: &Record) -> Result<String, String> {
    reader
        .seek(SeekFrom::Start(record.path_offset))
        .map_err(failure)?;
    let mut bytes = vec![0; record.path_length + 1];
    reader.read_exact(&mut bytes).map_err(failure)?;
    if bytes.pop() != Some(0) || bytes.contains(&0) {
        return Err(failure("path is not a single NUL-terminated string"));
    }
    String::from_utf8(bytes).map_err(|_| failure("indexed path is not UTF-8"))
}

#[derive(Clone, Copy)]
struct Slot {
    hash: u64,
    id: u32,
}

struct SizeRefresh {
    offset: u64,
    path: String,
    previous: u64,
    current: u64,
}

#[derive(Default)]
struct Inspection {
    warnings: Vec<String>,
    refreshes: Vec<SizeRefresh>,
}

/// Checks index structure and existing loose files without rewriting the index or source.
/// Asset-pack manifests require their matching upstream verifier for packed-file coverage.
pub fn validate(
    root: &Path,
    checkpoint: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<String>, String> {
    Ok(inspect(root, "ampr_emu.index", false, checkpoint)?.warnings)
}

/// Refresh only structurally valid loose executables after a backport/doctor change.
/// Preserve paths, record order, file IDs, hash slots and timestamps. A replacement
/// file breaks any staging hard link; never write through to the extracted input.
pub fn prepare_staged(
    root: &Path,
    checkpoint: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<String>, String> {
    let inspection = inspect(root, "ampr_emu.index", true, checkpoint)?;
    if inspection.refreshes.is_empty() {
        return Ok(inspection.warnings);
    }
    let original = root.join("ampr_emu.index");
    let temporary_name = format!(".sspi-ampr-{}.tmp", uuid::Uuid::new_v4());
    let temporary = root.join(&temporary_name);
    let result = (|| {
        checkpoint()?;
        crate::storage::guard_bytes(root, fs::metadata(&original).map_err(failure)?.len(), "AMPR index refresh")?;
        let mut input = File::open(&original).map_err(failure)?;
        let mut output = fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(failure)?;
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            checkpoint()?;
            let length = input.read(&mut buffer).map_err(failure)?;
            if length == 0 { break; }
            output.write_all(&buffer[..length]).map_err(failure)?;
        }
        for refresh in &inspection.refreshes {
            checkpoint()?;
            output.seek(SeekFrom::Start(refresh.offset)).map_err(failure)?;
            output.write_all(&refresh.current.to_le_bytes()).map_err(failure)?;
        }
        output.sync_all().map_err(failure)?;
        drop(output);
        drop(input);
        // Validate the complete replacement before it can reach the builder.
        inspect(root, &temporary_name, false, checkpoint)?;
        checkpoint()?;
        fs::rename(&temporary, &original).map_err(failure)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let mut warnings = inspection.warnings;
    for refresh in inspection.refreshes.iter().take(8) {
        warnings.push(format!(
            "AMPR index refreshed validated executable {}: {} -> {} bytes. Executable bytes, file IDs and hash lookups are preserved.",
            refresh.path, refresh.previous, refresh.current
        ));
    }
    if inspection.refreshes.len() > 8 {
        warnings.push(format!("AMPR index refreshed {} executable size records in total.", inspection.refreshes.len()));
    }
    Ok(warnings)
}

fn inspect(
    root: &Path,
    index_name: &str,
    refresh_executables: bool,
    checkpoint: &dyn Fn() -> Result<(), String>,
) -> Result<Inspection, String> {
    checkpoint()?;
    let root_metadata = fs::symlink_metadata(root).map_err(failure)?;
    if !root_metadata.is_dir() || linked(&root_metadata) {
        return Err(failure("application root must be an ordinary directory"));
    }
    let root = fs::canonicalize(root).map_err(failure)?;
    let Some(index_metadata) = regular_path(&root, index_name)? else {
        return Ok(Inspection::default());
    };
    let index_path = root.join(index_name);
    let mut reader = BufReader::new(File::open(&index_path).map_err(failure)?);
    let mut magic = [0; 8];
    let magic_length = reader.read(&mut magic).map_err(failure)?;
    if magic_length != 8 || magic != *b"AMPRIDX3" {
        return Ok(Inspection { warnings: vec![
            "AMPR index format is not AMPRIDX3; preserved without validating its file coverage."
                .into(),
        ], ..Default::default() });
    }
    reader.seek(SeekFrom::Start(0)).map_err(failure)?;
    let header = read::<48>(&mut reader)?;
    let count = u64_at(&header, 16);
    let path_bytes = u64_at(&header, 24);
    let hash_offset = u64_at(&header, 32);
    let slot_count = u64::from(u32_at(&header, 44));
    if u32_at(&header, 8) != 3 || u32_at(&header, 12) != 24 || u32_at(&header, 40) != 16 {
        return Err(failure("invalid AMPRIDX3 version or record sizes"));
    }
    if count == 0
        || count > MAX_ENTRIES
        || path_bytes == 0
        || path_bytes > MAX_PATH_BYTES
        || slot_count < 2
        || !slot_count.is_power_of_two()
        || slot_count < count
    {
        return Err(failure("invalid AMPRIDX3 record, path or hash-slot counts"));
    }
    if slot_count > MAX_SLOTS {
        return Err(failure("hash table exceeds the bounded validation limit"));
    }
    let paths_start = HEADER_BYTES + count * RECORD_BYTES;
    let paths_end = paths_start + path_bytes;
    let expected_end = hash_offset
        .checked_add(slot_count * SLOT_BYTES)
        .ok_or_else(|| failure("hash table size overflow"))?;
    if hash_offset < ((paths_end + 7) & !7)
        || hash_offset % 8 != 0
        || expected_end > index_metadata.len()
    {
        return Err(failure("truncated or overlapping AMPRIDX3 sections"));
    }

    let asset_packs = regular_path(&root, "ampr_assets.index")?.is_some();
    let mut packed_missing = 0usize;
    let mut paths = File::open(&index_path).map_err(failure)?;
    let mut records = Vec::with_capacity(count as usize);
    let mut refreshes = Vec::new();
    for id in 0..count {
        checkpoint()?;
        let bytes = read::<24>(&mut reader)?;
        let offset = u64::from(u32_at(&bytes, 0));
        let length = u32_at(&bytes, 4) as usize;
        if length == 0 || length >= 4096 || offset + length as u64 >= path_bytes {
            return Err(failure(format!(
                "invalid path bounds for file ID {}",
                id + 1
            )));
        }
        let mut record = Record {
            path_offset: paths_start + offset,
            path_length: length,
            hash: 0,
        };
        let path = record_path(&mut paths, &record)?;
        let relative = relative_path(&path)?;
        match regular_path(&root, &relative)? {
            Some(metadata) if metadata.len() != u64_at(&bytes, 8) => {
                let previous = u64_at(&bytes, 8);
                let executable = relative.eq_ignore_ascii_case("eboot.bin")
                    || Path::new(&relative).extension().and_then(|value| value.to_str())
                        .is_some_and(|extension| ["bin", "elf", "prx", "sprx", "self"].iter().any(|known| extension.eq_ignore_ascii_case(known)));
                if !refresh_executables || !executable || asset_packs {
                    return Err(failure(format!(
                        "stale size for {path}: index {previous} bytes, file {} bytes. Automatic refresh is limited to validated loose executables without an asset-pack manifest; other mismatches require the matching index/game set or AMPR tools.",
                        metadata.len()
                    )));
                }
                crate::fpkg_doctor::validate_executable(&root.join(&relative))
                    .map_err(|error| failure(format!("cannot refresh {path}: {error}. Executable and index were preserved.")))?;
                refreshes.push(SizeRefresh { offset: HEADER_BYTES + id * RECORD_BYTES + 8, path: path.clone(), previous, current: metadata.len() });
            }
            None if asset_packs => packed_missing += 1,
            None => {
                return Err(failure(format!(
                    "stale index: {path} is missing from the staged application"
                )))
            }
            _ => {}
        }
        record.hash = path_hash(&path);
        records.push(record);
    }

    reader.seek(SeekFrom::Start(hash_offset)).map_err(failure)?;
    let mut slots = Vec::with_capacity(slot_count as usize);
    let mut seen = vec![false; count as usize];
    for position in 0..slot_count {
        if position % 1024 == 0 {
            checkpoint()?;
        }
        let bytes = read::<16>(&mut reader)?;
        let hash = u64_at(&bytes, 0);
        let id = u32_at(&bytes, 8);
        let flags = u32_at(&bytes, 12);
        if id == 0 {
            if hash != 0 || flags != 0 {
                return Err(failure("invalid empty hash slot"));
            }
        } else {
            let record_index = (id - 1) as usize;
            if record_index >= records.len()
                || flags & !1 != 0
                || seen[record_index]
                || records[record_index].hash != hash
            {
                return Err(failure(
                    "invalid, duplicate or mismatched hash-slot file ID",
                ));
            }
            seen[record_index] = true;
        }
        slots.push(Slot { hash, id });
    }
    if seen.iter().any(|present| !present) {
        return Err(failure("hash table omits an indexed file ID"));
    }
    let mask = slots.len() - 1;
    let mut probes = 0u64;
    for (record_index, record) in records.iter().enumerate() {
        checkpoint()?;
        let mut position = record.hash as usize & mask;
        let mut own_key = None;
        loop {
            probes += 1;
            if probes > MAX_LOOKUP_PROBES {
                return Err(failure("hash chains exceed the bounded validation limit"));
            }
            let slot = slots[position];
            if slot.id == 0 {
                return Err(failure("broken hash lookup chain"));
            }
            if slot.id as usize == record_index + 1 {
                break;
            }
            if slot.hash == record.hash {
                let expected = match &own_key {
                    Some(value) => value,
                    None => own_key.insert(key(&record_path(&mut paths, record)?)),
                };
                let candidate = key(&record_path(&mut paths, &records[(slot.id - 1) as usize])?);
                if *expected == candidate {
                    return Err(failure("duplicate case-insensitive application paths"));
                }
            }
            position = (position + 1) & mask;
            if probes % 1024 == 0 {
                checkpoint()?;
            }
        }
    }
    checkpoint()?;
    let mut warnings = Vec::new();
    if asset_packs {
        warnings.push(format!(
            "AMPR asset manifest detected; {packed_missing} indexed files are not loose. Index structure and present file sizes were checked, but packed-file coverage and matching asset build IDs require the AMPR pack verifier. The existing index was preserved."
        ));
    }
    Ok(Inspection { warnings, refreshes })
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
    fn rejects_stale_sizes_and_missing_loose_files() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("data"), [1, 2]).unwrap();
        fixture.index(&[("/app0/data", 3)]);
        assert!(fixture.check().unwrap_err().contains("stale size"));
        fixture.index(&[("/app0/missing", 3)]);
        assert!(fixture.check().unwrap_err().contains("missing"));
    }

    #[test]
    fn distinguishes_unknown_from_truncated_known_format() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("ampr_emu.index"), b"AMPRIDX2").unwrap();
        assert_eq!(fixture.check().unwrap().len(), 1);
        fs::write(fixture.0.join("ampr_emu.index"), b"AMPRIDX3").unwrap();
        assert!(fixture.check().is_err());
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
        fs::write(fixture.0.join("ampr_emu.index"), index).unwrap();
        assert!(fixture.check().is_err());
        fixture.index(&[("/app0/data", 1), ("/app0/data", 1)]);
        assert!(fixture.check().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn preserves_asset_pack_file_ids_and_reports_limited_coverage() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("ampr_assets.index"), b"manifest").unwrap();
        fixture.index(&[("/app0/packed.bin", 123)]);
        assert!(fixture.check().unwrap()[0].contains("1 indexed files are not loose"));
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
        assert!(staged.check().unwrap_err().contains("stale size"));
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
    fn refuses_unverified_data_and_asset_pack_size_changes() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("data"), [1, 2]).unwrap();
        fixture.index(&[("/app0/data", 3)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("stale size"));
        fs::write(fixture.0.join("eboot.bin"), executable(128)).unwrap();
        fs::write(fixture.0.join("ampr_assets.index"), b"manifest").unwrap();
        let original = fixture.index(&[("/app0/eboot.bin", 384), ("/app0/packed.bin", 42)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("asset-pack manifest"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), original);
    }

    #[test]
    fn validates_all_records_and_hashes_before_refreshing_any_size() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("eboot.bin"), executable(128)).unwrap();
        let original = fixture.index(&[("/app0/eboot.bin", 384), ("/app0/missing", 1)]);
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("missing"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), original);
        let mut corrupt = fixture.index(&[("/app0/eboot.bin", 384)]);
        let offset = u64_at(&corrupt, 32) as usize;
        let occupied = (0..2).find(|slot| u32_at(&corrupt, offset + slot * 16 + 8) != 0).unwrap();
        corrupt[offset + occupied * 16] ^= 1;
        fs::write(fixture.0.join("ampr_emu.index"), &corrupt).unwrap();
        assert!(prepare_staged(&fixture.0, &|| Ok(())).unwrap_err().contains("hash-slot"));
        assert_eq!(fs::read(fixture.0.join("ampr_emu.index")).unwrap(), corrupt);
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
    }
}
