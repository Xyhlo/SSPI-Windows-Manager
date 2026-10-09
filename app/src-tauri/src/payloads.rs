//! Payload library and binloader delivery.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp::OwnedReadHalf, TcpStream},
    sync::Notify,
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{AppState, Settings, PS4_RECEIVER_ELF, RECEIVER_ELF, RECEIVER_VERSION};

const INDEX_NAME: &str = "payloads.json";
const MIN_PAYLOAD_SIZE: usize = 1;
pub(super) const MAX_PAYLOAD_SIZE: usize = 256 * 1024 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const BUILTIN_PS5: &str = "builtin:ps5-receiver";
const BUILTIN_PS4: &str = "builtin:ps4-receiver";
const MAX_LOADER_OUTPUT: usize = 32 * 1024;
const LOADER_REPLY_WAIT: Duration = Duration::from_secs(2);
const LOADER_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECEIVER_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PayloadEntry {
    pub(super) id: String,
    pub(super) name: String,
    file_name: String,
    path: String,
    size: usize,
    pub(super) sha256: String,
    pub(super) target: String,
    builtin: bool,
    added_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_sent_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    /// Parsed from the stored bytes each time the list is read; `None` for raw BIN files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elf: Option<ElfInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PayloadSendResult {
    pub(super) message: String,
    bytes: usize,
    port: u32,
    pub(super) verified: bool,
    /// Measured steps of this send, in order.
    #[serde(default)]
    steps: Vec<SendStep>,
    total_ms: u64,
    /// Socket write time for the payload bytes (connect excluded).
    send_ms: Option<u64>,
    bytes_per_second: Option<f64>,
    host: String,
    sha256: String,
}

impl PayloadSendResult {
    fn new(message: String, bytes: usize, port: u32, verified: bool) -> Self {
        Self { message, bytes, port, verified, steps: Vec::new(), total_ms: 0, send_ms: None, bytes_per_second: None, host: String::new(), sha256: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct SendStep {
    label: String,
    detail: String,
    ms: u64,
    ok: bool,
}

/// Records what a send actually did and how long each part took.
struct Trace {
    steps: Vec<SendStep>,
    sent: Option<(usize, Duration)>,
}

impl Trace {
    fn new() -> Self { Self { steps: Vec::new(), sent: None } }
    fn step(&mut self, label: impl Into<String>, detail: impl Into<String>, from: Instant, ok: bool) {
        let step = SendStep { label: label.into(), detail: detail.into(), ms: from.elapsed().as_millis() as u64, ok };
        crate::session_log::write("payload", &format!("{} | {} ms | {} | {}", step.label, step.ms, if ok { "ok" } else { "failed" }, step.detail));
        self.steps.push(step);
    }
}

/// ELF header facts shown before sending; `None` for raw BIN payloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct ElfInfo {
    class: String,
    endian: String,
    kind: String,
    machine: String,
    entry: String,
    segments: u16,
    loadable: u16,
    loadable_bytes: u64,
}

/// 1086888 -> "1,086,888" for trace details.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 { out.push(','); }
        out.push(c);
    }
    out
}

fn parse_elf(bytes: &[u8]) -> Option<ElfInfo> {
    if bytes.len() < 52 || &bytes[..4] != b"\x7fELF" || !matches!(bytes[4], 1 | 2) || !matches!(bytes[5], 1 | 2) {
        return None;
    }
    let (wide, little) = (bytes[4] == 2, bytes[5] == 1);
    // Offsets come from the file; checked_add keeps a hostile header from overflowing.
    let field = |at: usize, len: usize| at.checked_add(len).and_then(|end| bytes.get(at..end));
    let u16_at = |at: usize| field(at, 2).map(|b| if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) });
    let u32_at = |at: usize| field(at, 4).map(|b| { let a = [b[0], b[1], b[2], b[3]]; if little { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) } });
    let u64_at = |at: usize| field(at, 8).map(|b| { let a: [u8; 8] = b.try_into().unwrap(); if little { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) } });
    let kind = match u16_at(16)? { 1 => "REL".to_string(), 2 => "EXEC".into(), 3 => "DYN".into(), 4 => "CORE".into(), other => format!("0x{other:04x}") };
    let machine = match u16_at(18)? { 0x3e => "x86-64".to_string(), 0xb7 => "AArch64".into(), 0x03 => "x86".into(), 0x28 => "ARM".into(), other => format!("0x{other:04x}") };
    let (entry, phoff, phentsize, phnum) = if wide {
        (u64_at(24)?, u64_at(32)?, u16_at(54)?, u16_at(56)?)
    } else {
        (u64::from(u32_at(24)?), u64::from(u32_at(28)?), u16_at(42)?, u16_at(44)?)
    };
    let (mut loadable, mut loadable_bytes) = (0u16, 0u64);
    // Headers beyond the bytes read are simply not counted; never index past the buffer.
    for index in 0..u64::from(phnum.min(4096)) {
        let Some(at) = phoff.checked_add(index * u64::from(phentsize)).and_then(|at| usize::try_from(at).ok()) else { break };
        let Some(kind) = u32_at(at) else { break };
        if kind == 1 {
            loadable += 1;
            loadable_bytes = loadable_bytes.saturating_add(if wide { u64_at(at + 40).unwrap_or(0) } else { u64::from(u32_at(at + 20).unwrap_or(0)) });
        }
    }
    Some(ElfInfo {
        class: if wide { "ELF64" } else { "ELF32" }.into(),
        endian: if little { "LE" } else { "BE" }.into(),
        kind, machine,
        entry: format!("0x{entry:x}"),
        segments: phnum, loadable, loadable_bytes,
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayloadIndex {
    #[serde(default)]
    entries: Vec<PayloadEntry>,
    #[serde(default)]
    builtins: HashMap<String, SendHistory>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendHistory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_sent_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_result: Option<String>,
}

#[derive(Debug)]
struct ReceiverConfig {
    version: String,
    capabilities: Vec<String>,
    platform: Option<String>,
}

#[derive(Clone, Copy)]
struct Ps5Timing {
    stop_timeout: Duration,
    verify_timeout: Duration,
    poll_interval: Duration,
}

const DEFAULT_PS5_TIMING: Ps5Timing = Ps5Timing {
    stop_timeout: Duration::from_secs(6),
    verify_timeout: Duration::from_secs(30),
    poll_interval: Duration::from_millis(500),
};

#[tauri::command]
pub(super) fn list_payloads(app: AppHandle) -> Result<Vec<PayloadEntry>, String> {
    let _index = index_lock().lock().unwrap_or_else(|p| p.into_inner());
    list_payloads_at(&store_root(&app)?)
}

#[tauri::command]
pub(super) fn add_payloads(
    app: AppHandle,
    paths: Vec<String>,
    target: String,
) -> Result<Vec<PayloadEntry>, String> {
    let _index = index_lock().lock().unwrap_or_else(|p| p.into_inner());
    add_payloads_at(&store_root(&app)?, &paths, &target)
}

#[tauri::command]
pub(super) fn update_payload(
    app: AppHandle,
    id: String,
    name: Option<String>,
    target: Option<String>,
    notes: Option<String>,
) -> Result<Vec<PayloadEntry>, String> {
    let _index = index_lock().lock().unwrap_or_else(|p| p.into_inner());
    update_payload_at(&store_root(&app)?, &id, name, target, notes)
}

#[tauri::command]
pub(super) fn remove_payload(app: AppHandle, id: String) -> Result<Vec<PayloadEntry>, String> {
    let _index = index_lock().lock().unwrap_or_else(|p| p.into_inner());
    remove_payload_at(&store_root(&app)?, &id)
}

#[tauri::command]
pub(super) async fn send_payload(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    target: String,
    host: String,
    port: u32,
) -> Result<PayloadSendResult, String> {
    let _send = send_lock().try_lock().map_err(|_| "Another payload is being sent. Wait for it to finish.".to_string())?;
    let settings = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let result = send_payload_at(
        &store_root(&app)?,
        &id,
        &target,
        &host,
        port,
        &settings,
        DEFAULT_PS5_TIMING,
    )
    .await;
    crate::payload_autostart::record_manual(&target, &id, &result);
    result
}

fn send_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn index_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

pub(super) async fn send_autostart(app: &AppHandle, settings: &Settings, id: &str, target: &str, process_name: &str, generation: u64, automatic: bool) -> Result<(bool, String), String> {
    let _send = send_lock().try_lock().map_err(|_| "Another payload is being sent. Wait for it to finish.".to_string())?;
    send_autostart_at(&store_root(app)?,settings,id,target,process_name,automatic,|| crate::payload_autostart::is_current(app,target,generation)).await
}

async fn send_autostart_at(root: &Path, settings: &Settings, id: &str, target: &str, process_name: &str, automatic: bool, current: impl Fn() -> bool) -> Result<(bool, String), String> {
    let (host, port, receiver_port) = if target == "ps4" { (&settings.ps4_host, settings.ps4_loader_port, settings.ps4_receiver_port) } else { (&settings.ps5_host, settings.ps5_loader_port, settings.ps5_port) };
    // The receiver has its own live handshake and can bootstrap a manual run.
    if is_builtin(id) {
        if !current() { return Err("The order or console address changed.".into()); }
        match crate::console_diagnostics::running_process_names(target,host,receiver_port).await {
            Ok(_) => return Ok((true,"Receiver already running. Skipped sending.".into())),
            Err(error) if automatic => return Err(format!("Receiver state is unknown; automatic launch stopped. {error}")),
            Err(_) => {},
        }
        if !current() { return Err("The order or console address changed.".into()); }
        return send_payload_at(root, id, target, host, port.into(), settings, DEFAULT_PS5_TIMING).await.and_then(|r| if r.verified { Ok((true,r.message)) } else { Err(format!("{} The order stopped without resending.", r.message)) });
    }
    if process_name.is_empty() {
        if automatic { return Err("Set this payload's exact process name in Edit order, or use Run now. Automatic launch was blocked because its running state is unknown.".into()); }
        if !current() { return Err("The order or console address changed.".into()); }
        return send_payload_at(root,id,target,host,port.into(),settings,DEFAULT_PS5_TIMING).await.map(|r| (r.verified,format!("Manual send: {} Running process was not verified.",r.message)));
    }
    let names = crate::console_diagnostics::running_process_names(target, host, receiver_port).await
        .map_err(|e| format!("Running state is unknown; nothing was sent. {e}"))?;
    if names.iter().any(|name| name.eq_ignore_ascii_case(process_name)) {
        return Ok((true, format!("Already running: {process_name}. Skipped sending.")));
    }
    if !current() { return Err("The order or console address changed.".into()); }
    send_payload_at(root, id, target, host, port.into(), settings, DEFAULT_PS5_TIMING).await?;
    let verified = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            sleep(Duration::from_millis(500)).await;
            let names = crate::console_diagnostics::running_process_names(target, host, receiver_port).await?;
            if names.iter().any(|name| name.eq_ignore_ascii_case(process_name)) { return Ok::<(), String>(()); }
        }
    }).await;
    match verified {
        Ok(Ok(())) => Ok((true, format!("Running process verified: {process_name}."))),
        _ => Err(format!("Sent, but {process_name} could not be verified. The order stopped without resending.")),
    }
}

