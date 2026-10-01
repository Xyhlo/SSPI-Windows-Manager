use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ComponentPart { number: usize, name: String, bytes: Option<u64>, downloaded: bool }
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Component { kind: String, label: String, version: String, bytes_total: Option<u64>, parts: Vec<ComponentPart> }

pub(super) fn request_components(request: &DeliveryRequest, files: &[DownloadedFile]) -> Vec<Component> {
    let mut inputs = vec![(&request.package, &request.archive_parts, 0)];
    if let Some(backport) = &request.backport { inputs.push((&backport.package, &backport.parts, 100_000)); }
    inputs.into_iter().map(|(package, volumes, offset)| {
        let volumes: Vec<&Package> = if volumes.is_empty() { vec![package] } else { volumes.iter().collect() };
        let parts: Vec<_> = volumes.iter().enumerate().map(|(index, part)| {
            let retained = files.iter().find(|file| file.index == offset + index);
            let measured = retained.filter(|f| f.complete).and_then(|f| std::fs::metadata(&f.path).ok()).map(|m| m.len());
            ComponentPart { number: part.archive_part_number.map(|n| n as usize).unwrap_or(index + 1),
                name: retained.filter(|f| !f.name.is_empty()).map(|f| f.name.clone()).or_else(|| part.archive_file_name.clone()).unwrap_or_else(|| format!("Archive part {}", index + 1)),
                bytes: measured.or(part.expected_size), downloaded: retained.is_some_and(|f| f.complete) }
        }).collect();
        let bytes_total = parts.iter().map(|p| p.bytes).collect::<Option<Vec<_>>>().map(|sizes| sizes.into_iter().fold(0u64, u64::saturating_add));
        Component { kind: if offset > 0 { "backport".into() } else { package.kind.clone() }, label: package.label.clone(), version: package.version.clone(), bytes_total, parts }
    }).collect()
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct DownloadedFile {
    pub index: usize,
    #[serde(default)]
    pub complete: bool,
    pub path: PathBuf,
    pub name: String,
    pub kind: ArtifactKind,
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) enum Checkpoint {
    Archive { primary: PathBuf, inputs: Vec<PathBuf>, password: Option<String> },
    Extracted { paths: Vec<PathBuf>, dump: bool, inputs: Vec<PathBuf> },
    Package { path: PathBuf, dump: Option<PathBuf>, cleanup: bool, #[serde(default)] backports_embedded: bool, #[serde(default)] cleanup_extra: Vec<PathBuf> },
    Local { paths: Vec<PathBuf> },
    Combined(backport::CombinedState),
}

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Record {
    #[serde(default)]
    pub ps4_delivery: Option<ps4_inbox::DeliveryState>,
    #[serde(default)]
    pub package_only: bool,
    pub progress: Progress,
    pub request: Option<DeliveryRequest>,
    pub checkpoint: Option<Checkpoint>,
    #[serde(default)]
    pub downloads: Vec<DownloadedFile>,
    pub download_dir: PathBuf,
    #[serde(default)]
    pub pairing_sealed: bool,
}

#[derive(Default)]
pub(super) struct Store {
    pub records: HashMap<String, Record>,
    pub directory: PathBuf,
}

impl Store {
    pub fn load(directory: PathBuf) -> Self {
        let mut store = Self { directory, ..Default::default() };
        if let Ok(files) = std::fs::read_dir(&store.directory) {
            for file in files.flatten().filter(|f| f.path().extension().is_some_and(|s| s == "json")) {
                if file.metadata().map(|m| m.len() > 4 * 1024 * 1024).unwrap_or(true) { continue; }
                let Ok(bytes) = std::fs::read(file.path()) else { continue; };
                let Ok(mut record) = serde_json::from_slice::<Record>(&bytes) else { continue; };
                if record.progress.components.is_empty() { if let Some(request) = &record.request { record.progress.components = request_components(request, &record.downloads); } }
                let receiver = record.request.as_ref().is_some_and(|r| ps4_transport(r) == "receiver");
                let ps4_receiver_active = record.progress.target == "ps4" && receiver && matches!(record.progress.stage.as_str(), "uploading" | "submitting" | "installing");
                let ps4_console_owned = record.progress.target == "ps4" && !receiver && matches!(record.progress.stage.as_str(), "handoff" | "installing");
                if ps4_receiver_active {
                    record.progress.stage = "monitoring-ended".into();
                    record.progress.paused = false;
                    record.progress.message = ps4_receiver::MONITORING_ENDED.into();
                } else if ps4_console_owned {
                    record.progress.stage = "monitoring-ended".into();
                    record.progress.paused = false;
                    record.progress.message = ps4_inbox::UNCONFIRMED.into();
                } else if !terminal_stage(&record.progress.stage) {
                    record.progress.stage = "cancelled".into();
                    record.progress.paused = false;
                    record.progress.message = "Previous session stopped. Retry to continue from retained files.".into();
                }
                record.progress.retryable = ps4_receiver_active || ps4_console_owned || record.request.is_some() || record.checkpoint.is_some();
                store.records.insert(record.progress.job_id.clone(), record);
            }
        }
        store
    }

    pub fn save(&self, id: &str) -> Result<(), String> {
        let record = self.records.get(id).ok_or("Retry record is missing")?;
        if uuid::Uuid::parse_str(id).is_err() { return Err("Invalid job identity".into()); }
        std::fs::create_dir_all(&self.directory).map_err(redact)?;
        let temporary = self.directory.join(format!("{id}.tmp"));
        let path = self.directory.join(format!("{id}.json"));
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary).map_err(redact)?;
        file.write_all(&serde_json::to_vec(record).map_err(redact)?).map_err(redact)?;
        file.sync_all().map_err(redact)?;
        drop(file);
        std::fs::rename(temporary, path).map_err(redact)
    }
}

pub(super) fn checkpoint(app: &AppHandle, job: &str, checkpoint: Checkpoint) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut store = state.retry.lock().unwrap();
    if let Some(record) = store.records.get_mut(job) { record.checkpoint = Some(checkpoint); store.save(job)?; }
    Ok(())
}

pub(super) fn downloaded(app: &AppHandle, job: &str, file: DownloadedFile) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut store = state.retry.lock().unwrap();
    if let Some(record) = store.records.get_mut(job) {
        record.downloads.retain(|existing| existing.index != file.index);
        record.downloads.push(file);
        if let Some(request) = &record.request { record.progress.components = request_components(request, &record.downloads); }
        store.save(job)?;
    }
    let progress = store.records.get(job).map(|r| r.progress.clone());
    drop(store);
    if let Some(progress) = progress { emit(app, progress); }
    Ok(())
}

pub(super) fn record(app: &AppHandle, job: &str) -> Option<Record> {
    app.state::<AppState>().retry.lock().unwrap().records.get(job).cloned()
}

fn reusable_base(record: &Record) -> bool {
    match &record.checkpoint {
        Some(Checkpoint::Archive { primary, .. }) => primary.is_file(),
        Some(Checkpoint::Extracted { paths, dump: true, .. }) => paths.len() == 1 && paths[0].is_dir(),
        Some(Checkpoint::Combined(state)) => state.retained_paths().iter().any(|path| path.exists()),
        None => record.downloads.iter().any(|file| file.path.is_file()),
        _ => false,
    }
}

fn same_backport(a: &BackportInput, b: &BackportInput) -> bool {
    a.package.url == b.package.url || (a.package.label.eq_ignore_ascii_case(&b.package.label)
        && a.package.version == b.package.version && a.package.firmware == b.package.firmware)
}

