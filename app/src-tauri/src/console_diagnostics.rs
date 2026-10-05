//! Read-only console diagnostics (`diagnostics-v1`): the kernel log, processes, and the
//! log, settings and crash files other payloads leave behind. Also checks which known
//! homebrew debug services answer, and relays a live klog server to the interface.
use super::*;
use super::console_tools::{checked, exchange};

const TEXT_MAX: usize = 1024 * 1024;
const REPLY_MAX: usize = TEXT_MAX + 4096;
const CAPABILITY: &str = "diagnostics-v1";

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }
fn invalid(what: &str) -> String { format!("The receiver returned an invalid {what}.") }

/// Diagnostics replies are a JSON header line followed by raw bytes.
fn split_reply<'a>(bytes: &'a [u8], what: &str) -> Result<(Value, &'a [u8]), String> {
    let end = bytes.iter().take(4096).position(|b| *b == b'\n').ok_or_else(|| invalid(what))?;
    let header: Value = serde_json::from_slice(&bytes[..end]).map_err(|_| invalid(what))?;
    if !header.is_object() { return Err(invalid(what)); }
    Ok((header, &bytes[end + 1..]))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KernelLog { source: String, busy: bool, dropped: bool, bytes: usize, text: String, captured_at: u64 }

fn kernel_log(bytes: &[u8]) -> Result<KernelLog, String> {
    let (header, text) = split_reply(bytes, "kernel log")?;
    let source = header["source"].as_str().filter(|s| matches!(*s, "msgbuf" | "klog")).ok_or_else(|| invalid("kernel log"))?;
    if text.len() > TEXT_MAX || header["bytes"].as_u64() != Some(text.len() as u64) { return Err(invalid("kernel log")); }
    Ok(KernelLog {
        source: source.into(), busy: header["busy"].as_bool().unwrap_or(false), dropped: header["dropped"].as_bool().unwrap_or(false),
        bytes: text.len(), text: String::from_utf8_lossy(text).into_owned(), captured_at: now_ms(),
    })
}

#[tauri::command]
pub(super) async fn console_kernel_log(target: String, host: String, port: u16) -> Result<KernelLog, String> {
    let (mut socket, _) = checked(&target, &host, port, CAPABILITY, "read the kernel log").await?;
    kernel_log(&exchange(&mut socket, 0x69, &[], REPLY_MAX).await?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConsoleProcess {
    pid: i64, ppid: i64, name: String, state: String, uid: u64,
    title_id: Option<String>, app_type: Option<u64>, auth_id: Option<String>,
    rss_bytes: u64, vm_bytes: u64, threads: i64, started_at: Option<i64>, cpu_ms: u64,
    /// What the receiver allows here (`process-control-v1`): "app", "payload", or none.
    #[serde(default)] control: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProcessList { processes: Vec<ConsoleProcess>, truncated: bool, captured_at: u64 }

fn title_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 9 && b[..4].iter().all(u8::is_ascii_uppercase) && b[4..].iter().all(u8::is_ascii_digit)
}
fn process_list(bytes: &[u8]) -> Result<ProcessList, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid("process list"))?;
    let processes: Vec<ConsoleProcess> = serde_json::from_value(value["processes"].clone()).map_err(|_| invalid("process list"))?;
    const STATES: [&str; 8] = ["unknown", "starting", "running", "sleeping", "stopped", "zombie", "waiting", "locked"];
    let valid = processes.len() <= 4096 && processes.iter().all(|p| {
        p.pid >= 0 && p.name.len() <= 64 && STATES.contains(&p.state.as_str())
            && p.title_id.as_deref().map_or(true, title_id)
            && p.auth_id.as_deref().map_or(true, |a| a.len() == 16 && a.bytes().all(|b| b.is_ascii_hexdigit()))
            && p.control.as_deref().map_or(true, |c| matches!(c, "app" | "payload"))
    });
    if !valid { return Err(invalid("process list")); }
    Ok(ProcessList { processes, truncated: value["truncated"].as_bool().unwrap_or(false), captured_at: now_ms() })
}

#[tauri::command]
pub(super) async fn console_processes(target: String, host: String, port: u16) -> Result<ProcessList, String> {
    let (mut socket, _) = checked(&target, &host, port, CAPABILITY, "list processes").await?;
    process_list(&exchange(&mut socket, 0x6a, &[], 512 * 1024).await?)
}

pub(super) async fn running_process_names(target: &str, host: &str, port: u16) -> Result<Vec<String>, String> {
    let list = tokio::time::timeout(Duration::from_secs(5),console_processes(target.into(), host.into(), port)).await
        .map_err(|_| "The receiver process check timed out.".to_string())??;
    if list.truncated || list.processes.is_empty() { return Err("The receiver did not return a complete process list.".into()); }
    Ok(list.processes.into_iter().filter(|p| p.state != "zombie").map(|p| p.name).collect())
}

/* ------------------------------------------------------------------ stopping processes */

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProcessControl { pid: i64, name: String, kind: String, method: String, exited: bool, waited_ms: u64 }

fn control_request(pid: i64, name: &str, action: &str) -> Result<Vec<u8>, String> {
    let action = match action { "stop" => b's', "end" => b'e', _ => return Err("Choose Stop or End.".into()) };
    if !(2..=0x7fff_ffff).contains(&pid) { return Err("That process can't be stopped.".into()); }
    if name.is_empty() || name.len() >= 40 || !name.bytes().all(|b| (0x20..0x7f).contains(&b)) { return Err("That process name is invalid.".into()); }
    let mut body = (pid as u32).to_le_bytes().to_vec();
    body.push(action);
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    Ok(body)
}
fn control_reply(bytes: &[u8], pid: i64) -> Result<ProcessControl, String> {
    let reply: ProcessControl = serde_json::from_slice(bytes).map_err(|_| invalid("stop reply"))?;
    if reply.pid != pid || !matches!(reply.kind.as_str(), "app" | "payload") || !matches!(reply.method.as_str(), "close-app" | "sigterm" | "sigkill") {
        return Err(invalid("stop reply"));
    }
    Ok(reply)
}

/// Stops an app or a payload: `stop` closes it the way the system does (SIGTERM for a payload),
/// `end` forces it. The receiver refuses system processes, the loader and itself.
#[tauri::command]
pub(super) async fn console_process_control(target: String, host: String, port: u16, pid: i64, name: String, action: String) -> Result<ProcessControl, String> {
    let body = control_request(pid, &name, &action)?;
    let (mut socket, _) = checked(&target, &host, port, "process-control-v1", "stop processes").await?;
    control_reply(&exchange(&mut socket, 0x6e, &body, 4096).await?, pid)
}

/* ------------------------------------------------------------------ exports */

/// Writes an export the user chose a place for (.txt or .csv), replacing an existing file.
#[tauri::command]
pub(super) fn export_text_file(path: String, contents: String) -> Result<u64, String> {
    let target = PathBuf::from(&path);
    let extension = target.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if !matches!(extension.as_str(), "txt" | "csv") { return Err("Exports are saved as .txt or .csv files.".into()); }
    if contents.len() > 64 * 1024 * 1024 { return Err("This export is larger than 64 MiB.".into()); }
    let parent = target.parent().filter(|p| p.is_dir()).ok_or("Choose a folder that exists.")?;
    let temporary = parent.join(format!(".{}.sspi-export", uuid::Uuid::new_v4()));
    // Windows tools read UTF-8 CSV correctly when it starts with a byte order mark.
    let mut bytes = if extension == "csv" { vec![0xEF, 0xBB, 0xBF] } else { Vec::new() };
    bytes.extend_from_slice(contents.as_bytes());
    std::fs::write(&temporary, &bytes).map_err(|e| format!("The export couldn't be written: {e}"))?;
    std::fs::rename(&temporary, &target).map_err(|e| { let _ = std::fs::remove_file(&temporary); format!("The export couldn't be saved: {e}") })?;
    Ok(bytes.len() as u64)
}

/// Saves a .zip the interface built (theme icon packs), written whole through a temporary file.
#[tauri::command]
pub(super) fn export_zip_file(path: String, data: String) -> Result<u64, String> {
    let target = PathBuf::from(&path);
    if !target.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("zip")) { return Err("Packs are saved as .zip files.".into()); }
    if data.len() > (64usize << 20).div_ceil(3) * 4 { return Err("This pack is larger than 64 MiB.".into()); }
    let bytes = BASE64.decode(data).map_err(|_| "The pack data is invalid.".to_string())?;
    if !bytes.starts_with(b"PK") && !bytes.starts_with(b"PK") { return Err("The pack data is not a zip archive.".into()); }
    let parent = target.parent().filter(|p| p.is_dir()).ok_or("Choose a folder that exists.")?;
    let temporary = parent.join(format!(".{}.sspi-export", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, &bytes).map_err(|e| format!("The pack couldn't be written: {e}"))?;
    std::fs::rename(&temporary, &target).map_err(|e| { let _ = std::fs::remove_file(&temporary); format!("The pack couldn't be saved: {e}") })?;
    Ok(bytes.len() as u64)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LogFile { path: String, size: u64, modified: i64, kind: String }
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LogFileList { files: Vec<LogFile>, truncated: bool, incomplete: bool, captured_at: u64 }

/// The receiver only reads under these folders; the same rule is applied before asking.
fn payload_data_path(path: &str) -> bool {
    (path.starts_with("/data/") || path.starts_with("/user/data/")) && path.len() < 480
        && !path.split('/').any(|part| part == "." || part == "..") && !path.contains("//")
        && path.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b'\\')
}
fn log_files(bytes: &[u8]) -> Result<LogFileList, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid("log list"))?;
    let files: Vec<LogFile> = serde_json::from_value(value["files"].clone()).map_err(|_| invalid("log list"))?;
    if files.len() > 512 || files.iter().any(|f| !payload_data_path(&f.path) || !matches!(f.kind.as_str(), "log" | "config" | "crash")) {
        return Err(invalid("log list"));
    }
    Ok(LogFileList { files, truncated: value["truncated"].as_bool().unwrap_or(false), incomplete: value["incomplete"].as_bool().unwrap_or(false), captured_at: now_ms() })
}