fn store_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|path| path.join("payloads"))
        .map_err(|_| "Could not locate the app configuration folder.".into())
}

fn list_payloads_at(root: &Path) -> Result<Vec<PayloadEntry>, String> {
    let index = read_index(root)?;
    let mut entries = vec![builtin_entry(BUILTIN_PS5), builtin_entry(BUILTIN_PS4)];
    for entry in &mut entries {
        if let Some(history) = index.builtins.get(&entry.id) {
            entry.last_sent_at = history.last_sent_at;
            entry.last_result = history.last_result.clone();
        }
    }
    entries.extend(index.entries.into_iter().filter(|entry| !entry.builtin).map(|mut entry| {
        // Program headers sit near the start; 256 KiB covers them without reading large payloads.
        entry.elf = safe_store_file_name(&entry.file_name, &entry.id).ok().and_then(|name| {
            use std::io::Read;
            let mut head = Vec::new();
            fs::File::open(root.join(name)).ok()?.take(256 * 1024).read_to_end(&mut head).ok()?;
            parse_elf(&head)
        });
        entry
    }));
    Ok(entries)
}

fn add_payloads_at(
    root: &Path,
    paths: &[String],
    target: &str,
) -> Result<Vec<PayloadEntry>, String> {
    let target = validate_target(target)?;
    let mut index = read_index(root)?;
    fs::create_dir_all(root)
        .map_err(|_| "Could not create the payload library folder.".to_string())?;

    for chosen_path in paths {
        let source = PathBuf::from(chosen_path);
        let metadata = fs::metadata(&source)
            .map_err(|_| "Could not read one of the selected payload files.".to_string())?;
        if !metadata.is_file() {
            return Err("Only payload files can be added.".into());
        }
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let extension = extension.to_ascii_lowercase();
        if extension != "elf" && extension != "bin" {
            return Err("Payload files must use the .elf or .bin extension.".into());
        }
        if metadata.len() < MIN_PAYLOAD_SIZE as u64 || metadata.len() > MAX_PAYLOAD_SIZE as u64 {
            return Err(format!("Payload files must be between 1 byte and {} MiB.", MAX_PAYLOAD_SIZE / (1024 * 1024)));
        }
        let bytes = fs::read(&source)
            .map_err(|_| "Could not read one of the selected payload files.".to_string())?;
        if bytes.len() < MIN_PAYLOAD_SIZE || bytes.len() > MAX_PAYLOAD_SIZE {
            return Err(format!("Payload files must be between 1 byte and {} MiB.", MAX_PAYLOAD_SIZE / (1024 * 1024)));
        }
        if extension == "elf" && !bytes.starts_with(b"\x7fELF") {
            return Err("ELF payload files must start with the ELF signature.".into());
        }
        let sha256 = crate::sha256_hex(&bytes);
        if index
            .entries
            .iter()
            .any(|entry| entry.sha256.eq_ignore_ascii_case(&sha256))
        {
            continue;
        }
        let id = sha256.chars().take(16).collect::<String>();
        if index
            .entries
            .iter()
            .any(|entry| entry.id == id && entry.sha256 != sha256)
        {
            return Err("A payload identifier collision was detected.".into());
        }
        let file_name = format!("{id}.{extension}");
        let stored_path = root.join(&file_name);
        if stored_path.exists() {
            let existing = fs::read(&stored_path)
                .map_err(|_| "An existing payload copy could not be read.".to_string())?;
            if crate::sha256_hex(&existing) != sha256 {
                return Err("An existing payload file conflicts with this payload.".into());
            }
        } else {
            fs::write(&stored_path, &bytes)
                .map_err(|_| "Could not copy a payload into the library.".to_string())?;
        }
        let stem = source
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Payload")
            .trim();
        let name = truncate_chars(if stem.is_empty() { "Payload" } else { stem }, 80);
        index.entries.push(PayloadEntry {
            id,
            name,
            file_name,
            path: stored_path.to_string_lossy().into_owned(),
            size: bytes.len(),
            sha256,
            target: target.clone(),
            builtin: false,
            added_at: unix_ms(),
            last_sent_at: None,
            last_result: None,
            notes: None,
            elf: None,
        });
    }
    write_index(root, &index)?;
    list_payloads_at(root)
}

fn update_payload_at(
    root: &Path,
    id: &str,
    name: Option<String>,
    target: Option<String>,
    notes: Option<String>,
) -> Result<Vec<PayloadEntry>, String> {
    if is_builtin(id) {
        return Err("Built-in payloads can't be edited.".into());
    }
    let mut index = read_index(root)?;
    let entry = index
        .entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or_else(|| "Payload not found.".to_string())?;
    if let Some(name) = name {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err("Payload names must be between 1 and 80 characters.".into());
        }
        entry.name = name.into();
    }
    if let Some(target) = target {
        entry.target = validate_target(&target)?;
    }
    if let Some(notes) = notes {
        let notes = notes.trim();
        if notes.chars().count() > 500 {
            return Err("Payload notes must be 500 characters or shorter.".into());
        }
        entry.notes = (!notes.is_empty()).then(|| notes.to_string());
    }
    write_index(root, &index)?;
    list_payloads_at(root)
}

fn remove_payload_at(root: &Path, id: &str) -> Result<Vec<PayloadEntry>, String> {
    if is_builtin(id) {
        return Err("Built-in payloads can't be removed.".into());
    }
    let mut index = read_index(root)?;
    let position = index
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or_else(|| "Payload not found.".to_string())?;
    let entry = index.entries.remove(position);
    let file_name = safe_store_file_name(&entry.file_name, &entry.id)?;
    let file_path = root.join(file_name);
    if file_path.exists() {
        fs::remove_file(file_path).map_err(|_| "Could not remove the payload file.".to_string())?;
    }
    write_index(root, &index)?;
    list_payloads_at(root)
}

