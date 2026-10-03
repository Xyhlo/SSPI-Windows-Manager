mod rar_control;
use job_store::resume_checkpoint;
mod job_store;
mod receiver_notifications;
mod storage;
mod package_sources;
mod archives;
mod fpkg;
mod exfat;
mod fpkg_doctor;
mod ampr_index;
mod ampr_pack;
mod shadow_image;
mod links;
mod cloud;
mod backport;
mod debrid;
mod package_details;
mod static_catalog;
mod discovery;
mod payloads;
mod payload_catalog;
mod payload_autostart;
mod web_launcher;
mod updater;
mod ps4_theme;
mod ps4_protocol;
mod ps4_inbox;
mod ps4_receiver;
mod console_tools;
mod console_diagnostics;
mod pkg_meta;
mod pkg_server;
#[cfg(test)]
mod ps4_fake_ftp;
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
const RECEIVER_VERSION: &str = "1.0.13";
const PS4_RECEIVER_VERSION: &str = "1.0.12";

#[derive(Debug, Clone)]
struct ReceiverEndpoint {
    console: &'static str,
    host: String,
    port: u16,
    expected_version: &'static str,
    pkg_dir: &'static str,
    dump_prefix: Option<&'static str>,
    required_capabilities: &'static [&'static str],
}
impl ReceiverEndpoint {
    fn redact(&self, error: impl ToString) -> String {
        redact_delivery_error(error, if self.console == "PS4" { "ps4" } else { "ps5" })
    }
    fn ps5(s: &Settings) -> Self {
        Self { console: "PS5", host: s.ps5_host.clone(), port: s.ps5_port, expected_version: RECEIVER_VERSION,
            pkg_dir: "/user/data/tmp", dump_prefix: Some("/data/homebrew"), required_capabilities: &[] }
    }
    fn ps4(s: &Settings) -> Self {
        Self { console: "PS4", host: s.ps4_host.clone(), port: s.ps4_receiver_port, expected_version: PS4_RECEIVER_VERSION,
            pkg_dir: "/user/data/sspi-receiver/upload", dump_prefix: None,
            required_capabilities: &["pkg-preflight", "pkg-install", "url-install", "parallel-upload", "verify", "title-context", "progress-notifications", "install-control", "stop", "ps4", "installed-library-v1"] }
    }
    fn path_body(&self, path: &str) -> Vec<u8> {
        let mut body = path.as_bytes().to_vec();
        if self.console == "PS4" { body.push(0); }
        body
    }
}

