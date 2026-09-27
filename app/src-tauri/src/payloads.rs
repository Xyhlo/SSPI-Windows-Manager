//! Payload library, binloader delivery and local theme files.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{sleep, timeout},
};

use crate::{AppState, Settings, PS4_RECEIVER_ELF, RECEIVER_ELF, RECEIVER_VERSION};

const INDEX_NAME: &str = "payloads.json";
const MIN_PAYLOAD_SIZE: usize = 1;
const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;
const MAX_THEME_SIZE: usize = 8 * 1024 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
const BUILTIN_PS5: &str = "builtin:ps5-receiver";
const BUILTIN_PS4: &str = "builtin:ps4-receiver";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PayloadEntry {
    id: String,
    name: String,
    file_name: String,
    path: String,
    size: usize,
    sha256: String,
    target: String,
    builtin: bool,
    added_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_sent_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PayloadSendResult {
    message: String,
    bytes: usize,
    port: u32,
    verified: bool,
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
    verify_timeout: Duration::from_secs(12),
    poll_interval: Duration::from_millis(500),
};

#[tauri::command]
pub(super) fn list_payloads(app: AppHandle) -> Result<Vec<PayloadEntry>, String> {
    list_payloads_at(&store_root(&app)?)
}

#[tauri::command]
pub(super) fn add_payloads(
    app: AppHandle,
    paths: Vec<String>,
    target: String,
) -> Result<Vec<PayloadEntry>, String> {
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
    update_payload_at(&store_root(&app)?, &id, name, target, notes)
}

#[tauri::command]
pub(super) fn remove_payload(app: AppHandle, id: String) -> Result<Vec<PayloadEntry>, String> {
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
    let settings = state
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    send_payload_at(
        &store_root(&app)?,
        &id,
        &target,
        &host,
        port,
        &settings,
        DEFAULT_PS5_TIMING,
    )
    .await
}

#[tauri::command]
pub(super) fn save_theme_file(path: String, contents: String) -> Result<(), String> {
    let destination = PathBuf::from(&path);
    validate_theme_path(&destination)?;
    if contents.len() > MAX_THEME_SIZE {
        return Err("Theme files must be 8 MiB or smaller.".into());
    }
    atomic_write(&destination, contents.as_bytes())
        .map_err(|_| "Could not save the theme file.".to_string())
}

#[tauri::command]
pub(super) fn load_theme_file(path: String) -> Result<String, String> {
    let source = PathBuf::from(&path);
    validate_theme_path(&source)?;
    let metadata =
        fs::metadata(&source).map_err(|_| "Could not read the theme file.".to_string())?;
    if metadata.len() > MAX_THEME_SIZE as u64 {
        return Err("Theme files must be 8 MiB or smaller.".into());
    }
    fs::read_to_string(source).map_err(|_| "The theme file must contain valid UTF-8 text.".into())
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
    entries.extend(index.entries.into_iter().filter(|entry| !entry.builtin));
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
            return Err("Payload files must be between 1 byte and 64 MiB.".into());
        }
        let bytes = fs::read(&source)
            .map_err(|_| "Could not read one of the selected payload files.".to_string())?;
        if bytes.len() < MIN_PAYLOAD_SIZE || bytes.len() > MAX_PAYLOAD_SIZE {
            return Err("Payload files must be between 1 byte and 64 MiB.".into());
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
    let mut index = read_index(root)?;
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
            let message = crate::ps4_receiver::load_ps4_receiver(
                host.trim().into(),
                port as u16,
                settings.ps4_receiver_port,
            )
            .await?;
            Ok(PayloadSendResult {
                message,
                bytes: PS4_RECEIVER_ELF.len(),
                port,
                verified: true,
            })
        } else if id == BUILTIN_PS5 {
            send_ps5_receiver(host.trim(), port, settings.ps5_port, ps5_timing).await
        } else {
            let file_name = safe_store_file_name(&entry.file_name, &entry.id)?;
            let bytes = fs::read(root.join(file_name))
                .map_err(|_| "Could not read the stored payload.".to_string())?;
            send_bytes(host.trim(), port as u16, &bytes).await?;
            Ok(PayloadSendResult {
                message: format!(
                    "Sent {} ({} bytes) to {}:{}.",
                    entry.name,
                    bytes.len(),
                    host.trim(),
                    port
                ),
                bytes: bytes.len(),
                port,
                verified: false,
            })
        }
    }
    .await;

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

