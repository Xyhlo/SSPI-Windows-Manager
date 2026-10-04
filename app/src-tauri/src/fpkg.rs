//! Headless finalized PS5 package builds using the locally supplied LibProsperoPkg engine.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::redact;

/// Kind of package to produce. Kept deliberately small; `Dlc` and `Backport`
/// are accepted by the planner but routed through the same engine call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    Base,
    Update,
    Dlc,
    Backport,
}

impl PackageKind {
    pub fn from_label(label: &str) -> Self {
        match label.trim().to_ascii_lowercase().as_str() {
            "update" | "patch" => Self::Update,
            "dlc" | "addcont" | "add-cont" => Self::Dlc,
            "backport" | "bp" | "back-port" => Self::Backport,
            _ => Self::Base,
        }
    }

    pub fn engine_flag(self) -> &'static str {
        match self {
            Self::Base => "app",
            Self::Update => "patch",
            Self::Dlc => "ac",
            Self::Backport => "app",
        }
    }
}

/// Speed/weight presets, mapped to engine parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackagePreset {
    Fastest,
    Balanced,
    Smallest,
}

// Full Gollum dump: level 3 is 24.56 GB, level 1 is 2.3% larger and HyperFast4 44% larger,
// so Balanced stays on level 3 and Fastest drops only to SuperFast. On a 3 GiB slice,
// level 7 compressed 10x slower than level 3 for 3.9% less; level 5 took 7x for 2.1%.
const FASTEST_COMPRESSION_LEVEL: i8 = 1; // SuperFast
const BALANCED_COMPRESSION_LEVEL: i8 = 3; // Fast
const SMALLEST_COMPRESSION_LEVEL: i8 = 7; // Optimal3

impl PackagePreset {
    pub fn from_label(label: &str) -> Self {
        match label.trim().to_ascii_lowercase().as_str() {
            "fastest" | "fast" => Self::Fastest,
            "smallest" | "small" => Self::Smallest,
            _ => Self::Balanced,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fastest => "fastest",
            Self::Balanced => "balanced",
            Self::Smallest => "smallest",
        }
    }

    pub fn level(self) -> i8 {
        match self {
            Self::Fastest => FASTEST_COMPRESSION_LEVEL,
            Self::Balanced => BALANCED_COMPRESSION_LEVEL,
            Self::Smallest => SMALLEST_COMPRESSION_LEVEL,
        }
    }
}

pub fn deserialize_preset<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    Ok(PackagePreset::from_label(&value).label().into())
}

pub fn validate_compression_level(level: Option<i8>) -> Result<(), String> {
    match level {
        None | Some(-4..=-1 | 1..=9) => Ok(()),
        Some(_) => Err("Kraken compression level must be -4 through -1 or 1 through 9".into()),
    }
}

#[derive(Clone, Debug)]
pub struct PackageOptions {
    pub source: PathBuf,
    pub output_dir: PathBuf,
    /// This build's unique sspi-fpkg-<uuid> directory, removed after the engine exits.
    pub temp_dir: Option<PathBuf>,
    pub title_id: Option<String>,
    pub kind: PackageKind,
    pub preset: PackagePreset,
    /// Explicit Kraken level -4..-1 or 1..9; None keeps the saved speed preset.
    pub compression_level: Option<i8>,
    /// PFS filesystem version: 2 (all firmware) or 3 (FW >= 7.00).
    pub pfs_version: u8,
    /// Console firmware the package is meant for; gates PFS v3 and is written
    /// into the package metadata.
    pub target_fw: Option<String>,
    /// Per-block Kraken size in KiB (128 or 256).
    pub block_size_kib: u16,
    pub threads: Option<u16>,
}

impl PackageOptions {
    pub fn new(source: impl Into<PathBuf>, output_dir: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            output_dir: output_dir.into(),
            temp_dir: None,
            title_id: None,
            kind: PackageKind::Base,
            preset: PackagePreset::Balanced,
            compression_level: None,
            pfs_version: 2,
            target_fw: None,
            block_size_kib: 256,
            threads: None,
        }
    }

    pub fn effective_compression_level(&self) -> Result<i8, String> {
        validate_compression_level(self.compression_level)?;
        Ok(self.compression_level.unwrap_or_else(|| self.preset.level()))
    }

    /// PFS v3 only reads on FW >= 7.00; downgrade silently rather than ship a
    /// package the target console cannot mount.
    pub fn effective_pfs_version(&self) -> u8 {
        if self.pfs_version != 3 {
            return 2;
        }
        match self.target_fw.as_deref().and_then(parse_fw) {
            Some(fw) if fw >= (7, 0) => 3,
            _ => 2,
        }
    }
}

// Keep this reserve while building too; the planner may tune it after the benchmark.
pub const TEMP_RESERVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TempVolume {
    pub letter: char,
    pub fixed: bool,
    pub free_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct TempEnvironment {
    pub volumes: Vec<TempVolume>,
    pub system_temp: PathBuf,
}

impl TempEnvironment {
    pub fn system() -> Self {
        let mut volumes = Vec::new();
        #[cfg(windows)] {
            #[link(name = "kernel32")]
            extern "system" { fn GetDriveTypeW(root: *const u16) -> u32; }
            for letter in b'A'..=b'Z' {
                let root = [letter as u16, b':' as u16, b'\\' as u16, 0];
                if unsafe { GetDriveTypeW(root.as_ptr()) } != 3 { continue; } // DRIVE_FIXED
                if let Some(free_bytes) = super::free_space(Path::new(&format!("{}:\\", letter as char))) {
                    volumes.push(TempVolume { letter: letter as char, fixed: true, free_bytes });
                }
            }
        }
        Self { volumes, system_temp: std::env::temp_dir() }
    }
}

#[derive(Debug)]
pub struct TempChoice {
    pub dir: PathBuf,
    pub reason: String,
}

/// Choose the parent directory; the caller adds its unique sspi-fpkg-<job uuid> child.
pub fn choose_temp_dir(output_dir: &Path, input_bytes: u64, env: &TempEnvironment) -> TempChoice {
    let output_volume = super::archives::volume_of(&super::plain_path(output_dir));
    let temp_volume = super::archives::volume_of(&super::plain_path(&env.system_temp));
    let reason = match (output_volume, temp_volume) {
        (Some(output), Some(temp)) if output.eq_ignore_ascii_case(&temp) => "System temp shares the output volume",
        (Some(_), Some(temp)) => match env.volumes.iter().find(|v| temp.eq_ignore_ascii_case(&format!("{}:", v.letter))) {
            Some(volume) if volume.fixed => {
                if input_bytes.checked_add(TEMP_RESERVE_BYTES).is_some_and(|need| volume.free_bytes >= need) {
                    return TempChoice { dir: env.system_temp.clone(), reason: "System temp is on another fixed volume with room for the inner image and reserve".into() };
                }
                "System temp lacks room for the inner image and reserve"
            },
            _ => "System temp is not on a known fixed volume",
        },
        _ => "Cannot determine separate temp and output volumes",
    };
    TempChoice { dir: output_dir.join("work"), reason: format!("{reason}; using output work directory") }
}

struct TempWorkspace { dir: PathBuf, parent: PathBuf }

impl TempWorkspace {
    fn create(path: &Path) -> Result<Self, String> {
        let name = path.file_name().and_then(|name| name.to_str()).ok_or("Invalid packaging temp directory")?;
        if name.strip_prefix("sspi-fpkg-").and_then(|id| uuid::Uuid::parse_str(id).ok()).is_none() {
            return Err("Packaging temp directory must be named sspi-fpkg-<uuid>".into());
        }
        let parent = path.parent().ok_or("Packaging temp directory needs a parent")?;
        std::fs::create_dir_all(parent).map_err(redact)?;
        let parent = parent.canonicalize().map_err(redact)?;
        let dir = parent.join(name);
        std::fs::create_dir(&dir).map_err(redact)?; // Never take ownership of an existing directory.
        Ok(Self { dir, parent })
    }

    fn cleanup(&self) -> Result<(), String> {
        let metadata = match std::fs::symlink_metadata(&self.dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(redact(error)),
        };
        let resolved = self.dir.canonicalize().map_err(redact)?;
        if metadata.file_type().is_symlink() || is_reparse(&metadata) || resolved.parent() != Some(self.parent.as_path()) || resolved != self.dir {
            return Err("Packaging temp directory moved outside its workspace".into());
        }
        std::fs::remove_dir_all(&resolved).map_err(redact)
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) { let _ = self.cleanup(); }
}