// The receiver owns one AppInst status slot; serialize console deliveries, while
// downloads/extraction and the lanes within each delivery remain concurrent.
static CONSOLE_DELIVERY: AsyncMutex<()> = AsyncMutex::const_new(());
static PACKAGING_WORK: AsyncMutex<()> = AsyncMutex::const_new(());

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
const PACKAGES_CACHE: &str = "packages-v5";
const SECRET_SERVICE: &str = "SimplePs5Installer.GameSearch";
const CHUNK: usize = 8 * 1024 * 1024;
const MAX_COVER_BYTES: u64 = 10 * 1024 * 1024;
const MAX_COVER_CACHE: u64 = 500 * 1024 * 1024;
const RECEIVER_ELF: &[u8] = include_bytes!("../../../../Build-Output/Windows Manager/sspi_receiver.elf");
const PS4_RECEIVER_ELF: &[u8] = include_bytes!("../../../../Build-Output/Windows Manager/sspi_ps4_receiver.elf");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    ps5_host: String,
    ps5_port: u16,
    #[serde(default = "default_ps5_loader_port")] ps5_loader_port: u16,
    #[serde(default = "default_console")] active_console: String,
    #[serde(default)] ps4_host: String,
    #[serde(default = "default_ps4_transport")] ps4_transport: String,
    #[serde(default = "default_receiver_port")] ps4_receiver_port: u16,
    #[serde(default = "default_ps4_loader_port")] ps4_loader_port: u16,
    #[serde(default = "default_ps4_serve_port")] ps4_serve_port: u16,
    #[serde(default = "default_ps4_port")] ps4_ftp_port: u16,
    #[serde(default)] ps4_ftp_user: String,
    #[serde(default)] ps4_ftp_password_configured: bool,
    #[serde(default = "default_ps4_archive_mode")] ps4_archive_mode: String,
    #[serde(default = "default_keep_packages")] ps4_remove_after_install: bool,
    resolver_base_url: String,
    download_dir: String,
    onboarding_complete: bool,
    real_debrid_enabled: bool,
    real_debrid_configured: bool,
    #[serde(default)] torbox_enabled: bool,
    #[serde(default)] torbox_configured: bool,
    #[serde(default)] alldebrid_enabled: bool,
    #[serde(default)] alldebrid_configured: bool,
    theme: String,
    reduce_motion: bool,
    #[serde(default = "default_transfer_mode")]
    transfer_mode: String,
    #[serde(default = "default_upload_lanes")]
    upload_lanes: u32,
    /// Package extracted game dumps into a single FPKG before delivery.
    #[serde(default)]
    package_dumps: bool,
    #[serde(default)] download_package_only: bool,
    #[serde(default)] keep_archives: bool,
    #[serde(default)] keep_extractions: bool,
    #[serde(default = "default_keep_packages")] keep_packages: bool,
    /// Remove from list / Clear inactive never delete a finished package when set.
    #[serde(default = "default_keep_packages")] keep_packages_on_remove: bool,
    /// Speed/size preset: fastest | balanced | smallest.
    #[serde(default = "default_fpkg_preset", deserialize_with = "fpkg::deserialize_preset")]
    fpkg_preset: String,
    #[serde(default)]
    fpkg_compression_level: Option<i8>,
    #[serde(default)]
    fpkg_doctor: bool,
    /// PFS filesystem version: 2 is safe everywhere, 3 needs FW >= 7.00.
    #[serde(default = "default_pfs_version")]
    fpkg_pfs_version: u8,
    /// Optional explicit path to the packaging engine executable.
    #[serde(default)]
    fpkg_engine_path: String,
    /// Target console firmware, used to gate PFS v3 and report readiness.
    #[serde(default)]
    target_fw: String,
    /// Legacy preference; downloaded dumps are now removed only after confirmed installation.
    #[serde(default)]
    fpkg_cleanup_source: bool,
    /// Dump packaging output: `fpkg` (installable package) or `exfat` (ShadowMount Plus image).
    #[serde(default = "default_package_format")]
    package_format: String,
    /// Lizard (AMPR/LZ4) asset packing inside exFAT images. Experimental, off by default.
    #[serde(default)]
    lizard_packing: bool,
    /// What adding game folders does: ask | package | package-send | send.
    #[serde(default = "default_folder_action")]
    folder_action: String,
}
fn default_folder_action() -> String { "ask".into() }
fn default_package_format() -> String { "fpkg".into() }
fn default_keep_packages() -> bool { true }
fn default_console() -> String { "ps5".into() }
fn default_ps4_transport() -> String { "receiver".into() }
fn default_receiver_port() -> u16 { 9114 }
fn default_ps4_loader_port() -> u16 { 9090 }
fn default_ps5_loader_port() -> u16 { 9021 }
fn default_ps4_serve_port() -> u16 { 9115 }
fn default_ps4_port() -> u16 { 2121 }
fn default_ps4_archive_mode() -> String { "pc".into() }
fn default_transfer_mode() -> String {
    "balanced".into()
}
fn default_upload_lanes() -> u32 {
    4
}
fn default_fpkg_preset() -> String {
    "balanced".into()
}
fn default_pfs_version() -> u8 {
    2
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ps5_host: String::new(),
            ps5_port: 9114,
            ps5_loader_port: default_ps5_loader_port(),
            active_console: default_console(), ps4_host: String::new(), ps4_ftp_port: default_ps4_port(),
            ps4_transport: default_ps4_transport(), ps4_receiver_port: default_receiver_port(),
            ps4_loader_port: default_ps4_loader_port(), ps4_serve_port: default_ps4_serve_port(),
            ps4_ftp_user: String::new(), ps4_ftp_password_configured: false,
            ps4_archive_mode: default_ps4_archive_mode(), ps4_remove_after_install: true,
            resolver_base_url: String::new(),
            download_dir: dirs(),
            onboarding_complete: false,
            real_debrid_enabled: false,
            real_debrid_configured: false,
            torbox_enabled: false, torbox_configured: false,
            alldebrid_enabled: false, alldebrid_configured: false,
            theme: "dark".into(),
            reduce_motion: false,
            transfer_mode: default_transfer_mode(),
            upload_lanes: default_upload_lanes(),
            package_dumps: false,
            download_package_only: false, keep_archives: false, keep_extractions: false, keep_packages: true, keep_packages_on_remove: true,
            fpkg_preset: default_fpkg_preset(),
            fpkg_compression_level: None,
            fpkg_doctor: false,
            fpkg_pfs_version: default_pfs_version(),
            fpkg_engine_path: String::new(),
            target_fw: String::new(),
            fpkg_cleanup_source: false,
            package_format: default_package_format(),
            lizard_packing: false,
            folder_action: default_folder_action(),
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
    retry: Arc<Mutex<job_store::Store>>,
    http: Client,
    // B3: per-title serialization for concurrent resolves; waiters re-check fresh cache.
    resolving: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct PackagingInfo {
    preset: String,
    #[serde(default)]
    compression_level: i8,
    #[serde(default)]
    doctor: Option<fpkg_doctor::DoctorReport>,
    #[serde(default)]
    doctor_applied: bool,
    pfs_version: u8,
    threads: u16,
    input_bytes: u64,
    file_count: u64,
    output_bytes: u64,
    output_path: String,
    #[serde(default)] temp_path: String,
    elapsed_seconds: f64,
    log: Vec<String>,
    #[serde(default)] activity: String,
    #[serde(default)] phase_progress: Option<f64>,
    #[serde(default)] compression_input_bytes: Option<u64>,
    #[serde(default)] compression_output_bytes: Option<u64>,
    #[serde(default)] speed_bps: Option<f64>,
    #[serde(default)] last_activity_seconds: Option<f64>,
    #[serde(default)] heartbeat: bool,
    /// Engine telemetry (`stages-io-v1`): stages with durations, measured process I/O,
    /// files and the current file. The last snapshot is kept as the build summary.
    #[serde(default)] engine: Option<Value>,
    /// Doctor, staging and AMPR repair before the engine started.
    #[serde(default)] workspace_seconds: Option<f64>,
    /// Wall time from workspace preparation to the verified package.
    #[serde(default)] total_seconds: Option<f64>,
    /// Output format: empty or `fpkg` for packages, `exfat` for ShadowMount images.
    #[serde(default)] format: String,
    /// Lizard (AMPR/LZ4) packing summary for images built with it.
    #[serde(default)] lizard: Option<Value>,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
struct Progress {
    target: String,
    package_only: bool,
    removed: bool,
    components: Vec<job_store::Component>,
    work_paths: Vec<PathBuf>,
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
    #[serde(default)]
    local_pkg: bool,
    created_at: u64,
    stage_history: Vec<String>,
    paused: bool,
    packaging: Option<PackagingInfo>,
    provider_preparation: Option<debrid::PreparationProgress>,
    retryable: bool,
    space: Option<storage::SpacePlan>,
}
impl Default for Progress {
    fn default() -> Self {
        Self {
            target: String::new(),
            package_only: false,
            removed: false,
            components: Vec::new(),
            work_paths: Vec::new(),
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
            local_pkg: false,
            created_at: 0,
            stage_history: Vec::new(),
            paused: false,
            packaging: None,
            provider_preparation: None,
            retryable: false,
            space: None,
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
#[derive(Serialize, Deserialize, Clone, Default)]
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
    file_name: String,
    kind: String,
    size: u64,
    title_id: Option<String>,
    package_kind: Option<String>,
    version: Option<String>,
    icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalPkgMetadata {
    title_id: String,
    title: Option<String>,
    package_kind: Option<String>,
    version: Option<String>,
    icon: Option<String>,
    file_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalJobSeed {
    name: String,
    title_id: Option<String>,
    package_kind: String,
    version: String,
    icon: Option<String>,
    file_name: String,
    file_size: u64,
    local_pkg: bool,
}

fn read_local_pkg_metadata(path: &Path) -> Option<LocalPkgMetadata> {
    let meta = pkg_meta::read(path).ok()?;
    Some(LocalPkgMetadata {
        title_id: meta.title_id,
        title: meta.title.filter(|value| !value.trim().is_empty()),
        package_kind: matches!(meta.kind.as_str(), "base" | "update" | "dlc" | "theme").then_some(meta.kind),
        version: meta.version.filter(|value| !value.trim().is_empty()),
        icon: meta.icon0.map(|bytes| format!("data:image/png;base64,{}", BASE64.encode(bytes))),
        file_size: meta.file_size,
    })
}

fn local_job_seed(
    path: &Path,
    metadata: Option<LocalPkgMetadata>,
    kind_override: Option<String>,
    title_override: Option<String>,
) -> LocalJobSeed {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (fallback_name, fallback_version, fallback_icon) = if metadata.is_some() {
        (file_name.clone(), String::new(), None)
    } else {
        manual_metadata(path)
    };
    let package_kind = kind_override
        .filter(|value| !value.trim().is_empty())
        .or_else(|| metadata.as_ref().and_then(|meta| meta.package_kind.clone()))
        .unwrap_or_else(|| manual_kind_for_name(&file_name).to_owned());
    let title_id = title_override
        .filter(|value| !value.trim().is_empty())
        .or_else(|| metadata.as_ref().map(|meta| meta.title_id.clone()))
        .or_else(|| dump_title_id(path))
        .or_else(|| pkg_content_id(path).and_then(|cid| {
            cid.split(|c: char| !c.is_ascii_alphanumeric())
                .find(|value| title_id(value))
                .map(str::to_owned)
        }))
        .or_else(|| title_from_path(path));
    let file_size = metadata.as_ref().map(|meta| meta.file_size)
        .or_else(|| std::fs::metadata(path).ok().map(|stat| stat.len()))
        .unwrap_or_else(|| if path.is_dir() { dir_bytes(path) } else { 0 });
    LocalJobSeed {
        name: metadata.as_ref().and_then(|meta| meta.title.clone()).unwrap_or(fallback_name),
        title_id,
        package_kind,
        version: metadata.as_ref().and_then(|meta| meta.version.clone()).unwrap_or(fallback_version),
        icon: metadata.and_then(|meta| meta.icon).or(fallback_icon),
        file_name,
        file_size,
        local_pkg: path.is_file() && fpkg::package_magic(&read_magic_sync(path).unwrap_or([0; 8])),
    }
}

fn local_package_from_path(path: &Path, number: usize) -> LocalPackage {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let metadata = read_local_pkg_metadata(path);
    let title_id = metadata.as_ref().map(|meta| meta.title_id.clone()).or_else(|| {
        file_name.split(|c: char| !c.is_ascii_alphanumeric())
            .find(|value| title_id(&value.to_ascii_uppercase()))
            .map(|value| value.to_ascii_uppercase())
    });
    LocalPackage {
        number,
        path: path.display().to_string(),
        name: metadata.as_ref().and_then(|meta| meta.title.clone()).unwrap_or_else(|| file_name.clone()),
        file_name,
        kind: "pkg".into(),
        size: metadata.as_ref().map(|meta| meta.file_size).or_else(|| std::fs::metadata(path).ok().map(|stat| stat.len())).unwrap_or(0),
        title_id,
        package_kind: metadata.as_ref().and_then(|meta| meta.package_kind.clone()),
        version: metadata.as_ref().and_then(|meta| meta.version.clone()),
        icon: metadata.and_then(|meta| meta.icon),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveSettings {
    #[serde(default)] ps5_loader_port: Option<u16>,
    #[serde(default)] active_console: Option<String>,
    #[serde(default)] ps4_host: Option<String>,
    #[serde(default)] ps4_transport: Option<String>,
    #[serde(default)] ps4_receiver_port: Option<u16>,
    #[serde(default)] ps4_loader_port: Option<u16>,
    #[serde(default)] ps4_serve_port: Option<u16>,
    #[serde(default)] ps4_ftp_port: Option<u16>,
    #[serde(default)] ps4_ftp_user: Option<String>,
    #[serde(default)] ps4_ftp_password: Option<String>,
    #[serde(default)] ps4_archive_mode: Option<String>,
    #[serde(default)] ps4_remove_after_install: Option<bool>,
    ps5_host: String,
    ps5_port: u16,
    resolver_base_url: String,
    download_dir: String,
    onboarding_complete: bool,
    real_debrid_enabled: bool,
    theme: String,
    reduce_motion: bool,
    real_debrid_token: Option<String>,
    #[serde(default)] torbox_token: Option<String>,
    #[serde(default)] alldebrid_token: Option<String>,
    #[serde(default)] torbox_enabled: Option<bool>,
    #[serde(default)] alldebrid_enabled: Option<bool>,
    #[serde(default)]
    transfer_mode: Option<String>,
    #[serde(default)]
    upload_lanes: Option<u32>,
    #[serde(default)]
    package_dumps: Option<bool>,
    #[serde(default)] download_package_only: Option<bool>,
    #[serde(default)] keep_archives: Option<bool>,
    #[serde(default)] keep_extractions: Option<bool>,
    #[serde(default)] keep_packages: Option<bool>,
    #[serde(default)] keep_packages_on_remove: Option<bool>,

    #[serde(default)]
    fpkg_preset: Option<String>,
    #[serde(default)]
    fpkg_compression_level: Option<i8>,
    #[serde(default)]
    fpkg_doctor: Option<bool>,
    #[serde(default)]
    fpkg_pfs_version: Option<u8>,
    #[serde(default)]
    fpkg_engine_path: Option<String>,
    #[serde(default)]
    target_fw: Option<String>,
    #[serde(default)]
    fpkg_cleanup_source: Option<bool>,
    #[serde(default)] package_format: Option<String>,
    #[serde(default)] lizard_packing: Option<bool>,
    #[serde(default)] folder_action: Option<String>,
}
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DeliveryRequest {
    #[serde(default)] target: Option<String>,
    #[serde(default)] transport: Option<String>,
    package: Package,
    title_id: Option<String>,
    #[serde(default)]
    title_name: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    archive_parts: Vec<Package>,
    #[serde(default)]
    backport: Option<BackportInput>,
    #[serde(default)] provider: Option<String>,
    /// Overrides Settings → Package extracted dumps for this request (batch folder actions).
    #[serde(default, skip_serializing_if = "Option::is_none")] package_dumps: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone)]
struct BackportInput { package: Package, #[serde(default)] parts: Vec<Package> }

fn delivery_target(target: Option<&str>) -> Result<&str, String> {
    match target.unwrap_or("ps5") {
        "ps5" => Ok("ps5"), "ps4" => Ok("ps4"),
        _ => Err("Console must be ps5 or ps4.".into()),
    }
}
fn ps4_transport(request: &DeliveryRequest) -> &str { request.transport.as_deref().unwrap_or("inbox") }
fn delivery_can_package(request: &DeliveryRequest) -> bool {
    !ps4_title_id(&request.title_id.as_deref().unwrap_or("").to_ascii_uppercase())
        && !request.package.expected_content_id.to_ascii_uppercase()
            .split(|c: char| !c.is_ascii_alphanumeric()).any(ps4_title_id)
}
fn delivery_packages(request: &DeliveryRequest, settings: &Settings) -> bool {
    delivery_can_package(request) && request.package_dumps.unwrap_or(settings.package_dumps)
}
fn delivery_package_only(request: &DeliveryRequest, settings: &Settings) -> bool {
    delivery_packages(request, settings) && settings.download_package_only
}
fn snapshot_transport(request: &mut DeliveryRequest, settings: &Settings, retry: bool) -> Result<(), String> {
    if request.target.as_deref() == Some("ps4") {
        if !retry { request.transport = Some(settings.ps4_transport.clone()); }
        if !matches!(ps4_transport(request), "receiver" | "inbox") { return Err("PS4 transport must be receiver or inbox.".into()); }
    }
    Ok(())
}
fn ps4_receiver_job(app: &AppHandle, job: &str) -> bool {
    app.try_state::<AppState>().is_some_and(|state| state.retry.lock().unwrap().records.get(job)
        .and_then(|record| record.request.as_ref()).is_some_and(|r| r.target.as_deref() == Some("ps4") && ps4_transport(r) == "receiver"))
}

fn validate_delivery_target(request: &DeliveryRequest, package_only: bool, dump: bool) -> Result<&'static str, String> {
    if package_only { return Ok(""); }
    if delivery_target(request.target.as_deref())? == "ps5" { return Ok("ps5"); }
    if request.title_id.as_deref().unwrap_or("").to_ascii_uppercase().contains("PPSA")
        || request.package.expected_content_id.to_ascii_uppercase().contains("PPSA") {
        return Err("PS5 games can't be installed on a PS4.".into());
    }
    if request.backport.is_some() || request.package.kind.eq_ignore_ascii_case("backport") {
        return Err("Backports apply to PS5 games only.".into());
    }
    if dump { return Err("PS4 delivery takes PKG files. Game folders can only be sent to a PS5.".into()); }
    Ok("ps4")
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
    redact_delivery_error(error, "ps5")
}
fn redact_delivery_error(error: impl ToString, target: &str) -> String {
    let s = error.to_string();
    let s = s.replace("Bearer ", "Bearer [redacted]");
    if target == "ps5" && (s.to_ascii_lowercase().contains("early eof")
        || s.contains("UnexpectedEof")
        || s.contains("connection reset"))
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
        && (ps4_title_id(s) || s.starts_with("PPSA"))
        && s.as_bytes()[4..].iter().all(u8::is_ascii_digit)
}

// Legacy IDs identify PS2 classics packaged for PS4, using the same PKG transport.
fn ps4_title_id(s: &str) -> bool {
    s.len() == 9
        && ["CUSA", "SLUS", "SLES", "SCUS", "SCES", "SLPS", "SLPM", "SCPS", "SCAJ", "SLAJ", "SLKA", "SLKS", "SCKA"]
            .iter().any(|prefix| s.starts_with(prefix))
        && s.as_bytes()[4..].iter().all(u8::is_ascii_digit)
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
    if p.target.is_empty() { p.target = prev.target.clone(); }
    if p.components.is_empty() { p.components = prev.components.clone(); }
    for path in &prev.work_paths { if !p.work_paths.contains(path) { p.work_paths.push(path.clone()); } }
    if p.title.is_empty() { p.title = prev.title.clone(); }
    if p.icon.is_none() { p.icon = prev.icon.clone(); }
    if p.title_id.is_empty() { p.title_id = prev.title_id.clone(); }
    if p.package_kind.is_empty() { p.package_kind = prev.package_kind.clone(); }
    if p.package_label.is_empty() { p.package_label = prev.package_label.clone(); }
    if p.package_version.is_empty() { p.package_version = prev.package_version.clone(); }
    p.local_pkg |= prev.local_pkg;
    if p.space.is_none() { p.space = prev.space.clone(); }
    p.retryable = prev.retryable;
    if p.packaging.is_none() { p.packaging = prev.packaging.clone(); }
    if let Some(details) = p.packaging.as_mut() {
        if let Some(old) = prev.packaging.as_ref() { details.log = old.log.clone(); }
    }
    if p.stage == "packaging" && prev.stage == "packaging" && p.progress == 0. { p.progress = prev.progress; }
    p.created_at = prev.created_at;
    p.stage_history = prev.stage_history.clone();
    p.paused = prev.paused && !terminal_stage(&p.stage);
    if p.target == "ps4" && matches!(p.stage.as_str(), "handoff" | "installing") { p.paused = false; }
}

fn emit(app: &AppHandle, mut p: Progress) {
    if p.bytes_done > 0 && matches!(p.stage.as_str(), "downloading" | "extracting" | "uploading") && p.progress < 1. {
        static METERS: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
        let mut meters = METERS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
        let key = format!("{}:{}:{}", p.job_id, p.stage, p.paused);
        if meters.get(&key).is_some_and(|last| last.elapsed() < Duration::from_millis(200)) { return; }
        if meters.len() > 1024 { meters.retain(|_, time| time.elapsed() < Duration::from_secs(60)); }
        meters.insert(key, Instant::now());
    }
    let terminal = terminal_stage(&p.stage);
    if let Some(state) = app.try_state::<AppState>() {
        let mut jobs = state.jobs.lock().unwrap();
        if state.retry.lock().unwrap().records.get(&p.job_id).is_some_and(|record| record.progress.removed) { return; }
        if let Some(prev) = jobs.get(&p.job_id) {
            inherit_progress_context(&mut p, prev);
            if terminal && p.stage != "complete" && p.bytes_total == 0 {
                p.bytes_done = prev.bytes_done;
                p.bytes_total = prev.bytes_total;
                p.progress = prev.progress;
            }
        }
        if p.stage == "packaging" {
            if let Some(info) = p.packaging.as_mut() {
                if !info.heartbeat && info.log.last() != Some(&p.message) { info.log.push(p.message.clone()); }
                if info.log.len() > 40 { info.log.remove(0); }
            }
        }
        if p.created_at == 0 {
            p.created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        }
        if !terminal && !p.stage_history.contains(&p.stage) {
            p.stage_history.push(p.stage.clone());
        }
        {
            let mut store = state.retry.lock().unwrap();
            if let Some(record) = store.records.get_mut(&p.job_id) {
                p.package_only = record.package_only;
                p.retryable = true;
                let persist = record.progress.stage != p.stage || record.progress.work_paths != p.work_paths || terminal;
                record.progress = p.clone();
                if persist { if let Err(error) = store.save(&p.job_id) { eprintln!("Retry journal: {error}"); } }
            }
        }
        jobs.insert(p.job_id.clone(), p.clone());
        if terminal {
            state.cancel.lock().unwrap().remove(&p.job_id);
        }
    }
    if p.target != "ps4" || ps4_receiver_job(app, &p.job_id) { receiver_notifications::observe(app, &p); }
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
        assert!(pausable_stage("extracting"));
        assert!(pausable_stage("packaging"));
        for stage in ["submitting", "installing", "mounting", "complete"] { assert!(!pausable_stage(stage)); }
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
        "complete" | "failed" | "cancelled" | "monitoring-ended" | "delivered"
    )
}

fn job_error_stage(error: &str) -> &'static str {
    if error.starts_with(ps4_receiver::MONITORING_ENDED) {
        "monitoring-ended"
    } else if error == "cancelled" || error.ends_with(": cancelled") {
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
        let stage = value["stage"].as_str().filter(|stage| !stage.is_empty()).map(|stage| format!(" {stage}")).unwrap_or_default();
        let code = code.unwrap_or(-1);
        // PlayGo slot errors: the receiver already retried; a fresh receiver process clears them.
        let hint = match code as u32 {
            0x80B2_116F => ". PlayGo INVALID_SLOT: the PS5 installer had no free slot after three tries. Reload the receiver (Tools > Payloads), then Retry.",
            0x80B2_100D | 0x80B2_100E => ". PlayGo is not ready. Wait for other installs on the PS5 to finish, then Retry.",
            _ => "",
        };
        // Console error codes are reported and searched for in hex.
        Err(format!("Install submission failed: {error}{stage} (code 0x{:08X}, {code}){hint}", code as u32))
    }
}

#[derive(Debug, PartialEq)]
enum InstallDecision {
    Installing { status: String, progress: f64 },
    Complete,
    Unavailable,
}

fn install_decision(value: &Value) -> Result<InstallDecision, String> {
    let state = value["state"].as_str().unwrap_or("unknown");
    let status = value["status"].as_str().unwrap_or(state);
    let error = value["error"].as_str().unwrap_or("");
    let error_code = value["error_code"].as_i64().unwrap_or(0);
    // Failure to query AppInst (including an older receiver's initialization error)
    // is not a failed installation. Credentials can also fail to restore after success.
    if value["status_api_code"].as_i64().is_some_and(|code| code != 0)
        || value.get("stage").is_some() || state == "unconfirmed" {
        return Ok(InstallDecision::Unavailable);
    }
    if state == "failed" || error_code != 0 {
        return Err(format!(
            "Install failed: {status} {error} ({error_code})"
        ));
    }
    let api_ok = value["status_api_code"].as_i64().map(|code| code == 0)
        .unwrap_or_else(|| value["api_code"].as_i64().unwrap_or(0) == 0);
    let fully_installed = matches!(status, "installed" | "complete")
        || (status == "playable" && value["progress"].as_f64().unwrap_or(0.) >= 100.);
    if state == "complete" && fully_installed && api_ok
        && (error.is_empty() || value["auth_restore_code"].as_i64().is_some_and(|code| code != 0)) {
        return Ok(InstallDecision::Complete);
    }
    if !api_ok || appinst_idle(status) || state != "installing" { return Ok(InstallDecision::Unavailable); }
    Ok(InstallDecision::Installing {
        status: if status.is_empty() {
            "installing".into()
        } else {
            status.to_owned()
        },
        progress: (value["progress"].as_f64().unwrap_or(0.) / 100.).clamp(0., 1.),
    })
}

const INSTALL_CONFIRM_GRACE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, PartialEq)]
enum InstallOutcome {
    Complete,
    LibraryConfirmed,
    Installing { status: String, progress: f64 },
    Waiting,
    Unconfirmed,
    Failed(String),
}

fn classify_install_outcome(value: Option<&Value>, cid: &str, library_confirmed: bool, unanswered: Duration) -> InstallOutcome {
    if let Some(value) = value.filter(|value| value["content_id"].as_str().is_none_or(|id| id == cid)) {
        match install_decision(value) {
            Ok(InstallDecision::Complete) => return InstallOutcome::Complete,
            Ok(InstallDecision::Installing { status, progress }) => return InstallOutcome::Installing { status, progress },
            Err(error) => return InstallOutcome::Failed(error),
            Ok(InstallDecision::Unavailable) => {},
        }
    }
    if library_confirmed { InstallOutcome::LibraryConfirmed }
    else if unanswered >= INSTALL_CONFIRM_GRACE { InstallOutcome::Unconfirmed }
    else { InstallOutcome::Waiting }
}

fn install_library_entry_matches(entry: &Value, title: &str, cid: &str, version: Option<&str>, kind: &str) -> bool {
    if !title_id(title) || entry["titleId"].as_str() != Some(title) || !matches!(kind, "base" | "update") { return false; }
    if entry["sources"].as_array().is_some_and(|sources| !sources.is_empty()
        && !sources.iter().any(|source| source == "app" || source == "appdb")) { return false; }
    let content = entry["contentId"].as_str().filter(|id| !id.is_empty());
    if content.is_some_and(|id| id != cid) { return false; }
    let wanted = version.filter(|version| !version.trim().is_empty());
    let installed = entry[if kind == "update" { "updateVersion" } else { "baseVersion" }].as_str()
        .or_else(|| if kind == "base" { entry["version"].as_str() } else { None });
    if let Some(wanted) = wanted {
        // Numeric components allow zero-padding differences, never a different release.
        let components = |text: &str| text.trim().split('.').map(str::parse::<u32>).collect::<Result<Vec<_>, _>>().ok();
        if !installed.is_some_and(|actual| actual == wanted || components(actual).is_some_and(|a| Some(a) == components(wanted))) { return false; }
        return true;
    }
    // A title alone (or a base title when installing an update/DLC) proves too little.
    kind == "base" && content == Some(cid)
}

async fn install_cancellable<T>(tx: &mut watch::Receiver<bool>, work: impl std::future::Future<Output = T>) -> Result<T, String> {
    tokio::pin!(work);
    loop {
        if *tx.borrow() { return Err("cancelled".into()); }
        tokio::select! {
            biased;
            changed = tx.changed() => { if changed.is_err() || *tx.borrow() { return Err("cancelled".into()); } },
            result = &mut work => return Ok(result),
        }
    }
}

async fn confirm_install_from_library(endpoint: &ReceiverEndpoint, title: &str, cid: &str, version: Option<&str>, kind: &str) -> bool {
    let target = if endpoint.console == "PS4" { "ps4" } else { "ps5" };
    let Ok(snapshot) = console_tools::list_console_library(target.into(), endpoint.host.clone(), endpoint.port).await else { return false; };
    // Reuse the Library page's validated inventory/metadata path; its fields are private.
    let Ok(snapshot) = serde_json::to_value(snapshot) else { return false; };
    snapshot["entries"].as_array().is_some_and(|entries| entries.iter()
        .any(|entry| install_library_entry_matches(entry, title, cid, version, kind)))
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactKind {
    Pkg,
    Zip,
    Rar,
    SevenZ,
    Unknown,
}

fn artifact_kind(header: &[u8], name: &str, content_type: &str) -> ArtifactKind {
    if fpkg::package_magic(header) {
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

const RAR_PASSWORDS: [&[u8]; 7] = [b"", b"[DLPSGAME.COM]", b"DLPSGAME.COM", b"www.DLPSGAME.COM", b"[dlpsgame.com]", b"dlpsgame.com", b"www.dlpsgame.com"];

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
    let _library = rar_control::library_lock(&|| Ok(()))?;
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
    rar_control::extract(path, dest, password, &|| Ok(()))
}

fn extract_rar_builtin(
    source: &Path,
    dest: &Path,
    password: Option<&str>,
    progress: std::sync::Arc<dyn Fn(u64, u64, f64) + Send + Sync>,
    checkpoint: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<(), String> {
    let guessed = rar_first_volume(source);
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
        let _library = rar_control::library_lock(checkpoint.as_ref())?;
        let probe = if password.is_empty() {
            unrar::Archive::new(&path)
        } else {
            unrar::Archive::with_password(&path, password)
        };
        if let Ok(open) = probe.open_for_listing() {
            if open.volume_info() == unrar::VolumeInfo::Subsequent {
                return Err(format!(
                    "{} is not the first volume of the set; the first volume ({}) was not downloaded or was renamed",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    guessed.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            break;
        }
    }
    std::fs::create_dir_all(dest).map_err(redact)?;
    // File lengths include UnRAR's preallocation. Only PROCESSDATA callbacks
    // measure decompressed bytes; never scan the destination to infer progress.
    let mut listed = 0u64;
    for password in archive_passwords(password) {
        if let Ok(size) = rar_list_size(&path, password) {
            listed = size;
            break;
        }
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let processed = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let processed_watch = processed.clone();
    let stop_watch = stop.clone();
    let progress_watch = progress.clone();
    let poller = std::thread::spawn(move || {
        let mut previous = 0u64;
        let mut sampled_at = Instant::now();
        while !stop_watch.load(Ordering::Relaxed) {
            let done = processed_watch.load(Ordering::Relaxed);
            let speed = done.saturating_sub(previous) as f64 / sampled_at.elapsed().as_secs_f64().max(0.01);
            previous = done;
            sampled_at = Instant::now();
            progress_watch(if listed > 0 { done.min(listed) } else { done }, listed, speed);
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    let mut last = "Unable to extract RAR; check the password and all archive volumes.".to_string();
    let result = (|| {
        for password in archive_passwords(password) {
            checkpoint()?;
            processed.store(0, Ordering::Relaxed);
            match rar_control::extract_measured(&path, dest, password, checkpoint.as_ref(), &processed) {
                Ok(_) => return Ok(()),
                Err(error) if error.starts_with("RAR password error") => last = error,
                Err(error) => return Err(error),
            }
        }
        Err(last)
    })();
    stop.store(true, Ordering::Relaxed);
    let _ = poller.join();
    if result.is_ok() {
        let done = processed.load(Ordering::Relaxed);
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
            if file.read(&mut magic).unwrap_or(0) == 4 && fpkg::package_magic(&magic) {
                found.push(path);
            }
        }
    }
    Ok(found)
}

fn pkg_content_id(path: &Path) -> Option<String> { fpkg::package_identity(path).ok() }

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
    message: &str,
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
                message: message.into(),
                ..Default::default()
            },
        );
    }
    Ok(())
}

fn is_game_dump(root: &Path) -> bool {
    root.join("eboot.bin").is_file() && root.join("sce_sys").is_dir()
}

fn is_doctor_dump(root: &Path) -> bool {
    root.join("sce_sys/param.json").is_file()
        && (root.join("decrypted/eboot.bin.esbak").is_file() || root.join("eboot.bin.esbak").is_file())
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
    if is_game_dump(root) || is_doctor_dump(root) {
        return Some(root.to_path_buf());
    }
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if is_game_dump(&dir) || is_doctor_dump(&dir) {
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
            // In an old-style set the .rar is the first volume and .r00 the second;
            // .r00 only leads when the set has no .rar.
            if name.ends_with(".r00") && rar_first_volume(&path).is_file() {
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

/// First volume of the RAR set that `path` belongs to. Old-style sets are
/// `name.rar`, `name.r00`, `name.r01`, …, so their first volume is `name.rar`;
/// unrar's own guess for `name.r00` is `name.r01`, a continuation volume.
/// `.partN.rar` and `.NNN` sets keep unrar's guess.
fn rar_first_volume(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if let Some(caps) = Regex::new(r"^(.+)\.([rR])\d{2,3}$").ok().and_then(|regex| regex.captures(name)) {
        let extension = if &caps[2] == "R" { "RAR" } else { "rar" };
        return path.with_file_name(format!("{}.{extension}", &caps[1]));
    }
    unrar::Archive::new(path).as_first_part().filename().to_path_buf()
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
    let mut settings = state.settings.lock().unwrap().clone();
    settings.ps4_ftp_password_configured = provider_configured("ps4-ftp");
    settings
}

#[cfg(test)]
mod console_target_tests {
    use super::*;
    #[test]
    fn delivery_errors_only_show_receiver_socket_hint_for_ps5() {
        for cause in ["early eof", "UnexpectedEof", "connection reset"] {
            let error = format!("FTP: {cause}; Bearer test-token");
            let ps4 = redact_delivery_error(&error, "ps4");
            assert_eq!(ps4, error.replace("Bearer ", "Bearer [redacted]"));
            assert!(!ps4.contains("PS5")); assert!(!ps4.contains("ELF"));
            let ps5 = redact_delivery_error(&error, "ps5");
            assert_eq!(ps5, format!("PS5 closed the socket (receiver timed out or died). Reload the ELF and retry. [{ps4}]"));
            assert_eq!(redact(&error), ps5);
            assert_eq!(redact_delivery_error(&error, ""), ps4);
        }
    }
    fn request(target: Option<&str>) -> DeliveryRequest {
        DeliveryRequest { transport: None, target: target.map(str::to_string), package: Package { kind: "base".into(), ..Default::default() },
            title_id: Some("CUSA12345".into()), title_name: None, icon: None, archive_parts: vec![], backport: None, provider: None, package_dumps: None }
    }
    #[test]
    fn ps4_packages_bypass_ps5_packaging_settings_for_both_targets() {
        let settings = Settings { package_dumps: true, download_package_only: true, ..Default::default() };
        for target in ["ps4", "ps5"] {
            let mut request = request(Some(target));
            request.package.url = "https://example.test/game.pkg?download=1".into();
            for explicit in [None, Some(true), Some(false)] {
                request.package_dumps = explicit;
                assert!(!delivery_packages(&request, &settings));
                assert!(!delivery_package_only(&request, &settings));
                assert_eq!(validate_delivery_target(&request, delivery_package_only(&request, &settings), false).unwrap(), target);
            }
        }
        let mut request = request(Some("ps5"));
        for id in ["SLUS20062", "SLES50044", "SCUS97124", "SLPM65001"] {
            request.title_id = Some(id.into());
            assert!(title_id(id));
            assert!(!delivery_package_only(&request, &settings));
            assert_eq!(title_from_path(Path::new(&format!("Example-{id}.pkg"))).as_deref(), Some(id));
        }
        request.title_id = None;
        request.package.expected_content_id = "UP0000-CUSA12345_00-GAME000000000000".into();
        assert!(!delivery_package_only(&request, &settings));
        request.package.expected_content_id.clear();
        request.title_id = Some("PPSA12345".into());
        assert!(delivery_package_only(&request, &settings));
        request.target = Some("ps4".into());
        assert!(delivery_package_only(&request, &settings), "PS5 dump packaging on the PC remains independent of selected console");
        request.package_dumps = Some(false);
        assert!(!delivery_package_only(&request, &settings));
    }
    #[test]
    fn ps4_rejects_ps5_titles_backports_and_folders() {
        let mut request = request(Some("ps4")); request.title_id = Some("PPSA12345".into());
        assert_eq!(validate_delivery_target(&request, false, false).unwrap_err(), "PS5 games can't be installed on a PS4.");
        request.title_id = Some("CUSA12345".into()); request.package.kind = "backport".into();
        assert_eq!(validate_delivery_target(&request, false, false).unwrap_err(), "Backports apply to PS5 games only.");
        request.package.kind = "base".into(); request.backport = Some(BackportInput { package: Package::default(), parts: vec![] });
        assert_eq!(validate_delivery_target(&request, false, false).unwrap_err(), "Backports apply to PS5 games only.");
        request.backport = None;
        assert_eq!(validate_delivery_target(&request, false, true).unwrap_err(), "PS4 delivery takes PKG files. Game folders can only be sent to a PS5.");
        assert_eq!(validate_delivery_target(&request, false, false).unwrap(), "ps4");
        request.package.expected_content_id = "UP0000-PPSA12345_00-TEST000000000000".into();
        assert_eq!(validate_delivery_target(&request, false, false).unwrap_err(), "PS5 games can't be installed on a PS4.");
    }
    #[test]
    fn ps5_defaults_and_package_only_keep_existing_routes() {
        for target in [None, Some("ps5")] {
            let mut request = request(target); request.title_id = Some("PPSA12345".into()); request.package.kind = "backport".into();
            assert_eq!(validate_delivery_target(&request, false, true).unwrap(), "ps5");
        }
        let request = request(Some("unknown"));
        assert!(validate_delivery_target(&request, false, false).is_err());
        assert_eq!(validate_delivery_target(&request, true, true).unwrap(), "");
    }
    #[test]
    fn old_settings_requests_and_progress_deserialize_with_console_defaults() {
        let mut settings = serde_json::to_value(Settings::default()).unwrap();
        let fields: Vec<_> = settings.as_object().unwrap().keys().filter(|k| k.starts_with("ps4") || *k == "activeConsole").cloned().collect();
        for field in fields { settings.as_object_mut().unwrap().remove(&field); }
        let restored: Settings = serde_json::from_value(settings).unwrap();
        assert_eq!(restored.active_console, "ps5"); assert_eq!(restored.ps4_ftp_port, 2121);
        assert_eq!(restored.ps4_archive_mode, "pc"); assert!(restored.ps4_remove_after_install);
        let mut request = serde_json::to_value(request(None)).unwrap(); request.as_object_mut().unwrap().remove("target");
        let restored: DeliveryRequest = serde_json::from_value(request).unwrap();
        assert_eq!(delivery_target(restored.target.as_deref()).unwrap(), "ps5");
        let progress: Progress = serde_json::from_value(json!({"stage":"installing"})).unwrap(); assert!(progress.target.is_empty());
        let previous = Progress { target: "ps4".into(), ..Default::default() };
        let mut next = Progress { stage: "handoff".into(), ..Default::default() }; inherit_progress_context(&mut next, &previous);
        assert_eq!(next.target, "ps4"); assert!(terminal_stage("delivered")); assert!(!terminal_stage("handoff")); assert!(!pausable_stage("handoff"));
    }
}
#[tauri::command]
fn set_active_console(app: AppHandle, state: State<AppState>, console: String) -> Result<Settings, String> {
    delivery_target(Some(&console))?;
    let mut current = state.settings.lock().unwrap();
    let mut next = current.clone();
    next.active_console = console;
    next.ps4_ftp_password_configured = provider_configured("ps4-ftp");
    write_settings(&app, &next)?;
    *current = next.clone();
    Ok(next)
}
#[tauri::command]
async fn test_ps4(host: String, port: u16, user: Option<String>, password: Option<String>) -> Result<ps4_inbox::Ps4Probe, String> {
    ps4_inbox::probe(host, port, user, password).await
}
#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<AppState>,
    input: SaveSettings,
) -> Result<Settings, String> {
    if let Some(console) = &input.active_console { delivery_target(Some(console))?; }
    if input.ps5_loader_port == Some(0) { return Err("PS5 loader port must be between 1 and 65535.".into()); }
    ps4_receiver::validate_settings(input.ps4_transport.as_deref(), input.ps4_receiver_port, input.ps4_loader_port, input.ps4_serve_port)?;
    if input.ps4_ftp_port == Some(0) { return Err("PS4 FTP port must be between 1 and 65535.".into()); }
    if input.ps4_archive_mode.as_deref().is_some_and(|mode| !matches!(mode, "pc" | "ps4")) {
        return Err("PS4 archive mode must be pc or ps4.".into());
    }
    if input.folder_action.as_deref().is_some_and(|action| !matches!(action, "ask" | "package" | "package-send" | "send")) {
        return Err("Folder action must be ask, package, package-send or send.".into());
    }
    if input.package_format.as_deref().is_some_and(|format| !matches!(format, "fpkg" | "exfat")) {
        return Err("Package format must be fpkg or exfat.".into());
    }
    if !input.ps5_host.trim().is_empty() {
        validate_receiver_candidate(&input.ps5_host, input.ps5_port)?;
    }
    if !input.resolver_base_url.is_empty() && !valid_http(&input.resolver_base_url) {
        return Err("Resolver URL must be http/https".into());
    }
    if input.download_dir.trim().is_empty() {
        return Err("Download folder is required".into());
    }
    fpkg::validate_compression_level(input.fpkg_compression_level)?;
    if let Some(v) = input.real_debrid_token.filter(|v| !v.trim().is_empty()) {
        secret("real-debrid")?.set_password(&v).map_err(redact)?;
    }
    for (provider, token) in [("torbox", input.torbox_token), ("alldebrid", input.alldebrid_token)] {
        if let Some(token) = token.filter(|t| !t.trim().is_empty()) { secret(provider)?.set_password(token.trim()).map_err(redact)?; }
    }
    let mut s = state.settings.lock().unwrap();
    if let Some(password) = input.ps4_ftp_password.filter(|v| !v.trim().is_empty()) {
        secret("ps4-ftp")?.set_password(&password).map_err(redact)?;
    }
    *s = Settings {
        active_console: input.active_console.unwrap_or_else(|| s.active_console.clone()),
        ps4_host: input.ps4_host.map(|v| v.trim().to_string()).unwrap_or_else(|| s.ps4_host.clone()),
        ps4_transport: input.ps4_transport.unwrap_or_else(|| s.ps4_transport.clone()),
        ps4_receiver_port: input.ps4_receiver_port.unwrap_or(s.ps4_receiver_port),
        ps4_loader_port: input.ps4_loader_port.unwrap_or(s.ps4_loader_port),
        ps4_serve_port: input.ps4_serve_port.unwrap_or(s.ps4_serve_port),
        ps4_ftp_port: input.ps4_ftp_port.unwrap_or(s.ps4_ftp_port),
        ps4_ftp_user: input.ps4_ftp_user.map(|v| v.trim().to_string()).unwrap_or_else(|| s.ps4_ftp_user.clone()),
        ps4_ftp_password_configured: provider_configured("ps4-ftp"),
        ps4_archive_mode: input.ps4_archive_mode.unwrap_or_else(|| s.ps4_archive_mode.clone()),
        ps4_remove_after_install: input.ps4_remove_after_install.unwrap_or(s.ps4_remove_after_install),
        ps5_host: input.ps5_host.trim().into(),
        ps5_port: input.ps5_port,
        ps5_loader_port: input.ps5_loader_port.unwrap_or(s.ps5_loader_port),
        resolver_base_url: input.resolver_base_url.trim_end_matches('/').into(),
        download_dir: input.download_dir.trim().into(),
        onboarding_complete: input.onboarding_complete,
        real_debrid_enabled: input.real_debrid_enabled,
        torbox_enabled: input.torbox_enabled.unwrap_or(s.torbox_enabled),
        torbox_configured: provider_configured("torbox"),
        alldebrid_enabled: input.alldebrid_enabled.unwrap_or(s.alldebrid_enabled),
        alldebrid_configured: provider_configured("alldebrid"),
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
        package_dumps: input.package_dumps.unwrap_or(false),
        download_package_only: input.download_package_only.unwrap_or(s.download_package_only),
        keep_archives: input.keep_archives.unwrap_or(s.keep_archives),
        keep_extractions: input.keep_extractions.unwrap_or(s.keep_extractions),
        keep_packages: input.keep_packages.unwrap_or(s.keep_packages),
        keep_packages_on_remove: input.keep_packages_on_remove.unwrap_or(s.keep_packages_on_remove),

        fpkg_preset: fpkg::PackagePreset::from_label(input.fpkg_preset.as_deref().unwrap_or("balanced")).label().into(),
        fpkg_compression_level: input.fpkg_compression_level,
        fpkg_doctor: input.fpkg_doctor.unwrap_or(false),
        fpkg_pfs_version: match input.fpkg_pfs_version.unwrap_or_else(default_pfs_version) {
            3 => 3,
            _ => 2,
        },
        fpkg_engine_path: input.fpkg_engine_path.unwrap_or_default().trim().to_string(),
        target_fw: input.target_fw.unwrap_or_default().trim().to_string(),
        fpkg_cleanup_source: input.fpkg_cleanup_source.unwrap_or(false),
        package_format: input.package_format.unwrap_or_else(|| s.package_format.clone()),
        lizard_packing: input.lizard_packing.unwrap_or(s.lizard_packing),
        folder_action: input.folder_action.unwrap_or_else(|| s.folder_action.clone()),
    };
    write_settings(&app, &s)?;
    Ok(s.clone())
}

#[tauri::command]
async fn inspect_package_dump(path: String) -> Result<fpkg_doctor::DoctorReport, String> {
    tokio::task::spawn_blocking(move || {
        let mut report = fpkg_doctor::inspect(Path::new(&path), &|| Ok(()))?;
        match ampr_index::validate(Path::new(&path), &|| Ok(())) {
            Ok(warnings) => for message in warnings { report.issues.push(fpkg_doctor::DoctorIssue { path: "ampr_emu.index".into(), severity: "warning".into(), message }); },
            Err(message) => report.issues.push(fpkg_doctor::DoctorIssue { path: "ampr_emu.index".into(), severity: "error".into(), message }),
        }
        Ok(report)
    }).await.map_err(redact)?
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
async fn list_community_sources(app: AppHandle) -> Result<package_sources::community::CommunityListing, String> {
    package_sources::community::list(&app).await
}

#[tauri::command]
async fn install_community_source(app: AppHandle, id: String) -> Result<Vec<package_sources::SourceSummary>, String> {
    package_sources::community::install(&app, id.trim()).await
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
    let endpoint = ReceiverEndpoint::ps5(&settings);
    ping(&endpoint).await?;
    let mut stream = connect_receiver(&endpoint, "receiver version").await?;
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
fn provider_configured(provider: &str) -> bool { secret(provider).ok().and_then(|entry| entry.get_password().ok()).is_some_and(|token| !token.trim().is_empty()) }

#[tauri::command]
async fn get_provider_hosts(state: State<'_, AppState>) -> Result<Vec<debrid::ProviderHosts>, String> {
    let settings = state.settings.lock().unwrap().clone();
    Ok(debrid::hosts(&state.http, &settings).await)
}

#[tauri::command]
async fn verify_provider(state: State<'_, AppState>, provider: String, token: Option<String>) -> Result<String, String> {
    debrid::verify(&state.http, &provider, token).await
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
        merged.retain(|game| seen.insert(format!("{}:{}:{}", game.title_id, game.region, game.source_id)));
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
    merged.retain(|game| seen.insert(format!("{}:{}:{}", game.title_id, game.region, game.source_id)));
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
    let cache_path = cache_root(&app).ok().map(|root| root.join("catalog-v5.json"));
    if !refresh.unwrap_or(false) {
        if let Some(path) = cache_path.as_ref() {
            if let Some(mut catalog) = read_cache_json::<GameCatalog>(path, 30 * 60) {
                catalog.cached = true;
                return Ok(catalog);
            }
        }
    }
    if package_sources::has_enabled(&app) {
        let local = package_sources::home_titles(&app)?;
        let titles = if local.is_empty() { package_sources::search(&app, "", 100).await } else { Ok(local) };
        match titles {
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
                    sections: [ ("ps4", "PS4 catalog", "CUSA"), ("ps5", "PS5 catalog", "PPSA") ]
                        .into_iter().map(|(id, title, prefix)| CatalogSection {
                            id: id.into(), title: title.into(), games: games.iter()
                                .filter(|g| if prefix == "CUSA" { ps4_title_id(&g.title_id) } else { g.title_id.starts_with(prefix) }).cloned().collect(),
                        }).collect(),
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
        return Err("Enter a valid PS4, PS5, or PS2 classic title ID (for example CUSA12345, PPSA12345, or SLUS12345)".into());
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
    frame_io_target(stream, cmd, body, "ps5").await
}
async fn frame_io_target(stream: &mut TcpStream, cmd: u8, body: &[u8], target: &str) -> Result<(u8, Vec<u8>), String> {
    if body.len() > CHUNK {
        return Err("frame exceeds 8 MiB".into());
    }
    let mut h = vec![cmd];
    h.extend((body.len() as u32).to_le_bytes());
    stream.write_all(&h).await.map_err(|e| redact_delivery_error(e, target))?;
    stream.write_all(body).await.map_err(|e| redact_delivery_error(e, target))?;
    let mut rh = [0; 5];
    stream.read_exact(&mut rh).await.map_err(|e| redact_delivery_error(e, target))?;
    let n = u32::from_le_bytes(rh[1..5].try_into().unwrap()) as usize;
    if n > CHUNK {
        return Err("receiver frame exceeds 8 MiB".into());
    }
    let mut b = vec![0; n];
    stream.read_exact(&mut b).await.map_err(|e| redact_delivery_error(e, target))?;
    Ok((rh[0], b))
}
async fn ping(endpoint: &ReceiverEndpoint) -> Result<(), String> {
    let mut x = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
    )
    .await
    .map_err(|_| format!("{} connection timed out", endpoint.console))?
    .map_err(|e| endpoint.redact(e))?;
    let target = if endpoint.console == "PS4" { "ps4" } else { "ps5" };
    let (r, b) = tokio::time::timeout(Duration::from_secs(10), frame_io_target(&mut x, 1, &[], target))
        .await.map_err(|_| "Receiver PING timed out".to_string())??;
    if r == 1 && b == b"SSPI" {
        Ok(())
    } else {
        Err("Unexpected receiver PING response".into())
    }
}

/// Connect with a few retries; a dead/refusing receiver becomes a clear
/// reload-the-ELF message instead of a raw OS 10061 at whatever stage called.
async fn connect_receiver(endpoint: &ReceiverEndpoint, what: &str) -> Result<TcpStream, String> {
    let mut last = String::new();
    for attempt in 0..3u32 {
        match tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
        )
        .await
        {
            Ok(Ok(tcp)) => {
                tcp.set_nodelay(true).ok();
                return Ok(tcp);
            }
            Ok(Err(e)) => last = endpoint.redact(e),
            Err(_) => last = format!("{what} timed out"),
        }
        sleep(Duration::from_secs(1 + u64::from(attempt))).await;
    }
    Err(format!("{} receiver is not answering ({what}; last: {last}). Reload the ELF on the console and retry.", endpoint.console))
}
fn remote(endpoint: &ReceiverEndpoint, id: Option<&str>) -> String {
    format!(
        "{}/upload_{}_{}.pkg",
        endpoint.pkg_dir,
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
async fn create_remote_dir(endpoint: &ReceiverEndpoint, path: &str) -> Result<(), String> {
    let mut tcp = connect_receiver(endpoint, "CREATE_DIR").await?;
    tcp.set_nodelay(true).ok();
    let (r, body) = tokio::time::timeout(Duration::from_secs(20), frame(&mut tcp, 0x04, &endpoint.path_body(path)))
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
    endpoint: &ReceiverEndpoint,
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
        match send_file_once(Some(app), &settings, endpoint, path, remote, job, tx, message,
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
    app: Option<&AppHandle>,
    s: &Settings,
    endpoint: &ReceiverEndpoint,
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
    // Dump folders upload several files at once, so each file gets fewer lanes; an image is one file.
    let dump = endpoint.dump_prefix.is_some_and(|prefix| remote.starts_with(&format!("{prefix}/")))
        && !remote.starts_with(&format!("{IMAGE_STAGING}/"));
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
        let mut tcp = connect_receiver(endpoint, "lane START_UPLOAD").await?;
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
        let app = app.cloned();
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
                if let Some(app) = &app { transfer_checkpoint(app, &job, &cancel).await?; }
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
                if let Some(app) = &app { emit(
                    app,
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
                ); }
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
        failed = verify_uploaded_file(endpoint, remote, n).await.err();
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

async fn verify_uploaded_file(endpoint: &ReceiverEndpoint, remote: &str, expected: u64) -> Result<(), String> {
    let mut stream = connect_receiver(endpoint, "uploaded file verify").await?;
    let (code, body) = frame(&mut stream, 0x55, &endpoint.path_body(remote)).await?;
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

async fn package_and_install_dump(
    app: &AppHandle, s: &Settings, root: &Path, job: &str, title_id: Option<&str>,
    kind: &str, title_name: &Option<String>, icon: &Option<String>,
    tx: &mut watch::Receiver<bool>, cleanup: bool, cleanup_extra: &[PathBuf],
) -> Result<(), String> {
    if *tx.borrow() { return Err("cancelled".into()); }
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), message: "Waiting for packaging worker".into(), ..Default::default() });
    let mut queue_cancel = tx.clone();
    let package_slot = tokio::select! { slot = PACKAGING_WORK.lock() => slot, _ = queue_cancel.changed() => return Err("cancelled".into()) };
    // Total packaging time starts once this job owns the packaging worker, not while queued.
    let packaging_started = Instant::now();
    let engine = fpkg::locate_engine(Some(&s.fpkg_engine_path))
        .ok_or("Packaging engine unavailable. Reinstall the complete SSPI distribution or select an engine in Settings.")?;
    let job_uuid = uuid::Uuid::new_v4();
    let job_root = PathBuf::from(&s.download_dir).join("packaged").join(job_uuid.to_string());
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), work_paths: vec![job_root.clone()], message: "Creating packaging workspace".into(), ..Default::default() });
    let staging = job_root.join("source");
    let source = root.to_path_buf();
    let stage_copy = staging.clone();
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(),
        message: "Preparing a package workspace; original dump retained".into(), ..Default::default() });
    let app_control = app.clone(); let job_control = job.to_string(); let control_cancel = tx.clone();
    let doctor_enabled = s.fpkg_doctor;
    let package_download_dir = s.download_dir.clone();
    let image = s.package_format == "exfat";
    let lizard = image && s.lizard_packing;
    let prepared = tokio::task::spawn_blocking(move || {
        let began = Instant::now();
        let last_report = std::cell::RefCell::new(Instant::now());
        let control = || {
            job_store::blocking_checkpoint(&app_control, &job_control, &control_cancel)?;
            if last_report.borrow().elapsed() >= Duration::from_secs(1) {
                *last_report.borrow_mut() = Instant::now();
                emit(&app_control, Progress { job_id: job_control.clone(), stage: "packaging".into(), message: format!("Preparing and validating package workspace · {}s elapsed", began.elapsed().as_secs()), ..Default::default() });
            }
            Ok(())
        };
        let doctor = if doctor_enabled {
            let report = fpkg_doctor::inspect(&source, &control)?;
            emit(&app_control, Progress { job_id: job_control.clone(), stage: "packaging".into(),
                message: format!("Doctor checked {} modules; {} repair candidates", report.scanned_modules, report.repairs.len()),
                packaging: Some(PackagingInfo { doctor: Some(report.clone()), ..Default::default() }), ..Default::default() });
            let blockers = report.blockers();
            if !blockers.is_empty() { return Err(format!("packaging/doctor: {}", blockers.join("; "))); }
            Some(report)
        } else { None };
        let (source_bytes, workspace_bytes) = match &doctor {
            Some(report) => (report.source_bytes, report.private_bytes),
            None => fpkg::workspace_size(&source)?,
        };
        storage::publish(&app_control, &job_control, storage::plan(Path::new(&package_download_dir), "packaging", 0, source_bytes,
            workspace_bytes, true, source_bytes, false))?;
        let staged = fpkg::stage_source_with_repairs(&source, &stage_copy, doctor.as_ref().map(|d| d.repairs.as_slice()).unwrap_or(&[]), &control)?;
        for warning in ampr_index::prepare_staged(&stage_copy, &control).map_err(|error| format!("packaging/backport index: {error}"))? {
            emit(&app_control, Progress { job_id: job_control.clone(), stage: "packaging".into(), message: warning, ..Default::default() });
        }
        let preflight = fpkg::preflight_staged(&stage_copy, 2_000_000, &staged)?;
        // The FPKG notes about embedding runtimes in an installed package do not apply to images.
        for warning in preflight.warnings.iter().filter(|w| !image || !(w.starts_with("Backport runtime files are embedded") || w.starts_with("ampr_emu.index is preserved"))) {
            emit(&app_control, Progress { job_id: job_control.clone(), stage: "packaging".into(), message: warning.clone(), ..Default::default() });
        }
        if image {
            // The image is about the size of the dump; Lizard packs are written into staging first.
            storage::guard_bytes(Path::new(&package_download_dir), preflight.total_bytes.saturating_mul(if lizard { 2 } else { 1 }), "exFAT image")?;
        } else {
            storage::publish(&app_control, &job_control, storage::plan(Path::new(&package_download_dir), "package output and temporary files", 0, preflight.total_bytes, 0, true, preflight.total_bytes, true))?;
        }
        if !preflight.ok() { return Err(format!("packaging/preflight: {}", preflight.blockers.join("; "))); }
        Ok((doctor, preflight))
    }).await.map_err(redact)?;
    let workspace_seconds = packaging_started.elapsed().as_secs_f64();
    let (doctor_report, preflight) = match prepared {
        Ok(value) => value,
        Err(error) => {
            let workspace = job_root.clone(); let download = PathBuf::from(&s.download_dir);
            let _ = tokio::task::spawn_blocking(move || fpkg::cleanup_extracted(&workspace, &download)).await;
            return Err(error);
        }
    };
    if image {
        let result = package_image(app, s, root, job, title_id, title_name, icon, tx, cleanup, cleanup_extra,
            packaging_started, &job_root, &staging, doctor_report, preflight, workspace_seconds).await;
        drop(package_slot);
        return result;
    }
    let mut options = fpkg::PackageOptions::new(staging.clone(), job_root.join("output"));
    options.title_id = dump_title_id(root).or_else(|| title_id.map(str::to_string));
    options.kind = fpkg::PackageKind::from_label(kind);
    options.preset = fpkg::PackagePreset::from_label(&s.fpkg_preset);
    options.compression_level = s.fpkg_compression_level;
    options.pfs_version = s.fpkg_pfs_version;
    options.target_fw = Some(s.target_fw.clone()).filter(|fw| !fw.is_empty());
    options.threads = Some(fpkg::kraken_workers());
    let temp = fpkg::choose_temp_dir(&options.output_dir, preflight.total_bytes, &fpkg::TempEnvironment::system());
    let temp_dir = temp.dir.join(format!("sspi-fpkg-{job_uuid}"));
    options.temp_dir = Some(temp_dir.clone());
    let info = PackagingInfo { preset: options.preset.label().into(), compression_level: options.effective_compression_level()?,
        doctor: doctor_report, doctor_applied: s.fpkg_doctor, pfs_version: options.effective_pfs_version(),
        threads: options.threads.unwrap(), input_bytes: preflight.total_bytes, file_count: preflight.file_count,
        temp_path: temp_dir.display().to_string(),
        workspace_seconds: Some(workspace_seconds), ..Default::default() };
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), packaging: Some(info.clone()),
        message: format!("Temporary files: {}. {}", temp_dir.display(), temp.reason), ..Default::default() });
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), packaging: Some(info.clone()),
        title: title_name.clone().unwrap_or_default(), icon: icon.clone(),
        message: format!("Packing {} files at Kraken {} with {} workers", info.file_count, info.compression_level, info.threads), ..Default::default() });
    let app_event = app.clone(); let job_event = job.to_string(); let info_event = info.clone();
    let started = Instant::now();
    let last_engine = Arc::new(Mutex::new(None::<Value>)); let engine_seen = last_engine.clone();
    let pause_app = app.clone(); let pause_job = job.to_string(); let build_cancel = tx.clone();
    let build = fpkg::build_controlled(&engine, &options, move |line| {
        let value = serde_json::from_str::<Value>(&line).ok();
        let message = value.as_ref().and_then(|v| v["message"].as_str()).unwrap_or(&line).to_string();
        let mut info = info_event.clone();
        info.elapsed_seconds = started.elapsed().as_secs_f64();
        if let Some(v) = value.as_ref() {
            info.activity = v["activity"].as_str().unwrap_or("Packaging").to_string();
            info.phase_progress = v["phaseProgress"].as_f64();
            info.compression_input_bytes = v["compressionInputBytes"].as_u64();
            info.compression_output_bytes = v["compressionOutputBytes"].as_u64();
            info.speed_bps = v["speedBps"].as_f64();
            info.last_activity_seconds = v["lastActivitySeconds"].as_f64();
            info.heartbeat = v["heartbeat"].as_bool().unwrap_or(false);
            if let Some(engine) = v.get("engine").filter(|engine| engine.is_object()) {
                info.engine = Some(engine.clone());
                *engine_seen.lock().unwrap() = Some(engine.clone());
            }
        }
        emit(&app_event, Progress { job_id: job_event.clone(), stage: "packaging".into(),
            progress: value.as_ref().and_then(|v| v["progress"].as_f64()).unwrap_or(0.),
            message, packaging: Some(info), ..Default::default() });
    }, |_, _| {}, move || {
        if *build_cancel.borrow() { return Err("cancelled".into()); }
        Ok(pause_app.state::<AppState>().jobs.lock().unwrap().get(&pause_job).is_some_and(|p| p.paused))
    });
    let outcome = match build.await {
        Ok(outcome) => outcome,
        Err(error) => {
            let workspace = job_root.clone(); let download = PathBuf::from(&s.download_dir);
            let _ = tokio::task::spawn_blocking(move || fpkg::cleanup_extracted(&workspace, &download)).await;
            return Err(if error == "cancelled" { error } else { format!("packaging: {error}") });
        }
    };
    drop(package_slot);
    // Move the verified package out of the disposable workspace before anything records
    // its path, so removing this transfer's leftovers can never delete it.
    let built = outcome.output;
    let package_title = title_name.clone().filter(|name| !name.trim().is_empty()).or_else(|| options.title_id.clone()).unwrap_or_else(|| "Package".into());
    let package_dir = Path::new(&s.download_dir).join("FPKG").join(fpkg::package_folder_name(&package_title, options.title_id.as_deref()));
    let package = tokio::task::spawn_blocking(move || fpkg::relocate_package(&built, &package_dir)).await.map_err(redact)??;
    let size = std::fs::metadata(&package).map_err(redact)?.len();
    job_store::checkpoint(app, job, job_store::Checkpoint::Package { path: package.clone(), dump: Some(root.to_path_buf()), cleanup, backports_embedded: true, cleanup_extra: cleanup_extra.to_vec() })?;
    transfer_checkpoint(app, job, tx).await?;
    let mut info = info;
    info.output_bytes = size; info.output_path = package.display().to_string(); info.elapsed_seconds = outcome.seconds;
    info.engine = last_engine.lock().unwrap().take(); info.total_seconds = Some(packaging_started.elapsed().as_secs_f64());
    info.activity = "Finalized FIH package verified".into(); info.phase_progress = Some(1.); info.heartbeat = false;
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), progress: 1., packaging: Some(info),
        message: "Finalized FIH package verified".into(), ..Default::default() });
    // Persist the verified package checkpoint before releasing downloaded inputs. The package
    // now lives outside job_root, so the whole workspace (staging, engine work files) goes.
    let mut disposable = vec![job_root.clone()];
    if !s.keep_extractions {
        if cleanup { disposable.push(root.to_path_buf()); }
        disposable.extend(cleanup_extra.iter().cloned());
    }
    let mut cleanup_note = job_store::release_packaged_inputs(s.download_dir.clone(), disposable).await;
    if s.keep_extractions { cleanup_note = "Extracted files retained by settings.".into(); }
    if job_store::package_only(app, job) {
        emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1., bytes_done: size, bytes_total: size,
            message: format!("FPKG ready: {}. {cleanup_note}", package.display()), ..Default::default() });
        return Ok(());
    }
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), progress: 1., message: cleanup_note.clone(), ..Default::default() });
    upload(app, s, &ReceiverEndpoint::ps5(s), &package, job, options.title_id.as_deref(), tx, "FPKG", false, 0, 0).await?;
    let message = format!("FPKG installation confirmed. {cleanup_note}");
    emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1.,
        bytes_done: size, bytes_total: size, message, ..Default::default() });
    Ok(())
}

