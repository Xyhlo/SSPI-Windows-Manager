use super::*;
use std::io::{Read, Write};

pub(super) fn safe_extraction_path(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| {
        matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)
    }) && !path.to_string_lossy().contains(':')
}

// ---------------------------------------------------------------------------
// Windows storage + truthful progress helpers.
//
// These only describe where extraction/upload staging live, whether a volume
// can hold the output, how far extraction has honestly progressed, and which
// recorded files may be deleted after a validated extraction. Download
// requests, ranges, concurrency, retries and the mount protocol are untouched.
// ---------------------------------------------------------------------------

/// Slack kept free beyond the extracted total so a large archive cannot fill
/// the volume Windows runs on.
pub(super) const EXTRACTION_SLACK_BYTES: u64 = 128 * 1024 * 1024;
/// Minimum elapsed time / bytes before an extraction speed is reported, so the
/// first buffered megabytes cannot spike the displayed rate.
pub(super) const EXTRACTION_SPEED_FLOOR_SECS: f64 = 0.25;
pub(super) const EXTRACTION_SPEED_FLOOR_BYTES: u64 = 1024 * 1024;

fn drive_prefix(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => {
            Some(format!("{}:", letter.to_ascii_uppercase()))
        }
        _ => None,
    }
}

/// Actual Windows/system volume prefix (e.g. "D:"), never assumed to be C:.
/// Reads SystemDrive, falling back to SystemRoot/windir ("D:\Windows" -> "D:").
///
/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn system_volume_prefix() -> Option<String> {
    if let Ok(drive) = std::env::var("SystemDrive") {
        if let Some(prefix) = drive_prefix(&drive) {
            return Some(prefix);
        }
    }
    for key in ["SystemRoot", "windir"] {
        if let Ok(root) = std::env::var(key) {
            if let Some(prefix) = drive_prefix(&root) {
                return Some(prefix);
            }
        }
    }
    None
}

/// Volume holding `path`: drive prefix ("D:") or UNC host+share
/// ("\\server\share"). Returns None for relative paths.
pub(super) fn volume_of(path: &Path) -> Option<String> {
    let raw = path.to_string_lossy().replace('/', "\\");
    if raw.starts_with("\\\\") {
        let mut parts = raw[2..].split('\\').filter(|part| !part.is_empty());
        if let (Some(host), Some(share)) = (parts.next(), parts.next()) {
            return Some(format!("\\\\{host}\\{share}"));
        }
        return None;
    }
    drive_prefix(&raw)
}