fn attach_backport(record: &mut Record, requested: &DeliveryRequest, active: bool) -> Result<bool, String> {
    let overlay = requested.backport.as_ref().ok_or("No backport selected")?;
    let mut request = record.request.clone().ok_or("This transfer has no saved base request")?;
    if let Some(existing) = &request.backport {
        if !same_backport(existing, overlay) { return Err("This transfer already has a different backport. Finish or remove it before changing the pairing.".into()); }
        return Ok(false);
    }
    if matches!(record.checkpoint, Some(Checkpoint::Package { .. } | Checkpoint::Extracted { dump: false, .. })) {
        return Err("This transfer contains a prebuilt package. Adding a backport needs the extracted base game folder.".into());
    }
    if active && (record.pairing_sealed || !matches!(record.progress.stage.as_str(), "queued" | "unlocking" | "downloading" | "extracting")) {
        return Err("Packaging has already started for this base. Cancel it first, then select the base and backport again to reuse the retained dump.".into());
    }
    request.backport = Some(overlay.clone());
    if requested.provider.is_some() { request.provider = requested.provider.clone(); }
    backport::validate_request(&request)?;
    record.progress.components = request_components(&request, &record.downloads);
    record.progress.package_label = format!("{} + Backport", request.package.label);
    record.progress.message = "Backport added. The existing base download/extraction will be reused before packaging.".into();
    record.request = Some(request);
    if !active { record.pairing_sealed = false; }
    Ok(true)
}

/// Attach at a durable boundary without cancelling an in-flight base download.
/// The bool indicates a retained inactive job that needs to be started again.
pub(super) fn reuse_for_pair(app: &AppHandle, requested: &DeliveryRequest) -> Result<Option<(String, bool)>, String> {
    if requested.backport.is_none() { return Ok(None); }
    backport::validate_request(requested)?;
    let state = app.state::<AppState>();
    if !state.settings.lock().unwrap().package_dumps { return Err("Enable FPKG packaging to combine a base and backport".into()); }
    let mut jobs = state.jobs.lock().unwrap();
    let mut store = state.retry.lock().unwrap();
    let active = state.cancel.lock().unwrap();
    let candidate = store.records.iter().filter(|(_, record)| !record.progress.removed && record.progress.stage != "removing"
        && record.request.as_ref().is_some_and(|base| delivery_target(base.target.as_deref()).ok() == delivery_target(requested.target.as_deref()).ok()
            && overlapping_delivery(base, requested)))
        .filter(|(id, record)| active.contains_key(*id) || reusable_base(record))
        .max_by_key(|(id, record)| (active.contains_key(*id), record.progress.created_at)).map(|(id, _)| id.clone());
    let Some(id) = candidate else { return Ok(None); };
    let running = active.contains_key(&id);
    let previous = store.records[&id].clone();
    let changed = attach_backport(store.records.get_mut(&id).unwrap(), requested, running)?;
    if changed {
        if let Err(error) = store.save(&id) { store.records.insert(id.clone(), previous); return Err(error); }
        let progress = store.records[&id].progress.clone();
        jobs.insert(id.clone(), progress.clone());
        drop(active); drop(store); drop(jobs);
        let _ = app.emit("delivery-progress", progress);
    }
    Ok(Some((id, !running)))
}

pub(super) fn pairing_before_packaging(app: &AppHandle, job: &str) -> Result<Option<DeliveryRequest>, String> {
    let state = app.state::<AppState>();
    let mut store = state.retry.lock().unwrap();
    let record = store.records.get_mut(job).ok_or("Transfer checkpoint is missing")?;
    if let Some(request) = record.request.as_ref().filter(|request| request.backport.is_some()) { return Ok(Some(request.clone())); }
    record.pairing_sealed = true;
    store.save(job)?;
    Ok(None)
}

pub(super) async fn release_packaged_inputs(download_dir: String, paths: Vec<PathBuf>) -> String {
    tokio::task::spawn_blocking(move || {
        let mut retained = Vec::new();
        for path in paths {
            if path.exists() {
                if let Err(error) = fpkg::cleanup_extracted(&path, Path::new(&download_dir)) { retained.push(error); }
            }
        }
        if retained.is_empty() { "Verified package retained for retry; downloaded extracted inputs released".into() }
        else { format!("Verified package retained for retry; some extracted inputs kept: {}", retained.join("; ")) }
    }).await.unwrap_or_else(|error| format!("Verified package retained; input cleanup could not finish: {error}"))
}

pub(super) async fn queue_local(app: AppHandle, state: &AppState, path: PathBuf, kind: Option<String>, title: Option<String>, package_only: bool, target: Option<String>, package: Option<bool>) -> Result<String, String> {
    if !path.exists() { return Err("Local input is missing".into()); }
    if package_only && !is_game_dump(&path) && !is_doctor_dump(&path) { return Err("Select a complete PS5 dump containing sce_sys/param.json and eboot.bin".into()); }
    let pkg_metadata = if path.is_file() && fpkg::package_magic(&read_magic_sync(&path).unwrap_or([0; 8])) {
        let metadata_path = path.clone();
        tokio::task::spawn_blocking(move || read_local_pkg_metadata(&metadata_path)).await.map_err(redact)?
    } else { None };
    let seed = local_job_seed(&path, pkg_metadata, kind, title);
    let mut request = DeliveryRequest { transport: None, target, package: Package {
        kind: seed.package_kind.clone(), label: seed.name.clone(), version: seed.version.clone(),
        expected_size: seed.local_pkg.then_some(seed.file_size),
        archive_file_name: seed.local_pkg.then(|| seed.file_name.clone()),
        ..Default::default()
    }, title_id: seed.title_id.clone(), title_name: Some(seed.name.clone()), icon: seed.icon.clone(), archive_parts: vec![], backport: None, provider: None, package_dumps: package };
    snapshot_transport(&mut request, &state.settings.lock().unwrap(), false)?;
    let target = validate_delivery_target(&request, package_only, path.is_dir())?.to_string();
    if target == "ps4" && fpkg::package_magic(&read_magic_sync(&path).unwrap_or([0; 8])) { ps4_inbox::validate_pkg(&path)?; }
    let job = Uuid::new_v4().to_string();
    let download_dir = PathBuf::from(&state.settings.lock().unwrap().download_dir);
    {
        let mut store = state.retry.lock().unwrap();
        store.records.insert(job.clone(), Record { ps4_delivery: None, package_only, pairing_sealed: false, progress: Progress {
            job_id: job.clone(), target, title: seed.name, icon: seed.icon, title_id: seed.title_id.unwrap_or_default(),
            package_kind: seed.package_kind, package_label: request.package.label.clone(), package_version: request.package.version.clone(),
            local_pkg: seed.local_pkg, stage: "failed".into(), retryable: true, ..Default::default() },
            request: Some(request.clone()), checkpoint: Some(Checkpoint::Local { paths: vec![path] }), downloads: vec![], download_dir });
        store.save(&job)?;
    }
    queue_delivery(app, state, request, Some(job)).await
}