/// ShadowMount Plus exFAT image (optionally with Lizard asset packs) from the prepared staging copy.
#[allow(clippy::too_many_arguments)]
async fn package_image(
    app: &AppHandle, s: &Settings, root: &Path, job: &str, requested_id: Option<&str>,
    title_name: &Option<String>, icon: &Option<String>, tx: &mut watch::Receiver<bool>, cleanup: bool, cleanup_extra: &[PathBuf],
    packaging_started: Instant, job_root: &Path, staging: &Path, doctor: Option<fpkg_doctor::DoctorReport>,
    preflight: fpkg::Preflight, workspace_seconds: f64,
) -> Result<(), String> {
    let id = dump_title_id(root).or_else(|| requested_id.map(str::to_ascii_uppercase)).filter(|id| title_id(id))
        .ok_or("An exFAT image needs a CUSA or PPSA title ID in sce_sys/param.json")?;
    let file_name = shadow_image::image_name(staging, &id);
    let title = title_name.clone().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| id.clone());
    let output_dir = Path::new(&s.download_dir).join("ShadowMount").join(fpkg::package_folder_name(&title, Some(&id)));
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16);
    let runtime = if s.lizard_packing { shadow_image::ampr_runtime() } else { None };
    let info = PackagingInfo { preset: "exfat".into(), format: "exfat".into(), doctor, doctor_applied: s.fpkg_doctor,
        threads: workers as u16, input_bytes: preflight.total_bytes, file_count: preflight.file_count,
        workspace_seconds: Some(workspace_seconds), ..Default::default() };
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), packaging: Some(info.clone()),
        title: title_name.clone().unwrap_or_default(), icon: icon.clone(),
        message: format!("Building {file_name}{}", if s.lizard_packing { " with Lizard asset packing" } else { "" }), ..Default::default() });
    let (app_event, job_event, info_event, cancel) = (app.clone(), job.to_string(), info.clone(), tx.clone());
    let (staged, out, name, label, lizard) = (staging.to_path_buf(), output_dir.clone(), file_name.clone(), id.clone(), s.lizard_packing);
    let built = tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let control = || job_store::blocking_checkpoint(&app_event, &job_event, &cancel);
        let request = shadow_image::Request { staged: &staged, output_dir: &out, file_name: &name, label: &label, lizard,
            runtime: runtime.as_deref(), workers };
        shadow_image::build(&request, &control, &mut |engine, message| {
            let index = engine["stageIndex"].as_f64().unwrap_or(1.) - 1.;
            let count = engine["stageCount"].as_f64().unwrap_or(1.).max(1.);
            let progress = ((index + engine["stageProgress"].as_f64().unwrap_or(0.)) / count).clamp(0., 0.999);
            let mut info = info_event.clone();
            info.elapsed_seconds = started.elapsed().as_secs_f64();
            info.activity = message.clone();
            info.engine = Some(engine);
            emit(&app_event, Progress { job_id: job_event.clone(), stage: "packaging".into(), progress, message, packaging: Some(info), ..Default::default() });
        }).map(|built| (built, started.elapsed().as_secs_f64()))
    }).await.map_err(redact)?;
    let (built, seconds) = match built {
        Ok(value) => value,
        Err(error) => {
            let workspace = job_root.to_path_buf(); let download = PathBuf::from(&s.download_dir);
            let _ = tokio::task::spawn_blocking(move || fpkg::cleanup_extracted(&workspace, &download)).await;
            return Err(if error == "cancelled" { error } else { format!("packaging/exFAT image: {error}") });
        }
    };
    for warning in &built.warnings {
        emit(app, Progress { job_id: job.into(), stage: "packaging".into(), message: warning.clone(), ..Default::default() });
    }
    job_store::checkpoint(app, job, job_store::Checkpoint::Package { path: built.path.clone(), dump: Some(root.to_path_buf()), cleanup, backports_embedded: true, cleanup_extra: cleanup_extra.to_vec() })?;
    transfer_checkpoint(app, job, tx).await?;
    let mut info = info;
    info.output_bytes = built.bytes; info.output_path = built.path.display().to_string(); info.elapsed_seconds = seconds;
    info.engine = Some(built.engine.clone()); info.lizard = built.lizard.clone(); info.total_seconds = Some(packaging_started.elapsed().as_secs_f64());
    info.activity = "exFAT image verified".into(); info.phase_progress = Some(1.);
    emit(app, Progress { job_id: job.into(), stage: "packaging".into(), progress: 1., packaging: Some(info),
        message: format!("exFAT image verified: {} files, {:.2} GiB of game data in {}", built.files, built.payload_bytes as f64 / 1_073_741_824., built.path.display()), ..Default::default() });
    let mut disposable = vec![job_root.to_path_buf()];
    if !s.keep_extractions {
        if cleanup { disposable.push(root.to_path_buf()); }
        disposable.extend(cleanup_extra.iter().cloned());
    }
    let mut cleanup_note = job_store::release_packaged_inputs(s.download_dir.clone(), disposable).await;
    if s.keep_extractions { cleanup_note = "Extracted files retained by settings.".into(); }
    if job_store::package_only(app, job) {
        emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1., bytes_done: built.bytes, bytes_total: built.bytes,
            message: format!("ShadowMount image ready: {}. {cleanup_note}", built.path.display()), ..Default::default() });
        return Ok(());
    }
    let delivered = deliver_image(app, s, &built.path, job, tx).await?;
    emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1., bytes_done: built.bytes, bytes_total: built.bytes,
        message: format!("{delivered} {cleanup_note}"), ..Default::default() });
    Ok(())
}

