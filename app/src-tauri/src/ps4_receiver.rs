use super::*;
use std::net::IpAddr;

pub(super) const MONITORING_ENDED: &str = "PS4 installation is not confirmed. Keep SSPI open and use Retry to check the console again.";
const NOT_RUNNING: &str = "PS4 receiver isn't running. Load it from Settings → Consoles (needs GoldHEN 2.4b18.5+)";
const INVALID_PAYLOAD: &str = "The bundled PS4 receiver is not a self-contained GoldHEN payload. Rebuild or update Windows Manager and try again.";
const ADDCONT_BROKEN: u32 = 0x80A3000B;
static DELIVERY: AsyncMutex<()> = AsyncMutex::const_new(());

pub(super) fn validate_settings(transport: Option<&str>, receiver: Option<u16>, loader: Option<u16>, serve: Option<u16>) -> Result<(), String> {
    if transport.is_some_and(|s| !matches!(s, "receiver" | "inbox")) { return Err("PS4 transport must be receiver or inbox.".into()); }
    if receiver.is_some_and(|p| p < 1024) { return Err("PS4 receiver port must be between 1024 and 65535.".into()); }
    if loader == Some(0) { return Err("PS4 loader port must be between 1 and 65535.".into()); }
    if serve == Some(0) { return Err("PS4 serve port must be between 1 and 65535.".into()); }
    Ok(())
}

fn error_text(text: &str) -> String {
    // Only expose the payload's human-readable error, never its path/URL fields.
    let text = if text.starts_with("PS5 closed the socket (") {
        text.split_once('[').map(|(_, detail)| detail.trim_end_matches(']')).unwrap_or("Receiver closed the socket; reload the PS4 receiver and retry")
    } else { text };
    let text = redact_delivery_error(text, "ps4");
    let paths = Regex::new(r#"(?i)(?:[a-z]:[\\/]|\\\\|/(?:user|data|home|tmp)/)[^\s\"']+"#).unwrap();
    let text = paths.replace_all(&text, "[path]");
    if text.starts_with("PS4:") { text.into_owned() } else { format!("PS4: {text}") }
}

#[derive(Debug)]
struct Failure { message: String, code: Option<u32>, registration_rejected: bool }
impl From<String> for Failure {
    fn from(message: String) -> Self { Self { message, code: None, registration_rejected: false } }
}
fn monitoring_ended(detail: &str) -> Failure {
    if detail.starts_with(MONITORING_ENDED) { return detail.to_string().into(); }
    if detail.trim().is_empty() { MONITORING_ENDED.to_string().into() }
    else { format!("{MONITORING_ENDED} {}", error_text(detail)).into() }
}
fn privilege_error(value: &Value) -> String {
    let diagnostics: Vec<_> = value["diagnostics"].as_array().into_iter().flatten()
        .filter_map(Value::as_str).filter(|s| !s.trim().is_empty()).collect();
    let detail = diagnostics.iter().find(|s| {
        let text = s.to_ascii_lowercase();
        ["privileg", "credential", "jail", "sandbox", "libjbc", "uid"].iter().any(|word| text.contains(word))
    }).copied().or_else(|| diagnostics.first().copied())
        .or_else(|| value["error"].as_str().filter(|s| !s.trim().is_empty())).unwrap_or("system credentials unavailable");
    let detail = error_text(detail);
    let detail: String = detail.strip_prefix("PS4: ").unwrap_or(&detail).chars().filter(|c| !c.is_control()).take(300).collect();
    format!("Receiver is running without system privileges ({detail}). Reload it after enabling GoldHEN.")
}
fn sdk_code(value: &Value) -> Option<u32> {
    ["error_code", "install_api_code", "api_code", "status_api_code", "auth_restore_code"].iter()
        .filter_map(|key| value[*key].as_i64()).find(|code| *code != 0).map(|code| code as u32)
}
fn reply_json(code: u8, body: &[u8]) -> Result<Value, Failure> {
    let text = String::from_utf8_lossy(body);
    let value: Value = serde_json::from_str(text.strip_prefix("OK ").unwrap_or(&text).trim_end_matches('\0'))
        .map_err(|_| Failure::from(error_text("Receiver returned an invalid install response")))?;
    if code == 2 || value["state"] == "failed" || value["status"] == "failed" || sdk_code(&value).is_some() {
        if value["stage"] == "privileges" { return Err(privilege_error(&value).into()); }
        let sdk = sdk_code(&value);
        let reason = value["error"].as_str().filter(|s| !s.is_empty()).unwrap_or("Receiver rejected the operation");
        let message = match sdk { Some(code) => format!("{} (0x{code:08X})", error_text(reason)), None => error_text(reason) };
        return Err(Failure { message, code: sdk, registration_rejected: false });
    }
    if !matches!(code, 1 | 3) { return Err(error_text("Unexpected receiver reply code").into()); }
    Ok(value)
}

fn check_config(endpoint: &ReceiverEndpoint, config: &Value) -> Result<(), String> {
    if config["platform"] != "ps4" { return Err("PS4: This endpoint is not a PS4 receiver.".into()); }
    let version = config["version"].as_str().unwrap_or("unknown");
    if version != endpoint.expected_version {
        return Err(format!("PS4 receiver is v{version}, app needs v{}. Load the receiver from Settings → Consoles, then retry.", endpoint.expected_version));
    }
    if config["jailbroken"] != true { return Err(privilege_error(config)); }
    for (key, label) in [("bgft", "BGFT"), ("appinst", "AppInst")] {
        let status = config[key].as_str().unwrap_or("missing diagnostics");
        if status != "ready" {
            return Err(error_text(&format!("{label} {}", status.replacen("unavailable:", "unavailable: ", 1))));
        }
    }
    if config["writable"] != true { return Err("PS4: Receiver data directory is not writable. Reload with GoldHEN enabled.".into()); }
    for capability in endpoint.required_capabilities {
        if !config["capabilities"].as_array().is_some_and(|caps| caps.iter().any(|v| v == capability)) {
            return Err(format!("PS4: Receiver is missing {capability}. Load the matching receiver payload."));
        }
    }
    Ok(())
}

async fn config_on(socket: &mut TcpStream) -> Result<Value, String> {
    let (code, body) = frame(socket, 0x53, &[]).await.map_err(|e| error_text(&e))?;
    reply_json(code, &body).map_err(|e| e.message)
}
async fn checked_connection(endpoint: &ReceiverEndpoint) -> Result<TcpStream, String> {
    validate_receiver_candidate(&endpoint.host, endpoint.port)?;
    ping(endpoint).await.map_err(|_| NOT_RUNNING.to_string())?;
    let mut socket = connect_receiver(endpoint, "receiver version").await.map_err(|_| NOT_RUNNING.to_string())?;
    check_config(endpoint, &config_on(&mut socket).await?)?;
    Ok(socket)
}
pub(super) async fn ready(endpoint: &ReceiverEndpoint) -> Result<(), String> { checked_connection(endpoint).await.map(|_| ()) }

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Ps4InstalledTitle {
    pub title_id: String,
    pub name: String,
    pub version: Option<String>,
    pub base_version: Option<String>,
    pub update_version: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Ps4LibrarySnapshot {
    pub entries: Vec<Ps4InstalledTitle>,
    pub complete: bool,
    pub truncated: bool,
    pub errors: Vec<String>,
    pub metadata_warnings: Vec<String>,
}

fn installed_metadata_field<'a>(bytes: &'a [u8], cursor: &mut usize, limit: usize) -> Result<&'a [u8], String> {
    let end = cursor.checked_add(4).ok_or_else(|| "PS4 metadata length overflow".to_string())?;
    let length = u32::from_le_bytes(bytes.get(*cursor..end).ok_or_else(|| "PS4 installed metadata is truncated".to_string())?.try_into().unwrap()) as usize;
    *cursor = end;
    if length > limit { return Err("PS4 installed metadata exceeds its size limit".into()); }
    let end = cursor.checked_add(length).ok_or_else(|| "PS4 metadata length overflow".to_string())?;
    let field = bytes.get(*cursor..end).ok_or_else(|| "PS4 installed metadata is truncated".to_string())?;
    *cursor = end;
    Ok(field)
}

fn valid_installed_title_id(value: &str) -> bool {
    value.len() == 9 && value.starts_with("CUSA") && value.as_bytes()[4..].iter().all(u8::is_ascii_digit)
}

pub(super) fn compact_installed_icon(bytes: &[u8]) -> Option<Vec<u8>> {
    if !pkg_meta::is_valid_png(bytes) { return None; }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    for size in [256, 128, 64] {
        let thumbnail = decoded.thumbnail(size, size).to_rgb8();
        let mut output = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 78).encode_image(&thumbnail).ok()?;
        let output = output.into_inner();
        if output.len() <= 128 * 1024 { return Some(output); }
    }
    None
}

async fn decode_installed_metadata(expected_title_id: &str, data: &[u8]) -> Result<(Option<String>, Option<String>, Option<String>, Option<String>, Vec<String>), String> {
    const MAX_SFO: usize = 64 * 1024;
    const MAX_ICON: usize = 2 * 1024 * 1024;
    let mut cursor = 0;
    let base_sfo = installed_metadata_field(data, &mut cursor, MAX_SFO)?;
    let patch_sfo = installed_metadata_field(data, &mut cursor, MAX_SFO)?;
    let icon_bytes = installed_metadata_field(data, &mut cursor, MAX_ICON)?;
    if cursor != data.len() { return Err("PS4 installed metadata contains trailing bytes".into()); }
    let mut warnings = Vec::new();
    let base = if base_sfo.is_empty() { None } else { pkg_meta::parse_sfo(base_sfo) };
    let patch = if patch_sfo.is_empty() { None } else { pkg_meta::parse_sfo(patch_sfo) };
    let base_mismatch = base.as_ref().and_then(|(_, _, _, id)| id.as_deref()).is_some_and(|id| !id.eq_ignore_ascii_case(expected_title_id));
    let patch_mismatch = patch.as_ref().and_then(|(_, _, _, id)| id.as_deref()).is_some_and(|id| !id.eq_ignore_ascii_case(expected_title_id));
    if base_mismatch { warnings.push("base param.sfo title ID does not match the installed title".into()); }
    if patch_mismatch { warnings.push("patch param.sfo title ID does not match the installed title".into()); }
    let base = if base_mismatch { None } else { base };
    let patch = if patch_mismatch { None } else { patch };
    let name = patch.as_ref().and_then(|(_, title, _, _)| title.clone()).or_else(|| base.as_ref().and_then(|(_, title, _, _)| title.clone()));
    let base_version = base.as_ref().and_then(|(_, _, version, _)| version.clone());
    let update_version = patch.as_ref().and_then(|(_, _, version, _)| version.clone());
    if base.is_none() && patch.is_none() { warnings.push("title metadata is unavailable; showing the title ID".into()); }
    let icon = if icon_bytes.is_empty() { None } else {
        let native = icon_bytes.to_vec();
        let compact = tokio::task::spawn_blocking(move || compact_installed_icon(&native)).await.ok().flatten();
        if compact.is_none() { warnings.push("cover artwork could not be decoded".into()); }
        compact.map(|bytes| format!("data:image/jpeg;base64,{}", BASE64.encode(bytes)))
    };
    Ok((name, base_version, update_version, icon, warnings))
}

