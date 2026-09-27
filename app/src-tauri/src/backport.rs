use super::*;
use std::io::{Read, Write};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct ArchiveInput {
    primary: PathBuf,
    inputs: Vec<PathBuf>,
    kind: ArtifactKind,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct CombinedState {
    base_archive: Option<ArchiveInput>,
    backport_archive: Option<ArchiveInput>,
    base: Option<PathBuf>,
    overlay: Option<PathBuf>,
    merged: Option<PathBuf>,
    cleanup: Vec<PathBuf>,
}

impl CombinedState {
    pub(super) fn retained_paths(&self) -> Vec<PathBuf> {
        let mut paths = self.cleanup.clone();
        paths.extend(self.base.iter().chain(self.overlay.iter()).chain(self.merged.iter()).cloned());
        for archive in self.base_archive.iter().chain(self.backport_archive.iter()) {
            paths.push(archive.primary.clone()); paths.extend(archive.inputs.clone());
        }
        paths
    }
}

pub(super) fn version(value: &str) -> Option<Vec<u32>> {
    let value = value.trim().trim_start_matches(['v', 'V']);
    if value.is_empty() { return None; }
    let mut numbers: Vec<u32> = value.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
    while numbers.len() > 1 && numbers.last() == Some(&0) { numbers.pop(); }
    Some(numbers)
}

fn ensure_versions(base: &str, overlay: &str) -> Result<(), String> {
    if let (Some(a), Some(b)) = (version(base), version(overlay)) {
        if a != b { return Err(format!("Backport requires version {overlay}, but the selected base is {base}. Choose a matching base.")); }
    }
    Ok(())
}

pub(super) fn validate_request(request: &DeliveryRequest) -> Result<(), String> {
    let Some(backport) = &request.backport else { return Ok(()); };
    if request.package.kind != "base" { return Err("Combined packaging needs a base game as its primary input".into()); }
    if backport.package.kind != "backport" && !backport.package.label.to_ascii_lowercase().contains("backport") {
        return Err("Choose an explicitly identified backport, not a generic update or DLC fix".into());
    }
    if request.title_id.as_deref().is_none_or(|id| !id.starts_with("PPSA") || !title_id(id)) {
        return Err("Combined packaging requires a PS5 title ID".into());
    }
    if request.package.url == backport.package.url { return Err("Base and backport cannot be the same download".into()); }
    ensure_versions(&request.package.version, &backport.package.version)?;
    delivery_parts(&overlay_request(request))?;
    Ok(())
}

fn overlay_request(request: &DeliveryRequest) -> DeliveryRequest {
    let input = request.backport.as_ref().expect("validated combined request");
    DeliveryRequest { package: input.package.clone(), title_id: request.title_id.clone(),
        title_name: request.title_name.clone(), icon: request.icon.clone(), archive_parts: input.parts.clone(), backport: None, provider: request.provider.clone(),
        target: request.target.clone(), transport: request.transport.clone() }
}

fn save(app: &AppHandle, job: &str, state: &CombinedState) -> Result<(), String> {
    job_store::checkpoint(app, job, job_store::Checkpoint::Combined(state.clone()))
}

fn restored_state(saved: Option<job_store::Checkpoint>, download_root: &Path) -> Result<CombinedState, String> {
    Ok(match saved {
        Some(job_store::Checkpoint::Combined(state)) => state,
        Some(job_store::Checkpoint::Archive { primary, inputs, .. }) => {
            let kind = artifact_kind(&read_magic_sync(&primary).ok_or("Retained base archive is unreadable")?, &primary.to_string_lossy(), "");
            if !matches!(kind, ArtifactKind::Rar | ArtifactKind::Zip) { return Err("A retained base must be a RAR or ZIP dump to add a backport".into()); }
            CombinedState { base_archive: Some(ArchiveInput { primary, inputs, kind }), ..Default::default() }
        }
        Some(job_store::Checkpoint::Extracted { paths, dump: true, inputs }) if paths.len() == 1 => {
            let root = paths[0].clone();
            if !root.is_dir() { return Err("Retained extracted base is missing".into()); }
            let cleanup = if root.starts_with(download_root.join("extracted")) { vec![root.clone()] } else { vec![] };
            let base_archive = inputs.first().map(|primary| ArchiveInput { primary: primary.clone(), inputs: inputs.clone(),
                kind: artifact_kind(&read_magic_sync(primary).unwrap_or_default(), &primary.to_string_lossy(), "") });
            CombinedState { base: Some(root), base_archive, cleanup, ..Default::default() }
        }
        Some(job_store::Checkpoint::Extracted { .. } | job_store::Checkpoint::Package { .. }) => return Err("Combining a backport requires an extracted base game folder, not a prebuilt package".into()),
        _ => CombinedState::default(),
    })
}

fn component_destination(download_root: &Path, job: &str, overlay: bool, base: Option<&Path>) -> PathBuf {
    let parent = download_root.join("extracted");
    let destination = parent.join(job).join(if overlay { "backport" } else { "base" });
    // A base-only archive may have extracted directly into the job directory.
    // Its new backport must remain a sibling, never become part of that base tree.
    if overlay && base.is_some_and(|base| destination.starts_with(base)) {
        parent.join(format!("{job}-backport"))
    } else { destination }
}

pub(super) async fn run(app: &AppHandle, settings: &Settings, http: &Client, job: &String,
    request: &DeliveryRequest, cancel: &mut watch::Receiver<bool>) -> Result<(), String> {
    validate_request(request)?;
    let mut combined = restored_state(job_store::record(app, job).and_then(|record| record.checkpoint), Path::new(&settings.download_dir))?;
    // The upgraded checkpoint is durable before any consumed base archive is removed.
    save(app, job, &combined)?;
    let overlay_request = overlay_request(request);
    for is_overlay in [false, true] {
        transfer_checkpoint(app, job, cancel).await?;
        let component = if is_overlay { &overlay_request } else { request };
        let label = if is_overlay { "backport" } else { "base game" };
        let existing = if is_overlay { &combined.overlay } else { &combined.base };
        if existing.as_ref().is_some_and(|path| path.is_dir()) {
            if let Some(retained_archive) = if is_overlay { combined.backport_archive.as_ref() } else { combined.base_archive.as_ref() } {
                if !settings.keep_archives { archives::remove_consumed_inputs(&retained_archive.inputs, &combined.cleanup)?; }
            }
            continue;
        }
        let archive = if is_overlay { &mut combined.backport_archive } else { &mut combined.base_archive };
        if archive.is_none() {
            emit(app, Progress { job_id: job.clone(), stage: "downloading".into(), message: format!("Preparing {label} download for combined package"), ..Default::default() });
            let retained = job_store::record(app, job);
            let (primary, inputs, kind) = download_delivery_inputs(app, settings, http, job, component,
                delivery_parts(component)?, retained.as_ref(), cancel, if is_overlay { 100_000 } else { 0 },
                if is_overlay { "backport" } else { "base" }).await?;
            if !matches!(kind, ArtifactKind::Rar | ArtifactKind::Zip) {
                return Err(format!("Combined packaging needs a RAR or ZIP {label} dump; a prebuilt PKG cannot be merged"));
            }
            *archive = Some(ArchiveInput { primary, inputs, kind });
            save(app, job, &combined)?;
        }
        let archive = if is_overlay { combined.backport_archive.clone() } else { combined.base_archive.clone() }.unwrap();
        let destination = component_destination(Path::new(&settings.download_dir), job, is_overlay, combined.base.as_deref());
        let download_root = PathBuf::from(&settings.download_dir);
        let output = destination.clone();
        let source = archive.primary.clone();
        let packed_inputs = archive.inputs.clone();
        let password = component.package.archive_password.clone();
        let app_event = app.clone(); let job_event = job.clone(); let worker_cancel = cancel.clone();
        emit(app, Progress { job_id: job.clone(), stage: "extracting".into(), work_paths: vec![destination.clone()], message: format!("Preparing {label} extraction"), ..Default::default() });
        let result = tokio::task::spawn_blocking(move || {
            let control = || job_store::blocking_checkpoint(&app_event, &job_event, &worker_cancel);
            let progress_app = app_event.clone(); let progress_job = job_event.clone();
            let progress = Arc::new(move |done: u64, total: u64, speed: f64| emit(&progress_app, Progress {
                job_id: progress_job.clone(), stage: "extracting".into(), bytes_done: done, bytes_total: total, speed_bps: speed,
                progress: if total > 0 { done as f64 / total as f64 } else { 0. },
                message: format!("Extracting {label}: {:.2} / {:.2} GiB", done as f64 / 1_073_741_824., total as f64 / 1_073_741_824.), ..Default::default() }));
            archives::with_extraction_slot(&output, &control, &|| emit(&app_event, Progress {
                job_id: job_event.clone(), stage: "extracting".into(), message: "Waiting for another extraction on this drive".into(), ..Default::default()
            }), || {
            if output.exists() { fpkg::cleanup_extracted(&output, &download_root)?; }
            std::fs::create_dir_all(&output).map_err(redact)?;
            control()?;
            let packed = if packed_inputs.is_empty() { std::fs::metadata(&source).map_err(redact)?.len() }
                else { packed_inputs.iter().filter_map(|path| std::fs::metadata(path).ok()).map(|metadata| metadata.len()).sum() };
            let unpacked = storage::archive_size(&source, archive.kind, password.as_deref())?;
            storage::publish(&app_event, &job_event, storage::plan(&output, "extraction", packed, unpacked.unwrap_or(packed), 0, false, packed, unpacked.is_none()))?;
            let app_control = app_event.clone(); let job_control = job_event.clone(); let control_cancel = worker_cancel.clone();
            extract_tree(&source, &output, archive.kind, password.as_deref(), progress,
                Arc::new(move || job_store::blocking_checkpoint(&app_control, &job_control, &control_cancel)), 0)?;
            discover(&output, is_overlay, &control)
            })
        }).await.map_err(redact)?;
        let found = match result {
            Ok(path) => path,
            Err(error) => { let _ = fpkg::cleanup_extracted(&destination, Path::new(&settings.download_dir));
                return Err(if error == "cancelled" { error } else { format!("extraction/{label}: {error}; archive retained for retry") }); }
        };
        combined.cleanup.push(destination);
        if is_overlay { combined.overlay = Some(found); } else { combined.base = Some(found); }
        save(app, job, &combined)?;
        // Persist the extracted root before releasing downloaded archive space.
        if !settings.keep_archives { archives::remove_consumed_inputs(&archive.inputs, &combined.cleanup)?; }
    }
    if combined.merged.as_ref().is_none_or(|path| !path.is_dir()) {
        let base = combined.base.clone().ok_or("Base dump is missing")?;
        let overlay = combined.overlay.clone().ok_or("Backport dump is missing")?;
        let destination = PathBuf::from(&settings.download_dir).join("extracted").join(Uuid::new_v4().to_string());
        let output = destination.clone(); let expected = request.title_id.clone().unwrap_or_default();
        let expected_version = if version(&request.package.version).is_some() { request.package.version.clone() } else { overlay_request.package.version.clone() };
        let app_event = app.clone(); let job_event = job.clone(); let worker_cancel = cancel.clone();
        emit(app, Progress { job_id: job.clone(), stage: "packaging".into(), message: "Checking title/version and applying backport replacements in a private workspace".into(), ..Default::default() });
        let result = tokio::task::spawn_blocking(move || merge(&base, &overlay, &output, &expected, &expected_version,
            &|| job_store::blocking_checkpoint(&app_event, &job_event, &worker_cancel),
            &|done, total, name| emit(&app_event, Progress { job_id: job_event.clone(), stage: "packaging".into(),
                bytes_done: done, bytes_total: total, message: format!("Applying backport {done}/{total}: {name}"), ..Default::default() }))).await.map_err(redact)?;
        if let Err(error) = result { let _ = fpkg::cleanup_extracted(&destination, Path::new(&settings.download_dir)); return Err(format!("packaging/backport: {error}")); }
        combined.merged = Some(destination);
        save(app, job, &combined)?;
    }
    package_and_install_dump(app, settings, combined.merged.as_ref().unwrap(), job, request.title_id.as_deref(),
        "base", &request.title_name, &request.icon, cancel, true, &combined.cleanup).await
}

fn extract_tree(source: &Path, dest: &Path, kind: ArtifactKind, password: Option<&str>,
    progress: Arc<dyn Fn(u64, u64, f64) + Send + Sync>, control: Arc<dyn Fn() -> Result<(), String> + Send + Sync>, depth: usize) -> Result<(), String> {
    if depth > 3 { return Err("Archive nesting exceeds three levels".into()); }
    std::fs::create_dir_all(dest).map_err(redact)?;
    match kind {
        ArtifactKind::Rar => extract_rar_builtin(source, dest, password, progress.clone(), control.clone())?,
        ArtifactKind::Zip => archives::extract_zip_tree(source, dest, progress.as_ref(), control.as_ref())?,
        _ => return Err("Combined dumps support RAR and ZIP archives".into()),
    }
    if !discover_candidates(dest, true, control.as_ref())?.is_empty() || !discover_candidates(dest, false, control.as_ref())?.is_empty() { return Ok(()); }
    let mut seen_archives = std::collections::HashSet::new();
    // Some providers wrap each dump in another archive. Extract every first volume,
    // keeping distinct roots so an ambiguous backport cannot be silently selected.
    for nested in nested_rar_volumes(dest) {
        control()?;
        let nested = if read_magic_sync(&nested).is_some_and(|m| m.starts_with(b"Rar!")) { unrar::Archive::new(&nested).as_first_part().filename().to_path_buf() } else { nested };
        if !seen_archives.insert(nested.clone()) { continue; }
        let kind = artifact_kind(&read_magic_sync(&nested).ok_or("Unreadable nested archive")?, &nested.to_string_lossy(), "");
        let output = dest.join(format!("unpacked_{}", Uuid::new_v4()));
        extract_tree(&nested, &output, kind, password, progress.clone(), control.clone(), depth + 1)?;
    }
    Ok(())
}

fn checked_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(redact)?;
    #[cfg(windows)] { use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 { return Err("Backport merge refuses linked/reparse paths".into()); } }
    if metadata.file_type().is_symlink() { return Err("Backport merge refuses linked paths".into()); }
    Ok(metadata)
}