/// Receiver folder ShadowMount Plus skips (dot-prefixed); images wait here until verified.
const IMAGE_STAGING: &str = "/data/homebrew/.sspi-incoming";

#[derive(Debug, PartialEq)]
enum ImageUpload { Send, AlreadyThere(String), Refused(String) }

/// What the receiver's image status allows: an existing image is never replaced (ShadowMount
/// may have it mounted); the same size counts as delivered earlier.
fn image_upload_plan(status: &Value, name: &str, size: u64) -> ImageUpload {
    let final_path = format!("/data/homebrew/{name}");
    if status["image"]["exists"].as_bool() == Some(true) {
        if status["image"]["size"].as_u64() == Some(size) {
            return ImageUpload::AlreadyThere(format!("{final_path} is already on the PS5 with the same size; it was kept."));
        }
        return ImageUpload::Refused(format!("{final_path} already exists on the PS5 with a different size. ShadowMount Plus may have it mounted. Remove or rename it on the console, then retry."));
    }
    // A partial copy from an earlier attempt is overwritten, so its bytes count as free.
    let staged_bytes = status["staged"]["size"].as_u64().unwrap_or(0);
    if let Some(free) = status["free"].as_u64().filter(|free| *free > 0) {
        let need = size.saturating_sub(staged_bytes).saturating_add(1 << 30);
        if free < need {
            return ImageUpload::Refused(format!("The image needs {:.1} GB free on the PS5 and /data has {:.1} GB.", need as f64 / 1e9, free as f64 / 1e9));
        }
    }
    ImageUpload::Send
}