#[tauri::command]
pub(super) async fn console_log_files(target: String, host: String, port: u16) -> Result<LogFileList, String> {
    let (mut socket, _) = checked(&target, &host, port, CAPABILITY, "list payload logs").await?;
    log_files(&exchange(&mut socket, 0x6b, &[], 256 * 1024).await?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LogTail { path: String, size: u64, offset: u64, bytes: u64, modified: i64, text: String }

fn log_tail(bytes: &[u8], path: &str) -> Result<LogTail, String> {
    let (header, text) = split_reply(bytes, "log file")?;
    let (size, offset, count) = (header["size"].as_u64(), header["offset"].as_u64(), header["bytes"].as_u64());
    let (Some(size), Some(offset), Some(count)) = (size, offset, count) else { return Err(invalid("log file")) };
    if header["path"].as_str() != Some(path) || count != text.len() as u64 || offset.checked_add(count) != Some(size) {
        return Err(invalid("log file"));
    }
    Ok(LogTail { path: path.into(), size, offset, bytes: count, modified: header["modified"].as_i64().unwrap_or(0), text: String::from_utf8_lossy(text).into_owned() })
}

#[tauri::command]
pub(super) async fn console_read_log(target: String, host: String, port: u16, path: String, max_bytes: Option<u32>) -> Result<LogTail, String> {
    if !payload_data_path(&path) { return Err("Only log and settings files in payload data folders can be read.".into()); }
    let limit = max_bytes.unwrap_or(256 * 1024).clamp(1, TEXT_MAX as u32);
    let mut body = limit.to_le_bytes().to_vec();
    body.extend_from_slice(path.as_bytes()); body.push(0);
    let (mut socket, _) = checked(&target, &host, port, CAPABILITY, "read payload logs").await?;
    log_tail(&exchange(&mut socket, 0x6c, &body, REPLY_MAX).await?, &path)
}

/* ------------------------------------------------------------------ other payloads' services */

/// Well-known homebrew services. Nothing is sent to them; a connect only shows what is loaded.
/// Payload loader ports (9021, 9090) are never touched: a loader treats any connection as a payload.
fn known_services(target: &str) -> &'static [(u16, &'static str, &'static str)] {
    match target {
        "ps4" => &[(3232, "GoldHEN kernel log", "klog"), (2121, "GoldHEN FTP", "ftp"),
            (744, "ps4debug", "debugger"), (12800, "Remote Package Installer", "installer")],
        _ => &[(3232, "klogsrv kernel log", "klog"), (9081, "etaHEN kernel log", "klog"),
            (1337, "etaHEN FTP", "ftp"), (2121, "ftpsrv FTP", "ftp")],
    }
}
fn console_host(host: &str) -> Result<&str, String> {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 || host.contains(|c: char| c.is_whitespace() || c == '/' || c == '@') {
        return Err("Add the console's address in Options, Consoles.".into());
    }
    Ok(host)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DebugService { port: u16, name: String, kind: String, open: bool, latency_ms: Option<u64> }

#[tauri::command]
pub(super) async fn probe_debug_services(target: String, host: String) -> Result<Vec<DebugService>, String> {
    if !matches!(target.as_str(), "ps4" | "ps5") { return Err("Choose PS4 or PS5.".into()); }
    let host = console_host(&host)?.to_string();
    let checks = known_services(&target).iter().map(|&(port, name, kind)| {
        let host = host.clone();
        async move {
            let started = Instant::now();
            let open = matches!(tokio::time::timeout(Duration::from_millis(800), TcpStream::connect((host.as_str(), port))).await, Ok(Ok(_)));
            DebugService { port, name: name.into(), kind: kind.into(), open, latency_ms: open.then(|| started.elapsed().as_millis() as u64) }
        }
    });
    Ok(join_all(checks).await)
}

/* ------------------------------------------------------------------ live klog relay */

static STREAMS: std::sync::OnceLock<Mutex<HashMap<u64, tokio::task::AbortHandle>>> = std::sync::OnceLock::new();
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);
fn streams() -> &'static Mutex<HashMap<u64, tokio::task::AbortHandle>> { STREAMS.get_or_init(|| Mutex::new(HashMap::new())) }

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KlogStreamEvent { id: u64, text: Option<String>, closed: bool, error: Option<String> }

/// Streams a klog server (GoldHEN, etaHEN or klogsrv) as `klog-stream` events, batched every 200 ms.
#[tauri::command]
pub(super) async fn start_klog_stream(app: AppHandle, target: String, host: String, port: u16) -> Result<u64, String> {
    if !known_services(&target).iter().any(|&(p, _, kind)| p == port && kind == "klog") {
        return Err("That port is not a known kernel log server.".into());
    }
    let host = console_host(&host)?.to_string();
    let socket = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((host.as_str(), port))).await
        .map_err(|_| format!("Nothing answered on port {port}. Enable the kernel log server in your homebrew enabler."))?
        .map_err(|_| format!("Nothing answered on port {port}. Enable the kernel log server in your homebrew enabler."))?;
    let id = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
    let task = tokio::spawn(async move {
        let mut socket = socket;
        let mut buffer = vec![0u8; 32 * 1024];
        let mut pending: Vec<u8> = Vec::new();
        let mut flushed = Instant::now();
        let started = Instant::now();
        let error = loop {
            if started.elapsed() > Duration::from_secs(4 * 3600) { break Some("The live kernel log stopped after four hours.".to_string()); }
            match tokio::time::timeout(Duration::from_millis(200), socket.read(&mut buffer)).await {
                Ok(Ok(0)) => break None,
                Ok(Ok(n)) => pending.extend_from_slice(&buffer[..n]),
                Ok(Err(_)) => break Some("The kernel log connection closed.".to_string()),
                Err(_) => {}
            }
            if !pending.is_empty() && (flushed.elapsed() >= Duration::from_millis(200) || pending.len() > 64 * 1024) {
                let text = String::from_utf8_lossy(&pending).replace('\0', "\n");
                let _ = app.emit("klog-stream", KlogStreamEvent { id, text: Some(text), closed: false, error: None });
                pending.clear(); flushed = Instant::now();
            }
        };
        if !pending.is_empty() {
            let _ = app.emit("klog-stream", KlogStreamEvent { id, text: Some(String::from_utf8_lossy(&pending).into_owned()), closed: false, error: None });
        }
        let _ = app.emit("klog-stream", KlogStreamEvent { id, text: None, closed: true, error });
        streams().lock().unwrap().remove(&id);
    });
    streams().lock().unwrap().insert(id, task.abort_handle());
    Ok(id)
}