fn discover_candidates(root: &Path, overlay: bool, control: &dyn Fn() -> Result<(), String>) -> Result<Vec<PathBuf>, String> {
    let mut stack = vec![(root.to_path_buf(), 0)]; let mut found = Vec::new(); let mut count = 0;
    while let Some((path, depth)) = stack.pop() {
        control()?; count += 1;
        if count > 100_000 || depth > 24 { return Err("Dump discovery limit exceeded".into()); }
        if !checked_metadata(&path)?.is_dir() { continue; }
        let candidate = if overlay { path.join("eboot.bin").is_file() || path.join("fakelib").is_dir() || path.join("fakelib2").is_dir() }
            else { is_game_dump(&path) || is_doctor_dump(&path) };
        if candidate { found.push(path); continue; }
        for entry in std::fs::read_dir(&path).map_err(redact)? {
            let p = entry.map_err(redact)?.path(); if checked_metadata(&p)?.is_dir() { stack.push((p, depth + 1)); }
        }
    }
    Ok(found)
}

fn discover(root: &Path, overlay: bool, control: &dyn Fn() -> Result<(), String>) -> Result<PathBuf, String> {
    let mut found = discover_candidates(root, overlay, control)?;
    if found.len() != 1 { return Err(format!("Found {} possible {} roots; choose an archive containing one matching dump", found.len(), if overlay { "backport" } else { "base game" })); }
    Ok(found.remove(0))
}