/// Sends a finished package or image from a "package only" entry to the PS5. The entry itself
/// resumes from its verified checkpoint; older entries without one send the file as a new job.
#[tauri::command]
pub(super) async fn send_packaged(app: AppHandle, job_id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    if state.cancel.lock().unwrap().contains_key(&job_id) { return Err("This entry is already running".into()); }
    let (request, path, resumable) = {
        let mut store = state.retry.lock().unwrap();
        let record = store.records.get_mut(&job_id).ok_or("Download record not found")?;
        let checkpoint_path = match &record.checkpoint {
            Some(Checkpoint::Package { path, .. }) => Some(path.clone()),
            Some(Checkpoint::Extracted { paths, dump: false, .. }) if paths.len() == 1 => paths.first().cloned(),
            _ => None,
        };
        let output = record.progress.packaging.as_ref().map(|p| PathBuf::from(&p.output_path)).filter(|p| !p.as_os_str().is_empty());
        let path = checkpoint_path.clone().filter(|p| p.is_file()).or(output).ok_or("This entry has no finished package to send")?;
        if !path.is_file() { return Err(format!("The package is no longer at {}", path.display())); }
        let resumable = checkpoint_path.as_ref() == Some(&path) && record.request.is_some();
        if resumable {
            record.package_only = false;
            if let Some(request) = record.request.as_mut() { request.target = Some("ps5".into()); }
        }
        let request = record.request.clone();
        if resumable { store.save(&job_id)?; }
        (request, path, resumable)
    };
    match request.filter(|_| resumable) {
        Some(request) => queue_delivery(app.clone(), &state, request, Some(job_id)).await,
        None => queue_local(app.clone(), &state, path, None, None, false, Some("ps5".into()), None).await,
    }
}

pub(super) fn package_only(app: &AppHandle, job: &str) -> bool {
    record(app, job).is_some_and(|r| r.package_only)
}

pub(super) fn blocking_checkpoint(app: &AppHandle, job: &str, cancel: &watch::Receiver<bool>) -> Result<(), String> {
    loop {
        if *cancel.borrow() { return Err("cancelled".into()); }
        let paused = app.state::<AppState>().jobs.lock().unwrap().get(job).is_some_and(|p| p.paused);
        if !paused { return Ok(()); }
        std::thread::sleep(Duration::from_millis(80));
    }
}

pub(super) async fn resume_checkpoint(app: &AppHandle, settings: &Settings, job: &str, request: &DeliveryRequest,
    mut saved: Checkpoint, cancel: &mut watch::Receiver<bool>) -> Result<(), String> {
    let ps4 = validate_delivery_target(request, package_only(app, job), false)? == "ps4";
    let receiver = ps4 && ps4_transport(request) == "receiver";
    loop {
        transfer_checkpoint(app, job, cancel).await?;
        match saved {
            Checkpoint::Combined(_) => return Err("Combined retry must resume through its paired download job".into()),
            Checkpoint::Local { paths } => {
                let primary = paths.first().ok_or("Retained file list is empty")?;
                if primary.is_dir() {
                    validate_delivery_target(request, package_only(app, job), true)?;
                    let root = find_dump_root(primary).ok_or("Retained folder is not a complete game dump")?;
                    saved = Checkpoint::Extracted { paths: vec![root], dump: true, inputs: vec![] };
                } else if primary.extension().is_some_and(|x| x.eq_ignore_ascii_case("exfat")) {
                    saved = Checkpoint::Package { path: primary.clone(), dump: None, cleanup: false, backports_embedded: true, cleanup_extra: vec![] };
                } else if fpkg::package_magic(&read_magic_sync(primary).unwrap_or([0; 8])) {
                    saved = Checkpoint::Extracted { paths, dump: false, inputs: vec![] };
                } else {
                    saved = Checkpoint::Archive { primary: primary.clone(), inputs: vec![], password: request.package.archive_password.clone() };
                }
                checkpoint(app, job, saved.clone())?;
            },
            Checkpoint::Archive { primary, inputs, password } => {
                let kind = artifact_kind(&read_magic_sync(&primary).ok_or("Retained archive is missing or unreadable")?, &primary.to_string_lossy(), "");
                if ps4 && !receiver && settings.ps4_archive_mode == "ps4" {
                    if let Some(volumes) = ps4_inbox::archive_volumes(&primary, &inputs, password.as_deref(), request).await {
                        return ps4_inbox::deliver(app, settings, job, volumes, true, cancel).await;
                    }
                }
                emit(app, Progress { job_id: job.into(), stage: "extracting".into(),
                    message: "Inspecting retained archive and checking peak disk space".into(), ..Default::default() });
                let probe = primary.clone(); let password_probe = password.clone();
                let unpacked = tokio::task::spawn_blocking(move || storage::archive_size(&probe, kind, password_probe.as_deref())).await.map_err(redact)??;
                transfer_checkpoint(app, job, cancel).await?;
                let archive_bytes = if inputs.is_empty() { std::fs::metadata(&primary).map_err(redact)?.len() }
                    else { inputs.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum() };
                let occupied = inputs.iter().filter(|p| archives::volume_key(p).eq_ignore_ascii_case(&archives::volume_key(Path::new(&settings.download_dir))))
                    .filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
                let extracted = unpacked.unwrap_or(archive_bytes);
                storage::publish(app, job, storage::plan(Path::new(&settings.download_dir), "extraction",
                    archive_bytes, extracted, 0, false, occupied, unpacked.is_none()))?;
                let cache = PathBuf::from(&settings.download_dir).join("extracted").join(job);
                emit(app, Progress { job_id: job.into(), stage: "extracting".into(),
                    message: "Preparing extraction".into(), work_paths: vec![cache.clone()], ..Default::default() });
                let app_progress = app.clone(); let job_progress = job.to_string();
                let app_wait = app.clone(); let job_wait = job.to_string();
                let download_root = PathBuf::from(&settings.download_dir);
                let app_control = app.clone(); let job_control = job.to_string(); let control_cancel = cancel.clone();
                let content = tokio::task::spawn_blocking(move || {
                    let control: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::new(move || blocking_checkpoint(&app_control, &job_control, &control_cancel));
                    archives::with_extraction_slot(&cache, control.as_ref(), &|| emit(&app_wait, Progress {
                        job_id: job_wait.clone(), stage: "extracting".into(), message: "Waiting for another extraction on this drive".into(), ..Default::default()
                    }), || {
                    // An Archive checkpoint has no verified extracted output.
                    // Retry discards only this job's interrupted attempt.
                    if cache.exists() { fpkg::cleanup_extracted(&cache, &download_root)?; }
                    let result = archives::extract_content_controlled(&primary, &cache, kind, password.as_deref(), Arc::new(move |done, total, speed| {
                        emit(&app_progress, Progress { job_id: job_progress.clone(), stage: "extracting".into(),
                            progress: if total > 0 { (done as f64 / total as f64).min(0.99) } else { 0. },
                            bytes_done: done, bytes_total: total, speed_bps: speed,
                            message: format!("Extracting {:.2} / {:.2} GiB", done as f64 / 1_073_741_824., total as f64 / 1_073_741_824.),
                            ..Default::default() });
                    }), 0, control.clone());
                    if result.is_err() { let _ = std::fs::remove_dir(&cache); }
                    result
                    })
                }).await.map_err(redact)??;
                saved = Checkpoint::Extracted { paths: archives::extracted_outputs(&content), dump: matches!(content, ExtractedContent::Dump(_)), inputs };
                // The retry destination must be durable before deleting downloaded archives.
                checkpoint(app, job, saved.clone())?;
            },
            Checkpoint::Extracted { paths, dump, inputs } => {
                if ps4 {
                    validate_delivery_target(request, false, dump)?;
                    return if receiver { ps4_receiver::deliver(app, settings, job, paths, cancel).await }
                        else { ps4_inbox::deliver(app, settings, job, sort_pkgs(paths), false, cancel).await };
                }
                if paths.iter().any(|p| !p.exists()) { return Err("Retained extracted files are missing. Restore them or start a new download.".into()); }
                if let Some(paired) = pairing_before_packaging(app, job)? {
                    return backport::run(app, settings, &app.state::<AppState>().http.clone(), &job.to_string(), &paired, cancel).await;
                }
                if !settings.keep_archives { archives::remove_consumed_inputs(&inputs, &paths)?; }
                if dump {
                    let root = paths.first().ok_or("Retained dump path is missing")?;
                    let title = dump_title_id(root).or_else(|| request.title_id.clone());
                    if settings.package_dumps {
                        return package_and_install_dump(app, settings, root, job, title.as_deref(), &request.package.kind,
                            &request.title_name, &request.icon, cancel, !request.package.url.is_empty() && root.starts_with(Path::new(&settings.download_dir).join("extracted")), &[]).await;
                    }
                    return upload_dump(app, settings, root, job, title.as_deref(), &request.package.kind, cancel).await;
                }
                return upload_pkg_set(app, settings, &ReceiverEndpoint::ps5(settings), paths, job, request.title_id.as_deref(), cancel).await;
            },
            Checkpoint::Package { path, dump, cleanup, backports_embedded, cleanup_extra } => {
                if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("exfat")) {
                    if ps4 { return Err("ShadowMount exFAT images are for the PS5. Choose the PS5 as the target, then retry.".into()); }
                    if !path.is_file() {
                        if let Some(root) = dump.as_ref().filter(|root| root.is_dir()) {
                            return package_and_install_dump(app, settings, root, job, request.title_id.as_deref(),
                                &request.package.kind, &request.title_name, &request.icon, cancel, cleanup, &cleanup_extra).await;
                        }
                        return Err(format!("The retained image {} is missing and its dump was released. Start a new download to rebuild it.", path.display()));
                    }
                    if package_only(app, job) {
                        emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1.,
                            message: format!("ShadowMount image ready: {}", path.display()), ..Default::default() });
                        return Ok(());
                    }
                    let mut inputs = cleanup_extra;
                    if cleanup { inputs.extend(dump); }
                    let cleanup_note = if settings.keep_extractions { "Extracted files retained by settings.".into() } else { release_packaged_inputs(settings.download_dir.clone(), inputs).await };
                    emit(app, Progress { job_id: job.into(), stage: "uploading".into(), message: format!("Reusing the verified image. {cleanup_note}"), ..Default::default() });
                    let delivered = deliver_image(app, settings, &path, job, cancel).await?;
                    emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1., message: format!("{delivered} {cleanup_note}"), ..Default::default() });
                    return Ok(());
                }
                if ps4 {
                    validate_delivery_target(request, false, dump.is_some())?;
                    return if receiver { ps4_receiver::deliver(app, settings, job, vec![path], cancel).await }
                        else { ps4_inbox::deliver(app, settings, job, vec![path], false, cancel).await };
                }
                if !backports_embedded {
                    if let Some(root) = dump.as_ref().filter(|root| root.is_dir() && fpkg::has_backport_runtime(root)) {
                        emit(app, Progress { job_id: job.into(), stage: "packaging".into(),
                            message: "Rebuilding retained dump to include its backport runtime files inside the package".into(), ..Default::default() });
                        return package_and_install_dump(app, settings, root, job, request.title_id.as_deref(),
                            &request.package.kind, &request.title_name, &request.icon, cancel, cleanup, &cleanup_extra).await;
                    }
                }
                if dump.is_some() {
                    let engine = fpkg::locate_engine(Some(&settings.fpkg_engine_path))
                        .ok_or("The FPKG engine is required to verify this retained package before retry")?;
                    emit(app, Progress { job_id: job.into(), stage: "packaging".into(),
                        message: "Checking the retained package's backport runtime before upload".into(), ..Default::default() });
                    let result = fpkg::verify_runtime(&engine, &path, || {
                        if *cancel.borrow() { return Err("cancelled".into()); }
                        Ok(app.state::<AppState>().jobs.lock().unwrap().get(job).is_some_and(|p| p.paused))
                    }).await;
                    transfer_checkpoint(app, job, cancel).await?;
                    if let Err(error) = result {
                        if let Some(root) = dump.as_ref().filter(|root| root.is_dir()) {
                            emit(app, Progress { job_id: job.into(), stage: "packaging".into(),
                                message: format!("{error}. Rebuilding from the retained dump."), ..Default::default() });
                            return package_and_install_dump(app, settings, root, job, request.title_id.as_deref(),
                                &request.package.kind, &request.title_name, &request.icon, cancel, cleanup, &cleanup_extra).await;
                        }
                        return Err(format!("packaging/verification: {error}. The original inputs have been released. Start a new base + backport download to rebuild this package."));
                    }
                }
                let identity = fpkg::package_identity(&path)?;
                if package_only(app, job) {
                    emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1.,
                        message: format!("FPKG ready: {}", path.display()), ..Default::default() });
                    return Ok(());
                }
                let title = identity.split(|c: char| !c.is_ascii_alphanumeric()).find(|s| title_id(s));
                let mut inputs = cleanup_extra;
                if cleanup { inputs.extend(dump); }
                let cleanup_note = if settings.keep_extractions { "Extracted files retained by settings.".into() } else { release_packaged_inputs(settings.download_dir.clone(), inputs).await };
                emit(app, Progress { job_id: job.into(), stage: "uploading".into(), message: format!("Reusing verified package. {cleanup_note}"), ..Default::default() });
                upload(app, settings, &ReceiverEndpoint::ps5(settings), &path, job, title, cancel, "FPKG", false, 0, 0).await?;
                let message = format!("Package installation confirmed. {cleanup_note}");
                emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1., message, ..Default::default() });
                return Ok(());
            },
        }
    }
}