async fn image_request(endpoint: &ReceiverEndpoint, op: u8, name: &str) -> Result<(u8, String), String> {
    let mut socket = connect_receiver(endpoint, "image request").await?;
    let mut body = vec![op];
    body.extend_from_slice(name.as_bytes());
    let (code, reply) = tokio::time::timeout(Duration::from_secs(60), frame(&mut socket, 0x6d, &body)).await
        .map_err(|_| "The receiver did not answer the image request".to_string())??;
    Ok((code, String::from_utf8_lossy(&reply).into_owned()))
}

/// Uploads a finished image into a folder ShadowMount Plus skips, then has the receiver move it
/// into /data/homebrew, so a scan never mounts a partial file. Returns the completion message.
pub(crate) async fn deliver_image(app: &AppHandle, s: &Settings, path: &Path, job: &str, tx: &mut watch::Receiver<bool>) -> Result<String, String> {
    let endpoint = &ReceiverEndpoint::ps5(s);
    let name = path.file_name().and_then(|n| n.to_str())
        .filter(|n| n.len() <= 200 && n.bytes().all(|b| b.is_ascii_graphic()) && n.to_ascii_lowercase().ends_with(".exfat") && !n.starts_with('.'))
        .ok_or("The image file name must be ASCII without spaces and end in .exfat")?.to_string();
    let size = fs::metadata(path).await.map_err(redact)?.len();
    let _delivery = console_delivery_slot(tx).await?;
    test_ps5(endpoint.host.clone(), endpoint.port).await?;
    let (code, reply) = image_request(endpoint, b's', &name).await?;
    if code != 3 {
        return Err(if reply.contains("unsupported") { "Reload the SSPI receiver ELF on the PS5; the running receiver cannot deliver ShadowMount images.".into() } else { format!("Image status failed: {reply}") });
    }
    let status: Value = serde_json::from_str(&reply).map_err(|_| format!("Image status failed: {reply}"))?;
    match image_upload_plan(&status, &name, size) {
        ImageUpload::Send => {}
        ImageUpload::AlreadyThere(message) => return Ok(message),
        ImageUpload::Refused(error) => return Err(error),
    }
    let final_path = format!("/data/homebrew/{name}");
    let title = name.split(|c: char| !c.is_ascii_alphanumeric()).find(|part| title_id(part)).map(str::to_string);
    set_receiver_title(app, endpoint, job, title.as_deref(), false).await?;
    let staged = format!("{IMAGE_STAGING}/{name}");
    if let Err(error) = send_file(app, s, endpoint, path, &staged, job, tx, &format!("Uploading {name} to the PS5"), 0, size, None).await {
        // Receiver lanes release as their sockets close; then the partial copy can go.
        sleep(Duration::from_secs(2)).await;
        let _ = image_request(endpoint, b'd', &name).await;
        return Err(error);
    }
    emit(app, Progress { job_id: job.into(), stage: "uploading".into(), progress: 0.99, bytes_done: size, bytes_total: size,
        message: format!("Publishing {name} for ShadowMount Plus"), ..Default::default() });
    let (code, reply) = image_request(endpoint, b'p', &name).await?;
    if code != 1 { return Err(format!("The image was uploaded but could not be published: {reply}. The copy stays in {IMAGE_STAGING} on the PS5.")); }
    if !s.keep_packages { job_store::cleanup_installed_package(app, job, path)?; }
    Ok(format!("Image delivered to {final_path}. ShadowMount Plus mounts it on its next scan (about every 15 seconds)."))
}

#[cfg(test)]
mod image_delivery_tests {
    use super::*;

    #[test]
    fn an_existing_image_is_kept_or_refused_and_space_counts_the_partial_copy() {
        let status = |image: Value, staged: u64, free: u64| json!({"image": image, "staged": {"exists": staged > 0, "size": staged, "active": false}, "free": free});
        let gib = 1u64 << 30;
        assert_eq!(image_upload_plan(&status(json!({"exists": false, "size": 0}), 0, 60 * gib), "PPSA00001-v01.000.000.exfat", 50 * gib), ImageUpload::Send);
        assert!(matches!(image_upload_plan(&status(json!({"exists": true, "size": 50 * gib}), 0, 60 * gib), "a.exfat", 50 * gib), ImageUpload::AlreadyThere(m) if m.contains("/data/homebrew/a.exfat")));
        assert!(matches!(image_upload_plan(&status(json!({"exists": true, "size": 49 * gib}), 0, 60 * gib), "a.exfat", 50 * gib), ImageUpload::Refused(m) if m.contains("different size")));
        assert!(matches!(image_upload_plan(&status(json!({"exists": false, "size": 0}), 0, 50 * gib), "a.exfat", 50 * gib), ImageUpload::Refused(m) if m.contains("GB free")));
        // 40 GiB of an earlier partial copy is overwritten, so 11 GiB free is enough for 50 GiB.
        assert_eq!(image_upload_plan(&status(json!({"exists": false, "size": 0}), 40 * gib, 11 * gib + 1), "a.exfat", 50 * gib), ImageUpload::Send);
        // A receiver that cannot report free space does not block the upload.
        assert_eq!(image_upload_plan(&status(json!({"exists": false, "size": 0}), 0, 0), "a.exfat", 50 * gib), ImageUpload::Send);
    }

    #[tokio::test]
    async fn image_requests_carry_the_operation_and_name() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = [0u8; 5];
                socket.read_exact(&mut header).await.unwrap();
                let mut body = vec![0; u32::from_le_bytes(header[1..].try_into().unwrap()) as usize];
                socket.read_exact(&mut body).await.unwrap();
                assert_eq!(header[0], 0x6d);
                let reply: &[u8] = if body[0] == b's' { br#"{"image":{"exists":false,"size":0},"staged":{"exists":false,"size":0,"active":false},"free":1}"# } else { b"OK {}" };
                let mut response = vec![if body[0] == b's' { 3 } else { 1 }];
                response.extend_from_slice(&(reply.len() as u32).to_le_bytes());
                response.extend_from_slice(reply);
                socket.write_all(&response).await.unwrap();
                seen.push(body);
            }
            seen
        });
        let settings = Settings { ps5_host: "127.0.0.1".into(), ps5_port: port, ..Settings::default() };
        let endpoint = ReceiverEndpoint::ps5(&settings);
        let (code, reply) = image_request(&endpoint, b's', "PPSA00001.exfat").await.unwrap();
        assert_eq!(code, 3);
        assert!(serde_json::from_str::<Value>(&reply).is_ok());
        assert_eq!(image_request(&endpoint, b'p', "PPSA00001.exfat").await.unwrap().0, 1);
        assert_eq!(server.await.unwrap(), vec![b"sPPSA00001.exfat".to_vec(), b"pPPSA00001.exfat".to_vec()]);
    }
}