#[tauri::command]
pub(super) async fn list_ps4_library(host: String, port: u16) -> Result<Ps4LibrarySnapshot, String> {
    let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host: host.trim().into(), ps4_receiver_port: port, ..Settings::default() });
    let mut socket = checked_connection(&endpoint).await?;
    let (code, body) = frame(&mut socket, 0x5e, &[]).await.map_err(|error| error_text(&error))?;
    if code != 3 { return Err(error_text(&String::from_utf8_lossy(&body))); }
    let response: Value = serde_json::from_slice(&body).map_err(|_| error_text("PS4 installed-library response is invalid JSON"))?;
    let titles = response["titles"].as_array().ok_or_else(|| error_text("PS4 installed-library response has no title list"))?;
    let truncated = response["truncated"].as_bool().unwrap_or(true);
    let mut complete = response["complete"].as_bool().unwrap_or(false) && !truncated;
    let mut errors: Vec<String> = response["errors"].as_array().into_iter().flatten()
        .filter_map(Value::as_str).map(|text| text.chars().filter(|c| !c.is_control()).take(240).collect()).collect();
    if response["errorsTruncated"].as_bool().unwrap_or(false) { errors.push("Some PS4 inventory errors were omitted.".into()); }
    if titles.len() > 2048 {
        complete = false;
        errors.push("PS4 installed-library response exceeded the 2,048-title limit.".into());
    }

    let mut entries = Vec::with_capacity(titles.len().min(2048));
    let mut metadata_warnings = Vec::new();
    for value in titles.iter().take(2048) {
        let Some(title_id) = value.as_str().filter(|id| valid_installed_title_id(id)) else {
            complete = false;
            errors.push("PS4 installed-library response contained an invalid title ID.".into());
            continue;
        };
        let mut request = title_id.as_bytes().to_vec();
        request.push(0);
        let metadata = match frame(&mut socket, 0x5f, &request).await {
            Ok((3, data)) => Some(data),
            Ok((_, data)) => {
                metadata_warnings.push(format!("{title_id}: {}", String::from_utf8_lossy(&data).chars().filter(|c| !c.is_control()).take(160).collect::<String>()));
                None
            }
            Err(error) => {
                metadata_warnings.push(format!("{title_id}: {}", error_text(&error)));
                None
            }
        };
        let (name, base_version, update_version, icon) = if let Some(data) = metadata {
            match decode_installed_metadata(title_id, &data).await {
                Ok((name, base_version, update_version, icon, warnings)) => {
                    metadata_warnings.extend(warnings.into_iter().map(|warning| format!("{title_id}: {warning}")));
                    (name.unwrap_or_else(|| title_id.to_string()), base_version, update_version, icon)
                }
                Err(error) => {
                    metadata_warnings.push(format!("{title_id}: {error}"));
                    (title_id.to_string(), None, None, None)
                }
            }
        } else { (title_id.to_string(), None, None, None) };
        let version = update_version.clone().or_else(|| base_version.clone());
        entries.push(Ps4InstalledTitle { title_id: title_id.to_string(), name, version, base_version, update_version, icon });
    }
    errors.truncate(64);
    metadata_warnings.truncate(256);
    Ok(Ps4LibrarySnapshot { entries, complete, truncated, errors, metadata_warnings })
}

#[tauri::command]
pub(super) async fn test_ps4_receiver(host: String, port: u16) -> Result<String, String> {
    let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host: host.trim().into(), ps4_receiver_port: port, ..Settings::default() });
    ready(&endpoint).await?;
    Ok(format!("Receiver verified (v{} · BGFT ready · AppInst ready)", endpoint.expected_version))
}

fn validate_payload(payload: &[u8]) -> Result<(), String> {
    let valid = (|| -> Option<bool> {
        let header = payload.get(..64)?;
        let u16_at = |bytes: &[u8], offset| u16::from_le_bytes(bytes[offset..offset+2].try_into().unwrap());
        let u32_at = |bytes: &[u8], offset| u32::from_le_bytes(bytes[offset..offset+4].try_into().unwrap());
        let u64_at = |bytes: &[u8], offset| u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap());
        if &header[..7] != b"\x7fELF\x02\x01\x01" || !matches!(u16_at(header, 16), 2 | 3)
            || u16_at(header, 18) != 62 || u32_at(header, 20) != 1 || u16_at(header, 52) != 64 { return Some(false); }
        let entry = u64_at(header, 24);
        let table = usize::try_from(u64_at(header, 32)).ok()?;
        let stride = usize::from(u16_at(header, 54));
        let count = usize::from(u16_at(header, 56));
        if stride != 56 || count == 0 { return Some(false); }
        let entries = payload.get(table..table.checked_add(stride.checked_mul(count)?)?)?;
        let mut executable_entry = false;
        for segment in entries.chunks_exact(stride) {
            let kind = u32_at(segment, 0);
            if matches!(kind, 3 | 7) { return Some(false); } // PT_INTERP / PT_TLS require an application loader.
            if !matches!(kind, 1 | 2) { continue; }
            let offset = usize::try_from(u64_at(segment, 8)).ok()?;
            let size = usize::try_from(u64_at(segment, 32)).ok()?;
            let bytes = payload.get(offset..offset.checked_add(size)?)?;
            if kind == 1 {
                if u64_at(segment, 32) > u64_at(segment, 40) { return Some(false); }
                let start = u64_at(segment, 16);
                if u32_at(segment, 4) & 1 != 0 && entry >= start && entry < start.checked_add(size as u64)? {
                    executable_entry = true;
                }
            } else {
                if size % 16 != 0 { return Some(false); }
                for dynamic in bytes.chunks_exact(16) {
                    match u64_at(dynamic, 0) {
                        0 => break,
                        1 => return Some(false), // DT_NEEDED: GoldHEN does not resolve OpenOrbis imports.
                        _ => {},
                    }
                }
            }
        }
        Some(executable_entry)
    })().unwrap_or(false);
    if valid { Ok(()) } else { Err(INVALID_PAYLOAD.into()) }
}
#[tauri::command]
pub(super) fn export_ps4_receiver_payload() -> Result<String, String> {
    validate_payload(PS4_RECEIVER_ELF)?;
    let downloads = std::env::var("USERPROFILE").map(PathBuf::from).map(|p| p.join("Downloads"))
        .map_err(|_| "Windows Downloads folder could not be located".to_string())?;
    std::fs::create_dir_all(&downloads).map_err(|_| "Could not create the Windows Downloads folder".to_string())?;
    for number in 0..100 {
        let name = if number == 0 { "sspi_ps4_receiver.elf".into() } else { format!("sspi_ps4_receiver ({number}).elf") };
        let destination = downloads.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&destination) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(PS4_RECEIVER_ELF).map_err(|_| "Could not write the PS4 receiver to Downloads".to_string())?;
                return Ok(destination.to_string_lossy().into_owned());
            },
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(_) => return Err("Could not write the PS4 receiver to Downloads".into()),
        }
    }
    Err("Too many PS4 receiver payload copies already exist in Downloads".into())
}

fn loader_error(port: u16, step: &str) -> String {
    format!("Could not send the payload to GoldHEN BinLoader on port {port} ({step}). Enable BinLoader in GoldHEN settings; if it is already enabled, toggle it off and on, then retry.")
}
async fn send_payload(host: &str, port: u16, payload: &[u8]) -> Result<(), String> {
    // BinLoader receives executable bytes from the first byte of the connection.
    // An HTTP probe would be treated as another payload.
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut socket = TcpStream::connect((host, port)).await?;
        socket.write_all(payload).await?;
        socket.shutdown().await
    }).await.map_err(|_| loader_error(port, "raw TCP upload timed out"))?
        .map_err(|_| loader_error(port, "raw TCP upload failed"))
}
async fn probe_receiver(endpoint: &ReceiverEndpoint) -> Option<TcpStream> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut socket = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await.ok()?;
        let (code, body) = frame(&mut socket, 0x01, &[]).await.ok()?;
        (code == 1 && body == b"SSPI").then_some(socket)
    }).await.ok().flatten()
}
async fn load_payload(endpoint: &ReceiverEndpoint, loader_port: u16, payload: &[u8]) -> Result<String, String> {
    validate_payload(payload)?;
    validate_receiver_candidate(&endpoint.host, endpoint.port)?;
    validate_settings(None, Some(endpoint.port), Some(loader_port), None)?;
    if endpoint.host.chars().any(|c| c.is_control() || c.is_whitespace()) { return Err("Provide a valid PS4 host".into()); }
    if let Some(mut socket) = probe_receiver(endpoint).await {
        let config = tokio::time::timeout(Duration::from_secs(3), config_on(&mut socket)).await
            .map_err(|_| "PS4: Reading the running receiver version timed out".to_string())??;
        if config["platform"] != "ps4" { return Err("PS4: This endpoint is not a PS4 receiver. Check the PS4 address and receiver port.".into()); }
        if config["version"] == endpoint.expected_version {
            check_config(endpoint, &config)?;
            return Ok(format!("Receiver already running (v{})", endpoint.expected_version));
        }
        // Earlier receivers call _exit on STOP, terminating the loader's host process.
        let version = config["version"].as_str().unwrap_or("unknown");
        let parts: Option<Vec<u32>> = version.split('.').map(|part| part.parse().ok()).collect();
        if !parts.is_some_and(|parts| parts.len() == 3 && parts.as_slice() >= [1, 0, 1].as_slice()) {
            return Err(format!("PS4 receiver v{version} cannot be safely replaced while running. Restart the PS4, enable GoldHEN and BinLoader, then load the matching receiver."));
        }
        if !config["capabilities"].as_array().is_some_and(|caps| caps.iter().any(|v| v == "stop")) {
            return Err("PS4: The previous receiver cannot be stopped remotely. Restart the console, enable GoldHEN and BinLoader, then load the receiver again.".into());
        }
        let (code, _) = tokio::time::timeout(Duration::from_secs(3), frame(&mut socket, 0x5A, &[])).await
            .map_err(|_| "PS4: Stopping the previous receiver timed out".to_string())??;
        if code != 1 { return Err("PS4: The previous receiver refused STOP. Close it on the console, then retry.".into()); }
        drop(socket);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !matches!(tokio::time::timeout(Duration::from_millis(300), TcpStream::connect((endpoint.host.as_str(), endpoint.port))).await, Ok(Ok(_))) { break; }
            if Instant::now() >= deadline { return Err("PS4: The previous receiver did not stop within 5 seconds.".into()); }
            sleep(Duration::from_millis(100)).await;
        }
    }
    send_payload(&endpoint.host, loader_port, payload).await?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(mut socket) = probe_receiver(endpoint).await {
                check_config(endpoint, &config_on(&mut socket).await?)?;
                return Ok(format!("Receiver loaded (v{})", endpoint.expected_version));
            }
            sleep(Duration::from_millis(500)).await;
        }
    }).await;
    result.map_err(|_| format!("Payload sent to GoldHEN BinLoader, but no verified PS4 receiver started on port {} within 20 seconds. GoldHEN's 'Payload received' message only confirms delivery. Use GoldHEN 2.4b18.5+ and check that the receiver port matches its console configuration (default 9114).", endpoint.port))?
}
#[tauri::command]
pub(super) async fn load_ps4_receiver(host: String, loader_port: u16, receiver_port: u16) -> Result<String, String> {
    let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host: host.trim().into(), ps4_receiver_port: receiver_port, ..Settings::default() });
    load_payload(&endpoint, loader_port, PS4_RECEIVER_ELF).await
}

