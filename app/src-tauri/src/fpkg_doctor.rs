//! Bounded structural inspection and a staging-only repair plan. Source files are never changed.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const SELF_MAGIC: [[u8; 4]; 2] = [[0x4f, 0x15, 0x3d, 0x1d], [0x54, 0x14, 0xf5, 0xee]];
const MAX_FILES: usize = 200_000;
const MAX_DEPTH: usize = 32;
const MAX_ISSUES: usize = 512;
const MAX_HEADER_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub issues: Vec<DoctorIssue>,
    pub repairs: Vec<DoctorRepair>,
    pub scanned_modules: usize,
    #[serde(skip)]
    pub source_bytes: u64,
    #[serde(skip)]
    pub private_bytes: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorIssue {
    pub path: PathBuf,
    pub severity: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorRepair {
    pub relative: PathBuf,
    pub backup: PathBuf,
}

impl DoctorReport {
    pub fn blockers(&self) -> Vec<String> {
        self.issues
            .iter()
            .filter(|issue| issue.severity == "error")
            .map(|issue| format!("{}: {}", issue.path.display(), issue.message))
            .collect()
    }

    fn issue(
        &mut self,
        path: &Path,
        severity: &str,
        message: impl Into<String>,
    ) -> Result<(), String> {
        if self.issues.len() >= MAX_ISSUES {
            return Err(
                "Doctor issue limit reached; inspection stopped without changing any files".into(),
            );
        }
        self.issues.push(DoctorIssue {
            path: path.to_path_buf(),
            severity: severity.into(),
            message: message.into(),
        });
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tree {
    Game,
    Backups,
    Runtime,
    Excluded,
}

pub fn inspect(
    source: &Path,
    checkpoint: &dyn Fn() -> Result<(), String>,
) -> Result<DoctorReport, String> {
    checkpoint()?;
    if !safe_metadata(source)?.is_some_and(|metadata| metadata.is_dir()) {
        return Err("Doctor source is not a directory".into());
    }
    let mut report = DoctorReport::default();
    report.issue(Path::new(""), "info", "Structural checks cannot reconstruct arbitrary missing or modified bytes without a known baseline. Valid modified executables are preserved; passing these checks does not prove console launch compatibility.")?;
    inspect_metadata(source, &mut report)?;
    let mut checked = HashSet::new();
    inspect_module(source, Path::new("eboot.bin"), &mut report, checkpoint)?;
    checked.insert(PathBuf::from("eboot.bin"));

    let mut stack = vec![(PathBuf::new(), Tree::Game, 0usize)];
    let mut backup_targets = HashSet::new();
    let mut entries_seen = 0usize;
    while let Some((relative_dir, tree, depth)) = stack.pop() {
        checkpoint()?;
        for entry in std::fs::read_dir(source.join(&relative_dir)).map_err(crate::redact)? {
            checkpoint()?;
            entries_seen += 1;
            if entries_seen > MAX_FILES {
                return Err(
                    "Doctor file limit reached; inspection stopped without changing any files"
                        .into(),
                );
            }
            let entry = entry.map_err(crate::redact)?;
            let relative = relative_dir.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(crate::redact)?;
            reject_link(&metadata, &relative)?;
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if metadata.is_dir() {
                if depth >= MAX_DEPTH {
                    return Err(format!(
                        "Doctor directory depth limit reached at {}",
                        relative.display()
                    ));
                }
                let next_tree = if tree != Tree::Game {
                    tree
                } else if name == "decrypted" {
                    report.issue(&relative, "info", "Backup directory is excluded from the package. Only validated backups needed to repair an invalid or missing executable may be copied into staging.")?;
                    if relative_dir.as_os_str().is_empty() {
                        Tree::Backups
                    } else {
                        Tree::Excluded
                    }
                } else if name == "fakelib" || name == "fakelib2" {
                    report.issue(&relative, "warning", "Preserved backport runtime dependency. Installed packages require the compatible ShadowMountPlus 1.7 pre-spawn hook; doctor does not merge, rename or unpatch these libraries.")?;
                    Tree::Runtime
                } else {
                    Tree::Game
                };
                stack.push((relative, next_tree, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                return Err(format!(
                    "Doctor refuses a non-regular source entry: {}",
                    relative.display()
                ));
            }
            report.source_bytes = report.source_bytes.saturating_add(metadata.len());
            if crate::fpkg::private_staging_file(&relative) { report.private_bytes = report.private_bytes.saturating_add(metadata.len()); }
            if tree == Tree::Runtime || tree == Tree::Excluded {
                continue;
            }
            if crate::fpkg::BACKUP_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
                if tree == Tree::Game {
                    report.issue(
                        &relative,
                        "info",
                        "Backup file is excluded from the package; valid originals take priority.",
                    )?;
                }
                let target = if tree == Tree::Backups {
                    relative
                        .strip_prefix("decrypted")
                        .ok()
                        .map(Path::to_path_buf)
                } else {
                    Some(relative.clone())
                };
                if let Some(mut target) = target {
                    if let Some(stem) = target.file_stem().map(|name| name.to_os_string()) {
                        target.set_file_name(stem);
                        backup_targets.insert(target);
                    }
                }
                continue;
            }
            if tree == Tree::Backups {
                continue;
            }
            if name.starts_with("playgo") && name.ends_with(".dat") {
                report.issue(&relative, "info", "Stale PlayGo metadata is excluded; the builder generates a new package layout.")?;
                continue;
            }
            if name == "ampr_emu.index" {
                report.issue(&relative, "warning", "Preserved backport runtime index. Its matching emulation libraries and a compatible console pre-spawn hook are still required.")?;
                continue;
            }
            if !checked.contains(&relative) && is_module(&entry.path(), &relative)? {
                inspect_module(source, &relative, &mut report, checkpoint)?;
                checked.insert(relative);
            }
        }
    }
    let mut backup_targets = backup_targets.into_iter().collect::<Vec<_>>();
    backup_targets.sort();
    for relative in backup_targets {
        checkpoint()?;
        if excluded_repair_target(&relative)
            || checked.contains(&relative)
            || safe_metadata(&source.join(&relative))?.is_some()
        {
            continue;
        }
        let mut known_module = module_extension(&relative);
        if !known_module {
            for candidate in backup_paths(source, &relative) {
                if is_module(&candidate, &relative)? {
                    known_module = true;
                    break;
                }
            }
        }
        if known_module {
            inspect_module(source, &relative, &mut report, checkpoint)?;
            checked.insert(relative);
        }
    }
    Ok(report)
}

pub(crate) fn inspect_metadata(source: &Path, report: &mut DoctorReport) -> Result<(), String> {
    let relative = Path::new("sce_sys/param.json");
    let path = source.join(relative);
    let Some(metadata) = safe_metadata(&path)? else {
        return report.issue(
            relative,
            "error",
            "Missing metadata; doctor cannot invent title IDs, content IDs or a content version.",
        );
    };
    if !metadata.is_file() || metadata.len() > MAX_HEADER_BYTES {
        return report.issue(
            relative,
            "error",
            "Metadata must be a regular JSON file no larger than 1 MiB.",
        );
    }
    let mut data = Vec::new();
    File::open(&path)
        .map_err(crate::redact)?
        .take(MAX_HEADER_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(crate::redact)?;
    if data.len() as u64 > MAX_HEADER_BYTES {
        return report.issue(
            relative,
            "error",
            "Metadata exceeds the 1 MiB inspection limit.",
        );
    }
    let value: serde_json::Value = match serde_json::from_slice(&data) {
        Ok(value) => value,
        Err(error) => {
            return report.issue(
                relative,
                "error",
                format!("Malformed JSON metadata: {error}; an original metadata file is required."),
            )
        }
    };
    let title_id = value
        .get("titleId")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let valid_title = title_id.len() == 9
        && title_id.starts_with("PPSA")
        && title_id.as_bytes()[4..].iter().all(u8::is_ascii_digit);
    if !valid_title {
        report.issue(
            relative,
            "error",
            "titleId must be PPSA followed by five digits; doctor will not invent a replacement.",
        )?;
    }
    let content_id = value
        .get("contentId")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let bytes = content_id.as_bytes();
    let valid_content = bytes.len() == 36
        && bytes[0..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..6].iter().all(u8::is_ascii_digit)
        && bytes[6] == b'-'
        && bytes[16] == b'_'
        && bytes[17..19].iter().all(u8::is_ascii_digit)
        && bytes[19] == b'-'
        && bytes[20..]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_');
    if !valid_content {
        report.issue(
            relative,
            "error",
            "contentId is missing or malformed; an original content ID is required.",
        )?;
    } else if valid_title && &bytes[7..16] != title_id.as_bytes() {
        report.issue(relative, "error", "contentId identifies a different title than titleId; doctor will not guess which identity is correct.")?;
    }
    let version = value
        .get("contentVersion")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().zip([2, 3, 3]).any(|(part, count)| {
            part.len() != count || !part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        report.issue(relative, "error", "contentVersion must use the NN.NNN.NNN format; doctor cannot infer the original version.")?;
    }
    Ok(())
}

fn inspect_module(
    source: &Path,
    relative: &Path,
    report: &mut DoctorReport,
    checkpoint: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    checkpoint()?;
    report.scanned_modules += 1;
    let path = source.join(relative);
    let problem = match safe_metadata(&path)? {
        Some(metadata) if metadata.is_file() => match validate_executable(&path) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        },
        Some(_) => return report.issue(relative, "error", "Executable path is not a regular file; doctor will not replace a directory or special file."),
        None => "Executable is missing".into(),
    };
    for backup in backup_paths(source, relative) {
        checkpoint()?;
        let Some(metadata) = safe_metadata(&backup)? else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        match validate_executable(&backup) {
            Ok(()) => {
                report.issue(relative, "warning", format!("{problem}. A structurally valid backup {} is available for a staging-only replacement; the original dump remains unchanged. Launch compatibility still requires console testing.", backup.strip_prefix(source).unwrap_or(&backup).display()))?;
                report.repairs.push(DoctorRepair {
                    relative: relative.to_path_buf(),
                    backup,
                });
                return Ok(());
            }
            Err(error) => report.issue(
                relative,
                "warning",
                format!(
                    "Backup {} cannot repair this executable: {error}",
                    backup.strip_prefix(source).unwrap_or(&backup).display()
                ),
            )?,
        }
    }
    report.issue(relative, "error", format!("{problem}. No structurally valid known backup is available; doctor cannot reconstruct the executable."))
}

/// Recheck a recorded plan immediately before copying into private staging.
pub fn validate_repair(source: &Path, repair: &DoctorRepair) -> Result<(), String> {
    if repair.relative.as_os_str().is_empty()
        || !repair
            .relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        || excluded_repair_target(&repair.relative)
    {
        return Err("Doctor repair target must be a relative game executable path".into());
    }
    if !safe_metadata(source)?.is_some_and(|metadata| metadata.is_dir()) {
        return Err("Doctor repair source is not a directory".into());
    }
    if !backup_paths(source, &repair.relative).contains(&repair.backup) {
        return Err("Doctor repair backup is outside its known backup locations".into());
    }
    if !safe_metadata(&repair.backup)?.is_some_and(|metadata| metadata.is_file()) {
        return Err("Doctor repair backup is no longer a regular file".into());
    }
    if repair.relative != Path::new("eboot.bin") && !is_module(&repair.backup, &repair.relative)? {
        return Err("Doctor repair target is not a recognized game executable".into());
    }
    if let Some(metadata) = safe_metadata(&source.join(&repair.relative))? {
        if !metadata.is_file() {
            return Err("Doctor repair target is no longer a regular file".into());
        }
        if validate_executable(&source.join(&repair.relative)).is_ok() {
            return Err(
                "Doctor repair target now has a valid executable; inspect again to preserve it"
                    .into(),
            );
        }
    }
    validate_executable(&repair.backup)
}

fn excluded_repair_target(relative: &Path) -> bool {
    relative.components().any(|component| {
        matches!(
            component
                .as_os_str()
                .to_string_lossy()
                .to_ascii_lowercase()
                .as_str(),
            "decrypted" | "fakelib" | "fakelib2"
        )
    })
}

fn backup_paths(source: &Path, relative: &Path) -> Vec<PathBuf> {
    crate::fpkg::BACKUP_SUFFIXES.iter().flat_map(|suffix| {
        let mut filename = relative.file_name().unwrap_or_default().to_os_string();
        filename.push(suffix);
        let backup_relative = relative.with_file_name(filename);
        [source.join("decrypted").join(&backup_relative), source.join(backup_relative)]
    }).collect()
}

fn module_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("elf" | "prx" | "sprx" | "self")
    )
}

pub(crate) fn is_module(path: &Path, relative: &Path) -> Result<bool, String> {
    if module_extension(relative) {
        return Ok(true);
    }
    if !relative
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("bin"))
    {
        return Ok(false);
    }
    let Some(metadata) = safe_metadata(path)? else {
        return Ok(false);
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    let mut magic = [0u8; 4];
    let mut file = File::open(path).map_err(crate::redact)?;
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(error) => return Err(crate::redact(error)),
    }
    Ok(magic == ELF_MAGIC || SELF_MAGIC.contains(&magic))
}

pub(crate) fn validate_executable(path: &Path) -> Result<(), String> {
    let mut file = File::open(path).map_err(crate::redact)?;
    let size = file.metadata().map_err(crate::redact)?.len();
    let mut header = [0u8; 64];
    file.read_exact(&mut header)
        .map_err(|_| "Truncated executable header".to_string())?;
    if SELF_MAGIC.iter().any(|magic| header.starts_with(magic)) {
        return crate::fpkg::validate_plaintext_self(path).map(|_| ());
    }
    if !header.starts_with(&ELF_MAGIC) {
        return Err("Unrecognized executable format".into());
    }
    let u16_at = |offset| u16::from_le_bytes(header[offset..offset + 2].try_into().unwrap());
    let u32_at = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
    let u64_at = |offset| u64::from_le_bytes(header[offset..offset + 8].try_into().unwrap());
    if header[4..7] != [2, 1, 1] || u16_at(18) != 62 || u32_at(20) != 1 {
        return Err("Executable must be ELF64 little endian x86-64".into());
    }
    if !matches!(
        u16_at(16),
        2 | 3 | 0xfe00 | 0xfe01 | 0xfe04 | 0xfe0c | 0xfe10 | 0xfe18
    ) {
        return Err("Unsupported ELF executable type".into());
    }
    let phoff = u64_at(32);
    let phnum = u16_at(56) as u64;
    let table_size = phnum * 56;
    if u16_at(52) != 64
        || u16_at(54) != 56
        || phnum == 0
        || phnum > 4096
        || phoff < 64
        || phoff
            .checked_add(table_size)
            .is_none_or(|end| end > size || end > MAX_HEADER_BYTES)
    {
        return Err("Invalid or truncated ELF program-header table".into());
    }
    file.seek(SeekFrom::Start(phoff)).map_err(crate::redact)?;
    let mut table = vec![0u8; table_size as usize];
    file.read_exact(&mut table).map_err(crate::redact)?;
    let mut loadable = false;
    for (index, ph) in table.chunks_exact(56).enumerate() {
        let kind = u32::from_le_bytes(ph[0..4].try_into().unwrap());
        let offset = u64::from_le_bytes(ph[8..16].try_into().unwrap());
        let filesz = u64::from_le_bytes(ph[32..40].try_into().unwrap());
        let memsz = u64::from_le_bytes(ph[40..48].try_into().unwrap());
        if kind != 0 && filesz > 0 && offset.checked_add(filesz).is_none_or(|end| end > size) {
            return Err(format!("ELF segment {index} extends beyond the file"));
        }
        if kind == 1 {
            if memsz < filesz {
                return Err(format!(
                    "ELF load segment {index} is larger than its memory mapping"
                ));
            }
            loadable |= filesz > 0;
        }
    }
    if !loadable {
        return Err("ELF has no file-backed loadable segment".into());
    }
    Ok(())
}

fn reject_link(metadata: &Metadata, path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if metadata.file_type().is_symlink() || reparse {
        return Err(format!(
            "Doctor refuses linked or reparse-point paths: {}",
            path.display()
        ));
    }
    Ok(())
}

// Check every existing ancestor before opening a candidate, including backup directories.
fn safe_metadata(path: &Path) -> Result<Option<Metadata>, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(crate::redact)?.join(path)
    };
    let mut prefix = PathBuf::new();
    let mut last = None;
    for component in absolute.components() {
        if matches!(component, Component::ParentDir) {
            // A relocatable source root may contain `..`. Its preceding path has
            // already been checked, so a linked ancestor cannot be hidden by it.
            if !prefix.pop() {
                return Err("Doctor source path traverses above its filesystem root".into());
            }
        } else {
            prefix.push(component.as_os_str());
        }
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&prefix) {
            Ok(metadata) => {
                reject_link(&metadata, &prefix)?;
                last = Some(metadata);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(crate::redact(error)),
        }
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = crate::test_output_root().join(format!("doctor-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("sce_sys")).unwrap();
            std::fs::write(root.join("sce_sys/param.json"), br#"{"titleId":"PPSA12345","contentId":"IV0000-PPSA12345_00-SSPIPACKTEST0000","contentVersion":"01.000.000"}"#).unwrap();
            std::fs::write(root.join("eboot.bin"), elf(0x11)).unwrap();
            Self(root)
        }
        fn backup(&self, bytes: &[u8]) {
            std::fs::create_dir_all(self.0.join("decrypted")).unwrap();
            std::fs::write(self.0.join("decrypted/eboot.bin.esbak"), bytes).unwrap();
        }
        fn report(&self) -> DoctorReport {
            inspect(&self.0, &|| Ok(())).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let allowed = crate::test_output_root().canonicalize().unwrap();
            if let Ok(actual) = self.0.canonicalize() {
                assert!(actual.starts_with(&allowed) && actual != allowed);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
    fn elf(byte: u8) -> Vec<u8> {
        let mut bytes = vec![0u8; 128];
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
        bytes[96..104].copy_from_slice(&8u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&8u64.to_le_bytes());
        bytes[120..].fill(byte);
        bytes
    }

    #[test]
    fn valid_modified_executable_takes_priority_over_backup() {
        let fixture = Fixture::new();
        fixture.backup(&elf(0x22));
        let report = fixture.report();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert!(report.repairs.is_empty());
        assert_eq!(
            std::fs::read(fixture.0.join("eboot.bin")).unwrap(),
            elf(0x11)
        );
        assert_eq!(report.scanned_modules, 1);
    }

    #[test]
    fn invalid_executable_uses_validated_backup_without_mutation() {
        let fixture = Fixture::new();
        fixture.backup(&elf(0x22));
        std::fs::write(fixture.0.join("eboot.bin"), b"invalid").unwrap();
        let report = fixture.report();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert_eq!(report.repairs.len(), 1);
        assert_eq!(report.repairs[0].relative, Path::new("eboot.bin"));
        assert_eq!(
            report.repairs[0].backup,
            fixture.0.join("decrypted/eboot.bin.esbak")
        );
        assert_eq!(
            std::fs::read(fixture.0.join("eboot.bin")).unwrap(),
            b"invalid"
        );
    }

    #[test]
    fn malformed_backup_cannot_repair_an_executable() {
        let fixture = Fixture::new();
        fixture.backup(&ELF_MAGIC);
        std::fs::write(fixture.0.join("eboot.bin"), b"invalid").unwrap();
        let report = fixture.report();
        assert!(!report.blockers().is_empty());
        assert!(report.repairs.is_empty());
    }

    #[test]
    fn repair_plan_is_revalidated_before_copy() {
        let fixture = Fixture::new();
        fixture.backup(&elf(0x22));
        std::fs::write(fixture.0.join("eboot.bin"), b"invalid").unwrap();
        let mut repair = fixture.report().repairs.remove(0);
        validate_repair(&fixture.0, &repair).unwrap();
        repair.relative = PathBuf::from("../eboot.bin");
        assert!(validate_repair(&fixture.0, &repair)
            .unwrap_err()
            .contains("relative"));
        repair.relative = PathBuf::from("eboot.bin");
        repair.backup = fixture.0.join("sce_sys/param.json");
        assert!(validate_repair(&fixture.0, &repair)
            .unwrap_err()
            .contains("known backup"));
        repair.backup = fixture.0.join("decrypted/eboot.bin.esbak");
        std::fs::write(&repair.backup, b"changed since inspection").unwrap();
        assert!(validate_repair(&fixture.0, &repair).is_err());
        fixture.backup(&elf(0x22));
        std::fs::write(fixture.0.join("eboot.bin"), elf(0x33)).unwrap();
        assert!(validate_repair(&fixture.0, &repair)
            .unwrap_err()
            .contains("preserve"));
    }

    #[test]
    fn missing_module_backups_do_not_restore_runtime_dependencies() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("decrypted/modules")).unwrap();
        std::fs::create_dir_all(fixture.0.join("decrypted/fakelib2")).unwrap();
        std::fs::write(
            fixture.0.join("decrypted/modules/game.prx.esbak"),
            elf(0x22),
        )
        .unwrap();
        std::fs::write(
            fixture.0.join("decrypted/fakelib2/emulator.prx.esbak"),
            elf(0x22),
        )
        .unwrap();
        let report = fixture.report();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert_eq!(report.repairs.len(), 1);
        assert_eq!(report.repairs[0].relative, Path::new("modules/game.prx"));
    }

    #[test]
    fn missing_eboot_can_be_repaired_only_from_a_known_backup() {
        let fixture = Fixture::new();
        std::fs::remove_file(fixture.0.join("eboot.bin")).unwrap();
        assert!(!fixture.report().blockers().is_empty());
        fixture.backup(&elf(0x22));
        let report = fixture.report();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert_eq!(report.repairs.len(), 1);
        assert!(!fixture.0.join("eboot.bin").exists());
    }

    #[test]
    fn malformed_metadata_and_mismatched_title_are_unresolved() {
        let fixture = Fixture::new();
        let path = fixture.0.join("sce_sys/param.json");
        std::fs::write(&path, b"{ invalid").unwrap();
        assert!(fixture
            .report()
            .blockers()
            .iter()
            .any(|issue| issue.contains("Malformed JSON")));
        std::fs::write(&path, br#"{"titleId":"PPSA54321","contentId":"IV0000-PPSA12345_00-SSPIPACKTEST0000","contentVersion":"01.000.000"}"#).unwrap();
        assert!(fixture
            .report()
            .blockers()
            .iter()
            .any(|issue| issue.contains("different title")));
    }

    #[test]
    fn valid_nonzero_content_id_segment_is_preserved() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("sce_sys/param.json"), br#"{"titleId":"PPSA12345","contentId":"IV0000-PPSA12345_99-SSPIPACKTEST0000","contentVersion":"01.000.000"}"#).unwrap();
        assert!(fixture.report().blockers().is_empty());
    }

    #[test]
    fn elf_segment_and_header_bounds_are_checked() {
        let fixture = Fixture::new();
        let path = fixture.0.join("module.prx");
        let mut bytes = elf(0);
        bytes[72..80].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(validate_executable(&path).unwrap_err().contains("beyond"));
        bytes = elf(0);
        bytes[32..40].copy_from_slice(&(MAX_HEADER_BYTES + 1).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(validate_executable(&path)
            .unwrap_err()
            .contains("program-header"));
    }

    #[test]
    fn runtime_dependencies_are_preserved_and_not_repaired() {
        let fixture = Fixture::new();
        std::fs::create_dir_all(fixture.0.join("fakelib2")).unwrap();
        std::fs::write(
            fixture.0.join("fakelib2/emulation.sprx"),
            b"runtime-specific",
        )
        .unwrap();
        std::fs::write(fixture.0.join("ampr_emu.index"), b"index").unwrap();
        let report = fixture.report();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert!(report.repairs.is_empty());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.message.contains("pre-spawn hook")));
        assert_eq!(report.scanned_modules, 1);
    }

    #[test]
    fn cancellation_stops_inspection() {
        let fixture = Fixture::new();
        let count = std::cell::Cell::new(0);
        let result = inspect(&fixture.0, &|| {
            count.set(count.get() + 1);
            if count.get() > 2 {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "cancelled");
    }

    #[test]
    fn backup_ancestor_links_are_refused() {
        let fixture = Fixture::new();
        let other = Fixture::new();
        std::fs::write(other.0.join("eboot.bin.esbak"), elf(0x22)).unwrap();
        std::fs::write(fixture.0.join("eboot.bin"), b"invalid").unwrap();
        let link = fixture.0.join("decrypted");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&other.0, &link);
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&other.0, &link);
        if let Err(error) = linked {
            #[cfg(windows)]
            if error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("could not create test link: {error}");
        }
        let result = inspect(&fixture.0, &|| Ok(()));
        let parent_traversal = inspect(&fixture.0.join("decrypted/.."), &|| Ok(()));
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
        assert!(result.unwrap_err().contains("linked or reparse"));
        assert!(parent_traversal.unwrap_err().contains("linked or reparse"));
    }
}