async fn set_receiver_title(app: &AppHandle, endpoint: &ReceiverEndpoint, job: &str, title: Option<&str>, require_fih: bool) -> Result<(), String> {
    let Some(id) = title.filter(|id| title_id(id)) else { return Ok(()); };
    let mut socket = connect_receiver(endpoint, "receiver capabilities").await?;
    let (_, body) = frame(&mut socket, 0x53, &[]).await?;
    let config: Value = serde_json::from_slice(&body).map_err(redact)?;
    let supports = |name: &str| config["capabilities"].as_array().is_some_and(|items| items.iter().any(|v| v == name));
    if require_fih && !supports("fih-install") { return Err("Reload the new SSPI receiver ELF before installing a packaged PS5 dump (FIH support required)".into()); }
    if !supports("title-context") { return Ok(()); }
    let row = app.state::<AppState>().jobs.lock().unwrap().get(job).cloned();
    let title = row.as_ref().map(|p| p.title.as_str()).filter(|s| !s.is_empty()).unwrap_or(id);
    let name: String = title.chars().filter(|c| !c.is_control()).scan(0usize, |bytes, c| { *bytes += c.len_utf8(); (*bytes < 210).then_some(c) }).collect();
    let icon = row.as_ref().and_then(|p| p.icon.as_deref()).filter(|s| valid_http(s) && s.len() < 1000).unwrap_or("");
    let mut body = Vec::new();
    for value in [id, name.as_str(), icon] { body.extend_from_slice(value.as_bytes()); body.push(0); }
    let (code, reply) = frame(&mut socket, 0x57, &body).await?;
    if code != 1 { return Err(format!("Receiver title metadata rejected: {}", String::from_utf8_lossy(&reply))); }
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
    let endpoint = &ReceiverEndpoint::ps5(s);
    validate_mountable_dump(root)?;
    let _delivery = console_delivery_slot(tx).await?;
    test_ps5(s.ps5_host.clone(), s.ps5_port).await?;
    let title = dump_title_id(root)
        .or_else(|| title.map(str::to_ascii_uppercase).filter(|id| title_id(id)));
    set_receiver_title(app, endpoint, job, title.as_deref(), false).await?;
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
    let mut control = connect_receiver(endpoint, "dump preflight").await?;
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
    create_remote_dir(endpoint, &remote_root).await?;
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
                &ReceiverEndpoint::ps5(&settings),
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
    if ping(endpoint).await.is_err() {
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
                if ping(endpoint).await.is_err() {
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
    let endpoint = &ReceiverEndpoint::ps5(s);
    let mut tcp = connect_receiver(endpoint, "dump mount").await?;
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
    endpoint: &ReceiverEndpoint,
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
    if job_store::package_only(app, job) {
        let size = fs::metadata(path).await.map_err(redact)?.len();
        if announce_complete { emit(app, Progress { job_id: job.into(), stage: "complete".into(), progress: 1., bytes_done: set_offset + size, bytes_total: set_total.max(set_offset + size), message: format!("Package saved locally: {}", path.display()), ..Default::default() }); }
        return Ok(());
    }
    let _delivery = console_delivery_slot(tx).await?;
    test_ps5(endpoint.host.clone(), endpoint.port).await?;
    let (header, _, _) = file_header(path).await?;
    set_receiver_title(app, endpoint, job, title, header.starts_with(&[0x7f,b'F',b'I',b'H'])).await?;
    let n = fs::metadata(path).await.map_err(redact)?.len();
    let total = set_total.max(set_offset + n).max(1);
    let done = set_offset + n;
    let target = remote(endpoint, title);
    let mut control = connect_receiver(endpoint, "install preflight").await?;
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
        endpoint,
        path,
        &target,
        job,
        tx,
        &format!("Uploading {label} PKG to {}", endpoint.console),
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
    let mut submit = connect_receiver(endpoint, "install submission").await?;
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
    let label = format!("{label} [{cid}]");
    let metadata = pkg_meta::read(path).ok();
    let install_title = cid.split(|c: char| !c.is_ascii_alphanumeric()).find(|id| title_id(id))
        .or(title).unwrap_or("");
    let version = metadata.as_ref().and_then(|meta| meta.version.as_deref());
    let kind = metadata.as_ref().map(|meta| meta.kind.as_str()).unwrap_or_else(|| pkg_role_label(path));
    let mut last_answer = Instant::now();
    let mut last_library_check = None::<Instant>;
    let mut receiver_lost = false;
    let mut last_progress = 0.;
    loop {
        install_cancellable(tx, sleep(Duration::from_secs(2))).await?;
        let remaining = INSTALL_CONFIRM_GRACE.saturating_sub(last_answer.elapsed());
        let poll = async {
            let mut socket = connect_receiver(endpoint, "install status").await?;
            frame(&mut socket, 0x51, cid.as_bytes()).await
        };
        let reply = install_cancellable(tx, tokio::time::timeout(remaining.min(Duration::from_secs(15)), poll)).await?;
        let (answered, parsed) = match reply {
            Ok(Ok((_code, body))) => (true, serde_json::from_slice::<Value>(&body).ok()),
            _ => (false, None),
        };
        receiver_lost |= !answered;
        let mut outcome = classify_install_outcome(parsed.as_ref(), &cid, false, last_answer.elapsed());
        if matches!(outcome, InstallOutcome::Waiting | InstallOutcome::Unconfirmed) {
            emit(app, Progress {
                job_id: job.into(), stage: "installing".into(), progress: last_progress,
                bytes_done: done, bytes_total: total,
                message: if !answered {
                    format!("{} receiver stopped answering after AppInst accepted {label}. Reload it in Tools > Payloads so SSPI can confirm the install.", endpoint.console)
                } else {
                    format!("AppInst accepted {label}; install status is unavailable. Checking the console's library for confirmation.")
                }, ..Default::default()
            });
            if last_library_check.is_none_or(|checked| checked.elapsed() >= Duration::from_secs(30)) {
                last_library_check = Some(Instant::now());
                let remaining = INSTALL_CONFIRM_GRACE.saturating_sub(last_answer.elapsed());
                let confirmed = install_cancellable(tx, tokio::time::timeout(remaining.min(Duration::from_secs(30)),
                    confirm_install_from_library(endpoint, install_title, &cid, version, kind))).await?.unwrap_or(false);
                outcome = classify_install_outcome(parsed.as_ref(), &cid, confirmed, last_answer.elapsed());
            }
        }
        match outcome {
            InstallOutcome::Complete | InstallOutcome::LibraryConfirmed => {
                let from_library = outcome == InstallOutcome::LibraryConfirmed;
                let mut message = if from_library { format!("{label} installation confirmed from the console's library") }
                    else { format!("{label} installation confirmed") };
                if !s.keep_packages && job_store::cleanup_installed_package(app, job, path).is_err() {
                    message.push_str(". The local package was kept because cleanup could not finish");
                }
                // Report each package's confirmation even when a set has more packages to install.
                if !announce_complete {
                    emit(app, Progress { job_id: job.into(), stage: "installing".into(), progress: 1.,
                        bytes_done: done, bytes_total: total, message: message.clone(), ..Default::default() });
                }
                return finish_pkg_install(app, job, done, total, &message, announce_complete);
            }
            InstallOutcome::Installing { status, progress } => {
                last_answer = Instant::now();
                last_progress = progress.min(0.99);
                emit(app, Progress {
                    job_id: job.into(), stage: "installing".into(), progress: last_progress,
                    bytes_done: done, bytes_total: total, message: format!("{label}: {status}"), ..Default::default()
                });
            }
            InstallOutcome::Failed(error) => return Err(error),
            InstallOutcome::Unconfirmed => return Err(format!(
                "AppInst accepted {label}, but {} SSPI could not confirm the result within 10 minutes. The install may have finished; check the {} home screen.",
                if receiver_lost { "the receiver stopped answering and" } else { "install status remained unavailable and" }, endpoint.console)),
            InstallOutcome::Waiting => {},
        }
    }
}

async fn upload_pkg_set(
    app: &AppHandle,
    s: &Settings,
    endpoint: &ReceiverEndpoint,
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
            endpoint,
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
            "Real-Debrid: 1fichier is not unlocked for this account (hoster_not_free). Re-verify the token in Settings, or the file needs its source password.".into()
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
    let mut last = "Unrestrict response did not contain an HTTP download".to_string();
    for password in archive_passwords(Some("DLPSGAME.COM")) {
        let password = std::str::from_utf8(password).map_err(redact)?;
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

fn validate_download_range(existing: u64, content_range: &str) -> Result<(), String> {
    let offset = content_range.strip_prefix("bytes ").and_then(|s| s.split('-').next()).and_then(|s| s.parse::<u64>().ok());
    if offset != Some(existing) { return Err("Server returned the wrong resume range. Retained partial file was not changed.".into()); }
    Ok(())
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
    let mut existing = fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
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
    let mut cancelled = rx.clone();
    let response = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(45), request.send()) => result,
        _ = cancelled.changed() => return Err("cancelled".into()),
    }
        .map_err(|_| network_error("Package download request timed out"))?
        .map_err(|_| network_error("Package download request"))?;
    if !(response.status().is_success() || response.status() == reqwest::StatusCode::PARTIAL_CONTENT)
    {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }
    if existing > 0 && response.status() == reqwest::StatusCode::OK { existing = 0; }
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        validate_download_range(existing, response.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()).unwrap_or(""))?;
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
    let settings = app.state::<AppState>().settings.lock().unwrap().clone();
    let plan = storage::download_plan(&settings, set_total, set_done + existing,
        !filename.as_deref().unwrap_or(url).to_ascii_lowercase().ends_with(".pkg"));
    storage::publish(app, job, plan)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(existing > 0)
        .truncate(existing == 0)
        .open(part)
        .await
        .map_err(redact)?;
    let mut stream = response.bytes_stream();
    let mut done = existing;
    let mut checked = existing;
    loop {
        if *rx.borrow() {
            return Err("cancelled".into());
        }
        transfer_checkpoint(app, job, rx).await?;
        let mut cancelled = rx.clone();
        let next = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), stream.next()) => result,
            _ = cancelled.changed() => return Err("cancelled".into()),
        };
        let chunk = match next {
            Err(_) => return Err("Download stalled for 30 seconds. Cancel and retry.".into()),
            Ok(None) => break,
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(_))) => return Err(network_error("Package download stream")),
        };
        if done.saturating_sub(checked) >= 64 * 1024 * 1024 || checked == existing {
            storage::guard_bytes(part, total.saturating_sub(done).max(chunk.len() as u64), "download")?;
            checked = done;
        }
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
    if total > 0 && done < total { return Err(format!("Download incomplete ({done} of {total} bytes). Retry resumes the retained partial file.")); }
    let mut magic = [0; 8];
    let mut reader = fs::File::open(part).await.map_err(redact)?;
    let read = reader.read(&mut magic).await.map_err(redact)?;
    Ok((magic[..read].to_vec(), content_type, filename))
}