pub fn parse_fw(value: &str) -> Option<(u32, u32)> {
    let cleaned = value.trim().trim_start_matches("FW").trim();
    let mut parts = cleaned.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// A 3 GiB slice favoured 16 workers, but the full 47 GiB Gollum dump at Kraken 3 compressed
/// in 95 s with 23 workers against 119 s with 16, so the cap only guards very wide CPUs.
const MAX_KRAKEN_WORKERS: usize = 32;

fn worker_budget(cores: usize, available: u64) -> u16 {
    // Leave one logical core and 2 GiB free; allow 512 MiB per Kraken worker.
    cores.saturating_sub(1).max(1).min(MAX_KRAKEN_WORKERS).min((available.saturating_sub(2 << 30) / (512 << 20)).max(1) as usize) as u16
}

pub fn kraken_workers() -> u16 {
    let mut available = 4u64 << 30;
    #[cfg(windows)] {
        #[repr(C)]
        struct MemoryStatus { length: u32, load: u32, physical: u64, available: u64, page: u64, available_page: u64, virtual_bytes: u64, available_virtual: u64, extended: u64 }
        #[link(name = "kernel32")]
        extern "system" { fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32; }
        let mut status: MemoryStatus = unsafe { std::mem::zeroed() };
        status.length = std::mem::size_of::<MemoryStatus>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 { available = status.available; }
    }
    worker_budget(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4), available)
}

/// Structural checks before building. Passing these is not a console launch test.
#[derive(Debug, Default)]
pub struct Preflight {
    pub file_count: u64,
    pub total_bytes: u64,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl Preflight {
    pub fn ok(&self) -> bool {
        self.blockers.is_empty()
    }
}

const F_SELF_MAGIC: [u8; 4] = [0x4F, 0x15, 0x3D, 0x1D];
const PROSPERO_SELF_MAGIC: [u8; 4] = [0x54, 0x14, 0xF5, 0xEE];
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
pub(crate) const BACKUP_SUFFIXES: [&str; 4] = [".esbak", ".bak", ".orig", ".origbak"];

pub(crate) fn private_staging_file(relative: &Path) -> bool {
    relative.components().any(|c| c.as_os_str().eq_ignore_ascii_case("sce_sys"))
        || relative.extension().and_then(|e| e.to_str()).is_some_and(|e| ["bin", "elf", "prx", "sprx", "self"].iter().any(|known| e.eq_ignore_ascii_case(known)))
}

fn excluded_staging_name(lower: &str) -> bool {
    lower == "decrypted" || BACKUP_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
        || (lower.starts_with("playgo") && lower.ends_with(".dat"))
}

#[derive(Default)]
pub struct StagedFiles {
    files: std::collections::HashMap<PathBuf, u64>,
    pub warnings: Vec<String>,
}

/// Inspect the staged executable. Doctor repairs are applied separately.
pub fn preflight(source: &Path, max_files: u64) -> Result<Preflight, String> {
    let mut report = preflight_metadata(source)?;
    if source.is_dir() { walk_counts(source, &mut report, max_files, 0)?; }
    Ok(report)
}

/// The staging walk already validated every entry; only index replacement sizes change.
pub fn preflight_staged(source: &Path, max_files: u64, staged: &StagedFiles) -> Result<Preflight, String> {
    let mut report = preflight_metadata(source)?;
    report.file_count = staged.files.len() as u64;
    report.total_bytes = staged.files.values().sum();
    for name in ["ampr_emu.index", "ampr_assets.index"] {
        if let Some(old) = staged.files.get(Path::new(name)) {
            report.total_bytes = report.total_bytes.saturating_sub(*old).saturating_add(std::fs::metadata(source.join(name)).map_err(redact)?.len());
        }
    }
    if report.file_count > max_files { report.blockers.push(format!("Package has {} files; limit is {max_files}", report.file_count)); }
    report.warnings.extend(staged.warnings.iter().cloned());
    Ok(report)
}

fn preflight_metadata(source: &Path) -> Result<Preflight, String> {
    let mut report = Preflight::default();
    if !source.is_dir() {
        report.blockers.push("Package source is not a directory".into());
        return Ok(report);
    }
    if !source.join("sce_sys").join("param.json").is_file() {
        report
            .blockers
            .push("Missing sce_sys/param.json at the dump root".into());
    } else {
        let mut metadata = crate::fpkg_doctor::DoctorReport::default();
        crate::fpkg_doctor::inspect_metadata(source, &mut metadata)?;
        report.blockers.extend(metadata.blockers());
    }
    let eboot = source.join("eboot.bin");
    if !eboot.is_file() {
        report.blockers.push("Missing eboot.bin at the dump root".into());
    } else {
        let executable = &eboot;
        let head = read_head(executable, 8)?;
        if head.starts_with(&F_SELF_MAGIC) || head.starts_with(&PROSPERO_SELF_MAGIC) {
            match validate_plaintext_self(executable) {
                Ok(segments) => report.warnings.push(format!(
                    "eboot.bin: validated plaintext fSELF ({segments} data segments); preserving the executable. \
                     No decrypted backup is required. Console launch compatibility still needs testing."
                )),
                Err(error) => report.blockers.push(format!("eboot.bin: {error}")),
            }
        } else if !head.starts_with(&ELF_MAGIC) {
            report.blockers.push(
                "eboot.bin is neither a plain ELF nor a known fSELF container".into(),
            );
        } else if let Err(error) = crate::fpkg_doctor::validate_executable(executable) {
            report.blockers.push(format!("eboot.bin: {error}; restore a validated executable backup"));
        }
    }
    if source.join("fakelib").is_dir() || source.join("fakelib2").is_dir() {
        report.warnings.push(
            "Backport runtime files are embedded in the package. Installed titles require \
             a compatible pre-spawn loader such as ShadowMount Plus 1.7; kstuff package support alone does not mount these libraries. \
             fakelib and fakelib2 are preserved separately."
                .into(),
        );
    }
    if source.join("ampr_emu.index").is_file() {
        report.warnings.push(
            "ampr_emu.index is preserved with its backport runtime files; the console loader must support this index format"
                .into(),
        );
    }
    if source.join("decrypted").is_dir() {
        report
            .warnings
            .push("decrypted/ backup tree present; do not include it in the package".into());
    }
    Ok(report)
}

fn walk_counts(
    dir: &Path,
    report: &mut Preflight,
    max_files: u64,
    depth: u32,
) -> Result<(), String> {
    if depth > 32 { return Err("Package directory depth exceeds 32".into()); }
    for entry in std::fs::read_dir(dir).map_err(redact)? {
        let entry = entry.map_err(redact)?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(redact)?;
        if metadata.file_type().is_symlink() || is_reparse(&metadata) { return Err("Packaging refuses linked source paths".into()); }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if metadata.is_dir() {
            if name == "decrypted" {
                continue;
            }
            walk_counts(&path, report, max_files, depth + 1)?;
            continue;
        }
        if BACKUP_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            report.warnings.push(format!(
                "backup file in package tree: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            continue;
        }
        if name.starts_with("playgo") && name.ends_with(".dat") {
            report.warnings.push(format!(
                "stale playgo chunk map will be excluded during staging: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            continue;
        }
        report.file_count += 1;
        if report.file_count > max_files { return Err(format!("Package file count exceeds limit {max_files}")); }
        report.total_bytes = report
            .total_bytes
            .saturating_add(metadata.len());
    }
    Ok(())
}

fn read_head(path: &Path, n: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(redact)?;
    let mut buf = vec![0u8; n];
    let mut read = 0;
    while read < n {
        let count = file.read(&mut buf[read..]).map_err(redact)?;
        if count == 0 { break; }
        read += count;
    }
    buf.truncate(read);
    Ok(buf)
}

/// SELF magic identifies a container, not folder-only patches or encrypted data.
/// Read only its bounded header; never rewrite code or reconstruct missing bytes.
pub(crate) fn validate_plaintext_self(path: &Path) -> Result<usize, String> {
    let size = std::fs::metadata(path).map_err(redact)?.len();
    let base = read_head(path, 32)?;
    if base.len() != 32 { return Err("Truncated SELF header".into()); }
    if !base.starts_with(&F_SELF_MAGIC) && !base.starts_with(&PROSPERO_SELF_MAGIC) {
        return Err("Unrecognized SELF magic".into());
    }
    // Native Prospero SELF uses version 0x10; legacy fSELF wrappers use 0.
    // Both still require the same bounded plaintext ELF/segment validation below.
    if !matches!(base[4], 0 | 0x10) || base[5..8] != [1, 1, 0x12]
        || (base[4] == 0x10 && base[..4] != PROSPERO_SELF_MAGIC) {
        return Err("Unsupported SELF header format".into());
    }
    let u16_at = |b: &[u8], i| u16::from_le_bytes(b[i..i + 2].try_into().unwrap()) as usize;
    let u32_at = |b: &[u8], i| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |b: &[u8], i| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let header_size = u16_at(&base, 12);
    let meta_size = u16_at(&base, 14);
    let declared_size = u64_at(&base, 16);
    let count = u16_at(&base, 24);
    let elf_start = 32 + count * 32;
    let data_start = (header_size + meta_size) as u64;
    // Some producers append a version record after the declared SELF length.
    if count == 0 || elf_start + 64 > header_size || data_start > declared_size || declared_size > size {
        return Err("Invalid or truncated SELF header/metadata bounds".into());
    }
    let header = read_head(path, header_size)?;
    if header.len() != header_size { return Err("Truncated SELF header region".into()); }
    let elf = &header[elf_start..];
    if !elf.starts_with(&ELF_MAGIC) || elf[4..7] != [2, 1, 1] || u16_at(elf, 18) != 62 || u32_at(elf, 20) != 1 {
        return Err("SELF does not contain a supported x86-64 ELF header".into());
    }
    let phoff = u64_at(elf, 32);
    let phnum = u16_at(elf, 56);
    if phoff < 64 || phoff > elf.len() as u64 || phnum == 0 || u16_at(elf, 52) != 64 || u16_at(elf, 54) != 56 {
        return Err("Invalid ELF program-header table".into());
    }
    let phoff = phoff as usize;
    let phend = phoff + phnum * 56;
    let ext = (elf_start + phend + 15) & !15;
    if phend > elf.len() || ext + 64 > header_size { return Err("Truncated ELF program headers or SELF authority info".into()); }
    let authority = u64_at(&header, ext);
    let program_type = u64_at(&header, ext + 8);
    let mut mapped = vec![false; phnum];
    let mut ranges = Vec::new();
    let mut data_segments = 0;
    for i in 0..count {
        let entry = &header[32 + i * 32..32 + (i + 1) * 32];
        let flags = u64_at(entry, 0);
        let offset = u64_at(entry, 8);
        let stored = u64_at(entry, 16);
        let expanded = u64_at(entry, 24);
        if flags & 2 != 0 {
            return Err(format!("SELF segment {i} is encrypted; this file needs a decrypted executable or a compatible folder-install source"));
        }
        if offset < data_start || offset.checked_add(stored).is_none_or(|end| end > declared_size) {
            return Err(format!("SELF segment {i} lies outside the file data"));
        }
        if stored > 0 { ranges.push((offset, offset + stored)); }
        if flags & 0xF0000 != 0 { continue; } // Digest/extent entries do not index ELF program headers.
        if flags & 8 != 0 || stored != expanded {
            return Err(format!("SELF segment {i} uses a compressed layout that this preflight cannot validate"));
        }
        let index = ((flags >> 20) & 0xFFFF) as usize;
        if index >= phnum || mapped[index] { return Err(format!("Invalid or duplicate SELF segment mapping at entry {i}")); }
        let ph = &elf[phoff + index * 56..phoff + (index + 1) * 56];
        if stored != u64_at(ph, 32) { return Err(format!("SELF segment {i} does not match its ELF file size")); }
        mapped[index] = true;
        data_segments += 1;
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) { return Err("SELF data segments overlap".into()); }
    if authority >> 56 != 0x31 || program_type != 1 {
        return Err(format!("SELF is not a supported fake-authority executable (authority {authority:#018x}, type {program_type}); a compatible executable is required"));
    }
    for (i, present) in mapped.iter().enumerate() {
        let ph = &elf[phoff + i * 56..phoff + (i + 1) * 56];
        let kind = u32_at(ph, 0);
        if matches!(kind, 1 | 0x61000000 | 0x61000010 | 0x6FFFFF00) && u64_at(ph, 32) > 0 && !present {
            return Err(format!("SELF is missing data for ELF segment {i}"));
        }
    }
    if data_segments == 0 { return Err("SELF contains no executable data segments".into()); }
    Ok(data_segments)
}

/// Use hard links for bulk data and private copies for metadata/executables.
/// The extracted source remains intact until a verified package is journaled.
pub fn stage_source(source: &Path, staging: &Path) -> Result<(), String> {
    stage_source_controlled(source, staging, &|| Ok(()))
}

pub fn workspace_size(source: &Path) -> Result<(u64, u64), String> {
    let mut bytes = 0u64; let mut private_bytes = 0u64;
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).map_err(redact)? {
            let entry = entry.map_err(redact)?; let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(redact)?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) { return Err("Packaging refuses linked source paths".into()); }
            if metadata.is_dir() { stack.push(path); }
            else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                let relative = path.strip_prefix(source).map_err(redact)?;
                if private_staging_file(relative) {
                    private_bytes = private_bytes.saturating_add(metadata.len());
                }
            }
        }
    }
    Ok((bytes, private_bytes))
}

