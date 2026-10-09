//! SSPI distribution updates. User data never enters the application-file transaction.
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, io::{Read, Write}, path::{Path, PathBuf}, process::Stdio, sync::{atomic::{AtomicBool, Ordering}, Mutex, OnceLock}, time::{Duration, Instant}};
use tauri::{AppHandle, Emitter, Manager, State};

const BASE: &str = "https://amptis.com/apk/sspi-updates/v1";
const TRUSTED_HOSTS: &[&str] = &["amptis.com", "www.amptis.com"];
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
#[serde(rename_all = "camelCase")]
pub struct UpdateFile { pub path: String, pub size: u64, pub sha256: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Package { pub url: String, pub size: u64, pub sha256: String, pub format: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
fn stage_name(manifest: &Manifest) -> String {
    format!("stage-{}-{}", manifest.build, manifest.package.sha256.chars().take(16).collect::<String>().to_ascii_lowercase())
}
fn save_ready(root: &Path, manifest: &Manifest, stage: &Path) -> Result<(), String> {
    let destination = root.join("ready.json"); no_links(&destination)?;
    let temp = root.join(format!("ready-{}.tmp", uuid::Uuid::new_v4())); no_links(&temp)?;
    let saved = Saved { manifest: manifest.clone(), stage_dir: stage.into() };
    let bytes = serde_json::to_vec(&saved).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(&bytes)?; file.sync_all()?; drop(file);
        fs::rename(&temp, &destination)
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result.map_err(|e| format!("Could not save the prepared update at {}: {e}", destination.display()))
}
fn compact_stage(root: &Path, stage: &mut PathBuf, manifest: &Manifest) -> Result<(), String> {
    validate_manifest(manifest, &manifest.channel)?;
    if stage.parent() != Some(root) || !stage.file_name().is_some_and(|name| name.to_string_lossy().starts_with("stage-")) {
        return Err("The prepared update directory is outside the updater directory".into());
    }
    no_links(root)?; no_links(stage)?; no_links(&stage.join("files"))?;
    if !stage.join("files").is_dir() { return Err("The prepared update files are missing; download the update again".into()); }
    let compact = root.join(stage_name(manifest)); no_links(&compact)?;
    if compact == *stage { return save_ready(root, manifest, stage); }
    match fs::symlink_metadata(&compact) {
        Ok(_) => return Err(format!("Cannot shorten the update path because {} already exists; no files were replaced", compact.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not inspect the compact update directory: {error}")),
    }
    let original = stage.clone();
    fs::rename(&original, &compact).map_err(|e| format!("Could not shorten the prepared update path: {e}"))?;
    *stage = compact.clone();
    if let Err(error) = save_ready(root, manifest, &compact) {
        return match fs::rename(&compact, &original) {
            Ok(()) => { *stage = original; Err(format!("{error}. The original update directory was restored.")) },
            Err(rollback) => Err(format!("{error}. Could not restore the original path: {rollback}. Prepared files remain at {}.", compact.display())),
        };
    }
    Ok(())
}
fn restore_saved_stage(root: &Path, saved: &mut Saved) -> Result<bool, String> {
    if validate_manifest(&saved.manifest, &saved.manifest.channel).is_err()
        || saved.stage_dir.parent() != Some(root)
        || !saved.stage_dir.file_name().is_some_and(|name| name.to_string_lossy().starts_with("stage-")) {
        return Ok(false);
    }
    no_links(root)?; no_links(&saved.stage_dir)?; no_links(&saved.stage_dir.join("files"))?;
    match fs::symlink_metadata(&saved.stage_dir) {
        Ok(_) => return Ok(saved.stage_dir.join("files").is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not inspect the prepared update directory: {error}")),
    }
    // A crash can leave the rename committed while ready.json still names the old directory.
    let compact = root.join(stage_name(&saved.manifest));
    no_links(&compact)?; no_links(&compact.join("files"))?;
    if compact == saved.stage_dir || !compact.join("files").is_dir() { return Ok(false); }
    // Persist before pruning. An error must preserve the surviving stage for a later retry.
    save_ready(root, &saved.manifest, &compact)?;
    saved.stage_dir = compact;
    Ok(true)
}
fn diagnostic_text(path: &Path, limit: usize) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path).ok()?.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    let truncated = bytes.len() > limit; bytes.truncate(limit);
    let mut text: String = String::from_utf8_lossy(&bytes).chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
    if truncated { text.push_str(" [truncated]"); }
    Some(text)
}
fn helper_exit_error(result_path: &Path, stderr_path: &Path, exit: &str) -> String {
    if let Some(result) = diagnostic_text(result_path, 16 * 1024).and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()) {
        if result["status"] == "failed" {
            if let Some(message) = result["message"].as_str().filter(|message| !message.trim().is_empty() && message.len() <= 4096) {
                let message: String = message.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
                return format!("The updater could not prepare the installation: {message} SSPI is still running. Diagnostic result: {}", result_path.display());
            }
        }
    }
    let detail = diagnostic_text(stderr_path, 4096).filter(|text| !text.trim().is_empty()).unwrap_or_else(|| "No error output was recorded.".into());
    format!("The updater exited before preparing the installation ({exit}). {} SSPI is still running. Diagnostic output: {}", detail.trim(), stderr_path.display())
}
fn remove_previous_file(path: &Path) -> Result<(), String> {
    no_links(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not clear previous update state at {}: {error}", path.display())),
    }
}
fn prune_stages(root: &Path, keep: Option<&Path>) -> Result<(), String> {
    no_links(root)?;
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with("stage-") || keep == Some(path.as_path()) { continue; }
        no_links(&path)?;
        if path.is_dir() { fs::remove_dir_all(&path).map_err(|e| e.to_string())?; }
    }
    if keep.is_none() && root.join("ready.json").exists() {
        no_links(&root.join("ready.json"))?;
        fs::remove_file(root.join("ready.json")).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn initialize(app: &AppHandle) -> Result<(), String> { snapshot(app).map(|_| ()) }
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
    crate::session_log::write("update-recovery", "Restoring an interrupted update; SSPI restarts when it finishes");
    crate::session_log::flush();
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
        if let Ok(mut saved) = serde_json::from_slice::<Saved>(&bytes) {
            if validate_manifest(&saved.manifest, &s.channel).is_ok() && newer(&saved.manifest, &s)
                && restore_saved_stage(&dir, &mut saved)? {
                s.total = saved.manifest.package.size; s.downloaded = s.total; s.available = Some(saved.manifest);
                s.stage_dir = Some(saved.stage_dir); s.stage = "ready".into();
                s.message = "Update downloaded and verified. Restart SSPI to install.".into();
            }
        }
    }
    prune_stages(&dir, s.stage_dir.as_deref())?;
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
    crate::session_log::write("updater", &error);
    status.stage = "error".into(); status.message = error.clone(); publish(app, &status); error
}
fn valid_channel(s: &str) -> bool { matches!(s, "development" | "stable") }
fn hash_valid(s: &str) -> bool { s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) }
// Numeric fields, so 2.25.10 follows 2.25.9; pre-release and build suffixes are ignored.
fn version(s: &str) -> Vec<u64> { s.split(['-', '+']).next().unwrap_or("").split('.').map(|v| v.parse().unwrap_or(0)).collect() }
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
    parsed.scheme() == "https" && parsed.host_str().is_some_and(|host| TRUSTED_HOSTS.contains(&host)) && parsed.port_or_known_default() == Some(443)
        && parsed.username().is_empty() && parsed.password().is_none() && parsed.query().is_none() && parsed.fragment().is_none()
        && parsed.path().starts_with(&format!("/apk/sspi-updates/v1/{product}/{channel}/"))
        && !url.contains('%') && !url.contains('\\') && !url.split('/').any(|s| s == ".." || s == ".")
}
fn update_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().https_only(true).user_agent("GameSearch/0.1").connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() >= 4 || url.scheme() != "https" || url.port_or_known_default() != Some(443)
                || !url.host_str().is_some_and(|host| TRUSTED_HOSTS.contains(&host))
                || !url.username().is_empty() || url.password().is_some() {
                attempt.error("Untrusted update redirect")
            } else { attempt.follow() }
        })).build().map_err(|e| e.to_string())
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