async fn download_delivery_inputs(app2: &AppHandle, s: &Settings, http: &Client, job2: &String, request: &DeliveryRequest, parts: Vec<Package>, resume: Option<&job_store::Record>, rx: &mut watch::Receiver<bool>, index_offset: usize, slot: &str) -> Result<(PathBuf, Vec<PathBuf>, ArtifactKind), String> {
    let archive_set = request.package.archive_set_id.is_some();
    let job_title = request.title_name.clone().unwrap_or_default();
    let job_icon = request.icon.clone();
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
                let path = dir.join(format!("archive_{job2}_{slot}_{set}"));
                fs::create_dir_all(&path).await.map_err(redact)?;
                path
            } else {
                let path = dir.join(format!("archive_{job2}_{slot}"));
                fs::create_dir_all(&path).await.map_err(redact)?;
                path
            };
            let mut downloaded = Vec::new();
            let mut volume_names = Vec::new();
            let mut detected = ArtifactKind::Unknown;
            let set_total: u64 = parts.iter().filter_map(|package| package.expected_size).sum();
            let mut set_done: u64 = 0;
            let set_started = Instant::now();
            let initial = storage::download_plan(&s, set_total, 0, archive_set || !request.package.url.to_ascii_lowercase().ends_with(".pkg"));
            storage::publish(&app2, &job2, initial)?;
            for (index, package) in parts.iter().enumerate() {
                transfer_checkpoint(&app2, &job2, &rx).await?;
                if let Some(saved) = resume.as_ref().and_then(|r| r.downloads.iter().find(|f| f.index == index + index_offset && f.complete && f.path.is_file())) {
                    downloaded.push(saved.path.clone()); volume_names.push(saved.name.clone()); detected = saved.kind;
                    set_done += std::fs::metadata(&saved.path).map_err(redact)?.len();
                    emit(&app2, Progress { job_id: job2.clone(), stage: "downloading".into(), bytes_done: set_done,
                        bytes_total: set_total, message: "Reusing completed download".into(), ..Default::default() });
                    continue;
                }
                let access_type = package.access_type.to_ascii_lowercase();
                let mut url = package.url.clone();
                let needs_unlock = match access_type.as_str() {
                    "direct" => false,
                    "hosterlanding" | "hoster-landing" => true,
                    _ => !direct_package(&url),
                };
                if needs_unlock && debrid::enabled(s).is_empty() {
                    return Err(
                        "This source returned a hoster link. Enable and configure Real-Debrid, TorBox, or AllDebrid in Settings before installing."
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
                    let provider_progress = |preparation: Option<debrid::PreparationProgress>| {
                        let message = preparation.as_ref().map(|p| format!("{} · file {}/{}", p.state, index + 1, parts.len()))
                            .unwrap_or_else(|| format!("Unlocking hoster link {}/{}", index + 1, parts.len()));
                        emit(&app2, Progress {
                            job_id: job2.clone(), stage: "unlocking".into(), message,
                            provider_preparation: preparation,
                            title: job_title.clone(), icon: job_icon.clone(), ..Default::default()
                        });
                    };
                    let unrestricted = tokio::select! {
                        result = debrid::resolve(http, s, &url, request.provider.as_deref(), &provider_progress) => result?,
                        _ = rx.changed() => return Err("cancelled".into()),
                    };
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
                let part_path = resume.as_ref().and_then(|r| r.downloads.iter().find(|f| f.index == index + index_offset && !f.complete && f.path.is_file())).map(|f| f.path.clone()).unwrap_or(part_path);
                job_store::downloaded(&app2, &job2, job_store::DownloadedFile { index: index + index_offset, path: part_path.clone(), name: String::new(), kind: ArtifactKind::Unknown, complete: false })?;
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
                job_store::downloaded(&app2, &job2, job_store::DownloadedFile { index: index + index_offset, path: part_path.clone(), name: name.clone(), kind: detected, complete: true })?;
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

    Ok((primary, consumed_inputs, detected))
}

#[tauri::command]
async fn start_delivery(
    app: AppHandle,
    state: State<'_, AppState>,
    request: DeliveryRequest,
) -> Result<String, String> {
    let settings = state.settings.lock().unwrap().clone();
    validate_delivery_target(&request, delivery_package_only(&request, &settings), false)?;
    if let Some((id, resume)) = job_store::reuse_for_pair(&app, &request)? {
        if !resume { return Ok(id); }
        let request = job_store::record(&app, &id).and_then(|record| record.request).ok_or("Retained transfer request is missing")?;
        return queue_delivery(app.clone(), &state, request, Some(id)).await;
    }
    queue_delivery(app.clone(), &state, request, None).await
}

async fn queue_delivery(app: AppHandle, state: &AppState, mut request: DeliveryRequest, retry_id: Option<String>) -> Result<String, String> {
    let resume = retry_id.as_ref().and_then(|id| state.retry.lock().unwrap().records.get(id).cloned());
    let archive_set = request.package.archive_set_id.is_some();
    let mut s = state.settings.lock().unwrap().clone();
    snapshot_transport(&mut request, &s, retry_id.is_some())?;
    let receiver = request.target.as_deref() == Some("ps4") && ps4_transport(&request) == "receiver";
    // An existing record (a local import, Send to PS5 or Retry) keeps its own choice; the
    // "Download and package only" setting decides for new downloads only.
    let package_only = delivery_can_package(&request)
        && resume.as_ref().map(|r| r.package_only).unwrap_or_else(|| delivery_package_only(&request, &s));
    s.package_dumps = delivery_packages(&request, &s);
    let target = validate_delivery_target(&request, package_only, false)?.to_string();
    if target == "ps4" && !receiver {
        if let Some(saved) = resume.as_ref().and_then(|record| record.ps4_delivery.as_ref()) { ps4_inbox::validate_delivery(saved)?; }
    }
    backport::validate_request(&request)?;
    let parts = if resume.as_ref().is_some_and(|r| r.checkpoint.is_some()) { vec![] } else { delivery_parts(&request)? };
    if package_only { s.package_dumps = true; }
    if request.backport.is_some() && !s.package_dumps { return Err("Enable FPKG packaging to combine a base and backport".into()); }
    if resume.is_none() {
        let bytes = parts.iter().filter_map(|p| p.expected_size).fold(0u64, u64::saturating_add).saturating_add(request.backport.as_ref().map(|b| if b.parts.is_empty() { b.package.expected_size.unwrap_or(0) } else { b.parts.iter().filter_map(|p| p.expected_size).sum() }).unwrap_or(0));
        let plan = storage::download_plan(&s, bytes, 0, archive_set || !request.package.url.to_ascii_lowercase().ends_with(".pkg"));
        if !plan.enough { return Err(plan.message); }
    }
    if !package_only && target == "ps4" {
        if receiver { ps4_receiver::ready(&ReceiverEndpoint::ps4(&s)).await?; }
        else { ps4_inbox::ready(&s).await?; }
    }
    if !package_only && target == "ps5" { validate_receiver_candidate(&s.ps5_host, s.ps5_port).map_err(|_| {
        "PS5 receiver is not configured. Open Settings or download the receiver ELF.".to_string()
    })?;
    ping(&ReceiverEndpoint::ps5(&s))
        .await
        .map_err(|error| format!("PS5 receiver is not ready: {error}"))?; }
    let job = retry_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let (tx, mut rx) = watch::channel(false);
    {
        let mut jobs = state.jobs.lock().unwrap();
        let mut store = state.retry.lock().unwrap();
        if jobs.get(&job).is_some_and(|p| p.stage == "removing") || store.records.get(&job).is_some_and(|r| r.progress.removed) {
            return Err("This entry is being removed or was already removed".into());
        }
        let mut active = state.cancel.lock().unwrap();
        if updater::installing() { return Err("SSPI is preparing to restart for an update. Start this transfer after it reopens.".into()); }
        if active.contains_key(&job) { return Err("This job is already running".into()); }
        if active.keys().any(|id| store.records.get(id).and_then(|record| record.request.as_ref()).is_some_and(|other| job_store::overlapping_delivery(&request, other))) {
            return Err("This game already has an active transfer. Open Downloads to manage it. Use one Install with backport transfer to combine the base and backport.".into());
        }
        if !store.records.contains_key(&job) {
            store.records.insert(job.clone(), job_store::Record { ps4_delivery: None, package_only, pairing_sealed: false, progress: Progress { job_id: job.clone(), target: target.clone(), ..Default::default() },
                request: Some(request.clone()), checkpoint: None, downloads: vec![], download_dir: PathBuf::from(&s.download_dir) });
        }
        if let Some(record) = store.records.get_mut(&job) { record.pairing_sealed = false; record.package_only = package_only; }
        if target == "ps4" {
            if let Some(record) = store.records.get_mut(&job) {
                record.progress.target = target.clone(); record.progress.stage = "queued".into();
                record.progress.title_id = request.title_id.clone().unwrap_or_default();
                record.progress.package_kind = request.package.kind.clone(); record.progress.paused = false;
                jobs.insert(job.clone(), record.progress.clone());
            }
        }
        store.save(&job)?;
        active.insert(job.clone(), tx);
    }
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
                target: target.clone(),
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
                package_label: if request.backport.is_some() { format!("{} + Backport", request.package.label) } else { request.package.label.clone() },
                package_version: request.package.version.clone(),
                components: job_store::request_components(&request, &[]),
                ..Default::default()
            },
        );
        let res = async {
            transfer_checkpoint(&app2, &job2, &rx).await?;
            if target == "ps4" && !receiver && resume.as_ref().is_some_and(|r| r.ps4_delivery.is_some()) {
                return ps4_inbox::deliver(&app2, &s, &job2, vec![], false, &rx).await;
            }
            if request.backport.is_some() && !resume.as_ref().is_some_and(|r| matches!(r.checkpoint, Some(job_store::Checkpoint::Package { .. }))) {
                return backport::run(&app2, &s, &http, &job2, &request, &mut rx).await;
            }
            if let Some(checkpoint) = resume.as_ref().and_then(|r| r.checkpoint.clone()) {
                return resume_checkpoint(&app2, &s, &job2, &request, checkpoint, &mut rx).await;
            }
            let dir = PathBuf::from(&s.download_dir);
            let (primary, consumed_inputs, detected) = download_delivery_inputs(&app2, &s, &http, &job2, &request, parts, resume.as_ref(), &mut rx, 0, "base").await?;
            if detected == ArtifactKind::Pkg {
                if job_store::pairing_before_packaging(&app2, &job2)?.is_some() {
                    return Err("The base download is a prebuilt package. A backport can only be combined with an extracted game folder.".into());
                }
                let final_path = dir.join(format!(
                    "download_{}.pkg",
                    download_key(
                        &request.package.url,
                        request.title_id.as_deref(),
                        &request.package.kind
                    )
                ));
                fs::rename(&primary, &final_path).await.map_err(redact)?;
                job_store::checkpoint(&app2, &job2, job_store::Checkpoint::Package { path: final_path.clone(), dump: None, cleanup: false, backports_embedded: false, cleanup_extra: vec![] })?;
                if target == "ps4" {
                    return if receiver { ps4_receiver::deliver(&app2, &s, &job2, vec![final_path], &rx).await }
                        else { ps4_inbox::deliver(&app2, &s, &job2, vec![final_path], false, &rx).await };
                }
                let result = upload(
                    &app2,
                    &s,
                    &ReceiverEndpoint::ps5(&s),
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

                return result;
            }
            if matches!(detected, ArtifactKind::Rar | ArtifactKind::SevenZ | ArtifactKind::Zip) {
                let checkpoint = job_store::Checkpoint::Archive { primary, inputs: consumed_inputs, password: request.package.archive_password.clone() };
                job_store::checkpoint(&app2, &job2, checkpoint.clone())?;
                return resume_checkpoint(&app2, &s, &job2, &request, checkpoint, &mut rx).await;
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
                    message: redact_delivery_error(e, &target),
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
    if let Some(p) = jobs.get(&job_id).filter(|p| p.target == "ps4") {
        let receiver = state.retry.lock().unwrap().records.get(&job_id).and_then(|r| r.request.as_ref())
            .is_some_and(|r| ps4_transport(r) == "receiver");
        if !receiver && matches!(p.stage.as_str(), "handoff" | "installing") {
            return Err("Installation is managed by SSPI on the PS4.".into());
        }
        if receiver && matches!(p.stage.as_str(), "submitting" | "installing") {
            return Err("Installation is managed by the PS4 now; manage it on the console.".into());
        }
    }
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
fn pausable_stage(stage: &str) -> bool { matches!(stage, "queued" | "unlocking" | "downloading" | "extracting" | "packaging" | "uploading") }

#[tauri::command]
fn pause_job(app: AppHandle, state: State<AppState>, job_id: String, paused: bool) -> Result<(), String> {
    let event = {
        let mut jobs = state.jobs.lock().unwrap();
        let job = jobs.get_mut(&job_id).ok_or("Transfer no longer exists")?;
        if !pausable_stage(&job.stage) { return Err(format!("Pause is available while the PC is preparing or transferring files. Console installation is managed by the {}.", if job.target == "ps4" { "PS4" } else { "PS5" })); }
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
    state.jobs.lock().unwrap().values().filter(|p| !p.removed).cloned().collect()
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
        let (b, n, _size) = file_header(&supplied).await?;
        if b[..n].starts_with(b"PK")
            || b[..n].starts_with(b"Rar!")
            || b[..n].starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C])
        {
            let source = supplied.clone();
            let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../Build-Output/Windows Manager/local-import-cache");
            let kind = artifact_kind(&b[..n], &source.display().to_string(), "");
            let extracted = tokio::task::spawn_blocking(move || {
                extract_any_archive(&source, &cache, kind, |_, _, _| {})
            })
            .await
            .map_err(redact)??;
            return Ok(match extracted {
                ExtractedContent::Pkgs(packages) => tokio::task::spawn_blocking(move || {
                    packages.iter().enumerate().map(|(i, pkg)| local_package_from_path(pkg, i + 1)).collect()
                }).await.map_err(redact)?,
                ExtractedContent::Dump(root) => {
                    let name = root.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    vec![LocalPackage {
                        number: 1,
                        path: root.display().to_string(),
                        name: name.clone(),
                        file_name: name,
                        kind: "dump".into(),
                        size: 0,
                        title_id: title_from_path(&root),
                        package_kind: None,
                        version: None,
                        icon: None,
                    }]
                }
            });
        }
        if !fpkg::package_magic(&b[..n]) {
            return Err("Selected file does not have PKG magic".into());
        }
        return tokio::task::spawn_blocking(move || vec![local_package_from_path(&supplied, 1)])
            .await.map_err(redact);
    }
    let mut packages = vec![];
    let mut q = vec![supplied];
    while let Some(p) = q.pop() {
        let mut rd = fs::read_dir(&p).await.map_err(redact)?;
        while let Some(e) = rd.next_entry().await.map_err(redact)? {
            let p = e.path();
            if p.is_dir() {
                q.push(p)
            } else {
                let (b, n, _size) = file_header(&p).await?;
                if fpkg::package_magic(&b[..n]) {
                    packages.push(p)
                }
            }
        }
    }
    tokio::task::spawn_blocking(move || packages.iter().enumerate().map(|(i, pkg)| local_package_from_path(pkg, i + 1)).collect())
        .await.map_err(redact)
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
    version: String,
    icon: Option<String>,
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
            Some(magic) if fpkg::package_magic(&magic) => "pkg",
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
    let metadata = (content == "pkg").then(|| read_local_pkg_metadata(path)).flatten();
    let title_id = metadata.as_ref().map(|meta| meta.title_id.clone()).or_else(|| manual_title_for(path));
    let detected_kind = metadata.as_ref().and_then(|meta| meta.package_kind.as_deref()).unwrap_or_else(|| manual_kind_for_name(&name));
    let display_name = metadata.as_ref().and_then(|meta| meta.title.clone()).unwrap_or_else(|| name.clone());
    Some(ManualCandidate {
        path: path.display().to_string(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        detected_kind: detected_kind.into(),
        needs_title: false,
        version: metadata.as_ref().and_then(|meta| meta.version.clone()).unwrap_or_default(),
        icon: metadata.and_then(|meta| meta.icon),
        name: display_name,
        title_id,
        content: content.into(),
    })
}

/// Backport-style folders ship eboot.bin (+ fakelib/sce_module) without sce_sys.
/// They still upload as dumps; the console mount verdict comes from the ELF.
fn is_loose_dump(root: &Path) -> bool {
    root.is_dir() && root.join("eboot.bin").is_file()
}

fn manual_metadata(root: &Path) -> (String, String, Option<String>) {
    let param: Value = std::fs::read(root.join("sce_sys/param.json")).ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
    let localized = &param["localizedParameters"];
    let language = localized["defaultLanguage"].as_str().unwrap_or("en-US");
    let name = localized[language]["titleName"].as_str().or_else(|| localized["en-US"]["titleName"].as_str())
        .or_else(|| param["titleName"].as_str()).filter(|name| !name.trim().is_empty())
        .map(str::to_owned).unwrap_or_else(|| root.file_name().unwrap_or_default().to_string_lossy().into_owned());
    let version = param["contentVersion"].as_str().unwrap_or_default().to_owned();
    let icon_path = root.join("sce_sys/icon0.png");
    let icon = std::fs::metadata(&icon_path).ok().filter(|m| m.len() <= 1024 * 1024)
        .and_then(|_| std::fs::read(icon_path).ok()).map(|bytes| format!("data:image/png;base64,{}", BASE64.encode(bytes)));
    (name, version, icon)
}

fn manual_dump(root: &Path) -> ManualCandidate {
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dump")
        .to_owned();
    let detected_kind = manual_kind_for_name(&name).into();
    let (name, version, icon) = manual_metadata(root);
    let title_id = dump_title_id(root).or_else(|| manual_title_for(root));
    ManualCandidate {
        path: root.display().to_string(),
        size: dir_bytes(root),
        detected_kind, version, icon,
        needs_title: title_id.is_none(),
        name,
        title_id,
        content: "dump".into(),
    }
}

/// `\\?\D:\x` -> `D:\x` and `\\?\UNC\s\x` -> `\\s\x`, for display and Explorer.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") { return PathBuf::from(format!(r"\\{rest}")); }
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Shows a file SSPI produced in Explorer. Only paths inside the download folder qualify.
#[tauri::command]
async fn reveal_path(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let root = PathBuf::from(&state.settings.lock().unwrap().download_dir);
    let target = std::fs::canonicalize(&path).map_err(|_| "That file is no longer there.".to_string())?;
    let root = std::fs::canonicalize(&root).map_err(redact)?;
    if !target.starts_with(&root) { return Err("Only files in the download folder can be shown.".into()); }
    std::process::Command::new("explorer.exe").arg(format!("/select,{}", plain_path(&target).display())).spawn().map_err(redact)?;
    Ok(())
}

#[tauri::command]
async fn scan_manual_folder(path: String) -> Result<Vec<ManualCandidate>, String> {
    tokio::task::spawn_blocking(move || scan_manual_folder_sync(path)).await.map_err(redact)?
}

fn scan_manual_folder_sync(path: String) -> Result<Vec<ManualCandidate>, String> {
    let root = PathBuf::from(&path);
    if !root.exists() {
        return Err("Folder does not exist".into());
    }
    let mut out = Vec::new();
    if root.is_file() {
        if let Some(candidate) = classify_manual_file(&root) {
            out.push(candidate);
        }
    } else if is_game_dump(&root) || is_loose_dump(&root) || is_doctor_dump(&root) {
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
                    if is_game_dump(&p) || is_loose_dump(&p) || is_doctor_dump(&p) {
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
#[tauri::command]
async fn start_manual_install(
    app: AppHandle,
    state: State<'_, AppState>,
    items: Vec<ManualItem>,
    package_only: Option<bool>,
    target: Option<String>,
    package: Option<bool>,
) -> Result<String, String> {
    if items.is_empty() { return Err("Nothing to install".into()); }
    for item in &items {
        if !matches!(item.kind.as_str(), "base" | "update" | "dlc" | "backport") { return Err("Unknown package kind".into()); }
        if !Path::new(&item.path).exists() { return Err(format!("Missing local item: {}", item.path)); }
    }
    let mut first = None;
    for item in items {
        let id = job_store::queue_local(app.clone(), &state, PathBuf::from(item.path), Some(item.kind), item.title_id, package_only.unwrap_or(false), target.clone(), package).await?;
        first.get_or_insert(id);
    }
    first.ok_or_else(|| "Nothing was queued".into())
}

#[tauri::command]
async fn start_local_install(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    target: Option<String>,
) -> Result<String, String> {
    let source = PathBuf::from(path);
    job_store::queue_local(app.clone(), &state, source, None, None, false, target, None).await
}

pub fn run() {
    if let Err(error) = updater::recover_if_needed() {
        let message = format!("SSPI could not restore an interrupted update.\n\n{error}\n\nYour settings and downloads have not been removed.");
        eprintln!("{message}");
        #[cfg(windows)]
        {
            #[link(name = "user32")]
            extern "system" {
                fn MessageBoxW(window: *mut std::ffi::c_void, text: *const u16, caption: *const u16, flags: u32) -> i32;
            }
            let text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
            let caption: Vec<u16> = "SSPI update recovery".encode_utf16().chain(std::iter::once(0)).collect();
            unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10); }
        }
        std::process::exit(1);
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            if let Err(error) = package_sources::migrate_bundled(&handle) { eprintln!("Source migration: {error}"); }
            let launch_args: Vec<String> = std::env::args().collect();
            for pair in launch_args.windows(2).filter(|pair| pair[0] == "--import-source") {
                package_sources::install_from_path(&handle, &pair[1]).map_err(std::io::Error::other)?;
                if let Ok(root) = cache_root(&handle) { let _ = std::fs::remove_file(root.join("catalog-v5.json")); }
            }
            let settings: Settings = std::fs::read(config_path(&handle)?)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let mut retry = job_store::Store::load(handle.path().app_config_dir()?.join("jobs"));
            job_store::recover_legacy(&mut retry, Path::new(&settings.download_dir));
            let restored_jobs = retry.records.iter().filter(|(_, r)| !r.progress.removed).map(|(id, r)| (id.clone(), r.progress.clone())).collect();
            app.manage(AppState {
                settings: Arc::new(Mutex::new(settings)),
                cancel: Arc::new(Mutex::new(HashMap::new())),
                jobs: Arc::new(Mutex::new(restored_jobs)),
                retry: Arc::new(Mutex::new(retry)),
                resolving: Arc::new(AsyncMutex::new(HashMap::new())),
                http: Client::builder()
                    .user_agent("GameSearch/0.1")
                    .connect_timeout(Duration::from_secs(15))
                    .pool_idle_timeout(Duration::from_secs(30))
                    .build()?,
            });
            payload_autostart::start(handle.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            payload_catalog::get_payload_catalog,
            payload_catalog::download_catalog_payload,
            payload_autostart::get_payload_autostart,
            payload_autostart::set_payload_autostart,
            payload_autostart::run_payload_autostart,
            payload_autostart::stop_payload_autostart,
            web_launcher::get_web_launcher,
            web_launcher::start_web_launcher,
            web_launcher::stop_web_launcher,
            updater::get_update_status,
            updater::check_for_updates,
            updater::download_update,
            updater::install_update,
            console_tools::list_console_library,
            console_tools::get_title_icon,
            console_tools::set_title_icon,
            console_tools::restore_title_icon,
            console_tools::refresh_console_shell,
            console_tools::console_system_info,
            job_store::send_packaged,
            links::parse_download_links,
            links::start_link_downloads,
            cloud::list_cloud_files,
            console_diagnostics::console_kernel_log,
            console_diagnostics::console_process_control,
            ps4_theme::build_ps4_theme,
            console_diagnostics::export_zip_file,
            console_tools::list_console_themes,
            console_tools::apply_console_theme,
            console_tools::remove_console_theme,
            console_diagnostics::export_text_file,
            console_diagnostics::console_processes,
            console_diagnostics::console_log_files,
            console_diagnostics::console_read_log,
            console_diagnostics::probe_debug_services,
            console_diagnostics::start_klog_stream,
            console_diagnostics::stop_klog_stream,
            get_settings,
            save_settings,
            inspect_package_dump,
            export_receiver_payload,
            list_package_sources,
            list_community_sources,
            install_community_source,
            install_package_source,
            install_package_source_from_path,
            set_package_source_enabled,
            remove_package_source,
            test_ps5,
            test_ps4,
            ps4_receiver::test_ps4_receiver,
            ps4_receiver::list_ps4_library,
            ps4_receiver::load_ps4_receiver,
            ps4_receiver::export_ps4_receiver_payload,
            set_active_console,
            test_resolver,
            verify_real_debrid,
            verify_provider,
            get_provider_hosts,
            search_games,
            load_catalog,
            fetch_cover,
            get_game_details,
            resolve_packages,
            start_delivery,
            job_store::retry_job,
            job_store::remove_job,
            reveal_path,
            package_details::inspect_package_sizes,
            package_details::refresh_job_details,
            storage::delivery_space,
            list_jobs,
            system_drive_prefix,
            cancel_job,
            pause_job,
            scan_local_packages,
            start_local_install,
            scan_manual_folder,
            start_manual_install,
            discovery::probe_consoles,
            discovery::discover_consoles,
            payloads::list_payloads,
            payloads::add_payloads,
            payloads::update_payload,
            payloads::remove_payload,
            payloads::send_payload,
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
            verify_uploaded_file(&ReceiverEndpoint::ps5(&settings), "/data/homebrew/PPSA31246/data.bin", 4096).await.unwrap();
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
    fn plain_paths_drop_the_verbatim_prefix() {
        assert_eq!(plain_path(Path::new(r"\\?\D:\Games\FPKG\a.pkg")), PathBuf::from(r"D:\Games\FPKG\a.pkg"));
        assert_eq!(plain_path(Path::new(r"\\?\UNC\nas\share\a.pkg")), PathBuf::from(r"\\nas\share\a.pkg"));
        assert_eq!(plain_path(Path::new(r"D:\Games\a.pkg")), PathBuf::from(r"D:\Games\a.pkg"));
    }

    #[test]
    fn manual_loose_dump_detected() {
        // Backport layout: eboot.bin + fakelib, no sce_sys.
        let base = test_output_root().join(format!("gs-manual-test-{}", std::process::id()));
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
    fn missing_eboot_with_known_backup_reaches_doctor_but_cannot_upload_as_folder() {
        let root = test_output_root().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(root.join("sce_sys")).unwrap();
        std::fs::create_dir_all(root.join("decrypted")).unwrap();
        std::fs::write(root.join("sce_sys/param.json"), br#"{"titleId":"PPSA99999"}"#).unwrap();
        assert!(!is_doctor_dump(&root));
        std::fs::write(root.join("decrypted/eboot.bin.esbak"), b"candidate; doctor still validates its bytes").unwrap();
        assert!(is_doctor_dump(&root));
        assert_eq!(find_dump_root(&root).unwrap(), root);
        assert!(validate_mountable_dump(&root).is_err());
        assert_eq!(manual_dump(&root).content, "dump");
        std::fs::remove_dir_all(root).unwrap();
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
            remote(&ReceiverEndpoint::ps5(&Settings::default()), Some("CUSA12345")).starts_with("/user/data/tmp/upload_CUSA12345_"),
            true
        );
        assert_eq!(
            title_from_path(Path::new("Some Game CUSA54321 v1.00.pkg")).as_deref(),
            Some("CUSA54321")
        );
        assert!(remote(&ReceiverEndpoint::ps5(&Settings::default()), Some("PPSA12345")).contains("upload_PPSA12345_"));
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
        assert_eq!(
            accepted_submission(&json!({"api_code":-2135813777,"install_api_code":-2135813777,"state":"failed","error":"AppInst rejected PKG"})).unwrap_err(),
            "Install submission failed: AppInst rejected PKG (code 0x80B2116F, -2135813777). PlayGo INVALID_SLOT: the PS5 installer had no free slot after three tries. Reload the receiver (Tools > Payloads), then Retry."
        );
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
        assert_eq!(install_decision(&json!({
            "status_api_code":-7,
            "state":"failed",
            "status":"error"
        })).unwrap(), InstallDecision::Unavailable);
        assert_eq!(
            install_decision(&json!({
                "api_code":-99,
                "state":"installing",
                "status":"transferring",
                "progress":10
            }))
            .unwrap(),
            InstallDecision::Unavailable
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
    fn cleanup_requires_confirmed_installation() {
        for code in ["api_code", "status_api_code", "auth_restore_code"] {
            let mut value = json!({"state":"complete", "status":"installed", "progress":100});
            value[code] = json!(-7);
            assert_eq!(install_decision(&value).unwrap(), if code == "auth_restore_code" { InstallDecision::Complete } else { InstallDecision::Unavailable });
        }
        assert_eq!(install_decision(&json!({"state":"complete", "status":"playable", "progress":20})).unwrap(), InstallDecision::Unavailable);
        assert!(install_decision(&json!({"state":"complete", "status":"installed", "error_code":-7})).is_err());
    }

    #[test]
    fn accepted_install_outcomes_distinguish_lost_status_from_package_failure() {
        let cid = "UP0000-PPSA12345_00-TEST000000000000";
        assert_eq!(classify_install_outcome(None, cid, false, Duration::from_secs(599)), InstallOutcome::Waiting);
        assert_eq!(classify_install_outcome(None, cid, false, INSTALL_CONFIRM_GRACE), InstallOutcome::Unconfirmed);
        assert_eq!(classify_install_outcome(None, cid, true, INSTALL_CONFIRM_GRACE), InstallOutcome::LibraryConfirmed);
        for value in [json!({}), json!({"stage":"AppInst initialization failed", "state":"failed", "api_code":-9}),
            json!({"state":"failed", "status_api_code":-5, "error_code":-5}),
            json!({"state":"installing", "status":"none", "status_api_code":0}),
            json!({"state":"complete", "status":"installed", "content_id":"another package"})] {
            assert_eq!(classify_install_outcome(Some(&value), cid, false, Duration::ZERO), InstallOutcome::Waiting);
            assert_eq!(classify_install_outcome(Some(&value), cid, true, Duration::ZERO), InstallOutcome::LibraryConfirmed);
        }
        let working = json!({"state":"installing", "status":"transferring", "progress":42, "status_api_code":0, "content_id":cid});
        assert_eq!(classify_install_outcome(Some(&working), cid, true, INSTALL_CONFIRM_GRACE),
            InstallOutcome::Installing { status:"transferring".into(), progress:0.42 });
        let completed = json!({"state":"complete", "status":"installed", "status_api_code":0, "api_code":-4,
            "auth_restore_code":-4, "error":"SYSTEM AuthID restore failed", "content_id":cid});
        assert_eq!(classify_install_outcome(Some(&completed), cid, false, INSTALL_CONFIRM_GRACE), InstallOutcome::Complete);
        let failed = json!({"state":"failed", "status":"error", "status_api_code":0, "error_code":-7, "content_id":cid});
        assert!(matches!(classify_install_outcome(Some(&failed), cid, true, Duration::ZERO), InstallOutcome::Failed(_)));
    }

    #[test]
    fn library_confirmation_matches_identity_and_exact_installed_version() {
        let cid = "UP0000-PPSA12345_00-TEST000000000000";
        let mut entry = json!({"titleId":"PPSA12345", "contentId":cid, "baseVersion":"01.000.000", "updateVersion":"01.002.000", "version":"01.002.000"});
        assert!(install_library_entry_matches(&entry, "PPSA12345", cid, Some("1.0.0"), "base"));
        assert!(install_library_entry_matches(&entry, "PPSA12345", cid, Some("01.002.000"), "update"));
        assert!(!install_library_entry_matches(&entry, "PPSA54321", cid, Some("1.0.0"), "base"));
        assert!(!install_library_entry_matches(&entry, "PPSA12345", "different", Some("1.0.0"), "base"));
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, Some("01.003.000"), "update"));
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, Some("1.0.0"), "dlc"));
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, None, "update"));
        entry["contentId"] = Value::Null;
        assert!(install_library_entry_matches(&entry, "PPSA12345", cid, Some("1.0.0"), "base"));
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, None, "base"));
        entry["updateVersion"] = Value::Null;
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, Some("01.002.000"), "update"));
        entry["contentId"] = json!(cid); entry["baseVersion"] = Value::Null; entry["version"] = Value::Null;
        assert!(install_library_entry_matches(&entry, "PPSA12345", cid, None, "base"));
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, Some("1.0.0"), "base"));
        entry["sources"] = json!(["appmeta", "shadowmount"]);
        assert!(!install_library_entry_matches(&entry, "PPSA12345", cid, None, "base"));
        entry["sources"] = json!(["app", "appmeta"]);
        assert!(install_library_entry_matches(&entry, "PPSA12345", cid, None, "base"));
    }

    #[tokio::test]
    async fn install_grace_waits_are_interruptible_by_cancel() {
        let (sender, mut receiver) = watch::channel(false);
        let work = install_cancellable(&mut receiver, std::future::pending::<()>());
        let cancel = async { tokio::task::yield_now().await; sender.send(true).unwrap(); };
        let (result, _) = tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(work, cancel) }).await.unwrap();
        assert_eq!(result.unwrap_err(), "cancelled");
        assert_eq!(install_cancellable(&mut receiver, async { 7 }).await.unwrap_err(), "cancelled");
    }

    #[tokio::test]
    async fn install_fallback_reads_the_console_library_protocol() {
        let cid = "UP0000-PPSA12345_00-TEST000000000000";
        for (wanted, confirmed) in [("01.000.000", true), ("01.002.000", false)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut settings = Settings::default();
            settings.ps5_host = "127.0.0.1".into(); settings.ps5_port = listener.local_addr().unwrap().port();
            let peer = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let meta = serde_json::to_vec(&json!({"titleId":"PPSA12345", "contentId":cid, "contentVersion":"01.000.000"})).unwrap();
                let mut metadata = (meta.len() as u32).to_le_bytes().to_vec();
                metadata.extend(meta); metadata.extend([0u8; 8]); // Empty patch metadata and icon.
                let replies = [
                    (0x53, vec![], serde_json::to_vec(&json!({"platform":"ps5", "version":RECEIVER_VERSION, "capabilities":["installed-library-v1"]})).unwrap()),
                    (0x5e, vec![], serde_json::to_vec(&json!({"titles":["PPSA12345"], "complete":true, "truncated":false, "errors":[], "sources":{"PPSA12345":["app"]}})).unwrap()),
                    (0x5f, b"PPSA12345\0".to_vec(), metadata),
                ];
                for (command, expected, reply) in replies {
                    let mut header = [0u8; 5]; socket.read_exact(&mut header).await.unwrap();
                    assert_eq!(header[0], command);
                    let mut body = vec![0; u32::from_le_bytes(header[1..].try_into().unwrap()) as usize];
                    socket.read_exact(&mut body).await.unwrap(); assert_eq!(body, expected);
                    header[0] = 3; header[1..].copy_from_slice(&(reply.len() as u32).to_le_bytes());
                    socket.write_all(&header).await.unwrap(); socket.write_all(&reply).await.unwrap();
                }
            });
            let result = tokio::time::timeout(Duration::from_secs(3), confirm_install_from_library(
                &ReceiverEndpoint::ps5(&settings), "PPSA12345", cid, Some(wanted), "base")).await.unwrap();
            assert_eq!(result, confirmed); peer.await.unwrap();
        }
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
            let path = test_output_root().join(format!("{}.pkg", Uuid::new_v4()));
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
        let root = test_output_root().join(Uuid::new_v4().to_string());
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
    fn resumed_download_checks_the_returned_offset() {
        assert!(validate_download_range(100, "bytes 100-199/200").is_ok());
        assert!(validate_download_range(100, "bytes 0-99/200").is_err());
        assert!(validate_download_range(100, "").is_err());
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
        let root = test_output_root().join(Uuid::new_v4().to_string());
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

#[cfg(test)]
fn pkg_server_test_port() -> u16 {
    // A process-wide server must outlive each individual #[tokio::test] runtime.
    static SERVER: std::sync::OnceLock<(tokio::runtime::Runtime, u16)> = std::sync::OnceLock::new();
    SERVER.get_or_init(|| std::thread::spawn(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port(); drop(listener);
        runtime.block_on(pkg_server::ensure_started(port)).unwrap();
        (runtime, port)
    }).join().unwrap()).1
}