fn metadata(root: &Path) -> Result<Option<Value>, String> {
    let path = root.join("sce_sys/param.json");
    if !path.exists() { return Ok(None); }
    if checked_metadata(&path)?.len() > 1024 * 1024 { return Err("param.json exceeds 1 MiB".into()); }
    serde_json::from_slice(&std::fs::read(path).map_err(redact)?).map(Some).map_err(|e| format!("Invalid param.json: {e}"))
}

fn metadata_version(value: &Value) -> &str { value.get("contentVersion").and_then(Value::as_str).unwrap_or("") }

pub(super) fn merge(base: &Path, overlay: &Path, dest: &Path, expected_title: &str, expected_version: &str,
    control: &dyn Fn() -> Result<(), String>, progress: &dyn Fn(u64, u64, &str)) -> Result<(), String> {
    checked_metadata(base)?; checked_metadata(overlay)?;
    let base_meta = metadata(base)?.ok_or("Base dump lacks sce_sys/param.json")?;
    if dump_title_id(base).as_deref() != Some(expected_title) { return Err("Base dump title ID does not match the selected game".into()); }
    ensure_versions(metadata_version(&base_meta), expected_version)?;
    if let Some(id) = dump_title_id(overlay) { if id != expected_title { return Err(format!("Backport title {id} does not match base {expected_title}")); } }
    if let Some(overlay_meta) = metadata(overlay)? {
        ensure_versions(metadata_version(&base_meta), metadata_version(&overlay_meta))?;
        if let (Some(a), Some(b)) = (base_meta.get("contentId"), overlay_meta.get("contentId")) {
            if a != b { return Err("Backport content ID differs from the base dump".into()); }
        }
    }
    let mut files = Vec::new(); let mut stack = vec![overlay.to_path_buf()]; let mut names = std::collections::HashSet::new();
    while let Some(dir) = stack.pop() {
        control()?;
        for entry in std::fs::read_dir(dir).map_err(redact)? {
            let path = entry.map_err(redact)?.path(); let meta = checked_metadata(&path)?;
            let relative = path.strip_prefix(overlay).map_err(redact)?.to_path_buf();
            let key = relative.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
            if !names.insert(key) { return Err("Backport contains conflicting case-insensitive paths".into()); }
            if names.len() > 200_000 { return Err("Backport file limit exceeded".into()); }
            if meta.is_dir() { stack.push(path); } else if meta.is_file() { files.push((path, relative)); }
            else { return Err("Backport contains a non-regular file".into()); }
        }
    }
    if files.is_empty() { return Err("Backport contains no files".into()); }
    if dest.exists() { return Err("Combined workspace must be new".into()); }
    // Preserve backups for doctor. Link unchanged bulk data, and replace links
    // before writing overlays so neither extracted input is modified.
    copy_base(base, dest, control)?;
    let total = files.len() as u64;
    for (index, (source, relative)) in files.into_iter().enumerate() {
        control()?;
        let target = dest.join(&relative);
        if target.is_dir() { return Err(format!("Backport file conflicts with a base directory: {}", relative.display())); }
        std::fs::create_dir_all(target.parent().ok_or("Invalid overlay path")?).map_err(redact)?;
        if target.exists() { std::fs::remove_file(&target).map_err(redact)?; }
        copy_private(&source, &target, control)?;
        progress(index as u64 + 1, total, &relative.to_string_lossy());
    }
    Ok(())
}