async fn fetch_manifest(http: &reqwest::Client, s: &UpdateStatus) -> Result<Option<Manifest>, String> {
        let endpoint = format!("{BASE}/windows/{}/manifest.json", s.channel);
    let response = http.get(&endpoint).query(&[("t", crate::now_secs())]).header("Cache-Control", "no-cache").timeout(Duration::from_secs(30)).send().await.map_err(|_| "Could not reach the update server. Try again later.")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND { return Ok(None); }
        let mut final_url = response.url().clone(); final_url.set_query(None);
        if !response.status().is_success() || !trusted_url(final_url.as_str(), "windows", &s.channel) { return Err("The update server returned an unexpected response".to_string()); }
        let mut stream = response.bytes_stream(); let mut body = Vec::new();
        while let Some(chunk) = stream.next().await { let chunk = chunk.map_err(|_| "The update manifest download was interrupted")?; if body.len() + chunk.len() > 1024 * 1024 { return Err("The update manifest is too large".into()); } body.extend_from_slice(&chunk); }
        let manifest: Manifest = serde_json::from_slice(&body).map_err(|_| "The update manifest could not be read")?;
        validate_manifest(&manifest, &s.channel)?;
        Ok::<_, String>(Some(manifest))
}

#[tauri::command]
pub fn get_update_status(app: AppHandle) -> Result<UpdateStatus, String> { snapshot(&app) }