#[cfg(test)]
fn test_output_root() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/host-test-work");
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[cfg(test)]
mod local_pkg_metadata_tests {
    use super::*;

    #[test]
    #[ignore = "requires SSPI_TEST_LOCAL_PKG pointing at a valid local PKG"]
    fn real_pkg_metadata_reaches_manual_candidate_and_job_seed() {
        let path = PathBuf::from(std::env::var("SSPI_TEST_LOCAL_PKG").expect("SSPI_TEST_LOCAL_PKG"));
        let metadata = read_local_pkg_metadata(&path).expect("PKG metadata should validate");
        let title = metadata.title.clone().filter(|value| !value.trim().is_empty()).expect("SFO title");
        let version = metadata.version.clone().filter(|value| !value.trim().is_empty()).expect("APP_VER");
        let icon = metadata.icon.clone().expect("validated PKG icon");
        assert!(metadata.title_id.starts_with("CUSA") || metadata.title_id.starts_with("PPSA"));
        assert!(icon.starts_with("data:image/png;base64,"));

        let manual = classify_manual_file(&path).expect("manual PKG candidate");
        assert_eq!(manual.name, title);
        assert_eq!(manual.title_id.as_deref(), Some(metadata.title_id.as_str()));
        assert_eq!(manual.version, version);
        assert_eq!(manual.icon.as_deref(), Some(icon.as_str()));

        let job = local_job_seed(&path, Some(metadata), None, None);
        assert_eq!(job.name, manual.name);
        assert_eq!(job.title_id, manual.title_id);
        assert_eq!(job.version, manual.version);
        assert_eq!(job.icon, manual.icon);
        assert_eq!(job.package_kind, manual.detected_kind);
        assert!(job.local_pkg);
    }
}
