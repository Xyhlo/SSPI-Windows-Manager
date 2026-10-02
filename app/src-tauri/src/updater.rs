//! SSPI distribution updates. User data never enters the application-file transaction.
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, io::Read, path::{Path, PathBuf}, sync::{atomic::{AtomicBool, Ordering}, Mutex, OnceLock}, time::{Duration, Instant}};
use tauri::{AppHandle, Emitter, Manager, State};

const BASE: &str = "https://amptis.com/apk/sspi-updates/v1";
const HELPER: &str = include_str!("../../../build/sspi-update-helper.ps1");
const MAX_PACKAGE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 4 * 1024 * 1024 * 1024;
static STATUS: OnceLock<Mutex<Option<UpdateStatus>>> = OnceLock::new();
static OPERATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static INSTALLING: AtomicBool = AtomicBool::new(false);
const PREPARATION_LIMIT: Duration = Duration::from_secs(15 * 60);
const PREPARATION_IDLE_LIMIT: Duration = Duration::from_secs(90);

/// Read under AppState.cancel's mutex at the shared job-admission boundary.
pub(crate) fn installing() -> bool { INSTALLING.load(Ordering::Acquire) }

struct InstallAdmission { handed_off: bool }
impl InstallAdmission {
    /// The caller holds AppState.cancel while checking jobs and acquiring this guard.
    fn acquire() -> Result<Self, String> {
        INSTALLING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "An update installation is already being prepared")?;
        Ok(Self { handed_off: false })
    }
}
impl Drop for InstallAdmission {
    fn drop(&mut self) { if !self.handed_off { INSTALLING.store(false, Ordering::Release); } }
}