pub fn stage_source_controlled(source: &Path, staging: &Path, checkpoint: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    stage_source_with_repairs(source, staging, &[], checkpoint).map(|_| ())
}

pub fn stage_source_with_repairs(source: &Path, staging: &Path, repairs: &[crate::fpkg_doctor::DoctorRepair], checkpoint: &dyn Fn() -> Result<(), String>) -> Result<StagedFiles, String> {
    let mut staged = StagedFiles::default();
    let mut excluded = 0; let mut scene = 0;
    std::fs::create_dir_all(staging).map_err(redact)?;
    if std::fs::read_dir(staging).map_err(redact)?.next().is_some() { return Err("Packaging staging directory must be empty".into()); }
    let mut stack = vec![(source.to_path_buf(), staging.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        for entry in std::fs::read_dir(&from).map_err(redact)? {
            checkpoint()?;
            let entry = entry.map_err(redact)?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(redact)?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                return Err(format!("Packaging refuses a linked source path: {}", path.display()));
            }
            let name = entry.file_name();
            let lower = name.to_string_lossy().to_ascii_lowercase();
            if excluded_staging_name(&lower) {
                excluded += 1;
                if excluded <= 8 { staged.warnings.push(format!("Staging excluded backup or regenerated metadata: {}", path.strip_prefix(source).map_err(redact)?.display())); }
                continue;
            }
            if lower == "_duplex_" || [".nfo", ".sfv", ".diz"].iter().any(|suffix| lower.ends_with(suffix)) { scene += 1; }
            let dest = to.join(&name);
            if metadata.is_dir() {
                std::fs::create_dir_all(&dest).map_err(redact)?;
                stack.push((path, dest));
            } else if metadata.is_file() {
                let relative = path.strip_prefix(source).map_err(redact)?;
                let original = path.clone();
                staged.files.insert(relative.to_path_buf(), metadata.len());
                let private = private_staging_file(relative);
                if private || std::fs::hard_link(&original, &dest).is_err() {
                    super::storage::guard_bytes(staging, metadata.len(), "packaging workspace")?;
                    use std::io::{Read, Write};
                    let mut input = std::fs::File::open(&original).map_err(redact)?;
                    let mut output = std::fs::File::create(&dest).map_err(redact)?;
                    let mut buffer = vec![0u8; 4 * 1024 * 1024];
                    loop { checkpoint()?; let n = input.read(&mut buffer).map_err(redact)?; if n == 0 { break; } output.write_all(&buffer[..n]).map_err(redact)?; }
                }
            }
        }
    }
    // Repair targets are private copies; never truncate a source hard link.
    for repair in repairs {
        checkpoint()?;
        crate::fpkg_doctor::validate_repair(source, repair)?;
        if repair.relative.is_absolute() || repair.relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return Err("Invalid doctor repair target".into());
        }
        let dest = staging.join(&repair.relative);
        if dest.exists() { std::fs::remove_file(&dest).map_err(redact)?; }
        std::fs::create_dir_all(dest.parent().ok_or("Invalid doctor target")?).map_err(redact)?;
        super::storage::guard_bytes(staging, std::fs::metadata(&repair.backup).map_err(redact)?.len(), "doctor repair")?;
        use std::io::{Read, Write};
        let mut input = std::fs::File::open(&repair.backup).map_err(redact)?;
        staged.files.insert(repair.relative.clone(), input.metadata().map_err(redact)?.len());
        let mut output = std::fs::File::create(&dest).map_err(redact)?;
        let mut buffer = vec![0u8; 4 * 1024 * 1024];
        loop { checkpoint()?; let n = input.read(&mut buffer).map_err(redact)?; if n == 0 { break; } output.write_all(&buffer[..n]).map_err(redact)?; }
    }
    if excluded > 0 { staged.warnings.push(format!("Staging excluded {excluded} backup/metadata entries in total; AMPR references keep their file IDs.")); }
    if scene > 0 { staged.warnings.push(format!("Staging kept {scene} scene/release entries (.nfo/.sfv/.diz/_DUPLEX_); their AMPR sizes are reconciled with the staged bytes.")); }
    Ok(staged)
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))] { let _ = metadata; false }
}

/// `<Title> [PPSA12345]`, safe as one Windows folder name.
pub fn package_folder_name(title: &str, title_id: Option<&str>) -> String {
    let mut name: String = title.chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c })
        .collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    name = name.chars().take(80).collect::<String>().trim_end_matches(['.', ' ']).to_string();
    if name.is_empty() { name = "Package".into(); }
    match title_id.filter(|id| !id.is_empty() && !name.contains(*id)) {
        Some(id) => format!("{name} [{id}]"),
        None => name,
    }
}

/// Moves a verified package out of its disposable workspace. The destination is a plain
/// (non-verbatim) path; an existing file with the same name gets a ` (2)`-style suffix.
pub fn relocate_package(built: &Path, folder: &Path) -> Result<PathBuf, String> {
    let file = built.file_name().ok_or("Package has no file name")?;
    std::fs::create_dir_all(folder).map_err(redact)?;
    let stem = Path::new(file).file_stem().unwrap_or(file).to_string_lossy().into_owned();
    let extension = Path::new(file).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut destination = folder.join(file);
    for n in 2.. {
        if !destination.exists() { break; }
        if n > 999 { return Err("Too many packages with the same name in the FPKG folder".into()); }
        destination = folder.join(format!("{stem} ({n}){extension}"));
    }
    if std::fs::rename(built, &destination).is_err() {
        // Different volume: copy, then remove the workspace copy only after the copy is complete.
        std::fs::copy(built, &destination).map_err(redact)?;
        std::fs::remove_file(built).map_err(redact)?;
    }
    Ok(destination)
}