async fn send_ps5_receiver(
    host: &str,
    loader_port: u32,
    receiver_port: u16,
    timing: Ps5Timing,
) -> Result<PayloadSendResult, String> {
    match probe_receiver(host, receiver_port).await? {
        Some(config)
            if config.version == RECEIVER_VERSION && config.platform.as_deref() == Some("ps5") =>
        {
            return Ok(PayloadSendResult {
                message: format!("Receiver already running (v{RECEIVER_VERSION})"),
                bytes: 0,
                port: loader_port,
                verified: true,
            });
        }
        Some(config) => {
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
            send_stop(host, receiver_port).await?;
            wait_for_port_closed(host, receiver_port, timing.stop_timeout).await?;
        }
        None => {}
    }

    send_bytes(host, loader_port as u16, RECEIVER_ELF).await?;
    let deadline = Instant::now() + timing.verify_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(PayloadSendResult {
                message: format!(
                    "Payload sent to {host}:{loader_port}, but the receiver hasn't answered yet."
                ),
                bytes: RECEIVER_ELF.len(),
                port: loader_port,
                verified: false,
            });
        }
        if let Ok(Ok(Some(config))) = timeout(remaining, probe_receiver(host, receiver_port)).await
        {
            if config.version == RECEIVER_VERSION && config.platform.as_deref() == Some("ps5") {
                return Ok(PayloadSendResult {
                    message: format!("Receiver loaded and verified (v{RECEIVER_VERSION})."),
                    bytes: RECEIVER_ELF.len(),
                    port: loader_port,
                    verified: true,
                });
            }
        }
        sleep(timing.poll_interval).await;
    }
}

async fn probe_receiver(host: &str, port: u16) -> Result<Option<ReceiverConfig>, String> {
    let mut stream = match timeout(
        Duration::from_millis(1500),
        TcpStream::connect((host, port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) | Err(_) => return Ok(None),
    };
    let (ping_code, ping_body) = exchange(&mut stream, 0x01, &[]).await?;
    if ping_code != 1 || ping_body != b"SSPI" {
        return Err("The service at the PS5 receiver port isn't an SSPI receiver.".into());
    }
    let (code, body) = exchange(&mut stream, 0x53, &[]).await?;
    if code != 3 {
        return Err("The PS5 receiver didn't return its configuration.".into());
    }
    parse_receiver_config(&body)
        .map(Some)
        .ok_or_else(|| "The PS5 receiver returned invalid configuration.".into())
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

async fn send_bytes(host: &str, port: u16, bytes: &[u8]) -> Result<(), String> {
    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect((host, port)))
        .await
        .map_err(|_| "The payload loader connection timed out.".to_string())?
        .map_err(|_| "Could not connect to the payload loader.".to_string())?;
    timeout(Duration::from_secs(90), async {
        stream.write_all(bytes).await?;
        stream.shutdown().await?;
        Ok::<(), io::Error>(())
    })
    .await
    .map_err(|_| "Sending the payload timed out.".to_string())?
    .map_err(|_| "Could not send the complete payload.".to_string())?;
    drop(stream);
    Ok(())
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
            .map_err(|_| "Could not send receiver request".to_string())?;
        let mut header = [0_u8; 5];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|_| "Incomplete receiver response".to_string())?;
        let response_len =
            u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if response_len > MAX_RESPONSE {
            return Err("Receiver response too large".into());
        }
        let mut response = vec![0; response_len];
        stream
            .read_exact(&mut response)
            .await
            .map_err(|_| "Incomplete receiver response".to_string())?;
        Ok((header[0], response))
    })
    .await
    .map_err(|_| "Receiver request timed out".to_string())?
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
            .unwrap_or("theme"),
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

fn validate_theme_path(path: &Path) -> Result<(), String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    if extension.eq_ignore_ascii_case("sspitheme") {
        Ok(())
    } else {
        Err("Theme files must use the .sspitheme extension.".into())
    }
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
    use serde_json::json;
    use std::net::Ipv4Addr;
    use tokio::{net::TcpListener, task::JoinHandle};

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("sspi-payloads-test-{}", uuid::Uuid::new_v4()))
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
        assert!(result.message.contains("receiver hasn't answered yet"));
        loader_task.await.unwrap();
    }

    #[test]
    fn theme_files_enforce_extension_size_and_utf8() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let theme = root.join("theme.SSPITHEME");
        save_theme_file(
            theme.to_string_lossy().into_owned(),
            "theme contents".into(),
        )
        .unwrap();
        assert_eq!(
            load_theme_file(theme.to_string_lossy().into_owned()).unwrap(),
            "theme contents"
        );
        assert!(save_theme_file(
            root.join("bad.json").to_string_lossy().into_owned(),
            "{}".into()
        )
        .is_err());
        assert!(save_theme_file(
            theme.to_string_lossy().into_owned(),
            "x".repeat(MAX_THEME_SIZE + 1)
        )
        .unwrap_err()
        .contains("8 MiB"));
        fs::write(&theme, vec![0xff, 0xfe]).unwrap();
        assert!(load_theme_file(theme.to_string_lossy().into_owned())
            .unwrap_err()
            .contains("UTF-8"));
        let _ = fs::remove_dir_all(root);
    }
}