pub(super) fn volume_key(path: &Path) -> String {
    volume_of(path).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn is_on_system_volume_with(path: &Path, system: Option<&str>) -> bool {
    match (volume_of(path), system) {
        (Some(volume), Some(prefix)) => volume.eq_ignore_ascii_case(prefix),
        _ => false,
    }
}

/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn is_on_system_volume(path: &Path) -> bool {
    let prefix = system_volume_prefix();
    is_on_system_volume_with(path, prefix.as_deref())
}

/// Warning text naming every affected path when download, extraction or upload
/// staging would land on the Windows/system drive. Returns None when placement
/// is fine or the system volume cannot be determined (never guess C:).
///
/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn describe_system_drive_placement(
    download_dir: &Path,
    extraction_dir: &Path,
    staging_dir: &Path,
) -> Option<String> {
    let prefix = system_volume_prefix();
    let affected: Vec<String> = [
        ("download", download_dir),
        ("extraction", extraction_dir),
        ("upload staging", staging_dir),
    ]
    .into_iter()
    .filter(|(_, dir)| is_on_system_volume_with(dir, prefix.as_deref()))
    .map(|(role, dir)| format!("{role} ({})", dir.display()))
    .collect();
    if affected.is_empty() {
        return None;
    }
    let volumes = prefix.map(|prefix| format!(" ({prefix})")).unwrap_or_default();
    Some(format!(
        "Warning: {} on the Windows system drive{volumes}. Large downloads and extraction can fill the drive Windows runs on. Choose a different folder/drive in Settings before starting.",
        affected.join(", "),
    ))
}

/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn sanitize_set_id(set: &str) -> String {
    let cleaned: String = set
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if cleaned.trim_matches('_').is_empty() {
        "set".into()
    } else {
        cleaned
    }
}

/// Extraction scratch for a chosen download root. Temporary extraction files
/// always stay under the selected destination, never on an assumed drive.
///
/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn extraction_dir(download_dir: &Path) -> PathBuf {
    download_dir.join("extracted")
}

/// Multipart staging for a chosen download root ("archive_<set>" per set).
///
/// Delivery-wiring API (lib.rs staging/upload wiring + unit tests).
#[allow(dead_code)]
pub(super) fn staging_dir_for_set(download_dir: &Path, archive_set: Option<&str>) -> PathBuf {
    match archive_set {
        Some(set) => download_dir.join(format!("archive_{}", sanitize_set_id(set))),
        None => download_dir.to_path_buf(),
    }
}

/// Creates (or verifies) a destination without ever falling back elsewhere.
/// Errors name the role + path and point at Settings so an unavailable drive
/// is reported instead of silently landing on the system volume.
pub(super) fn require_writable_dir(path: &Path, role: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(path).map_err(|error| {
        format!(
            "{role} folder is unavailable at {} ({}). Choose another folder/drive in Settings; refusing to fall back to the system drive.",
            path.display(),
            redact(error),
        )
    })?;
    Ok(path.to_path_buf())
}

/// Per-volume space check: requirements sharing a volume are summed once (plus
/// one slack allowance per volume) and compared against a single free-space
/// probe, so shared download/extraction/staging volumes are never
/// double-counted. Unknown free space (None) cannot prove insufficiency and
/// passes; unknown *totals* must be surfaced as indeterminate progress by the
/// caller, never invented here.
pub(super) fn check_space_on_volumes(
    requirements: &[(PathBuf, u64)],
    slack_bytes: u64,
) -> Result<(), String> {
    use std::collections::HashMap;
    let mut per_volume: HashMap<String, (PathBuf, u64)> = HashMap::new();
    for (dir, need) in requirements {
        let entry = per_volume
            .entry(volume_key(dir))
            .or_insert_with(|| (dir.clone(), 0));
        entry.1 = entry.1.saturating_add(*need);
    }
    let mut keys: Vec<String> = per_volume.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let (dir, need) = &per_volume[&key];
        let need = need.saturating_add(slack_bytes);
        if let Some(free) = free_space(dir) {
            if free < need {
                return Err(format!(
                    "Not enough extraction space on {key}: {:.2} GB required, {:.2} GB free at {}. Free space or choose a different folder in Settings.",
                    need as f64 / 1_073_741_824.0,
                    free as f64 / 1_073_741_824.0,
                    dir.display(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtractionPhase {
    Inspection,
    Extraction,
    Finalization,
    // Constructed by the delivery cleanup wiring + tests.
    #[allow(dead_code)]
    Cleanup,
}

pub(super) fn extraction_phase_label(phase: ExtractionPhase) -> &'static str {
    match phase {
        ExtractionPhase::Inspection => "inspection",
        ExtractionPhase::Extraction => "extraction",
        ExtractionPhase::Finalization => "finalization",
        ExtractionPhase::Cleanup => "cleanup",
    }
}

/// Tags an extraction failure with its exact phase + affected path so a retry
/// knows what broke and where. Existing message bodies are preserved after the
/// tag so current UI matching keeps working.
pub(super) fn phase_error(phase: ExtractionPhase, path: &Path, detail: impl ToString) -> String {
    format!(
        "extraction/{}: {}: {}",
        extraction_phase_label(phase),
        path.display(),
        detail.to_string(),
    )
}

/// Recovers the failing phase from a tagged error. Returns "unknown" for
/// untagged (legacy) errors rather than guessing.
pub(super) fn error_phase(error: &str) -> &'static str {
    match error.split(':').next().unwrap_or("").trim() {
        "extraction/inspection" => "inspection",
        "extraction/extraction" => "extraction",
        "extraction/finalization" => "finalization",
        "cleanup" => "cleanup",
        "staging" => "staging",
        "upload" => "upload",
        _ => "unknown",
    }
}

// Cleanup-failure classifier for retry UI + tests.
#[allow(dead_code)]
pub(super) fn is_cleanup_error(error: &str) -> bool {
    error.starts_with("cleanup:")
}

/// Honest extraction meter: bytes actually written plus completed files.
/// `fraction` is None when the uncompressed total is unknown so the UI shows
/// file counts + indeterminate progress instead of a made-up bar. A phase is
/// only complete after its required work succeeds (`is_complete`).
#[derive(Debug, Clone)]
pub(super) struct ExtractionMeter {
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub files_done: u64,
    pub files_total: u64,
}

impl ExtractionMeter {
    pub(super) fn new(total_bytes: u64, files_total: u64) -> Self {
        Self {
            total_bytes,
            done_bytes: 0,
            files_done: 0,
            files_total,
        }
    }

    pub(super) fn add_bytes(&mut self, n: u64) {
        self.done_bytes = self.done_bytes.saturating_add(n);
    }

    pub(super) fn complete_file(&mut self) {
        self.files_done = self.files_done.saturating_add(1);
    }

    // Indeterminate-total helper for progress UI + tests (ZIP totals are
    // always known; RAR totals come from the header walk in lib.rs).
    #[allow(dead_code)]
    pub(super) fn fraction(&self) -> Option<f64> {
        if self.total_bytes == 0 {
            None
        } else {
            Some(
                (self.done_bytes.min(self.total_bytes) as f64 / self.total_bytes as f64)
                    .min(0.99),
            )
        }
    }

    pub(super) fn is_complete(&self) -> bool {
        self.files_total > 0
            && self.files_done >= self.files_total
            && (self.total_bytes == 0 || self.done_bytes >= self.total_bytes)
    }
}

pub(super) fn honest_extraction_speed(done_bytes: u64, elapsed_secs: f64) -> f64 {
    if elapsed_secs < EXTRACTION_SPEED_FLOOR_SECS || done_bytes < EXTRACTION_SPEED_FLOOR_BYTES {
        0.
    } else {
        done_bytes as f64 / elapsed_secs.max(0.01)
    }
}

#[derive(Debug, Clone, Copy)]
// File counts + phase ride along for the phased progress API (delivery wiring
// and unit tests); the legacy byte callback below has no slot for them.
#[allow(dead_code)]
pub(super) struct ExtractionReport {
    pub phase: ExtractionPhase,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub speed_bps: f64,
}

/// Metered ZIP extraction core. Reports explicit inspection -> extraction ->
/// finalization phases from actual extracted bytes + completed files; unknown
/// totals stay indeterminate (total 0) instead of guessing.
fn extract_zip_tree_metered(
    source: &Path,
    dest: &Path,
    report: &dyn Fn(ExtractionReport),
) -> Result<ExtractionMeter, String> {
    extract_zip_tree_controlled(source, dest, report, &|| Ok(()))
}

fn extract_zip_tree_controlled(source: &Path, dest: &Path, report: &dyn Fn(ExtractionReport), checkpoint: &dyn Fn() -> Result<(), String>) -> Result<ExtractionMeter, String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(source).map_err(redact)?)
        .map_err(|error| phase_error(ExtractionPhase::Inspection, source, redact(error)))?;
    // Unavailable destinations are reported, never silently replaced.
    require_writable_dir(dest, "extraction")
        .map_err(|error| phase_error(ExtractionPhase::Inspection, dest, error))?;
    report(ExtractionReport {
        phase: ExtractionPhase::Inspection,
        done_bytes: 0,
        total_bytes: 0,
        files_done: 0,
        files_total: 0,
        speed_bps: 0.,
    });
    let mut total = 0u64;
    let mut files_total = 0u64;
    let mut names = std::collections::HashSet::new();
    for i in 0..zip.len() {
        checkpoint()?;
        let entry = zip
            .by_index(i)
            .map_err(|error| phase_error(ExtractionPhase::Inspection, source, redact(error)))?;
        let path = entry
            .enclosed_name()
            .ok_or_else(|| phase_error(ExtractionPhase::Inspection, source, "ZIP contains an unsafe path"))?;
        if !safe_extraction_path(&path)
            || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err(phase_error(
                ExtractionPhase::Inspection,
                source,
                "ZIP contains an unsafe path or symbolic link",
            ));
        }
        if entry.is_dir() {
            continue;
        }
        if !names.insert(path.to_string_lossy().to_lowercase()) {
            return Err(phase_error(
                ExtractionPhase::Inspection,
                source,
                "ZIP contains duplicate file paths",
            ));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| phase_error(ExtractionPhase::Inspection, source, "ZIP expanded size overflow"))?;
        files_total += 1;
    }
    if files_total == 0 {
        return Err(phase_error(
            ExtractionPhase::Inspection,
            source,
            "ZIP contains no files",
        ));
    }
    check_space_on_volumes(&[(dest.to_path_buf(), total)], EXTRACTION_SLACK_BYTES)
        .map_err(|error| phase_error(ExtractionPhase::Inspection, source, error))?;
    let mut meter = ExtractionMeter::new(total, files_total);
    let started = Instant::now();
    let mut reported = Instant::now();
    let mut buffer = vec![0u8; 1024 * 1024];
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|error| phase_error(ExtractionPhase::Extraction, source, redact(error)))?;
        let out = dest.join(
            entry
                .enclosed_name()
                .ok_or_else(|| phase_error(ExtractionPhase::Extraction, source, "ZIP contains an unsafe path"))?,
        );
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|error| phase_error(ExtractionPhase::Extraction, &out, redact(error)))?;
            continue;
        }
        std::fs::create_dir_all(out.parent().ok_or_else(|| {
            phase_error(ExtractionPhase::Extraction, source, "ZIP entry has no parent")
        })?)
        .map_err(|error| phase_error(ExtractionPhase::Extraction, &out, redact(error)))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .map_err(|error| phase_error(ExtractionPhase::Extraction, &out, redact(error)))?;
        loop {
            checkpoint()?;
            if meter.done_bytes % (64 * 1024 * 1024) < buffer.len() as u64 {
                storage::guard_bytes(dest, meter.total_bytes.saturating_sub(meter.done_bytes), "extraction")?;
            }
            let read = entry
                .read(&mut buffer)
                .map_err(|error| phase_error(ExtractionPhase::Extraction, &out, redact(error)))?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|error| phase_error(ExtractionPhase::Extraction, &out, redact(error)))?;
            meter.add_bytes(read as u64);
            if reported.elapsed() >= Duration::from_millis(250) {
                report(ExtractionReport {
                    phase: ExtractionPhase::Extraction,
                    done_bytes: meter.done_bytes,
                    total_bytes: meter.total_bytes,
                    files_done: meter.files_done,
                    files_total: meter.files_total,
                    speed_bps: honest_extraction_speed(
                        meter.done_bytes,
                        started.elapsed().as_secs_f64(),
                    ),
                });
                reported = Instant::now();
            }
        }
        file.flush()
            .map_err(|error| phase_error(ExtractionPhase::Extraction, &out, redact(error)))?;
        meter.complete_file();
    }
    report(ExtractionReport {
        phase: ExtractionPhase::Finalization,
        done_bytes: meter.done_bytes,
        total_bytes: meter.total_bytes,
        files_done: meter.files_done,
        files_total: meter.files_total,
        speed_bps: 0.,
    });
    // Finalization completes only after its required work succeeds.
    if !meter.is_complete() {
        return Err(phase_error(
            ExtractionPhase::Finalization,
            dest,
            format!(
                "extraction finished incomplete ({}/{} files, {}/{} bytes)",
                meter.files_done, meter.files_total, meter.done_bytes, meter.total_bytes,
            ),
        ));
    }
    Ok(meter)
}