pub fn cleanup_extracted(source: &Path, download_root: &Path) -> Result<(), String> {
    let source = source.canonicalize().map_err(redact)?;
    let download_root = download_root.canonicalize().map_err(redact)?;
    if source == download_root || !source.starts_with(&download_root) {
        return Err("Cleanup is limited to extracted dumps inside the configured download folder".into());
    }
    let mut scan = vec![source.clone()];
    while let Some(dir) = scan.pop() {
        for entry in std::fs::read_dir(dir).map_err(redact)? {
            let entry = entry.map_err(redact)?;
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(redact)?;
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                return Err("Cleanup refused a linked path; extracted dump retained".into());
            }
            if metadata.is_dir() { scan.push(entry.path()); }
        }
    }
    std::fs::remove_dir_all(&source).map_err(redact)?;
    // Only empty wrapper folders are pruned. A sibling component or another
    // job's files stop this walk without being touched.
    let staging = download_root.join("extracted");
    let mut parent = source.parent().map(Path::to_path_buf);
    while let Some(path) = parent.filter(|path| path.starts_with(&staging)) {
        if std::fs::remove_dir(&path).is_err() { break; }
        parent = path.parent().map(Path::to_path_buf);
    }
    Ok(())
}

/// Validate the finalized image bounds and read its embedded CNT identity.
pub fn package_identity(path: &Path) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(redact)?;
    let size = file.metadata().map_err(redact)?.len();
    let mut header = [0u8; 0x80];
    file.read_exact(&mut header).map_err(redact)?;
    if header[..4] == [0x7f, b'F', b'I', b'H'] {
        let image = u64::from_le_bytes(header[0x10..0x18].try_into().unwrap());
        let length = u64::from_le_bytes(header[0x18..0x20].try_into().unwrap());
        let cnt = u64::from_le_bytes(header[0x58..0x60].try_into().unwrap());
        if image < 0x10000 || length == 0 || image.checked_add(length).is_none_or(|end| end > cnt)
            || cnt.checked_add(0x80).is_none_or(|end| end > size) {
            return Err("Finalized FIH image has incomplete or invalid bounds".into());
        }
        file.seek(SeekFrom::Start(cnt)).map_err(redact)?;
        file.read_exact(&mut header).map_err(redact)?;
    }
    if header[..4] != [0x7f, b'C', b'N', b'T'] { return Err("Invalid package metadata header".into()); }
    for offset in [0x40, 0x30] {
        if let Ok(raw) = std::str::from_utf8(&header[offset..(offset+36).min(header.len())]) {
            let id = raw.trim_end_matches('\0');
            if id.len() == 36 && (id.contains("-PPSA") || id.contains("-CUSA")) { return Ok(id.to_string()); }
        }
    }
    Err("Package content ID is missing".into())
}

pub fn package_magic(header: &[u8]) -> bool {
    header.starts_with(&[0x7f,b'C',b'N',b'T']) || header.starts_with(&[0x7f,b'F',b'I',b'H'])
}