// Only inspect SSPI's download root and its own archive staging folders.
// Older releases did not persist jobs, but their retained archives are usable.
pub(super) fn recover_legacy(store: &mut Store, root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return; };
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_file()) { candidates.push(path); }
        else if entry.file_type().is_ok_and(|t| t.is_dir()) && entry.file_name().to_string_lossy().starts_with("archive_") {
            if let Ok(files) = std::fs::read_dir(path) { candidates.extend(files.flatten().filter(|f| f.file_type().is_ok_and(|t| t.is_file())).map(|f| f.path())); }
        }
    }
    for path in candidates {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let kind = artifact_kind(&read_magic_sync(&path).unwrap_or([0; 8]), &name, "");
        if !matches!(kind, ArtifactKind::Rar | ArtifactKind::Zip | ArtifactKind::Pkg) || path.extension().is_some_and(|s| s == "part") { continue; }
        if kind == ArtifactKind::Rar && rar_first_volume(&path) != path { continue; }
        let already_known = store.records.values().any(|r| r.downloads.iter().any(|f| f.path == path) || match &r.checkpoint {
            Some(Checkpoint::Archive { primary, .. }) | Some(Checkpoint::Package { path: primary, .. }) => primary == &path,
            Some(Checkpoint::Local { paths }) => paths.contains(&path),
            Some(Checkpoint::Extracted { paths, inputs, .. }) => paths.contains(&path) || inputs.contains(&path),
            Some(Checkpoint::Combined(_)) => r.downloads.iter().any(|f| f.path == path),
            None => r.downloads.iter().any(|f| f.path == path),
        });
        if already_known { continue; }
        let id = Uuid::new_v4().to_string();
        let progress = Progress { job_id: id.clone(), stage: "failed".into(), title: name.clone(),
            title_id: title_from_path(&path).unwrap_or_default(), package_kind: "base".into(),
            package_label: name.clone(), created_at: now_secs() * 1000, retryable: true,
            message: format!("Retained file from an earlier session: {}. Retry uses this file without downloading it again.", path.display()),
            ..Default::default() };
        let app_archive = name.eq_ignore_ascii_case("archive.rar") || path.parent().and_then(|p| p.file_name()).is_some_and(|n| n.to_string_lossy().starts_with("archive_"));
        let saved = if app_archive && matches!(kind, ArtifactKind::Rar | ArtifactKind::Zip) {
            let inputs = if kind == ArtifactKind::Rar {
                std::fs::read_dir(path.parent().unwrap_or(root)).ok().into_iter().flatten().flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                    .map(|e| e.path()).filter(|p| rar_first_volume(p) == path).collect()
            } else { vec![path.clone()] };
            Checkpoint::Archive { primary: path.clone(), inputs, password: None }
        } else { Checkpoint::Local { paths: vec![path.clone()] } };
        store.records.insert(id.clone(), Record { ps4_delivery: None, package_only: false, pairing_sealed: false, progress, request: None, checkpoint: Some(saved),
            downloads: vec![DownloadedFile { index: 0, path, name: String::new(), kind, complete: true }], download_dir: root.to_path_buf() });
        let _ = store.save(&id);
    }
}