async fn send_payload_at(
    root: &Path,
    id: &str,
    target: &str,
    host: &str,
    port: u32,
    settings: &Settings,
    ps5_timing: Ps5Timing,
) -> Result<PayloadSendResult, String> {
    let index = { let _index = index_lock().lock().unwrap_or_else(|p| p.into_inner()); read_index(root)? };
    let builtin = is_builtin(id);
    let entry = if builtin {
        builtin_entry(id)
    } else {
        index
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| "Payload not found.".to_string())?
    };

    let started = Instant::now();
    let mut trace = Trace::new();
    trace.step("Prepare payload", format!("{} | {} | {} bytes | SHA-256 {} | {host}:{port}", entry.name, target, entry.size, entry.sha256), started, true);
    let outcome = async {
        if !(1..=65535).contains(&port) {
            return Err("The loader port must be between 1 and 65535.".into());
        }
        if host.trim().is_empty() {
            return Err("Enter a console address before sending a payload.".into());
        }
        let target = validate_console_target(target)?;
        if matches!(entry.target.as_str(), "ps4" | "ps5") && entry.target != target {
            return Err(format!(
                "This payload is marked for {}.",
                entry.target.to_ascii_uppercase()
            ));
        }

        if id == BUILTIN_PS4 {
            let step = Instant::now();
            let loaded = crate::ps4_receiver::load_ps4_receiver(
                host.trim().into(),
                port as u16,
                settings.ps4_receiver_port,
            )
            .await;
            trace.step("Load and verify PS4 receiver", format!("{} bytes to :{port}, receiver on :{}", grouped(PS4_RECEIVER_ELF.len()), settings.ps4_receiver_port), step, loaded.is_ok());
            Ok(PayloadSendResult::new(loaded?, PS4_RECEIVER_ELF.len(), port, true))
        } else if id == BUILTIN_PS5 {
            send_ps5_receiver(host.trim(), port, settings.ps5_port, ps5_timing, &mut trace).await
        } else {
            let file_name = safe_store_file_name(&entry.file_name, &entry.id)?;
            let step = Instant::now();
            let bytes = fs::read(root.join(file_name))
                .map_err(|_| "Could not read the stored payload.".to_string())?;
            trace.step("Read stored copy", format!("{} bytes", grouped(bytes.len())), step, true);
            if let Some(mut reply) = send_bytes(host.trim(), port as u16, &bytes, target == "ps5", &mut trace).await? {
                reply.wait(LOADER_REPLY_WAIT).await;
                if let Some(error) = reply.record(&mut trace) { return Err(error); }
            }
            Ok(PayloadSendResult::new(
                format!("Sent {} ({} bytes) to {}:{}. Console execution was not verified.", entry.name, bytes.len(), host.trim(), port),
                bytes.len(),
                port,
                false,
            ))
        }
    }
    .await;
    let outcome = outcome.map(|mut result| {
        if !result.verified { result.message = with_log_location(result.message); }
        result.total_ms = started.elapsed().as_millis() as u64;
        if let Some((bytes, took)) = trace.sent {
            result.send_ms = Some(took.as_millis() as u64);
            result.bytes_per_second = (took.as_secs_f64() > 0.).then(|| bytes as f64 / took.as_secs_f64());
        }
        result.steps = trace.steps;
        result.host = host.trim().to_string();
        result.sha256 = entry.sha256.clone();
        result
    }).map_err(with_log_location);
    crate::session_log::write("payload-result", match &outcome { Ok(result) => &result.message, Err(error) => error });

    // A send can take seconds. Preserve edits/imports made while the socket was active.
    let _index = index_lock().lock().unwrap_or_else(|p| p.into_inner());
    let mut index = read_index(root)?;
    let now = unix_ms();
    let last_result = outcome
        .as_ref()
        .map(|result| result.message.clone())
        .unwrap_or_else(Clone::clone);
    if builtin {
        index.builtins.insert(
            id.to_string(),
            SendHistory {
                last_sent_at: Some(now),
                last_result: Some(last_result),
            },
        );
    } else if let Some(entry) = index.entries.iter_mut().find(|entry| entry.id == id) {
        entry.last_sent_at = Some(now);
        entry.last_result = Some(last_result);
    }
    write_index(root, &index)?;
    outcome
}

fn with_log_location(message: String) -> String {
    match crate::session_log::location() {
        Some(path) => format!("{message} Diagnostic log: {path}"),
        None => message,
    }
}

async fn send_ps5_receiver(
    host: &str,
    loader_port: u32,
    receiver_port: u16,
    timing: Ps5Timing,
    trace: &mut Trace,
) -> Result<PayloadSendResult, String> {
    let step = Instant::now();
    let probed = probe_receiver(host, receiver_port).await;
    let found = match &probed {
        Ok(ReceiverProbe::Online(config)) => format!("v{} ({}) answered at {host}:{receiver_port}", config.version, config.platform.as_deref().unwrap_or("unknown")),
        Ok(ReceiverProbe::Unavailable(error)) => error.clone(),
        Err(error) => error.clone(),
    };
    trace.step("Probe receiver", found, step, matches!(&probed, Ok(ReceiverProbe::Online(_))));
    match probed? {
        ReceiverProbe::Online(config)
            if config.version == RECEIVER_VERSION && config.platform.as_deref() == Some("ps5") =>
        {
            return Ok(PayloadSendResult::new(format!("Receiver already running (v{RECEIVER_VERSION})"), 0, loader_port, true));
        }
        ReceiverProbe::Online(config) => {
            if config.platform.as_deref() == Some("ps4") {
                return Err(
                    "A PS4 receiver answered at the PS5 address. Check the selected console."
                        .into(),
                );
            }
            if !config
                .capabilities
                .iter()
                .any(|capability| capability == "stop")
            {
                return Err(format!(
                    "The PS5 receiver {} is running and can't be replaced remotely. Restart the PS5, then load the receiver again.",
                    config.version
                ));
            }
            let step = Instant::now();
            let stopped = send_stop(host, receiver_port).await;
            trace.step("Stop old receiver", format!("v{} sent STOP", config.version), step, stopped.is_ok());
            stopped?;
            let step = Instant::now();
            let closed = wait_for_port_closed(host, receiver_port, timing.stop_timeout).await;
            trace.step("Wait for port to close", format!(":{receiver_port}"), step, closed.is_ok());
            closed?;
        }
        ReceiverProbe::Unavailable(_) => {}
    }

    let reply = send_bytes(host, loader_port as u16, RECEIVER_ELF, true, trace).await?
        .expect("PS5 loader capture is enabled");
    let step = Instant::now();
    let deadline = Instant::now() + timing.verify_timeout;
    let mut last_probe = format!("No SSPI receiver answered at {host}:{receiver_port}.");
    loop {
        if let Some(error) = reply.failure(true) {
            reply.record_for_receiver(trace);
            return Err(error);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if let Some(error) = reply.record_for_receiver(trace) { return Err(error); }
            trace.step("Verify receiver", format!("Not verified within {} s. {last_probe}", timing.verify_timeout.as_secs()), step, false);
            return Ok(PayloadSendResult::new(format!("Payload sent to {host}:{loader_port}, but the receiver did not become ready within {} s. {last_probe} The loader accepting bytes does not prove startup; check its output in the send steps or diagnostic log.", timing.verify_timeout.as_secs()), RECEIVER_ELF.len(), loader_port, false));
        }
        let probed = tokio::select! {
            biased;
            error = reply.wait_for_receiver_failure() => {
                reply.record_for_receiver(trace);
                return Err(error);
            }
            probed = timeout(remaining, probe_receiver(host, receiver_port)) => probed,
        };
        match probed {
            Ok(Ok(ReceiverProbe::Online(config))) if config.version == RECEIVER_VERSION && config.platform.as_deref() == Some("ps5") => {
                if let Some(error) = reply.record_for_receiver(trace) { return Err(error); }
                trace.step("Verify receiver", format!("v{} answered on :{receiver_port}, {} capabilities", config.version, config.capabilities.len()), step, true);
                return Ok(PayloadSendResult::new(format!("Receiver loaded and verified (v{RECEIVER_VERSION})."), RECEIVER_ELF.len(), loader_port, true));
            }
            Ok(Ok(ReceiverProbe::Online(config))) => last_probe = format!("Receiver at {host}:{receiver_port} returned {} v{}; expected PS5 v{RECEIVER_VERSION}.", config.platform.as_deref().unwrap_or("unknown platform"), config.version),
            Ok(Err(error)) => last_probe = error,
            Ok(Ok(ReceiverProbe::Unavailable(error))) => last_probe = error,
            Err(_) => last_probe = format!("The receiver probe at {host}:{receiver_port} did not finish before the startup deadline."),
        }
        tokio::select! {
            error = reply.wait_for_receiver_failure() => {
                reply.record_for_receiver(trace);
                return Err(error);
            }
            _ = sleep(timing.poll_interval.min(deadline.saturating_duration_since(Instant::now()))) => {}
        }
    }
}

enum ReceiverProbe {
    Online(ReceiverConfig),
    Unavailable(String),
}

fn receiver_probe_error(host: &str, port: u16, error: String) -> String {
    let recovery = if error == "Receiver request timed out after 2000 ms" {
        " The receiver port accepted a connection but did not answer. Restart the PS5, enable its ELF loader, then load the bundled receiver again."
    } else { "" };
    format!("PS5 receiver at {host}:{port}: {error}{recovery}")
}