/// Locate the packaging engine. Order:
///   1. explicit path in Settings
///   2. SSPI_FPKG_ENGINE environment variable
///   3. `resources/fpkg/fpkg-cli.exe` next to the application
///   4. PATH lookup of `fpkg-cli`
pub fn locate_engine(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(value) = explicit.filter(|v| !v.trim().is_empty()) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(value) = std::env::var("SSPI_FPKG_ENGINE") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("resources").join("fpkg").join("fpkg-cli.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    which("fpkg-cli")
}

/// The .NET runtime shipped beside the application in `resources/dotnet`, when it is complete.
fn bundled_dotnet_root(app_dir: &Path) -> Option<PathBuf> {
    let root = app_dir.join("resources").join("dotnet");
    (root.join("host").join("fxr").is_dir() && root.join("shared").join("Microsoft.NETCore.App").is_dir()).then_some(root)
}

/// The packaging engines are published framework-dependent. A full SSPI build ships the .NET
/// runtime they need, so point them at it and no separate .NET install is required.
pub(crate) fn use_bundled_dotnet(command: &mut Command) {
    let exe = std::env::current_exe().ok();
    if let Some(root) = exe.as_deref().and_then(Path::parent).and_then(bundled_dotnet_root) {
        command.env("DOTNET_ROOT", &root).env("DOTNET_ROOT_X64", &root);
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in ["exe", ""] {
            let candidate = if ext.is_empty() {
                dir.join(name)
            } else {
                dir.join(format!("{name}.{ext}"))
            };
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[derive(Debug)]
pub struct BuildOutcome {
    pub output: PathBuf,
    pub seconds: f64,
    /// Last engine lines, kept for diagnostics on failure paths.
    #[allow(dead_code)]
    pub log_tail: Vec<String>,
}

/// Recheck retained SSPI packages before upload: older builds could omit fakelib files.
pub async fn verify_runtime(
    engine: &Path, package: &Path,
    paused: impl Fn() -> Result<bool, String> + Send,
) -> Result<(), String> {
    let mut cmd = Command::new(engine);
    use_bundled_dotnet(&mut cmd);
    cmd.arg("verify").arg(package).kill_on_drop(true)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x08004000);
    let mut child = cmd.spawn().map_err(|error| format!("Could not start package verification: {error}"))?;
    let mut input = child.stdin.take().ok_or("Engine control input unavailable")?;
    let mut out = BufReader::new(child.stdout.take().ok_or("Engine stdout unavailable")?).lines();
    let mut err = BufReader::new(child.stderr.take().ok_or("Engine stderr unavailable")?).lines();
    let mut out_done = false;
    let mut err_done = false;
    let mut last_paused = false;
    let mut tail = Vec::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    while !out_done || !err_done {
        tokio::select! {
            _ = tick.tick() => {
                let pause = match paused() {
                    Ok(pause) => pause,
                    Err(error) => { let _ = child.kill().await; let _ = child.wait().await; return Err(error); }
                };
                if pause != last_paused {
                    use tokio::io::AsyncWriteExt;
                    let _ = input.write_all(if pause { b"pause\n" } else { b"resume\n" }).await;
                    last_paused = pause;
                }
            },
            line = out.next_line(), if !out_done => match line.map_err(redact)? {
                Some(line) => push_tail(&mut tail, line), None => out_done = true,
            },
            line = err.next_line(), if !err_done => match line.map_err(redact)? {
                Some(line) => push_tail(&mut tail, line), None => err_done = true,
            },
        }
    }
    let status = child.wait().await.map_err(redact)?;
    if status.success() { Ok(()) } else {
        Err(format!("Retained package failed runtime verification: {}",
            tail.iter().rev().take(3).cloned().collect::<Vec<_>>().join(" | ")))
    }
}

/// Run the engine and stream its progress lines to the caller.
///
/// `on_line` receives human-readable status; `on_progress` receives
/// (fraction, message) when the engine reports a percentage. Cancellation is
/// honoured by killing the child process.
pub async fn build(
    engine: &Path,
    options: &PackageOptions,
    on_line: impl Fn(String) + Send + 'static,
    on_progress: impl Fn(f64, String) + Send + 'static,
) -> Result<BuildOutcome, String> {
    build_controlled(engine, options, on_line, on_progress, || Ok(false), || false).await
}

pub async fn build_controlled(
    engine: &Path, options: &PackageOptions,
    on_line: impl Fn(String) + Send + 'static,
    on_progress: impl Fn(f64, String) + Send + 'static,
    paused: impl Fn() -> Result<bool, String> + Send + 'static,
    yielding: impl Fn() -> bool + Send + 'static,
) -> Result<BuildOutcome, String> {
    let source = options.source.clone();
    let pre = tokio::task::spawn_blocking(move || preflight(&source, 2_000_000)).await.map_err(redact)??;
    for warning in &pre.warnings {
        on_line(format!("warning: {warning}"));
    }
    for blocker in &pre.blockers {
        on_line(format!("blocked: {blocker}"));
    }
    if !pre.ok() {
        return Err(format!(
            "Dump is not package-ready: {}",
            pre.blockers.join("; ")
        ));
    }

    std::fs::create_dir_all(&options.output_dir).map_err(redact)?;
    let temp_dir = options.temp_dir.clone().unwrap_or_else(|| {
        choose_temp_dir(&options.output_dir, pre.total_bytes, &TempEnvironment::system()).dir
            .join(format!("sspi-fpkg-{}", uuid::Uuid::new_v4()))
    });
    let temp = TempWorkspace::create(&temp_dir)?;
    let outcome = build_in_temp(engine, options, &temp.dir, on_line, on_progress, paused, yielding).await;
    let cleanup = temp.cleanup().map_err(|error| format!("Cannot clean packaging temp {}: {error}", temp.dir.display()));
    match (outcome, cleanup) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
    }
}

fn guard_build_space(output: &Path, temp: &Path) -> Result<(), String> {
    super::storage::guard_bytes(output, 0, "packaging")?;
    if let Some(free) = super::free_space(temp) {
        if free < TEMP_RESERVE_BYTES {
            return Err(format!("Not enough space during packaging temporary files: {:.2} GiB reserve required, {:.2} GiB free at {}. Free space, then Retry; retained inputs are kept.",
                TEMP_RESERVE_BYTES as f64 / (1u64 << 30) as f64, free as f64 / (1u64 << 30) as f64, temp.display()));
        }
    }
    Ok(())
}

fn build_command(engine: &Path, options: &PackageOptions, temp: &Path) -> Result<Command, String> {
    let pfs = options.effective_pfs_version();

    let mut cmd = Command::new(engine);
    use_bundled_dotnet(&mut cmd);
    cmd.arg("build")
        .arg("--source")
        .arg(&options.source)
        .arg("--output")
        .arg(&options.output_dir)
        .arg("--temp")
        .arg(temp)
        .arg("--level")
        .arg(options.effective_compression_level()?.to_string())
        .arg("--pfs")
        .arg(format!("v{pfs}"))
        .arg("--block-size")
        .arg(options.block_size_kib.to_string())
        .arg("--kind")
        .arg(options.kind.engine_flag())
        // BelowNormal lost 20-30 s on the 47 GiB Kraken 3 dump to ordinary desktop load.
        .arg("--priority")
        .arg("normal")
        .arg("--json-progress");
    if let Some(title) = options.title_id.as_deref() {
        cmd.arg("--title-id").arg(title);
    }
    if let Some(threads) = options.threads {
        cmd.arg("--threads").arg(threads.to_string());
    }
    if let Some(fw) = options.target_fw.as_deref() {
        cmd.arg("--target-fw").arg(fw);
    }

    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW; the engine runs at Normal priority.
    cmd.kill_on_drop(true);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    Ok(cmd)
}

fn set_engine_priority(child: &tokio::process::Child, below_normal: bool) {
    #[cfg(windows)]
    if let Some(handle) = child.raw_handle() {
        #[link(name = "kernel32")]
        extern "system" { fn SetPriorityClass(process: *mut std::ffi::c_void, class: u32) -> i32; }
        const NORMAL_PRIORITY_CLASS: u32 = 0x0000_0020;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        unsafe { SetPriorityClass(handle as _, if below_normal { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS }); }
    }
    #[cfg(not(windows))]
    let _ = (child, below_normal);
}

async fn build_in_temp(
    engine: &Path, options: &PackageOptions, temp: &Path,
    on_line: impl Fn(String) + Send + 'static,
    on_progress: impl Fn(f64, String) + Send + 'static,
    paused: impl Fn() -> Result<bool, String> + Send + 'static,
    yielding: impl Fn() -> bool + Send + 'static,
) -> Result<BuildOutcome, String> {
    guard_build_space(&options.output_dir, temp)?;
    let mut cmd = build_command(engine, options, temp)?;
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|error| format!("Failed to start packaging engine: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Engine stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Engine stderr unavailable".to_string())?;

    let mut input = child.stdin.take().ok_or("Engine control input unavailable")?;
    let mut last_paused = false;
    let mut below_normal: Option<(bool, Instant)> = None;
    let mut control_tick = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut log_tail: Vec<String> = Vec::new();
    let mut output_path: Option<PathBuf> = None;

    let mut out_lines = BufReader::new(stdout).lines();
    let mut err_lines = BufReader::new(stderr).lines();

    let mut out_done = false;
    let mut err_done = false;
    while !out_done || !err_done {
        tokio::select! {
            _ = control_tick.tick() => {
                let pause = match paused().and_then(|pause| { guard_build_space(&options.output_dir, temp)?; Ok(pause) }) {
                    Ok(pause) => pause,
                    Err(error) => { let _ = child.kill().await; let _ = child.wait().await; return Err(error); }
                };
                if pause != last_paused {
                    use tokio::io::AsyncWriteExt;
                    let _ = input.write_all(if pause { b"pause\n" } else { b"resume\n" }).await;
                    last_paused = pause;
                }
                // While another game has priority the engine runs below normal. Reapplied every
                // few seconds, because the engine sets its own priority class as it starts.
                let low = yielding();
                if below_normal.is_none_or(|(applied, at)| applied != low || (low && at.elapsed() >= std::time::Duration::from_secs(3))) {
                    set_engine_priority(&child, low);
                    below_normal = Some((low, Instant::now()));
                }
            },
            line = out_lines.next_line(), if !out_done => {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => { let _ = child.kill().await; let _ = child.wait().await; return Err(redact(error)); }
                };
                match line {
                    Some(line) => {
                        if let Some(path) = parse_output_path(&line) {
                            output_path = Some(path);
                        }
                        if let Some((fraction, message)) = parse_progress(&line) {
                            on_progress(fraction, message.clone());
                        }
                        on_line(line.clone());
                        let heartbeat = serde_json::from_str::<serde_json::Value>(&line).ok()
                            .is_some_and(|value| value["heartbeat"].as_bool() == Some(true));
                        if !heartbeat { push_tail(&mut log_tail, line); }
                    }
                    None => out_done = true,
                }
            }
            line = err_lines.next_line(), if !err_done => {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => { let _ = child.kill().await; let _ = child.wait().await; return Err(redact(error)); }
                };
                match line {
                    Some(line) => { push_tail(&mut log_tail, line.clone()); on_line(line); }
                    None => err_done = true,
                }
            }
        }
    }

    let status = child.wait().await.map_err(redact)?;
    if !status.success() {
        // A failed engine run must not cost the user their only copy.
        return Err(format!(
            "Packaging failed ({}): {}",
            status,
            log_tail.iter().rev().take(3).cloned().collect::<Vec<_>>().join(" | ")
        ));
    }

    let output = output_path
        .ok_or_else(|| "Engine reported success but produced no package".to_string())?;

    let output = output.canonicalize().map_err(redact)?;
    let output_dir = options.output_dir.canonicalize().map_err(redact)?;
    if !output.starts_with(&output_dir) || !output.is_file() {
        return Err("Packaging output is outside this job's output directory".into());
    }
    if read_head(&output, 4)? != [0x7f, b'F', b'I', b'H'] {
        return Err("Engine did not produce a finalized FIH image".into());
    }
    let id = package_identity(&output)?;
    if options.title_id.as_ref().is_some_and(|title| !id.contains(title)) {
        return Err("Packaged content ID does not match the selected title".into());
    }

    Ok(BuildOutcome {
        output,
        seconds: started.elapsed().as_secs_f64(),
        log_tail,
    })
}

fn push_tail(tail: &mut Vec<String>, line: String) {
    tail.push(line);
    if tail.len() > 200 {
        tail.remove(0);
    }
}

/// Engine lines: `{"progress":0.42,"message":"..."}` or `42% message`
fn parse_progress(line: &str) -> Option<(f64, String)> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('{') {
        let value: serde_json::Value = serde_json::from_str(&format!("{{{rest}")).ok()?;
        let fraction = value.get("progress")?.as_f64()?;
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Packaging")
            .to_string();
        return Some((fraction.clamp(0., 1.), message));
    }
    if let Some(percent) = trimmed.split('%').next() {
        if let Ok(value) = percent.trim().parse::<f64>() {
            return Some((
                (value / 100.).clamp(0., 1.),
                trimmed.to_string(),
            ));
        }
    }
    None
}