#[tauri::command]
pub(super) async fn retry_job(app: AppHandle, job_id: String) -> Result<String, String> {
    let state = app.state::<AppState>();
    {
        let jobs = state.jobs.lock().unwrap();
        if jobs.get(&job_id).is_some_and(|p| !terminal_stage(&p.stage)) { return Err("This job is still active".into()); }
    }
    let saved = record(&app, &job_id).ok_or("No retained retry information is available for this job")?;
    if saved.progress.removed { return Err("This entry was removed".into()); }
    let request = saved.request.unwrap_or_else(|| DeliveryRequest {
        transport: None,
        target: if saved.progress.target.is_empty() { None } else { Some(saved.progress.target) },
        package: Package { kind: saved.progress.package_kind, label: saved.progress.package_label, version: saved.progress.package_version, ..Default::default() },
        title_id: Some(saved.progress.title_id), title_name: Some(saved.progress.title), icon: saved.progress.icon, archive_parts: vec![], backport: None, provider: None, package_dumps: None,
    });
    queue_delivery(app.clone(), &state, request, Some(job_id)).await
}

/// Built FPKGs this record produced (a local PKG import is an original, never listed here).
fn finished_packages(record: &Record) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(info) = &record.progress.packaging { if !info.output_path.is_empty() { paths.push(PathBuf::from(&info.output_path)); } }
    if let Some(Checkpoint::Package { path, .. }) = &record.checkpoint { if !paths.contains(path) { paths.push(path.clone()); } }
    paths
}

fn referenced_paths(record: &Record) -> Vec<PathBuf> {
    let mut paths: Vec<_> = record.downloads.iter().map(|f| f.path.clone()).collect();
    paths.extend(record.progress.work_paths.clone());
    if let Some(info) = &record.progress.packaging { if !info.output_path.is_empty() { paths.push(PathBuf::from(&info.output_path)); } }
    match &record.checkpoint {
        Some(Checkpoint::Archive { primary, inputs, .. }) => { paths.push(primary.clone()); paths.extend(inputs.clone()); },
        Some(Checkpoint::Extracted { paths: outputs, inputs, .. }) => { paths.extend(outputs.clone()); paths.extend(inputs.clone()); },
        Some(Checkpoint::Package { path, dump, cleanup_extra, .. }) => { paths.push(path.clone()); paths.extend(dump.clone()); paths.extend(cleanup_extra.clone()); },
        Some(Checkpoint::Local { paths: inputs }) => paths.extend(inputs.clone()),
        Some(Checkpoint::Combined(combined)) => paths.extend(combined.retained_paths()),
        None => {},
    }
    paths
}

fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)] { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 }
    #[cfg(not(windows))] { metadata.file_type().is_symlink() }
}

fn checked_path(root: &Path, path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() { return Ok(None); }
    let canonical = std::fs::canonicalize(path).map_err(redact)?;
    if canonical == root || !canonical.starts_with(root) { return Err("Cleanup path is outside this job's download folder".into()); }
    // Check the original ancestor chain as well as the resolved target: junctions must never redirect cleanup.
    let mut ancestor = Some(path);
    while let Some(current) = ancestor {
        if is_reparse(&std::fs::symlink_metadata(current).map_err(redact)?) { return Err("Cleanup refuses linked folders or files".into()); }
        if std::fs::canonicalize(current).map_err(redact)? == root { break; }
        ancestor = current.parent();
    }
    Ok(Some(canonical))
}

fn check_tree(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(redact)?;
    if is_reparse(&metadata) { return Err("Cleanup refuses a folder containing links".into()); }
    if metadata.is_dir() { for item in std::fs::read_dir(path).map_err(redact)? { check_tree(&item.map_err(redact)?.path())?; } }
    Ok(())
}

fn cleanup_files(record: &Record, protected: &[PathBuf]) -> Result<(usize, usize), String> {
    if !record.download_dir.exists() { return Ok((0, 0)); }
    let root = std::fs::canonicalize(&record.download_dir).map_err(redact)?;
    let remote = record.request.as_ref().is_some_and(|r| !r.package.url.is_empty());
    let mut owned: Vec<_> = if matches!(record.checkpoint, Some(Checkpoint::Local { .. })) { vec![] } else { record.downloads.iter().map(|f| f.path.clone()).collect() };
    owned.extend(record.progress.work_paths.clone());
    if remote { owned.extend(referenced_paths(record)); }
    else if let Some(Checkpoint::Archive { inputs, .. }) = &record.checkpoint { owned.extend(inputs.clone()); }
    if let Some(info) = &record.progress.packaging { if !info.output_path.is_empty() { owned.push(PathBuf::from(&info.output_path)); } }
    let protected: Vec<_> = protected.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect();
    let mut targets = Vec::new(); let mut retained = 0;
    for path in owned {
        let Some(mut target) = checked_path(&root, &path)? else { continue; };
        if let Ok(relative) = target.strip_prefix(&root) {
            let segments: Vec<_> = relative.components().collect();
            if segments.len() >= 2 && ["extracted", "packaged"].iter().any(|name| segments[0].as_os_str() == *name)
                && Uuid::parse_str(&segments[1].as_os_str().to_string_lossy()).is_ok() {
                target = root.join(segments[0]).join(segments[1]);
            }
        }
        if pkg_server::is_served(&target) || protected.iter().any(|p| p.starts_with(&target) || target.starts_with(p)) { retained += 1; continue; }
        if targets.iter().any(|p: &PathBuf| target.starts_with(p)) { continue; }
        targets.retain(|p: &PathBuf| !p.starts_with(&target));
        targets.push(target);
    }
    for path in &targets { checked_path(&root, path)?; check_tree(path)?; }
    let mut deleted = 0;
    for path in &targets {
        if pkg_server::is_served(path) { retained += 1; continue; }
        if path.is_dir() { std::fs::remove_dir_all(path).map_err(redact)?; } else { std::fs::remove_file(path).map_err(redact)?; }
        deleted += 1;
        if let Some(parent) = path.parent().filter(|p| *p != root && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("archive_"))) { let _ = std::fs::remove_dir(parent); }
    }
    Ok((deleted, retained))
}

