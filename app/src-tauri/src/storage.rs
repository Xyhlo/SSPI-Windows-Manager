use super::*;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct SpacePlan {
    pub phase: String,
    pub directory: String,
    pub free_bytes: Option<u64>,
    pub required_bytes: u64,
    pub archive_bytes: u64,
    pub extracted_bytes: u64,
    pub package_bytes: u64,
    pub temporary_bytes: u64,
    pub workspace_bytes: u64,
    pub estimated: bool,
    pub enough: bool,
    pub message: String,
    pub reserved_bytes: u64,
    pub bytes_at_check: u64,
}

fn overhead(bytes: u64) -> u64 { bytes.saturating_add(bytes / 20) }

pub(super) fn peak_bytes(archive: u64, extracted: u64, package: u64, temporary: u64, workspace: u64) -> u64 {
    archive.saturating_add(extracted).max(extracted.saturating_add(package).saturating_add(temporary).saturating_add(workspace))
}

pub(super) fn plan(root: &Path, phase: &str, archive: u64, extracted: u64, workspace: u64,
    packing: bool, occupied: u64, estimated: bool) -> SpacePlan {
    let package = if packing { overhead(extracted) } else { 0 };
    let temporary = if packing { overhead(extracted) } else { 0 };
    let workspace = if packing { workspace } else { 0 };
    let peak = peak_bytes(archive, extracted, package, temporary, workspace);
    let required = peak.saturating_sub(occupied).saturating_add(GIB.max(peak / 20));
    let free = free_space(root);
    let enough = free.is_none_or(|free| free >= required);
    let message = if archive == 0 && extracted == 0 {
        "Size is not supplied by the source. Space will be checked when download headers arrive and again before extraction and packaging.".into()
    } else {
        format!("{} space for {phase} at {}: {:.2} GiB additional required, {} free. {}{}{}",
            if enough { "Checked" } else { "Not enough" }, root.display(), required as f64 / GIB as f64,
            free.map(|v| format!("{:.2} GiB", v as f64 / GIB as f64)).unwrap_or_else(|| "unknown".into()),
            if estimated { "Estimate; actual sizes are rechecked at each stage. " } else { "" },
            if packing { "Includes the package, builder scratch space, private staging copies and a safety margin; existing inputs are counted once." }
                else { "Includes the download/extraction overlap and a safety margin. Archive retention follows Settings; packaging is checked separately." },
            if enough { "" } else { " Free space or choose another folder in Settings, then Retry. Retained files are kept." })
    };
    SpacePlan { phase: phase.into(), directory: root.display().to_string(), free_bytes: free, required_bytes: required,
        archive_bytes: archive, extracted_bytes: extracted, package_bytes: package, temporary_bytes: temporary,
        workspace_bytes: workspace, estimated, enough, message, reserved_bytes: 0, bytes_at_check: 0 }
}

pub(super) fn download_plan(settings: &Settings, bytes: u64, occupied: u64, archive: bool) -> SpacePlan {
    // Before headers are available this is a provisional 1:1 expansion estimate.
    // Budget only the current overlap. Do not multiply guessed extraction bytes
    // into package and scratch copies that do not exist during this stage.
    let extracted = if archive { bytes } else { 0 };
    plan(Path::new(&settings.download_dir), if archive { "download / extract" } else { "download" }, bytes, extracted, 0,
        false, occupied, archive)
}

fn account_other_jobs(state: &AppState, job: &str, plan: &mut SpacePlan) {
    let jobs = state.jobs.lock().unwrap();
    let volume = archives::volume_key(Path::new(&plan.directory));
    plan.reserved_bytes = jobs.values().filter(|p| p.job_id != job && matches!(p.stage.as_str(), "queued" | "unlocking" | "downloading" | "extracting" | "packaging"))
        .filter_map(|p| p.space.as_ref().filter(|s| archives::volume_key(Path::new(&s.directory)).eq_ignore_ascii_case(&volume))
            .map(|s| s.required_bytes.saturating_sub(p.bytes_done.saturating_sub(s.bytes_at_check))))
        .fold(0u64, u64::saturating_add);
    plan.free_bytes = free_space(Path::new(&plan.directory));
    plan.enough = plan.free_bytes.is_none_or(|free| free.saturating_sub(plan.reserved_bytes) >= plan.required_bytes);
    if plan.reserved_bytes > 0 {
        plan.message.push_str(&format!(" Other active jobs have {:.2} GiB reserved on this volume.", plan.reserved_bytes as f64 / GIB as f64));
        if !plan.enough {
            plan.message = plan.message.replacen("Checked space", "Not enough space", 1);
            plan.message.push_str(" Wait for those jobs to finish or free space, then Retry.");
        }
    }
}