/// Engine lines: `OK <path>` or `output: <path>` or `wrote <path>`.
fn parse_output_path(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(output) = value.get("output").and_then(|v| v.as_str()) {
            return Some(PathBuf::from(output));
        }
    }
    for prefix in ["OK ", "output:", "OUTPUT:", "wrote "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let candidate = PathBuf::from(rest.trim());
            if candidate.extension().is_some_and(|e| e.eq_ignore_ascii_case("pkg")) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_dotnet_runtime_is_used_only_when_complete() {
        let app = std::env::temp_dir().join(format!("sspi-dotnet-root-{}", std::process::id()));
        let root = app.join("resources").join("dotnet");
        std::fs::create_dir_all(root.join("host").join("fxr").join("9.0.20")).unwrap();
        assert_eq!(bundled_dotnet_root(&app), None, "a runtime without its shared framework is incomplete");
        std::fs::create_dir_all(root.join("shared").join("Microsoft.NETCore.App").join("9.0.20")).unwrap();
        assert_eq!(bundled_dotnet_root(&app), Some(root));
        let _ = std::fs::remove_dir_all(&app);
        assert_eq!(bundled_dotnet_root(&app), None);
    }

    #[test]
    fn package_folder_names_are_safe_and_carry_the_title_id() {
        assert_eq!(package_folder_name("The Lord of the Rings: Gollum™", Some("PPSA06367")), "The Lord of the Rings Gollum™ [PPSA06367]");
        assert_eq!(package_folder_name("  A/B\\\\C?..  ", Some("PPSA00001")), "A B C [PPSA00001]");
        assert_eq!(package_folder_name("PPSA00001", Some("PPSA00001")), "PPSA00001");
        assert_eq!(package_folder_name("\u{7}\u{7}", None), "Package");
    }

    #[test]
    fn relocation_moves_the_package_and_never_overwrites_one() {
        let root = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        let workspace = root.join("packaged").join("job").join("output");
        std::fs::create_dir_all(&workspace).unwrap();
        let folder = root.join("FPKG").join("Game [PPSA00001]");
        for (n, body) in [b"first".as_slice(), b"second".as_slice()].into_iter().enumerate() {
            let built = workspace.join("UP0000-PPSA00001_00-GAME000000000000-A0100-V0100.pkg");
            std::fs::write(&built, body).unwrap();
            let moved = relocate_package(&built, &folder).unwrap();
            assert!(!built.exists());
            assert_eq!(std::fs::read(&moved).unwrap(), body);
            let expected = if n == 0 { "UP0000-PPSA00001_00-GAME000000000000-A0100-V0100.pkg" } else { "UP0000-PPSA00001_00-GAME000000000000-A0100-V0100 (2).pkg" };
            assert_eq!(moved.file_name().unwrap(), expected);
        }
        assert_eq!(std::fs::read(folder.join("UP0000-PPSA00001_00-GAME000000000000-A0100-V0100.pkg")).unwrap(), b"first");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kraken_worker_budget_reserves_a_core_and_memory() {
        assert_eq!(worker_budget(24, 64 << 30), 23);
        assert_eq!(worker_budget(64, 64 << 30), 32);
        assert_eq!(worker_budget(12, 64 << 30), 11);
        assert_eq!(worker_budget(24, 4 << 30), 4);
        assert_eq!(worker_budget(24, 1 << 30), 1);
        assert_eq!(worker_budget(1, 64 << 30), 1);
    }

    #[test]
    fn staged_inventory_matches_preflight_after_index_repairs_and_excludes_backups() {
        let root = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        let source = root.join("source"); let staging = root.join("staging");
        std::fs::create_dir_all(source.join("sce_sys")).unwrap();
        std::fs::create_dir_all(source.join("_DUPLEX_")).unwrap();
        std::fs::write(source.join("sce_sys/param.json"), r#"{"titleId":"PPSA99999","contentId":"IV0000-PPSA99999_00-SSPIPACKTEST0000","contentVersion":"01.000.000"}"#).unwrap();
        std::fs::write(source.join("eboot.bin"), b"broken").unwrap();
        std::fs::write(source.join("eboot.bin.orig"), plaintext_self()).unwrap();
        std::fs::write(source.join("_DUPLEX_/duplex.nfo"), b"release text\r\n").unwrap();
        std::fs::write(source.join("ampr_emu.index"), b"old index").unwrap();
        std::fs::write(source.join("sce_sys/playgo-chunk.dat"), b"stale").unwrap();
        let doctor = crate::fpkg_doctor::inspect(&source, &|| Ok(())).unwrap();
        assert!(doctor.blockers().is_empty()); assert_eq!(doctor.repairs.len(), 1);
        assert_eq!(workspace_size(&source).unwrap(), (doctor.source_bytes, doctor.private_bytes));
        let inventory = stage_source_with_repairs(&source, &staging, &doctor.repairs, &|| Ok(())).unwrap();
        crate::ampr_index::prepare_staged(&staging, &|| Ok(())).unwrap();
        let fast = preflight_staged(&staging, 100, &inventory).unwrap(); let full = preflight(&staging, 100).unwrap();
        assert!(fast.ok() && full.ok()); assert_eq!((fast.file_count, fast.total_bytes), (full.file_count, full.total_bytes));
        assert!(fast.warnings.iter().any(|s| s.contains("scene/release")));
        assert!(!staging.join("eboot.bin.orig").exists()); assert!(!staging.join("sce_sys/playgo-chunk.dat").exists());
        assert_eq!(std::fs::read(source.join("eboot.bin")).unwrap(), b"broken");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staging_preserves_patched_files_and_backport_libraries() {
        let root = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        let source = root.join("download/extracted/game");
        let staging = root.join("download/packaged/source");
        std::fs::create_dir_all(source.join("sce_sys")).unwrap();
        std::fs::create_dir_all(source.join("decrypted")).unwrap();
        std::fs::create_dir_all(source.join("fakelib")).unwrap();
        std::fs::create_dir_all(source.join("fakelib2")).unwrap();
        std::fs::write(source.join("eboot.bin"), F_SELF_MAGIC).unwrap();
        std::fs::write(source.join("decrypted/eboot.bin.esbak"), ELF_MAGIC).unwrap();
        std::fs::write(source.join("sce_sys/param.json"), b"{}").unwrap();
        std::fs::write(source.join("sce_sys/playgo-chunk.dat"), b"stale").unwrap();
        std::fs::write(source.join("fakelib/runtime.sprx"), b"overlay").unwrap();
        std::fs::write(source.join("fakelib2/runtime.sprx"), b"different overlay").unwrap();
        std::fs::write(source.join("ampr_emu.index"), b"index").unwrap();
        std::fs::write(source.join("data.bin"), b"bulk data").unwrap();
        stage_source(&source, &staging).unwrap();
        assert_eq!(std::fs::read(staging.join("eboot.bin")).unwrap(), F_SELF_MAGIC);
        assert_eq!(std::fs::read(source.join("eboot.bin")).unwrap(), F_SELF_MAGIC);
        assert_eq!(std::fs::read(staging.join("fakelib/runtime.sprx")).unwrap(), b"overlay");
        assert_eq!(std::fs::read(staging.join("fakelib2/runtime.sprx")).unwrap(), b"different overlay");
        assert_eq!(std::fs::read(staging.join("ampr_emu.index")).unwrap(), b"index");
        assert!(!staging.join("decrypted").exists());
        assert!(!staging.join("sce_sys/playgo-chunk.dat").exists());
        std::fs::write(staging.join("sce_sys/param.json"), b"changed").unwrap();
        assert_eq!(std::fs::read(source.join("sce_sys/param.json")).unwrap(), b"{}");
        assert!(cleanup_extracted(&root, &root.join("download")).is_err());
        assert!(cleanup_extracted(&root.join("download"), &root.join("download")).is_err());
        cleanup_extracted(&staging, &root.join("download")).unwrap();
        assert!(source.join("fakelib/runtime.sprx").exists());
        cleanup_extracted(&source, &root.join("download")).unwrap();
        assert!(!source.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires the local packaging engine and generated test dump"]
    async fn local_engine_builds_a_finalized_image() {
        let fixture = PathBuf::from(std::env::var_os("SSPI_FPKG_FIXTURE").expect("fixture directory"));
        let engine = locate_engine(None).expect("SSPI_FPKG_ENGINE");
        let output = fixture.join(format!("rust-{}", uuid::Uuid::new_v4()));
        let staging = output.join("source");
        stage_source(&fixture.join("source"), &staging).unwrap();
        let mut options = PackageOptions::new(staging, output.join("packages"));
        options.title_id = Some("PPSA99999".into());
        options.preset = PackagePreset::Fastest;
        options.threads = Some(4);
        let result = build(&engine, &options, |_| {}, |_, _| {}).await.unwrap();
        assert!(result.output.is_file());
        assert_eq!(package_identity(&result.output).unwrap(), "IV0000-PPSA99999_00-SSPIPACKTEST0000");
        assert_eq!(read_head(&result.output, 4).unwrap(), [0x7f, b'F', b'I', b'H']);
        assert!(fixture.join("source/eboot.bin").is_file());
        println!("Actual headless engine: {} bytes, {:.2}s, finalized FIH identity verified", std::fs::metadata(result.output).unwrap().len(), result.seconds);
    }

    #[test]
    fn pfs3_requires_fw_7() {
        let mut options = PackageOptions::new("C:/dump", "C:/out");
        options.pfs_version = 3;
        options.target_fw = Some("4.03".into());
        assert_eq!(options.effective_pfs_version(), 2);
        options.target_fw = Some("11.20".into());
        assert_eq!(options.effective_pfs_version(), 3);
    }

    #[test]
    fn presets_map_to_levels() {
        assert_eq!(PackagePreset::from_label("fast").level(), FASTEST_COMPRESSION_LEVEL);
        assert_eq!(PackagePreset::from_label("fastest"), PackagePreset::Fastest);
        assert_eq!(PackagePreset::from_label("standard"), PackagePreset::Balanced);
        assert_eq!(PackagePreset::from_label("balanced"), PackagePreset::Balanced);
        assert_eq!(PackagePreset::from_label("").level(), BALANCED_COMPRESSION_LEVEL);
        assert_eq!(PackagePreset::from_label("smallest").level(), SMALLEST_COMPRESSION_LEVEL);
        let mut options = PackageOptions::new("source", "output");
        assert_eq!(options.effective_compression_level().unwrap(), BALANCED_COMPRESSION_LEVEL);
        assert_eq!(options.effective_pfs_version(), 2);
        options.preset = PackagePreset::Smallest;
        for level in (-4..=-1).chain(1..=9) { options.compression_level = Some(level); assert_eq!(options.effective_compression_level().unwrap(), level); }
        for level in [0, 10, -5, i8::MIN, i8::MAX] { options.compression_level = Some(level); assert!(options.effective_compression_level().is_err()); }
    }

    #[test]
    fn settings_migrate_old_presets_and_keep_explicit_levels() {
        for (saved, canonical) in [("fast", "fastest"), ("standard", "balanced"), ("smallest", "smallest"), ("fastest", "fastest"), ("balanced", "balanced")] {
            let mut value = serde_json::to_value(crate::Settings::default()).unwrap();
            value["fpkgPreset"] = saved.into();
            value["fpkgCompressionLevel"] = 3.into();
            let settings: crate::Settings = serde_json::from_value(value).unwrap();
            assert_eq!(settings.fpkg_preset, canonical);
            assert_eq!(settings.fpkg_compression_level, Some(3));
            assert_eq!(serde_json::to_value(settings).unwrap()["fpkgPreset"], canonical);
        }
        let mut value = serde_json::to_value(crate::Settings::default()).unwrap();
        for field in ["fpkgPreset", "fpkgCompressionLevel", "fpkgPfsVersion"] { value.as_object_mut().unwrap().remove(field); }
        let settings: crate::Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.fpkg_preset, "balanced");
        assert_eq!(settings.fpkg_compression_level, None);
        assert_eq!(settings.fpkg_pfs_version, 2);
    }

    #[test]
    fn settings_save_level_validation_accepts_hyperfast_and_rejects_gaps() {
        let mut value = serde_json::to_value(crate::Settings::default()).unwrap();
        for level in [-4, -1, 1, 3, 9, 0, 10, -5] {
            value["fpkgCompressionLevel"] = level.into();
            let input: crate::SaveSettings = serde_json::from_value(value.clone()).unwrap();
            let settings: crate::Settings = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(input.fpkg_compression_level, Some(level as i8));
            assert_eq!(settings.fpkg_compression_level, input.fpkg_compression_level);
            assert_eq!(validate_compression_level(input.fpkg_compression_level).is_ok(), matches!(level, -4..=-1 | 1..=9));
        }
        assert!(validate_compression_level(None).is_ok());
    }

    #[test]
    fn packaging_info_keeps_signed_levels_and_defaults_old_temp_path() {
        let info = crate::PackagingInfo { compression_level: -4, temp_path: "C:/Temp/sspi-fpkg-job".into(), ..Default::default() };
        let mut value = serde_json::to_value(info).unwrap();
        assert_eq!(value["compressionLevel"], -4);
        assert_eq!(value["tempPath"], "C:/Temp/sspi-fpkg-job");
        value.as_object_mut().unwrap().remove("tempPath");
        value["compressionLevel"] = 3.into();
        let restored: crate::PackagingInfo = serde_json::from_value(value).unwrap();
        assert!(restored.temp_path.is_empty());
        assert_eq!(restored.compression_level, 3);
    }

    fn temp_environment(fixed: bool, free_bytes: u64) -> TempEnvironment {
        TempEnvironment { system_temp: PathBuf::from("C:/Windows/Temp"), volumes: vec![TempVolume { letter: 'C', fixed, free_bytes }] }
    }

    #[test]
    fn temp_uses_another_fixed_volume_with_enough_space() {
        let input = 40 << 30;
        let env = temp_environment(true, input + TEMP_RESERVE_BYTES);
        let choice = choose_temp_dir(Path::new("D:/packages"), input, &env);
        assert_eq!(choice.dir, env.system_temp);
        assert!(choice.reason.contains("another fixed volume"));
    }

    #[test]
    fn temp_falls_back_when_space_is_short_or_size_overflows() {
        let output = Path::new("D:/packages");
        let input = 40 << 30;
        for (bytes, free) in [(input, input + TEMP_RESERVE_BYTES - 1), (u64::MAX, u64::MAX)] {
            let choice = choose_temp_dir(output, bytes, &temp_environment(true, free));
            assert_eq!(choice.dir, output.join("work"));
            assert!(choice.reason.contains("lacks room"));
        }
    }

    #[test]
    fn temp_falls_back_on_the_same_volume() {
        for output in [Path::new("c:/packages"), Path::new(r"\\?\C:\packages")] {
            let choice = choose_temp_dir(output, 1, &temp_environment(true, u64::MAX));
            assert_eq!(choice.dir, output.join("work"));
            assert!(choice.reason.contains("shares the output volume"));
        }
    }

    #[test]
    fn temp_skips_removable_network_and_unknown_volumes() {
        let output = Path::new("D:/packages");
        let mut env = temp_environment(false, u64::MAX);
        assert_eq!(choose_temp_dir(output, 1, &env).dir, output.join("work"));
        env.system_temp = PathBuf::from(r"\\server\share\Temp");
        assert_eq!(choose_temp_dir(output, 1, &env).dir, output.join("work"));
        env.system_temp = PathBuf::from("Z:/Temp");
        assert_eq!(choose_temp_dir(output, 1, &env).dir, output.join("work"));
        env.system_temp = PathBuf::from("Temp");
        assert_eq!(choose_temp_dir(output, 1, &env).dir, output.join("work"));
    }

    #[test]
    fn build_arguments_pass_temp_and_signed_level() {
        let mut options = PackageOptions::new("D:/dump", "D:/output");
        options.preset = PackagePreset::Smallest;
        options.compression_level = Some(-4);
        let temp = Path::new("C:/Temp with spaces/sspi-fpkg-job");
        let command = build_command(Path::new("fpkg-cli.exe"), &options, temp).unwrap();
        let args: Vec<_> = command.as_std().get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        for (flag, expected) in [("--temp", temp.to_str().unwrap()), ("--level", "-4"), ("--pfs", "v2"), ("--priority", "normal")] {
            let index = args.iter().position(|arg| arg == flag).unwrap();
            assert_eq!(args[index + 1], expected);
        }
    }

    #[test]
    fn temp_workspace_cleans_up_on_success_and_error_without_touching_siblings() {
        let root = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let sibling = root.join("keep");
        std::fs::write(&sibling, b"keep").unwrap();
        for fail in [false, true] {
            let dir = root.join(format!("sspi-fpkg-{}", uuid::Uuid::new_v4()));
            let result: Result<(), String> = (|| {
                let temp = TempWorkspace::create(&dir)?;
                std::fs::create_dir(temp.dir.join("nested")).unwrap();
                std::fs::write(temp.dir.join("nested/inner.img"), b"inner image").unwrap();
                if fail { return Err("engine failed".into()); }
                temp.cleanup()
            })();
            assert_eq!(result.is_err(), fail);
            assert!(!dir.exists());
            assert_eq!(std::fs::read(&sibling).unwrap(), b"keep");
        }
        assert!(TempWorkspace::create(&sibling).is_err());
        let existing = root.join(format!("sspi-fpkg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&existing).unwrap();
        assert!(TempWorkspace::create(&existing).is_err());
        assert!(existing.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires SSPI_BACKPORT_FIXTURE generated under Build-Output"]
    fn local_backport_fixture_passes_doctor_and_staging() {
        let source = PathBuf::from(std::env::var_os("SSPI_BACKPORT_FIXTURE").expect("host-only fixture"));
        let staging = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        let report = crate::fpkg_doctor::inspect(&source, &|| Ok(())).unwrap();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert!(report.repairs.is_empty());
        stage_source_with_repairs(&source, &staging, &report.repairs, &|| Ok(())).unwrap();
        assert!(crate::ampr_index::validate(&staging, &|| Ok(())).unwrap().is_empty());
        assert!(preflight(&staging, 100).unwrap().ok());
        for name in ["eboot.bin", "fakelib/fixture.prx", "fakelib2/fixture.prx", "ampr_emu.index"] {
            assert_eq!(std::fs::read(staging.join(name)).unwrap(), std::fs::read(source.join(name)).unwrap(), "{name}");
        }
        std::fs::remove_dir_all(staging).unwrap();
    }

    #[test]
    fn doctor_repairs_only_the_staged_executable() {
        let root = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        let source = root.join("source");
        let staging = root.join("staged");
        std::fs::create_dir_all(source.join("sce_sys")).unwrap();
        std::fs::create_dir_all(source.join("decrypted")).unwrap();
        std::fs::write(source.join("sce_sys/param.json"), br#"{"titleId":"PPSA99999","contentId":"IV0000-PPSA99999_00-SSPIPACKTEST0000","contentVersion":"01.000.000"}"#).unwrap();
        std::fs::write(source.join("eboot.bin"), b"broken").unwrap();
        let backup = plaintext_self();
        std::fs::write(source.join("decrypted/eboot.bin.esbak"), &backup).unwrap();
        let report = crate::fpkg_doctor::inspect(&source, &|| Ok(())).unwrap();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert_eq!(report.repairs.len(), 1);
        stage_source_with_repairs(&source, &staging, &report.repairs, &|| Ok(())).unwrap();
        assert!(preflight(&staging, 100).unwrap().ok());
        assert_eq!(std::fs::read(staging.join("eboot.bin")).unwrap(), backup);
        assert_eq!(std::fs::read(source.join("eboot.bin")).unwrap(), b"broken");
        assert!(!staging.join("decrypted").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn engine_flags_cover_kinds() {
        assert_eq!(PackageKind::from_label("DLC").engine_flag(), "ac");
        assert_eq!(PackageKind::from_label("update").engine_flag(), "patch");
        assert_eq!(PackageKind::from_label("base").engine_flag(), "app");
    }

    #[test]
    fn progress_parser_handles_both_formats() {
        let (fraction, _) = parse_progress("{\"progress\":0.5,\"message\":\"x\"}").unwrap();
        assert!((fraction - 0.5).abs() < f64::EPSILON);
        let (fraction, _) = parse_progress("42% kraken").unwrap();
        assert!((fraction - 0.42).abs() < f64::EPSILON);
        assert!(parse_progress("building").is_none());
    }

    #[test]
    fn output_parser_accepts_engine_lines() {
        assert_eq!(
            parse_output_path("OK C:/out/UP0000-PPSA00001_00-ABC-A0100-V0100.pkg"),
            Some(PathBuf::from("C:/out/UP0000-PPSA00001_00-ABC-A0100-V0100.pkg"))
        );
        assert!(parse_output_path("OK C:/out/readme.txt").is_none());
    }

    #[test]
    fn preflight_accepts_plaintext_fself_without_backup_and_keeps_bytes() {
        let dir = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(dir.join("sce_sys").join("param.json"), r#"{"titleId":"PPSA99999","contentId":"IV0000-PPSA99999_00-SSPIPACKTEST0000","contentVersion":"01.000.000"}"#).unwrap();
        let executable = plaintext_self();
        std::fs::write(dir.join("eboot.bin"), &executable).unwrap();
        let report = preflight(&dir, 100).unwrap();
        assert!(report.ok(), "{:?}", report.blockers);
        assert!(report.warnings.iter().any(|b| b.contains("No decrypted backup is required")));
        let staging = dir.with_extension("staged");
        stage_source(&dir, &staging).unwrap();
        assert!(preflight(&staging, 100).unwrap().ok());
        assert_eq!(std::fs::read(staging.join("eboot.bin")).unwrap(), executable);
        assert_eq!(std::fs::read(dir.join("eboot.bin")).unwrap(), executable);
        std::fs::write(dir.join("playgo-chunk.dat"), b"x").unwrap();
        let report = preflight(&dir, 100).unwrap();
        assert!(report.ok());
        assert!(report.warnings.iter().any(|b| b.contains("playgo")));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(staging).unwrap();
    }

    #[test]
    #[ignore = "requires SSPI_SELF_DUMP; read-only inspection of a local dump"]
    fn inspect_existing_dump_without_changes() {
        let source = PathBuf::from(std::env::var_os("SSPI_SELF_DUMP").expect("dump directory"));
        let report = crate::fpkg_doctor::inspect(&source, &|| Ok(())).unwrap();
        assert!(report.blockers().is_empty(), "{:?}", report.blockers());
        assert!(report.repairs.is_empty(), "Unexpected executable repair");
        assert!(validate_plaintext_self(&source.join("eboot.bin")).unwrap() > 0);
    }

    // A complete, bounded SELF with one digest entry and one mapped ELF load segment.
    fn plaintext_self() -> Vec<u8> {
        let mut b = vec![0u8; 720];
        b[..4].copy_from_slice(&F_SELF_MAGIC);
        b[4..8].copy_from_slice(&[0, 1, 1, 0x12]);
        b[8..12].copy_from_slice(&0x101u32.to_le_bytes());
        b[12..14].copy_from_slice(&336u16.to_le_bytes());
        b[14..16].copy_from_slice(&336u16.to_le_bytes());
        b[16..24].copy_from_slice(&720u64.to_le_bytes());
        b[24..26].copy_from_slice(&2u16.to_le_bytes());
        b[26..28].copy_from_slice(&0x22u16.to_le_bytes());
        for (i, values) in [[0x110004u64, 672, 32, 32], [0x2804, 704, 16, 16]].iter().enumerate() {
            for (j, value) in values.iter().enumerate() { b[32 + i * 32 + j * 8..40 + i * 32 + j * 8].copy_from_slice(&value.to_le_bytes()); }
        }
        let elf = &mut b[96..216];
        elf[..4].copy_from_slice(&ELF_MAGIC);
        elf[4..9].copy_from_slice(&[2, 1, 1, 9, 2]);
        elf[16..18].copy_from_slice(&0xFE10u16.to_le_bytes());
        elf[18..20].copy_from_slice(&62u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[64..68].copy_from_slice(&1u32.to_le_bytes());
        elf[68..72].copy_from_slice(&5u32.to_le_bytes());
        elf[72..80].copy_from_slice(&0x4000u64.to_le_bytes());
        elf[96..104].copy_from_slice(&16u64.to_le_bytes());
        elf[104..112].copy_from_slice(&16u64.to_le_bytes());
        elf[112..120].copy_from_slice(&0x4000u64.to_le_bytes());
        b[224..232].copy_from_slice(&0x3100000000000002u64.to_le_bytes());
        b[232..240].copy_from_slice(&1u64.to_le_bytes());
        b[704..].fill(0x90);
        b
    }

    #[test]
    fn self_validation_checks_encryption_authority_bounds_and_mappings() {
        let dir = crate::test_output_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("eboot.bin");
        let check = |bytes: &[u8]| { std::fs::write(&path, bytes).unwrap(); validate_plaintext_self(&path) };
        let original = plaintext_self();
        assert_eq!(check(&original).unwrap(), 1);
        let mut prospero = original.clone(); prospero[..4].copy_from_slice(&PROSPERO_SELF_MAGIC);
        assert_eq!(check(&prospero).unwrap(), 1);
        prospero[4] = 0x10;
        assert_eq!(check(&prospero).unwrap(), 1);
        let mut encrypted_native = prospero.clone(); encrypted_native[64] |= 2;
        assert!(check(&encrypted_native).unwrap_err().contains("encrypted"));
        prospero[4] = 0x20;
        assert!(check(&prospero).unwrap_err().contains("Unsupported SELF"));
        let mut trailing = original.clone(); trailing.extend_from_slice(b"version record");
        assert_eq!(check(&trailing).unwrap(), 1);
        for len in [0, 4, 31, 100, original.len() - 1] { assert!(check(&original[..len]).is_err(), "truncated at {len}"); }
        for (offset, value, expected) in [
            (64, 0x2806u64, "encrypted"),
            (64, 0x280C, "compressed"),
            (64, 0x102804, "mapping"),
            (72, u64::MAX, "outside"),
            (80, 15, "compressed"),
            (224, 0x4500000000000002, "fake-authority"),
            (232, 4, "fake-authority"),
            (128, u64::MAX, "program-header"),
        ] {
            let mut invalid = original.clone(); invalid[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            let error = check(&invalid).unwrap_err();
            assert!(error.contains(expected), "{offset}: {error}");
        }
        let mut missing = original.clone(); missing[64..72].copy_from_slice(&0x110004u64.to_le_bytes());
        assert!(check(&missing).unwrap_err().contains("missing data"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SSPI_SELF_DUMP and local engine; builds a host-only module fixture, not a complete game"]
    async fn local_engine_packages_existing_self_without_backup() {
        let source = PathBuf::from(std::env::var_os("SSPI_SELF_DUMP").expect("downloaded dump"));
        let original = std::fs::read(source.join("eboot.bin")).unwrap();
        assert!(validate_plaintext_self(&source.join("eboot.bin")).unwrap() > 0);
        let param: serde_json::Value = serde_json::from_slice(&std::fs::read(source.join("sce_sys/param.json")).unwrap()).unwrap();
        let root = crate::test_output_root().join(format!("self-module-fixture-{}", uuid::Uuid::new_v4()));
        let fixture = root.join("fixture");
        for name in ["eboot.bin", "sce_module/libc.prx", "sce_sys/about/right.sprx", "sce_sys/param.json", "sce_sys/icon0.png"] {
            if !source.join(name).is_file() { continue; }
            let target = fixture.join(name);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(source.join(name), target).unwrap();
        }
        let staging = root.join("staged");
        stage_source(&fixture, &staging).unwrap();
        assert!(preflight(&staging, 100).unwrap().ok());
        assert!(!staging.join("decrypted").exists());
        let mut options = PackageOptions::new(&staging, root.join("host-only-packages"));
        options.preset = PackagePreset::Fastest;
        let outcome = build(&locate_engine(None).unwrap(), &options, |line| println!("{line}"), |_, _| {}).await.unwrap();
        assert_eq!(package_identity(&outcome.output).unwrap(), param["contentId"].as_str().unwrap());
        assert_eq!(std::fs::read(staging.join("eboot.bin")).unwrap(), original);
        assert_eq!(std::fs::read(source.join("eboot.bin")).unwrap(), original);
        println!("Host-only module package: {} (not a complete game or console launch test)", outcome.output.display());
    }
}

pub fn has_backport_runtime(root: &Path) -> bool {
    root.join("fakelib").is_dir() || root.join("fakelib2").is_dir() || root.join("ampr_emu.index").is_file()
}
