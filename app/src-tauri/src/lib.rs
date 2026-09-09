mod package_sources;
mod archives;
use archives::safe_extraction_path;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::{future::join_all, StreamExt};
use keyring::Entry;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{watch, Mutex as AsyncMutex, Semaphore},
    time::{sleep, Duration},
};
use url::Url;
use uuid::Uuid;

const CONFIG_NAME: &str = "settings.json";
const RECEIVER_VERSION: &str = "1.0.3";
// The receiver owns one AppInst status slot; serialize console deliveries, while
// downloads/extraction and the lanes within each delivery remain concurrent.
static CONSOLE_DELIVERY: AsyncMutex<()> = AsyncMutex::const_new(());

async fn console_delivery_slot(cancel: &watch::Receiver<bool>) -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    if *cancel.borrow() { return Err("cancelled".into()); }
    let mut cancelled = cancel.clone();
    tokio::select! {
        slot = CONSOLE_DELIVERY.lock() => {
            if *cancel.borrow() { return Err("cancelled".into()); }
            Ok(slot)
        },
        _ = cancelled.changed() => Err("cancelled".into()),
    }
}

fn retryable_upload_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["closed the socket", "early eof", "connection reset", "forcibly closed",
     "connection aborted", "still closing", "capacity busy", "timed out",
     "os error 10053", "os error 10054", "os error 10060"].iter().any(|part| error.contains(part))
}
const PACKAGES_CACHE: &str = "packages-v3";
const SECRET_SERVICE: &str = "SimplePs5Installer.GameSearch";
const CHUNK: usize = 8 * 1024 * 1024;
const MAX_COVER_BYTES: u64 = 10 * 1024 * 1024;
const MAX_COVER_CACHE: u64 = 500 * 1024 * 1024;
const RECEIVER_ELF: &[u8] = include_bytes!("../../../../Build-Output/Windows Manager/sspi_receiver.elf");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    ps5_host: String,
    ps5_port: u16,
    resolver_base_url: String,
    download_dir: String,
    onboarding_complete: bool,
    real_debrid_enabled: bool,
    real_debrid_configured: bool,
    theme: String,
    reduce_motion: bool,
    #[serde(default = "default_transfer_mode")]
    transfer_mode: String,
    #[serde(default = "default_upload_lanes")]
    upload_lanes: u32,
}
fn default_transfer_mode() -> String {
    "balanced".into()
}
fn default_upload_lanes() -> u32 {
    4
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ps5_host: String::new(),
            ps5_port: 9114,
            resolver_base_url: String::new(),
            download_dir: dirs(),
            onboarding_complete: false,
            real_debrid_enabled: false,
            real_debrid_configured: false,
            theme: "dark".into(),
            reduce_motion: false,
            transfer_mode: default_transfer_mode(),
            upload_lanes: default_upload_lanes(),
        }
    }
}
fn dirs() -> String {
    std::env::var("USERPROFILE")
        .map(|x| format!("{x}\\Downloads\\Game Search"))
        .unwrap_or_else(|_| ".".into())
}
#[derive(Clone)]
struct AppState {
    settings: Arc<Mutex<Settings>>,
    cancel: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    jobs: Arc<Mutex<HashMap<String, Progress>>>,
    http: Client,
    // B3: per-title serialization for concurrent resolves; waiters re-check fresh cache.
    resolving: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    job_id: String,
    stage: String,
    progress: f64,
    bytes_done: u64,
    bytes_total: u64,
    speed_bps: f64,
    eta_seconds: Option<u64>,
    message: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    icon: Option<String>,
    title_id: String,
    package_kind: String,
    package_label: String,
    package_version: String,
    created_at: u64,
    stage_history: Vec<String>,
    paused: bool,
}
impl Default for Progress {
    fn default() -> Self {
        Self {
            job_id: String::new(),
            stage: String::new(),
            progress: 0.,
            bytes_done: 0,
            bytes_total: 0,
            speed_bps: 0.,
            eta_seconds: None,
            message: String::new(),
            title: String::new(),
            icon: None,
            title_id: String::new(),
            package_kind: String::new(),
            package_label: String::new(),
            package_version: String::new(),
            created_at: 0,
            stage_history: Vec::new(),
            paused: false,
        }
    }
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Game {
    title_id: String,
    name: String,
    region: String,
    icon: Option<String>,
    packages: Vec<Package>,
    source_id: String,
    source_name: String,
    source_version: String,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CatalogSection {
    id: String,
    title: String,
    games: Vec<Game>,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GameCatalog {
    sections: Vec<CatalogSection>,
    cached: bool,
    source: String,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Package {
    kind: String,
    label: String,
    url: String,
    #[serde(default)]
    access_type: String,
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    source_name: String,
    #[serde(default)]
    source_version: String,
    #[serde(default)]
    candidate_id: String,
    #[serde(default)]
    group_id: String,
    #[serde(default)]
    hoster: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    firmware: String,
    #[serde(default)]
    source_page_url: String,
    #[serde(default)]
    expected_size: Option<u64>,
    #[serde(default)]
    expected_sha256: String,
    #[serde(default)]
    expected_content_id: String,
    #[serde(default)]
    archive_set_id: Option<String>,
    #[serde(default)]
    archive_part_number: Option<u32>,
    #[serde(default)]
    archive_part_count: Option<u32>,
    #[serde(default)]
    archive_file_name: Option<String>,
    #[serde(default)]
    archive_format_hint: Option<String>,
    #[serde(default)]
    archive_password: Option<String>,
    #[serde(default)]
    mirror_id: Option<String>,
    #[serde(default)]
    intermediate_url: Option<String>,
    #[serde(default)]
    referer: Option<String>,
    #[serde(default)]
    diagnostics: Vec<String>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LocalPackage {
    number: usize,
    path: String,
    name: String,
    kind: String,
    size: u64,
    title_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSettings {
    ps5_host: String,
    ps5_port: u16,
    resolver_base_url: String,
    download_dir: String,
    onboarding_complete: bool,
    real_debrid_enabled: bool,
    theme: String,
    reduce_motion: bool,
    real_debrid_token: Option<String>,
    #[serde(default)]
    transfer_mode: Option<String>,
    #[serde(default)]
    upload_lanes: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryRequest {
    package: Package,
    title_id: Option<String>,
    #[serde(default)]
    title_name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    archive_parts: Vec<Package>,
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let p = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(p.join(CONFIG_NAME))
}
fn cache_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("gamesearch"))
}
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn read_cache_json<T: for<'de> Deserialize<'de>>(path: &Path, max_age: u64) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let saved = value.get("savedAt").and_then(Value::as_u64)?;
    if now_secs().saturating_sub(saved) > max_age {
        return None;
    }
    serde_json::from_value(value.get("payload").cloned()?).ok()
}
fn write_cache_json<T: Serialize>(path: &Path, payload: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = json!({"savedAt": now_secs(), "payload": payload});
    std::fs::write(path, serde_json::to_vec(&body).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn write_settings(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let p = config_path(app)?;
    std::fs::create_dir_all(p.parent().ok_or("config parent")?).map_err(|e| e.to_string())?;
    std::fs::write(p, serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn secret(name: &str) -> Result<Entry, String> {
    Entry::new(SECRET_SERVICE, name).map_err(|e| e.to_string())
}
fn redact(error: impl ToString) -> String {
    let s = error.to_string();
    let s = s.replace("Bearer ", "Bearer [redacted]");
    if s.to_ascii_lowercase().contains("early eof")
        || s.contains("UnexpectedEof")
        || s.contains("connection reset")
    {
        format!("PS5 closed the socket (receiver timed out or died). Reload the ELF and retry. [{s}]")
    } else {
        s
    }
}
fn network_error(context: &str) -> String {
    format!("{context} failed")
}
fn valid_http(s: &str) -> bool {
    Url::parse(s)
        .map(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.username().is_empty()
                && u.password().is_none()
        })
        .unwrap_or(false)
}
fn direct_package(url: &str) -> bool {
    let p = Url::parse(url)
        .ok()
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_default();
    p.ends_with(".pkg")
}
fn title_id(s: &str) -> bool {
    s.len() == 9
        && (s.starts_with("CUSA") || s.starts_with("PPSA"))
        && s[4..].bytes().all(|b| b.is_ascii_digit())
}

async fn patch_site_search(http: &Client, base: &str, query: &str) -> Vec<Game> {
    let response = http
        .get(format!("{base}/api/internal/search"))
        .query(&[("term", query)])
        .header(reqwest::header::REFERER, format!("{base}/"))
        .send()
        .await;
    let Ok(response) = response else {
        return Vec::new();
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    response
        .json::<Value>()
        .await
        .ok()
        .and_then(|value| search_results(&value).ok())
        .unwrap_or_default()
}

fn game_from_value(item: &Value) -> Option<Game> {
    let id = item
        .get("titleid")
        .or_else(|| item.get("title_id"))
        .or_else(|| item.get("titleId"))
        .or_else(|| item.get("cusa"))
        .or_else(|| item.get("cusa_id"))
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)?
        .to_ascii_uppercase();
    if !title_id(&id) {
        return None;
    }
    let icon = item
        .get("icon")
        .or_else(|| item.get("icon_url"))
        .or_else(|| item.get("cover"))
        .or_else(|| item.get("cover_url"))
        .or_else(|| item.get("image"))
        .or_else(|| item.get("image_url"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            if value.starts_with("http://") || value.starts_with("https://") {
                value.to_owned()
            } else {
                let host = if id.starts_with("PPSA") {
                    "https://prosperopatches.com"
                } else {
                    "https://orbispatches.com"
                };
                format!("{host}/{}", value.trim_start_matches('/'))
            }
        });
    Some(Game {
        title_id: id,
        name: item
            .get("name")
            .or_else(|| item.get("title"))
            .or_else(|| item.get("game_title"))
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .to_owned(),
        region: item
            .get("region")
            .or_else(|| item.get("region_code"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .to_owned(),
        icon,
        packages: packages_from_value(item).unwrap_or_default(),
        source_id: item
            .get("sourceId")
            .or_else(|| item.get("source_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        source_name: item
            .get("sourceName")
            .or_else(|| item.get("source_name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        source_version: item
            .get("sourceVersion")
            .or_else(|| item.get("source_version"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

fn search_results(value: &Value) -> Result<Vec<Game>, String> {
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        return Err("Search service did not report success".into());
    }
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.as_array())
        .ok_or("Search response has no results")?;
    Ok(results.iter().filter_map(game_from_value).collect())
}

fn resolver_api_urls(base: &str, path: &str) -> Vec<String> {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let mut urls = vec![format!("{base}/{path}")];
    if !base.to_ascii_lowercase().ends_with("/api") {
        urls.push(format!("{base}/api/{path}"));
    }
    urls
}

fn resolver_download_urls(base: &str, game_title_id: &str) -> Vec<String> {
    resolver_api_urls(base, &format!("games/{game_title_id}/downloads"))
}

fn catalog_from_value(value: &Value) -> Result<GameCatalog, String> {
    let raw_sections = value
        .get("sections")
        .and_then(Value::as_array)
        .ok_or("Resolver catalog response has no sections")?;
    let sections = raw_sections
        .iter()
        .filter_map(|section| {
            let games = section
                .get("games")
                .or_else(|| section.get("results"))
                .or_else(|| section.get("titles"))
                .and_then(Value::as_array)?
                .iter()
                .filter_map(game_from_value)
                .collect::<Vec<_>>();
            if games.is_empty() {
                return None;
            }
            let title = section
                .get("title")
                .or_else(|| section.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("Supported games")
                .to_owned();
            let id = section
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| title.to_ascii_lowercase().replace(' ', "_"));
            Some(CatalogSection { id, title, games })
        })
        .collect::<Vec<_>>();
    if sections.is_empty() {
        return Err("Resolver catalog did not contain valid CUSA or PPSA titles".into());
    }
    Ok(GameCatalog {
        sections,
        cached: value
            .get("cached")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        source: "Resolver catalog / Orbis Patches".into(),
    })
}

fn package_array(value: &Value) -> Option<&Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get("downloads").and_then(Value::as_array))
        .or_else(|| value.get("packages").and_then(Value::as_array))
        .or_else(|| value.get("results").and_then(Value::as_array))
        .or_else(|| {
            value.get("data").and_then(|data| {
                data.as_array()
                    .or_else(|| data.get("downloads").and_then(Value::as_array))
                    .or_else(|| data.get("packages").and_then(Value::as_array))
                    .or_else(|| data.get("results").and_then(Value::as_array))
            })
        })
}

fn packages_from_value(value: &Value) -> Result<Vec<Package>, String> {
    let values = package_array(value).ok_or("Resolver response has no package list")?;
    let packages: Vec<Package> = values
        .iter()
        .filter_map(|item| {
            let url = item
                .get("url")
                .or_else(|| item.get("link"))
                .or_else(|| item.get("download_url"))
                .or_else(|| item.get("downloadUrl"))
                .or_else(|| item.get("clean_url"))
                .or_else(|| item.get("cleanUrl"))
                .and_then(Value::as_str)?;
            if !valid_http(url) {
                return None;
            }
            let raw_kind = item
                .get("kind")
                .or_else(|| item.get("type"))
                .or_else(|| item.get("category"))
                .and_then(Value::as_str)
                .unwrap_or("file")
                .to_ascii_lowercase();
            let kind = if raw_kind.contains("backport") {
                "backport"
            } else if raw_kind.contains("update") || raw_kind.contains("patch") {
                "update"
            } else if raw_kind.contains("dlc") || raw_kind.contains("addon") {
                "dlc"
            } else if raw_kind.contains("base") || raw_kind.contains("game") {
                "base"
            } else {
                "file"
            };
            Some(Package {
                kind: kind.to_owned(),
                label: item
                    .get("label")
                    .or_else(|| item.get("name"))
                    .or_else(|| item.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or(kind)
                    .to_owned(),
                url: url.to_owned(),
                access_type: item
                    .get("accessType")
                    .or_else(|| item.get("access_type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                source_id: item
                    .get("sourceId")
                    .or_else(|| item.get("source_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                source_name: item
                    .get("sourceName")
                    .or_else(|| item.get("source_name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                source_version: item
                    .get("sourceVersion")
                    .or_else(|| item.get("source_version"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                candidate_id: item
                    .get("candidateId")
                    .or_else(|| item.get("candidate_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                group_id: item
                    .get("groupId")
                    .or_else(|| item.get("group_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                hoster: item
                    .get("hoster")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                version: item
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                firmware: item
                    .get("firmware")
                    .or_else(|| item.get("requiredFirmware"))
                    .or_else(|| item.get("required_firmware"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                source_page_url: item
                    .get("sourcePageUrl")
                    .or_else(|| item.get("source_page_url"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                expected_size: item
                    .get("expectedSize")
                    .or_else(|| item.get("expected_size"))
                    .and_then(Value::as_u64),
                expected_sha256: item
                    .get("expectedSha256")
                    .or_else(|| item.get("expected_sha256"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                expected_content_id: item
                    .get("expectedContentId")
                    .or_else(|| item.get("expected_content_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                archive_set_id: item
                    .get("archiveSetId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                archive_part_number: item
                    .get("archivePartNumber")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                archive_part_count: item
                    .get("archivePartCount")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                archive_file_name: item
                    .get("archiveFileName")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                archive_password: item.get("archivePassword").and_then(Value::as_str).map(str::to_owned),
                archive_format_hint: item
                    .get("archiveFormatHint")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                mirror_id: item
                    .get("mirrorId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                intermediate_url: item
                    .get("intermediateUrl")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                referer: item
                    .get("referer")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                diagnostics: item
                    .get("diagnostics")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    if packages.is_empty() && !values.is_empty() {
        Err("Resolver returned entries, but none contained a usable HTTP link".into())
    } else {
        Ok(packages)
    }
}
fn inherit_progress_context(p: &mut Progress, prev: &Progress) {
    if p.title.is_empty() { p.title = prev.title.clone(); }
    if p.icon.is_none() { p.icon = prev.icon.clone(); }
    if p.title_id.is_empty() { p.title_id = prev.title_id.clone(); }
    if p.package_kind.is_empty() { p.package_kind = prev.package_kind.clone(); }
    if p.package_label.is_empty() { p.package_label = prev.package_label.clone(); }
    if p.package_version.is_empty() { p.package_version = prev.package_version.clone(); }
    p.created_at = prev.created_at;
    p.stage_history = prev.stage_history.clone();
    p.paused = prev.paused && !terminal_stage(&p.stage);
}

fn emit(app: &AppHandle, mut p: Progress) {
    let terminal = terminal_stage(&p.stage);
    if let Some(state) = app.try_state::<AppState>() {
        let mut jobs = state.jobs.lock().unwrap();
        if let Some(prev) = jobs.get(&p.job_id) {
            inherit_progress_context(&mut p, prev);
        }
        if p.created_at == 0 {
            p.created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        }
        if !terminal && !p.stage_history.contains(&p.stage) {
            p.stage_history.push(p.stage.clone());
        }
        jobs.insert(p.job_id.clone(), p.clone());
        if terminal {
            state.cancel.lock().unwrap().remove(&p.job_id);
        }
    }
    let _ = app.emit("delivery-progress", p);
}

#[cfg(test)]
mod download_context_tests {
    use super::*;
    #[test]
    fn pause_survives_in_flight_events_and_clears_on_completion() {
        let previous = Progress { stage: "uploading".into(), paused: true, ..Default::default() };
        let mut next = Progress { stage: "uploading".into(), ..Default::default() };
        inherit_progress_context(&mut next, &previous);
        assert!(next.paused);
        next.stage = "cancelled".into();
        inherit_progress_context(&mut next, &previous);
        assert!(!next.paused);
        assert!(pausable_stage("downloading"));
        assert!(pausable_stage("uploading"));
        for stage in ["extracting", "submitting", "installing", "mounting", "complete"] { assert!(!pausable_stage(stage)); }
    }


    #[test]
    fn failed_event_keeps_game_package_identity_and_observed_stages() {
        let previous = Progress {
            title: "Stardew Valley".into(), title_id: "CUSA06840".into(),
            package_kind: "update".into(), package_label: "Update package".into(),
            package_version: "1.63".into(), icon: Some("cover".into()), created_at: 42,
            stage_history: vec!["downloading".into(), "extracting".into(), "uploading".into()],
            ..Default::default()
        };
        let mut failure = Progress { stage: "failed".into(), message: "Connection lost".into(), ..Default::default() };
        inherit_progress_context(&mut failure, &previous);
        let wire = serde_json::to_value(&failure).unwrap();
        assert_eq!(wire["titleId"], "CUSA06840");
        assert_eq!(wire["packageKind"], "update");
        assert_eq!(wire["packageVersion"], "1.63");
        assert_eq!(wire["createdAt"], 42);
        assert_eq!(wire["stageHistory"][2], "uploading");
        assert_eq!(wire["stage"], "failed");
    }
}

fn terminal_stage(stage: &str) -> bool {
    matches!(
        stage,
        "complete" | "failed" | "cancelled" | "monitoring-ended"
    )
}

fn job_error_stage(error: &str) -> &'static str {
    if error == "cancelled" {
        "cancelled"
    } else if error == "Install remains in progress; check receiver status later" {
        "monitoring-ended"
    } else {
        "failed"
    }
}

fn download_key(url: &str, title: Option<&str>, kind: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in url
        .bytes()
        .chain(title.unwrap_or("").bytes())
        .chain(kind.bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn validate_receiver_candidate(host: &str, port: u16) -> Result<(), String> {
    if host.trim().is_empty() || port < 1024 {
        Err("Provide a host and valid port".into())
    } else {
        Ok(())
    }
}

fn require_preflight(response: u8) -> Result<(), String> {
    if response == 1 {
        Ok(())
    } else {
        Err("Installer preflight rejected by receiver".into())
    }
}

fn accepted_submission(value: &Value) -> Result<String, String> {
    let code = value["install_api_code"]
        .as_i64()
        .or_else(|| value["api_code"].as_i64());
    let content_id = value["content_id"].as_str().filter(|x| !x.is_empty());
    if code == Some(0) && value["state"].as_str() == Some("submitted") {
        Ok(content_id.unwrap_or("").to_owned())
    } else {
        let error = value["error"].as_str().unwrap_or("rejected");
        let stage = value["stage"].as_str().unwrap_or("");
        Err(format!(
            "Install submission failed: {error} {stage} (code {})",
            code.unwrap_or(-1)
        ))
    }
}

#[derive(Debug, PartialEq)]
enum InstallDecision {
    Installing { status: String, progress: f64 },
    Complete,
}

fn install_decision(value: &Value) -> Result<InstallDecision, String> {
    let state = value["state"].as_str().unwrap_or("unknown");
    let status = value["status"].as_str().unwrap_or(state);
    let error = value["error"].as_str().unwrap_or("");
    let error_code = value["error_code"].as_i64().unwrap_or(0);
    if state == "failed" {
        return Err(format!(
            "Install failed: {status} {error} ({error_code})"
        ));
    }
    if state == "complete" && matches!(status, "playable" | "installed" | "complete") {
        return Ok(InstallDecision::Complete);
    }
    Ok(InstallDecision::Installing {
        status: if status.is_empty() {
            "installing".into()
        } else {
            status.to_owned()
        },
        progress: (value["progress"].as_f64().unwrap_or(0.) / 100.).clamp(0., 1.),
    })
}

fn split_ranges(total: u64, lanes: u64) -> Vec<(u64, u64)> {
    (0..lanes)
        .map(|i| {
            let start = total * i / lanes;
            (start, total * (i + 1) / lanes - start)
        })
        .collect()
}

#[cfg(test)]
fn extract_zip_pkgs(source: &Path, cache: &Path) -> Result<Vec<PathBuf>, String> {
    match extract_any_archive(source, cache, ArtifactKind::Zip, |_,_,_|{})? {
        ExtractedContent::Pkgs(pkgs) => Ok(pkgs),
        ExtractedContent::Dump(_) => Err("Expected PKGs".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactKind {
    Pkg,
    Zip,
    Rar,
    SevenZ,
    Unknown,
}

fn artifact_kind(header: &[u8], name: &str, content_type: &str) -> ArtifactKind {
    if header.starts_with(&[0x7f, 0x43, 0x4e, 0x54]) {
        return ArtifactKind::Pkg;
    }
    if header.starts_with(b"PK") {
        return ArtifactKind::Zip;
    }
    if header.starts_with(b"Rar!") {
        return ArtifactKind::Rar;
    }
    if header.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return ArtifactKind::SevenZ;
    }
    let lower = name.to_ascii_lowercase();
    let content = content_type.to_ascii_lowercase();
    if lower.contains(".7z") || content.contains("7z") || content.contains("7-zip") {
        ArtifactKind::SevenZ
    } else if lower.contains(".rar")
        || lower.contains(".r00")
        || content.contains("rar")
        || content.contains("x-rar")
    {
        ArtifactKind::Rar
    } else if lower.contains(".zip") || lower.contains(".z01") || content.contains("zip") {
        ArtifactKind::Zip
    } else if lower.ends_with(".pkg") {
        ArtifactKind::Pkg
    } else {
        ArtifactKind::Unknown
    }
}

fn disposition_filename(header: &str) -> Option<String> {
    let utf = Regex::new(r"(?i)filename\*\s*=\s*(?:UTF-8''|utf-8'')([^;]+)").ok()?;
    if let Some(value) = utf
        .captures(header)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str().trim_matches('"').to_owned())
    {
        let decoded = urlencoding_fallback(&value);
        if !decoded.is_empty() {
            return Some(decoded);
        }
    }
    let plain = Regex::new(r#"(?i)filename\s*=\s*"?([^";]+)"?"#).ok()?;
    plain
        .captures(header)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str().trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn urlencoding_fallback(value: &str) -> String {
    let mut out = String::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""), 16)
            {
                out.push(byte as char);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    Path::new(&out)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(out)
}

const RAR_PASSWORDS: [&[u8]; 5] = [b"", b"[DLPSGAME.COM]", b"DLPSGAME.COM", b"[dlpsgame.com]", b"dlpsgame.com"];

fn archive_passwords(password: Option<&str>) -> Vec<&[u8]> {
    let mut candidates = Vec::new();
    if let Some(value) = password.filter(|p| !p.is_empty()) { candidates.push(value.as_bytes()); }
    for value in RAR_PASSWORDS { if !candidates.contains(&value) { candidates.push(value); } }
    candidates
}

fn dir_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let mut directory = path;
    while !directory.is_dir() { directory = directory.parent()?; }
    let mut wide: Vec<u16> = directory.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut free = 0u64;
    extern "system" {
        fn GetDiskFreeSpaceExW(p: *const u16, a: *mut u64, b: *mut u64, c: *mut u64) -> i32;
    }
    let mut total = 0u64;
    let mut avail = 0u64;
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut avail) } != 0 {
        Some(free)
    } else {
        None
    }
}

fn rar_list_size(path: &Path, password: &[u8]) -> Result<u64, String> {
    let archive = if password.is_empty() {
        unrar::Archive::new(path)
    } else {
        unrar::Archive::with_password(path, password)
    };
    let mut total = 0u64;
    for entry in archive.open_for_listing().map_err(|error| error.to_string())? {
        let header = entry.map_err(|error| error.to_string())?;
        if header.is_file() {
            total = total.saturating_add(header.unpacked_size);
        }
    }
    Ok(total)
}

fn rar_extract_to(path: &Path, dest: &Path, password: &[u8]) -> Result<u64, String> {
    std::fs::create_dir_all(dest).map_err(redact)?;
    let archive = if password.is_empty() {
        unrar::Archive::new(path)
    } else {
        unrar::Archive::with_password(path, password)
    };
    let mut archive = archive
        .open_for_processing()
        .map_err(|error| error.to_string())?;
    let mut files = 0u64;
    loop {
        let Some(header) = archive.read_header().map_err(|error| error.to_string())? else {
            break;
        };
        let entry_path = &header.entry().filename;
        if !safe_extraction_path(entry_path) { return Err("RAR contains an unsafe path".into()); }
        archive = if header.entry().is_file() {
            files += 1;
            header
                .extract_with_base(dest)
                .map_err(|error| error.to_string())?
        } else {
            header.skip().map_err(|error| error.to_string())?
        };
    }
    if files == 0 {
        return Err(
            "archive produced no files (opened on a non-first volume or all entries are split continuations)"
                .into(),
        );
    }
    Ok(files)
}

fn extract_rar_builtin(
    source: &Path,
    dest: &Path,
    password: Option<&str>,
    progress: std::sync::Arc<dyn Fn(u64, u64, f64) + Send + Sync>,
) -> Result<(), String> {
    let guessed = unrar::Archive::new(source)
        .as_first_part()
        .filename()
        .to_path_buf();
    let path = if guessed.is_file() {
        guessed.clone()
    } else if source.is_file() {
        source.to_path_buf()
    } else {
        return Err(format!(
            "RAR first volume is missing ({})",
            guessed
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
    };
    for password in archive_passwords(password) {
        let probe = if password.is_empty() {
            unrar::Archive::new(&path)
        } else {
            unrar::Archive::with_password(&path, password)
        };
        if let Ok(open) = probe.open_for_listing() {
            if open.volume_info() == unrar::VolumeInfo::Subsequent {
                return Err(format!(
                    "{} is not the first volume of the set; part1 was not downloaded or was renamed",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            break;
        }
    }
    std::fs::create_dir_all(dest).map_err(redact)?;
    // E1/E2: know the total up front (header walk, not extraction) and poll at
    // 1 Hz instead of re-walking a 100 GB tree four times a second.
    let mut listed = 0u64;
    for password in archive_passwords(password) {
        if let Ok(size) = rar_list_size(&path, password) {
            listed = size;
            break;
        }
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let dest_watch = dest.to_path_buf();
    let stop_watch = stop.clone();
    let progress_watch = progress.clone();
    let started = Instant::now();
    let poller = std::thread::spawn(move || {
        while !stop_watch.load(Ordering::Relaxed) {
            let done = dir_bytes(&dest_watch);
            let speed = done as f64 / started.elapsed().as_secs_f64().max(0.01);
            if listed > 0 {
                progress_watch(done.min(listed), listed, speed);
            } else {
                progress_watch(done, 0, speed);
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    });
    let mut last = "Unable to extract RAR; check the password and all archive volumes.".to_string();
    let result = (|| {
        for password in archive_passwords(password) {
            let _ = std::fs::remove_dir_all(dest);
            std::fs::create_dir_all(dest).map_err(redact)?;
            match rar_extract_to(&path, dest, password) {
                Ok(_) => return Ok(()),
                Err(error) => last = error,
            }
        }
        Err(last)
    })();
    stop.store(true, Ordering::Relaxed);
    let _ = poller.join();
    if result.is_ok() {
        let done = dir_bytes(dest);
        progress(done, done.max(1), 0.);
    }
    result
}

fn rar_volume_filename(package: &Package, count: usize) -> String {
    if let Some(file) = package
        .archive_file_name
        .as_deref()
        .and_then(|name| Path::new(name).file_name())
        .and_then(|name| name.to_str())
        .filter(|file| !file.is_empty() && !file.contains("..") && !file.contains('/') && !file.contains('\\'))
    {
        return file.to_string();
    }
    let number = match package.archive_part_number.unwrap_or(1) {
        0 => 1,
        n => n,
    };
    if count <= 1 {
        "archive.rar".into()
    } else {
        let width = if count >= 10 { 2 } else { 1 };
        format!("archive.part{number:0width$}.rar")
    }
}

fn prepare_rar_volumes(downloaded: &[PathBuf], parts: &[Package]) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let dir = downloaded
        .first()
        .and_then(|path| path.parent())
        .ok_or("Archive staging directory is missing")?;
    let count = downloaded.len();
    let stem = parts
        .iter()
        .filter_map(|package| package.archive_file_name.as_deref())
        .filter_map(|name| Path::new(name).file_name().and_then(|n| n.to_str()))
        .find_map(|name| {
            Regex::new(r"(?i)^(.+?)\.part0*\d+\.rar$")
                .ok()?
                .captures(name)?
                .get(1)
                .map(|m| m.as_str().to_owned())
        })
        .unwrap_or_else(|| "archive".to_owned());
    let width = if count >= 100 {
        3
    } else if count >= 10 {
        2
    } else {
        1
    };
    let names: Vec<String> = parts
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, package)| {
            let number = package.archive_part_number.unwrap_or(index as u32 + 1).max(1) as usize;
            if count <= 1 {
                format!("{stem}.rar")
            } else {
                format!("{stem}.part{number:0width$}.rar")
            }
        })
        .collect();
    if names.len() != count {
        return Err("Archive part list does not match downloaded files".into());
    }
    if names.iter().collect::<std::collections::HashSet<_>>().len() != names.len() {
        return Err("Archive parts reused the same volume name".into());
    }
    let mut first = None;
    let mut prepared = Vec::with_capacity(downloaded.len());
    for (path, name) in downloaded.iter().zip(names.iter()) {
        let dest = dir.join(name);
        if path != &dest {
            if dest.exists() {
                std::fs::remove_file(&dest).map_err(redact)?;
            }
            std::fs::rename(path, &dest).map_err(redact)?;
        }
        prepared.push(dest.clone());
        if first.is_none() {
            first = Some(dest);
        }
    }
    Ok((first.ok_or("Archive staging directory is missing")?, prepared))
}

fn collect_extracted_pkgs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let canonical = root.canonicalize().map_err(redact)?;
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(redact)? {
            let path = entry.map_err(redact)?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let canon = path.canonicalize().map_err(redact)?;
            if !canon.starts_with(&canonical) {
                return Err("Extracted path escaped staging directory".into());
            }
            let mut magic = [0; 4];
            let mut file = std::fs::File::open(&path).map_err(redact)?;
            use std::io::Read;
            if file.read(&mut magic).unwrap_or(0) == 4 && magic == [0x7f, 0x43, 0x4e, 0x54] {
                found.push(path);
            }
        }
    }
    Ok(found)
}

fn pkg_content_id(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 0x54];
    file.read_exact(&mut header).ok()?;
    if header[..4] != [0x7f, 0x43, 0x4e, 0x54] {
        return None;
    }
    for offset in [0x30usize, 0x40] {
        let end = offset + 0x24;
        if end > header.len() {
            continue;
        }
        let raw = std::str::from_utf8(&header[offset..end]).ok()?;
        let id = raw.trim_end_matches('\0').trim();
        let upper = id.to_ascii_uppercase();
        if id.len() >= 16 && (upper.contains("-CUSA") || upper.contains("-PPSA")) {
            return Some(id.to_owned());
        }
    }
    None
}

fn pkg_role(path: &Path) -> u8 {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name.contains("dlc")
        || name.contains("fpack")
        || name.contains("-ac.")
        || name.contains("_ac_")
        || name.contains("extradat")
    {
        2
    } else if name.contains("update")
        || name.contains("patch")
        || name.contains("marlin")
        || name.contains("fixpak")
    {
        1
    } else {
        0
    }
}

fn sort_pkgs(mut packages: Vec<PathBuf>) -> Vec<PathBuf> {
    packages.sort_by(|left, right| {
        pkg_role(left).cmp(&pkg_role(right)).then_with(|| {
            let left_size = std::fs::metadata(left).map(|m| m.len()).unwrap_or(0);
            let right_size = std::fs::metadata(right).map(|m| m.len()).unwrap_or(0);
            right_size.cmp(&left_size)
        })
    });
    packages
}

fn pkg_role_label(path: &Path) -> &'static str {
    match pkg_role(path) {
        1 => "update",
        2 => "DLC",
        _ => "base",
    }
}

fn appinst_idle(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "" | "none" | "unknown" | "n/a"
    )
}

fn finish_pkg_install(
    app: &AppHandle,
    job: &str,
    done: u64,
    total: u64,
    label: &str,
    announce_complete: bool,
) -> Result<(), String> {
    if announce_complete {
        emit(
            app,
            Progress {
                job_id: job.into(),
                stage: "complete".into(),
                progress: 1.,
                bytes_done: done,
                bytes_total: total,
                speed_bps: 0.,
                eta_seconds: None,
                message: format!("{label} submitted to AppInst"),
                ..Default::default()
            },
        );
    }
    Ok(())
}

fn is_game_dump(root: &Path) -> bool {
    root.join("eboot.bin").is_file() && root.join("sce_sys").is_dir()
}

fn dump_title_id(root: &Path) -> Option<String> {
    let from_json = std::fs::read_to_string(root.join("sce_sys").join("param.json"))
        .ok()
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
        .and_then(|value| {
            ["titleId", "titleid", "TITLEID", "title_id"]
                .iter()
                .find_map(|key| value.get(*key).and_then(|v| v.as_str()))
                .map(str::to_ascii_uppercase)
                .filter(|id| title_id(id))
        });
    from_json.or_else(|| {
        let name = root.file_name()?.to_str()?;
        if name.eq_ignore_ascii_case("image0") {
            title_from_path(root.parent()?)
        } else {
            title_from_path(root)
        }
    })
}

fn find_dump_root(root: &Path) -> Option<PathBuf> {
    if is_game_dump(root) {
        return Some(root.to_path_buf());
    }
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if is_game_dump(&dir) {
            found.push(dir);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    if found.is_empty() {
        None
    } else if found.len() == 1 {
        found.pop()
    } else {
        found.into_iter().max_by_key(|path| dir_bytes(path))
    }
}

fn dump_files(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let canonical = root.canonicalize().map_err(redact)?;
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).map_err(redact)? {
            let path = entry.map_err(redact)?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let canon = path.canonicalize().map_err(redact)?;
            if !canon.starts_with(&canonical) {
                return Err("Dump path escaped extraction directory".into());
            }
            let relative = canon
                .strip_prefix(&canonical)
                .map_err(|_| "Dump path escaped extraction directory")?
                .to_string_lossy()
                .replace('\\', "/");
            if relative.is_empty() || relative.split('/').any(|part| part == ".." || part == ".") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case(".ds_store")
                || name.eq_ignore_ascii_case("thumbs.db")
                || name.starts_with("._")
            {
                continue;
            }
            files.push((path, relative));
        }
    }
    if files.is_empty() {
        Err("Game dump contained no files".into())
    } else {
        Ok(files)
    }
}



fn shadowmount_dir(title: Option<&str>, kind: &str) -> Result<String, String> {
    let id = title
        .map(str::to_ascii_uppercase)
        .filter(|value| title_id(value))
        .ok_or("Game dump needs a CUSA or PPSA title ID")?;
    Ok(
        if matches!(
            kind.to_ascii_lowercase().as_str(),
            "backport" | "bp" | "back-port"
        ) {
            format!("/data/homebrew/backports/{id}")
        } else {
            format!("/data/homebrew/{id}")
        },
    )
}

enum ExtractedContent {
    Pkgs(Vec<PathBuf>),
    Dump(PathBuf),
}

fn extracted_from_dir(dest: &Path) -> Option<ExtractedContent> {
    if let Some(root) = find_dump_root(dest) {
        return Some(ExtractedContent::Dump(root));
    }
    let pkgs = collect_extracted_pkgs(dest).unwrap_or_default();
    if pkgs.is_empty() {
        None
    } else {
        Some(ExtractedContent::Pkgs(pkgs))
    }
}

fn nested_rar_volumes(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if let Some(part) = rar_part_from_name(&name) {
                if part != 1 {
                    continue;
                }
            }
            let continuation = Regex::new(r"(?i)\.(r\d{2}|z\d{2}|\d{3})$")
                .ok()
                .is_some_and(|regex| regex.is_match(&name));
            if continuation
                && !name.ends_with(".r00")
                && !name.ends_with(".z01")
                && !name.ends_with(".001")
            {
                continue;
            }
            if !(name.ends_with(".rar")
                || name.ends_with(".zip")
                || name.ends_with(".7z")
                || name.ends_with(".r00")
                || name.ends_with(".z01")
                || name.ends_with(".001"))
            {
                continue;
            }
            found.push(path);
        }
    }
    found
}

fn extract_listing(root: &Path) -> String {
    let mut names = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .take(24)
        {
            let path = entry.path();
            let shown = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                names.push(format!("{shown}/"));
                if depth < 2 {
                    stack.push((path, depth + 1));
                }
            } else {
                names.push(format!(
                    "{shown} ({} MB)",
                    entry.metadata().map(|meta| meta.len()).unwrap_or(0) / 1_048_576
                ));
            }
            if names.len() >= 40 {
                break;
            }
        }
        if names.len() >= 40 {
            break;
        }
    }
    if names.is_empty() {
        "empty folder".into()
    } else {
        names.join(", ")
    }
}

fn rar_part_from_name(name: &str) -> Option<u32> {
    Regex::new(r"(?i)\.part0*(\d+)\.rar(?:$|[?#])")
        .ok()?
        .captures(name)?
        .get(1)?
        .as_str()
        .parse()
        .ok()
        .filter(|part| *part > 0)
}

fn missing_rar_volumes(names: &[String]) -> Option<String> {
    let parts = names
        .iter()
        .filter_map(|name| rar_part_from_name(name))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    let max = parts.iter().copied().max()?;
    if names.len() == 1 && max >= 1 && names.iter().any(|name| {
        let lower = name.to_ascii_lowercase();
        lower.contains(".part") && lower.ends_with(".rar")
    }) {
        return Some(format!(
            "only {} was downloaded; this is a split RAR and every .partN.rar volume is required",
            names[0]
        ));
    }
    let mut missing = Vec::new();
    for expected in 1..=max.max(parts.len() as u32) {
        if !parts.contains(&expected) {
            missing.push(expected);
        }
    }
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "missing part {}",
            missing
                .iter()
                .map(|part| part.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

fn extract_any_archive(
    source: &Path, cache: &Path, kind: ArtifactKind,
    progress: impl Fn(u64,u64,f64) + Send + Sync + 'static,
) -> Result<ExtractedContent, String> {
    archives::extract_content(source, cache, kind, None, Arc::new(progress), 0)
}

fn archive_incomplete(package: &Package) -> bool {
    package
        .diagnostics
        .iter()
        .any(|value| value.contains("incomplete archive set") || value.contains("missing Part."))
}

fn delivery_parts(request: &DeliveryRequest) -> Result<Vec<Package>, String> {
    if request.package.archive_set_id.is_none() {
        return Ok(vec![request.package.clone()]);
    }
    let mut parts = request.archive_parts.clone();
    if parts.is_empty() {
        parts.push(request.package.clone());
    }
    let claimed = parts
        .iter()
        .find_map(|part| part.archive_part_count)
        .or(request.package.archive_part_count);
    if parts.len() == 1 && claimed.unwrap_or(2) > 1 {
        return Err(format!(
            "Archive set {} needs every complementary part from the same mirror before installation.",
            request.package.archive_set_id.as_deref().unwrap_or("unknown")
        ));
    }
    if request.package.archive_set_id.is_some() && parts.len() <= 1 {
        let blob = format!(
            "{} {}",
            request.package.label,
            request.package.archive_file_name.as_deref().unwrap_or("")
        )
        .to_ascii_lowercase();
        if blob.contains(".part") {
            return Err("one volume of a split set".into());
        }
    }
    if let Some(count) = claimed {
        if parts.len() as u32 != count {
            return Err(format!(
                "Archive set has {}/{} parts. Choose a complete mirror.",
                parts.len(),
                count
            ));
        }
    }
    if parts.iter().any(archive_incomplete) {
        return Err(
            "This archive set is incomplete (missing parts). Choose another mirror.".into(),
        );
    }
    parts.sort_by_key(|part| part.archive_part_number.unwrap_or(0));
    let numbers: Vec<u32> = parts
        .iter()
        .filter_map(|part| part.archive_part_number)
        .collect();
    if !numbers.is_empty() {
        let highest = numbers.iter().copied().max().unwrap_or(1);
        let gaps: Vec<String> = (1..=highest)
            .filter(|want| !numbers.contains(want))
            .map(|want| want.to_string())
            .collect();
        if !gaps.is_empty() {
            return Err(format!(
                "This mirror is missing RAR volume(s) {}. Pick a mirror that lists all {highest} parts.",
                gaps.join(", ")
            ));
        }
        if numbers.len() != parts.len() {
            return Err(format!(
                "{} of {} links on this mirror have no part number; the archive set cannot be ordered.",
                parts.len() - numbers.len(),
                parts.len()
            ));
        }
    }
    Ok(parts)
}

async fn file_header(path: &Path) -> Result<([u8; 8], usize, u64), String> {
    let mut file = fs::File::open(path).await.map_err(redact)?;
    let size = file.metadata().await.map_err(redact)?.len();
    let mut header = [0; 8];
    let read = file.read(&mut header).await.map_err(redact)?;
    Ok((header, read, size))
}



#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}
#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<AppState>,
    input: SaveSettings,
) -> Result<Settings, String> {
    if !input.ps5_host.trim().is_empty() {
        validate_receiver_candidate(&input.ps5_host, input.ps5_port)?;
    }
    if !input.resolver_base_url.is_empty() && !valid_http(&input.resolver_base_url) {
        return Err("Resolver URL must be http/https".into());
    }
    if input.download_dir.trim().is_empty() {
        return Err("Download folder is required".into());
    }
    if let Some(v) = input.real_debrid_token.filter(|v| !v.trim().is_empty()) {
        secret("real-debrid")?.set_password(&v).map_err(redact)?;
    }
    let mut s = state.settings.lock().unwrap();
    *s = Settings {
        ps5_host: input.ps5_host.trim().into(),
        ps5_port: input.ps5_port,
        resolver_base_url: input.resolver_base_url.trim_end_matches('/').into(),
        download_dir: input.download_dir.trim().into(),
        onboarding_complete: input.onboarding_complete,
        real_debrid_enabled: input.real_debrid_enabled,
        real_debrid_configured: secret("real-debrid")
            .ok()
            .and_then(|x| x.get_password().ok())
            .is_some(),
        theme: input.theme,
        reduce_motion: input.reduce_motion,
        transfer_mode: input
            .transfer_mode
            .unwrap_or_else(default_transfer_mode)
            .to_ascii_lowercase(),
        upload_lanes: input.upload_lanes.unwrap_or_else(default_upload_lanes).clamp(1, 12),
    };
    write_settings(&app, &s)?;
    Ok(s.clone())
}

#[tauri::command]
fn export_receiver_payload() -> Result<String, String> {
    if !RECEIVER_ELF.starts_with(&[0x7f, b'E', b'L', b'F']) {
        return Err("Bundled receiver payload is invalid".into());
    }
    let downloads = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .map(|path| path.join("Downloads"))
        .map_err(|_| "Windows Downloads folder could not be located".to_string())?;
    std::fs::create_dir_all(&downloads).map_err(redact)?;
    let mut destination = downloads.join("sspi_receiver.elf");
    for number in 1..100 {
        if !destination.exists() {
            break;
        }
        destination = downloads.join(format!("sspi_receiver ({number}).elf"));
    }
    if destination.exists() {
        return Err("Too many receiver payload copies already exist in Downloads".into());
    }
    std::fs::write(&destination, RECEIVER_ELF).map_err(redact)?;
    Ok(destination.to_string_lossy().into_owned())
}

#[tauri::command]
fn list_package_sources(app: AppHandle) -> Result<Vec<package_sources::SourceSummary>, String> {
    package_sources::list(&app)
}

#[tauri::command]
async fn install_package_source(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
) -> Result<Vec<package_sources::SourceSummary>, String> {
    package_sources::install(&app, &state.http, url.trim()).await
}

#[tauri::command]
fn install_package_source_from_path(
    app: AppHandle,
    path: String,
) -> Result<Vec<package_sources::SourceSummary>, String> {
    package_sources::install_from_path(&app, path.trim())
}

#[tauri::command]
fn set_package_source_enabled(
    app: AppHandle,
    id: String,
    enabled: bool,
) -> Result<Vec<package_sources::SourceSummary>, String> {
    package_sources::set_enabled(&app, &id, enabled)
}

#[tauri::command]
fn remove_package_source(
    app: AppHandle,
    id: String,
) -> Result<Vec<package_sources::SourceSummary>, String> {
    package_sources::remove(&app, &id)
}
#[tauri::command]
async fn test_ps5(host: String, port: u16) -> Result<String, String> {
    validate_receiver_candidate(&host, port)?;
    let settings = Settings {
        ps5_host: host,
        ps5_port: port,
        ..Settings::default()
    };
    ping(&settings).await?;
    let mut stream = connect_ps5(&settings, "receiver version").await?;
    stream.set_nodelay(true).ok();
    let (code, body) = frame(&mut stream, 0x53, &[]).await?;
    let text = String::from_utf8_lossy(&body);
    let version = text
        .split("\"version\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or("unknown");
    if code != 3 && code != 1 {
        return Err("Receiver GET_CONFIG failed".into());
    }
    if version != RECEIVER_VERSION {
        return Err(format!(
            "Receiver is v{version}, app needs v{RECEIVER_VERSION}. Download receiver ELF, reload it on the PS5, retry."
        ));
    }
    let sha = sha256_hex(RECEIVER_ELF);
    Ok(format!(
        "Receiver verified (v{version}, sha {})",
        &sha[..12.min(sha.len())]
    ))
}
#[tauri::command]
async fn test_resolver(state: State<'_, AppState>, base_url: String) -> Result<String, String> {
    let base = base_url.trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err("Set a Resolver URL first".into());
    };
    if !valid_http(&base) {
        return Err("Resolver URL must start with http:// or https://".into());
    }
    let suffixes: &[&str] = if base.to_ascii_lowercase().ends_with("/api") {
        &["/health", ""]
    } else {
        &["/health", "/api", "/api/health"]
    };
    for suffix in suffixes {
        if let Ok(response) = state.http.get(format!("{base}{suffix}")).send().await {
            if response.status().is_success() {
                return Ok(format!("Resolver reachable at {suffix}"));
            }
        }
    }
    Err("Resolver did not answer a supported health endpoint".into())
}
#[tauri::command]
async fn verify_real_debrid(
    state: State<'_, AppState>,
    token: Option<String>,
) -> Result<String, String> {
    let t = token.filter(|x| !x.is_empty()).map(Ok).unwrap_or_else(|| {
        secret("real-debrid")?
            .get_password()
            .map_err(|_| "No Real-Debrid token stored".to_string())
    })?;
    let r = state
        .http
        .get("https://api.real-debrid.com/rest/1.0/user")
        .bearer_auth(t)
        .send()
        .await
        .map_err(|_| network_error("Real-Debrid request"))?;
    if r.status().is_success() {
        Ok("Token verified".into())
    } else {
        Err(format!("Token verification failed: HTTP {}", r.status()))
    }
}
#[tauri::command]
async fn search_games(
    app: AppHandle,
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<Game>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Enter a title name, CUSA ID, or PPSA ID".into());
    }
    // B4: 15-minute disk cache per normalized query (branch-tagged).
    let search_key = format!(
        "{}:{}",
        if package_sources::has_enabled(&app) { "src-3.2.0" } else { "web" },
        query.to_lowercase()
    );
    let search_path = cache_root(&app).ok().map(|root| {
        root.join("search")
            .join(format!("{}.json", sha256_hex(search_key.as_bytes())))
    });
    if let Some(path) = search_path.as_ref() {
        if let Some(cached) = read_cache_json::<Vec<Game>>(path, 15 * 60) {
            return Ok(cached);
        }
    }
    let mut merged = Vec::new();
    if package_sources::has_enabled(&app) {
        let titles = package_sources::search(&app, query, 30).await?;
        merged.extend(titles.into_iter().map(|title| Game {
            title_id: title.title_id,
            name: title.name,
            region: if title.region.is_empty() || title.region == "?" {
                "UNK".into()
            } else {
                title.region
            },
            icon: title.icon,
            packages: Vec::new(),
            source_id: title.source_id,
            source_name: title.source_name,
            source_version: title.source_version,
        }));
        let mut seen = std::collections::HashSet::new();
        merged.retain(|game| seen.insert(format!("{}:{}", game.title_id, game.region)));
        return if merged.is_empty() {
            Err("No titles matched in enabled Package Sources.".into())
        } else {
            if let Some(path) = search_path.as_ref() {
                let _ = write_cache_json(path, &merged);
            }
            Ok(merged)
        };
    }
    let resolver_base = state.settings.lock().unwrap().resolver_base_url.clone();
    if !resolver_base.is_empty() {
        for endpoint in resolver_api_urls(&resolver_base, "games/search") {
            let Ok(mut url) = Url::parse(&endpoint) else {
                continue;
            };
            url.query_pairs_mut()
                .append_pair("q", query)
                .append_pair("limit", "30");
            let Ok(response) = state.http.get(url).send().await else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(value) = response.json::<Value>().await else {
                continue;
            };
            if let Ok(games) = search_results(&value) {
                merged.extend(games);
                break;
            }
        }
    }
    merged.extend(patch_site_search(&state.http, "https://prosperopatches.com", query).await);
    merged.extend(patch_site_search(&state.http, "https://orbispatches.com", query).await);
    let mut seen = std::collections::HashSet::new();
    merged.retain(|game| seen.insert(format!("{}:{}", game.title_id, game.region)));
    if merged.is_empty() {
        Err("No matching PS5 or PS4 titles were returned".into())
    } else {
        if let Some(path) = search_path.as_ref() {
            let _ = write_cache_json(path, &merged);
        }
        Ok(merged)
    }
}

#[tauri::command]
async fn load_catalog(
    app: AppHandle,
    state: State<'_, AppState>,
    refresh: Option<bool>,
) -> Result<GameCatalog, String> {
    let cache_path = cache_root(&app).ok().map(|root| root.join("catalog-v3.json"));
    if !refresh.unwrap_or(false) {
        if let Some(path) = cache_path.as_ref() {
            if let Some(mut catalog) = read_cache_json::<GameCatalog>(path, 30 * 60) {
                catalog.cached = true;
                return Ok(catalog);
            }
        }
    }
    if package_sources::has_enabled(&app) {
        match package_sources::search(&app, "", 40).await {
            Ok(titles) if !titles.is_empty() => {
                let games = titles
                    .into_iter()
                    .map(|title| Game {
                        title_id: title.title_id,
                        name: title.name,
                        region: title.region,
                        icon: title.icon,
                        packages: Vec::new(),
                        source_id: title.source_id,
                        source_name: title.source_name,
                        source_version: title.source_version,
                    })
                    .collect::<Vec<_>>();
                let catalog = GameCatalog {
                    sections: vec![CatalogSection {
                        id: "package_sources".into(),
                        title: "Latest titles".into(),
                        games,
                    }],
                    cached: false,
                    source: "Package Sources".into(),
                };
                if let Some(path) = cache_path.as_ref() {
                    let _ = write_cache_json(path, &catalog);
                }
                return Ok(catalog);
            }
            Ok(_) => {}
            Err(error) => {
                if let Some(path) = cache_path.as_ref() {
                    if let Some(mut catalog) = read_cache_json::<GameCatalog>(path, 7 * 24 * 60 * 60)
                    {
                        catalog.cached = true;
                        return Ok(catalog);
                    }
                }
                return Err(error);
            }
        }
    }
    let base = state.settings.lock().unwrap().resolver_base_url.clone();
    let mut last_error = "Resolver catalog did not return a usable response".to_owned();
    if !base.is_empty() {
        for endpoint in resolver_api_urls(&base, "games/catalog") {
            let Ok(mut url) = Url::parse(&endpoint) else {
                last_error = "Resolver catalog URL is invalid".into();
                continue;
            };
            url.query_pairs_mut()
                .append_pair("supported", "true")
                .append_pair("limit", "24");
            if refresh.unwrap_or(false) {
                url.query_pairs_mut().append_pair("refresh", "true");
            }
            let response = match state.http.get(url).send().await {
                Ok(response) => response,
                Err(_) => {
                    last_error = "Resolver catalog request failed".into();
                    continue;
                }
            };
            if !response.status().is_success() {
                last_error = format!("Resolver catalog returned HTTP {}", response.status());
                continue;
            }
            let value: Value = match response.json().await {
                Ok(value) => value,
                Err(_) => {
                    last_error = "Resolver catalog returned invalid JSON".into();
                    continue;
                }
            };
            match catalog_from_value(&value) {
                Ok(catalog) => {
                    if let Some(path) = cache_path.as_ref() {
                        let _ = write_cache_json(path, &catalog);
                    }
                    return Ok(catalog);
                }
                Err(error) => last_error = error,
            }
        }
    }

    let mut sections = Vec::new();
    // B5: fallbacks run concurrently with a hard per-source cap (was: sequential, unbounded).
    let http = state.http.clone();
    let settled = join_all(
        [
            (
                "https://prosperopatches.com",
                "recently-updated",
                "ps5_recently_updated",
                "PS5 Recently Updated",
            ),
            (
                "https://prosperopatches.com",
                "recently-added",
                "ps5_recently_added",
                "PS5 Recently Added",
            ),
            (
                "https://prosperopatches.com",
                "recently-ac",
                "ps5_recent_content",
                "PS5 Additional Content",
            ),
            (
                "https://orbispatches.com",
                "recently-updated",
                "ps4_recently_updated",
                "PS4 Recently Updated",
            ),
            (
                "https://orbispatches.com",
                "recently-added",
                "ps4_recently_added",
                "PS4 Recently Added",
            ),
        ]
        .into_iter()
        .map(|(base, action, id, title)| {
            let http = http.clone();
            async move {
                let outcome = tokio::time::timeout(
                    Duration::from_secs(12),
                    async {
                        let response = http
                            .post(format!("{base}/api/internal/loadtitletiles"))
                            .header(reqwest::header::REFERER, format!("{base}/"))
                            .header(
                                reqwest::header::CONTENT_TYPE,
                                "application/x-www-form-urlencoded; charset=UTF-8",
                            )
                            .body(format!(r#"{{"offset":0,"action":"{action}"}}"#))
                            .send()
                            .await
                            .map_err(|_| ())?;
                        if !response.status().is_success() {
                            return Err(());
                        }
                        let value: Value = response.json().await.map_err(|_| ())?;
                        Ok::<Vec<Game>, ()>(
                            value
                                .get("titles")
                                .and_then(Value::as_array)
                                .map(|items| {
                                    items.iter().filter_map(game_from_value).collect::<Vec<_>>()
                                })
                                .unwrap_or_default(),
                        )
                    },
                )
                .await;
                (id, title, outcome)
            }
        }),
    )
    .await;
    for (id, title, outcome) in settled {
        if let Ok(Ok(games)) = outcome {
            if !games.is_empty() {
                sections.push(CatalogSection {
                    id: id.into(),
                    title: title.into(),
                    games,
                });
            }
        }
    }
    if sections.is_empty() {
        if let Some(path) = cache_path.as_ref() {
            if let Some(mut catalog) = read_cache_json::<GameCatalog>(path, 7 * 24 * 60 * 60) {
                catalog.cached = true;
                return Ok(catalog);
            }
        }
        return Err(if base.is_empty() {
            "The live catalog is unavailable. Add your Resolver URL in Settings and retry.".into()
        } else {
            format!("{last_error}; Prospero and Orbis Patches fallbacks also failed")
        });
    }
    let catalog = GameCatalog {
        sections,
        cached: false,
        source: "Prospero Patches + Orbis Patches".into(),
    };
    if let Some(path) = cache_path.as_ref() {
        let _ = write_cache_json(path, &catalog);
    }
    Ok(catalog)
}

// B6: bound the cover cache; oldest .bin/.json pairs go first.
fn trim_covers(dir: &Path) {
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    if let Ok(listing) = std::fs::read_dir(dir) {
        for entry in listing.flatten() {
            let path = entry.path();
            if !path.extension().is_some_and(|ext| ext == "bin") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let age = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                total = total.saturating_add(meta.len());
                entries.push((age, path));
            }
        }
    }
    if total <= MAX_COVER_CACHE {
        return;
    }
    entries.sort_by_key(|(age, _)| *age);
    for (_, bin) in entries {
        if total <= MAX_COVER_CACHE {
            break;
        }
        let size = std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&bin);
        let _ = std::fs::remove_file(bin.with_extension("json"));
        total = total.saturating_sub(size);
    }
}

#[tauri::command]
async fn fetch_cover(app: AppHandle, state: State<'_, AppState>, url: String) -> Result<String, String> {    if !valid_http(&url) {
        return Err("Cover URL is invalid".into());
    }
    let hash = sha256_hex(url.as_bytes());
    let cover_dir = cache_root(&app).map(|root| root.join("covers"));
    if let Ok(dir) = &cover_dir {
        let meta = dir.join(format!("{hash}.json"));
        let bin = dir.join(format!("{hash}.bin"));
        if let (Ok(meta), Ok(bytes)) = (std::fs::read(&meta), std::fs::read(&bin)) {
            if let Ok(value) = serde_json::from_slice::<Value>(&meta) {
                if let Some(mime) = value.get("mime").and_then(Value::as_str) {
                    if !bytes.is_empty() && bytes.len() as u64 <= MAX_COVER_BYTES {
                        return Ok(format!("data:{mime};base64,{}", BASE64.encode(bytes)));
                    }
                }
            }
        }
    }
    let response = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|_| network_error("Cover request"))?;
    if !response.status().is_success() {
        return Err(format!("Cover service returned HTTP {}", response.status()));
    }
    if response.content_length().unwrap_or(0) > MAX_COVER_BYTES {
        return Err("Cover image is larger than 10 MiB".into());
    }
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/webp")
        .to_owned();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| network_error("Cover response"))?;
    if bytes.len() as u64 > MAX_COVER_BYTES {
        return Err("Cover image is larger than 10 MiB".into());
    }
    if let Ok(dir) = &cover_dir {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(format!("{hash}.bin")), &bytes);
        let _ = std::fs::write(
            dir.join(format!("{hash}.json")),
            serde_json::to_vec(&json!({"mime": mime, "url": url})).unwrap_or_default(),
        );
        trim_covers(dir);
    }
    Ok(format!("data:{mime};base64,{}", BASE64.encode(bytes)))
}
#[tauri::command]
async fn resolve_packages(
    app: AppHandle,
    state: State<'_, AppState>,
    game_title_id: String,
    game_name: Option<String>,
    game_region: Option<String>,
) -> Result<Vec<Package>, String> {
    if !title_id(&game_title_id) {
        return Err("Only CUSA and PPSA title IDs are supported".into());
    }
    let cache_path = cache_root(&app)
        .ok()
        .map(|root| root.join(PACKAGES_CACHE).join(format!("{game_title_id}.json")));
    // B3: serialize concurrent resolves per title; the waiter re-checks fresh cache below.
    let slot = {
        let mut pending = state.resolving.lock().await;
        pending
            .entry(game_title_id.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };
    let _serial = slot.lock().await;
    if package_sources::has_enabled(&app) {
        // B1: serve fresh cache instantly; network only on miss. Error path below
        // still falls back to older entries.
        if let Some(path) = cache_path.as_ref() {
            if let Some(cached) = read_cache_json::<Vec<Package>>(path, 60 * 60) {
                return Ok(cached);
            }
        }
        return match package_sources::resolve(
            &app,
            &game_title_id,
            game_name.as_deref().unwrap_or(&game_title_id),
            game_region.as_deref().unwrap_or(""),
        )
        .await
        {
            Ok(packages) => {
                let mapped = packages
                    .into_iter()
                    .map(|package| Package {
                    kind: package.kind,
                    label: package.label,
                    url: package.url,
                    access_type: package.access_type,
                    source_id: package.source_id,
                    source_name: package.source_name,
                    source_version: package.source_version,
                    candidate_id: package.candidate_id,
                    group_id: package.group_id,
                    hoster: package.hoster,
                    version: package.version,
                    firmware: package.firmware,
                    source_page_url: package.source_page_url,
                    expected_size: package.expected_size,
                    expected_sha256: package.expected_sha256,
                    expected_content_id: package.expected_content_id,
                    archive_set_id: package.archive_set_id,
                    archive_part_number: package.archive_part_number,
                    archive_part_count: package.archive_part_count,
                    archive_file_name: package.archive_file_name,
                    archive_format_hint: package.archive_format_hint,
                    archive_password: package.archive_password,
                    mirror_id: package.mirror_id,
                    intermediate_url: package.intermediate_url,
                    referer: package.referer,
                    diagnostics: package.diagnostics,
                    })
                    .collect();
                if let Some(path) = cache_path.as_ref() {
                    let _ = write_cache_json(path, &mapped);
                }
                Ok(mapped)
            }
            Err(error) => {
                if let Some(path) = cache_path.as_ref() {
                    if let Some(cached) = read_cache_json::<Vec<Package>>(path, 24 * 60 * 60) {
                        return Ok(cached);
                    }
                }
                Err(error)
            }
        };
    }
    let base = state.settings.lock().unwrap().resolver_base_url.clone();
    if base.is_empty() {
        if let Some(path) = cache_path.as_ref() {
            if let Some(cached) = read_cache_json::<Vec<Package>>(path, 24 * 60 * 60) {
                return Ok(cached);
            }
        }
        return Err("Install and enable a Package Source in Settings".into());
    }
    let mut last_error = "Resolver did not return a usable response".to_owned();
    for endpoint in resolver_download_urls(&base, &game_title_id) {
        let response = match state.http.get(&endpoint).send().await {
            Ok(response) => response,
            Err(_) => {
                last_error = "Resolver request failed".into();
                continue;
            }
        };
        if !response.status().is_success() {
            last_error = format!("Resolver returned HTTP {}", response.status());
            continue;
        }
        let value: Value = match response.json().await {
            Ok(value) => value,
            Err(_) => {
                last_error = "Resolver returned invalid JSON".into();
                continue;
            }
        };
        match packages_from_value(&value) {
            Ok(packages) => {
                if let Some(path) = cache_path.as_ref() {
                    let _ = write_cache_json(path, &packages);
                }
                return Ok(packages);
            }
            Err(error) => last_error = error,
        }
    }
    if let Some(path) = cache_path.as_ref() {
        if let Some(cached) = read_cache_json::<Vec<Package>>(path, 24 * 60 * 60) {
            return Ok(cached);
        }
    }
    Err(last_error)
}
#[tauri::command]
async fn get_game_details(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    title_id: Option<String>,
) -> Result<Value, String> {
    let query = name.trim();
    if query.is_empty() && title_id.as_deref().unwrap_or("").is_empty() {
        return Ok(json!({"enabled":false}));
    }
    let cache_key = title_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(query);
    let cache_path = cache_root(&app)
        .ok()
        .map(|root| root.join("metadata").join(format!("{}.json", sha256_hex(cache_key.as_bytes()))));
    if let Some(path) = cache_path.as_ref() {
        if let Some(cached) = read_cache_json::<Value>(path, 24 * 60 * 60) {
            return Ok(cached);
        }
    }
    let id = title_id
        .as_deref()
        .map(str::to_ascii_uppercase)
        .filter(|value| {
            value.len() == 9
                && (value.starts_with("CUSA") || value.starts_with("PPSA"))
                && value.as_bytes()[4..].iter().all(|byte| byte.is_ascii_digit())
        });
    let patch_base = id.as_ref().map(|value| {
        if value.starts_with("PPSA") {
            "https://prosperopatches.com"
        } else {
            "https://orbispatches.com"
        }
    });
    let mut game = json!({"name": query});
    let mut attribution = "Prospero Patches / Orbis Patches".to_owned();
    if let (Some(id), Some(base)) = (id.as_ref(), patch_base) {
        if let Ok(Ok(response)) = tokio::time::timeout(
            Duration::from_secs(8),
            state
                .http
                .get(format!("{base}/{id}"))
                .header(reqwest::header::REFERER, format!("{base}/"))
                .header(
                    reqwest::header::USER_AGENT,
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) GameSearch/0.2",
                )
                .send(),
        )
        .await
        {
            if response.status().is_success() {
                if let Ok(body) = response.text().await {
                    if let Some(title) = meta_content(&body, "og:title") {
                        game["name"] = json!(title);
                    }
                    if let Some(description) = meta_content(&body, "og:description") {
                        game["description"] = json!(description);
                    }
                    if let Some(image) = meta_content(&body, "og:image") {
                        game["background_image"] = json!(image);
                    }
                    if let Some(firmware) = scrape_labeled(
                        &body,
                        &["Firmware", "Required Firmware", "Min Firmware", "FW"],
                    ) {
                        game["firmware"] = json!(firmware);
                    }
                    if let Some(version) =
                        scrape_labeled(&body, &["Version", "App Version", "Game Version"])
                    {
                        game["version"] = json!(version);
                    }
                    if let Some(released) =
                        scrape_labeled(&body, &["Released", "Release Date", "Release"])
                    {
                        game["released"] = json!(released);
                    }
                    if let Some(size) = scrape_labeled(&body, &["Size", "PKG Size", "Download Size"])
                    {
                        game["size"] = json!(size);
                    }
                }
            }
        }
    }
    let metacritic = scrape_metacritic(&state.http, query).await;
    if let Some(score) = metacritic.as_ref().and_then(|value| value.get("metacritic")) {
        game["metacritic"] = score.clone();
        attribution = "Metacritic (best effort) + Prospero Patches / Orbis Patches".into();
    }
    if let Some(description) = metacritic
        .as_ref()
        .and_then(|value| value.get("description"))
        .cloned()
    {
        if game.get("description").and_then(Value::as_str).unwrap_or("").is_empty() {
            game["description"] = description;
        }
    }
    if let Some(image) = metacritic
        .as_ref()
        .and_then(|value| value.get("background_image"))
        .cloned()
    {
        if game.get("background_image").and_then(Value::as_str).unwrap_or("").is_empty() {
            game["background_image"] = image;
        }
    }
    let payload = json!({"enabled":true,"attribution":attribution,"game":game});
    if let Some(path) = cache_path.as_ref() {
        let _ = write_cache_json(path, &payload);
    }
    Ok(payload)
}

fn scrape_labeled(html: &str, labels: &[&str]) -> Option<String> {
    let text = Regex::new(r"(?s)<[^>]+>")
        .ok()?
        .replace_all(html, " ")
        .into_owned();
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for label in labels {
        let pattern = format!(
            r"(?i){}[:\s]+([A-Za-z0-9.+() _-]{{1,40}})",
            regex::escape(label)
        );
        if let Some(value) = Regex::new(&pattern)
            .ok()
            .and_then(|regex| regex.captures(&collapsed))
            .and_then(|capture| capture.get(1))
            .map(|value| value.as_str().trim().to_owned())
            .filter(|value| !value.is_empty() && value.len() < 48)
        {
            return Some(value);
        }
    }
    None
}

fn meta_content(html: &str, property: &str) -> Option<String> {
    let pattern = format!(
        r#"(?is)<meta[^>]+(?:property|name)=[\"']{}[\"'][^>]+content=[\"']([^\"']+)"#,
        regex::escape(property)
    );
    Regex::new(&pattern)
        .ok()?
        .captures(html)
        .and_then(|capture| capture.get(1))
        .map(|value| html_unescape(value.as_str()))
}

fn html_unescape(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

async fn scrape_metacritic(http: &Client, query: &str) -> Option<Value> {
    let browser = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
    let search = tokio::time::timeout(
        Duration::from_secs(8),
        http.get("https://www.metacritic.com/search")
            .query(&[("q", query)])
            .header(reqwest::header::USER_AGENT, browser)
            .send(),
    )
    .await
    .ok()?
    .ok()?;
    if !search.status().is_success() || search.content_length().unwrap_or(0) > 2 * 1024 * 1024 {
        return None;
    }
    let body = search.text().await.ok()?;
    let score = Regex::new(r#"(?is)metascore_w[^>]*>\s*(\d{2,3})"#)
        .ok()
        .and_then(|regex| regex.captures(&body))
        .and_then(|capture| capture.get(1))
        .and_then(|value| value.as_str().parse::<u32>().ok());
    let image = meta_content(&body, "og:image");
    let description = meta_content(&body, "og:description");
    let href = Regex::new(r#"href="(https://www\.metacritic\.com/game/[^"]+)""#)
        .ok()
        .and_then(|regex| regex.captures(&body))
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str().to_owned());
    let mut game = json!({});
    if let Some(score) = score {
        game["metacritic"] = json!(score);
    }
    if let Some(description) = description {
        game["description"] = json!(description);
    }
    if let Some(image) = image {
        game["background_image"] = json!(image);
    }
    if score.is_none() {
        if let Some(href) = href {
            if let Ok(Ok(page)) = tokio::time::timeout(
                Duration::from_secs(8),
                http.get(&href)
                    .header(reqwest::header::USER_AGENT, browser)
                    .send(),
            )
            .await
            {
                if page.status().is_success() {
                    if let Ok(html) = page.text().await {
                        if let Some(score) = Regex::new(r#"(?is)metascore_w[^>]*>\s*(\d{2,3})"#)
                            .ok()
                            .and_then(|regex| regex.captures(&html))
                            .and_then(|capture| capture.get(1))
                            .and_then(|value| value.as_str().parse::<u32>().ok())
                        {
                            game["metacritic"] = json!(score);
                        }
                        if let Some(description) = meta_content(&html, "og:description") {
                            game["description"] = json!(description);
                        }
                        if let Some(image) = meta_content(&html, "og:image") {
                            game["background_image"] = json!(image);
                        }
                    }
                }
            }
        }
    }
    Some(game)
}

async fn frame(stream: &mut TcpStream, cmd: u8, body: &[u8]) -> Result<(u8, Vec<u8>), String> {
    let seconds = if cmd == 0x52 { 180 } else { 90 };
    tokio::time::timeout(Duration::from_secs(seconds), frame_io(stream, cmd, body))
        .await
        .map_err(|_| format!("Receiver command 0x{cmd:02x} timed out"))?
}
async fn frame_io(stream: &mut TcpStream, cmd: u8, body: &[u8]) -> Result<(u8, Vec<u8>), String> {
    if body.len() > CHUNK {
        return Err("frame exceeds 8 MiB".into());
    }
    let mut h = vec![cmd];
    h.extend((body.len() as u32).to_le_bytes());
    stream.write_all(&h).await.map_err(redact)?;
    stream.write_all(body).await.map_err(redact)?;
    let mut rh = [0; 5];
    stream.read_exact(&mut rh).await.map_err(redact)?;
    let n = u32::from_le_bytes(rh[1..5].try_into().unwrap()) as usize;
    if n > CHUNK {
        return Err("receiver frame exceeds 8 MiB".into());
    }
    let mut b = vec![0; n];
    stream.read_exact(&mut b).await.map_err(redact)?;
    Ok((rh[0], b))
}
async fn ping(s: &Settings) -> Result<(), String> {
    let mut x = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((s.ps5_host.as_str(), s.ps5_port)),
    )
    .await
    .map_err(|_| "PS5 connection timed out".to_string())?
    .map_err(redact)?;
    let (r, b) = tokio::time::timeout(Duration::from_secs(10), frame(&mut x, 1, &[]))
        .await.map_err(|_| "Receiver PING timed out".to_string())??;
    if r == 1 && b == b"SSPI" {
        Ok(())
    } else {
        Err("Unexpected receiver PING response".into())
    }
}

/// Connect with a few retries; a dead/refusing receiver becomes a clear
/// reload-the-ELF message instead of a raw OS 10061 at whatever stage called.
async fn connect_ps5(s: &Settings, what: &str) -> Result<TcpStream, String> {
    let mut last = String::new();
    for attempt in 0..3u32 {
        match tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect((s.ps5_host.as_str(), s.ps5_port)),
        )
        .await
        {
            Ok(Ok(tcp)) => {
                tcp.set_nodelay(true).ok();
                return Ok(tcp);
            }
            Ok(Err(e)) => last = redact(e),
            Err(_) => last = format!("{what} timed out"),
        }
        sleep(Duration::from_secs(1 + u64::from(attempt))).await;
    }
    Err(format!("PS5 receiver is not answering ({what}; last: {last}). Reload the ELF on the console and retry."))
}
fn remote(id: Option<&str>) -> String {
    format!(
        "/user/data/tmp/upload_{}_{}.pkg",
        id.filter(|x| title_id(x)).unwrap_or("PKG"),
        Uuid::new_v4().simple()
    )
}
fn title_from_path(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_uppercase)
        .find(|x| title_id(x))
}
async fn create_remote_dir(s: &Settings, path: &str) -> Result<(), String> {
    let mut tcp = connect_ps5(s, "CREATE_DIR").await?;
    tcp.set_nodelay(true).ok();
    let (r, body) = tokio::time::timeout(Duration::from_secs(20), frame(&mut tcp, 0x04, path.as_bytes()))
        .await
        .map_err(|_| "CREATE_DIR timed out".to_string())??;
    if r != 1 {
        Err(format!(
            "CREATE_DIR rejected: {}",
            String::from_utf8_lossy(&body)
        ))
    } else {
        Ok(())
    }
}

async fn send_file(
    app: &AppHandle,
    s: &Settings,
    path: &Path,
    remote: &str,
    job: &str,
    tx: &watch::Receiver<bool>,
    message: &str,
    overall_done: u64,
    overall_total: u64,
    job_bytes: Option<Arc<AtomicU64>>,
) -> Result<(), String> {
    let n = fs::metadata(path).await.map_err(redact)?.len();
    let overall_total = overall_total.max(n + overall_done).max(1);
    emit(
        app,
        Progress {
            job_id: job.into(),
            stage: "uploading".into(),
            progress: (overall_done as f64 / overall_total as f64).min(0.99),
            bytes_done: overall_done,
            bytes_total: overall_total,
            speed_bps: 0.,
            eta_seconds: None,
            message: message.into(),
            ..Default::default()
        },
    );
    let mut settings = s.clone();
    for attempt in 0..4u32 {
        if *tx.borrow() { return Err("cancelled".into()); }
        match send_file_once(app, &settings, path, remote, job, tx, message,
            overall_done, overall_total, job_bytes.clone()).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt < 3 && retryable_upload_error(&error) => {
                settings.upload_lanes = 1;
                emit(app, Progress { job_id: job.into(), stage: "uploading".into(),
                    message: format!("Connection interrupted; retrying file ({}/3) with one lane", attempt + 1),
                    ..Default::default() });
                let mut cancel = tx.clone();
                tokio::select! {
                    _ = sleep(Duration::from_millis(700 * (1 << attempt))) => {},
                    _ = cancel.changed() => return Err("cancelled".into()),
                }
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

async fn send_file_once(
    app: &AppHandle,
    s: &Settings,
    path: &Path,
    remote: &str,
    job: &str,
    tx: &watch::Receiver<bool>,
    message: &str,
    overall_done: u64,
    overall_total: u64,
    job_bytes: Option<Arc<AtomicU64>>,
) -> Result<(), String> {
    let n = fs::metadata(path).await.map_err(redact)?.len();
    let overall_total = overall_total.max(n + overall_done).max(1);
    let dump = remote.starts_with("/data/homebrew/");
    let requested = if s.transfer_mode.eq_ignore_ascii_case("max") {
        // 1 Gb LAN: up to 12 lanes per file; dump files cap lower so 8 parallel
        // files stay well under the receiver's connection budget.
        s.upload_lanes.clamp(1, 12)
    } else {
        s.upload_lanes.clamp(1, 4)
    };
    let lanes = if n < 48 * 1024 * 1024 {
        1
    } else if dump {
        requested.min(6)
    } else {
        requested
    };
    let mut sockets = Vec::new();
    for (off, len) in split_ranges(n, u64::from(lanes)) {
        let mut tcp = connect_ps5(s, "lane START_UPLOAD").await?;
        tcp.set_nodelay(true).ok();
        let mut b = remote.as_bytes().to_vec();
        b.push(0);
        b.extend(n.to_le_bytes());
        b.extend(off.to_le_bytes());
        b.extend(len.to_le_bytes());
        let (r, body) = tokio::time::timeout(Duration::from_secs(90), frame(&mut tcp, 0x10, &b))
            .await
            .map_err(|_| "START_UPLOAD timed out".to_string())??;
        if r != 4 {
            return Err(format!(
                "START_UPLOAD rejected: {}",
                String::from_utf8_lossy(&body)
            ));
        }
        sockets.push((tcp, off, len));
    }
    let sent = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let source = path.to_path_buf();
    let mut copies = Vec::new();
    for (mut tcp, off, len) in sockets {
        let app = app.clone();
        let job = job.to_string();
        let source = source.clone();
        let sent = sent.clone();
        let cancel = tx.clone();
        let message = message.to_owned();
        let overall_done = overall_done;
        let overall_total = overall_total;
        let job_bytes = job_bytes.clone();
        // U5: rate comes from the shared aggregate when present, not one file's ramp.
        let shared_rate = job_bytes.clone();
        let started_at = started;
        copies.push(tauri::async_runtime::spawn(async move {
            use tokio::io::AsyncSeekExt;
            let mut f = fs::File::open(source).await.map_err(redact)?;
            f.seek(std::io::SeekFrom::Start(off))
                .await
                .map_err(redact)?;
            let mut remain = len;
            let mut buffer = vec![0; len.min(CHUNK as u64) as usize];
            while remain > 0 {
                if *cancel.borrow() {
                    return Err("cancelled".to_string());
                }
                transfer_checkpoint(&app, &job, &cancel).await?;
                let chunk_len = remain.min(buffer.len() as u64) as usize;
                let chunk = &mut buffer[..chunk_len];
                f.read_exact(chunk).await.map_err(redact)?;
                let (r, body) = frame(&mut tcp, 0x11, chunk).await?;
                if r != 1 {
                    return Err(format!(
                        "UPLOAD_CHUNK rejected: {}",
                        String::from_utf8_lossy(&body)
                    ));
                }
                remain -= chunk_len as u64;
                let added = chunk_len as u64;
                let done = sent.fetch_add(added, Ordering::Relaxed) + added;
                let all = job_bytes
                    .as_ref()
                    .map(|bytes| bytes.fetch_add(added, Ordering::Relaxed) + added)
                    .unwrap_or(overall_done + done);
                let speed = match shared_rate.as_ref() {
                    Some(shared) => {
                        shared.load(Ordering::Relaxed) as f64
                            / started_at.elapsed().as_secs_f64().max(0.01)
                    }
                    None => done as f64 / started.elapsed().as_secs_f64().max(0.01),
                };
                emit(
                    &app,
                    Progress {
                        job_id: job.clone(),
                        stage: "uploading".into(),
                        progress: if all >= overall_total {
                            1.
                        } else {
                            (all as f64 / overall_total as f64).min(0.99)
                        },
                        bytes_done: all,
                        bytes_total: overall_total,
                        speed_bps: speed,
                        eta_seconds: Some(
                            ((overall_total.saturating_sub(all)) as f64 / speed.max(0.01)) as u64,
                        ),
                        message: message.clone(),
                        ..Default::default()
                    },
                );
            }
            let (r, body) = frame(&mut tcp, 0x12, &[]).await?;
            if r != 1 {
                return Err(format!(
                    "END_UPLOAD rejected: {}",
                    String::from_utf8_lossy(&body)
                ));
            }
            Ok::<(), String>(())
        }));
    }
    let mut failed: Option<String> = None;
    for result in join_all(copies).await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                failed = Some(e);
                break;
            }
            Err(e) => {
                failed = Some(redact(e));
                break;
            }
        }
    }
    // VERIFY releases the receiver's file descriptor and transfer records for each
    // dump file before its upload permit can be reused for the next one.
    if failed.is_none() {
        failed = verify_uploaded_file(s, remote, n).await.err();
    }
    // A retry re-sends the whole file, so take this attempt's bytes back out of
    // the shared set counter instead of double-counting them.
    if let Some(e) = failed {
        if let Some(shared) = job_bytes.as_ref() {
            shared.fetch_sub(sent.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        return Err(e);
    }
    Ok(())
}

fn verified_upload_size(code: u8, body: &[u8], expected: u64) -> Result<(), String> {
    let text = String::from_utf8_lossy(body);
    if code != 1 {
        return Err(format!("Receiver verification failed: {text}"));
    }
    let size = text.strip_prefix("OK ")
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|value| value.get("size").and_then(Value::as_u64));
    if size != Some(expected) {
        return Err(format!("Receiver verified size {size:?}, expected {expected}"));
    }
    Ok(())
}

async fn verify_uploaded_file(s: &Settings, remote: &str, expected: u64) -> Result<(), String> {
    let mut stream = connect_ps5(s, "uploaded file verify").await?;
    let (code, body) = frame(&mut stream, 0x55, remote.as_bytes()).await?;
    verified_upload_size(code, &body, expected)
}

fn validate_mountable_dump(root: &Path) -> Result<(), String> {
    if !root.join("eboot.bin").is_file() {
        return Err(format!("Dump is missing eboot.bin at {}", root.display()));
    }
    if !root.join("sce_sys").is_dir() {
        return Err(format!("{} has no sce_sys metadata. An eboot/fakelib backport is an overlay, not a standalone game dump. Apply it to its matching complete base dump on the PC, then select that complete dump. No files were uploaded.", root.display()));
    }
    if !root.join("sce_sys/param.json").is_file() {
        return Err(format!("Dump is missing sce_sys/param.json at {}; select a complete PS5 dump before uploading", root.display()));
    }
    Ok(())
}

async fn upload_dump(
    app: &AppHandle,
    s: &Settings,
    root: &Path,
    job: &str,
    title: Option<&str>,
    kind: &str,
    tx: &watch::Receiver<bool>,
) -> Result<(), String> {
    validate_mountable_dump(root)?;
    let _delivery = console_delivery_slot(tx).await?;
    test_ps5(s.ps5_host.clone(), s.ps5_port).await?;
    let title = dump_title_id(root)
        .or_else(|| title.map(str::to_ascii_uppercase).filter(|id| title_id(id)));
    let remote_root = shadowmount_dir(title.as_deref(), kind)?;
    let files = dump_files(root)?;
    if !files
        .iter()
        .any(|(_, relative)| relative.eq_ignore_ascii_case("eboot.bin"))
    {
        return Err(format!(
            "Dump is missing eboot.bin at the title root (looked in {})",
            root.display()
        ));
    }
    let sizes: Vec<u64> = files
        .iter()
        .map(|(path, _)| std::fs::metadata(path).map(|m| m.len()).map_err(redact))
        .collect::<Result<_, _>>()?;
    let overall_total = sizes.iter().copied().sum::<u64>().max(1);
    let mut control = connect_ps5(s, "dump preflight").await?;
    let (preflight, body) = frame(&mut control, 0x56, &[]).await?;
    require_preflight(preflight).map_err(|error| {
        format!("{error}: {}", String::from_utf8_lossy(&body))
    })?;
    if let Some(free) = String::from_utf8_lossy(&body)
        .split("\"free\":")
        .nth(1)
        .and_then(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .ok()
        })
    {
        if free > 0 && free < overall_total.saturating_add(1 << 30) {
            return Err(format!(
                "Dump needs {:.1} GB free on the PS5 and /data has {:.1} GB.",
                overall_total as f64 / 1_073_741_824.0,
                free as f64 / 1_073_741_824.0
            ));
        }
    }
    create_remote_dir(s, &remote_root).await?;
    let file_count = files.len();
    let transferred = Arc::new(AtomicU64::new(0));
    // Cap total concurrent sockets (~32) so the receiver isn't drowned right
    // before mount: fewer parallel files when per-file lanes are high.
    let max_lanes = if s.transfer_mode.eq_ignore_ascii_case("max") {
        s.upload_lanes.clamp(1, 12)
    } else {
        s.upload_lanes.clamp(1, 4)
    };
    let dump_lane_cap = max_lanes.min(6).max(1) as usize;
    let dump_files_at_once = (if s.transfer_mode.eq_ignore_ascii_case("max") {
        8
    } else {
        4
    })
    .min((32 / dump_lane_cap).max(1));
    let sem = Arc::new(Semaphore::new(dump_files_at_once));
    let mut tasks = Vec::new();
    for (index, ((path, relative), _size)) in files.into_iter().zip(sizes).enumerate() {
        let permit = sem.clone();
        let app = app.clone();
        let settings = s.clone();
        let job = job.to_string();
        let cancel = tx.clone();
        let transferred = transferred.clone();
        let remote = format!("{remote_root}/{relative}");
        let file_label = relative.clone();
        // U2: filename appears on start/finish lines only; byte frames carry the
        // aggregate message so concurrent tasks stop strobing one line.
        let start_message = format!("Uploading {}/{file_count} · {file_label}", index + 1);
        let lane_message = format!("Uploading dump · {}/{} files", index + 1, file_count);
        let done_message = format!("Uploaded {}/{file_count} · {file_label}", index + 1);
        tasks.push(tauri::async_runtime::spawn(async move {
            let _permit = permit.acquire().await.map_err(redact)?;
            if *cancel.borrow() {
                return Err("cancelled".into());
            }
            emit(
                &app,
                Progress {
                    job_id: job.clone(),
                    stage: "uploading".into(),
                    progress: (transferred.load(Ordering::Relaxed) as f64
                        / overall_total as f64)
                        .min(0.99),
                    bytes_done: transferred.load(Ordering::Relaxed),
                    bytes_total: overall_total,
                    speed_bps: 0.,
                    eta_seconds: None,
                    message: start_message,
                    ..Default::default()
                },
            );
            send_file(
                &app,
                &settings,
                &path,
                &remote,
                &job,
                &cancel,
                &lane_message,
                0,
                overall_total,
                Some(transferred.clone()),
            )
            .await?;
            emit(
                &app,
                Progress {
                    job_id: job.clone(),
                    stage: "uploading".into(),
                    progress: (transferred.load(Ordering::Relaxed) as f64
                        / overall_total as f64)
                        .min(0.99),
                    bytes_done: transferred.load(Ordering::Relaxed),
                    bytes_total: overall_total,
                    speed_bps: 0.,
                    eta_seconds: None,
                    message: done_message,
                    ..Default::default()
                },
            );
            Ok::<(), String>(())
        }));
    }
    for result in join_all(tasks).await {
        result.map_err(redact)??;
    }
    // Every file was size-checked, fsynced and released by VERIFY in send_file_once.
    drop(control);
    let overall_done = transferred.load(Ordering::Relaxed);
    // U3: full-size numbers only with a message that says what they mean.
    begin_console_stage(app, job, tx, "mounting").await?;
    emit(
        app,
        Progress {
            job_id: job.into(),
            stage: "mounting".into(),
            progress: 0.99,
            bytes_done: overall_done,
            bytes_total: overall_total,
            speed_bps: 0.,
            eta_seconds: None,
            message: format!(
                "Transferred {:.1} GB · registering {remote_root} with PS5 AppInst",
                overall_total as f64 / 1_073_741_824.0
            ),
            ..Default::default()
        },
    );
    // Pre-mount liveness: never throw a mount at a dead receiver and report a
    // raw 10061. If the ELF died after upload, the files are still on the PS5.
    if ping(s).await.is_err() {
        return Err(format!("Receiver stopped answering after upload. Files are on the PS5 at {remote_root}. Reload the ELF on the console, then mount {remote_root} again."));
    }
    let mut mounted = String::new();
    let mut last_error = String::new();
    // Mount is idempotent (nullfs is unmounted first), so ride out transient
    // backlog/TIME_WAIT storms after dozens of sockets slam shut at once.
    for attempt in 0..6u32 {
        match mount_dump(s, &remote_root).await {
            Ok(text) => {
                mounted = text;
                break;
            }
            Err(error) => {
                // Definitive rejections will never succeed on retry — fail fast
                // instead of burning six attempts against a clear answer.
                if error.contains("missing sce_sys")
                    || error.contains("not found")
                    || error.contains("rejected")
                    || error.contains("invalid")
                    || error.contains("does not match")
                    || error.contains("both /data")
                {
                    return Err(error);
                }
                last_error = error;
                emit(
                    app,
                    Progress {
                        job_id: job.into(),
                        stage: "mounting".into(),
                        progress: 0.99,
                        bytes_done: overall_done,
                        bytes_total: overall_total,
                        speed_bps: 0.,
                        eta_seconds: None,
                        message: format!(
                            "Mount attempt {}/6 for {remote_root}…",
                            attempt + 1
                        ),
                        ..Default::default()
                    },
                );
                sleep(Duration::from_secs(5 + u64::from(attempt) * 5)).await;
                if ping(s).await.is_err() {
                    return Err(format!("Receiver stopped answering during mount. Files are on the PS5 at {remote_root}. Reload the ELF on the console, then mount {remote_root} again."));
                }
            }
        }
    }
    if mounted.is_empty() {
        return Err(format!("Mount kept failing ({last_error}). Files are on the PS5 at {remote_root}. Reload the ELF and mount it again."));
    }
    emit(
        app,
        Progress {
            job_id: job.into(),
            stage: "complete".into(),
            progress: 1.,
            bytes_done: overall_done,
            bytes_total: overall_total,
            speed_bps: 0.,
            eta_seconds: None,
            message: mounted,
            ..Default::default()
        },
    );
    Ok(())
}

async fn mount_dump(s: &Settings, remote_root: &str) -> Result<String, String> {
    let mut tcp = connect_ps5(s, "dump mount").await?;
    tcp.set_nodelay(true).ok();
    let (code, body) = tokio::time::timeout(
        Duration::from_secs(180),
        frame(&mut tcp, 0x52, remote_root.as_bytes()),
    )
    .await
    .map_err(|_| "Dump mount timed out".to_string())??;
    let text = String::from_utf8_lossy(&body).into_owned();
    if code != 1 {
        Err(if text.is_empty() {
            "Dump mount rejected".into()
        } else {
            format!("Dump mount failed: {text}")
        })
    } else {
        Ok(text)
    }
}

async fn upload(
    app: &AppHandle,
    s: &Settings,
    path: &Path,
    job: &str,
    title: Option<&str>,
    tx: &mut watch::Receiver<bool>,
    label: &str,
    announce_complete: bool,
    // U4: position of this package inside a multi-PKG set (0/0 = single file).
    set_offset: u64,
    set_total: u64,
) -> Result<(), String> {
    let _delivery = console_delivery_slot(tx).await?;
    test_ps5(s.ps5_host.clone(), s.ps5_port).await?;
    let n = fs::metadata(path).await.map_err(redact)?.len();
    let total = set_total.max(set_offset + n).max(1);
    let done = set_offset + n;
    let target = remote(title);
    let mut control = connect_ps5(s, "install preflight").await?;
    let (preflight, body) = frame(&mut control, 0x56, &[]).await?;
    require_preflight(preflight).map_err(|error| {
        format!(
            "{error}: {}",
            String::from_utf8_lossy(&body)
        )
    })?;
    send_file(
        app,
        s,
        path,
        &target,
        job,
        tx,
        &format!("Uploading {label} PKG to PS5"),
        0,
        n,
        None,
    )
    .await?;
    begin_console_stage(app, job, tx, "submitting").await?;
    emit(
        app,
        Progress {
            job_id: job.into(),
            stage: "submitting".into(),
            progress: 0.99,
            bytes_done: done,
            bytes_total: total,
            speed_bps: 0.,
            eta_seconds: None,
            // U3: full-size numbers only with a message that says what they mean.
            message: format!(
                "Transferred {:.1} MB · submitting {label} to AppInst",
                n as f64 / 1_048_576.0
            ),
            ..Default::default()
        },
    );
    let mut submit = connect_ps5(s, "install submission").await?;
    if *tx.borrow() { return Err("cancelled".into()); }
    let (_frame_code, v) = frame(&mut submit, 0x50, target.as_bytes()).await?;
    let parsed: Value = serde_json::from_slice(&v).map_err(|_| {
        format!(
            "Install submission failed: {}",
            String::from_utf8_lossy(&v)
        )
    })?;
    let mut cid = accepted_submission(&parsed)?;
    if cid.is_empty() {
        cid = pkg_content_id(path).unwrap_or_default();
    }
    // F3: a blind poll can never observe anything — fail fast with the package named.
    if cid.is_empty() {
        return Err(format!(
            "Install submission returned no content ID for {label}; refusing blind poll. Package: {target}"
        ));
    }
    // F5: carry the content ID in every install message from here on.
    let label = format!("{label} [{cid}]");
    let unconfirmed = |label: &str, cid: &str| {
        format!(
            "AppInst never acknowledged {label} (content id '{cid}'). The package is on the PS5 but was not confirmed installed — check the home menu, then retry."
        )
    };
    let mut saw_work = false;
    let mut idle_ticks = 0u32;
    // F4: longer honest window (5 min); worst case is a slower honest answer.
    for tick in 0..150 {
        if *tx.borrow() {
            return Err("cancelled".into());
        }
        sleep(Duration::from_secs(2)).await;
        let mut status_sock = connect_ps5(s, "install status").await?;
        let (_frame_code, v) = frame(&mut status_sock, 0x51, cid.as_bytes()).await?;
        let parsed = match serde_json::from_slice::<Value>(&v) {
            Ok(value) => value,
            Err(_) => {
                idle_ticks += 1;
                emit(
                    app,
                    Progress {
                        job_id: job.into(),
                        stage: "installing".into(),
                        progress: ((tick + 1) as f64 / 20.).min(0.95),
                        bytes_done: done,
                        bytes_total: total,
                        speed_bps: 0.,
                        eta_seconds: None,
                        message: format!("Waiting for {label} AppInst"),
                        ..Default::default()
                    },
                );
                if idle_ticks >= 45 {
                    // F1/F2: unparseable replies are idle time — never a silent complete.
                    if saw_work {
                        return Err(
                            "Install remains in progress; check receiver status later".into(),
                        );
                    }
                    return Err(unconfirmed(&label, &cid));
                }
                continue;
            }
        };
        match install_decision(&parsed)? {
            InstallDecision::Complete => {
                return finish_pkg_install(app, job, done, total, &label, announce_complete);
            }
            InstallDecision::Installing { status, progress } => {
                if appinst_idle(&status) {
                    idle_ticks += 1;
                    emit(
                        app,
                        Progress {
                            job_id: job.into(),
                            stage: "installing".into(),
                            progress: if saw_work {
                                0.95
                            } else {
                                ((tick + 1) as f64 / 20.).min(0.9)
                            },
                            bytes_done: done,
                            bytes_total: total,
                            speed_bps: 0.,
                            eta_seconds: None,
                            message: format!(
                                "{label}: AppInst idle ({status}). {}",
                                if saw_work {
                                    "Install likely finished, confirming"
                                } else {
                                    "Waiting for promote"
                                }
                            ),
                            ..Default::default()
                        },
                    );
                    if saw_work && idle_ticks >= 20 {
                        // F2: saw real progress, then silence — uncertain, not complete.
                        return Err(
                            "Install remains in progress; check receiver status later".into(),
                        );
                    }
                    if !saw_work && idle_ticks >= 45 {
                        // F1: never acknowledged — fail honestly.
                        return Err(unconfirmed(&label, &cid));
                    }
                } else {
                    saw_work = true;
                    idle_ticks = 0;
                    emit(
                        app,
                        Progress {
                            job_id: job.into(),
                            stage: "installing".into(),
                            progress: progress.max(0.1),
                            bytes_done: done,
                            bytes_total: total,
                            speed_bps: 0.,
                            eta_seconds: None,
                            message: format!("{label}: {status}"),
                            ..Default::default()
                        },
                    );
                }
            }
        }
    }
    // Loop exhausted with no terminal state: same honesty rule as the idle breaks.
    if saw_work {
        return Err("Install remains in progress; check receiver status later".into());
    }
    Err(unconfirmed(&label, &cid))
}

async fn upload_pkg_set(
    app: &AppHandle,
    s: &Settings,
    packages: Vec<PathBuf>,
    job: &str,
    title: Option<&str>,
    tx: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let packages = sort_pkgs(packages);
    if packages.is_empty() {
        return Err("No PKG files to install".into());
    }
    let count = packages.len();
    let set_total: u64 = packages
        .iter()
        .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .sum::<u64>()
        .max(1);
    let mut set_offset = 0u64;
    for (index, package) in packages.iter().enumerate() {
        let role = pkg_role_label(package);
        let name = package
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let id = title_from_path(package)
            .or_else(|| title.map(str::to_string));
        upload(
            app,
            s,
            package,
            job,
            id.as_deref(),
            tx,
            &format!("{}/{} {role} ({name})", index + 1, count),
            index + 1 == count,
            set_offset,
            set_total,
        )
        .await?;
        set_offset += std::fs::metadata(package).map(|m| m.len()).unwrap_or(0);
    }
    Ok(())
}

fn rd_filesize(value: &Value) -> Option<u64> {
    value["filesize"]
        .as_u64()
        .or_else(|| value["file_size"].as_u64())
        .or_else(|| value["filesize"].as_i64().and_then(|n| u64::try_from(n).ok()))
}

fn rd_download_url(value: &Value) -> Option<(String, Option<String>, Option<u64>)> {
    let filename = value["filename"].as_str().map(str::to_owned);
    let filesize = rd_filesize(value);
    let direct = value["download"]
        .as_str()
        .filter(|item| valid_http(item))
        .map(str::to_owned);
    if let Some(download) = direct {
        return Some((download, filename, filesize));
    }
    value["alternative"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(value["links"].as_array().into_iter().flatten())
        .find_map(|item| {
            item["download"]
                .as_str()
                .or_else(|| item.as_str())
                .filter(|url| valid_http(url))
                .map(|url| {
                    (
                        url.to_owned(),
                        filename.clone(),
                        rd_filesize(item).or(filesize),
                    )
                })
        })
}

fn rd_error_message(value: &Value, status: u16) -> String {
    let error = value["error"].as_str().unwrap_or("unknown");
    let code = value["error_code"].as_i64().unwrap_or(0);
    match error {
        "hoster_not_free" => {
            "Real-Debrid: 1fichier is not unlocked for this account (hoster_not_free). Re-verify the token in Settings, or the file needs the DLPS password.".into()
        }
        "hoster_unavailable" | "hoster_temporarily_unavailable" => {
            "Real-Debrid: 1fichier is down right now. Retry in a minute.".into()
        }
        "file_unavailable" | "unavailable_file" | "link_offline" => {
            "Real-Debrid: that 1fichier file is offline or deleted.".into()
        }
        "bad_token" | "bad_login" => {
            "Real-Debrid token was rejected. Paste a new token in Settings and Verify.".into()
        }
        "traffic_exhausted" => "Real-Debrid quota empty.".into(),
        "infringing_file" => "Real-Debrid: infringing_file.".into(),
        other => format!("Real-Debrid: {other} (code {code}, HTTP {status})"),
    }
}

async fn unrestrict_hoster(
    http: &Client,
    landing: &str,
) -> Result<(String, Option<String>, Option<u64>), String> {
    let token = secret("real-debrid")?
        .get_password()
        .map_err(|_| "Real-Debrid token is not stored".to_string())?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err("Real-Debrid token is not stored".into());
    }
    let passwords = ["DLPSGAME.COM", "dlpsgame.com", ""];
    let mut last = "Unrestrict response did not contain an HTTP download".to_string();
    for password in passwords {
        for attempt in 0..3u32 {
            let mut form = vec![("link", landing.to_string())];
            if !password.is_empty() {
                form.push(("password", password.to_string()));
            }
            let response = tokio::time::timeout(
                Duration::from_secs(60),
                http.post("https://api.real-debrid.com/rest/1.0/unrestrict/link")
                    .bearer_auth(&token)
                    .form(&form)
                    .send(),
            )
            .await
            .map_err(|_| network_error("Real-Debrid unrestrict timed out"))?
            .map_err(|_| network_error("Real-Debrid unrestrict request"))?;
            let status = response.status().as_u16();
            let value: Value = response
                .json()
                .await
                .map_err(|_| network_error("Real-Debrid response"))?;
            if let Some(found) = rd_download_url(&value) {
                return Ok(found);
            }
            last = rd_error_message(&value, status);
            let error = value["error"].as_str().unwrap_or("");
            if matches!(
                error,
                "traffic_exhausted" | "infringing_file" | "bad_token" | "bad_login"
            ) {
                return Err(last);
            }
            if matches!(
                error,
                "hoster_not_free" | "file_unavailable" | "unavailable_file"
            ) {
                break;
            }
            sleep(Duration::from_secs(2 + u64::from(attempt))).await;
        }
    }
    Err(last)
}

async fn download_url(
    http: &Client,
    url: &str,
    part: &Path,
    app: &AppHandle,
    job: &str,
    rx: &watch::Receiver<bool>,
    message: &str,
    title: &str,
    icon: &Option<String>,
    size_hint: u64,
    set_done: u64,
    set_total: u64,
    set_started: Instant,
) -> Result<(Vec<u8>, String, Option<String>), String> {
    let existing = fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    // D1: one model — bar and text both come from set-wide bytes.
    let set_fraction = |done: u64, total: u64| {
        if total > 0 {
            (done as f64 / total as f64).clamp(0., 0.99)
        } else {
            0.
        }
    };
    emit(
        app,
        Progress {
            job_id: job.into(),
            stage: "downloading".into(),
            progress: set_fraction(set_done + existing, set_total),
            bytes_done: set_done + existing,
            bytes_total: set_total,
            speed_bps: 0.,
            eta_seconds: None,
            // D3: resumed bytes are stated, not silently folded in.
            message: if existing > 0 {
                format!("{message} — connecting (resumed {} MB)", existing / 1_048_576)
            } else {
                format!("{message} — connecting")
            },
            title: title.into(),
            icon: icon.clone(),
            ..Default::default()
        },
    );
    let mut request = http.get(url);
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let response = tokio::time::timeout(Duration::from_secs(45), request.send())
        .await
        .map_err(|_| network_error("Package download request timed out"))?
        .map_err(|_| network_error("Package download request"))?;
    if !(response.status().is_success() || response.status() == reqwest::StatusCode::PARTIAL_CONTENT)
    {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }
    if existing > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err("Origin does not support resume; remove the retained .part and retry".into());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let filename = response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(disposition_filename);
    let total = (response.content_length().unwrap_or(0) + existing).max(size_hint);
    let set_total = set_total.max(set_done + total);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .await
        .map_err(redact)?;
    let mut stream = response.bytes_stream();
    let mut done = existing;
    loop {
        if *rx.borrow() {
            return Err("cancelled".into());
        }
        transfer_checkpoint(app, job, rx).await?;
        let chunk = match tokio::time::timeout(Duration::from_secs(30), stream.next()).await {
            Err(_) => return Err("Download stalled for 30 seconds. Cancel and retry.".into()),
            Ok(None) => break,
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(_))) => return Err(network_error("Package download stream")),
        };
        file.write_all(&chunk).await.map_err(redact)?;
        done += chunk.len() as u64;
        let all = set_done + done;
        // D2: speed/ETA measured on the whole set across volumes, not per volume.
        let speed = all as f64 / set_started.elapsed().as_secs_f64().max(0.01);
        emit(
            app,
            Progress {
                job_id: job.to_owned(),
                stage: "downloading".into(),
                progress: set_fraction(all, set_total),
                bytes_done: all,
                bytes_total: set_total,
                speed_bps: speed,
                eta_seconds: (set_total > all)
                    .then(|| ((set_total - all) as f64 / speed.max(0.01)) as u64),
                message: message.into(),
                title: title.into(),
                icon: icon.clone(),
                ..Default::default()
            },
        );
    }
    file.flush().await.map_err(redact)?;
    let mut magic = [0; 8];
    let mut reader = fs::File::open(part).await.map_err(redact)?;
    let read = reader.read(&mut magic).await.map_err(redact)?;
    Ok((magic[..read].to_vec(), content_type, filename))
}

#[tauri::command]
async fn start_delivery(
    app: AppHandle,
    state: State<'_, AppState>,
    request: DeliveryRequest,
) -> Result<String, String> {
    let parts = delivery_parts(&request)?;
    let archive_set = request.package.archive_set_id.is_some();
    let s = state.settings.lock().unwrap().clone();
    validate_receiver_candidate(&s.ps5_host, s.ps5_port).map_err(|_| {
        "PS5 receiver is not configured. Open Settings or download the receiver ELF.".to_string()
    })?;
    ping(&s)
        .await
        .map_err(|error| format!("PS5 receiver is not ready: {error}"))?;
    let job = Uuid::new_v4().to_string();
    let (tx, mut rx) = watch::channel(false);
    state.cancel.lock().unwrap().insert(job.clone(), tx);
    let http = state.http.clone();
    let app2 = app.clone();
    let job2 = job.clone();
    tauri::async_runtime::spawn(async move {
        let job_title = request.title_name.clone().unwrap_or_default();
        let job_icon = request.icon.clone();
        emit(
            &app2,
            Progress {
                job_id: job2.clone(),
                stage: "queued".into(),
                progress: 0.,
                bytes_done: 0,
                bytes_total: 0,
                speed_bps: 0.,
                eta_seconds: None,
                message: "Starting download".into(),
                title: job_title.clone(),
                icon: job_icon.clone(),
                title_id: request.title_id.clone().unwrap_or_default(),
                package_kind: request.package.kind.clone(),
                package_label: request.package.label.clone(),
                package_version: request.package.version.clone(),
                ..Default::default()
            },
        );
        let res = async {
            let dir = PathBuf::from(&s.download_dir);
            fs::create_dir_all(&dir).await.map_err(redact)?;
            let staging = if archive_set {
                let set = request
                    .package
                    .archive_set_id
                    .as_deref()
                    .unwrap_or("set")
                    .chars()
                    .map(|character| {
                        if character.is_ascii_alphanumeric() {
                            character
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>();
                let path = dir.join(format!("archive_{set}"));
                fs::create_dir_all(&path).await.map_err(redact)?;
                path
            } else {
                dir.clone()
            };
            let mut downloaded = Vec::new();
            let mut volume_names = Vec::new();
            let mut detected = ArtifactKind::Unknown;
            let set_total: u64 = parts.iter().filter_map(|package| package.expected_size).sum();
            let mut set_done: u64 = 0;
            let set_started = Instant::now();
            for (index, package) in parts.iter().enumerate() {
                let access_type = package.access_type.to_ascii_lowercase();
                let mut url = package.url.clone();
                let needs_unlock = match access_type.as_str() {
                    "direct" => false,
                    "hosterlanding" | "hoster-landing" => true,
                    _ => !direct_package(&url),
                };
                if needs_unlock && !s.real_debrid_enabled {
                    return Err(
                        "This source returned a hoster link. Enable Real-Debrid in Settings before installing."
                            .into(),
                    );
                }
                let mut rd_name = None;
                let mut rd_size = None;
                if needs_unlock {
                    emit(
                        &app2,
                        Progress {
                            job_id: job2.clone(),
                            stage: "unlocking".into(),
                            progress: index as f64 / parts.len().max(1) as f64,
                            bytes_done: 0,
                            bytes_total: 0,
                            speed_bps: 0.,
                            eta_seconds: None,
                            message: format!(
                                "Unlocking hoster link {}/{}",
                                index + 1,
                                parts.len()
                            ),
                            title: job_title.clone(),
                            icon: job_icon.clone(),
                            ..Default::default()
                        },
                    );
                    let unrestricted = unrestrict_hoster(&http, &url).await?;
                    url = unrestricted.0;
                    rd_name = unrestricted.1;
                    rd_size = unrestricted.2;
                }
                let file_key = download_key(
                    &package.url,
                    request.title_id.as_deref(),
                    &package.kind,
                );
                let part_path = staging.join(format!(
                    "download_{file_key}{}.part",
                    package
                        .archive_part_number
                        .map(|part| format!("_p{part:02}"))
                        .unwrap_or_default()
                ));
                let (header, content_type, disposition) = download_url(
                    &http,
                    &url,
                    &part_path,
                    &app2,
                    &job2,
                    &rx,
                    &format!(
                        "Downloading {} · part {}/{}",
                        package.label,
                        index + 1,
                        parts.len()
                    ),
                    &job_title,
                    &job_icon,
                    rd_size.or(package.expected_size).unwrap_or(0),
                    set_done,
                    set_total,
                    set_started,
                )
                .await?;
                let name = rd_name
                    .or(disposition)
                    .or_else(|| package.archive_file_name.clone())
                    .unwrap_or_else(|| url.clone());
                detected = artifact_kind(&header, &name, &content_type);
                if header.first() == Some(&b'<') {
                    return Err(
                        "Hoster returned HTML instead of a package. Unlock failed or the link expired."
                            .into(),
                    );
                }
                downloaded.push(part_path);
                volume_names.push(name);
                set_done += std::fs::metadata(downloaded.last().unwrap())
                    .map(|meta| meta.len())
                    .unwrap_or(0);
            }
            if downloaded.iter().any(|path| {
                std::fs::File::open(path)
                    .ok()
                    .and_then(|mut file| {
                        let mut magic = [0u8; 4];
                        std::io::Read::read(&mut file, &mut magic)
                            .ok()
                            .map(|_| magic)
                    })
                    .map(|magic| magic == *b"Rar!")
                    .unwrap_or(false)
            }) {
                detected = ArtifactKind::Rar;
            }
            let mut primary = downloaded
                .first()
                .cloned()
                .ok_or("Download produced no files")?;
            let consumed_inputs;
            if detected == ArtifactKind::Rar {
                if let Some(count) = parts
                    .iter()
                    .find_map(|part| part.archive_part_count)
                    .or(request.package.archive_part_count)
                {
                    if downloaded.len() as u32 != count {
                        return Err(format!(
                            "expected {count} volumes, got {}",
                            downloaded.len()
                        ));
                    }
                }
                if let Some(missing) = missing_rar_volumes(&volume_names) {
                    return Err(format!(
                        "Split RAR is incomplete ({missing}). Download every .partN.rar from the same hoster page."
                    ));
                }
                let mut named = parts.clone();
                for (package, name) in named.iter_mut().zip(volume_names.iter()) {
                    if package.archive_file_name.is_none() {
                        if let Some(file) = Path::new(name)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .filter(|file| {
                                file.contains('.')
                                    && !file.contains("..")
                                    && !file.contains('/')
                                    && !file.contains('\\')
                            })
                        {
                            package.archive_file_name = Some(file.to_string());
                        }
                    }
                }
                let (prepared_primary, prepared_inputs) = prepare_rar_volumes(&downloaded, &named)?;
                primary = prepared_primary;
                consumed_inputs = prepared_inputs;
            } else {
                consumed_inputs = downloaded.clone();
            }
            if detected == ArtifactKind::Pkg {
                let final_path = dir.join(format!(
                    "download_{}.pkg",
                    download_key(
                        &request.package.url,
                        request.title_id.as_deref(),
                        &request.package.kind
                    )
                ));
                fs::rename(&primary, &final_path).await.map_err(redact)?;
                let result = upload(
                    &app2,
                    &s,
                    &final_path,
                    &job2,
                    request.title_id.as_deref(),
                    &mut rx,
                    "PKG",
                    true,
                    0,
                    0,
                )
                .await;
                if result.is_ok() {
                    archives::remove_consumed_inputs(&[final_path], &[])?;
                }
                return result;
            }
            if matches!(
                detected,
                ArtifactKind::Rar | ArtifactKind::SevenZ | ArtifactKind::Zip
            ) {
                emit(
                    &app2,
                    Progress {
                        job_id: job2.clone(),
                        stage: "extracting".into(),
                        progress: 0.,
                        bytes_done: 0,
                        bytes_total: 0,
                        speed_bps: 0.,
                        eta_seconds: None,
                        message: "Extracting archive".into(),
                        title: request.title_name.clone().unwrap_or_default(),
                        icon: request.icon.clone(),
                        ..Default::default()
                    },
                );
                let cache = dir.join("extracted");
                let app_extract = app2.clone();
                let job_extract = job2.clone();
                let extract_title = request.title_name.clone().unwrap_or_default();
                let extract_icon = request.icon.clone();
                let password = request.package.archive_password.clone();
                let extracted = tokio::task::spawn_blocking(move || {
                    archives::extract_content(&primary, &cache, detected, password.as_deref(), Arc::new(move |done, total, speed| {
                        emit(
                            &app_extract,
                            Progress {
                                job_id: job_extract.clone(),
                                stage: "extracting".into(),
                                progress: if total > 0 {
                                    (done as f64 / total as f64).min(0.99)
                                } else {
                                    0.
                                },
                                bytes_done: done,
                                bytes_total: total,
                                speed_bps: speed,
                                eta_seconds: (total > done && speed > 1.)
                                    .then(|| ((total - done) as f64 / speed) as u64),
                                message: if total > 0 {
                                    format!(
                                        "Extracting {:.2} / {:.2} GB · {:.1} MB/s",
                                        done as f64 / 1_073_741_824.0,
                                        total as f64 / 1_073_741_824.0,
                                        speed / 1_048_576.0
                                    )
                                } else {
                                    format!(
                                        "Extracting {:.2} GB · {:.1} MB/s",
                                        done as f64 / 1_073_741_824.0,
                                        speed / 1_048_576.0
                                    )
                                },
                                title: extract_title.clone(),
                                icon: extract_icon.clone(),
                                ..Default::default()
                            },
                        );
                    }), 0)
                })
                .await
                .map_err(redact)??;
                let extracted_outputs = archives::extracted_outputs(&extracted);
                archives::remove_consumed_inputs(&consumed_inputs, &extracted_outputs)?;
                match extracted {
                    ExtractedContent::Pkgs(packages) => {
                        // F7: receipt — which files, what sizes, which identities.
                        let receipt = packages
                            .iter()
                            .map(|p| {
                                let name = p
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned();
                                let size = std::fs::metadata(p)
                                    .map(|m| m.len())
                                    .unwrap_or(0);
                                let cid = pkg_content_id(p).unwrap_or_else(|| "no-cid".into());
                                format!(
                                    "{name} ({} MB, {})",
                                    size / 1_048_576,
                                    cid
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        emit(
                            &app2,
                            Progress {
                                job_id: job2.clone(),
                                stage: "extracting".into(),
                                progress: 1.,
                                bytes_done: 0,
                                bytes_total: 0,
                                speed_bps: 0.,
                                eta_seconds: None,
                                message: format!(
                                    "Extracted {} PKG{}: {}",
                                    packages.len(),
                                    if packages.len() == 1 { "" } else { "s" },
                                    receipt
                                ),
                                title: request.title_name.clone().unwrap_or_default(),
                                icon: request.icon.clone(),
                                ..Default::default()
                            },
                        );
                        return upload_pkg_set(
                            &app2,
                            &s,
                            packages,
                            &job2,
                            request.title_id.as_deref(),
                            &mut rx,
                        )
                        .await;
                    }
                    ExtractedContent::Dump(root) => {
                        let dump_id = request.title_id.clone().or_else(|| title_from_path(&root));
                        return upload_dump(
                            &app2,
                            &s,
                            &root,
                            &job2,
                            dump_id.as_deref(),
                            &request.package.kind,
                            &rx,
                        )
                        .await;
                    }
                }
            }
            Err("Downloaded file has unknown package magic".into())
        }
        .await;
        if let Err(e) = res {
            let stage = job_error_stage(&e);
            emit(
                &app2,
                Progress {
                    job_id: job2,
                    stage: stage.into(),
                    // F6: uncertain is never 100 % — 100 % means confirmed.
                    progress: if stage == "monitoring-ended" { 0.95 } else { 0. },
                    bytes_done: 0,
                    bytes_total: 0,
                    speed_bps: 0.,
                    eta_seconds: None,
                    message: redact(e),
                    ..Default::default()
                },
            )
        }
    });
    Ok(job)
}
#[tauri::command]
fn cancel_job(state: State<AppState>, job_id: String) -> Result<bool, String> {
    let jobs = state.jobs.lock().unwrap();
    if jobs.get(&job_id).is_some_and(|p| matches!(p.stage.as_str(), "submitting" | "installing" | "mounting")) {
        return Err("Installation is managed by the PS5 now; manage it on the console.".into());
    }
    Ok(
    state
        .cancel
        .lock()
        .unwrap()
        .get(&job_id)
        .map(|x| x.send(true).is_ok())
        .unwrap_or(false))
}
fn pausable_stage(stage: &str) -> bool { matches!(stage, "downloading" | "uploading") }

#[tauri::command]
fn pause_job(app: AppHandle, state: State<AppState>, job_id: String, paused: bool) -> Result<(), String> {
    let event = {
        let mut jobs = state.jobs.lock().unwrap();
        let job = jobs.get_mut(&job_id).ok_or("Transfer no longer exists")?;
        if !pausable_stage(&job.stage) { return Err("Pause is available during download and upload. Console installation is managed by the PS5.".into()); }
        job.paused = paused;
        job.clone()
    };
    let _ = app.emit("delivery-progress", event);
    Ok(())
}

async fn begin_console_stage(app: &AppHandle, job: &str, cancel: &watch::Receiver<bool>, stage: &str) -> Result<(), String> {
    loop {
        transfer_checkpoint(app, job, cancel).await?;
        let state = app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        if *cancel.borrow() { return Err("cancelled".into()); }
        if let Some(p) = jobs.get_mut(job) {
            if p.paused { continue; }
            p.stage = stage.into();
        }
        return Ok(());
    }
}

async fn transfer_checkpoint(app: &AppHandle, job: &str, cancel: &watch::Receiver<bool>) -> Result<(), String> {
    loop {
        if *cancel.borrow() { return Err("cancelled".into()); }
        let paused = app.state::<AppState>().jobs.lock().unwrap().get(job).is_some_and(|p|p.paused);
        if !paused { return Ok(()); }
        let mut changed = cancel.clone();
        tokio::select! { _ = sleep(Duration::from_millis(80)) => {}, _ = changed.changed() => {} }
    }
}

#[tauri::command]
fn list_jobs(state: State<AppState>) -> Vec<Progress> {
    state.jobs.lock().unwrap().values().cloned().collect()
}

#[tauri::command]
fn system_drive_prefix() -> Option<String> {
    std::env::var("SystemDrive")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("SystemRoot")
                .ok()
                .and_then(|root| root.split(['\\', '/']).next().map(str::to_owned))
        })
}
#[tauri::command]
async fn scan_local_packages(path: String) -> Result<Vec<LocalPackage>, String> {
    let supplied = PathBuf::from(&path);
    if supplied.is_file() {
        let (b, n, size) = file_header(&supplied).await?;
        if b[..n].starts_with(b"PK")
            || b[..n].starts_with(b"Rar!")
            || b[..n].starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C])
        {
            let source = supplied.clone();
            let cache = supplied
                .parent()
                .unwrap_or(Path::new("."))
                .join("GameSearch Extracted");
            let kind = artifact_kind(&b[..n], &source.display().to_string(), "");
            let extracted = tokio::task::spawn_blocking(move || {
                extract_any_archive(&source, &cache, kind, |_, _, _| {})
            })
            .await
            .map_err(redact)??;
            return Ok(match extracted {
                ExtractedContent::Pkgs(packages) => packages
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let name = p
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let title_id = name
                            .split(|c: char| !c.is_ascii_alphanumeric())
                            .find(|x| title_id(&x.to_ascii_uppercase()))
                            .map(|x| x.to_ascii_uppercase());
                        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                        LocalPackage {
                            number: i + 1,
                            path: p.display().to_string(),
                            name,
                            kind: "pkg".into(),
                            size,
                            title_id,
                        }
                    })
                    .collect(),
                ExtractedContent::Dump(root) => vec![LocalPackage {
                    number: 1,
                    path: root.display().to_string(),
                    name: root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    kind: "dump".into(),
                    size: 0,
                    title_id: title_from_path(&root),
                }],
            });
        }
        if !b[..n].starts_with(&[0x7f, 0x43, 0x4e, 0x54]) {
            return Err("Selected file does not have PKG magic".into());
        }
        let name = supplied
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let title_id = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .find(|x| title_id(&x.to_ascii_uppercase()))
            .map(|x| x.to_ascii_uppercase());
        return Ok(vec![LocalPackage {
            number: 1,
            path,
            name,
            kind: "pkg".into(),
            size,
            title_id,
        }]);
    }
    let mut out = vec![];
    let mut q = vec![PathBuf::from(path)];
    while let Some(p) = q.pop() {
        let mut rd = fs::read_dir(&p).await.map_err(redact)?;
        while let Some(e) = rd.next_entry().await.map_err(redact)? {
            let p = e.path();
            if p.is_dir() {
                q.push(p)
            } else {
                let (b, n, size) = file_header(&p).await?;
                if b[..n].starts_with(&[0x7f, 0x43, 0x4e, 0x54]) {
                    let name = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let id = name
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .find(|x| title_id(&x.to_ascii_uppercase()))
                        .map(|x| x.to_ascii_uppercase());
                    out.push(LocalPackage {
                        number: out.len() + 1,
                        path: p.display().to_string(),
                        name,
                        kind: "pkg".into(),
                        size,
                        title_id: id,
                    })
                }
            }
        }
    }
    Ok(out)
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ManualCandidate {
    path: String,
    name: String,
    /// "pkg" | "dump" | "archive"
    content: String,
    /// auto-detected "base" | "update" | "dlc" | "backport" (user-overridable in UI)
    detected_kind: String,
    title_id: Option<String>,
    size: u64,
    needs_title: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManualItem {
    path: String,
    kind: String,
    title_id: Option<String>,
}

fn manual_kind_for_name(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.contains("backport")
        || lower.contains("_bp_")
        || lower.contains("-bp.")
        || lower.contains(" bp ")
    {
        "backport"
    } else if lower.contains("update")
        || lower.contains("patch")
        || lower.contains("fixpak")
        || lower.contains("marlin")
    {
        "update"
    } else if lower.contains("dlc")
        || lower.contains("fpack")
        || lower.contains("extradat")
        || lower.contains("-ac.")
        || lower.contains("_ac_")
    {
        "dlc"
    } else {
        "base"
    }
}

fn manual_title_for(path: &Path) -> Option<String> {
    title_from_path(path).or_else(|| path.parent().and_then(title_from_path))
}

fn read_magic_sync(path: &Path) -> Option<[u8; 8]> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 8];
    let read = file.read(&mut magic).ok()?;
    if read < 4 {
        return None;
    }
    Some(magic)
}

fn classify_manual_file(path: &Path) -> Option<ManualCandidate> {
    let name = path.file_name()?.to_str()?.to_owned();
    let lower = name.to_ascii_lowercase();
    let content = if lower.ends_with(".pkg") {
        // PKGs must carry real magic; anything else is junk with a pkg name.
        match read_magic_sync(path) {
            Some(magic) if magic[..4] == [0x7f, 0x43, 0x4e, 0x54] => "pkg",
            _ => return None,
        }
    } else if lower.ends_with(".rar")
        || lower.ends_with(".zip")
        || lower.ends_with(".7z")
        || lower.ends_with(".001")
    {
        "archive"
    } else {
        return None;
    };
    let title_id = manual_title_for(path);
    Some(ManualCandidate {
        path: path.display().to_string(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        detected_kind: manual_kind_for_name(&name).into(),
        needs_title: false,
        name,
        title_id,
        content: content.into(),
    })
}

/// Backport-style folders ship eboot.bin (+ fakelib/sce_module) without sce_sys.
/// They still upload as dumps; the console mount verdict comes from the ELF.
fn is_loose_dump(root: &Path) -> bool {
    root.is_dir() && root.join("eboot.bin").is_file()
}

fn manual_dump(root: &Path) -> ManualCandidate {
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dump")
        .to_owned();
    let title_id = dump_title_id(root).or_else(|| manual_title_for(root));
    ManualCandidate {
        path: root.display().to_string(),
        size: dir_bytes(root),
        detected_kind: manual_kind_for_name(&name).into(),
        needs_title: title_id.is_none(),
        name,
        title_id,
        content: "dump".into(),
    }
}

#[tauri::command]
fn scan_manual_folder(path: String) -> Result<Vec<ManualCandidate>, String> {
    let root = PathBuf::from(&path);
    if !root.exists() {
        return Err("Folder does not exist".into());
    }
    let mut out = Vec::new();
    if root.is_file() {
        if let Some(candidate) = classify_manual_file(&root) {
            out.push(candidate);
        }
    } else if is_game_dump(&root) || is_loose_dump(&root) {
        out.push(manual_dump(&root));
    } else {
        let mut stack = vec![root.clone()];
        let mut seen_files = 0usize;
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    if is_game_dump(&p) || is_loose_dump(&p) {
                        out.push(manual_dump(&p));
                    } else {
                        stack.push(p);
                    }
                    continue;
                }
                if seen_files >= 500 {
                    break;
                }
                seen_files += 1;
                if let Some(candidate) = classify_manual_file(&p) {
                    out.push(candidate);
                }
            }
        }
        // Files living inside a discovered dump root belong to that dump.
        let dumps: Vec<String> = out
            .iter()
            .filter(|c| c.content == "dump")
            .map(|c| c.path.clone())
            .collect();
        out.retain(|c| {
            c.content == "dump"
                || !dumps.iter().any(|d| {
                    c.path.starts_with(&format!("{d}\\")) || c.path.starts_with(&format!("{d}/"))
                })
        });
    }
    if out.is_empty() {
        return Err(format!(
            "No PKG, game dump, or archive found in {}",
            root.display()
        ));
    }
    out.sort_by(|a, b| {
        (a.content.clone(), a.name.clone()).cmp(&(b.content.clone(), b.name.clone()))
    });
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn install_manual_item(
    app: &AppHandle,
    s: &Settings,
    item_path: &Path,
    kind: &str,
    title: Option<&str>,
    tag: &str,
    job: &str,
    job_title: &str,
    icon: &Option<String>,
    rx: &mut watch::Receiver<bool>,
    announce_complete: bool,
) -> Result<(), String> {
    if item_path.is_dir() {
        return upload_dump(app, s, item_path, job, title, kind, rx).await;
    }
    let name = item_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let header = read_magic_sync(item_path).unwrap_or([0; 8]);
    let detected = artifact_kind(&header, &name, "");
    if detected == ArtifactKind::Pkg {
        return upload(
            app, s, item_path, job, title, rx,
            &format!("{tag} ({name})"), announce_complete, 0, 0,
        )
        .await;
    }
    if !matches!(
        detected,
        ArtifactKind::Rar | ArtifactKind::SevenZ | ArtifactKind::Zip
    ) {
        return Err(format!("{name} is not a PKG, dump, or supported archive"));
    }
    let cache = PathBuf::from(&s.download_dir).join("manual-extract");
    let app_extract = app.clone();
    let job_extract = job.to_string();
    let extract_title = job_title.to_owned();
    let extract_icon = icon.clone();
    let extract_tag = tag.to_owned();
    let extracted = tokio::task::spawn_blocking({
        let source = item_path.to_path_buf();
        let cache = cache.clone();
        move || {
            extract_any_archive(&source, &cache, detected, move |done, total, speed| {
                emit(
                    &app_extract,
                    Progress {
                        job_id: job_extract.clone(),
                        stage: "extracting".into(),
                        progress: if total > 0 {
                            (done as f64 / total as f64).min(0.99)
                        } else {
                            0.
                        },
                        bytes_done: done,
                        bytes_total: total,
                        speed_bps: speed,
                        eta_seconds: (total > done && speed > 1.)
                            .then(|| ((total - done) as f64 / speed) as u64),
                        message: format!("{extract_tag}: extracting {name}"),
                        title: extract_title.clone(),
                        icon: extract_icon.clone(),
                        ..Default::default()
                    },
                );
            })
        }
    })
    .await
    .map_err(redact)??;
    match extracted {
        ExtractedContent::Pkgs(packages) => {
            let count = packages.len();
            for (index, package) in packages.iter().enumerate() {
                let role = pkg_role_label(package);
                let pkg_name = package
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let id = title_from_path(package).or_else(|| title.map(str::to_string));
                upload(
                    app, s, package, job, id.as_deref(), rx,
                    &format!("{tag} · {}/{} {role} ({pkg_name})", index + 1, count),
                    announce_complete && index + 1 == count,
                    0, 0,
                )
                .await?;
            }
            Ok(())
        }
        ExtractedContent::Dump(root) => {
            let dump_id = title
                .map(str::to_string)
                .or_else(|| title_from_path(&root));
            upload_dump(app, s, &root, job, dump_id.as_deref(), kind, rx).await
        }
    }
}

#[tauri::command]
async fn start_manual_install(
    app: AppHandle,
    state: State<'_, AppState>,
    items: Vec<ManualItem>,
) -> Result<String, String> {
    if items.is_empty() {
        return Err("Nothing to install".into());
    }
    struct Job {
        path: PathBuf,
        kind: String,
        title: Option<String>,
        name: String,
    }
    let mut queue = Vec::new();
    for item in &items {
        let kind = item.kind.to_ascii_lowercase();
        if !matches!(kind.as_str(), "base" | "update" | "dlc" | "backport") {
            return Err(format!("Unknown kind for {}", item.path));
        }
        let path = PathBuf::from(&item.path);
        if !path.exists() {
            return Err(format!("Missing: {}", item.path));
        }
        let title = item
            .title_id
            .clone()
            .map(|t| t.to_ascii_uppercase())
            .filter(|t| title_id(t));
        if path.is_dir() && (is_game_dump(&path) || is_loose_dump(&path)) && title.is_none() {
            return Err(format!(
                "{} is a game dump and needs a CUSA/PPSA title ID",
                item.path
            ));
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        queue.push(Job { path, kind, title, name });
    }
    let s = state.settings.lock().unwrap().clone();
    validate_receiver_candidate(&s.ps5_host, s.ps5_port).map_err(|_| {
        "PS5 receiver is not configured. Open Settings or download the receiver ELF.".to_string()
    })?;
    ping(&s)
        .await
        .map_err(|error| format!("PS5 receiver is not ready: {error}"))?;
    let job = Uuid::new_v4().to_string();
    let (tx, mut rx) = watch::channel(false);
    state.cancel.lock().unwrap().insert(job.clone(), tx);
    let job_title = if queue.len() == 1 {
        queue[0].name.clone()
    } else {
        format!("Manual install ({} items)", queue.len())
    };
    let app2 = app.clone();
    let job2 = job.clone();
    tauri::async_runtime::spawn(async move {
        emit(
            &app2,
            Progress {
                job_id: job2.clone(),
                stage: "queued".into(),
                progress: 0.,
                bytes_done: 0,
                bytes_total: 0,
                speed_bps: 0.,
                eta_seconds: None,
                message: format!("Manual install: {} item(s)", queue.len()),
                title: job_title.clone(),
                icon: None,
                title_id: if queue.len() == 1 { queue[0].title.clone().unwrap_or_default() } else { String::new() },
                package_kind: if queue.len() == 1 { queue[0].kind.clone() } else { "batch".into() },
                package_label: job_title.clone(),
                ..Default::default()
            },
        );
        let total = queue.len();
        let mut res: Result<(), String> = Ok(());
        for (index, item) in queue.iter().enumerate() {
            if *rx.borrow() {
                res = Err("cancelled".into());
                break;
            }
            let tag = format!("Manual {}/{} {} ({})", index + 1, total, item.kind, item.name);
            res = install_manual_item(
                &app2,
                &s,
                &item.path,
                &item.kind,
                item.title.as_deref(),
                &tag,
                &job2,
                &job_title,
                &None,
                &mut rx,
                index + 1 == total,
            )
            .await;
            if res.is_err() {
                break;
            }
        }
        if let Err(e) = res {
            let stage = job_error_stage(&e);
            emit(
                &app2,
                Progress {
                    job_id: job2,
                    stage: stage.into(),
                    progress: if stage == "monitoring-ended" { 0.95 } else { 0. },
                    bytes_done: 0,
                    bytes_total: 0,
                    speed_bps: 0.,
                    eta_seconds: None,
                    message: redact(e),
                    ..Default::default()
                },
            )
        }
    });
    Ok(job)
}

#[tauri::command]
async fn start_local_install(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    let s = state.settings.lock().unwrap().clone();
    validate_receiver_candidate(&s.ps5_host, s.ps5_port).map_err(|_| {
        "PS5 receiver is not configured. Open Settings or download the receiver ELF.".to_string()
    })?;
    ping(&s)
        .await
        .map_err(|error| format!("PS5 receiver is not ready: {error}"))?;
    let source = PathBuf::from(&path);
    if !source.is_file() {
        return Err("Local install requires a regular PKG file".into());
    }
    let (header, read, size) = file_header(&source).await?;
    if size < 4 || !header[..read].starts_with(&[0x7f, 0x43, 0x4e, 0x54]) {
        return Err("Local file does not have PKG magic".into());
    }
    let job = Uuid::new_v4().to_string();
    let (tx, mut rx) = watch::channel(false);
    state.cancel.lock().unwrap().insert(job.clone(), tx);
    let a = app.clone();
    let j = job.clone();
    let local_id = pkg_content_id(&source)
        .and_then(|id| id.split(|c: char| !c.is_ascii_alphanumeric()).find(|part| title_id(part)).map(str::to_string))
        .or_else(|| title_from_path(&source)).unwrap_or_default();
    emit(&app, Progress {
        job_id: job.clone(), stage: "queued".into(),
        title: source.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        title_id: local_id, package_kind: pkg_role_label(&source).to_ascii_lowercase(),
        package_label: source.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        message: "Waiting to upload".into(), ..Default::default()
    });
    tauri::async_runtime::spawn(async move {
        if let Err(e) = upload(
            &a,
            &s,
            Path::new(&path),
            &j,
            title_from_path(Path::new(&path)).as_deref(),
            &mut rx,
            "PKG",
            true,
            0,
            0,
        )
        .await
        {
            let stage = job_error_stage(&e);
            emit(
                &a,
                Progress {
                    job_id: j,
                    stage: stage.into(),
                    // F6: uncertain is never 100 % — 100 % means confirmed.
                    progress: if stage == "monitoring-ended" { 0.95 } else { 0. },
                    bytes_done: 0,
                    bytes_total: 0,
                    speed_bps: 0.,
                    eta_seconds: None,
                    message: redact(e),
                    ..Default::default()
                },
            )
        }
    });
    Ok(job)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            if let Err(error) = package_sources::migrate_bundled(&handle) { eprintln!("Source migration: {error}"); }
            let settings = std::fs::read(config_path(&handle)?)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            app.manage(AppState {
                settings: Arc::new(Mutex::new(settings)),
                cancel: Arc::new(Mutex::new(HashMap::new())),
                jobs: Arc::new(Mutex::new(HashMap::new())),
                resolving: Arc::new(AsyncMutex::new(HashMap::new())),
                http: Client::builder()
                    .user_agent("GameSearch/0.1")
                    .connect_timeout(Duration::from_secs(15))
                    .pool_idle_timeout(Duration::from_secs(30))
                    .build()?,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            export_receiver_payload,
            list_package_sources,
            install_package_source,
            install_package_source_from_path,
            set_package_source_enabled,
            remove_package_source,
            test_ps5,
            test_resolver,
            verify_real_debrid,
            search_games,
            load_catalog,
            fetch_cover,
            get_game_details,
            resolve_packages,
            start_delivery,
            list_jobs,
            system_drive_prefix,
            cancel_job,
            pause_job,
            scan_local_packages,
            start_local_install,
            scan_manual_folder,
            start_manual_install
        ])
        .run(tauri::generate_context!())
        .expect("Tauri error")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_and_urls() {
        let h = [0x10, 3, 0, 0, 0];
        assert_eq!(u32::from_le_bytes(h[1..].try_into().unwrap()), 3);
        assert!(valid_http("https://x.test/a.pkg"));
        assert!(!valid_http("magnet:?x"));
        assert!(direct_package("https://x/a.pkg"));
        assert_eq!(
            artifact_kind(b"Rar!\x1a\x07", "file.part01.rar", ""),
            ArtifactKind::Rar
        );
        assert_eq!(
            artifact_kind(&[0x7f, 0x43, 0x4e, 0x54], "x.bin", ""),
            ArtifactKind::Pkg
        );
        assert_eq!(
            disposition_filename(r#"attachment; filename="game.part01.rar""#).as_deref(),
            Some("game.part01.rar")
        );
        let part: Package = serde_json::from_value(json!({
            "kind": "base",
            "label": "Part 1",
            "url": "https://x.test/a",
            "archivePartNumber": 1,
            "archiveFileName": "Game.part1.rar"
        }))
        .unwrap();
        assert_eq!(rar_volume_filename(&part, 3), "Game.part1.rar");
        assert_eq!(rar_volume_filename(&part, 1), "Game.part1.rar");
        assert_eq!(rar_part_from_name("[DLPSGAME.COM]-PPSA16617.part1.rar"), Some(1));
        assert_eq!(rar_part_from_name("[DLPSGAME.COM]-PPSA16617.part2.rar"), Some(2));
        assert!(missing_rar_volumes(&["[DLPSGAME.COM]-PPSA16617.part1.rar".into()]).is_some());
        assert!(missing_rar_volumes(&[
            "[DLPSGAME.COM]-PPSA16617.part1.rar".into(),
            "[DLPSGAME.COM]-PPSA16617.part2.rar".into()
        ])
        .is_none());
    }
    #[test]
    fn uploaded_file_verification_requires_exact_size() {
        assert!(verified_upload_size(1, br#"OK {"size":123}"#, 123).is_ok());
        assert!(verified_upload_size(1, br#"OK {"size":0}"#, 0).is_ok());
        assert!(verified_upload_size(1, br#"OK {"size":122}"#, 123).is_err());
        assert!(verified_upload_size(1, b"OK", 123).is_err());
        assert!(verified_upload_size(2, br#"OK {"size":123}"#, 123).is_err());
    }

    #[test]
    fn verify_uploaded_file_uses_receiver_protocol() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut header = [0; 5];
                stream.read_exact(&mut header).await.unwrap();
                assert_eq!(header[0], 0x55);
                let mut path = vec![0; u32::from_le_bytes(header[1..].try_into().unwrap()) as usize];
                stream.read_exact(&mut path).await.unwrap();
                assert_eq!(path, b"/data/homebrew/PPSA31246/data.bin");
                let body = br#"OK {"size":4096}"#;
                stream.write_all(&[1]).await.unwrap();
                stream.write_all(&(body.len() as u32).to_le_bytes()).await.unwrap();
                stream.write_all(body).await.unwrap();
            });
            let settings = Settings { ps5_host: "127.0.0.1".into(), ps5_port: port, ..Settings::default() };
            verify_uploaded_file(&settings, "/data/homebrew/PPSA31246/data.bin", 4096).await.unwrap();
            server.await.unwrap();
        });
    }

    #[test]
    fn incomplete_backport_is_rejected_before_upload() {
        let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/work");
        std::fs::create_dir_all(&work).unwrap();
        let root = work.canonicalize().unwrap().join(format!("mount-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("fakelib")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"test").unwrap();
        let error = validate_mountable_dump(&root).unwrap_err();
        assert!(error.contains("overlay") && error.contains("No files were uploaded"));
        std::fs::create_dir_all(root.join("sce_sys")).unwrap();
        assert!(validate_mountable_dump(&root).unwrap_err().contains("param.json"));
        std::fs::write(root.join("sce_sys/param.json"), br#"{"titleId":"PPSA31246"}"#).unwrap();
        assert!(validate_mountable_dump(&root).is_ok());
        assert!(root.starts_with(work.canonicalize().unwrap()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn manual_loose_dump_detected() {
        // Backport layout: eboot.bin + fakelib, no sce_sys.
        let base = std::env::temp_dir().join(format!("gs-manual-test-{}", std::process::id()));
        let root = base.join("RE9-BESTPIG-PPSA31246");
        std::fs::create_dir_all(root.join("fakelib")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"fake").unwrap();
        assert!(is_loose_dump(&root));
        assert!(!is_game_dump(&root));
        let candidate = manual_dump(&root);
        assert_eq!(candidate.content, "dump");
        assert_eq!(candidate.title_id.as_deref(), Some("PPSA31246"));
        assert!(!candidate.needs_title);
        std::fs::remove_dir_all(&base).ok();
    }
    #[test]
    fn manual_kind_guessing() {
        assert_eq!(manual_kind_for_name("Terraria_CUSA00740_v1.35_BACKPORT_[5.05]_OPOISSO893.pkg"), "backport");
        assert_eq!(manual_kind_for_name("[SuperPSX]-RE9 REQUIEM_4xx[PPSA31246][01.200.000]-Bestpig"), "base");
        assert_eq!(manual_kind_for_name("game update v1.02.pkg"), "update");
        assert_eq!(manual_kind_for_name("extra DLC pack.pkg"), "dlc");
        assert_eq!(manual_kind_for_name("plain game.pkg"), "base");
        assert_eq!(manual_kind_for_name("my BACKPORT folder"), "backport");
    }
    #[test]
    fn filters_and_safety() {
        assert!(title_id("CUSA12345"));
        assert!(title_id("PPSA12345"));
        assert!(Path::new("a/b")
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir)));
        assert_eq!(
            remote(Some("CUSA12345")).starts_with("/user/data/tmp/upload_CUSA12345_"),
            true
        );
        assert_eq!(
            title_from_path(Path::new("Some Game CUSA54321 v1.00.pkg")).as_deref(),
            Some("CUSA54321")
        );
        assert!(remote(Some("PPSA12345")).contains("upload_PPSA12345_"));
    }
    #[test]
    fn protocol_classification_and_ranges() {
        assert!(require_preflight(1).is_ok());
        assert!(require_preflight(2).is_err());
        let submitted_in_error_frame = json!({
            "api_code":-99,
            "install_api_code":0,
            "auth_restore_code":-99,
            "state":"submitted",
            "content_id":"UP0000-CUSA12345_00-TEST000000000000"
        });
        assert_eq!(
            accepted_submission(&submitted_in_error_frame).unwrap(),
            "UP0000-CUSA12345_00-TEST000000000000"
        );
        assert!(accepted_submission(&json!({
            "install_api_code":-1,
            "state":"failed",
            "content_id":""
        }))
        .is_err());
        let installing_in_error_frame = json!({
            "api_code":-99,
            "status_api_code":0,
            "auth_restore_code":-99,
            "state":"installing",
            "status":"transferring",
            "progress":42
        });
        assert_eq!(
            install_decision(&installing_in_error_frame).unwrap(),
            InstallDecision::Installing {
                status: "transferring".into(),
                progress: 0.42
            }
        );
        assert_eq!(
            install_decision(&json!({
                "status_api_code":0,
                "state":"complete",
                "status":"playable",
                "progress":100
            }))
            .unwrap(),
            InstallDecision::Complete
        );
        assert!(install_decision(&json!({
            "status_api_code":-7,
            "state":"failed",
            "status":"error"
        }))
        .is_err());
        assert_eq!(
            install_decision(&json!({
                "api_code":-99,
                "state":"installing",
                "status":"transferring",
                "progress":10
            }))
            .unwrap(),
            InstallDecision::Installing {
                status: "transferring".into(),
                progress: 0.10
            }
        );
        assert_eq!(pkg_role(Path::new("Game CUSA12345 Update v1.10.pkg")), 1);
        assert_eq!(pkg_role(Path::new("Game CUSA12345 DLC.pkg")), 2);
        assert_eq!(pkg_role(Path::new("Game CUSA12345.pkg")), 0);
        assert!(appinst_idle("none"));
        assert!(appinst_idle(""));
        assert!(!appinst_idle("transferring"));
        let sorted = sort_pkgs(vec![
            PathBuf::from("a-update.pkg"),
            PathBuf::from("z-base.pkg"),
            PathBuf::from("b-DLC.pkg"),
        ]);
        assert!(sorted[0].to_string_lossy().contains("base"));
        assert!(sorted[1].to_string_lossy().contains("update"));
        assert!(sorted[2].to_string_lossy().contains("DLC"));
        let n = 100u64;
        let x: Vec<_> = (0..4)
            .map(|i| (n * i / 4, n * (i + 1) / 4 - n * i / 4))
            .collect();
        assert_eq!(x, vec![(0, 25), (25, 25), (50, 25), (75, 25)]);
    }
    #[test]
    fn magic_and_redaction() {
        assert!([0x7f, 0x43, 0x4e, 0x54].starts_with(&[0x7f, 0x43, 0x4e, 0x54]));
        assert!(b"PK\x03\x04".starts_with(b"PK"));
        assert!(redact("Bearer abc").contains("[redacted]"));
        assert_eq!(network_error("Metadata request"), "Metadata request failed");
        assert!(!network_error("Metadata request").contains("key="));
        assert!(!terminal_stage("installing"));
        assert!(terminal_stage("complete"));
        assert!(terminal_stage("monitoring-ended"));
        assert_eq!(job_error_stage("cancelled"), "cancelled");
        assert_eq!(
            job_error_stage("Install remains in progress; check receiver status later"),
            "monitoring-ended"
        );
        assert_eq!(job_error_stage("bad magic"), "failed");
        assert!(validate_receiver_candidate("192.168.0.10", 9114).is_ok());
        assert!(validate_receiver_candidate("", 9114).is_err());
        assert!(validate_receiver_candidate("localhost", 80).is_err());
        let key = download_key("https://host.test/file?id=7", Some("CUSA12345"), "base");
        assert_eq!(
            key,
            download_key("https://host.test/file?id=7", Some("CUSA12345"), "base")
        );
        assert_ne!(
            key,
            download_key("https://host.test/file?id=8", Some("CUSA12345"), "base")
        );
    }

    #[test]
    fn search_and_resolver_shapes_are_tolerant() {
        let games = search_results(&json!({
            "success": true,
            "results": [{
                "titleid": "CUSA17419",
                "name": "Persona 5 Royal",
                "region": "EU",
                "icon": "covers/p5.webp"
            }]
        }))
        .unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].title_id, "CUSA17419");
        assert!(games[0].icon.as_deref().unwrap().starts_with("https://"));

        let packages = packages_from_value(&json!({
            "data": {
                "packages": [{
                    "type": "Game Update",
                    "name": "Version 1.02",
                    "downloadUrl": "https://host.test/update.pkg"
                }]
            }
        }))
        .unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].kind, "update");
        let catalog = catalog_from_value(&json!({
            "cached": true,
            "sections": [{
                "id": "recently_updated",
                "title": "Recently Updated",
                "games": [{
                    "title_id": "CUSA17419",
                    "name": "Persona 5 Royal",
                    "region": "EU",
                    "cover_url": "https://cdn.test/p5.webp",
                    "downloads": [{
                        "kind": "base",
                        "label": "Base / Host",
                        "url": "https://host.test/base.pkg"
                    }]
                }]
            }]
        }))
        .unwrap();
        assert!(catalog.cached);
        assert_eq!(catalog.sections.len(), 1);
        assert_eq!(catalog.sections[0].games[0].title_id, "CUSA17419");
        assert_eq!(catalog.sections[0].games[0].packages.len(), 1);
        assert_eq!(
            resolver_download_urls("https://resolver.test", "CUSA17419").len(),
            2
        );
        assert_eq!(
            resolver_download_urls("https://resolver.test/api", "CUSA17419").len(),
            1
        );
    }

    #[test]
    fn local_header_read_is_bounded() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            use tokio::io::AsyncWriteExt;
            let path = std::env::temp_dir().join(format!("{}.pkg", Uuid::new_v4()));
            let mut file = fs::File::create(&path).await.unwrap();
            file.write_all(&[0x7f, 0x43, 0x4e, 0x54]).await.unwrap();
            file.write_all(&vec![0xAA; 1024 * 1024]).await.unwrap();
            file.flush().await.unwrap();
            let (header, read, size) = file_header(&path).await.unwrap();
            assert_eq!(read, 8);
            assert_eq!(&header[..4], &[0x7f, 0x43, 0x4e, 0x54]);
            assert_eq!(size, 1024 * 1024 + 4);
            fs::remove_file(path).await.unwrap();
        });
    }
    pub(crate) fn make_zip(entries: Vec<(&str, Vec<u8>)>) -> (PathBuf, PathBuf) {
        use std::io::Write;
        let root = std::env::temp_dir().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("input.zip");
        let file = std::fs::File::create(&source).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        (source, root.join("cache"))
    }
    #[test]
    fn retry_recognizes_windows_socket_errors() {
        assert!(retryable_upload_error("An existing connection was forcibly closed (os error 10054)"));
        assert!(retryable_upload_error("Connection reset by peer"));
        assert!(!retryable_upload_error("preallocate failed (console storage full?)"));
        assert!(!retryable_upload_error("cancelled"));
    }
    #[test]
    fn console_deliveries_wait_and_can_be_cancelled() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (_tx, rx) = watch::channel(false);
            let first = console_delivery_slot(&rx).await.unwrap();
            let (cancel, waiting) = watch::channel(false);
            assert!(tokio::time::timeout(Duration::from_millis(20),console_delivery_slot(&waiting)).await.is_err());
            cancel.send(true).unwrap();
            assert!(console_delivery_slot(&waiting).await.is_err());
            drop(first);
            assert!(console_delivery_slot(&rx).await.is_ok());
        });
    }
    #[test]
    fn range_coverage_and_assembly() {
        let ranges = split_ranges(101, 4);
        assert_eq!(ranges, vec![(0, 25), (25, 25), (50, 25), (75, 26)]);
        let mut bytes = Vec::new();
        for (at, n) in ranges {
            assert_eq!(at as usize, bytes.len());
            bytes.extend(std::iter::repeat_n(1u8, n as usize));
        }
        assert_eq!(bytes.len(), 101);
    }
    #[test]
    fn zip_traversal_is_rejected() {
        let (source, cache) = make_zip(vec![("../escape.pkg", vec![0x7f, 0x43, 0x4e, 0x54])]);
        assert!(extract_zip_pkgs(&source, &cache).is_err());
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }
    #[test]
    fn zip_non_pkg_is_rejected() {
        let (source, cache) = make_zip(vec![("bad.pkg", b"not pkg".to_vec())]);
        assert!(extract_zip_pkgs(&source, &cache).is_err());
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }
    #[test]
    fn zip_valid_pkg_is_discovered() {
        let (source, cache) = make_zip(vec![("CUSA12345.pkg", vec![0x7f, 0x43, 0x4e, 0x54, 1])]);
        let found = extract_zip_pkgs(&source, &cache).unwrap();
        assert_eq!(found.len(), 1);
        let _ = std::fs::remove_dir_all(source.parent().unwrap());
    }
    #[test]
    fn dump_roots_and_shadowmount_paths() {
        assert_eq!(
            shadowmount_dir(Some("PPSA31246"), "base").unwrap(),
            "/data/homebrew/PPSA31246"
        );
        assert_eq!(
            shadowmount_dir(Some("PPSA31246"), "backport").unwrap(),
            "/data/homebrew/backports/PPSA31246"
        );
        let root = std::env::temp_dir().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(root.join("sce_sys")).unwrap();
        std::fs::write(root.join("eboot.bin"), [1]).unwrap();
        std::fs::write(root.join("sce_sys").join("param.json"), "{}").unwrap();
        assert!(is_game_dump(&root));
        assert_eq!(find_dump_root(&root).as_deref(), Some(root.as_path()));
        let files = dump_files(&root).unwrap();
        assert!(files.iter().any(|(_, relative)| relative == "eboot.bin"));
        let _ = std::fs::remove_dir_all(root);
    }
}