#[derive(Deserialize)]
struct HelperProgress { sequence: u64, message: String }
struct PreparationWatch { started: Instant, progressed: Instant, sequence: u64 }
impl PreparationWatch {
    fn new(now: Instant) -> Self { Self { started: now, progressed: now, sequence: 0 } }
    fn observe(&mut self, now: Instant, progress: &HelperProgress) -> bool {
        if progress.sequence <= self.sequence || progress.message.len() > 1024 { return false; }
        self.sequence = progress.sequence; self.progressed = now; true
    }
    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= PREPARATION_LIMIT || now.duration_since(self.progressed) >= PREPARATION_IDLE_LIMIT
    }
}
struct InstallHelper { child: std::process::Child, handed_off: bool }
impl Drop for InstallHelper {
    fn drop(&mut self) {
        if !self.handed_off { let _ = self.child.kill(); let _ = self.child.wait(); }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateFile { pub path: String, pub size: u64, pub sha256: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Package { pub url: String, pub size: u64, pub sha256: String, pub format: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32, pub product: String, pub channel: String, pub version: String,
    pub build: u64, pub published_at: String, pub notes: String, pub restart: String,
    pub package: Package, pub files: Vec<UpdateFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub stage: String, pub current_version: String, pub current_build: u64,
    pub channel: String, pub available: Option<Manifest>, pub downloaded: u64,
    pub total: u64, pub message: String, pub last_checked: u64,
    #[serde(skip)]
    stage_dir: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved { manifest: Manifest, stage_dir: PathBuf }
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity { schema: u32, product: String, version: String, build: u64, channel: String }

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app.path().app_config_dir().map_err(|e| e.to_string())?.join("updater");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    no_links(&root)?;
    Ok(root)
}
fn install_root() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())?.parent().map(Path::to_path_buf).ok_or("Application directory is unavailable".into())
}
/// Call at the beginning of run(), before Tauri starts jobs. A rollback must run
/// outside this process because Windows can keep the current executable locked.
pub fn recover_if_needed() -> Result<(), String> {
    let target = install_root()?;
    let journal = target.join(".sspi-update/transaction.json");
    if !journal.is_file() { return Ok(()); }
    no_links(&journal)?;
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&journal).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if value["state"] == "committed" { return Ok(()); }
    let helper = target.join(".sspi-update/recovery-helper.ps1"); no_links(&helper)?;
    fs::write(&helper, HELPER).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(helper)
        .arg("-RecoverRoot").arg(target).arg("-WaitPid").arg(std::process::id().to_string()).arg("-RestartRecovered");
    #[cfg(windows)] { use std::os::windows::process::CommandExt; cmd.creation_flags(0x08000000); }
    cmd.spawn().map_err(|e| format!("Could not start update recovery: {e}"))?;
    std::process::exit(0)
}
fn new_status(app: &AppHandle) -> Result<UpdateStatus, String> {
    let mut s = UpdateStatus { stage: "idle".into(), current_version: app.package_info().version.to_string(), current_build: 0,
        channel: "development".into(), available: None, downloaded: 0, total: 0, message: String::new(), last_checked: 0, stage_dir: None };
    if let Ok(bytes) = fs::read(install_root()?.join("resources/updater/build.json")) {
        let id: Identity = serde_json::from_slice(&bytes).map_err(|_| "The installed update identity is invalid")?;
        if id.schema != 1 || id.product != "windows" || !valid_channel(&id.channel) || id.version != s.current_version {
            return Err("The installed update identity does not match this application".into());
        }
        s.current_build = id.build; s.channel = id.channel;
    }
    let dir = root(app)?;
    if let Ok(bytes) = fs::read(dir.join("last-result.json")) {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if value["status"] == "failed" {
                s.stage = "error".into(); s.message = value["message"].as_str().unwrap_or("The update failed; the previous version was restored.").into();
            } else if value["status"] == "complete" {
                s.message = "Update installed. Reload updated receiver payloads on each console when needed.".into();
            }
        }
    }
    if let Ok(bytes) = fs::read(dir.join("ready.json")) {
        if let Ok(saved) = serde_json::from_slice::<Saved>(&bytes) {
            if validate_manifest(&saved.manifest, &s.channel).is_ok() && newer(&saved.manifest, &s)
                && saved.stage_dir.parent() == Some(dir.as_path()) && saved.stage_dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with("stage-"))
                && no_links(&saved.stage_dir).is_ok() && saved.stage_dir.join("files").is_dir() {
                s.total = saved.manifest.package.size; s.downloaded = s.total; s.available = Some(saved.manifest);
                s.stage_dir = Some(saved.stage_dir); s.stage = "ready".into();
                s.message = "Update downloaded and verified. Restart SSPI to install.".into();
            }
        }
    }
    Ok(s)
}
fn snapshot(app: &AppHandle) -> Result<UpdateStatus, String> {
    let mut state = STATUS.get_or_init(|| Mutex::new(None)).lock().map_err(|_| "Update state is unavailable")?;
    if state.is_none() { *state = Some(new_status(app)?); }
    Ok(state.as_ref().unwrap().clone())
}
fn publish(app: &AppHandle, status: &UpdateStatus) {
    if let Ok(mut state) = STATUS.get_or_init(|| Mutex::new(None)).lock() { *state = Some(status.clone()); }
    let _ = app.emit("sspi-update", status);
}
fn fail(app: &AppHandle, mut status: UpdateStatus, error: String) -> String {
    status.stage = "error".into(); status.message = error.clone(); publish(app, &status); error
}
fn valid_channel(s: &str) -> bool { matches!(s, "development" | "stable") }
fn hash_valid(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) }
fn version(s: &str) -> Vec<u64> { s.split('-').next().unwrap_or("").split('.').map(|v| v.parse().unwrap_or(0)).collect() }
fn newer(m: &Manifest, s: &UpdateStatus) -> bool { m.build > s.current_build && version(&m.version) >= version(&s.current_version) }