#[derive(Clone, Copy)]
struct Timing { poll: Duration, idle: Duration }
impl Default for Timing {
    fn default() -> Self { Self { poll: Duration::from_secs(2), idle: Duration::from_secs(15 * 60) } }
}
struct Context<'a> {
    app: Option<&'a AppHandle>,
    job: &'a str,
    cancel: &'a watch::Receiver<bool>,
    #[cfg(test)] paused: Option<&'a watch::Receiver<bool>>,
    #[cfg(test)] events: Option<&'a Mutex<Vec<Progress>>>,
}
impl Context<'_> {
    fn paused(&self) -> bool {
        #[cfg(test)] if let Some(paused) = self.paused { return *paused.borrow(); }
        self.app.is_some_and(|app| app.state::<AppState>().jobs.lock().unwrap().get(self.job).is_some_and(|p| p.paused))
    }
    fn report(&self, mut progress: Progress) {
        progress.job_id = self.job.into(); progress.target = "ps4".into();
        #[cfg(test)] if let Some(events) = self.events { events.lock().unwrap().push(progress.clone()); }
        if let Some(app) = self.app { emit(app, progress); }
    }
    async fn checkpoint(&self) -> Result<(), String> {
        if let Some(app) = self.app { transfer_checkpoint(app, self.job, self.cancel).await }
        else if *self.cancel.borrow() { Err("cancelled".into()) } else { Ok(()) }
    }
}

fn rank(kind: &str) -> u8 { match kind { "base" => 0, "update" => 1, _ => 2 } }
async fn delivery_slot(context: &Context<'_>, meta: &pkg_meta::PkgMeta) -> Result<tokio::sync::MutexGuard<'static, ()>, String> {
    loop {
        context.checkpoint().await?;
        let blocked = || context.app.is_some_and(|app| ps4_inbox::blocked_by_lower_rank(
            &app.state::<AppState>().jobs.lock().unwrap(), context.job, &meta.title_id, rank(&meta.kind)));
        if !blocked() {
            let mut cancel = context.cancel.clone();
            let slot = tokio::select! { slot = DELIVERY.lock() => slot, _ = cancel.changed() => return Err("cancelled".into()) };
            if !blocked() { return Ok(slot); }
            drop(slot);
        }
        context.report(Progress { stage: "queued".into(), message: "Waiting for the base game to finish installing on the PS4".into(), ..Default::default() });
        let mut cancel = context.cancel.clone();
        tokio::select! { _ = sleep(Duration::from_secs(2)) => {}, _ = cancel.changed() => return Err("cancelled".into()) }
    }
}
fn package_token(job: &str, path: &Path) -> Result<String, String> {
    let path = std::fs::canonicalize(path).map_err(|_| "PS4: The retained package is missing or unreadable".to_string())?;
    let mut hash = Sha256::new(); hash.update(job.as_bytes()); hash.update(path.to_string_lossy().as_bytes());
    Ok(format!("{:x}", hash.finalize())[..32].into())
}
async fn title_context(socket: &mut TcpStream, meta: &pkg_meta::PkgMeta, title: &str, icon_url: &str) -> Result<(), String> {
    let name: String = title.chars().filter(|c| !c.is_control()).scan(0usize, |bytes, c| { *bytes += c.len_utf8(); (*bytes < 210).then_some(c) }).collect();
    let mut body = Vec::new();
    for value in [meta.title_id.as_str(), name.as_str(), icon_url] { body.extend_from_slice(value.as_bytes()); body.push(0); }
    let (code, reply) = frame(socket, 0x57, &body).await.map_err(|e| error_text(&e))?;
    if code != 1 { return Err(error_text(&String::from_utf8_lossy(&reply))); }
    Ok(())
}
async fn control_install(endpoint: &ReceiverEndpoint, cmd: u8, cid: &str) -> Result<(), String> {
    let mut socket = connect_receiver(endpoint, "install control").await.map_err(|e| error_text(&e))?;
    let (code, reply) = frame(&mut socket, cmd, &endpoint.path_body(cid)).await.map_err(|e| error_text(&e))?;
    if code != 1 {
        let error = serde_json::from_slice::<Value>(&reply).ok().and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&reply).into_owned());
        return Err(error_text(&error));
    }
    Ok(())
}

async fn monitor(context: &Context<'_>, endpoint: &ReceiverEndpoint, cid: &str, bgft: bool, timing: Timing) -> Result<(), Failure> {
    let mut changed = Instant::now();
    let mut previous = None;
    let mut applied_pause = false;
    let mut console_owned = !bgft;
    loop {
        if *context.cancel.borrow() && !console_owned {
            match control_install(endpoint, 0x5B, cid).await {
                Ok(()) => return Err("cancelled".to_string().into()),
                Err(error) if error.to_ascii_lowercase().contains("not cancellable") => {
                    // BGFT can enter installation between the last poll and Cancel.
                    console_owned = true;
                    context.report(Progress { stage:"installing".into(), message:"Installation is managed by the PS4 now".into(), ..Default::default() });
                },
                Err(error) => return Err(error.into()),
            }
        }
        if bgft && !console_owned && context.paused() != applied_pause {
            applied_pause = context.paused();
            control_install(endpoint, if applied_pause { 0x5C } else { 0x5D }, cid).await?;
            changed = Instant::now();
        }
        // Use a fresh connection after an interrupted poll; a stalled control socket
        // must not revoke the URL while BGFT can still be pulling from it.
        let poll = async {
            let mut socket = connect_receiver(endpoint, "install status").await?;
            frame(&mut socket, 0x51, &endpoint.path_body(cid)).await
        };
        let mut cancelled = context.cancel.clone();
        let status = tokio::select! {
            status = poll => Some(status),
            _ = cancelled.changed(), if !console_owned => None,
        };
        if status.is_none() { continue; }
        if let Some(Ok((code, body))) = status.filter(|reply| reply.as_ref().is_ok_and(|(code, body)| {
            *code == 2 || serde_json::from_slice::<Value>(body).is_ok()
        })) {
            let response = serde_json::from_slice::<Value>(&body).ok();
            let unconfirmed = code == 3 && response.as_ref().is_some_and(|value|
                value["state"] == "unconfirmed" && value["status"] == "awaiting_confirmation");
            let parsed = if unconfirmed { response.unwrap() } else { reply_json(code, &body)? };
            let status = parsed["status"].as_str().or_else(|| parsed["state"].as_str()).unwrap_or("unknown");
            let downloaded = parsed["downloaded"].as_u64().unwrap_or(0);
            let total = parsed["total"].as_u64().unwrap_or(0);
            let current = (status.to_string(), downloaded, total, parsed["progress"].as_f64());
            if previous.as_ref() != Some(&current) { changed = Instant::now(); previous = Some(current); }
            if matches!(status, "installed" | "complete") || parsed["state"] == "complete" {
                if parsed["error"].as_str().unwrap_or("").is_empty() { return Ok(()); }
                return Err(error_text(parsed["error"].as_str().unwrap()).into());
            }
            console_owned = !bgft || status == "installing" || unconfirmed;
            let paused = status == "paused" || applied_pause;
            context.report(Progress { stage: if console_owned { "installing" } else { "uploading" }.into(),
                progress: if total > 0 && !console_owned { (downloaded as f64 / total as f64).min(0.99) }
                    else { (parsed["progress"].as_f64().unwrap_or(0.) / 100.).clamp(0., 0.99) },
                bytes_done: downloaded, bytes_total: total, paused,
                message: if unconfirmed { "Waiting for PS4 installation confirmation" } else if console_owned { "Installing on the PS4" } else if paused { "PS4 download paused" } else { "PS4 is downloading from this PC" }.into(),
                ..Default::default() });
            if unconfirmed { return Err(monitoring_ended(parsed["error"].as_str().unwrap_or(""))); }
        }
        if changed.elapsed() >= timing.idle { return Err(MONITORING_ENDED.to_string().into()); }
        let mut cancel = context.cancel.clone();
        tokio::select! { _ = sleep(timing.poll) => {}, _ = cancel.changed(), if !console_owned => {} }
    }
}