pub(super) fn extract_zip_tree(source: &Path, dest: &Path, progress: &dyn Fn(u64, u64, f64), checkpoint: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    // Legacy (done, total, speed) callback kept for the current caller; the
    // measured work happens in extract_zip_tree_metered above.
    let result = extract_zip_tree_controlled(source, dest, &|report| {
        progress(report.done_bytes, report.total_bytes, report.speed_bps);
    }, checkpoint);
    match result {
        Ok(meter) => {
            progress(meter.done_bytes, meter.total_bytes.max(1), 0.);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn extract_content(
    source: &Path, cache: &Path, kind: ArtifactKind, password: Option<&str>,
    progress: Arc<dyn Fn(u64, u64, f64) + Send + Sync>, depth: usize,
) -> Result<ExtractedContent, String> {
    extract_content_controlled(source, cache, kind, password, progress, depth, Arc::new(|| Ok(())))
}

pub(super) fn extract_content_controlled(
    source: &Path, cache: &Path, kind: ArtifactKind, password: Option<&str>,
    progress: Arc<dyn Fn(u64, u64, f64) + Send + Sync>, depth: usize,
    checkpoint: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<ExtractedContent, String> {
    checkpoint()?;
    if depth > 3 {
        return Err(phase_error(
            ExtractionPhase::Inspection,
            source,
            "Archive nesting exceeds 3 levels",
        ));
    }
    if kind == ArtifactKind::Pkg { return Ok(ExtractedContent::Pkgs(vec![source.to_owned()])); }
    if kind == ArtifactKind::SevenZ {
        return Err(phase_error(
            ExtractionPhase::Inspection,
            source,
            "7z archives are not supported by this build. Extract the complete set with 7-Zip, then select the extracted folder.",
        ));
    }
    if kind == ArtifactKind::Unknown {
        return Err(phase_error(
            ExtractionPhase::Inspection,
            source,
            "File has no supported archive or PKG signature",
        ));
    }
    std::fs::create_dir_all(cache)
        .map_err(|error| phase_error(ExtractionPhase::Inspection, cache, redact(error)))?;
    let dest = cache.join(Uuid::new_v4().to_string());
    let mut result = (|| {
        match kind {
            ArtifactKind::Zip => extract_zip_tree(source, &dest, progress.as_ref(), checkpoint.as_ref())?,
            ArtifactKind::Rar => {
                let first = rar_first_volume(source);
                let listed = if first.is_file() { first.as_path() } else { source };
                let size = archive_passwords(password).into_iter().find_map(|p| rar_list_size(listed, p).ok());
                if let (Some(size), Some(free)) = (size, free_space(cache)) {
                    if free < size.saturating_add(128 * 1024 * 1024) {
                        return Err(phase_error(
                            ExtractionPhase::Inspection,
                            source,
                            format!("Not enough free space to extract this RAR: {:.2} GiB required, {:.2} GiB free. Free space and Retry; the archive is retained.", size as f64 / 1_073_741_824., free as f64 / 1_073_741_824.),
                        ));
                    }
                }
                extract_rar_builtin(source, &dest, password, progress.clone(), checkpoint.clone())
                    .map_err(|error| phase_error(ExtractionPhase::Extraction, source, error))?;
            }
            _ => unreachable!(),
        }
        if let Some(content) = extracted_from_dir(&dest) { return Ok(content); }
        let mut nested_packages = Vec::new();
        for inner in nested_rar_volumes(&dest) {
            let mut header = [0u8; 8];
            let mut file = std::fs::File::open(&inner)
                .map_err(|error| phase_error(ExtractionPhase::Inspection, &inner, redact(error)))?;
            let read = file
                .read(&mut header)
                .map_err(|error| phase_error(ExtractionPhase::Inspection, &inner, redact(error)))?;
            let kind = artifact_kind(&header[..read], &inner.to_string_lossy(), "");
            match extract_content_controlled(&inner, &dest, kind, password, progress.clone(), depth + 1, checkpoint.clone())
                .map_err(|error| {
                    // Keep the exact inner phase; tag only untagged (legacy) errors.
                    let tagged = if error_phase(&error) == "unknown" {
                        phase_error(ExtractionPhase::Extraction, &inner, error)
                    } else {
                        error
                    };
                    format!("{}: {tagged}", inner.file_name().unwrap_or_default().to_string_lossy())
                })? {
                ExtractedContent::Dump(root) => return Ok(ExtractedContent::Dump(root)),
                ExtractedContent::Pkgs(mut pkgs) => nested_packages.append(&mut pkgs),
            }
        }
        if !nested_packages.is_empty() { return Ok(ExtractedContent::Pkgs(nested_packages)); }
        Err(phase_error(
            ExtractionPhase::Finalization,
            &dest,
            format!("Archive has no valid PKG or PS5 dump (eboot.bin + sce_sys): {}", extract_listing(&dest)),
        ))
    })();
    // Failure removes only this attempt's partial outputs. The source archive
    // inputs are never touched here, so failure/cancellation preserves them
    // for retry; successful outputs are kept for upload retries by never
    // deleting anything except via remove_consumed_inputs below.
    // Output validation before reporting success: every persisted output path
    // must exist, otherwise finalization fails instead of claiming completion.
    if let Ok(content) = &result {
        let outputs = extracted_outputs(content);
        if let Some(missing) = outputs.iter().find(|path| !path.exists()) {
            result = Err(phase_error(
                ExtractionPhase::Finalization,
                missing,
                "extracted output is missing after extraction",
            ));
        }
    }
    if result.is_err() { let _ = std::fs::remove_dir_all(&dest); }
    result
}

/// Output paths the caller must persist as job retry state BEFORE deleting any
/// inputs. Extracted outputs are always kept for upload retries.
pub(super) fn extracted_outputs(content: &ExtractedContent) -> Vec<PathBuf> {
    match content {
        ExtractedContent::Pkgs(pkgs) => pkgs.clone(),
        ExtractedContent::Dump(root) => vec![root.clone()],
    }
}

/// Deletes only recorded job-owned archive/part files, after all handles are
/// closed, outputs are validated, and the output paths above are persisted.
/// Never touches extracted outputs (a consumed entry matching an output is
/// refused, not deleted). Missing files are already-gone renames and pass so
/// cleanup stays idempotent/retryable; real failures are tagged "cleanup:" and
/// name every affected file plus where the kept outputs live.
/// (On Windows an still-open handle surfaces here as a retryable cleanup
/// error instead of a silent skip: close inputs before calling.)
///
/// What a job shows while it waits for one of the scheduler's extraction slots.
pub(super) fn extraction_wait_message(ahead: usize) -> String {
    if ahead > 0 { format!("Waiting for an extraction slot · {ahead} ahead") } else { "Waiting for an extraction slot".into() }
}

pub(super) fn remove_consumed_inputs(
    consumed: &[PathBuf],
    outputs: &[PathBuf],
) -> Result<(), String> {
    let mut failures = Vec::new();
    for path in consumed {
        if outputs.iter().any(|output| output == path) {
            failures.push(format!(
                "{} (refused: extracted output, kept for upload retry)",
                path.display()
            ));
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Err(error) = std::fs::remove_file(path) {
            failures.push(format!("{} ({})", path.display(), redact(error)));
        }
    }
    // Removing an empty owned staging directory does not touch unrelated files
    // or another job's remaining volumes. Never recurse during this pruning.
    for parent in consumed.iter().filter_map(|path| path.parent()) {
        let name = parent.file_name().unwrap_or_default().to_string_lossy();
        if name.strip_prefix("archive_").and_then(|name| name.get(..36)).is_some_and(|id| Uuid::parse_str(id).is_ok()) {
            let _ = std::fs::remove_dir(parent);
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    let kept = if outputs.is_empty() {
        "no outputs recorded".to_owned()
    } else {
        outputs
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("; ")
    };
    Err(format!(
        "{}: failed to remove {} archive file(s) ({}); extracted outputs were kept ({}). Close any program holding the files and retry cleanup.",
        extraction_phase_label(ExtractionPhase::Cleanup),
        failures.len(),
        failures.join(", "),
        kept,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumed_archive_prunes_empty_owned_folder_but_keeps_other_parts() {
        let root = crate::test_output_root().join(Uuid::new_v4().to_string());
        let owned = root.join(format!("archive_{}_base", Uuid::new_v4()));
        std::fs::create_dir_all(&owned).unwrap();
        let a = owned.join("part1.rar"); let b = owned.join("part2.rar");
        std::fs::write(&a, b"one").unwrap(); std::fs::write(&b, b"two").unwrap();
        remove_consumed_inputs(&[a], &[]).unwrap(); assert!(b.exists());
        remove_consumed_inputs(&[b], &[]).unwrap(); assert!(!owned.exists()); assert!(root.exists());
    }
    #[test]
    fn zip_preserves_ps5_dump_structure() {
        let (source, cache) = crate::tests::make_zip(vec![
            ("Game/eboot.bin", vec![1,2,3]), ("Game/sce_sys/param.json", b"{}".to_vec()),
            ("Game/data/levels.bin", vec![4,5,6])]);
        let ExtractedContent::Dump(root) = extract_content(&source, &cache, ArtifactKind::Zip, None, Arc::new(|_,_,_|{}), 0).unwrap() else { panic!("expected dump") };
        assert_eq!(std::fs::read(root.join("data/levels.bin")).unwrap(), [4,5,6]);
        let _=std::fs::remove_dir_all(source.parent().unwrap());
    }
    #[test]
    fn source_password_is_tried_first_and_brackets_are_kept() {
        assert_eq!(archive_passwords(Some("secret"))[0], b"secret");
        assert!(archive_passwords(None).contains(&b"[DLPSGAME.COM]".as_slice()));
    }
    #[cfg(windows)]
    #[test]
    fn source_password_variants_decrypt_rar_headers_and_files() {
        use std::os::windows::process::CommandExt;
        let archiver = Path::new("C:/Program Files/WinRAR/Rar.exe");
        if !archiver.is_file() { eprintln!("skipping: local WinRAR is required to create encrypted fixtures"); return; }
        let root = crate::test_output_root().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("payload.txt");
        let contents = b"SSPI encrypted archive password fallback\n";
        std::fs::write(&input, contents).unwrap();
        for (index, password) in ["www.DLPSGAME.COM", "[DLPSGAME.COM]", "DLPSGAME.COM"].iter().enumerate() {
            let archive = root.join(format!("variant-{index}.rar"));
            let status = std::process::Command::new(archiver)
                .args(["a", "-idq", "-ep", &format!("-hp{password}")])
                .arg(&archive).arg(&input).creation_flags(0x08000000).status().unwrap();
            assert!(status.success());
            assert_eq!(storage::archive_size(&archive, ArtifactKind::Rar, Some("outdated-source-password")).unwrap(), Some(contents.len() as u64));
            let destination = root.join(format!("extracted-{index}"));
            extract_rar_builtin(&archive, &destination, Some("outdated-source-password"), Arc::new(|_,_,_| {}), Arc::new(|| Ok(()))).unwrap();
            assert_eq!(std::fs::read(destination.join("payload.txt")).unwrap(), contents);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn old_style_rar_sets_start_at_the_rar_volume() {
        assert_eq!(rar_first_volume(Path::new("d/lfc-cusa00109.r00")), PathBuf::from("d/lfc-cusa00109.rar"));
        assert_eq!(rar_first_volume(Path::new("d/lfc-cusa00109.r17")), PathBuf::from("d/lfc-cusa00109.rar"));
        assert_eq!(rar_first_volume(Path::new("d/GAME.R05")), PathBuf::from("d/GAME.RAR"));
        assert_eq!(rar_first_volume(Path::new("d/game.rar")), PathBuf::from("d/game.rar"));
        assert_eq!(rar_first_volume(Path::new("d/game.part07.rar")), PathBuf::from("d/game.part01.rar"));
        assert_eq!(rar_first_volume(Path::new("d/game.003")), PathBuf::from("d/game.001"));
    }
    /// Stored RAR 4.x volumes with old-style names (name.rar, name.r00, …).
    /// Current WinRAR can no longer create these, but older releases use them.
    fn rar4_old_style_set(stem: &str, name: &str, data: &[u8], chunk: usize) -> Vec<(String, Vec<u8>)> {
        fn crc32(bytes: &[u8]) -> u32 {
            !bytes.iter().fold(!0u32, |crc, &byte| (0..8).fold(crc ^ byte as u32, |c, _| (c >> 1) ^ (0xEDB8_8320 & (c & 1).wrapping_neg())))
        }
        fn block(kind: u8, flags: u16, body: &[u8]) -> Vec<u8> {
            let mut header = vec![kind];
            header.extend(flags.to_le_bytes());
            header.extend((7 + body.len() as u16).to_le_bytes());
            header.extend(body);
            let mut out = (crc32(&header) as u16).to_le_bytes().to_vec();
            out.extend(header);
            out
        }
        let parts: Vec<&[u8]> = data.chunks(chunk).collect();
        parts.iter().enumerate().map(|(index, part)| {
            let (first, last) = (index == 0, index + 1 == parts.len());
            let mut body = Vec::new();
            body.extend((part.len() as u32).to_le_bytes());
            body.extend((data.len() as u32).to_le_bytes());
            body.push(2); // Windows host
            body.extend((if last { crc32(data) } else { crc32(part) }).to_le_bytes());
            body.extend(0x5B2A_0000u32.to_le_bytes()); // DOS time
            body.extend([29, 0x30]); // unpack version 2.9, stored
            body.extend((name.len() as u16).to_le_bytes());
            body.extend(0x20u32.to_le_bytes());
            body.extend(name.as_bytes());
            let mut volume = b"Rar!\x1a\x07\x00".to_vec();
            volume.extend(block(0x73, 0x0001 | if first { 0x0100 } else { 0 }, &[0; 6])); // volume, first volume
            volume.extend(block(0x74, 0x8000 | if first { 0 } else { 0x01 } | if last { 0 } else { 0x02 }, &body));
            volume.extend_from_slice(part);
            volume.extend(block(0x7B, if last { 0 } else { 0x0001 }, &[])); // more volumes follow
            let file = if first { format!("{stem}.rar") } else { format!("{stem}.r{:02}", index - 1) };
            (file, volume)
        }).collect()
    }
    #[test]
    fn nested_old_style_rar_set_extracts_from_its_rar_volume() {
        // Tomb Raider's download: one archive wrapping a release folder that holds
        // lfc-cusa00109.rar/.r00/.r01. Windows lists .r00 before .rar.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut payload = b"\x7fCNT".to_vec();
        payload.extend((0..300 * 1024).map(|_| { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state as u8 }));
        let volumes = rar4_old_style_set("lfc-test", "game.pkg", &payload, 100 * 1024);
        assert_eq!(volumes.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["lfc-test.rar", "lfc-test.r00", "lfc-test.r01", "lfc-test.r02"]);
        let names: Vec<String> = volumes.iter().map(|(name, _)| format!("Release-TEST/{name}")).collect();
        let (source, cache) = crate::tests::make_zip(names.iter().map(String::as_str).zip(volumes.into_iter().map(|(_, bytes)| bytes)).collect());
        let content = extract_content(&source, &cache, ArtifactKind::Zip, None, Arc::new(|_,_,_| {}), 0).unwrap();
        let ExtractedContent::Pkgs(pkgs) = content else { panic!("expected a PKG") };
        assert_eq!(pkgs.len(), 1);
        assert_eq!(std::fs::read(&pkgs[0]).unwrap(), payload);
        std::fs::remove_dir_all(source.parent().unwrap()).unwrap();
    }
    #[test]
    fn encrypted_rar_extracts_with_the_explicit_password() {
        let source=Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encrypted.rar");
        if !source.is_file() {
            eprintln!("skipping: tests/fixtures/encrypted.rar is not shipped with the repo");
            return;
        }
        let dest=crate::test_output_root().join(Uuid::new_v4().to_string());
        assert!(rar_extract_to(&source,&dest,b"wrong-password").is_err());
        extract_rar_builtin(&source,&dest,Some("unrar"),Arc::new(|_,_,_|{}),Arc::new(|| Ok(()))).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join(".gitignore")).unwrap(),"target\nCargo.lock\n");
        let _=std::fs::remove_dir_all(dest);
    }
    #[test]
    fn zip_rejects_traversal_even_for_non_pkg_entries() {
        let (source, cache) = crate::tests::make_zip(vec![("../escape.txt", vec![1])]);
        assert!(extract_content(&source,&cache,ArtifactKind::Zip,None,Arc::new(|_,_,_|{}),0).is_err());
        assert!(!source.parent().unwrap().join("escape.txt").exists());
        let _=std::fs::remove_dir_all(source.parent().unwrap());
    }

    #[test]
    fn cancelled_extraction_preserves_archive_and_removes_partial_output() {
        let (source, cache) = crate::tests::make_zip(vec![("game/large.bin", vec![1; 4 * 1024 * 1024])]);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let error = extract_content_controlled(&source, &cache, ArtifactKind::Zip, None, Arc::new(|_,_,_|{}), 0,
            Arc::new(move || if count.fetch_add(1, Ordering::SeqCst) >= 5 { Err("cancelled".into()) } else { Ok(()) })).err().expect("cancelled extraction");
        assert!(error.contains("cancelled"));
        assert!(source.is_file());
        assert!(!cache.exists() || std::fs::read_dir(cache).unwrap().next().is_none());
    }

    #[test]
    fn rar_callback_can_cancel_with_archive_open() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encrypted.rar");
        if !source.exists() { return; }
        let dest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/rar-control-tests").join(Uuid::new_v4().to_string());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let error = rar_control::extract(&source, &dest, b"unrar", &|| {
            if calls.fetch_add(1, Ordering::SeqCst) >= 2 { Err("cancelled".into()) } else { Ok(()) }
        }).unwrap_err();
        assert_eq!(error, "cancelled");
        assert!(source.is_file());
        // Reopening and extracting proves cancellation released the native handle.
        assert!(rar_control::extract(&source, &dest, b"unrar", &|| Ok(())).is_ok());
    }
    #[test]
    fn system_volume_is_detected_not_assumed() {
        // A non-C Windows volume must be detectable; the code never hardcodes C:.
        assert!(is_on_system_volume_with(Path::new("D:\\Games\\x"), Some("D:")));
        assert!(is_on_system_volume_with(Path::new("d:/games/x"), Some("D:")));
        assert!(!is_on_system_volume_with(Path::new("C:\\Games\\x"), Some("D:")));
        assert!(!is_on_system_volume_with(Path::new("\\\\server\\share\\x"), Some("D:")));
        assert!(!is_on_system_volume_with(Path::new("relative\\x"), Some("D:")));
        assert!(!is_on_system_volume_with(Path::new("D:\\Games\\x"), None));
        assert_eq!(volume_of(Path::new("D:\\Games\\x")).as_deref(), Some("D:"));
        assert_eq!(volume_of(Path::new("\\\\server\\share\\dir\\f")).as_deref(), Some("\\\\server\\share"));
        assert_eq!(volume_of(Path::new("relative\\x")), None);
        // The live detector returns a valid "X:" shape or honestly reports None.
        if let Some(prefix) = system_volume_prefix() {
            assert_eq!(prefix.len(), 2);
            assert!(prefix.chars().next().unwrap().is_ascii_alphabetic());
            assert!(prefix.ends_with(':'));
        }
        // Placement warning names affected paths and points at Settings.
        if let Some(prefix) = system_volume_prefix() {
            let on_system = PathBuf::from(format!("{prefix}\\GameSearch\\dl"));
            let warning = describe_system_drive_placement(&on_system, &on_system, &on_system).unwrap();
            assert!(warning.contains("system drive") && warning.contains("Settings"));
            assert!(warning.contains(&on_system.display().to_string()));
        }
        assert!(describe_system_drive_placement(
            Path::new("relative\\dl"), Path::new("relative\\extracted"), Path::new("relative\\dl"),
        ).is_none());
        assert!(describe_system_drive_placement(
            Path::new("\\\\server\\share\\dl"), Path::new("\\\\server\\share\\ex"), Path::new("\\\\server\\share\\dl"),
        ).is_none());
    }
    #[test]
    fn staging_layout_uses_selected_destination() {
        let root = PathBuf::from("D:\\Games\\Import");
        assert_eq!(extraction_dir(&root), PathBuf::from("D:\\Games\\Import\\extracted"));
        assert_eq!(
            staging_dir_for_set(&root, Some("set:1/2")),
            PathBuf::from("D:\\Games\\Import\\archive_set_1_2"),
        );
        assert_eq!(staging_dir_for_set(&root, None), root);
        assert_eq!(sanitize_set_id("a/b\\c:set"), "a_b_c_set");
        assert_eq!(sanitize_set_id("///"), "set");
        // Temporary extraction files resolve under the selected root.
        let tmp = crate::test_output_root().join(Uuid::new_v4().to_string());
        let extract = extraction_dir(&tmp);
        require_writable_dir(&extract, "extraction").unwrap();
        let probe = extract.join("part.tmp");
        std::fs::write(&probe, b"tmp").unwrap();
        assert!(probe.starts_with(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn unavailable_destination_reports_without_fallback() {
        let root = crate::test_output_root().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let bad = blocker.join("subdir");
        let error = require_writable_dir(&bad, "extraction").err().expect("expected unavailable-dir error");
        assert!(error.contains("extraction") && error.contains("refusing to fall back"));
        assert!(!bad.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn space_check_sums_shared_volumes_once() {
        let root = crate::test_output_root().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let Some(free) = free_space(&root) else {
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        if free < EXTRACTION_SLACK_BYTES + 4 * 1024 * 1024 {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let fits = free.saturating_sub(EXTRACTION_SLACK_BYTES).saturating_sub(1024 * 1024);
        assert!(check_space_on_volumes(&[(root.join("a"), fits)], EXTRACTION_SLACK_BYTES).is_ok());
        // The same requirement twice on one shared volume must be summed.
        let half = free.saturating_sub(EXTRACTION_SLACK_BYTES) / 2 + 1024 * 1024;
        let error = check_space_on_volumes(
            &[(root.join("a"), half), (root.join("b"), half)],
            EXTRACTION_SLACK_BYTES,
        ).err().expect("expected insufficient-space error");
        assert!(error.contains("Not enough extraction space") && error.contains("Settings"));
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn zip_progress_is_measured_not_invented() {
        use std::sync::Mutex;
        let payload = vec![7u8; 3 * 1024 * 1024];
        let (source, cache) = crate::tests::make_zip(vec![
            ("a.bin", payload.clone()),
            ("sub/b.bin", vec![9u8; 1024]),
        ]);
        let dest = cache.join("metered");
        let reports = Mutex::new(Vec::new());
        let meter = extract_zip_tree_metered(&source, &dest, &|report| {
            reports.lock().unwrap().push(report);
        }).unwrap();
        assert_eq!(meter.total_bytes, 3 * 1024 * 1024 + 1024);
        assert_eq!(meter.done_bytes, meter.total_bytes);
        assert_eq!((meter.files_done, meter.files_total), (2, 2));
        assert!(meter.is_complete());
        let reports = reports.lock().unwrap();
        assert!(!reports.is_empty());
        assert_eq!(reports[0].phase, ExtractionPhase::Inspection);
        assert_eq!(reports.last().unwrap().phase, ExtractionPhase::Finalization);
        let mut last = 0u64;
        for report in reports.iter() {
            assert!(report.done_bytes >= last, "extraction bytes went backwards");
            last = report.done_bytes;
        }
        // Unknown totals stay indeterminate instead of inventing a bar.
        let unknown = ExtractionMeter::new(0, 3);
        assert_eq!(unknown.fraction(), None);
        assert!(!unknown.is_complete());
        let known = ExtractionMeter::new(100, 3);
        assert!(known.fraction().unwrap() <= 0.99);
        // Sub-second / sub-megabyte samples report no speed instead of spikes.
        assert_eq!(honest_extraction_speed(512, 0.01), 0.);
        assert_eq!(honest_extraction_speed(2 * 1024 * 1024, 1.0), 2.0 * 1024.0 * 1024.0);
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }
    #[test]
    fn failed_extraction_keeps_inputs_and_names_phase() {
        let (source, cache) = crate::tests::make_zip(vec![("../escape.txt", vec![1])]);
        let error = extract_content(&source, &cache, ArtifactKind::Zip, None, Arc::new(|_,_,_|{}), 0).err().expect("expected extraction failure");
        assert_eq!(error_phase(&error), "inspection");
        assert!(error.contains(&source.display().to_string()));
        assert!(source.is_file(), "failed extraction must preserve the input archive");
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }
    #[test]
    fn corrupt_archive_reports_phase() {
        let root = crate::test_output_root().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("broken.zip");
        std::fs::write(&source, b"definitely not a zip archive............").unwrap();
        let error = extract_content(&source, &root.join("cache"), ArtifactKind::Zip, None, Arc::new(|_,_,_|{}), 0).err().expect("expected extraction failure");
        assert_eq!(error_phase(&error), "inspection");
        assert!(source.is_file(), "corrupt inputs are preserved for retry");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn successful_cleanup_touches_only_consumed_files() {
        let root = crate::test_output_root().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(root.join("out")).unwrap();
        let part1 = root.join("archive.part01.rar");
        let part2 = root.join("archive.part02.rar");
        let unrelated = root.join("keep-me.pkg");
        let output = root.join("out").join("game.pkg");
        for path in [&part1, &part2, &unrelated, &output] {
            std::fs::write(path, b"x").unwrap();
        }
        let outputs = extracted_outputs(&ExtractedContent::Pkgs(vec![output.clone()]));
        remove_consumed_inputs(&[part1.clone(), part2.clone()], &outputs).unwrap();
        assert!(!part1.exists() && !part2.exists());
        assert!(unrelated.is_file() && output.is_file());
        // Cleanup is idempotent: already-removed files pass for retry.
        remove_consumed_inputs(&[part1.clone(), part2.clone()], &outputs).unwrap();
        // Extracted outputs are refused, never deleted; failures are tagged.
        let error = remove_consumed_inputs(&[output.clone()], &outputs).err().expect("expected cleanup failure");
        assert!(is_cleanup_error(&error) && error.contains("refused"));
        assert!(output.is_file());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn upload_retry_works_after_archive_deletion() {
        // Extract, persist outputs, delete only the consumed archive: the
        // extracted PKG an upload retry needs must survive.
        let (source, cache) = crate::tests::make_zip(vec![
            ("CUSA12345.pkg", vec![0x7f, 0x43, 0x4e, 0x54, 1, 2, 3]),
        ]);
        let content = extract_content(&source, &cache, ArtifactKind::Zip, None, Arc::new(|_,_,_|{}), 0).unwrap();
        let outputs = extracted_outputs(&content);
        assert_eq!(outputs.len(), 1);
        remove_consumed_inputs(&[source.clone()], &outputs).unwrap();
        assert!(!source.exists(), "consumed archive is removed after validation");
        assert_eq!(&std::fs::read(&outputs[0]).unwrap()[..4], &[0x7f, 0x43, 0x4e, 0x54]);
        let _ = std::fs::remove_dir_all(cache.parent().unwrap());
    }
}