#[tauri::command]
pub(super) fn stop_klog_stream(id: u64) -> bool {
    match streams().lock().unwrap().remove(&id) { Some(handle) => { handle.abort(); true } None => false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framed(header: &str, body: &[u8]) -> Vec<u8> { let mut v = header.as_bytes().to_vec(); v.push(b'\n'); v.extend_from_slice(body); v }

    #[test]
    fn kernel_log_keeps_bytes_and_rejects_mismatched_headers() {
        let text = b"Fatal trap 12: page fault\n\xffend\n";
        let log = kernel_log(&framed(&format!(r#"{{"source":"msgbuf","bytes":{},"busy":false,"dropped":false}}"#, text.len()), text)).unwrap();
        assert_eq!(log.source, "msgbuf"); assert!(log.text.starts_with("Fatal trap 12")); assert!(log.text.contains('\u{fffd}'));
        let busy = kernel_log(&framed(r#"{"source":"klog","bytes":0,"busy":true,"dropped":false}"#, b"")).unwrap();
        assert!(busy.busy && busy.text.is_empty());
        assert!(kernel_log(&framed(r#"{"source":"klog","bytes":5,"busy":false}"#, b"abc")).is_err());
        assert!(kernel_log(&framed(r#"{"source":"serial","bytes":3}"#, b"abc")).is_err());
        assert!(kernel_log(b"no header").is_err());
    }

    #[test]
    fn process_lists_are_validated() {
        let good = br#"{"processes":[{"pid":57,"ppid":1,"name":"SceShellUI","state":"sleeping","uid":0,"titleId":"NPXS40087","appType":0,"authId":"3800000000000010","rssBytes":1,"vmBytes":2,"threads":40,"startedAt":1759000000,"cpuMs":1200},
            {"pid":90,"ppid":57,"name":"eboot.bin","state":"running","uid":0,"titleId":null,"appType":null,"authId":null,"rssBytes":1,"vmBytes":2,"threads":3,"startedAt":0,"cpuMs":0}],"truncated":false}"#;
        let list = process_list(good).unwrap();
        assert_eq!(list.processes.len(), 2); assert_eq!(list.processes[0].title_id.as_deref(), Some("NPXS40087"));
        let bad_title = String::from_utf8_lossy(good).replace("NPXS40087", "../etc");
        assert!(process_list(bad_title.as_bytes()).is_err());
        let bad_state = String::from_utf8_lossy(good).replace("\"running\"", "\"exploded\"");
        assert!(process_list(bad_state.as_bytes()).is_err());
    }

    #[test]
    fn log_lists_and_reads_stay_inside_payload_data_folders() {
        let list = log_files(br#"{"files":[{"path":"/data/etaHEN/etaHEN.log","size":9,"modified":1759000000,"kind":"log"},
            {"path":"/user/data/orbiscore-1-eboot.bin.orbisdmp","size":40,"modified":1759000000,"kind":"crash"}],"truncated":false,"incomplete":false}"#).unwrap();
        assert_eq!(list.files.len(), 2);
        assert!(log_files(br#"{"files":[{"path":"/system/priv/x.log","size":1,"modified":0,"kind":"log"}]}"#).is_err());
        assert!(log_files(br#"{"files":[{"path":"/data/../system/x.log","size":1,"modified":0,"kind":"log"}]}"#).is_err());
        for path in ["/data/etaHEN/etaHEN.log", "/user/data/GoldHEN/plugins.ini"] { assert!(payload_data_path(path)); }
        for path in ["/data/../x.log", "/data//x.log", "/mnt/usb0/x.log", "data/x.log", "/data/a\\b.log"] { assert!(!payload_data_path(path)); }
        let tail = log_tail(&framed(r#"{"path":"/data/etaHEN/etaHEN.log","size":10,"offset":6,"bytes":4,"modified":1}"#, b"end\n"), "/data/etaHEN/etaHEN.log").unwrap();
        assert_eq!(tail.text, "end\n");
        assert!(log_tail(&framed(r#"{"path":"/data/other.log","size":4,"offset":0,"bytes":4}"#, b"end\n"), "/data/etaHEN/etaHEN.log").is_err());
        assert!(log_tail(&framed(r#"{"path":"/data/a.log","size":9,"offset":6,"bytes":4}"#, b"end\n"), "/data/a.log").is_err());
    }

    #[test]
    fn ps4_receiver_and_goldhen_logs_are_listed_and_readable() {
        let paths = ["/user/data/sspi-receiver/receiver.log", "/user/data/sspi-receiver/bootstrap.log",
            "/user/data/sspi-receiver/receiver.log.1", "/data/GoldHEN/goldhen.log", "/data/GoldHEN/logs/klog.txt"];
        let files: Vec<_> = paths.iter().map(|path| serde_json::json!({
            "path": path, "size": 5, "modified": 1759000000, "kind": "log"
        })).collect();
        let listing = serde_json::to_vec(&serde_json::json!({
            "files": files, "truncated": false, "incomplete": false, "roots": 4
        })).unwrap();
        assert_eq!(log_files(&listing).unwrap().files.len(), paths.len());
        for path in paths {
            assert!(payload_data_path(path));
            let header = serde_json::json!({ "path": path, "size": 5, "offset": 0, "bytes": 5, "modified": 1759000000 });
            assert_eq!(log_tail(&framed(&header.to_string(), b"boot\n"), path).unwrap().text, "boot\n");
        }
        assert!(!payload_data_path("/user/data/sspi-receiver/../secret.log"));
        assert!(!payload_data_path("/data/GoldHEN/../../system/secret.log"));
    }

    #[test]
    fn stop_requests_and_replies_are_strict() {
        let body = control_request(88, "kstuff.elf", "stop").unwrap();
        assert_eq!(body, [88, 0, 0, 0, b's', b'k', b's', b't', b'u', b'f', b'f', b'.', b'e', b'l', b'f', 0]);
        assert_eq!(control_request(88, "kstuff.elf", "end").unwrap()[4], b'e');
        assert!(control_request(1, "init", "stop").is_err());
        assert!(control_request(88, "kstuff.elf", "pause").is_err());
        assert!(control_request(88, "", "stop").is_err() && control_request(88, "bad\nname", "stop").is_err());
        let ok = control_reply(br#"{"pid":88,"name":"kstuff.elf","kind":"payload","method":"sigterm","exited":true,"waitedMs":120}"#, 88).unwrap();
        assert!(ok.exited && ok.waited_ms == 120);
        assert!(control_reply(br#"{"pid":89,"name":"x","kind":"payload","method":"sigterm","exited":true,"waitedMs":1}"#, 88).is_err());
        assert!(control_reply(br#"{"pid":88,"name":"x","kind":"system","method":"sigterm","exited":true,"waitedMs":1}"#, 88).is_err());
        let list = process_list(br#"{"processes":[{"pid":88,"ppid":86,"name":"kstuff.elf","state":"sleeping","uid":0,"titleId":null,"appType":0,"authId":null,"rssBytes":1,"vmBytes":2,"threads":3,"startedAt":0,"cpuMs":0,"control":"payload"}],"truncated":false}"#).unwrap();
        assert_eq!(list.processes[0].control.as_deref(), Some("payload"));
        assert!(process_list(br#"{"processes":[{"pid":88,"ppid":86,"name":"x","state":"sleeping","uid":0,"rssBytes":1,"vmBytes":2,"threads":3,"cpuMs":0,"control":"system"}]}"#).is_err());
    }

    #[test]
    fn exports_are_text_or_csv_and_written_whole() {
        let root = crate::test_output_root().join(format!("export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let csv = root.join("processes.csv");
        assert_eq!(export_text_file(csv.display().to_string(), "pid,name\n88,kstuff.elf\n".into()).unwrap(), 3 + 23);
        assert!(std::fs::read(&csv).unwrap().starts_with(&[0xEF, 0xBB, 0xBF]));
        let txt = root.join("klog.TXT");
        export_text_file(txt.display().to_string(), "panic".into()).unwrap();
        assert_eq!(std::fs::read_to_string(&txt).unwrap(), "panic");
        assert!(export_text_file(root.join("x.exe").display().to_string(), "x".into()).is_err());
        assert!(export_text_file(root.join("missing/x.txt").display().to_string(), "x".into()).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2, "no temporary files are left behind");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn zip_exports_are_zip_archives_only() {
        let root = std::env::temp_dir().join(format!("sspi-zip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        // An empty archive: just the 22-byte end-of-central-directory record.
        let zip = BASE64.encode([b"PK".as_slice(), &[0u8; 18]].concat());
        assert_eq!(export_zip_file(root.join("icons.ZIP").display().to_string(), zip.clone()).unwrap(), 22);
        assert!(export_zip_file(root.join("icons.png").display().to_string(), zip).is_err());
        assert!(export_zip_file(root.join("x.zip").display().to_string(), BASE64.encode(b"not a zip")).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn klog_relay_only_connects_to_known_kernel_log_ports() {
        assert!(known_services("ps4").iter().any(|s| s.0 == 3232 && s.2 == "klog"));
        assert!(known_services("ps5").iter().any(|s| s.0 == 9081 && s.2 == "klog"));
        for target in ["ps4", "ps5"] { assert!(!known_services(target).iter().any(|s| s.0 == 9021 || s.0 == 9090), "loader ports must never be probed"); }
        assert!(console_host(" ").is_err() && console_host("a/b").is_err() && console_host("192.0.2.20").is_ok());
    }
}