// Keep this policy aligned with the publisher and helper. No settings, jobs, payload library,
// downloads or source-store directories are replaceable, even inside a malformed manifest.
pub(crate) fn application_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 220 || path.contains('\\') || path.contains(':') || !path.is_ascii() { return false; }
    if path.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.ends_with('.') || s.ends_with(' ')
        || s.bytes().any(|c| c < 32 || b"<>\"|?*".contains(&c))
        || matches!(s.split('.').next().unwrap_or("").to_ascii_uppercase().as_str(), "CON"|"PRN"|"AUX"|"NUL"|"COM1"|"COM2"|"COM3"|"COM4"|"COM5"|"COM6"|"COM7"|"COM8"|"COM9"|"LPT1"|"LPT2"|"LPT3"|"LPT4"|"LPT5"|"LPT6"|"LPT7"|"LPT8"|"LPT9")) { return false; }
    let lower = path.to_ascii_lowercase();
    if matches!(lower.as_str(), "sspi.exe" | "sspi-core.exe" | "sspi.core.exe" | "sspi-launcher.exe" | "sspi_receiver.elf" | "sspi_receiver.elf.sha256" | "sspi_ps4_receiver.elf" | "sspi_ps4_receiver.elf.sha256" | "gamesource-windows.gssource" | "readme.md" | "license" | "third_party_notices.md") { return true; }
    if matches!(lower.as_str(), "resources/updater/build.json" | "resources/updater/sspi-update-helper.ps1") { return true; }
    if matches!(lower.as_str(), "resources/web-launcher/webkit-autoloader-host.py" | "resources/web-launcher/license") { return true; }
    if let Some(runtime_version) = lower.strip_prefix("resources/dotnet/shared/microsoft.netcore.app/").and_then(|path| path.strip_suffix("/.version")) {
        let parts: Vec<_> = runtime_version.split('.').collect();
        if parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit())) { return true; }
    }
    ["resources/fpkg/", "resources/dotnet/", "resources/ampr/", "resources/themepack/", "resources/receivers/"].iter().any(|prefix| lower.starts_with(prefix))
        && matches!(Path::new(path).extension().and_then(|v| v.to_str()).unwrap_or("").to_ascii_lowercase().as_str(), "exe"|"dll"|"json"|"txt"|"md"|"dat"|"bin"|"sprx"|"elf"|"sha256"|"config"|"xml")
}
fn trusted_url(url: &str, product: &str, channel: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else { return false; };
    parsed.scheme() == "https" && parsed.host_str() == Some("amptis.com") && parsed.port_or_known_default() == Some(443)
        && parsed.username().is_empty() && parsed.password().is_none() && parsed.query().is_none() && parsed.fragment().is_none()
        && parsed.path().starts_with(&format!("/apk/sspi-updates/v1/{product}/{channel}/"))
        && !url.contains('%') && !url.contains('\\') && !url.split('/').any(|s| s == ".." || s == ".")
}
fn validate_manifest(m: &Manifest, channel: &str) -> Result<(), String> {
    if m.schema != 1 || m.product != "windows" || m.channel != channel || !valid_channel(channel) || m.build == 0
        || m.version.is_empty() || m.version.len() > 64 || !m.version.bytes().all(|c| c.is_ascii_alphanumeric() || b".-+".contains(&c))
        || m.notes.len() > 8000 || m.restart.len() > 1000 || m.package.format != "zip"
        || !trusted_url(&m.package.url, "windows", channel) || !hash_valid(&m.package.sha256)
        || m.package.size == 0 || m.package.size > MAX_PACKAGE || m.files.is_empty() || m.files.len() > 4096 { return Err("The update manifest is invalid or is for another application".into()); }
    let mut names = HashSet::new(); let mut total = 0u64;
    for f in &m.files {
        if !application_path(&f.path) || !hash_valid(&f.sha256) || !names.insert(f.path.to_ascii_lowercase()) { return Err(format!("Unsafe or duplicate update file: {}", f.path)); }
        total = total.checked_add(f.size).ok_or("Update size overflow")?;
        if total > MAX_EXPANDED { return Err("The expanded update exceeds the size limit".into()); }
    }
    if !names.contains("sspi.exe") || !names.contains("resources/updater/build.json") { return Err("The update is missing its executable or build identity".into()); }
    Ok(())
}
fn no_links(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(ancestor) {
            #[cfg(windows)]
            { use std::os::windows::fs::MetadataExt; if meta.file_attributes() & 0x400 != 0 { return Err("Update paths cannot contain junctions or symbolic links".into()); } }
            if meta.file_type().is_symlink() { return Err("Update paths cannot contain symbolic links".into()); }
        }
    }
    Ok(())
}
fn file_hash(path: &Path) -> Result<String, String> {
    let mut input = fs::File::open(path).map_err(|e| e.to_string())?; let mut hasher = Sha256::new(); let mut buf = [0u8; 128 * 1024];
    loop { let n = input.read(&mut buf).map_err(|e| e.to_string())?; if n == 0 { break; } hasher.update(&buf[..n]); }
    Ok(format!("{:x}", hasher.finalize()))
}
fn extract(zip: &Path, target: &Path, manifest: &Manifest) -> Result<(), String> {
    fs::create_dir(target).map_err(|e| e.to_string())?; no_links(target)?;
    let mut archive = zip::ZipArchive::new(fs::File::open(zip).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if archive.len() != manifest.files.len() { return Err("The package file list does not match the manifest".into()); }
    let mut seen = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        let file = manifest.files.iter().find(|f| f.path == name).ok_or_else(|| format!("Unlisted package file: {name}"))?;
        if !application_path(&name) || !seen.insert(name.to_ascii_lowercase()) || entry.is_dir()
            || entry.unix_mode().is_some_and(|m| m & 0o170000 != 0 && m & 0o170000 != 0o100000)
            || entry.size() != file.size { return Err(format!("Invalid package entry: {name}")); }
        let dest = target.join(&name); fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?; no_links(&dest)?;
        let mut out = fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(|e| e.to_string())?;
        let count = std::io::copy(&mut (&mut entry).take(file.size + 1), &mut out).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?; drop(out);
        if count != file.size || !file_hash(&dest)?.eq_ignore_ascii_case(&file.sha256) { return Err(format!("Verification failed for {name}")); }
    }
    let id: Identity = serde_json::from_slice(&fs::read(target.join("resources/updater/build.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if id.schema != 1 || id.product != manifest.product || id.version != manifest.version || id.build != manifest.build || id.channel != manifest.channel { return Err("Package identity does not match the manifest".into()); }
    Ok(())
}

#[tauri::command]
pub fn get_update_status(app: AppHandle) -> Result<UpdateStatus, String> { snapshot(&app) }

#[tauri::command]
pub async fn check_for_updates(app: AppHandle, state: State<'_, crate::AppState>) -> Result<UpdateStatus, String> {
    let Ok(_guard) = OPERATION.try_lock() else { return snapshot(&app); };
    let mut s = snapshot(&app)?; if s.stage == "installing" { return Ok(s); }
    s.stage = "checking".into(); s.message = "Checking for updates…".into(); publish(&app, &s);
    let result = async {
        let endpoint = format!("{BASE}/windows/{}/manifest.json", s.channel);
    let response = state.http.get(&endpoint).query(&[("t", crate::now_secs())]).header("Cache-Control", "no-cache").timeout(Duration::from_secs(30)).send().await.map_err(|_| "Could not reach the update server. Try again later.")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND { return Ok(None); }
        let mut final_url = response.url().clone(); final_url.set_query(None);
        if !response.status().is_success() || !trusted_url(final_url.as_str(), "windows", &s.channel) { return Err("The update server returned an unexpected response".to_string()); }
        let mut stream = response.bytes_stream(); let mut body = Vec::new();
        while let Some(chunk) = stream.next().await { let chunk = chunk.map_err(|_| "The update manifest download was interrupted")?; if body.len() + chunk.len() > 1024 * 1024 { return Err("The update manifest is too large".into()); } body.extend_from_slice(&chunk); }
        let manifest: Manifest = serde_json::from_slice(&body).map_err(|_| "The update manifest could not be read")?;
        validate_manifest(&manifest, &s.channel)?;
        Ok::<_, String>(Some(manifest))
    }.await;
    s.last_checked = crate::now_secs();
    match result {
        Err(e) => Err(fail(&app, s, e)),
        Ok(Some(m)) if newer(&m, &s) => {
            let ready = s.stage_dir.is_some() && s.available.as_ref().is_some_and(|v| v.build == m.build && v.package.sha256 == m.package.sha256);
            if !ready { s.stage_dir = None; s.downloaded = 0; }
            s.total = m.package.size; s.available = Some(m); s.stage = if ready { "ready" } else { "available" }.into();
            s.message = if ready { "Update ready. Restart SSPI to install." } else { "A new SSPI update is available." }.into(); publish(&app, &s); Ok(s)
        },
        Ok(_) => {
            // A temporarily absent feed must not invalidate an already verified download.
            if s.stage_dir.is_some() { s.stage = "ready".into(); s.message = "Update ready. Restart SSPI to install.".into(); }
            else { s.available = None; s.stage = "current".into(); s.message = "You're up to date on this channel.".into(); }
            publish(&app, &s); Ok(s)
        }
    }
}

#[tauri::command]
pub async fn download_update(app: AppHandle, state: State<'_, crate::AppState>) -> Result<UpdateStatus, String> {
    let _guard = OPERATION.try_lock().map_err(|_| "An update operation is already running")?;
    let mut s = snapshot(&app)?; let m = s.available.clone().ok_or("Check for updates first")?;
    validate_manifest(&m, &s.channel)?;
    if !newer(&m, &s) { return Err("This update is not newer than the installed version".into()); }
    let dir = root(&app)?.join(format!("stage-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).map_err(|e| e.to_string())?; no_links(&dir)?;
    s.stage = "downloading".into(); s.downloaded = 0; s.total = m.package.size; s.stage_dir = None; s.message = "Downloading update…".into(); publish(&app, &s);
    let result = async {
        let response = state.http.get(&m.package.url).timeout(Duration::from_secs(1800)).send().await.map_err(|_| "Update download failed. Check your connection and retry.")?;
        if response.status() != reqwest::StatusCode::OK || !trusted_url(response.url().as_str(), "windows", &s.channel)
            || response.content_length().is_some_and(|n| n != m.package.size) { return Err("Unexpected update download response or length".into()); }
        let zip = dir.join("package.zip"); let mut out = tokio::fs::File::create(&zip).await.map_err(|e| e.to_string())?;
        let mut stream = response.bytes_stream(); let mut sha = Sha256::new(); let mut last = std::time::Instant::now();
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), stream.next()).await.map_err(|_| "Update download timed out")? {
            let chunk = chunk.map_err(|_| "Update download was interrupted. Retry to start a fresh download.")?;
            s.downloaded += chunk.len() as u64; if s.downloaded > m.package.size { return Err("Update download exceeds its declared length".into()); }
            sha.update(&chunk); tokio::io::AsyncWriteExt::write_all(&mut out, &chunk).await.map_err(|e| e.to_string())?;
            if last.elapsed() >= Duration::from_millis(150) { publish(&app, &s); last = std::time::Instant::now(); }
        }
        out.sync_all().await.map_err(|e| e.to_string())?; drop(out);
        if s.downloaded != m.package.size || !format!("{:x}", sha.finalize()).eq_ignore_ascii_case(&m.package.sha256) { return Err("Update checksum or length mismatch. Nothing was installed; retry the download.".into()); }
        s.stage = "verifying".into(); s.message = "Verifying and staging application files…".into(); publish(&app, &s);
        let staged = dir.join("files"); let manifest = m.clone();
        tokio::task::spawn_blocking(move || extract(&zip, &staged, &manifest)).await.map_err(|e| e.to_string())??;
        let saved = Saved { manifest: m.clone(), stage_dir: dir.clone() };
        fs::write(root(&app)?.join("ready.json"), serde_json::to_vec(&saved).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    }.await;
    if let Err(e) = result { if no_links(&dir).is_ok() { let _ = fs::remove_dir_all(&dir); } return Err(fail(&app, s, e)); }
    s.stage = "ready".into(); s.stage_dir = Some(dir); s.message = "Verified and ready. Restart SSPI to install; your settings and credentials are preserved.".into(); publish(&app, &s); Ok(s)
}

#[tauri::command]
pub async fn install_update(app: AppHandle, state: State<'_, crate::AppState>) -> Result<(), String> {
    let _guard = OPERATION.try_lock().map_err(|_| "An update operation is already running")?;
    let mut s = snapshot(&app)?;
    let mut admission = {
        let active = state.cancel.lock().map_err(|_| "Job state is unavailable")?;
        if !active.is_empty() { return Err("Finish or cancel active jobs before restarting to install the update.".into()); }
        InstallAdmission::acquire()?
    };
    let m = s.available.clone().ok_or("Download an update first")?;
    let dir = s.stage_dir.clone().ok_or("Download and verify the update before installing")?;
    validate_manifest(&m, &s.channel)?; no_links(&dir)?;
    let destination = install_root()?; no_links(&destination)?;
    if !destination.join("SSPI.exe").is_file() { return Err("Automatic updates require a packaged SSPI installation. Build and run the distribution first.".into()); }
    let helper = dir.join("sspi-update-helper.ps1"); fs::write(&helper, HELPER).map_err(|e| e.to_string())?;
    let progress_path = dir.join("helper-progress.json");
    let plan = serde_json::json!({"schema":1,"targetRoot":destination,"stageRoot":dir.join("files"),"resultPath":root(&app)?.join("last-result.json"),"readyPath":dir.join("helper-ready"),"progressPath":progress_path,"parentPid":std::process::id(),"manifest":m});
    let plan_path = dir.join("install-plan.json"); fs::write(&plan_path, serde_json::to_vec(&plan).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(dir.join("helper-ready"));
    let _ = fs::remove_file(&progress_path);
    let mut command = std::process::Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&helper).arg("-Plan").arg(&plan_path);
    #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    let child = command.spawn().map_err(|e| format!("Could not start the updater: {e}"))?;
    let mut helper = InstallHelper { child, handed_off: false };
    let mut watch = PreparationWatch::new(Instant::now());
    s.stage = "installing".into(); s.message = "Preparing the update; verifying staged application files…".into(); publish(&app, &s);
    loop {
        if dir.join("helper-ready").is_file() {
            s.stage = "installing".into(); s.message = "Restarting SSPI to install the update…".into(); publish(&app, &s);
            helper.handed_off = true; admission.handed_off = true;
            app.exit(0); return Ok(());
        }
        match helper.child.try_wait() {
            Ok(Some(_)) => return Err(fail(&app, s, "The updater could not prepare the installation. Check updater/last-result.json for details; SSPI is still running.".into())),
            Err(error) => return Err(fail(&app, s, format!("The update helper could not be monitored: {error}. Nothing was installed."))),
            Ok(None) => {}
        }
        if let Ok(bytes) = fs::read(&progress_path) {
            if bytes.len() <= 4096 {
                if let Ok(progress) = serde_json::from_slice::<HelperProgress>(&bytes) {
                    if watch.observe(Instant::now(), &progress) { s.message = progress.message; publish(&app, &s); }
                }
            }
        }
        if watch.expired(Instant::now()) {
            return Err(fail(&app, s, "Update preparation stopped making progress or reached its 15-minute limit. Nothing was installed; SSPI is still running.".into()));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn install_admission_blocks_jobs_and_resets_after_failure() {
        assert!(!installing());
        { let _admission = InstallAdmission::acquire().unwrap(); assert!(installing()); assert!(InstallAdmission::acquire().is_err()); }
        assert!(!installing());
        let mut handed_off = InstallAdmission::acquire().unwrap(); handed_off.handed_off = true; drop(handed_off);
        assert!(installing(), "job admission remains closed after shutdown handoff");
        INSTALLING.store(false, Ordering::Release);
    }
    #[test] fn preparation_accepts_slow_progress_but_is_bounded() {
        let start = Instant::now(); let mut watch = PreparationWatch::new(start);
        assert!(!watch.expired(start + Duration::from_secs(11)), "valid verification is not killed after ten seconds");
        for minute in 1..15 {
            let now = start + Duration::from_secs(minute * 60);
            assert!(!watch.expired(now));
            assert!(watch.observe(now, &HelperProgress { sequence: minute, message: "Verifying a staged file".into() }));
        }
        assert!(watch.expired(start + PREPARATION_LIMIT), "heartbeats cannot extend the absolute limit");
        let mut stalled = PreparationWatch::new(start);
        assert!(stalled.observe(start + Duration::from_secs(5), &HelperProgress { sequence: 1, message: "Starting".into() }));
        assert!(!stalled.observe(start + Duration::from_secs(80), &HelperProgress { sequence: 1, message: "Stale heartbeat".into() }));
        assert!(stalled.expired(start + Duration::from_secs(95)), "an unchanged progress file does not keep a dead helper alive");
    }
    #[test] fn only_application_files() {
        for path in ["../SSPI.exe", "/SSPI.exe", "C:/SSPI.exe", "resources/dotnet/../settings.json", "resources/fpkg/a:stream.dll", "resources/fpkg/CON.dll", "resources/fpkg/a. /b.dll", "settings.json", "credentials.json", "payloads/a.elf", "resources/fpkg/link/../../outside.exe",
            "resources/web-launcher/other.py", "resources/web-launcher/LICENSE.txt", "resources/web-launcher/nested/webkit-autoloader-host.py", "resources/web-launcher/../webkit-autoloader-host.py", "resources/fpkg/webkit-autoloader-host.py",
            "resources/dotnet/.version", "resources/dotnet/shared/Microsoft.NETCore.App/custom/.version", "resources/dotnet/shared/Microsoft.NETCore.App/9.0/.version", "resources/dotnet/shared/Microsoft.NETCore.App/9.0.20/.secrets", "resources/dotnet/shared/Microsoft.NETCore.App/9.0.20-preview.1/.version", "resources/fpkg/fpkg-cli.pdb"] { assert!(!application_path(path), "{path}"); }
        for path in ["SSPI.exe", "SSPI-core.exe", "resources/dotnet/shared/Microsoft.NETCore.App/9.0.0/System.dll", "sspi_ps4_receiver.elf", "resources/updater/build.json",
            "resources/web-launcher/webkit-autoloader-host.py", "resources/web-launcher/LICENSE", "resources/dotnet/shared/Microsoft.NETCore.App/9.0.20/.version"] { assert!(application_path(path), "{path}"); }
    }
    #[test] fn urls_are_fixed_to_distribution() {
        assert!(trusted_url(&format!("{BASE}/windows/development/123/update.zip"), "windows", "development"));
        for url in ["http://amptis.com/apk/sspi-updates/v1/windows/development/a.zip", "https://amptis.com.evil.test/apk/sspi-updates/v1/windows/development/a.zip", "https://amptis.com/apk/sspi-updates/v1/windows/development/%2e%2e/a", "https://amptis.com/apk/sspi-updates/v1/windows/stable/a.zip", "https://amptis.com/apk/sspi-updates/v1/windows/development/../a.zip"] { assert!(!trusted_url(url, "windows", "development")); }
    }
    #[test] fn development_builds_and_downgrades() {
        let mut m: Manifest = serde_json::from_value(serde_json::json!({"schema":1,"product":"windows","channel":"development","version":"2.23.0","build":12,"publishedAt":"", "notes":"", "restart":"", "package":{"url":"", "size":1,"sha256":"","format":"zip"},"files":[]})).unwrap();
        let s = UpdateStatus { stage:"idle".into(), current_version:"2.23.0".into(),current_build:11, channel:"development".into(),available:None,downloaded:0,total:0,message:String::new(),last_checked:0,stage_dir:None };
        assert!(newer(&m,&s)); m.build=11; assert!(!newer(&m,&s)); m.build=13; m.version="2.22.9".into(); assert!(!newer(&m,&s));
    }
}