async fn probe_receiver(host: &str, port: u16) -> Result<ReceiverProbe, String> {
    let mut stream = match timeout(
        RECEIVER_CONNECT_TIMEOUT,
        TcpStream::connect((host, port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return Ok(ReceiverProbe::Unavailable(format!("Could not connect to the PS5 receiver at {host}:{port}: {}.", socket_error_detail(&error)))),
        Err(_) => return Ok(ReceiverProbe::Unavailable(format!("The PS5 receiver connection to {host}:{port} timed out after {} ms.", RECEIVER_CONNECT_TIMEOUT.as_millis()))),
    };
    let (ping_code, ping_body) = exchange(&mut stream, 0x01, &[]).await
        .map_err(|error| receiver_probe_error(host, port, error))?;
    if ping_code != 1 || ping_body != b"SSPI" {
        return Err(format!("The service at {host}:{port} isn't an SSPI receiver (unexpected handshake response code {ping_code}, {} bytes).", ping_body.len()));
    }
    let (code, body) = exchange(&mut stream, 0x53, &[]).await
        .map_err(|error| receiver_probe_error(host, port, error))?;
    if code != 3 {
        return Err(format!("The PS5 receiver at {host}:{port} didn't return its configuration (response code {code})."));
    }
    parse_receiver_config(&body)
        .map(ReceiverProbe::Online)
        .ok_or_else(|| format!("The PS5 receiver at {host}:{port} returned invalid configuration."))
}

async fn send_stop(host: &str, port: u16) -> Result<(), String> {
    let mut stream = timeout(
        Duration::from_millis(1500),
        TcpStream::connect((host, port)),
    )
    .await
    .map_err(|_| "The old PS5 receiver could not be stopped.".to_string())?
    .map_err(|_| "The old PS5 receiver could not be stopped.".to_string())?;
    let (code, _) = exchange(&mut stream, 0x5a, &[])
        .await
        .map_err(|_| "The old PS5 receiver could not be stopped.".to_string())?;
    if code != 1 {
        return Err("The old PS5 receiver could not be stopped.".into());
    }
    Ok(())
}

async fn wait_for_port_closed(host: &str, port: u16, max_wait: Duration) -> Result<(), String> {
    let deadline = Instant::now() + max_wait;
    loop {
        let open = matches!(
            timeout(Duration::from_millis(150), TcpStream::connect((host, port))).await,
            Ok(Ok(_))
        );
        if !open {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "The previous PS5 receiver did not stop. Restart the PS5 and try again.".into(),
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

#[derive(Default)]
struct LoaderOutput {
    bytes: Vec<u8>,
    truncated: bool,
    closed: bool,
    read_error: Option<String>,
    rejection: Option<String>,
    startup_failure: Option<String>,
    startup_line: Vec<u8>,
    startup_line_overflow: bool,
    tail: String,
}

impl LoaderOutput {
    fn append(&mut self, bytes: &[u8]) {
        let keep = bytes.len().min(MAX_LOADER_OUTPUT.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep < bytes.len();
        // Arbitrary payload stdout has no status protocol. SSPI markers are acted on only
        // by the built-in receiver path, never by manual/catalog sends.
        for &byte in bytes {
            if byte == b'\n' {
                if !self.startup_line_overflow && self.startup_failure.is_none() {
                    let line = String::from_utf8_lossy(&self.startup_line);
                    if is_receiver_startup_failure(line.trim_end_matches('\r')) {
                        self.startup_failure = Some(line.trim_end_matches('\r').into());
                    }
                }
                self.startup_line.clear();
                self.startup_line_overflow = false;
            } else if self.startup_line.len() < 256 {
                self.startup_line.push(byte);
            } else {
                self.startup_line_overflow = true;
            }
        }
        let text = format!("{}{}", self.tail, String::from_utf8_lossy(bytes));
        for message in [
            "[elfldr.elf] Unknown payload format",
            "[elfldr.elf] Error reading URI payload",
            "[elfldr.elf] Error reading HTTP payload",
            "[elfldr.elf] Error reading ELF payload",
            "[elfldr.elf] Error reading SELF payload",
            "[elfldr.elf] Error spawning payload",
        ] {
            if text.contains(message) { self.rejection = Some(message.into()); }
        }
        self.tail = text.chars().rev().take(128).collect::<String>().chars().rev().collect();
    }
}

fn is_receiver_startup_failure(line: &str) -> bool {
    let Some(marker) = line.strip_prefix("[SSPI startup] ") else { return false; };
    let Some((stage, code)) = marker.rsplit_once(" 0x") else { return false; };
    code.len() == 8 && code.bytes().all(|byte| byte.is_ascii_hexdigit()) && matches!(stage,
        "kernel init failed" | "kernel log init failed" | "libc thread symbol missing" |
        "runtime patch init failed" | "runtime linker init failed" |
        "payload library allocation failed" | "load libraries failed" |
        "library constructors failed" | "socket failed" | "bind failed" | "listen failed")
}

struct LoaderReply {
    output: Arc<Mutex<LoaderOutput>>,
    reader: JoinHandle<()>,
    started: Instant,
    changed: Arc<Notify>,
}

impl LoaderReply {
    fn start(mut stream: OwnedReadHalf) -> Self {
        let output = Arc::new(Mutex::new(LoaderOutput::default()));
        let shared = output.clone();
        let changed = Arc::new(Notify::new());
        let reader_changed = changed.clone();
        let reader = tokio::spawn(async move {
            let mut buffer = [0_u8; 4096];
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) => {
                        shared.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
                        return;
                    }
                    Ok(count) => {
                        shared.lock().unwrap_or_else(|p| p.into_inner()).append(&buffer[..count]);
                        reader_changed.notify_one();
                    }
                    Err(error) => {
                        shared.lock().unwrap_or_else(|p| p.into_inner()).read_error = Some(error.to_string());
                        return;
                    }
                }
            }
        });
        Self { output, reader, started: Instant::now(), changed }
    }

    async fn wait(&mut self, max_wait: Duration) {
        let _ = timeout(max_wait, &mut self.reader).await;
    }

    fn failure(&self, builtin_receiver: bool) -> Option<String> {
        let output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        if builtin_receiver {
            if let Some(marker) = &output.startup_failure {
                return Some(format!("SSPI receiver startup failed: {marker}. Payload bytes were sent, but the receiver did not start. It was not resent.\nLoader output:\n{}{}",
                    loader_output_text(&output), if output.truncated { "\n[output truncated at 32 KiB]" } else { "" }));
            }
        }
        output.rejection.as_ref().map(|message| format!("The payload loader rejected the payload: {message}. It was not resent."))
    }

    async fn wait_for_receiver_failure(&self) -> String {
        loop {
            if let Some(error) = self.failure(true) { return error; }
            self.changed.notified().await;
        }
    }

    fn record_for_receiver(&self, trace: &mut Trace) -> Option<String> {
        self.record_output(trace, true)
    }

    fn record(&self, trace: &mut Trace) -> Option<String> {
        self.record_output(trace, false)
    }

    fn record_output(&self, trace: &mut Trace, builtin_receiver: bool) -> Option<String> {
        let failure = self.failure(builtin_receiver);
        let output = self.output.lock().unwrap_or_else(|p| p.into_inner());
        let text = loader_output_text(&output);
        let status = if let Some(error) = &output.read_error {
            format!("Read ended: {error}; this alone does not establish whether the payload started")
        } else if output.closed {
            "Loader closed its output stream; this alone does not establish whether the payload started".into()
        } else {
            "Capture window ended; this alone does not establish whether the payload started".into()
        };
        let detail = format!("{status}. {}{}", if text.trim().is_empty() { "No loader output." } else { text.trim() }, if output.truncated { " [output truncated at 32 KiB]" } else { "" });
        trace.step("Loader output", detail, self.started, failure.is_none());
        failure
    }
}

fn loader_output_text(output: &LoaderOutput) -> String {
    String::from_utf8_lossy(&output.bytes).chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

impl Drop for LoaderReply {
    fn drop(&mut self) { self.reader.abort(); }
}

fn socket_error_detail(error: &io::Error) -> String {
    let code = error.raw_os_error().map(|code| code.to_string()).unwrap_or_else(|| "unavailable".into());
    format!("{error} [kind={:?}, os_code={code}]", error.kind())
}

fn loader_connect_timeout(host: &str, port: u16, duration: Duration) -> String {
    format!("The payload loader connection timed out. {host}:{port} did not connect within {} ms. No payload bytes were sent.", duration.as_millis())
}

async fn send_bytes(host: &str, port: u16, bytes: &[u8], capture_output: bool, trace: &mut Trace) -> Result<Option<LoaderReply>, String> {
    let step = Instant::now();
    let connected = timeout(LOADER_CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| loader_connect_timeout(host, port, LOADER_CONNECT_TIMEOUT))
        .and_then(|result| result.map_err(|error| format!("Could not connect to the payload loader. {host}:{port}: {}. No payload bytes were sent.", socket_error_detail(&error))));
    let detail = match &connected {
        Ok(stream) => {
            let mut detail = format!("{host}:{port}");
            if let Ok(local) = stream.local_addr() { detail.push_str(&format!(" | local {local}")); }
            if let Ok(peer) = stream.peer_addr() { detail.push_str(&format!(" | peer {peer}")); }
            detail
        }
        Err(error) => error.clone(),
    };
    trace.step("Connect to loader", detail, step, connected.is_ok());
    let (reader, mut stream) = connected?.into_split();
    // Drain while uploading too: a rejecting loader can reply before it consumes the whole ELF.
    let mut reply = capture_output.then(|| LoaderReply::start(reader));
    let step = Instant::now();
    let written = timeout(Duration::from_secs(90), async {
        stream.write_all(bytes).await?;
        stream.shutdown().await?;
        Ok::<(), io::Error>(())
    })
    .await
    .map_err(|_| "Sending the payload timed out.".to_string())
    .and_then(|result| result.map_err(|error| format!("Could not send the complete payload: {error}.")));
    let took = step.elapsed();
    trace.step("Send payload", match &written { Ok(_) => format!("{} bytes written, upload finished (write EOF)", grouped(bytes.len())), Err(error) => error.clone() }, step, written.is_ok());
    if let Err(error) = written {
        drop(stream);
        if let Some(reply) = &mut reply {
            reply.wait(Duration::from_millis(250)).await;
            if let Some(rejection) = reply.record(trace) { return Err(format!("{error} {rejection}")); }
        }
        return Err(format!("{error} The payload may have been partially delivered; it was not resent."));
    }
    trace.sent = Some((bytes.len(), took));
    drop(stream);
    Ok(reply)
}

async fn exchange(
    stream: &mut TcpStream,
    command: u8,
    body: &[u8],
) -> Result<(u8, Vec<u8>), String> {
    let length =
        u32::try_from(body.len()).map_err(|_| "The receiver request is too large".to_string())?;
    let mut request = Vec::with_capacity(5 + body.len());
    request.push(command);
    request.extend_from_slice(&length.to_le_bytes());
    request.extend_from_slice(body);
    timeout(Duration::from_secs(2), async {
        stream
            .write_all(&request)
            .await
            .map_err(|error| format!("Could not send receiver request: {}", socket_error_detail(&error)))?;
        let mut header = [0_u8; 5];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|error| format!("Incomplete receiver response header: {}", socket_error_detail(&error)))?;
        let response_len =
            u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if response_len > MAX_RESPONSE {
            return Err(format!("The service returned an invalid SSPI response length ({response_len} bytes; header {:02x?}). The receiver port may belong to another service or an incompatible receiver. Check the receiver port in Options, Consoles; it is separate from the ELF loader port.", header));
        }
        let mut response = vec![0; response_len];
        stream
            .read_exact(&mut response)
            .await
            .map_err(|error| format!("Incomplete receiver response body: {}", socket_error_detail(&error)))?;
        Ok((header[0], response))
    })
    .await
    .map_err(|_| "Receiver request timed out after 2000 ms".to_string())?
}

fn parse_receiver_config(body: &[u8]) -> Option<ReceiverConfig> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let version = value.get("version")?.as_str()?.to_string();
    let capabilities = value
        .get("capabilities")?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let platform = value
        .get("platform")
        .and_then(serde_json::Value::as_str)
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            if capabilities.iter().any(|item| item == "ps4") {
                Some("ps4".into())
            } else if capabilities
                .iter()
                .any(|item| item == "dump-mount" || item == "fih-install")
            {
                Some("ps5".into())
            } else {
                None
            }
        });
    Some(ReceiverConfig {
        version,
        capabilities,
        platform,
    })
}