#[tauri::command]
pub(super) async fn remove_job(app: AppHandle, job_id: String, delete_files: bool) -> Result<String, String> {
    let state = app.state::<AppState>();
    let keep_packages = state.settings.lock().unwrap().keep_packages_on_remove;
    let (saved, mut protected) = {
        let mut jobs = state.jobs.lock().unwrap();
        let current = jobs.get_mut(&job_id).ok_or("Download entry not found")?;
        if !terminal_stage(&current.stage) || state.cancel.lock().unwrap().contains_key(&job_id) { return Err("Stop this transfer before removing it".into()); }
        let store = state.retry.lock().unwrap();
        let saved = store.records.get(&job_id).cloned().ok_or("Download record not found")?;
        let protected: Vec<_> = store.records.iter().filter(|(id, _)| **id != job_id).flat_map(|(_, r)| referenced_paths(r)).collect();
        current.stage = "removing".into();
        (saved, protected)
    };
    // A finished package is a result, not a leftover: keep it (and any workspace still
    // holding it) unless the user turned that off in Packaging settings.
    let kept_packages: Vec<PathBuf> = if keep_packages { finished_packages(&saved).into_iter().filter(|path| path.is_file()).collect() } else { vec![] };
    protected.extend(kept_packages.iter().cloned());
    let served = delete_files && referenced_paths(&saved).iter().any(|path| pkg_server::is_served(path));
    let result = if delete_files {
        let work = saved.clone(); tokio::task::spawn_blocking(move || cleanup_files(&work, &protected)).await.map_err(redact).and_then(|r| r)
    } else { Ok((0, 0)) };
    let mut jobs = state.jobs.lock().unwrap();
    let (deleted, retained) = match result { Ok(result) => result, Err(error) => { jobs.insert(job_id.clone(), saved.progress); return Err(format!("Entry retained: {error}")); } };
    let mut store = state.retry.lock().unwrap();
    let record = store.records.get_mut(&job_id).ok_or("Download record not found")?;
    record.progress.removed = true;
    record.progress.retryable = false;
    if delete_files {
        record.downloads.clear(); record.progress.work_paths.clear(); record.progress.packaging = None;
        if !matches!(record.checkpoint, Some(Checkpoint::Local { .. })) { record.checkpoint = None; }
    }
    if let Err(error) = store.save(&job_id) { jobs.insert(job_id.clone(), saved.progress.clone()); store.records.insert(job_id.clone(), saved); return Err(error); }
    jobs.remove(&job_id);
    drop(store); drop(jobs);
    let _ = app.emit("delivery-removed", &job_id);
    Ok(if delete_files {
        let kept = if kept_packages.is_empty() { String::new() } else { format!(" Finished package kept: {}.", kept_packages.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")) };
        if served { format!("Entry removed; {deleted} downloaded/staged paths deleted. {retained} shared or PS4-served paths retained. Packages still served to the PS4 were kept; keep SSPI open while it downloads. Imported originals are kept.{kept}") }
        else { format!("Entry removed; {deleted} downloaded/staged paths deleted. {retained} shared paths retained. Imported originals are kept.{kept}") }
    } else { "Entry removed. Files kept on disk.".into() })
}

pub(super) fn overlapping_delivery(a: &DeliveryRequest, b: &DeliveryRequest) -> bool {
    let a_urls = std::iter::once(&a.package).chain(a.archive_parts.iter()).map(|part| part.url.trim()).filter(|url| !url.is_empty());
    if a_urls.into_iter().any(|url| std::iter::once(&b.package).chain(b.archive_parts.iter()).any(|part| part.url.trim() == url)) {
        return true;
    }
    a.package.kind == "base" && b.package.kind == "base"
        && a.title_id.as_ref().is_some_and(|id| !id.is_empty() && b.title_id.as_ref().is_some_and(|other| id.eq_ignore_ascii_case(other)))
        && backport::version(&a.package.version).is_some() && backport::version(&a.package.version) == backport::version(&b.package.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair_request() -> DeliveryRequest {
        DeliveryRequest { transport: None, target: None, package: Package { kind: "base".into(), version: "01.200".into(), label: "Game".into(), url: "https://base.example/archive.rar".into(), ..Default::default() },
            title_id: Some("PPSA31246".into()), title_name: None, icon: None, archive_parts: vec![], provider: None, package_dumps: None,
            backport: Some(BackportInput { package: Package { kind: "backport".into(), version: "01.200".into(), label: "Backport 4.xx".into(), url: "https://overlay.example/backport.zip".into(), ..Default::default() }, parts: vec![] }) }
    }
    #[test]
    fn attaching_backport_keeps_downloads_and_checkpoint_and_is_idempotent() {
        let (_, mut record) = cleanup_fixture(); let pair = pair_request();
        record.request = Some(DeliveryRequest { backport: None, ..pair.clone() }); record.progress.stage = "extracting".into();
        let paths = referenced_paths(&record); let checkpoint = serde_json::to_value(&record.checkpoint).unwrap();
        assert!(attach_backport(&mut record, &pair, true).unwrap());
        assert_eq!(referenced_paths(&record), paths); assert_eq!(serde_json::to_value(&record.checkpoint).unwrap(), checkpoint);
        assert_eq!(record.progress.components.len(), 2); assert!(record.request.as_ref().unwrap().backport.is_some());
        assert!(!attach_backport(&mut record, &pair, true).unwrap());
    }
    #[test]
    fn packaging_boundary_blocks_late_changes_but_cancelled_dump_can_be_paired() {
        let (_, mut record) = cleanup_fixture(); let pair = pair_request();
        record.request = Some(DeliveryRequest { backport: None, ..pair.clone() }); record.progress.stage = "extracting".into(); record.pairing_sealed = true;
        assert!(attach_backport(&mut record, &pair, true).unwrap_err().contains("Packaging has already started"));
        assert!(record.request.as_ref().unwrap().backport.is_none());
        record.progress.stage = "cancelled".into();
        assert!(attach_backport(&mut record, &pair, false).unwrap()); assert!(!record.pairing_sealed);
    }
    #[test]
    fn pairing_rejects_different_versions_and_prebuilt_packages() {
        let (root, mut record) = cleanup_fixture(); let mut pair = pair_request();
        record.request = Some(DeliveryRequest { backport: None, ..pair.clone() }); record.progress.stage = "downloading".into();
        pair.backport.as_mut().unwrap().package.version = "01.300".into(); assert!(attach_backport(&mut record, &pair, true).is_err());
        pair.backport.as_mut().unwrap().package.version = "01.200".into();
        record.checkpoint = Some(Checkpoint::Package { path: root.join("ready.pkg"), dump: None, cleanup: false, backports_embedded: false, cleanup_extra: vec![] });
        assert!(attach_backport(&mut record, &pair, true).unwrap_err().contains("prebuilt package"));
    }
    #[test]
    fn same_base_is_blocked_across_mirrors_and_combined_jobs() {
        let a = DeliveryRequest { transport: None, target: None, package: Package { kind: "base".into(), version: "01.200".into(), url: "https://one/base".into(), ..Default::default() },
            title_id: Some("PPSA31246".into()), title_name: None, icon: None, archive_parts: vec![], backport: None, provider: None, package_dumps: None };
        let mut b = a.clone(); b.package.url = "https://two/base".into();
        b.backport = Some(BackportInput { package: Package::default(), parts: vec![] });
        assert!(overlapping_delivery(&a, &b));
        b.package.version = "01.300".into(); assert!(!overlapping_delivery(&a, &b));
        b.package.version = "01.200".into(); b.package.kind = "update".into(); assert!(!overlapping_delivery(&a, &b));
        b.archive_parts.push(a.package.clone()); assert!(overlapping_delivery(&a, &b));
    }
    #[test]
    fn legacy_records_load_and_ps4_target_survives_restart() {
        let (root, mut record) = cleanup_fixture();
        let mut legacy = serde_json::to_value(&record).unwrap(); legacy.as_object_mut().unwrap().remove("ps4_delivery");
        legacy["progress"].as_object_mut().unwrap().remove("target");
        let old: Record = serde_json::from_value(legacy).unwrap(); assert!(old.ps4_delivery.is_none()); assert!(old.progress.target.is_empty());
        let mut request = pair_request(); request.target = Some("ps4".into()); request.backport = None; request.title_id = Some("CUSA12345".into());
        record.request = Some(request); record.progress.target = "ps4".into(); record.progress.stage = "delivered".into();
        let id = record.progress.job_id.clone(); let mut store = Store::load(root.join("target-journal")); store.records.insert(id.clone(), record); store.save(&id).unwrap();
        let loaded = Store::load(root.join("target-journal")); let saved = &loaded.records[&id];
        assert_eq!(saved.progress.stage, "delivered"); assert_eq!(saved.progress.target, "ps4");
        assert_eq!(saved.request.as_ref().unwrap().target.as_deref(), Some("ps4"));
    }
    #[test]
    fn restart_preserves_ps4_console_handoff_as_unconfirmed_and_retryable() {
        let (root, original) = cleanup_fixture();
        let journal = root.join("ps4-recovery"); let mut store = Store::load(journal.clone());
        let delivery: ps4_inbox::DeliveryState = serde_json::from_value(json!({
            "root": "/data/SSPI", "generation": 0, "units": [{ "index": 0, "outcome": null,
                "files": [{ "local": root.join("game.pkg"), "final_name": "game-01234567.pkg", "size": 3, "primary": true, "renamed": true }] }]
        })).unwrap();
        for target in ["ps4", "ps5", ""] {
            for stage in ["handoff", "installing", "uploading"] {
                let mut record = original.clone(); let id = Uuid::new_v4().to_string();
                record.progress.job_id = id.clone(); record.progress.target = target.into(); record.progress.stage = stage.into(); record.progress.paused = true;
                record.ps4_delivery = Some(delivery.clone());
                record.checkpoint = None; record.request = None;
                store.records.insert(id.clone(), record); store.save(&id).unwrap();
                let loaded = Store::load(journal.clone()); let saved = &loaded.records[&id];
                assert!(!saved.progress.paused);
                assert_eq!(serde_json::to_value(&saved.ps4_delivery).unwrap(), serde_json::to_value(&Some(delivery.clone())).unwrap());
                if target == "ps4" && matches!(stage, "handoff" | "installing") {
                    assert_eq!(saved.progress.stage, "monitoring-ended"); assert!(saved.progress.retryable);
                    assert_eq!(saved.progress.message, "Uploaded to the PS4 inbox, but SSPI hasn't confirmed the install. Check SSPI on the PS4.");
                } else {
                    assert_eq!(saved.progress.stage, "cancelled"); assert!(!saved.progress.retryable);
                    assert_eq!(saved.progress.message, "Previous session stopped. Retry to continue from retained files.");
                }
            }
        }
    }
    #[test]
    fn receiver_restart_preserves_transport_and_recovers_monitoring() {
        let (root, original) = cleanup_fixture();
        let journal = root.join("ps4-receiver-recovery"); let mut store = Store::load(journal.clone());
        for stage in ["uploading", "submitting", "installing"] {
            let mut record = original.clone(); let id = Uuid::new_v4().to_string();
            let mut request = pair_request(); request.target = Some("ps4".into()); request.transport = Some("receiver".into()); request.backport = None;
            record.request = Some(request); record.progress.job_id = id.clone(); record.progress.target = "ps4".into();
            record.progress.stage = stage.into(); record.progress.paused = true;
            store.records.insert(id.clone(), record); store.save(&id).unwrap();
            let loaded = Store::load(journal.clone()); let saved = &loaded.records[&id];
            assert_eq!(saved.progress.stage, "monitoring-ended"); assert_eq!(saved.progress.message, ps4_receiver::MONITORING_ENDED);
            assert!(saved.progress.retryable); assert!(!saved.progress.paused);
            assert_eq!(ps4_transport(saved.request.as_ref().unwrap()), "receiver");
        }
    }
    #[tokio::test]
    async fn releasing_extracted_dump_prunes_only_empty_wrappers() {
        let (root, _) = cleanup_fixture();
        let wrapper = root.join("extracted").join(Uuid::new_v4().to_string());
        let dump = wrapper.join("unpacked").join("Game");
        std::fs::create_dir_all(&dump).unwrap(); std::fs::write(dump.join("data.bin"), b"data").unwrap();
        let other = root.join("extracted/other-job"); std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("keep.bin"), b"keep").unwrap();
        release_packaged_inputs(root.display().to_string(), vec![dump]).await;
        assert!(!wrapper.exists()); assert!(other.join("keep.bin").is_file()); assert!(root.join("archive.rar").is_file());
    }
    #[tokio::test]
    async fn verified_package_checkpoint_survives_extracted_input_cleanup() {
        let (root, mut record) = cleanup_fixture();
        let dump = root.join("extracted").join(Uuid::new_v4().to_string()); std::fs::create_dir_all(&dump).unwrap();
        std::fs::write(dump.join("asset.bin"), b"extracted input").unwrap();
        let package = root.join("verified.pkg"); std::fs::write(&package, b"retained package fixture").unwrap();
        let original = root.join("imported-original.bin"); std::fs::write(&original, b"original").unwrap();
        let id = record.progress.job_id.clone();
        record.checkpoint = Some(Checkpoint::Package { path: package.clone(), dump: Some(dump.clone()), cleanup: true, backports_embedded: true, cleanup_extra: vec![] });
        let mut store = Store::load(root.join("journal")); store.records.insert(id.clone(), record); store.save(&id).unwrap();
        let note = release_packaged_inputs(root.display().to_string(), vec![dump.clone()]).await;
        assert!(!dump.exists()); assert!(package.exists()); assert!(original.exists()); assert!(note.contains("retained for retry"));
        let restored = Store::load(root.join("journal"));
        assert!(matches!(&restored.records[&id].checkpoint, Some(Checkpoint::Package { path, .. }) if path == &package));
    }
    fn cleanup_fixture() -> (PathBuf, Record) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/job-tests").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("archive.rar"); std::fs::write(&path, b"retained archive").unwrap();
        let record = Record { ps4_delivery: None, package_only: false, pairing_sealed: false, progress: Progress { job_id: Uuid::new_v4().to_string(), stage: "cancelled".into(), ..Default::default() }, request: None,
            checkpoint: Some(Checkpoint::Archive { primary: path.clone(), inputs: vec![path.clone()], password: None }),
            downloads: vec![DownloadedFile { index: 0, complete: true, path, name: "archive.rar".into(), kind: ArtifactKind::Rar }], download_dir: root.clone() };
        (root, record)
    }
    #[test]
    fn cleanup_preserves_shared_files_and_download_root() {
        let (root, record) = cleanup_fixture(); let archive = root.join("archive.rar");
        assert_eq!(cleanup_files(&record, &[archive.clone()]).unwrap().0, 0);
        assert!(archive.exists());
        assert_eq!(cleanup_files(&record, &[]).unwrap().0, 1);
        assert!(root.is_dir()); assert!(!archive.exists());
    }
    #[test]
    fn cleanup_keeps_ps4_served_files_and_their_parent_workspace() {
        let (root, mut record) = cleanup_fixture();
        let workspace = root.join("extracted").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&workspace).unwrap(); let path = workspace.join("game.pkg");
        let mut bytes = vec![0u8; 4096]; bytes[..4].copy_from_slice(b"\x7fCNT");
        bytes[0x40..0x64].copy_from_slice(b"UP0000-CUSA12345_00-ABCDEFGHIJKLMNOP"); bytes[0x430..0x438].copy_from_slice(&4096u64.to_be_bytes());
        let digest = Sha256::digest(&bytes[..0xfe0]); bytes[0xfe0..0x1000].copy_from_slice(&digest);
        std::fs::write(&path, bytes).unwrap(); record.progress.work_paths.push(path.clone());
        let token = Uuid::new_v4().simple().to_string();
        pkg_server::register(&token, &path, None, "127.0.0.1".parse().unwrap(), 9115).unwrap();
        assert!(pkg_server::is_served(&path)); assert!(pkg_server::is_served(&workspace));
        let (deleted, retained) = cleanup_files(&record, &[]).unwrap();
        assert_eq!(deleted, 1); assert!(retained > 0); assert!(path.is_file()); assert!(!root.join("archive.rar").exists());
        pkg_server::unregister(&token);
        assert_eq!(cleanup_files(&record, &[]).unwrap().0, 1); assert!(!workspace.exists());
    }
    #[test]
    fn cleanup_refuses_outside_paths_before_deleting_anything() {
        let (root, mut record) = cleanup_fixture();
        let outside = root.parent().unwrap().join(format!("{}.txt", Uuid::new_v4())); std::fs::write(&outside, b"original").unwrap();
        record.progress.work_paths.push(outside.clone());
        assert!(cleanup_files(&record, &[]).is_err());
        assert!(root.join("archive.rar").exists()); assert!(outside.exists());
    }
    #[test]
    fn cleanup_keeps_imported_originals_and_removes_only_own_workspace() {
        let (root, mut record) = cleanup_fixture(); let original = root.join("archive.rar");
        record.checkpoint = Some(Checkpoint::Local { paths: vec![original.clone()] });
        let workspace = root.join("packaged").join(Uuid::new_v4().to_string()); std::fs::create_dir_all(&workspace).unwrap(); std::fs::write(workspace.join("partial.pkg"), b"partial").unwrap();
        record.progress.work_paths.push(workspace.clone());
        assert_eq!(cleanup_files(&record, &[]).unwrap().0, 1);
        assert!(original.exists()); assert!(!workspace.exists());
    }
    #[test]
    fn removing_a_packaged_job_keeps_the_finished_package_when_asked() {
        // The reported case: Remove from list > Delete leftover files wiped a package-only FPKG.
        let (root, mut record) = cleanup_fixture();
        let workspace = root.join("packaged").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(workspace.join("output").join("work")).unwrap();
        let package = root.join("FPKG").join("Game [PPSA00001]").join("UP0000-PPSA00001_00-GAME000000000000-A0100-V0100.pkg");
        std::fs::create_dir_all(package.parent().unwrap()).unwrap(); std::fs::write(&package, b"finished").unwrap();
        record.progress.work_paths.push(workspace.clone());
        record.progress.packaging = Some(PackagingInfo { output_path: package.display().to_string(), ..Default::default() });
        record.checkpoint = Some(Checkpoint::Package { path: package.clone(), dump: None, cleanup: false, backports_embedded: true, cleanup_extra: vec![] });
        assert_eq!(finished_packages(&record), vec![package.clone()]);
        cleanup_files(&record, &finished_packages(&record)).unwrap();
        assert!(package.is_file()); assert!(!workspace.exists()); assert!(!root.join("archive.rar").exists());
        // With the setting off the package is the job's own output and goes too.
        cleanup_files(&record, &[]).unwrap();
        assert!(!package.exists());

        // Packages built before relocation still sit inside their workspace: keep the workspace.
        let (root, mut record) = cleanup_fixture();
        let legacy = root.join("packaged").join(Uuid::new_v4().to_string()).join("output").join("legacy.pkg");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap(); std::fs::write(&legacy, b"finished").unwrap();
        record.progress.packaging = Some(PackagingInfo { output_path: legacy.display().to_string(), ..Default::default() });
        cleanup_files(&record, &finished_packages(&record)).unwrap();
        assert!(legacy.is_file());
    }
    #[test]
    fn dismissed_entry_keeps_legacy_recovery_from_recreating_it() {
        let (root, mut record) = cleanup_fixture(); record.progress.removed = true;
        let id = record.progress.job_id.clone(); let mut store = Store::load(root.join("journal"));
        store.records.insert(id.clone(), record); store.save(&id).unwrap();
        let mut restored = Store::load(root.join("journal")); recover_legacy(&mut restored, &root);
        assert_eq!(restored.records.len(), 1); assert!(restored.records[&id].progress.removed);
    }
    #[test]
    fn combined_components_keep_backport_size_separate_from_base_parts() {
        let base = Package { kind: "base".into(), expected_size: Some(1024), ..Default::default() };
        let overlay = Package { kind: "backport".into(), expected_size: Some(98 * 1024 * 1024), ..Default::default() };
        let request = DeliveryRequest { transport: None, target: None, package: base.clone(), archive_parts: vec![base; 8], backport: Some(BackportInput { package: overlay, parts: vec![] }), title_id: None, title_name: None, icon: None, provider: None, package_dumps: None };
        let parts = request_components(&request, &[]); assert_eq!(parts.len(), 2); assert_eq!(parts[0].parts.len(), 8);
        assert_eq!(parts[0].bytes_total, Some(8192)); assert_eq!(parts[1].bytes_total, Some(98 * 1024 * 1024)); assert_eq!(parts[1].kind, "backport");
    }
    #[test]
    fn restart_retains_checkpoint_and_never_automatically_runs() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/job-tests").join(Uuid::new_v4().to_string());
        let id = Uuid::new_v4().to_string();
        let mut store = Store::load(root.clone());
        store.records.insert(id.clone(), Record { ps4_delivery: None, package_only: false, pairing_sealed: false, progress: Progress { job_id: id.clone(), stage: "extracting".into(), ..Default::default() }, request: None,
            checkpoint: Some(Checkpoint::Local { paths: vec![root.join("retained.rar")] }), downloads: vec![], download_dir: root.clone() });
        store.save(&id).unwrap();
        store.save(&id).unwrap();
        let loaded = Store::load(root.clone());
        assert_eq!(loaded.records[&id].progress.stage, "cancelled");
        assert!(loaded.records[&id].progress.retryable);
        assert!(matches!(loaded.records[&id].checkpoint, Some(Checkpoint::Local { .. })));
    }

    #[test]
    fn older_archive_is_recovered_once_without_touching_it() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/job-tests").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let archive = root.join("archive.rar");
        std::fs::write(&archive, b"Rar!\x1a\x07\x00\x00").unwrap();
        let mut store = Store::load(root.join("journal"));
        recover_legacy(&mut store, &root);
        assert_eq!(store.records.len(), 1);
        let saved = store.records.values().next().unwrap();
        assert!(saved.progress.retryable);
        assert!(matches!(&saved.checkpoint, Some(Checkpoint::Archive { primary, inputs, .. }) if primary == &archive && inputs == &vec![archive.clone()]));
        recover_legacy(&mut store, &root);
        assert_eq!(store.records.len(), 1);
        assert_eq!(std::fs::read(archive).unwrap(), b"Rar!\x1a\x07\x00\x00");
    }
}

pub(super) fn cleanup_installed_package(app: &AppHandle, job: &str, path: &Path) -> Result<(), String> {
    let Some(record) = record(app, job) else { return Ok(()); };
    if record.package_only || record.request.as_ref().is_none_or(|r| r.package.url.is_empty()) { return Ok(()); }
    let root = record.download_dir.canonicalize().map_err(redact)?;
    if let Some(path) = checked_path(&root, path)? { std::fs::remove_file(path).map_err(redact)?; }
    Ok(())
}