pub(super) fn publish(app: &AppHandle, job: &str, mut plan: SpacePlan) -> Result<(), String> {
    static CHECK: Mutex<()> = Mutex::new(());
    let _check = CHECK.lock().unwrap();
    account_other_jobs(&app.state::<AppState>(), job, &mut plan);
    let previous = app.state::<AppState>().jobs.lock().unwrap().get(job).cloned().unwrap_or_default();
    plan.bytes_at_check = previous.bytes_done;
    let enough = plan.enough;
    let message = plan.message.clone();
    emit(app, Progress { space: Some(plan), message: message.clone(), ..previous });
    if enough { Ok(()) } else { Err(message) }
}

pub(super) fn guard_bytes(path: &Path, remaining: u64, phase: &str) -> Result<(), String> {
    let need = remaining.saturating_add(GIB);
    if let Some(free) = free_space(path) {
        if free < need { return Err(format!("Not enough space during {phase}: {:.2} GiB additional required, {:.2} GiB free at {}. Free space, then Retry; retained inputs are kept.", need as f64/GIB as f64, free as f64/GIB as f64, path.display())); }
    }
    Ok(())
}

pub(super) fn archive_size(path: &Path, kind: ArtifactKind, password: Option<&str>) -> Result<Option<u64>, String> {
    match kind {
        ArtifactKind::Rar => {
            let mut last = String::new();
            for password in archive_passwords(password) {
                match rar_list_size(path, password) { Ok(bytes) => return Ok(Some(bytes)), Err(error) => last = error }
            }
            Err(format!("Cannot inspect archive size: {last}"))
        },
        ArtifactKind::Zip => {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(path).map_err(redact)?).map_err(redact)?;
            let mut total = 0u64;
            for i in 0..zip.len() { total = total.saturating_add(zip.by_index(i).map_err(redact)?.size()); }
            Ok(Some(total))
        },
        _ => Ok(None),
    }
}

#[tauri::command]
pub(super) fn delivery_space(state: State<'_, AppState>, request: DeliveryRequest) -> Result<SpacePlan, String> {
    let settings = state.settings.lock().unwrap().clone();
    validate_delivery_target(&request, settings.package_dumps && settings.download_package_only, false)?;
    backport::validate_request(&request)?;
    let parts = delivery_parts(&request)?;
    let bytes = parts.iter().filter_map(|p| p.expected_size).fold(0u64, u64::saturating_add).saturating_add(request.backport.as_ref().map(|b| if b.parts.is_empty() { b.package.expected_size.unwrap_or(0) } else { b.parts.iter().filter_map(|p| p.expected_size).sum() }).unwrap_or(0));
    let archive = request.package.archive_set_id.is_some() || !request.package.archive_format_hint.as_deref().unwrap_or("").is_empty()
        || !request.package.url.to_ascii_lowercase().ends_with(".pkg");
    let mut plan = download_plan(&settings, bytes, 0, archive);
    account_other_jobs(&state, "", &mut plan);
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peak_uses_stage_overlap_instead_of_adding_deleted_archives() {
        assert_eq!(peak_bytes(100, 200, 210, 210, 10), 630);
        assert_eq!(peak_bytes(500, 200, 210, 210, 10), 700);
        assert_eq!(peak_bytes(100, 200, 0, 0, 0), 300);
        assert_eq!(peak_bytes(u64::MAX, 1, 2, 3, 4), u64::MAX);
    }
    #[test]
    fn existing_inputs_are_not_charged_twice() {
        let root = Path::new("C:/");
        let fresh = plan(root, "test", 100*GIB, 200*GIB, 10*GIB, true, 0, false);
        let retained = plan(root, "test", 100*GIB, 200*GIB, 10*GIB, true, 100*GIB, false);
        assert_eq!(fresh.required_bytes - retained.required_bytes, 100*GIB);
    }
    #[test]
    fn initial_71_gib_archive_estimates_two_copies_not_ten() {
        let settings = Settings { download_dir: "C:/".into(), package_dumps: true, ..Default::default() };
        let initial = download_plan(&settings, 71 * GIB, 0, true);
        assert_eq!(initial.archive_bytes, 71 * GIB);
        assert_eq!(initial.extracted_bytes, 71 * GIB);
        assert_eq!(initial.package_bytes, 0);
        assert_eq!(initial.temporary_bytes, 0);
        assert_eq!(initial.required_bytes, 142 * GIB + 142 * GIB / 20);
        let downloaded = download_plan(&settings, 71 * GIB, 71 * GIB, true);
        assert_eq!(initial.required_bytes - downloaded.required_bytes, 71 * GIB);
    }
    #[test]
    fn header_total_replaces_provisional_expansion_and_packaging_is_separate() {
        let exact = plan(Path::new("C:/"), "extraction", 71 * GIB, 100 * GIB, 0, false, 71 * GIB, false);
        assert_eq!(exact.required_bytes, 100 * GIB + 171 * GIB / 20);
        assert!(!exact.estimated);
        assert_eq!(exact.temporary_bytes, 0);
        let packaging = plan(Path::new("C:/"), "packaging", 0, 100 * GIB, 0, true, 100 * GIB, false);
        assert!(packaging.temporary_bytes > 0);
        assert!(packaging.required_bytes < 250 * GIB);
    }
}