fn builtin_entry(id: &str) -> PayloadEntry {
    let (name, file_name, target, bytes) = if id == BUILTIN_PS4 {
        (
            "SSPI receiver for PS4",
            "sspi_ps4_receiver.elf",
            "ps4",
            PS4_RECEIVER_ELF,
        )
    } else {
        (
            "SSPI receiver for PS5",
            "sspi_receiver.elf",
            "ps5",
            RECEIVER_ELF,
        )
    };
    PayloadEntry {
        id: id.into(),
        name: name.into(),
        file_name: file_name.into(),
        path: String::new(),
        size: bytes.len(),
        sha256: crate::sha256_hex(bytes),
        target: target.into(),
        builtin: true,
        added_at: 0,
        last_sent_at: None,
        last_result: None,
        notes: None,
        elf: parse_elf(bytes),
    }
}

fn validate_target(target: &str) -> Result<String, String> {
    let target = target.trim().to_ascii_lowercase();
    if matches!(target.as_str(), "ps4" | "ps5" | "any") {
        Ok(target)
    } else {
        Err("Payload target must be PS4, PS5 or any.".into())
    }
}

fn validate_console_target(target: &str) -> Result<String, String> {
    let target = target.trim().to_ascii_lowercase();
    if matches!(target.as_str(), "ps4" | "ps5") {
        Ok(target)
    } else {
        Err("Choose PS4 or PS5 before sending a payload.".into())
    }
}

fn is_builtin(id: &str) -> bool {
    id == BUILTIN_PS4 || id == BUILTIN_PS5
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn safe_store_file_name<'a>(file_name: &'a str, id: &str) -> Result<&'a str, String> {
    let name = Path::new(file_name)
        .file_name()
        .and_then(|part| part.to_str());
    if name != Some(file_name)
        || !file_name.starts_with(id)
        || file_name.contains('/')
        || file_name.contains('\\')
    {
        Err("The payload library index contains an invalid file name.".into())
    } else {
        Ok(file_name)
    }
}

fn read_index(root: &Path) -> Result<PayloadIndex, String> {
    let path = root.join(INDEX_NAME);
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "The payload library index is unreadable.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(PayloadIndex::default()),
        Err(_) => Err("Could not read the payload library index.".into()),
    }
}