#[tauri::command]
pub async fn check_for_updates(app: AppHandle, _state: State<'_, crate::AppState>) -> Result<UpdateStatus, String> {
    let Ok(_guard) = OPERATION.try_lock() else { return snapshot(&app); };
    let mut s = snapshot(&app)?; if s.stage == "installing" { return Ok(s); }
    s.stage = "checking".into(); s.message = "Checking for updates…".into(); publish(&app, &s);
    let result = fetch_manifest(&update_client()?, &s).await;
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
            s.stage_dir = None; s.available = None; s.downloaded = 0; s.total = 0;
            s.stage = "current".into(); s.message = "You're up to date on this channel.".into();
            prune_stages(&root(&app)?, None)?;
            publish(&app, &s); Ok(s)
        }
    }
}

#[tauri::command]
pub async fn download_update(app: AppHandle, _state: State<'_, crate::AppState>) -> Result<UpdateStatus, String> {
    let _guard = OPERATION.try_lock().map_err(|_| "An update operation is already running")?;
    let mut s = snapshot(&app)?; let m = s.available.clone().ok_or("Check for updates first")?;
    validate_manifest(&m, &s.channel)?;
    if !newer(&m, &s) { return Err("This update is not newer than the installed version".into()); }
    let dir = root(&app)?.join(stage_name(&m));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?; no_links(&dir)?;
    s.stage = "downloading".into(); s.downloaded = 0; s.total = m.package.size; s.stage_dir = None; s.message = "Downloading update…".into(); publish(&app, &s);
    let result = async {
        let zip = dir.join("package.zip"); no_links(&zip)?;
        let mut retries = 0u32;
        loop {
            let offset = fs::metadata(&zip).map(|m| m.len()).unwrap_or(0);
            if offset > m.package.size { fs::remove_file(&zip).map_err(|e| e.to_string())?; continue; }
            if offset == m.package.size { s.downloaded = offset; break; }
            let attempt = async {
                let mut request = update_client()?.get(&m.package.url).header("Accept-Encoding", "identity");
                if offset > 0 { request = request.header("Range", format!("bytes={offset}-")); }
                let response = tokio::time::timeout(Duration::from_secs(60), request.send()).await
                    .map_err(|_| "Update connection timed out")?.map_err(|_| "Update download connection failed")?;
                if !trusted_url(response.url().as_str(), "windows", &s.channel) { return Err("Untrusted update download redirect".to_string()); }
                let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT && offset > 0;
                let start = if resumed { offset } else { 0 };
                if resumed {
                    let expected = format!("bytes {}-{}/{}", offset, m.package.size - 1, m.package.size);
                    if response.headers().get("content-range").and_then(|h| h.to_str().ok()) != Some(expected.as_str()) {
                        return Err("Invalid update resume range".into());
                    }
                } else if response.status() != reqwest::StatusCode::OK { return Err("Unexpected update download response".into()); }
                if response.content_length().is_some_and(|n| n != m.package.size - start) { return Err("Unexpected update download length".into()); }
                let mut out = tokio::fs::OpenOptions::new().write(true).create(true).append(resumed).truncate(!resumed)
                    .open(&zip).await.map_err(|e| e.to_string())?;
                s.downloaded = start;
                let mut stream = response.bytes_stream(); let mut last = Instant::now();
                while let Some(chunk) = tokio::time::timeout(Duration::from_secs(60), stream.next()).await.map_err(|_| "Update download idle timeout")? {
                    let chunk = chunk.map_err(|_| "Update download was interrupted")?;
                    if chunk.len() as u64 > m.package.size - s.downloaded { return Err("Update download exceeds its declared length".into()); }
                    tokio::io::AsyncWriteExt::write_all(&mut out, &chunk).await.map_err(|e| e.to_string())?;
                    s.downloaded += chunk.len() as u64;
                    if last.elapsed() >= Duration::from_millis(150) { publish(&app, &s); last = Instant::now(); }
                }
                out.sync_all().await.map_err(|e| e.to_string())?;
                if s.downloaded != m.package.size { return Err("Update download ended early".into()); }
                Ok::<_, String>(())
            }.await;
            match attempt {
                Ok(()) => break,
                Err(error) if retries >= 3 => return Err(format!("{error}. Retry to resume the download.")),
                Err(_) => { retries += 1; tokio::time::sleep(Duration::from_secs(1 << retries)).await; }
            }
        }
        let hash_path = zip.clone();
        let hash = tokio::task::spawn_blocking(move || file_hash(&hash_path)).await.map_err(|e| e.to_string())??;
        if !hash.eq_ignore_ascii_case(&m.package.sha256) {
            fs::remove_file(&zip).map_err(|e| e.to_string())?;
            return Err("Update checksum mismatch. Nothing was installed; retry the download.".into());
        }
        if dir.join("files").exists() { no_links(&dir.join("files"))?; fs::remove_dir_all(dir.join("files")).map_err(|e| e.to_string())?; }
        s.stage = "verifying".into(); s.message = "Verifying and staging application files…".into(); publish(&app, &s);
        let staged = dir.join("files"); let manifest = m.clone();
        tokio::task::spawn_blocking(move || extract(&zip, &staged, &manifest)).await.map_err(|e| e.to_string())??;
        save_ready(&root(&app)?, &m, &dir)?;
        Ok::<_, String>(())
    }.await;
    if let Err(e) = result { return Err(fail(&app, s, e)); }
    s.stage = "ready".into(); s.stage_dir = Some(dir); s.message = "Verified and ready. Restart SSPI to install; your settings and credentials are preserved.".into(); publish(&app, &s);
    if let Err(error) = prune_stages(&root(&app)?, s.stage_dir.as_deref()) { eprintln!("Update staging cleanup: {error}"); }
    Ok(s)
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
    let latest = fetch_manifest(&update_client()?, &s).await.map_err(|e| fail(&app, s.clone(), e))?;
    if !latest.as_ref().is_some_and(|latest| newer(latest, &s)
        && serde_json::to_value(latest).ok() == serde_json::to_value(&m).ok()) {
        s.stage_dir = None; s.available = latest.filter(|latest| newer(latest, &s));
        s.stage = if s.available.is_some() { "available" } else { "current" }.into();
        s.downloaded = 0; s.message = "The staged update is no longer offered. Check and download the current update.".into();
        prune_stages(&root(&app)?, None)?; publish(&app, &s); return Err(s.message);
    }
    let destination = install_root()?; no_links(&destination)?;
    if !destination.join("SSPI.exe").is_file() { return Err("Automatic updates require a packaged SSPI installation. Build and run the distribution first.".into()); }
    let updater_root = root(&app).map_err(|error| fail(&app, s.clone(), error))?;
    let mut dir = dir;
    let compacted = compact_stage(&updater_root, &mut dir, &m);
    s.stage_dir = Some(dir.clone());
    compacted.map_err(|error| fail(&app, s.clone(), error))?;
    publish(&app, &s);
    let helper_path = dir.join("sspi-update-helper.ps1");
    let progress_path = dir.join("helper-progress.json");
    let result_path = updater_root.join("last-result.json");
    let stderr_path = dir.join("helper-stderr.log");
    let stdout_path = dir.join("helper-stdout.log");
    let plan_path = dir.join("install-plan.json");
    let plan = serde_json::json!({"schema":1,"targetRoot":destination,"stageRoot":dir.join("files"),"resultPath":result_path,"readyPath":dir.join("helper-ready"),"progressPath":progress_path,"parentPid":std::process::id(),"manifest":m});
    let prepared = (|| -> Result<std::process::Child, String> {
        for path in [&helper_path, &plan_path, &stderr_path, &stdout_path] { no_links(path)?; }
        fs::write(&helper_path, HELPER).map_err(|e| format!("Could not write the update helper: {e}"))?;
        fs::write(&plan_path, serde_json::to_vec(&plan).map_err(|e| e.to_string())?).map_err(|e| format!("Could not write the update installation plan: {e}"))?;
        for path in [&result_path, &dir.join("helper-ready"), &progress_path] { remove_previous_file(path)?; }
        let stdout = fs::File::create(&stdout_path).map_err(|e| format!("Could not open updater output log: {e}"))?;
        let stderr = fs::File::create(&stderr_path).map_err(|e| format!("Could not open updater error log: {e}"))?;
        let mut command = std::process::Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&helper_path).arg("-Plan").arg(&plan_path)
            .stdin(Stdio::null()).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr));
        #[cfg(windows)] { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
        command.spawn().map_err(|e| format!("Could not start the updater: {e}. SSPI is still running. Diagnostic output: {}", stderr_path.display()))
    })();
    let child = prepared.map_err(|error| fail(&app, s.clone(), error))?;
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
            Ok(Some(exit)) => return Err(fail(&app, s, helper_exit_error(&result_path, &stderr_path, &exit.to_string()))),
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
    fn stage_test_manifest() -> Manifest {
        Manifest { schema: 1, product: "windows".into(), channel: "development".into(), version: "2.25.7".into(), build: 20261006200540,
            published_at: String::new(), notes: String::new(), restart: String::new(),
            package: Package { url: format!("{BASE}/windows/development/20261006200540/update.zip"), size: 2, sha256: "c4".repeat(32), format: "zip".into() },
            files: ["SSPI.exe", "resources/updater/build.json"].into_iter().map(|path| UpdateFile { path: path.into(), size: 1, sha256: "a1".repeat(32) }).collect() }
    }
    fn stage_test_root() -> PathBuf {
        let root = crate::test_output_root().join(format!("update-stage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap(); root
    }
    #[test] fn compact_stage_paths_fit_windows_powershell_with_long_usernames() {
        let manifest = stage_test_manifest();
        // A 41-character profile folder: the usual one holding a 32-character user name.
        let profile = format!("C:\\{}", "u".repeat(38));
        let root = format!("{profile}\\AppData\\Roaming\\com.simpleps5installer.gamesearch\\updater");
        let relative = "resources\\dotnet\\shared\\Microsoft.NETCore.App\\9.0.20\\System.Runtime.InteropServices.RuntimeInformation.dll";
        let legacy = format!("{root}\\stage-{}-{}\\files\\{relative}", manifest.build, manifest.package.sha256);
        let compact = format!("{root}\\{}\\files\\{relative}", stage_name(&manifest));
        assert!(legacy.len() >= 260);
        assert!(compact.len() < 260, "{compact}");
        assert_eq!(stage_name(&manifest), "stage-20261006200540-c4c4c4c4c4c4c4c4");
        assert_eq!(manifest.package.sha256.len(), 64, "directory shortening must not shorten verification hashes");
    }
    #[test] fn legacy_stage_migration_preserves_files_manifest_and_ready_state() {
        let root = stage_test_root(); let manifest = stage_test_manifest();
        let legacy = root.join(format!("stage-{}-{}", manifest.build, manifest.package.sha256));
        fs::create_dir_all(legacy.join("files")).unwrap();
        fs::write(legacy.join("files/SSPI.exe"), b"unchanged staged bytes").unwrap();
        save_ready(&root, &manifest, &legacy).unwrap();
        let mut stage = legacy.clone(); compact_stage(&root, &mut stage, &manifest).unwrap();
        assert_eq!(stage, root.join(stage_name(&manifest))); assert!(!legacy.exists());
        assert_eq!(fs::read(stage.join("files/SSPI.exe")).unwrap(), b"unchanged staged bytes");
        let saved: Saved = serde_json::from_slice(&fs::read(root.join("ready.json")).unwrap()).unwrap();
        assert_eq!(saved.stage_dir, stage);
        assert_eq!(serde_json::to_value(&saved.manifest).unwrap(), serde_json::to_value(&manifest).unwrap());
        compact_stage(&root, &mut stage, &manifest).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test] fn stage_migration_refuses_escape_and_existing_destination() {
        let root = stage_test_root(); let manifest = stage_test_manifest();
        let mut outside = root.join("outside/stage-legacy");
        fs::create_dir_all(outside.join("files")).unwrap();
        assert!(compact_stage(&root, &mut outside, &manifest).unwrap_err().contains("outside"));
        let legacy = root.join("stage-legacy"); fs::create_dir_all(legacy.join("files")).unwrap();
        save_ready(&root, &manifest, &legacy).unwrap();
        let ready = fs::read(root.join("ready.json")).unwrap();
        let compact = root.join(stage_name(&manifest)); fs::create_dir_all(&compact).unwrap();
        fs::write(compact.join("keep"), b"keep").unwrap();
        let mut stage = legacy.clone();
        assert!(compact_stage(&root, &mut stage, &manifest).unwrap_err().contains("already exists"));
        assert_eq!(stage, legacy); assert!(legacy.join("files").is_dir());
        assert_eq!(fs::read(compact.join("keep")).unwrap(), b"keep");
        assert_eq!(fs::read(root.join("ready.json")).unwrap(), ready);
        fs::remove_dir_all(root).unwrap();
    }
    #[test] fn stage_migration_rolls_back_when_ready_state_cannot_be_persisted() {
        let root = stage_test_root(); let manifest = stage_test_manifest();
        let legacy = root.join("stage-legacy"); fs::create_dir_all(legacy.join("files")).unwrap();
        fs::write(legacy.join("files/SSPI.exe"), b"unchanged").unwrap();
        fs::create_dir(root.join("ready.json")).unwrap();
        let mut stage = legacy.clone();
        assert!(compact_stage(&root, &mut stage, &manifest).unwrap_err().contains("original update directory was restored"));
        assert_eq!(stage, legacy); assert!(!root.join(stage_name(&manifest)).exists());
        assert_eq!(fs::read(legacy.join("files/SSPI.exe")).unwrap(), b"unchanged");
        assert!(root.join("ready.json").is_dir());
        assert!(!fs::read_dir(&root).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
        fs::remove_dir_all(root).unwrap();
    }
    #[test] fn interrupted_stage_migration_recovers_before_pruning() {
        let root = stage_test_root(); let manifest = stage_test_manifest();
        let legacy = root.join(format!("stage-{}-{}", manifest.build, manifest.package.sha256));
        fs::create_dir_all(legacy.join("files")).unwrap();
        fs::write(legacy.join("files/SSPI.exe"), b"surviving verified bytes").unwrap();
        save_ready(&root, &manifest, &legacy).unwrap();
        let compact = root.join(stage_name(&manifest)); fs::rename(&legacy, &compact).unwrap();
        let mut saved: Saved = serde_json::from_slice(&fs::read(root.join("ready.json")).unwrap()).unwrap();
        assert_eq!(saved.stage_dir, legacy);
        assert!(restore_saved_stage(&root, &mut saved).unwrap());
        assert_eq!(saved.stage_dir, compact);
        let persisted: Saved = serde_json::from_slice(&fs::read(root.join("ready.json")).unwrap()).unwrap();
        assert_eq!(persisted.stage_dir, compact);
        assert_eq!(serde_json::to_value(&persisted.manifest).unwrap(), serde_json::to_value(&manifest).unwrap());
        prune_stages(&root, Some(&saved.stage_dir)).unwrap();
        assert_eq!(fs::read(compact.join("files/SSPI.exe")).unwrap(), b"surviving verified bytes");
        fs::remove_dir_all(root).unwrap();
    }
    #[test] fn stage_recovery_requires_missing_contained_legacy_and_valid_manifest() {
        let root = stage_test_root(); let manifest = stage_test_manifest();
        let legacy = root.join("stage-legacy"); fs::create_dir_all(&legacy).unwrap();
        let compact = root.join(stage_name(&manifest)); fs::create_dir_all(compact.join("files")).unwrap();
        let mut saved = Saved { manifest: manifest.clone(), stage_dir: legacy.clone() };
        assert!(!restore_saved_stage(&root, &mut saved).unwrap(), "an existing legacy directory must not adopt another stage");
        assert_eq!(saved.stage_dir, legacy);
        fs::remove_dir(&legacy).unwrap(); saved.manifest.product = "another-product".into();
        assert!(!restore_saved_stage(&root, &mut saved).unwrap());
        saved.manifest = manifest; saved.stage_dir = root.join("outside/stage-legacy");
        assert!(!restore_saved_stage(&root, &mut saved).unwrap());
        assert!(!root.join("ready.json").exists()); assert!(compact.join("files").is_dir());
        fs::remove_dir_all(root).unwrap();
    }
    #[test] fn stage_recovery_persistence_failure_preserves_surviving_directory() {
        let root = stage_test_root(); let manifest = stage_test_manifest();
        let compact = root.join(stage_name(&manifest)); fs::create_dir_all(compact.join("files")).unwrap();
        let mut saved = Saved { manifest, stage_dir: root.join("stage-legacy") };
        fs::create_dir(root.join("ready.json")).unwrap();
        assert!(restore_saved_stage(&root, &mut saved).is_err(), "startup must stop before pruning when persistence fails");
        assert!(compact.join("files").is_dir()); assert_eq!(saved.stage_dir, root.join("stage-legacy"));
        fs::remove_dir(root.join("ready.json")).unwrap();
        assert!(restore_saved_stage(&root, &mut saved).unwrap());
        assert_eq!(saved.stage_dir, compact);
        fs::remove_dir_all(root).unwrap();
    }
    #[test] fn helper_failure_reports_current_result_or_bounded_stderr_and_exit() {
        let root = stage_test_root(); let result = root.join("last-result.json"); let stderr = root.join("helper-stderr.log");
        fs::write(&stderr, "PowerShell parser failure\0".repeat(400)).unwrap();
        let error = helper_exit_error(&result, &stderr, "exit code: 1");
        assert!(error.contains("exit code: 1") && error.contains("PowerShell parser failure") && error.contains("[truncated]"));
        assert!(error.contains(&stderr.display().to_string()) && !error.contains('\0')); assert!(error.len() < 5000);
        fs::write(&result, br#"{"status":"failed","message":"Update file verification failed: resources/dotnet/example.dll"}"#).unwrap();
        let error = helper_exit_error(&result, &stderr, "exit code: 1");
        assert!(error.contains("Update file verification failed: resources/dotnet/example.dll"));
        assert!(error.contains(&result.display().to_string()) && !error.contains("PowerShell parser failure"));
        remove_previous_file(&result).unwrap(); assert!(!result.exists());
        fs::write(&result, br#"{"status":"complete","message":"stale success"}"#).unwrap();
        assert!(!helper_exit_error(&result, &stderr, "exit code: 1").contains("stale success"));
        fs::write(&result, b"malformed").unwrap(); fs::remove_file(&stderr).unwrap();
        assert!(helper_exit_error(&result, &stderr, "exit code: 2").contains("No error output was recorded"));
        fs::remove_dir_all(root).unwrap();
    }
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
        assert!(trusted_url("https://www.amptis.com/apk/sspi-updates/v1/windows/development/123/update.zip", "windows", "development"));
        for url in ["http://amptis.com/apk/sspi-updates/v1/windows/development/a.zip", "https://amptis.com.evil.test/apk/sspi-updates/v1/windows/development/a.zip", "https://amptis.com/apk/sspi-updates/v1/windows/development/%2e%2e/a", "https://amptis.com/apk/sspi-updates/v1/windows/stable/a.zip", "https://amptis.com/apk/sspi-updates/v1/windows/development/../a.zip"] { assert!(!trusted_url(url, "windows", "development")); }
    }
    #[test] fn development_builds_and_downgrades() {
        let mut m: Manifest = serde_json::from_value(serde_json::json!({"schema":1,"product":"windows","channel":"development","version":"2.23.0","build":12,"publishedAt":"", "notes":"", "restart":"", "signature":"future-field", "package":{"url":"", "size":1,"sha256":"","format":"zip","future":true},"files":[]})).unwrap();
        let s = UpdateStatus { stage:"idle".into(), current_version:"2.23.0".into(),current_build:11, channel:"development".into(),available:None,downloaded:0,total:0,message:String::new(),last_checked:0,stage_dir:None };
        assert!(newer(&m,&s)); m.build=11; assert!(!newer(&m,&s)); m.build=13; m.version="2.22.9".into(); assert!(!newer(&m,&s));
    }
    #[test] fn double_digit_versions_and_build_ids_order_numerically() {
        let offer = |version: &str, build: u64| Manifest { version: version.into(), build, ..stage_test_manifest() };
        let installed = UpdateStatus { stage: "idle".into(), current_version: "2.25.9".into(), current_build: 20261007064742, channel: "development".into(),
            available: None, downloaded: 0, total: 0, message: String::new(), last_checked: 0, stage_dir: None };
        assert!(version("2.25.10") > version("2.25.9") && version("2.25.10+7") == version("2.25.10"));
        assert!(newer(&offer("2.25.10", 20261010090000), &installed));
        assert!(!newer(&offer("2.25.10", 20261007064742), &installed), "an equal build is not newer");
        assert!(!newer(&offer("2.25.8", 20261010090000), &installed), "no downgrade with a later build");
        let current = UpdateStatus { current_version: "2.25.10".into(), current_build: 20261010090000, ..installed };
        assert!(!newer(&offer("2.25.9", 20261011000000), &current));
        assert!(newer(&offer("2.25.11", 20261011000000), &current));
    }
    #[test] fn win_c_prunes_only_obsolete_stages() {
        let root = crate::test_output_root().join(format!("win-c-update-stages-{}", uuid::Uuid::new_v4()));
        for dir in ["stage-old", "stage-ready", "user-data"] { fs::create_dir_all(root.join(dir)).unwrap(); }
        fs::write(root.join("ready.json"), b"{}").unwrap();
        let keep = root.join("stage-ready");
        prune_stages(&root, Some(&keep)).unwrap();
        assert!(!root.join("stage-old").exists()); assert!(keep.exists()); assert!(root.join("user-data").exists());
        prune_stages(&root, None).unwrap();
        assert!(!keep.exists()); assert!(!root.join("ready.json").exists()); assert!(root.join("user-data").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