async fn submit(socket: &mut TcpStream, cmd: u8, body: &[u8], expected_cid: &str) -> Result<String, Failure> {
    let (code, body) = frame(socket, cmd, body).await.map_err(|e| error_text(&e))?;
    let value = reply_json(code, &body).map_err(|mut failure| {
        failure.registration_rejected = cmd == 0x59 && rejected_before_registration(code, &body);
        failure
    })?;
    let cid = accepted_submission(&value).map_err(|e| error_text(&e))?;
    if !cid.is_empty() && cid != expected_cid { return Err(error_text("Receiver acknowledged a different content ID").into()); }
    Ok(if cid.is_empty() { expected_cid.into() } else { cid })
}
fn rejected_before_registration(code: u8, body: &[u8]) -> bool {
    if code != 2 { return false; }
    let Ok(value) = serde_json::from_slice::<Value>(body.strip_suffix(&[0]).unwrap_or(body)) else { return false; };
    let no_task = value.get("task_id").is_none() || value["task_id"].as_i64() == Some(0);
    let error = value["error"].as_str().unwrap_or("").trim().trim_start_matches("PS4:").trim();
    no_task && (matches!(value["stage"].as_str(), Some("validate" | "privileges" | "busy"))
        || error.starts_with("Another download for this content"))
}
async fn install_url(context: &Context<'_>, settings: &Settings, endpoint: &ReceiverEndpoint, path: &Path,
    meta: &pkg_meta::PkgMeta, title: &str, icon: Option<Vec<u8>>, local_ip: IpAddr, timing: Timing) -> Result<(), Failure> {
    let token = package_token(context.job, path)?;
    // A retry may already have an unconfirmed BGFT owner for this same token.
    let mut sent = pkg_server::activity(&token).is_some();
    let mut monitoring = false;
    pkg_server::ensure_started(settings.ps4_serve_port).await.map_err(|error| if sent { monitoring_ended(&error).message } else { error })?;
    let served = pkg_server::register(&token, path, icon, local_ip, settings.ps4_serve_port)
        .map_err(|error| if sent { monitoring_ended(&error).message } else { error })?;
    let result = async {
        context.checkpoint().await?;
        let mut socket = connect_receiver(endpoint, "URL install").await.map_err(|e| error_text(&e))?;
        title_context(&mut socket, meta, title, served.icon_url.as_deref().unwrap_or("")).await?;
        let mut body = serde_json::to_vec(&json!({"url":served.manifest_url,"content_id":meta.content_id,"kind":meta.kind,
            "title":title,"title_id":meta.title_id,"icon_url":served.icon_url.as_deref().unwrap_or(""),"size":meta.file_size,
            "declared_size":meta.original_size,"content_type":meta.content_type,"digest":meta.digest_hex,"header_sha256":meta.header_sha256})).map_err(redact)?;
        body.push(0);
        context.report(Progress { stage: "uploading".into(), bytes_total: meta.file_size, message: "PS4 is downloading from this PC".into(), ..Default::default() });
        // Treat even a partial write as possible submission: a failed reply cannot
        // establish whether BGFT registered a task and started fetching the URL.
        sent = true;
        let cid = submit(&mut socket, 0x59, &body, &meta.content_id).await?;
        monitoring = true;
        monitor(context, endpoint, &cid, true, timing).await
    }.await;
    match result {
        Ok(()) => { pkg_server::unregister(&token); Ok(()) },
        Err(error) if !sent || error.registration_rejected || (monitoring && error.message == "cancelled") => {
            pkg_server::unregister(&token); Err(error)
        },
        Err(error) => Err(monitoring_ended(&error.message)),
    }
}
async fn install_dlc(context: &Context<'_>, settings: &Settings, endpoint: &ReceiverEndpoint, path: &Path,
    meta: &pkg_meta::PkgMeta, title: &str, timing: Timing) -> Result<(), Failure> {
    create_remote_dir(endpoint, endpoint.pkg_dir).await.map_err(|e| error_text(&e))?;
    let remote = remote(endpoint, Some(&meta.title_id));
    if let Some(app) = context.app {
        send_file(app, settings, endpoint, path, &remote, context.job, context.cancel, "Uploading DLC PKG to PS4", 0, meta.file_size, None).await.map_err(|e| if e == "cancelled" { e } else { error_text(&e) })?;
        begin_console_stage(app, context.job, context.cancel, "submitting").await?;
    } else {
        send_file_once(None, settings, endpoint, path, &remote, context.job, context.cancel, "Uploading DLC PKG to PS4", 0, meta.file_size, None).await?;
    }
    context.checkpoint().await?;
    let mut socket = connect_receiver(endpoint, "DLC install").await.map_err(|e| error_text(&e))?;
    title_context(&mut socket, meta, title, "").await?;
    let cid = submit(&mut socket, 0x50, &endpoint.path_body(&remote), &meta.content_id).await?;
    context.report(Progress { stage: "installing".into(), message: "Installing DLC on the PS4".into(), ..Default::default() });
    monitor(context, endpoint, &cid, false, timing).await
}

async fn deliver_packages(context: &Context<'_>, settings: &Settings, pkgs: &[PathBuf], timing: Timing) -> Result<(), String> {
    if pkgs.is_empty() { return Err("PS4: No PKG files to install".into()); }
    let mut packages = Vec::new();
    for path in pkgs {
        ps4_inbox::validate_pkg(path).map_err(|e| error_text(&e))?;
        let meta = pkg_meta::read(path).map_err(|e| error_text(&e))?;
        if !matches!(meta.kind.as_str(), "base" | "update" | "dlc" | "theme") { return Err("PS4: Could not determine the PKG type from its metadata".into()); }
        packages.push((path, meta));
    }
    packages.sort_by_key(|(_, meta)| rank(&meta.kind));
    let endpoint = ReceiverEndpoint::ps4(settings);
    for (path, meta) in packages {
        let _slot = delivery_slot(context, &meta).await?;
        context.checkpoint().await?;
        let mut socket = checked_connection(&endpoint).await?;
        let local_ip = socket.local_addr().map_err(|_| "PS4: Could not determine the PC address for the receiver route".to_string())?.ip();
        let (code, body) = frame(&mut socket, 0x56, &[]).await.map_err(|e| error_text(&e))?;
        let preflight = reply_json(code, &body).map_err(|e| e.message)?;
        let free = preflight["free"].as_u64();
        context.report(Progress { stage: "uploading".into(), title_id: meta.title_id.clone(),
            message: free.map(|n| format!("PS4 ready · {:.2} GiB free", n as f64 / 1_073_741_824.)).unwrap_or_else(|| "PS4 installer ready".into()), ..Default::default() });
        let row = context.app.and_then(|app| app.state::<AppState>().jobs.lock().unwrap().get(context.job).cloned()).unwrap_or_default();
        let title = if row.title.is_empty() { meta.title.as_deref().unwrap_or(&meta.title_id) } else { &row.title };
        let icon = receiver_notifications::artwork(row.icon.as_deref(), meta.icon0.as_deref()).await;
        if let Some(icon) = icon.as_ref() {
            let progress = Progress { job_id: context.job.into(), target: "ps4".into(), title_id: meta.title_id.clone(), title: title.into(), stage: "uploading".into(), ..row.clone() };
            // Seed the payload's cache before its install-start toast; later events use these same bytes.
            let _ = receiver_notifications::prime_ps4(&endpoint, progress, icon.clone()).await;
        }
        let result = if meta.kind == "dlc" { install_dlc(context, settings, &endpoint, path, &meta, title, timing).await }
            else { install_url(context, settings, &endpoint, path, &meta, title, icon.clone(), local_ip, timing).await };
        match result {
            Err(failure) if meta.kind == "dlc" && failure.code == Some(ADDCONT_BROKEN) => {
                install_url(context, settings, &endpoint, path, &meta, title, icon, local_ip, timing).await.map_err(|e| e.message)?;
            },
            Err(failure) => return Err(failure.message),
            Ok(()) => {},
        }
    }
    Ok(())
}