fn write_index(root: &Path, index: &PayloadIndex) -> Result<(), String> {
    fs::create_dir_all(root)
        .map_err(|_| "Could not create the payload library folder.".to_string())?;
    let bytes = serde_json::to_vec_pretty(index)
        .map_err(|_| "Could not encode the payload library index.".to_string())?;
    atomic_write(&root.join(INDEX_NAME), &bytes)
        .map_err(|_| "Could not save the payload library index.".into())
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp_name = format!(
        ".{}.{}.tmp",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("index"),
        uuid::Uuid::new_v4()
    );
    let temp = parent.join(temp_name);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temp, destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_are_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_086_888), "1,086,888");
        assert_eq!(grouped(64 * 1024 * 1024), "67,108,864");
    }

    #[test]
    fn elf_headers_are_read_without_trusting_offsets() {
        let receiver = parse_elf(RECEIVER_ELF).expect("the built-in PS5 receiver is an ELF");
        assert_eq!((receiver.class.as_str(), receiver.endian.as_str(), receiver.machine.as_str()), ("ELF64", "LE", "x86-64"));
        assert!(receiver.loadable > 0 && receiver.loadable_bytes > 0 && receiver.entry.starts_with("0x"));
        assert_eq!(parse_elf(b"not an elf payload, a raw BIN"), None);
        assert_eq!(parse_elf(&RECEIVER_ELF[..40]), None);
        // Program headers pointing past the buffer (or overflowing) are simply not counted.
        let mut hostile = RECEIVER_ELF[..64].to_vec();
        hostile[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        hostile[56..58].copy_from_slice(&u16::MAX.to_le_bytes());
        let parsed = parse_elf(&hostile).unwrap();
        assert_eq!((parsed.loadable, parsed.loadable_bytes), (0, 0));
    }
    use serde_json::json;
    use std::net::Ipv4Addr;
    use tokio::{net::TcpListener, task::JoinHandle};

    fn temp_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/tests")
            .join(format!("sspi-payloads-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn payload_import_accepts_above_64_mib_and_rejects_above_shared_limit() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let source = root.join("large.elf");
        let mut file = fs::File::create(&source).unwrap();
        file.write_all(&RECEIVER_ELF[..256]).unwrap();
        let size = 64 * 1024 * 1024 + 1;
        file.set_len(size).unwrap();
        drop(file);
        let entries = add_payloads_at(&root, &[source.to_string_lossy().into_owned()], "ps5").unwrap();
        let entry = entries.iter().find(|entry| !entry.builtin).unwrap();
        assert_eq!(entry.size, size as usize);
        assert_eq!(fs::metadata(&entry.path).unwrap().len(), size);
        fs::File::create(&source).unwrap().set_len(MAX_PAYLOAD_SIZE as u64 + 1).unwrap();
        assert!(add_payloads_at(&root, &[source.to_string_lossy().into_owned()], "ps5").unwrap_err().contains("256 MiB"));
        assert_eq!(list_payloads_at(&root).unwrap().len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires SSPI_TEST_LARGE_ELF; sends only to a local test socket"]
    async fn large_local_elf_import_and_send_preserves_every_byte() {
        let source = std::env::var("SSPI_TEST_LARGE_ELF").expect("set SSPI_TEST_LARGE_ELF");
        let bytes = fs::read(&source).unwrap();
        assert!(bytes.len() > 64 * 1024 * 1024 && bytes.len() <= MAX_PAYLOAD_SIZE);
        assert_eq!(parse_elf(&bytes).unwrap().machine, "x86-64");
        let expected_hash = crate::sha256_hex(&bytes);
        let expected_size = bytes.len();
        drop(bytes);
        let root = temp_root();
        let entries = add_payloads_at(&root, &[source.clone()], "ps5").unwrap();
        let entry = entries.iter().find(|entry| !entry.builtin).unwrap();
        assert_eq!(entry.sha256, expected_hash);
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = loader.local_addr().unwrap().port();
        let received = tokio::spawn(async move {
            let (mut socket, _) = loader.accept().await.unwrap();
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).await.unwrap();
            (bytes.len(), crate::sha256_hex(&bytes))
        });
        let sent = send_payload_at(&root, &entry.id, "ps5", "127.0.0.1", port.into(), &Settings::default(), test_timing()).await.unwrap();
        assert_eq!(sent.bytes, expected_size);
        assert!(!sent.verified, "transport success does not prove console execution");
        assert_eq!(received.await.unwrap(), (expected_size, expected_hash.clone()));
        assert_eq!(crate::sha256_hex(&fs::read(source).unwrap()), expected_hash);
        fs::remove_dir_all(root).unwrap();
    }

    fn test_timing() -> Ps5Timing {
        Ps5Timing {
            stop_timeout: Duration::from_millis(700),
            verify_timeout: Duration::from_secs(2),
            poll_interval: Duration::from_millis(30),
        }
    }

    fn receiver_config(version: &str, capabilities: &[&str]) -> Vec<u8> {
        serde_json::to_vec(&json!({"version":version,"platform":"ps5","capabilities":capabilities}))
            .unwrap()
    }

    async fn serve_receiver(listener: TcpListener, config: Vec<u8>) {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            tokio::spawn(async move {
                loop {
                    let mut header = [0_u8; 5];
                    if socket.read_exact(&mut header).await.is_err() {
                        return;
                    }
                    let command = header[0];
                    let (code, body) = match command {
                        0x01 => (1, b"SSPI".to_vec()),
                        0x53 => (3, config.clone()),
                        0x5a => (1, b"stopping".to_vec()),
                        _ => (2, b"unknown".to_vec()),
                    };
                    let mut response = vec![code];
                    response.extend_from_slice(&(body.len() as u32).to_le_bytes());
                    response.extend_from_slice(&body);
                    if socket.write_all(&response).await.is_err() {
                        return;
                    }
                    if command == 0x5a {
                        return;
                    }
                    if command == 0x53 {
                        return;
                    }
                }
            });
        }
    }

    async fn start_receiver(config: Vec<u8>) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(serve_receiver(listener, config));
        (port, task)
    }

    async fn process_receiver(target: &str, running: std::sync::Arc<std::sync::atomic::AtomicBool>, truncated: bool) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST,0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = target.to_string();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket,_) = listener.accept().await.unwrap();
                let mut header = [0u8;5];
                socket.read_exact(&mut header).await.unwrap();
                assert_eq!(header[0],0x53);
                reply(&mut socket,3,&serde_json::to_vec(&json!({"platform":target,"version":"1.0.14","capabilities":["diagnostics-v1"]})).unwrap()).await;
                socket.read_exact(&mut header).await.unwrap();
                assert_eq!(header[0],0x6a);
                let name = if running.load(std::sync::atomic::Ordering::SeqCst) { "service.elf" } else { "System" };
                reply(&mut socket,3,&serde_json::to_vec(&json!({"truncated":truncated,"processes":[{
                    "pid":200,"ppid":1,"name":name,"state":"sleeping","uid":0,"titleId":null,"appType":null,"authId":null,
                    "rssBytes":1,"vmBytes":1,"threads":1,"startedAt":1,"cpuMs":1,"control":"payload"
                }]})).unwrap()).await;
            }
        });
        (port,task)
    }

    #[tokio::test]
    async fn autostart_skips_live_processes_and_refuses_unknown_incomplete_or_cancelled_runs() {
        for target in ["ps4","ps5"] {
            for (running,truncated,name,current) in [(true,false,"service.elf",true),(false,true,"service.elf",true),(false,false,"",true),(false,false,"service.elf",false)] {
                let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(running));
                let (receiver_port,receiver) = process_receiver(target,flag,truncated).await;
                let loader = TcpListener::bind((Ipv4Addr::LOCALHOST,0)).await.unwrap();
                let loader_port = loader.local_addr().unwrap().port();
                let settings = Settings { ps5_host:"127.0.0.1".into(),ps4_host:"127.0.0.1".into(),ps5_port:receiver_port,ps4_receiver_port:receiver_port,ps5_loader_port:loader_port,ps4_loader_port:loader_port,..Settings::default() };
                let result = send_autostart_at(&temp_root(),&settings,"test",target,name,true,|| current).await;
                if running { assert!(result.unwrap().1.contains("Already running")); } else { assert!(result.is_err()); }
                if truncated {
                    let id = if target == "ps4" { BUILTIN_PS4 } else { BUILTIN_PS5 };
                    assert!(send_autostart_at(&temp_root(),&settings,id,target,"",true,|| true).await.unwrap_err().contains("Receiver state is unknown"));
                }
                assert!(timeout(Duration::from_millis(30),loader.accept()).await.is_err(),"Must not touch the loader");
                receiver.abort();
            }
        }
    }

    #[tokio::test]
    async fn autostart_sends_only_absent_payload_and_verifies_its_process() {
        for target in ["ps4","ps5"] {
          for (automatic,process_name) in [(true,"service.elf"),(false,"")] {
            let root = temp_root(); fs::create_dir_all(&root).unwrap();
            let input = root.join("service.bin"); fs::write(&input,b"test service payload").unwrap();
            let entries = add_payloads_at(&root,&[input.to_string_lossy().into_owned()],target).unwrap();
            let id = entries.iter().find(|p| !p.builtin).unwrap().id.clone();
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (receiver_port,receiver) = process_receiver(target,flag.clone(),false).await;
            let loader = TcpListener::bind((Ipv4Addr::LOCALHOST,0)).await.unwrap();
            let loader_port = loader.local_addr().unwrap().port();
            let sent = tokio::spawn(async move {
                let (mut socket,_) = loader.accept().await.unwrap();
                let mut bytes = Vec::new(); socket.read_to_end(&mut bytes).await.unwrap();
                assert_eq!(bytes,b"test service payload");
                flag.store(true,std::sync::atomic::Ordering::SeqCst);
                assert!(timeout(Duration::from_millis(800),loader.accept()).await.is_err());
            });
            let settings = Settings { ps5_host:"127.0.0.1".into(),ps4_host:"127.0.0.1".into(),ps5_port:receiver_port,ps4_receiver_port:receiver_port,ps5_loader_port:loader_port,ps4_loader_port:loader_port,..Settings::default() };
            let result = send_autostart_at(&root,&settings,&id,target,process_name,automatic,|| true).await.unwrap();
            if automatic {
                assert!(result.0 && result.1.contains("verified"));
                let second = send_autostart_at(&root,&settings,&id,target,process_name,true,|| true).await.unwrap();
                assert!(second.1.contains("Already running"));
            } else { assert!(!result.0 && result.1.contains("Manual send")); }
            sent.await.unwrap(); receiver.abort();
          }
        }
    }

    #[tokio::test]
    async fn add_dedupe_update_remove_and_index_round_trip() {
        let root = temp_root();
        let source = root.join("input.bin");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"test payload bytes").unwrap();
        let list = add_payloads_at(&root, &[source.to_string_lossy().into_owned()], "any").unwrap();
        assert_eq!(list[0].id, BUILTIN_PS5);
        let added = list.iter().find(|entry| !entry.builtin).unwrap();
        let id = added.id.clone();
        assert_eq!(added.sha256, crate::sha256_hex(b"test payload bytes"));
        assert_eq!(fs::read(&added.path).unwrap(), b"test payload bytes");
        assert_eq!(
            list_payloads_at(&root)
                .unwrap()
                .iter()
                .filter(|entry| !entry.builtin)
                .count(),
            1
        );

        let deduped =
            add_payloads_at(&root, &[source.to_string_lossy().into_owned()], "ps4").unwrap();
        assert_eq!(deduped.iter().filter(|entry| !entry.builtin).count(), 1);
        let updated = update_payload_at(
            &root,
            &id,
            Some(" Renamed ".into()),
            Some("ps5".into()),
            Some(" Notes ".into()),
        )
        .unwrap();
        let updated = updated.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.target, "ps5");
        assert_eq!(updated.notes.as_deref(), Some("Notes"));
        assert!(remove_payload_at(&root, BUILTIN_PS5).is_err());
        let removed = remove_payload_at(&root, &id).unwrap();
        assert!(removed.iter().all(|entry| entry.id != id));
        assert!(!root.join(format!("{id}.bin")).exists());
        assert!(root.join(INDEX_NAME).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn payload_validation_and_builtins_match_the_embedded_images() {
        let root = temp_root();
        let builtins = list_payloads_at(&root).unwrap();
        assert_eq!(builtins[0].id, BUILTIN_PS5);
        assert_eq!(builtins[0].name, "SSPI receiver for PS5");
        assert_eq!(builtins[0].size, RECEIVER_ELF.len());
        assert_eq!(builtins[0].sha256, crate::sha256_hex(RECEIVER_ELF));
        assert_eq!(builtins[0].path, "");
        assert_eq!(builtins[1].id, BUILTIN_PS4);
        let elf = root.join("input.elf");
        fs::create_dir_all(&root).unwrap();
        fs::write(&elf, b"not an elf").unwrap();
        assert!(
            add_payloads_at(&root, &[elf.to_string_lossy().into_owned()], "ps5")
                .unwrap_err()
                .contains("ELF signature")
        );
        assert!(add_payloads_at(&root, &[], "invalid").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn user_payload_is_sent_byte_for_byte_and_result_is_persisted() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let source = root.join("input.bin");
        let expected = b"payload to loader".to_vec();
        fs::write(&source, &expected).unwrap();
        let entries =
            add_payloads_at(&root, &[source.to_string_lossy().into_owned()], "any").unwrap();
        let id = entries
            .iter()
            .find(|entry| !entry.builtin)
            .unwrap()
            .id
            .clone();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            socket.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, expected);
        });
        let result = send_payload_at(
            &root,
            &id,
            "ps4",
            "127.0.0.1",
            port as u32,
            &Settings::default(),
            test_timing(),
        )
        .await
        .unwrap();
        assert!(!result.verified);
        assert!(result.message.starts_with("Sent "));
        receiver.await.unwrap();
        let stored = list_payloads_at(&root)
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == id)
            .unwrap();
        assert!(stored.last_sent_at.is_some());
        assert_eq!(stored.last_result.as_deref(), Some(result.message.as_str()));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ps5_builtin_returns_existing_receiver_without_sending_loader_bytes() {
        let root = temp_root();
        let (receiver_port, receiver_task) =
            start_receiver(receiver_config(RECEIVER_VERSION, &["stop"])).await;
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let loader_port = loader.local_addr().unwrap().port();
        let result = send_payload_at(
            &root,
            BUILTIN_PS5,
            "ps5",
            "127.0.0.1",
            loader_port as u32,
            &Settings {
                ps5_port: receiver_port,
                ..Settings::default()
            },
            test_timing(),
        )
        .await
        .unwrap();
        assert!(result.verified, "{}", result.message);
        assert_eq!(
            result.message,
            format!("Receiver already running (v{RECEIVER_VERSION})")
        );
        assert!(timeout(Duration::from_millis(80), loader.accept())
            .await
            .is_err());
        let history = list_payloads_at(&root).unwrap();
        assert!(history[0].last_sent_at.is_some());
        assert_eq!(
            history[0].last_result.as_deref(),
            Some(result.message.as_str())
        );
        receiver_task.abort();
    }

    #[tokio::test]
    async fn ps5_builtin_stops_replaceable_receiver_and_verifies_the_new_one() {
        let root = temp_root();
        let old_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let receiver_port = old_listener.local_addr().unwrap().port();
        let old_config = receiver_config("1.0.1", &["stop"]);
        let old_task = tokio::spawn(async move {
            let first = old_listener.accept().await.unwrap();
            serve_one_config(first.0, old_config).await;
            let (mut stop, _) = old_listener.accept().await.unwrap();
            let mut header = [0_u8; 5];
            stop.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 0x5a);
            reply(&mut stop, 1, b"stopping").await;
            drop(old_listener);
        });
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let loader_port = loader.local_addr().unwrap().port();
        let loader_task = tokio::spawn(async move {
            let (mut stream, _) = loader.accept().await.unwrap();
            let mut sent = Vec::new();
            stream.read_to_end(&mut sent).await.unwrap();
            assert_eq!(sent, RECEIVER_ELF);
            drop(stream);
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, receiver_port))
                .await
                .unwrap();
            tokio::spawn(serve_receiver(
                listener,
                receiver_config(RECEIVER_VERSION, &["stop"]),
            ));
        });
        let result = send_payload_at(
            &root,
            BUILTIN_PS5,
            "ps5",
            "127.0.0.1",
            loader_port as u32,
            &Settings {
                ps5_port: receiver_port,
                ..Settings::default()
            },
            test_timing(),
        )
        .await
        .unwrap();
        assert!(result.verified, "{}", result.message);
        assert_eq!(result.bytes, RECEIVER_ELF.len());
        old_task.await.unwrap();
        loader_task.await.unwrap();
    }

    async fn serve_one_config(mut socket: TcpStream, config: Vec<u8>) {
        for (command, code, body) in [(0x01, 1, b"SSPI".to_vec()), (0x53, 3, config)] {
            let mut header = [0_u8; 5];
            socket.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], command);
            reply(&mut socket, code, &body).await;
        }
    }

    async fn reply(socket: &mut TcpStream, code: u8, body: &[u8]) {
        let mut response = vec![code];
        response.extend_from_slice(&(body.len() as u32).to_le_bytes());
        response.extend_from_slice(body);
        socket.write_all(&response).await.unwrap();
    }

    #[tokio::test]
    async fn ps5_builtin_reports_sent_but_unverified_when_receiver_does_not_appear() {
        let root = temp_root();
        let receiver_probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let receiver_port = receiver_probe.local_addr().unwrap().port();
        drop(receiver_probe);
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let loader_port = loader.local_addr().unwrap().port();
        let loader_task = tokio::spawn(async move {
            let (mut stream, _) = loader.accept().await.unwrap();
            let mut sent = Vec::new();
            stream.read_to_end(&mut sent).await.unwrap();
            assert_eq!(sent, RECEIVER_ELF);
        });
        let result = send_payload_at(
            &root,
            BUILTIN_PS5,
            "ps5",
            "127.0.0.1",
            loader_port as u32,
            &Settings {
                ps5_port: receiver_port,
                ..Settings::default()
            },
            test_timing(),
        )
        .await
        .unwrap();
        assert!(!result.verified);
        assert!(result.message.contains("receiver did not become ready"));
        assert!(result.steps.iter().any(|step| step.label == "Loader output" && step.detail.contains("No loader output")));
        loader_task.await.unwrap();
    }

    #[tokio::test]
    async fn refused_loader_connection_keeps_native_error_and_never_reaches_upload() {
        // Reserve a loopback port without listening, so another test cannot claim it.
        let reservation = tokio::net::TcpSocket::new_v4().unwrap();
        reservation.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let mut trace = Trace::new();
        let error = match send_bytes("127.0.0.1", port, b"must not be sent", true, &mut trace).await {
            Err(error) => error,
            Ok(_) => panic!("a non-listening port accepted a payload"),
        };
        assert!(error.starts_with("Could not connect to the payload loader. "), "{error}");
        assert!(error.contains(&format!("127.0.0.1:{port}")), "{error}");
        assert!(error.contains("kind=ConnectionRefused"), "{error}");
        assert!(error.contains("os_code=") && !error.contains("os_code=unavailable"), "{error}");
        #[cfg(windows)]
        {
            assert!(error.contains("os_code=10061"), "{error}");
            assert!(error.contains(&io::Error::from_raw_os_error(10061).to_string()), "{error}");
        }
        assert!(error.contains("No payload bytes were sent."));
        assert!(trace.sent.is_none());
        assert_eq!(trace.steps.len(), 1);
        assert_eq!(trace.steps[0].label, "Connect to loader");
        assert_eq!(trace.steps[0].detail, error);
        assert!(!trace.steps[0].ok);
    }

    #[test]
    fn loader_timeout_identifies_endpoint_deadline_and_unsent_payload() {
        let message = loader_connect_timeout("192.0.2.7", 9021, LOADER_CONNECT_TIMEOUT);
        assert!(message.starts_with("The payload loader connection timed out. "));
        assert!(message.contains("192.0.2.7:9021"));
        assert!(message.contains("3000 ms"));
        assert!(message.contains("No payload bytes were sent."));
    }

    #[tokio::test]
    async fn ps5_foreign_service_header_identifies_receiver_port_without_sending_payload() {
        let receiver = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let receiver_port = receiver.local_addr().unwrap().port();
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let loader_port = loader.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = receiver.accept().await.unwrap();
            let mut request = [0_u8; 5];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [0x01, 0, 0, 0, 0]);
            stream.write_all(b"HTTP/").await.unwrap();
        });
        let mut trace = Trace::new();
        let error = send_ps5_receiver("127.0.0.1", loader_port.into(), receiver_port, test_timing(), &mut trace).await.unwrap_err();
        assert!(error.contains(&format!("127.0.0.1:{receiver_port}")), "{error}");
        assert!(error.contains("invalid SSPI response length"), "{error}");
        assert!(error.contains("header [48, 54, 54, 50, 2f]"), "{error}");
        assert!(error.contains("separate from the ELF loader port"), "{error}");
        assert!(trace.sent.is_none());
        assert_eq!(trace.steps.len(), 1);
        assert!(timeout(Duration::from_millis(50), loader.accept()).await.is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn ps5_silent_receiver_requires_restart_without_sending_payload() {
        for silent_command in [0x01, 0x53] {
            let receiver = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let receiver_port = receiver.local_addr().unwrap().port();
            let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let loader_port = loader.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut stream, _) = receiver.accept().await.unwrap();
                let mut request = [0_u8; 5];
                stream.read_exact(&mut request).await.unwrap();
                assert_eq!(request, [0x01, 0, 0, 0, 0]);
                if silent_command == 0x53 {
                    stream.write_all(&[1, 4, 0, 0, 0, b'S', b'S', b'P', b'I']).await.unwrap();
                    stream.read_exact(&mut request).await.unwrap();
                    assert_eq!(request, [0x53, 0, 0, 0, 0]);
                }
                let mut closed = [0_u8; 1];
                assert_eq!(stream.read(&mut closed).await.unwrap(), 0);
            });
            let mut trace = Trace::new();
            let error = send_ps5_receiver("127.0.0.1", loader_port.into(), receiver_port, test_timing(), &mut trace).await.unwrap_err();
            assert!(error.contains(&format!("127.0.0.1:{receiver_port}")), "{error}");
            assert!(error.contains("Receiver request timed out after 2000 ms"), "{error}");
            assert!(error.contains("Restart the PS5, enable its ELF loader, then load the bundled receiver again"), "{error}");
            assert!(trace.sent.is_none());
            assert_eq!(trace.steps.len(), 1);
            assert!(timeout(Duration::from_millis(50), loader.accept()).await.is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn receiver_probe_preserves_connection_and_protocol_socket_errors() {
        let reservation = tokio::net::TcpSocket::new_v4().unwrap();
        reservation.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let port = reservation.local_addr().unwrap().port();
        match probe_receiver("127.0.0.1", port).await {
            Ok(ReceiverProbe::Unavailable(detail)) => {
                assert!(detail.contains(&format!("127.0.0.1:{port}")), "{detail}");
                // Windows can report refusal after the probe's 1500 ms deadline.
                if detail.contains("kind=ConnectionRefused") {
                    assert!(detail.contains("os_code=") && !detail.contains("os_code=unavailable"), "{detail}");
                } else {
                    assert!(detail.contains("timed out after 1500 ms"), "{detail}");
                }
            }
            _ => panic!("a refused probe should report unavailability without preventing a loader send"),
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 5];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [0x01, 0, 0, 0, 0]);
        });
        match probe_receiver("127.0.0.1", port).await {
            Err(error) => {
                assert!(error.contains(&format!("127.0.0.1:{port}")), "{error}");
                assert!(error.contains("Incomplete receiver response header"), "{error}");
                assert!(error.contains("kind=UnexpectedEof"), "{error}");
                assert!(!error.contains("No payload bytes were sent"));
            }
            _ => panic!("a connection closing before its response should preserve the protocol error"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn loader_output_follows_upload_eof_without_changing_payload_bytes() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = vec![0x5a; 4 * 1024 * 1024];
        let server_bytes = expected.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, server_bytes);
            stream.write_all(b"SSPI payload started\nSSPI ready\n").await.unwrap();
        });
        let mut trace = Trace::new();
        let mut reply = send_bytes("127.0.0.1", port, &expected, true, &mut trace).await.unwrap().unwrap();
        reply.wait(Duration::from_secs(1)).await;
        assert!(reply.record(&mut trace).is_none());
        assert!(trace.steps.last().unwrap().detail.contains("SSPI payload started"));
        assert!(trace.steps[0].detail.contains("local 127.0.0.1:"));
        assert!(trace.steps[0].detail.contains(&format!("peer 127.0.0.1:{port}")));
        assert_eq!(trace.sent.unwrap().0, expected.len());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn loader_explicit_rejection_is_a_failure_and_remains_in_history() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let input = root.join("rejected.bin");
        fs::write(&input, b"payload").unwrap();
        let entries = add_payloads_at(&root, &[input.to_string_lossy().into_owned()], "ps5").unwrap();
        let id = &entries.iter().find(|entry| !entry.builtin).unwrap().id;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.read_to_end(&mut Vec::new()).await.unwrap();
            stream.write_all(b"[elfldr.elf] Error spawning payload\n\r\0").await.unwrap();
        });
        let error = send_payload_at(&root, id, "ps5", "127.0.0.1", port.into(), &Settings::default(), test_timing()).await.unwrap_err();
        assert!(error.contains("Error spawning payload"));
        assert!(error.contains("not resent"));
        assert_eq!(read_index(&root).unwrap().entries.iter().find(|entry| &entry.id == id).unwrap().last_result.as_deref(), Some(error.as_str()));
        server.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn loader_silence_is_bounded_and_not_mistaken_for_rejection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.read_to_end(&mut Vec::new()).await.unwrap();
            sleep(Duration::from_secs(60)).await;
        });
        let mut trace = Trace::new();
        let mut reply = send_bytes("127.0.0.1", port, b"payload", true, &mut trace).await.unwrap().unwrap();
        let started = Instant::now();
        reply.wait(Duration::from_millis(60)).await;
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(reply.record(&mut trace).is_none());
        assert!(trace.steps.last().unwrap().detail.contains("No loader output"));
        drop(reply);
        server.abort();
    }

    #[test]
    fn loader_capture_is_bounded_and_detects_split_rejections_after_the_limit() {
        let mut output = LoaderOutput::default();
        output.append(&vec![b'x'; MAX_LOADER_OUTPUT + 1]);
        output.append(b"\n[elfldr.elf] Error spa");
        output.append(b"wning payload\n\r\0");
        assert_eq!(output.bytes.len(), MAX_LOADER_OUTPUT);
        assert!(output.truncated);
        assert_eq!(output.rejection.as_deref(), Some("[elfldr.elf] Error spawning payload"));
        let mut ordinary = LoaderOutput::default();
        ordinary.append(b"SSPI loaded; error count 0\n");
        assert!(ordinary.rejection.is_none());
    }

    #[test]
    fn receiver_fatal_markers_are_exact_fragment_safe_and_bounded() {
        let marker = b"[SSPI startup] load libraries failed 0xffffffff\n";
        for split in 0..marker.len() {
            let mut output = LoaderOutput::default();
            output.append(&vec![b'x'; MAX_LOADER_OUTPUT + 100]);
            output.append(b"\n");
            output.append(&marker[..split]);
            assert!(output.startup_failure.is_none());
            output.append(&marker[split..]);
            assert_eq!(output.startup_failure.as_deref(), Some("[SSPI startup] load libraries failed 0xffffffff"));
            assert!(output.rejection.is_none());
            assert_eq!(output.bytes.len(), MAX_LOADER_OUTPUT);
        }
        for line in [
            "catalog: [SSPI startup] load libraries failed 0xffffffff",
            "[SSPI startup] arbitrary failed 0xffffffff",
            "[SSPI startup] load libraries failed 0xffffffff extra",
            "[SSPI startup] load libraries failed 0xfffffff",
            "[SSPI startup] load libraries failed 0xgggggggg",
            "[SSPI startup] runtime init complete 0x00000000",
            "[SSPI startup] listener ready 0x0000239a",
            "[SceLncUtil] getAppStatus: LNC_ISOK::0x80940004",
        ] {
            let mut output = LoaderOutput::default();
            output.append(format!("{line}\n").as_bytes());
            assert!(output.startup_failure.is_none(), "{line}");
        }
    }

    #[tokio::test]
    async fn builtin_fatal_startup_interrupts_inflight_probe_without_resending() {
        let root = temp_root();
        let receiver = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let receiver_port = receiver.local_addr().unwrap().port();
        drop(receiver);
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = loader.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = loader.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, RECEIVER_ELF);
            // A listening but silent receiver makes readiness probes block until cancelled.
            let receiver = TcpListener::bind((Ipv4Addr::LOCALHOST, receiver_port)).await.unwrap();
            sleep(Duration::from_millis(50)).await;
            stream.write_all(b"[SSPI startup] runtime init complete 0x00000000\n[SceLncUtil] getAppStatus: LNC_ISOK::0x80940004\n[SSPI startup] load lib").await.unwrap();
            sleep(Duration::from_millis(20)).await;
            let fatal_at = Instant::now();
            stream.write_all(b"raries failed 0xffffffff\n").await.unwrap();
            assert!(timeout(Duration::from_millis(250), loader.accept()).await.is_err(), "payload must not be resent");
            drop(receiver);
            fatal_at
        });
        let mut trace = Trace::new();
        let error = send_ps5_receiver("127.0.0.1", port.into(), receiver_port, Ps5Timing { verify_timeout: Duration::from_secs(30), ..test_timing() }, &mut trace).await.unwrap_err();
        let finished = Instant::now();
        let fatal_at = server.await.unwrap();
        assert!(finished.duration_since(fatal_at) < Duration::from_millis(500));
        assert!(error.starts_with("SSPI receiver startup failed:"), "{error}");
        assert!(error.contains("load libraries failed 0xffffffff"));
        assert!(error.contains("runtime init complete"));
        assert!(error.contains("not resent"));
        assert!(trace.steps.iter().any(|step| step.label == "Loader output" && !step.ok));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn manual_payload_startup_text_does_not_claim_execution_or_fail() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let input = root.join("catalog.bin");
        fs::write(&input, b"payload").unwrap();
        let entries = add_payloads_at(&root, &[input.to_string_lossy().into_owned()], "ps5").unwrap();
        let id = &entries.iter().find(|entry| !entry.builtin).unwrap().id;
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = loader.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = loader.accept().await.unwrap();
            stream.read_to_end(&mut Vec::new()).await.unwrap();
            stream.write_all(b"[SSPI startup] load libraries failed 0xffffffff\n").await.unwrap();
        });
        let result = send_payload_at(&root, id, "ps5", "127.0.0.1", port.into(), &Settings::default(), test_timing()).await.unwrap();
        assert!(!result.verified);
        assert!(result.message.contains("Console execution was not verified"));
        assert!(result.steps.iter().any(|step| step.label == "Loader output" && step.ok && step.detail.contains("load libraries failed")));
        server.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn loader_rejection_is_drained_while_large_upload_is_still_in_progress() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut header = [0; 64];
            stream.read_exact(&mut header).await.unwrap();
            stream.write_all(b"[elfldr.elf] Error reading ELF payload\n").await.unwrap();
            stream.shutdown().await.unwrap();
            sleep(Duration::from_millis(30)).await;
        });
        let mut trace = Trace::new();
        let bytes = vec![0x5a; 32 * 1024 * 1024];
        let result = timeout(Duration::from_secs(5), send_bytes("127.0.0.1", port, &bytes, true, &mut trace)).await.unwrap();
        let error = match result {
            Ok(Some(mut reply)) => { reply.wait(Duration::from_secs(1)).await; reply.record(&mut trace).unwrap() },
            Err(error) => error,
            _ => panic!("PS5 output capture missing"),
        };
        assert!(error.contains("Error reading ELF payload"), "{error}");
        assert!(!error.contains("No payload bytes were sent"), "{error}");
        assert!(trace.steps.iter().any(|step| step.label == "Loader output"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn ps4_raw_send_does_not_wait_for_or_interpret_loader_output() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, b"ps4 payload");
            sleep(Duration::from_secs(60)).await;
        });
        let mut trace = Trace::new();
        let reply = timeout(Duration::from_secs(1), send_bytes("127.0.0.1", port, b"ps4 payload", false, &mut trace)).await.unwrap().unwrap();
        assert!(reply.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn ps5_verification_keeps_late_startup_diagnostics_and_stops_at_deadline() {
        let root = temp_root();
        let receiver_probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let receiver_port = receiver_probe.local_addr().unwrap().port();
        drop(receiver_probe);
        let loader = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let loader_port = loader.local_addr().unwrap().port();
        let loader_task = tokio::spawn(async move {
            let (mut stream, _) = loader.accept().await.unwrap();
            stream.read_to_end(&mut Vec::new()).await.unwrap();
            sleep(Duration::from_millis(100)).await;
            stream.write_all(b"SSPI SDK startup: kernel init failed\n").await.unwrap();
            sleep(Duration::from_secs(60)).await;
        });
        let started = Instant::now();
        let result = send_payload_at(&root, BUILTIN_PS5, "ps5", "127.0.0.1", loader_port.into(), &Settings { ps5_port: receiver_port, ..Settings::default() }, Ps5Timing { verify_timeout: Duration::from_millis(300), ..test_timing() }).await.unwrap();
        assert!(!result.verified);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(result.steps.iter().any(|step| step.label == "Loader output" && step.detail.contains("kernel init failed")));
        loader_task.abort();
        fs::remove_dir_all(root).unwrap();
    }

}