fn copy_base(base: &Path, dest: &Path, control: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(redact)?;
    let mut stack = vec![(base.to_path_buf(), dest.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        for entry in std::fs::read_dir(from).map_err(redact)? {
            control()?; let path = entry.map_err(redact)?.path(); let meta = checked_metadata(&path)?;
            let target = to.join(path.file_name().ok_or("Invalid base path")?);
            if meta.is_dir() { std::fs::create_dir_all(&target).map_err(redact)?; stack.push((path, target)); }
            else if meta.is_file() && std::fs::hard_link(&path, &target).is_err() { copy_private(&path, &target, control)?; }
        }
    }
    Ok(())
}

fn copy_private(source: &Path, target: &Path, control: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    storage::guard_bytes(target.parent().unwrap(), checked_metadata(source)?.len(), "combined backport workspace")?;
    let mut input = std::fs::File::open(source).map_err(redact)?; let mut output = std::fs::File::create(target).map_err(redact)?;
    let mut buffer = vec![0; 4 * 1024 * 1024];
    loop { control()?; let n = input.read(&mut buffer).map_err(redact)?; if n == 0 { break; } output.write_all(&buffer[..n]).map_err(redact)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attached_backport_stays_outside_a_flat_extracted_base() {
        let root = fixture(); let flat_base = root.join("extracted/job");
        let output = component_destination(&root, "job", true, Some(&flat_base));
        assert!(!output.starts_with(&flat_base)); assert!(output.starts_with(root.join("extracted")));
        assert_eq!(component_destination(&root, "job", true, Some(&flat_base.join("base"))), flat_base.join("backport"));
    }
    #[test]
    fn upgrading_extracted_base_reuses_dump_and_all_archive_parts() {
        let root = fixture();
        let dump = root.join("extracted/job/base"); std::fs::create_dir_all(&dump).unwrap();
        std::fs::write(dump.join("eboot.bin"), b"retained executable").unwrap();
        let inputs = vec![root.join("base.part1.rar"), root.join("base.part2.rar")];
        for path in &inputs { std::fs::write(path, b"Rar!\x1a\x07\x01\x00").unwrap(); }
        let state = restored_state(Some(job_store::Checkpoint::Extracted { paths: vec![dump.clone()], dump: true, inputs: inputs.clone() }), &root).unwrap();
        assert_eq!(state.base.as_ref(), Some(&dump));
        assert_eq!(state.base_archive.as_ref().unwrap().inputs, inputs);
        assert_eq!(state.cleanup, vec![dump.clone()]);
        assert!(state.overlay.is_none() && state.merged.is_none());
        assert_eq!(std::fs::read(dump.join("eboot.bin")).unwrap(), b"retained executable");
        assert!(inputs.iter().all(|path| path.is_file()));
    }
    #[test]
    fn upgrading_retained_archive_preserves_downloads_for_extraction() {
        let root = fixture(); let primary = root.join("base.part1.rar");
        let inputs = vec![primary.clone(), root.join("base.part2.rar")];
        for path in &inputs { std::fs::write(path, b"Rar!\x1a\x07\x01\x00").unwrap(); }
        let state = restored_state(Some(job_store::Checkpoint::Archive { primary: primary.clone(), inputs: inputs.clone(), password: None }), &root).unwrap();
        let archive = state.base_archive.unwrap();
        assert_eq!(archive.primary, primary); assert_eq!(archive.inputs, inputs);
        assert!(matches!(archive.kind, ArtifactKind::Rar));
        assert!(state.base.is_none() && state.cleanup.is_empty());
        assert!(inputs.iter().all(|path| path.is_file()));
    }
    #[test]
    fn imported_dump_is_reused_without_becoming_a_cleanup_target() {
        let root = fixture(); let dump = root.join("base");
        let state = restored_state(Some(job_store::Checkpoint::Extracted { paths: vec![dump.clone()], dump: true, inputs: vec![] }), &root).unwrap();
        assert_eq!(state.base, Some(dump)); assert!(state.cleanup.is_empty());
        assert!(state.base_archive.is_none());
    }
    #[test]
    fn upgrading_rejects_prebuilt_package_and_missing_dump() {
        let root = fixture();
        assert!(restored_state(Some(job_store::Checkpoint::Package { path: root.join("ready.pkg"), dump: None, cleanup: false, backports_embedded: false, cleanup_extra: vec![] }), &root).is_err());
        assert!(restored_state(Some(job_store::Checkpoint::Extracted { paths: vec![root.join("missing")], dump: true, inputs: vec![] }), &root).is_err());
    }
    fn fixture() -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/work").join(format!("combined-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("base/sce_sys")).unwrap();
        std::fs::write(root.join("base/sce_sys/param.json"), r#"{"titleId":"PPSA31246","contentVersion":"01.200"}"#).unwrap();
        std::fs::write(root.join("base/eboot.bin"), b"original executable").unwrap();
        std::fs::create_dir_all(root.join("overlay/fakelib2")).unwrap();
        std::fs::write(root.join("overlay/eboot.bin"), b"patched executable").unwrap();
        std::fs::write(root.join("overlay/fakelib2/runtime.prx"), b"replacement library").unwrap(); root
    }
    #[test] fn combines_replacements_without_changing_inputs() {
        let root = fixture(); merge(&root.join("base"), &root.join("overlay"), &root.join("merged"), "PPSA31246", "1.200", &|| Ok(()), &|_,_,_|{}).unwrap();
        assert_eq!(std::fs::read(root.join("base/eboot.bin")).unwrap(), b"original executable");
        assert_eq!(std::fs::read(root.join("merged/eboot.bin")).unwrap(), b"patched executable");
        assert_eq!(std::fs::read(root.join("merged/fakelib2/runtime.prx")).unwrap(), b"replacement library");
    }
    #[test] fn rejects_wrong_title_and_version() {
        let root = fixture();
        assert!(merge(&root.join("base"), &root.join("overlay"), &root.join("merged"), "PPSA99999", "1.200", &|| Ok(()), &|_,_,_|{}).is_err());
        assert!(merge(&root.join("base"), &root.join("overlay"), &root.join("merged"), "PPSA31246", "1.100", &|| Ok(()), &|_,_,_|{}).is_err());
    }
    #[test] fn discovers_nested_overlay_and_rejects_multiple_choices() {
        let root = fixture(); assert_eq!(discover(&root.join("overlay"), true, &|| Ok(())).unwrap(), root.join("overlay"));
        assert!(discover(&root, true, &|| Ok(())).is_err());
    }
    #[test] fn merge_respects_cancellation() {
        let root = fixture(); assert_eq!(merge(&root.join("base"), &root.join("overlay"), &root.join("merged"), "PPSA31246", "1.200", &|| Err("cancelled".into()), &|_,_,_|{}).unwrap_err(), "cancelled");
    }

    fn zip_files(path: &Path, files: Vec<(String, Vec<u8>)>) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, bytes) in files {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
    }
    fn zip_tree(source: &Path, path: &Path) {
        let mut files = Vec::new(); let mut stack = vec![source.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let file = entry.unwrap().path();
                if file.is_dir() { stack.push(file); }
                else { files.push((file.strip_prefix(source).unwrap().to_string_lossy().replace('\\', "/"), std::fs::read(file).unwrap())); }
            }
        }
        zip_files(path, files);
    }
    fn unpack(archive: &Path, destination: &Path, kind: ArtifactKind, overlay: bool) -> PathBuf {
        extract_tree(archive, destination, kind, None, Arc::new(|_,_,_|{}), Arc::new(|| Ok(())), 0).unwrap();
        discover(destination, overlay, &|| Ok(())).unwrap()
    }
    fn assert_combined_archives(root: &Path, base: &Path, overlay: &Path) {
        let before = std::fs::read(base.join("eboot.bin")).unwrap();
        let patched = std::fs::read(overlay.join("eboot.bin")).unwrap();
        let metadata = std::fs::read(base.join("sce_sys/param.json")).unwrap();
        merge(base, overlay, &root.join("merged"), "PPSA31246", "01.200.000", &|| Ok(()), &|_,_,_|{}).unwrap();
        assert_eq!(std::fs::read(base.join("eboot.bin")).unwrap(), before);
        assert_eq!(std::fs::read(overlay.join("eboot.bin")).unwrap(), patched);
        assert_eq!(std::fs::read(root.join("merged/eboot.bin")).unwrap(), patched);
        assert_eq!(std::fs::read(root.join("merged/sce_sys/param.json")).unwrap(), metadata);
        assert_eq!(std::fs::read(root.join("merged/fakelib2/runtime.prx")).unwrap(), b"replacement library");
    }
    #[test] fn two_zip_archives_extract_and_merge_without_mutating_inputs() {
        let root = fixture();
        zip_tree(&root.join("base"), &root.join("base.zip"));
        zip_tree(&root.join("overlay"), &root.join("overlay.zip"));
        let base = unpack(&root.join("base.zip"), &root.join("base-extracted"), ArtifactKind::Zip, false);
        let overlay = unpack(&root.join("overlay.zip"), &root.join("overlay-extracted"), ArtifactKind::Zip, true);
        assert_combined_archives(&root, &base, &overlay);
        assert!(root.join("base.zip").is_file() && root.join("overlay.zip").is_file());
    }
    #[test] fn nested_wrapper_zip_finds_the_single_dump() {
        let root = fixture(); zip_tree(&root.join("base"), &root.join("inner.zip"));
        zip_files(&root.join("wrapper.zip"), vec![("inner.zip".into(), std::fs::read(root.join("inner.zip")).unwrap())]);
        let base = unpack(&root.join("wrapper.zip"), &root.join("extracted"), ArtifactKind::Zip, false);
        assert_eq!(std::fs::read(base.join("eboot.bin")).unwrap(), b"original executable");
    }
    #[test] fn complete_dump_archive_assets_are_not_unwrapped() {
        let root = fixture();
        std::fs::create_dir_all(root.join("base/data")).unwrap();
        std::fs::write(root.join("base/data/game.7z"), b"opaque game archive asset").unwrap();
        zip_files(&root.join("base/data/game.zip"), vec![("asset.dat".into(), b"asset bytes".to_vec())]);
        zip_tree(&root.join("base"), &root.join("base.zip"));
        let base = unpack(&root.join("base.zip"), &root.join("extracted"), ArtifactKind::Zip, false);
        assert_eq!(base, root.join("extracted"));
        assert_eq!(std::fs::read(base.join("data/game.7z")).unwrap(), b"opaque game archive asset");
        assert_eq!(std::fs::read_dir(base.join("data")).unwrap().count(), 2);
        assert!(!base.join("asset.dat").exists());
    }
    #[test] fn overlay_metadata_rejects_wrong_title_or_version() {
        let root = fixture(); std::fs::create_dir_all(root.join("overlay/sce_sys")).unwrap();
        std::fs::write(root.join("overlay/sce_sys/param.json"), r#"{"titleId":"PPSA99999","contentVersion":"01.200.000"}"#).unwrap();
        assert!(merge(&root.join("base"), &root.join("overlay"), &root.join("merged"), "PPSA31246", "01.200", &|| Ok(()), &|_,_,_|{}).unwrap_err().contains("does not match"));
        std::fs::write(root.join("overlay/sce_sys/param.json"), r#"{"titleId":"PPSA31246","contentVersion":"01.300.000"}"#).unwrap();
        assert!(merge(&root.join("base"), &root.join("overlay"), &root.join("merged"), "PPSA31246", "01.200", &|| Ok(()), &|_,_,_|{}).unwrap_err().contains("version"));
        assert!(!root.join("merged").exists());
    }
    #[test] #[ignore = "requires locally installed WinRAR command-line archiver"]
    fn two_real_rar_archives_extract_and_merge_without_mutating_inputs() {
        let exe = std::env::var_os("SSPI_RAR_EXE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files\WinRAR\Rar.exe"));
        assert!(exe.is_file(), "set SSPI_RAR_EXE to the locally installed Rar.exe");
        let root = fixture().canonicalize().unwrap();
        for part in ["base", "overlay"] {
            let result = std::process::Command::new(&exe).args(["a", "-idq", "-r"])
                .arg(root.join(format!("{part}.rar"))).arg(".").current_dir(root.join(part)).output().unwrap();
            assert!(result.status.success(), "RAR fixture creation failed: {}", String::from_utf8_lossy(&result.stderr));
        }
        let base = unpack(&root.join("base.rar"), &root.join("base-extracted"), ArtifactKind::Rar, false);
        let overlay = unpack(&root.join("overlay.rar"), &root.join("overlay-extracted"), ArtifactKind::Rar, true);
        assert_combined_archives(&root, &base, &overlay);
        println!("Two-RAR merge evidence: {}", root.display());
    }
}