pub(super) async fn deliver(app: &AppHandle, settings: &Settings, job: &str, pkgs: Vec<PathBuf>, cancel: &watch::Receiver<bool>) -> Result<(), String> {
    let context = Context { app: Some(app), job, cancel, #[cfg(test)] paused: None, #[cfg(test)] events: None };
    deliver_packages(&context, settings, &pkgs, Timing::default()).await?;
    let mut message = "Installed on the PS4".to_string();
    if let Some(record) = job_store::record(app, job) {
        if !settings.keep_archives {
            let inputs = match &record.checkpoint { Some(job_store::Checkpoint::Archive { inputs, .. } | job_store::Checkpoint::Extracted { inputs, .. }) => inputs.clone(), _ => vec![] };
            if archives::remove_consumed_inputs(&inputs, &pkgs).is_err() { message.push_str(". Local archives were kept because cleanup could not finish."); }
        }
        if !settings.keep_packages {
            for path in &pkgs {
                if job_store::cleanup_installed_package(app, job, path).is_err() { message.push_str(". A local package was kept because cleanup could not finish."); }
            }
        }
    }
    context.report(Progress { stage: "complete".into(), progress: 1., message, ..Default::default() });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use tokio::net::TcpListener;

    const CID: &str = "UP0000-CUSA12345_00-ABCDEFGHIJKLMNOP";
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    fn status(name: &str, bytes: u64) -> Value {
        json!({"api_code":0,"status_api_code":0,"auth_restore_code":0,"state":if name == "installed" { "complete" } else { "installing" },
            "content_id":CID,"status":name,"progress":if name == "installed" { 100 } else { 30 },"downloaded":bytes,"total":8192,"error_code":0,"error":""})
    }
    fn test_sfo() -> Vec<u8> {
        let fields = [("CATEGORY", "gd"), ("TITLE", "Test Game"), ("APP_VER", "1.03")];
        let mut keys = Vec::new();
        let mut values = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in fields {
            let key_offset = keys.len() as u16;
            keys.extend_from_slice(key.as_bytes()); keys.push(0);
            let value_offset = values.len() as u32;
            let length = value.len() as u32 + 1;
            values.extend_from_slice(value.as_bytes()); values.push(0);
            entries.extend_from_slice(&key_offset.to_le_bytes());
            entries.extend_from_slice(&0x0204u16.to_le_bytes());
            entries.extend_from_slice(&length.to_le_bytes()); entries.extend_from_slice(&length.to_le_bytes());
            entries.extend_from_slice(&value_offset.to_le_bytes());
        }
        let key_offset = 20u32 + entries.len() as u32;
        let value_offset = key_offset + keys.len() as u32;
        let mut sfo = Vec::new(); sfo.extend_from_slice(b"\0PSF");
        sfo.extend_from_slice(&0x00000101u32.to_le_bytes()); sfo.extend_from_slice(&key_offset.to_le_bytes());
        sfo.extend_from_slice(&value_offset.to_le_bytes()); sfo.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        sfo.extend_from_slice(&entries); sfo.extend_from_slice(&keys); sfo.extend_from_slice(&values); sfo
    }
    fn test_installed_metadata() -> Vec<u8> {
        let base = test_sfo(); let icon = BASE64.decode(PNG).unwrap(); let mut wire = Vec::new();
        for field in [&base[..], &[][..], &icon[..]] { wire.extend_from_slice(&(field.len() as u32).to_le_bytes()); wire.extend_from_slice(field); }
        wire
    }
    struct Peer {
        commands: Vec<u8>, bodies: Vec<(u8, Vec<u8>)>, urls: Vec<Value>, upload: Vec<u8>,
        statuses: VecDeque<Value>, version: String, bgft: String, fallback: Option<i64>, paused: bool,
        fetched_ranges: usize, url_reply: Option<(u8, Vec<u8>)>, url_disconnect: bool, cancel_rejected: bool,
        jailbroken: bool, diagnostics: Vec<String>, platform: String,
    }
    struct Fake { endpoint: ReceiverEndpoint, peer: Arc<Mutex<Peer>>, accept: tokio::task::JoinHandle<()> }
    impl Drop for Fake { fn drop(&mut self) { self.accept.abort(); } }
    impl Fake {
        async fn start(statuses: Vec<Value>, fallback: Option<i64>) -> Self {
            Self::listen(TcpListener::bind("127.0.0.1:0").await.unwrap(), statuses, fallback).await
        }
        async fn listen(listener: TcpListener, statuses: Vec<Value>, fallback: Option<i64>) -> Self {
            let port = listener.local_addr().unwrap().port();
            let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host: "127.0.0.1".into(), ps4_receiver_port: port, ..Settings::default() });
            let peer = Arc::new(Mutex::new(Peer { commands: vec![], bodies: vec![], urls: vec![], upload: vec![], statuses: statuses.into(),
                version: PS4_RECEIVER_VERSION.into(), bgft: "ready".into(), fallback, paused: false, fetched_ranges: 0,
                url_reply: None, url_disconnect: false, cancel_rejected: false, jailbroken: true, diagnostics: vec![], platform: "ps4".into() }));
            let shared = peer.clone();
            let accept = tokio::spawn(async move {
                loop {
                    let (socket, _) = listener.accept().await.unwrap();
                    tokio::spawn(Self::serve(socket, shared.clone()));
                }
            });
            Self { endpoint, peer, accept }
        }
        async fn serve(mut socket: TcpStream, shared: Arc<Mutex<Peer>>) {
            let mut offset = 0usize;
            loop {
                let mut header = [0; 5];
                if socket.read_exact(&mut header).await.is_err() { break; }
                let cmd = header[0];
                let mut body = vec![0; u32::from_le_bytes(header[1..].try_into().unwrap()) as usize];
                socket.read_exact(&mut body).await.unwrap();
                {
                    let mut peer = shared.lock().unwrap(); peer.commands.push(cmd); peer.bodies.push((cmd, body.clone()));
                }
                if cmd == 0x59 {
                    assert_eq!(body.last(), Some(&0));
                    let value: Value = serde_json::from_slice(&body[..body.len()-1]).unwrap();
                    assert_eq!(value["content_id"], CID);
                    let http = Client::builder().no_proxy().build().unwrap();
                    let response = http.get(value["url"].as_str().unwrap()).send().await.unwrap();
                    assert!(response.status().is_success());
                    let manifest: Value = response.json().await.unwrap();
                    assert_eq!(manifest["originalFileSize"], 8192);
                    assert_eq!(manifest["pieces"][0]["fileSize"], 8192);
                    assert_eq!(value["declared_size"], manifest["originalFileSize"]);
                    assert_eq!(value["digest"], manifest["packageDigest"]);
                    assert!(value["content_type"].as_u64().is_some());
                    let header = http.get(manifest["pieces"][0]["url"].as_str().unwrap()).header("Range", "bytes=0-4095").send().await.unwrap().bytes().await.unwrap();
                    assert_eq!(value["header_sha256"], format!("{:x}", Sha256::digest(&header)));
                    let response = http.get(manifest["pieces"][0]["url"].as_str().unwrap()).header("Range", "bytes=0-63").send().await.unwrap();
                    assert_eq!(response.status(), 206);
                    assert_eq!(response.headers()["Content-Range"], "bytes 0-63/8192");
                    let range = response.bytes().await.unwrap(); assert_eq!(range.len(), 64); assert_eq!(&range[..4], b"\x7fCNT");
                    if let Some(url) = value["icon_url"].as_str().filter(|url| !url.is_empty()) {
                        let png = http.get(url).send().await.unwrap().bytes().await.unwrap();
                        assert_eq!(&png[..], &BASE64.decode(PNG).unwrap());
                    }
                    let mut peer = shared.lock().unwrap(); peer.urls.push(value); peer.fetched_ranges += 1;
                    if peer.url_disconnect { return; }
                }
                let (code, reply) = {
                    let mut peer = shared.lock().unwrap();
                    match cmd {
                        0x01 => (1, b"SSPI".to_vec()),
                        0x53 => (3, json!({"version":peer.version,"platform":peer.platform,"writable":true,"jailbroken":peer.jailbroken,"diagnostics":peer.diagnostics,"bgft":peer.bgft,"appinst":"ready",
                            "capabilities":["pkg-preflight","pkg-install","url-install","parallel-upload","verify","title-context","progress-notifications","install-control","stop","ps4","installed-library-v1"]}).to_string().into_bytes()),
                        0x5e => (3, br#"{"titles":["CUSA12345"],"complete":true,"truncated":false,"errors":[]}"#.to_vec()),
                        0x5f => { assert_eq!(body,b"CUSA12345\0"); (3,test_installed_metadata()) },
                        0x56 => (1, b"OK {\"bgft\":\"ready\",\"appinst\":\"ready\",\"free\":1099511627776}".to_vec()),
                        0x04 => { assert_eq!(body, b"/user/data/sspi-receiver/upload\0"); (1, b"OK".to_vec()) },
                        0x10 => {
                            let nul = body.iter().position(|b| *b == 0).unwrap();
                            assert!(body.starts_with(b"/user/data/sspi-receiver/upload/upload_CUSA12345_"));
                            assert_eq!(body.len(), nul + 25);
                            let size = u64::from_le_bytes(body[nul+1..nul+9].try_into().unwrap()) as usize;
                            offset = u64::from_le_bytes(body[nul+9..nul+17].try_into().unwrap()) as usize;
                            peer.upload.resize(size, 0); (4, b"READY".to_vec())
                        },
                        0x11 => { peer.upload[offset..offset+body.len()].copy_from_slice(&body); offset += body.len(); (1, b"OK".to_vec()) },
                        0x12 => { assert!(body.is_empty()); (1, b"OK".to_vec()) },
                        0x55 => { assert_eq!(body.last(), Some(&0)); (1, format!("OK {{\"size\":{}}}", peer.upload.len()).into_bytes()) },
                        0x57 => {
                            let parts: Vec<_> = body.split(|b| *b == 0).collect();
                            assert_eq!(parts.len(), 4); assert_eq!(parts[0], b"CUSA12345"); (1, b"OK".to_vec())
                        },
                        0x58 => {
                            assert_eq!(&body[1..11], b"CUSA12345\0");
                            let count = u32::from_le_bytes(body[11..15].try_into().unwrap()) as usize;
                            assert_eq!(&body[15..15+count], &BASE64.decode(PNG).unwrap()); (1, b"OK".to_vec())
                        },
                        0x50 if peer.fallback.is_some() => (2, json!({"api_code":peer.fallback,"install_api_code":peer.fallback,"auth_restore_code":0,
                            "state":"failed","content_id":CID,"path":"/user/data/sspi-receiver/upload/test.pkg","error":"PS4: ADDCONT_BROKEN"}).to_string().into_bytes()),
                        0x59 if peer.url_reply.is_some() => peer.url_reply.clone().unwrap(),
                        0x50 | 0x59 => (1, json!({"api_code":0,"install_api_code":0,"auth_restore_code":0,"state":"submitted","content_id":CID,"path":"","error":"","task_id":7}).to_string().into_bytes()),
                        0x51 => {
                            assert_eq!(body, format!("{CID}\0").as_bytes());
                            let status = peer.statuses.pop_front().unwrap_or_else(|| status(if peer.paused { "paused" } else { "downloading" }, 1024));
                            (if status["state"] == "failed" { 2 } else { 3 }, status.to_string().into_bytes())
                        },
                        0x5B if peer.cancel_rejected => (2, br#"{"error":"BGFT did not confirm cancellation"}"#.to_vec()),
                        0x5B | 0x5C | 0x5D => { assert_eq!(body, format!("{CID}\0").as_bytes()); peer.paused = cmd == 0x5C; (1, b"OK".to_vec()) },
                        _ => panic!("Unexpected command {cmd:02X}"),
                    }
                };
                let mut response = vec![code]; response.extend_from_slice(&(reply.len() as u32).to_le_bytes()); response.extend_from_slice(&reply);
                if socket.write_all(&response).await.is_err() { break; }
            }
        }
        async fn seen(&self, cmd: u8) {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !self.peer.lock().unwrap().commands.contains(&cmd) { sleep(Duration::from_millis(5)).await; }
            }).await.unwrap();
        }
        fn settings(&self, serve_port: u16) -> Settings {
            Settings { ps4_host: self.endpoint.host.clone(), ps4_receiver_port: self.endpoint.port, ps4_serve_port: serve_port, ..Settings::default() }
        }
    }
    fn package(category: &str) -> PathBuf {
        // "theme" is add-on content whose header carries IRO tag 2.
        let theme = category == "theme";
        let category = if theme { "ac" } else { category };
        let mut bytes = vec![0; 8192];
        if theme { bytes[0x98..0x9c].copy_from_slice(&2u32.to_be_bytes()); }
        bytes[..4].copy_from_slice(b"\x7fCNT"); bytes[0x40..0x64].copy_from_slice(CID.as_bytes());
        bytes[0x10..0x14].copy_from_slice(&2u32.to_be_bytes()); bytes[0x18..0x1c].copy_from_slice(&0x1000u32.to_be_bytes());
        bytes[0x20..0x28].copy_from_slice(&0x1000u64.to_be_bytes()); bytes[0x28..0x30].copy_from_slice(&0x1000u64.to_be_bytes());
        bytes[0x430..0x438].copy_from_slice(&8192u64.to_be_bytes());
        bytes[0x74..0x78].copy_from_slice(&(if category == "ac" { 0x1bu32 } else { 0x1a }).to_be_bytes());
        let mut sfo = vec![0; 48]; sfo[..4].copy_from_slice(b"\0PSF");
        sfo[8..12].copy_from_slice(&36u32.to_le_bytes()); sfo[12..16].copy_from_slice(&45u32.to_le_bytes()); sfo[16..20].copy_from_slice(&1u32.to_le_bytes());
        sfo[22..24].copy_from_slice(&0x204u16.to_le_bytes()); sfo[24..28].copy_from_slice(&3u32.to_le_bytes()); sfo[28..32].copy_from_slice(&3u32.to_le_bytes());
        sfo[36..45].copy_from_slice(b"CATEGORY\0"); sfo[45..47].copy_from_slice(category.as_bytes());
        let icon = BASE64.decode(PNG).unwrap();
        for (index, id, offset, data) in [(0, 0x1000u32, 0x1100u32, &sfo), (1, 0x1200u32, 0x1400u32, &icon)] {
            let entry = 0x1000 + index * 32;
            bytes[entry..entry+4].copy_from_slice(&id.to_be_bytes()); bytes[entry+16..entry+20].copy_from_slice(&offset.to_be_bytes());
            bytes[entry+20..entry+24].copy_from_slice(&(data.len() as u32).to_be_bytes());
            bytes[offset as usize..offset as usize+data.len()].copy_from_slice(data);
        }
        let digest = Sha256::digest(&bytes[..0xfe0]); bytes[0xfe0..0x1000].copy_from_slice(&digest);
        let path = test_output_root().join(format!("ps4-receiver-{}.pkg", Uuid::new_v4())); std::fs::write(&path, bytes).unwrap(); path
    }
    fn timing() -> Timing { Timing { poll: Duration::from_millis(10), idle: Duration::from_secs(3) } }
    fn context<'a>(job: &'a str, cancel: &'a watch::Receiver<bool>, events: &'a Mutex<Vec<Progress>>) -> Context<'a> {
        Context { app: None, job, cancel, paused: None, events: Some(events) }
    }

    #[tokio::test]
    async fn installed_library_uses_bounded_receiver_frames_and_parses_base_metadata() {
        let fake = Fake::start(vec![], None).await;
        let snapshot = list_ps4_library(fake.endpoint.host.clone(), fake.endpoint.port).await.unwrap();
        assert!(snapshot.complete && !snapshot.truncated && snapshot.errors.is_empty());
        assert_eq!(snapshot.entries.len(), 1);
        let title = &snapshot.entries[0];
        assert_eq!(title.title_id, "CUSA12345");
        assert_eq!(title.name, "Test Game");
        assert_eq!(title.version.as_deref(), Some("1.03"));
        assert_eq!(title.base_version.as_deref(), Some("1.03"));
        assert_eq!(title.update_version, None);
        let icon = title.icon.as_deref().unwrap().strip_prefix("data:image/jpeg;base64,").unwrap();
        let decoded = BASE64.decode(icon).unwrap();
        assert!(image::load_from_memory(&decoded).is_ok());
        let peer = fake.peer.lock().unwrap();
        assert!(peer.bodies.iter().any(|(command, body)| *command == 0x5e && body.is_empty()));
        assert!(peer.bodies.iter().any(|(command, body)| *command == 0x5f && body == b"CUSA12345\0"));
    }

    #[tokio::test]
    #[ignore = "requires an explicitly configured live PS4 receiver"]
    async fn live_installed_library_snapshot_is_complete_and_icons_decode() {
        let host = std::env::var("SSPI_TEST_PS4_HOST").expect("set SSPI_TEST_PS4_HOST to run the read-only hardware smoke test");
        let port = std::env::var("SSPI_TEST_PS4_PORT").ok().and_then(|value| value.parse().ok()).unwrap_or(9114);
        let snapshot = list_ps4_library(host, port).await.unwrap();
        assert!(snapshot.complete, "inventory errors: {:?}", snapshot.errors);
        assert!(!snapshot.entries.is_empty());
        assert!(snapshot.entries.iter().all(|entry| valid_installed_title_id(&entry.title_id) && !entry.name.trim().is_empty()));
        assert!(snapshot.entries.iter().all(|entry| entry.icon.as_deref().is_some_and(|icon| {
            icon.strip_prefix("data:image/jpeg;base64,").and_then(|data| BASE64.decode(data).ok()).is_some_and(|bytes| image::load_from_memory(&bytes).is_ok())
        })));
        std::fs::write(crate::test_output_root().join("ps4-library-live-rust.json"), serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn themes_install_through_the_url_route_as_themes() {
        let port = pkg_server_test_port();
        let fake = Fake::start(vec![status("downloading", 4096), status("installing", 8192), status("installed", 8192)], None).await;
        let path = package("theme"); let job = Uuid::new_v4().to_string();
        let (_tx, rx) = watch::channel(false); let events = Mutex::new(vec![]);
        deliver_packages(&context(&job, &rx, &events), &fake.settings(port), &[path.clone()], timing()).await.unwrap();
        let peer = fake.peer.lock().unwrap();
        assert!(peer.commands.contains(&0x59) && !peer.commands.contains(&0x10) && !peer.commands.contains(&0x50));
        assert_eq!(peer.urls[0]["kind"], "theme");
        assert_eq!(peer.urls[0]["content_type"], 0x1b);
        assert_eq!(peer.fetched_ranges, 1);
        drop(peer); std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn base_url_dlc_upload_and_signed_unsigned_dlc_fallbacks() {
        let port = pkg_server_test_port();
        for (category, fallback) in [("gd", None), ("gp", None), ("ac", None), ("ac", Some(ADDCONT_BROKEN as i32 as i64)), ("ac", Some(ADDCONT_BROKEN as i64))] {
            let fake = Fake::start(vec![status("downloading", 4096), status("installing", 8192), status("installed", 8192)], fallback).await;
            let path = package(category); let job = Uuid::new_v4().to_string();
            let (_tx, rx) = watch::channel(false); let events = Mutex::new(vec![]);
            deliver_packages(&context(&job, &rx, &events), &fake.settings(port), &[path.clone()], timing()).await.unwrap();
            let peer = fake.peer.lock().unwrap();
            let url = category != "ac" || fallback.is_some();
            assert_eq!(peer.commands.contains(&0x59), url); assert_eq!(peer.fetched_ranges, usize::from(url));
            if category == "ac" {
                for cmd in [0x04, 0x10, 0x11, 0x12, 0x55, 0x50] { assert!(peer.commands.contains(&cmd)); }
                assert_eq!(peer.upload, std::fs::read(&path).unwrap());
                assert!(peer.commands.iter().position(|c| *c == 0x55).unwrap() < peer.commands.iter().position(|c| *c == 0x50).unwrap());
                if url { assert_eq!(peer.urls[0]["kind"], "dlc"); }
            } else { assert!(!peer.commands.contains(&0x10)); }
            assert!(events.lock().unwrap().iter().any(|p| p.stage == "installing"));
            assert!(pkg_server::activity(&package_token(&job, &path).unwrap()).is_none());
            drop(peer); std::fs::remove_file(&path).unwrap();
        }
        url_token_lifetime(port).await;
    }

    async fn url_token_lifetime(port: u16) {
        let path = package("gd"); let meta = pkg_meta::read(&path).unwrap(); let job = Uuid::new_v4().to_string();
        let fake = Fake::start(vec![], None).await;
        let (_tx, rx) = watch::channel(false); let events = Mutex::new(vec![]); let context = context(&job, &rx, &events);
        let settings = fake.settings(port); let ip = "127.0.0.1".parse().unwrap(); let token = package_token(&job, &path).unwrap();
        let result = install_url(&context, &settings, &fake.endpoint, &path, &meta, "Game", None, ip,
            Timing { poll:Duration::from_millis(5), idle:Duration::from_millis(30) }).await;
        assert_eq!(result.unwrap_err().message, MONITORING_ENDED); assert!(pkg_server::activity(&token).is_some());
        let url = fake.peer.lock().unwrap().urls[0]["url"].as_str().unwrap().to_string();
        assert!(Client::builder().no_proxy().build().unwrap().get(&url).send().await.unwrap().status().is_success());
        fake.peer.lock().unwrap().statuses.push_back(status("installed", 8192));
        install_url(&context, &settings, &fake.endpoint, &path, &meta, "Game", None, ip, timing()).await.unwrap();
        assert_eq!(fake.peer.lock().unwrap().urls[1]["url"], url); assert!(pkg_server::activity(&token).is_none());
        fake.peer.lock().unwrap().statuses.push_back(json!({"api_code":0,"status_api_code":0,"auth_restore_code":0,"state":"failed",
            "status":"failed","content_id":CID,"progress":0,"downloaded":1,"total":8192,"error_code":-1,"error":"PS4: Disk full"}));
        let error = install_url(&context, &settings, &fake.endpoint, &path, &meta, "Game", None, ip, timing()).await.unwrap_err();
        assert!(error.message.starts_with(MONITORING_ENDED)); assert!(error.message.contains("Disk full")); assert!(pkg_server::activity(&token).is_some());
        let (cancel_tx, cancel_rx) = watch::channel(false); let context = Context { cancel:&cancel_rx, ..context };
        fake.peer.lock().unwrap().commands.clear();
        let run = install_url(&context, &settings, &fake.endpoint, &path, &meta, "Game", None, ip, timing());
        let cancel = async { fake.seen(0x59).await; cancel_tx.send(true).unwrap(); };
        let (result, _) = tokio::join!(run, cancel); assert_eq!(result.unwrap_err().message, "cancelled");
        assert!(pkg_server::activity(&token).is_none()); assert!(fake.peer.lock().unwrap().commands.contains(&0x5B));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn ambiguous_url_submission_and_failed_cancel_keep_the_source_served() {
        let port = pkg_server_test_port(); let ip = "127.0.0.1".parse().unwrap();
        for mode in ["disconnect", "malformed", "payload-error", "registered-validation-error", "cancel-rejected"] {
            let fake = Fake::start(vec![], None).await;
            {
                let mut peer = fake.peer.lock().unwrap();
                match mode {
                    "disconnect" => peer.url_disconnect = true,
                    "malformed" => peer.url_reply = Some((1, b"not JSON".to_vec())),
                    "payload-error" => peer.url_reply = Some((2, json!({"state":"failed","stage":"register","task_id":7,"error":"Installer timed out; ownership unconfirmed"}).to_string().into_bytes())),
                    "registered-validation-error" => peer.url_reply = Some((2, json!({"state":"failed","stage":"validate","task_id":7,"error":"Registration outcome uncertain"}).to_string().into_bytes())),
                    "cancel-rejected" => peer.cancel_rejected = true,
                    _ => unreachable!(),
                }
            }
            let path = package("gd"); let meta = pkg_meta::read(&path).unwrap(); let job = Uuid::new_v4().to_string();
            let token = package_token(&job, &path).unwrap(); let (tx, rx) = watch::channel(false); let events = Mutex::new(vec![]);
            let context = context(&job, &rx, &events); let settings = fake.settings(port);
            let run = install_url(&context, &settings, &fake.endpoint, &path, &meta, "Game", None, ip, timing());
            let cancel = async { if mode == "cancel-rejected" { fake.seen(0x59).await; tx.send(true).unwrap(); } };
            let (result, _) = tokio::join!(run, cancel);
            let error = result.unwrap_err(); assert!(error.message.starts_with(MONITORING_ENDED)); assert_eq!(job_error_stage(&error.message), "monitoring-ended");
            assert!(pkg_server::is_served(&path)); assert!(pkg_server::activity(&token).is_some());
            let url = fake.peer.lock().unwrap().urls[0]["url"].as_str().unwrap().to_string();
            assert!(Client::builder().no_proxy().build().unwrap().get(url).send().await.unwrap().status().is_success());
            // A cancelled Retry must not unregister an earlier unconfirmed task.
            tx.send(true).unwrap();
            let error = install_url(&context, &settings, &fake.endpoint, &path, &meta, "Game", None, ip, timing()).await.unwrap_err();
            assert!(error.message.starts_with(MONITORING_ENDED)); assert_eq!(job_error_stage(&error.message), "monitoring-ended");
            assert!(pkg_server::activity(&token).is_some());
            pkg_server::unregister(&token); std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn explicit_pre_registration_rejections_release_the_url() {
        let port = pkg_server_test_port();
        for value in [
            json!({"state":"failed","stage":"validate","task_id":0,"error":"Invalid PKG"}),
            json!({"state":"failed","stage":"busy","error":"Receiver busy"}),
            json!({"state":"failed","stage":"privileges","task_id":0,"error":"uid=1000"}),
            json!({"state":"failed","stage":"register","error":"Another download for this content is already in the PS4 download queue. Remove it on the console."}),
        ] {
            let fake = Fake::start(vec![], None).await;
            fake.peer.lock().unwrap().url_reply = Some((2, value.to_string().into_bytes()));
            let path = package("gd"); let meta = pkg_meta::read(&path).unwrap(); let job = Uuid::new_v4().to_string();
            let (_tx, rx) = watch::channel(false); let events = Mutex::new(vec![]);
            let error = install_url(&context(&job, &rx, &events), &fake.settings(port), &fake.endpoint, &path, &meta, "Game", None, "127.0.0.1".parse().unwrap(), timing()).await.unwrap_err();
            assert_eq!(job_error_stage(&error.message), "failed"); assert!(error.registration_rejected);
            assert!(!pkg_server::is_served(&path)); assert!(pkg_server::activity(&package_token(&job, &path).unwrap()).is_none());
            if value["stage"] == "privileges" { assert_eq!(error.message, "Receiver is running without system privileges (uid=1000). Reload it after enabling GoldHEN."); }
            else { assert!(error.message.contains(value["error"].as_str().unwrap())); }
            std::fs::remove_file(path).unwrap();
        }
        for (code, value) in [
            (1,json!({"stage":"busy","error":"busy"})),
            (2,json!({"stage":"validate","task_id":8,"error":"bad"})),
            (2,json!({"stage":"submit","task_id":0,"error":"timeout"})),
        ] { assert!(!rejected_before_registration(code, &serde_json::to_vec(&value).unwrap())); }
    }

    #[tokio::test]
    async fn advancing_install_percentage_prevents_idle_monitoring_timeout() {
        let mut statuses = vec![];
        for progress in [10, 20, 30, 40, 50] { let mut value = status("installing",8192); value["progress"] = json!(progress); statuses.push(value); }
        statuses.push(status("installed",8192));
        let fake = Fake::start(statuses, None).await;
        let (_tx, rx) = watch::channel(false); let events = Mutex::new(vec![]);
        monitor(&context("percentage", &rx, &events), &fake.endpoint, CID, true,
            Timing { poll:Duration::from_millis(20), idle:Duration::from_millis(30) }).await.unwrap();
        assert_eq!(events.lock().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn unprivileged_receiver_fails_test_and_delivery_readiness() {
        let fake = Fake::start(vec![], None).await;
        { let mut peer = fake.peer.lock().unwrap(); peer.jailbroken = false; peer.diagnostics = vec!["BGFT ready".into(), "uid=1000; libjbc unavailable".into(), "sandbox still active".into()]; }
        let expected = "Receiver is running without system privileges (uid=1000; libjbc unavailable). Reload it after enabling GoldHEN.";
        assert_eq!(ready(&fake.endpoint).await.unwrap_err(), expected);
        assert_eq!(test_ps4_receiver(fake.endpoint.host.clone(),fake.endpoint.port).await.unwrap_err(), expected);
        let value = json!({"state":"failed","stage":"privileges","error":"uid=1000; libjbc unavailable"});
        assert_eq!(reply_json(2, &serde_json::to_vec(&value).unwrap()).unwrap_err().message, expected);
    }

    #[tokio::test]
    async fn ps4_ping_socket_errors_do_not_add_the_ps5_hint() {
        let mut errors = vec![];
        for ps4 in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap(); let mut bytes = [0;5]; socket.read_exact(&mut bytes).await.unwrap(); assert_eq!(bytes,[1,0,0,0,0]);
            });
            let settings = Settings { ps4_host:"127.0.0.1".into(), ps5_host:"127.0.0.1".into(), ps4_receiver_port:port, ps5_port:port, ..Settings::default() };
            let endpoint = if ps4 { ReceiverEndpoint::ps4(&settings) } else { ReceiverEndpoint::ps5(&settings) };
            let expected = redact_delivery_error("connection reset; Bearer example",if ps4 { "ps4" } else { "ps5" });
            assert_eq!(endpoint.redact("connection reset; Bearer example"), expected);
            errors.push(ping(&endpoint).await.unwrap_err()); server.await.unwrap();
        }
        assert!(!errors[0].contains("PS5") && !errors[0].contains("Reload the ELF"));
        assert_eq!(errors[1],redact_delivery_error(&errors[0],"ps5"));
    }

    #[tokio::test]
    async fn pause_resume_and_cancel_are_sent_to_bgft() {
        let fake = Fake::start(vec![], None).await;
        let (cancel_tx, cancel_rx) = watch::channel(false); let (pause_tx, pause_rx) = watch::channel(false);
        let events = Mutex::new(vec![]);
        let mut context = context("controls", &cancel_rx, &events); context.paused = Some(&pause_rx);
        let monitor = monitor(&context, &fake.endpoint, CID, true, timing());
        let control = async {
            fake.seen(0x51).await; pause_tx.send(true).unwrap(); fake.seen(0x5C).await;
            pause_tx.send(false).unwrap(); fake.seen(0x5D).await; cancel_tx.send(true).unwrap();
        };
        let (result, _) = tokio::join!(monitor, control); assert_eq!(result.unwrap_err().message, "cancelled");
        fake.seen(0x5B).await;
        assert!(events.lock().unwrap().iter().any(|p| p.paused && p.stage == "uploading"));
    }

    #[tokio::test]
    async fn idle_monitoring_never_reports_installed_and_installing_is_console_owned() {
        let fake = Fake::start(vec![status("installing", 8192), status("installed", 8192)], None).await;
        let (_tx, rx) = watch::channel(true); let events = Mutex::new(vec![]);
        monitor(&context("console-owned", &rx, &events), &fake.endpoint, CID, false, timing()).await.unwrap();
        assert!(!fake.peer.lock().unwrap().commands.contains(&0x5B));
        let (_tx, rx) = watch::channel(false);
        let error = monitor(&context("idle", &rx, &events), &fake.endpoint, CID, true, Timing { poll: Duration::from_millis(5), idle: Duration::from_millis(30) }).await.unwrap_err();
        assert_eq!(error.message, MONITORING_ENDED); assert_eq!(job_error_stage(&error.message), "monitoring-ended");
        assert!(!events.lock().unwrap().iter().any(|p| p.stage == "complete"));
    }

    #[tokio::test]
    async fn unconfirmed_install_status_keeps_download_progress_and_exact_reason_for_retry() {
        for query_code in [0, -60] {
            let value = json!({"api_code":query_code,"status_api_code":query_code,"error_code":0,"state":"unconfirmed",
                "status":"awaiting_confirmation","content_id":CID,"progress":99,"downloaded":8192,"total":8192,
                "error":"Download complete; installed package confirmation is unavailable"});
            let fake = Fake::start(vec![value], None).await;
            let (_tx, rx) = watch::channel(false); let events = Mutex::new(vec![]);
            let failure = monitor(&context("unconfirmed", &rx, &events), &fake.endpoint, CID, true, timing()).await.unwrap_err();
            assert!(failure.message.starts_with(MONITORING_ENDED));
            assert!(failure.message.contains("Download complete; installed package confirmation is unavailable"));
            assert_eq!(job_error_stage(&failure.message), "monitoring-ended");
            let reports = events.lock().unwrap(); let last = reports.last().unwrap();
            assert_eq!(last.stage, "installing"); assert_eq!((last.bytes_done,last.bytes_total), (8192,8192));
            assert_eq!(last.message, "Waiting for PS4 installation confirmation");
            assert!(!reports.iter().any(|p| matches!(p.stage.as_str(), "complete" | "failed")));
        }
    }

    #[test]
    fn uncertain_errors_keep_details_without_repeated_wrapping_or_false_cancellation() {
        let failure = monitoring_ended("Progress query failed (0x8002003C)");
        assert!(failure.message.contains("Progress query failed (0x8002003C)"));
        assert_eq!(monitoring_ended(&failure.message).message, failure.message);
        assert_eq!(job_error_stage(&monitoring_ended("cancelled").message), "monitoring-ended");
        assert_eq!(job_error_stage("cancelled"), "cancelled");
    }

    #[tokio::test]
    async fn version_mismatch_and_get_config_diagnostics_are_specific() {
        let fake = Fake::start(vec![], None).await;
        fake.peer.lock().unwrap().version = "0.9.0".into();
        let error = ready(&fake.endpoint).await.unwrap_err(); assert!(error.contains("v0.9.0") && error.contains(&format!("v{PS4_RECEIVER_VERSION}")));
        fake.peer.lock().unwrap().version = PS4_RECEIVER_VERSION.into(); fake.peer.lock().unwrap().bgft = "unavailable:0x80020012".into();
        assert_eq!(ready(&fake.endpoint).await.unwrap_err(), "PS4: BGFT unavailable: 0x80020012");
        fake.peer.lock().unwrap().bgft = "ready".into();
        assert_eq!(test_ps4_receiver(fake.endpoint.host.clone(), fake.endpoint.port).await.unwrap(), format!("Receiver verified (v{PS4_RECEIVER_VERSION} · BGFT ready · AppInst ready)"));
    }

    fn loader_payload() -> Vec<u8> {
        let mut bytes = vec![0; 193];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&192u64.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
        bytes[96..104].copy_from_slice(&193u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&193u64.to_le_bytes());
        bytes[192] = 0xC3;
        bytes
    }

    #[test]
    fn loader_rejects_application_elf_and_malformed_payloads() {
        let payload = loader_payload();
        assert!(validate_payload(&payload).is_ok());
        for bytes in [b"placeholder".as_slice(), b"\x7fELF", &payload[..100]] {
            assert_eq!(validate_payload(bytes).unwrap_err(), INVALID_PAYLOAD);
        }
        for (offset, replacement) in [
            (4, vec![1]), // ELF32
            (18, 183u16.to_le_bytes().to_vec()), // ARM64
            (24, 200u64.to_le_bytes().to_vec()), // Entry outside the executable segment
            (32, u64::MAX.to_le_bytes().to_vec()), // Program table overflow
            (68, 4u32.to_le_bytes().to_vec()), // Non-executable entry
            (96, 194u64.to_le_bytes().to_vec()), // Segment extends beyond the file
            (120, 3u32.to_le_bytes().to_vec()), // PT_INTERP
            (120, 7u32.to_le_bytes().to_vec()), // PT_TLS
        ] {
            let mut invalid = payload.clone();
            invalid[offset..offset+replacement.len()].copy_from_slice(&replacement);
            assert_eq!(validate_payload(&invalid).unwrap_err(), INVALID_PAYLOAD);
        }
        let mut dynamic = payload;
        dynamic[120..124].copy_from_slice(&2u32.to_le_bytes());
        dynamic[128..136].copy_from_slice(&176u64.to_le_bytes());
        dynamic[152..160].copy_from_slice(&16u64.to_le_bytes());
        assert!(validate_payload(&dynamic).is_ok());
        dynamic[176..184].copy_from_slice(&1u64.to_le_bytes()); // DT_NEEDED
        assert_eq!(validate_payload(&dynamic).unwrap_err(), INVALID_PAYLOAD);
    }

    #[test]
    fn readiness_requires_positive_privilege_and_storage_diagnostics() {
        let endpoint = ReceiverEndpoint::ps4(&Settings::default());
        let good = json!({"version":PS4_RECEIVER_VERSION,"platform":"ps4","jailbroken":true,"writable":true,
            "bgft":"ready","appinst":"ready","capabilities":endpoint.required_capabilities});
        assert!(check_config(&endpoint, &good).is_ok());
        for key in ["jailbroken", "writable"] {
            for value in [Value::Null, json!(false), json!("true")] {
                let mut incomplete = good.clone(); incomplete[key] = value;
                assert!(check_config(&endpoint, &incomplete).is_err());
            }
        }
    }

    #[tokio::test]
    async fn load_ps4_receiver_sends_elf_as_the_only_loader_connection() {
        let loader = TcpListener::bind("127.0.0.1:0").await.unwrap(); let loader_port = loader.local_addr().unwrap().port();
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap(); let receiver_port = probe.local_addr().unwrap().port(); drop(probe);
        let payload = loader_payload(); let expected = payload.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = loader.accept().await.unwrap();
            let mut first = [0; 4]; socket.read_exact(&mut first).await.unwrap(); assert_eq!(&first, b"\x7fELF");
            let mut body = first.to_vec(); socket.read_to_end(&mut body).await.unwrap(); assert_eq!(body, expected);
            let fake = Fake::listen(TcpListener::bind(("127.0.0.1", receiver_port)).await.unwrap(), vec![], None).await;
            assert!(tokio::time::timeout(Duration::from_millis(100), loader.accept()).await.is_err());
            fake
        });
        let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host:"127.0.0.1".into(), ps4_receiver_port:receiver_port, ..Settings::default() });
        assert_eq!(load_payload(&endpoint, loader_port, &payload).await.unwrap(), format!("Receiver loaded (v{PS4_RECEIVER_VERSION})"));
        let fake = server.await.unwrap();
        assert_eq!(load_payload(&fake.endpoint, loader_port, &payload).await.unwrap(), format!("Receiver already running (v{PS4_RECEIVER_VERSION})"));
    }

    #[tokio::test]
    async fn loader_stops_a_receiver_with_safe_shutdown_before_sending_the_new_payload() {
        let old = TcpListener::bind("127.0.0.1:0").await.unwrap(); let receiver_port = old.local_addr().unwrap().port();
        let loader = TcpListener::bind("127.0.0.1:0").await.unwrap(); let loader_port = loader.local_addr().unwrap().port();
        let old_receiver = tokio::spawn(async move {
            let (mut socket, _) = old.accept().await.unwrap();
            for (cmd, code, reply) in [(0x01, 1, b"SSPI".as_slice()), (0x53, 3, br#"{"version":"1.0.1","platform":"ps4","capabilities":["stop"]}"#.as_slice()), (0x5A, 1, b"stopping".as_slice())] {
                let mut header = [0;5]; socket.read_exact(&mut header).await.unwrap(); assert_eq!(header, [cmd,0,0,0,0]);
                let mut bytes = vec![code]; bytes.extend_from_slice(&(reply.len() as u32).to_le_bytes()); bytes.extend_from_slice(reply);
                socket.write_all(&bytes).await.unwrap();
            }
        });
        let payload = loader_payload(); let expected = payload.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = loader.accept().await.unwrap();
            let mut body = vec![]; socket.read_to_end(&mut body).await.unwrap(); assert_eq!(body, expected);
            Fake::listen(TcpListener::bind(("127.0.0.1",receiver_port)).await.unwrap(),vec![],None).await
        });
        let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host:"127.0.0.1".into(), ps4_receiver_port:receiver_port, ..Settings::default() });
        assert_eq!(load_payload(&endpoint,loader_port,&payload).await.unwrap(),format!("Receiver loaded (v{PS4_RECEIVER_VERSION})"));
        let _receiver = server.await.unwrap(); old_receiver.await.unwrap();
    }

    #[tokio::test]
    async fn same_version_receiver_must_be_ready_and_foreign_receiver_is_never_stopped() {
        let fake = Fake::start(vec![], None).await;
        let payload = loader_payload();
        fake.peer.lock().unwrap().bgft = "unavailable:0x80020012".into();
        assert_eq!(load_payload(&fake.endpoint, 9090, &payload).await.unwrap_err(), "PS4: BGFT unavailable: 0x80020012");
        { let mut peer = fake.peer.lock().unwrap(); peer.bgft = "ready".into(); peer.jailbroken = false; }
        assert!(load_payload(&fake.endpoint, 9090, &payload).await.unwrap_err().contains("without system privileges"));
        for version in [PS4_RECEIVER_VERSION, "1.0.5"] {
            { let mut peer = fake.peer.lock().unwrap(); peer.jailbroken = true; peer.platform = "ps5".into(); peer.version = version.into(); }
            assert!(load_payload(&fake.endpoint, 9090, &payload).await.unwrap_err().contains("not a PS4 receiver"));
        }
        assert!(!fake.peer.lock().unwrap().commands.contains(&0x5A));
    }

    #[tokio::test]
    async fn loader_never_stops_legacy_or_unknown_receivers_that_may_exit_the_host() {
        let fake = Fake::start(vec![], None).await;
        for version in ["1.0.0", "0.9.0", "unknown", ""] {
            fake.peer.lock().unwrap().version = version.into();
            assert!(load_payload(&fake.endpoint, 9090, &loader_payload()).await.unwrap_err().contains("cannot be safely replaced"));
        }
        assert!(!fake.peer.lock().unwrap().commands.contains(&0x5A));
    }

    #[tokio::test]
    async fn payload_delivery_without_a_receiver_is_not_reported_as_success() {
        let loader = TcpListener::bind("127.0.0.1:0").await.unwrap(); let loader_port = loader.local_addr().unwrap().port();
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap(); let receiver_port = probe.local_addr().unwrap().port(); drop(probe);
        let server = tokio::spawn(async move {
            let (mut socket, _) = loader.accept().await.unwrap();
            let mut body = vec![]; socket.read_to_end(&mut body).await.unwrap(); assert!(body.starts_with(b"\x7fELF"));
        });
        let endpoint = ReceiverEndpoint::ps4(&Settings { ps4_host:"127.0.0.1".into(), ps4_receiver_port:receiver_port, ..Settings::default() });
        let error = load_payload(&endpoint, loader_port, &loader_payload()).await.unwrap_err();
        assert!(error.contains("Payload sent to GoldHEN BinLoader, but no verified PS4 receiver started"));
        assert!(error.contains("only confirms delivery")); server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_payload_is_rejected_before_contacting_a_receiver() {
        let fake = Fake::start(vec![], None).await;
        assert_eq!(load_payload(&fake.endpoint, 9090, b"\x7fELFfake PS4 payload").await.unwrap_err(), INVALID_PAYLOAD);
        assert!(fake.peer.lock().unwrap().commands.is_empty());
    }

    #[test]
    fn ps5_endpoint_literals_and_wire_paths_are_unchanged() {
        let settings = Settings { ps5_host:"192.168.1.5".into(), ps5_port:9120, ..Settings::default() };
        let endpoint = ReceiverEndpoint::ps5(&settings);
        assert_eq!((endpoint.console, endpoint.host.as_str(), endpoint.port, endpoint.expected_version), ("PS5", "192.168.1.5", 9120, RECEIVER_VERSION));
        assert_eq!(endpoint.pkg_dir, "/user/data/tmp"); assert_eq!(endpoint.dump_prefix, Some("/data/homebrew")); assert!(endpoint.required_capabilities.is_empty());
        assert_eq!(endpoint.path_body("/user/data/tmp/test.pkg"), b"/user/data/tmp/test.pkg");
        assert!(remote(&endpoint, Some("CUSA12345")).starts_with("/user/data/tmp/upload_CUSA12345_"));
        let ps4 = ReceiverEndpoint::ps4(&Settings::default()); assert_eq!(ps4.path_body("path"), b"path\0"); assert!(ps4.dump_prefix.is_none());
    }
    #[test]
    fn transport_snapshot_survives_retry_and_old_jobs_stay_inbox() {
        let mut request: DeliveryRequest = serde_json::from_value(json!({"target":"ps4","package":Package { kind:"base".into(), ..Default::default() },"titleId":null})).unwrap();
        assert_eq!(ps4_transport(&request), "inbox");
        let settings = Settings::default(); snapshot_transport(&mut request, &settings, true).unwrap(); assert_eq!(ps4_transport(&request), "inbox");
        snapshot_transport(&mut request, &settings, false).unwrap(); assert_eq!(ps4_transport(&request), "receiver");
        let mut restored: DeliveryRequest = serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
        snapshot_transport(&mut restored, &Settings { ps4_transport:"inbox".into(), ..settings }, true).unwrap(); assert_eq!(ps4_transport(&restored), "receiver");
    }
    #[test]
    fn settings_and_payload_error_codes_are_checked() {
        assert!(validate_settings(Some("receiver"), Some(1024), Some(1), Some(65535)).is_ok());
        for (transport, receiver, loader, serve) in [(Some("ftp"),None,None,None),(None,Some(80),None,None),(None,None,Some(0),None),(None,None,None,Some(0))] {
            assert!(validate_settings(transport,receiver,loader,serve).is_err());
        }
        for code in [ADDCONT_BROKEN as i64, ADDCONT_BROKEN as i32 as i64] { assert_eq!(sdk_code(&json!({"error_code":code})),Some(ADDCONT_BROKEN)); }
        assert_eq!(error_text("PS4: failed /user/data/private.pkg"), "PS4: failed [path]");
        assert_eq!(Settings::default().ps4_transport,"receiver");
    }
}
