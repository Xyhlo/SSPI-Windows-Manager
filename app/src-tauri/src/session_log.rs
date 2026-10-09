use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, mpsc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use regex::Regex;

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_MESSAGE_CHARS: usize = 16 * 1024;
const QUEUE: usize = 1024;
static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
// Callers only queue records: transfer and UI paths never wait for the disk.
static WRITER: OnceLock<Mutex<mpsc::SyncSender<Queued>>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);
static SECRETS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

enum Queued { Line(Vec<u8>), Flush(mpsc::Sender<()>) }

struct Log {
    path: PathBuf,
}

impl Log {
    fn open(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let path = directory.join("SSPI.log");
        OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path })
    }

    fn append(&self, scope: &str, message: &str) -> io::Result<()> {
        self.append_lines(&[record(scope, message)?])
    }

    fn append_lines(&self, lines: &[Vec<u8>]) -> io::Result<()> {
        let bytes = lines.iter().map(|line| line.len() as u64).sum::<u64>();
        let length = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if length > 0 && length.saturating_add(bytes) > MAX_FILE_BYTES {
            // Replaces the previous file. A viewer or scanner holding either file must not end
            // logging: keep appending up to twice the limit and rotate on a later write.
            if let Err(error) = fs::rename(&self.path, self.path.with_extension("previous.log")) {
                if length.saturating_add(bytes) > 2 * MAX_FILE_BYTES { return Err(error); }
            }
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        file.write_all(&lines.concat())?;
        file.flush()
    }
}

fn record(scope: &str, message: &str) -> io::Result<Vec<u8>> {
    let record = serde_json::json!({
        "unixMs": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "pid": std::process::id(),
        "scope": sanitize(scope),
        "message": sanitize(message),
    });
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    Ok(line)
}

fn sanitize(message: &str) -> String {
    static URL: OnceLock<Regex> = OnceLock::new();
    static AUTH: OnceLock<Regex> = OnceLock::new();
    static SECRET: OnceLock<Regex> = OnceLock::new();
    let mut message = message.to_string();
    if let Some(secrets) = SECRETS.get() {
        for secret in secrets.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter() {
            if message.contains(secret.as_str()) { message = message.replace(secret.as_str(), "[redacted]"); }
        }
    }
    let message = URL.get_or_init(|| Regex::new(r#"(?i)\b(?:https?|ftp)://[^\s\"'<>]+"#).unwrap())
        .replace_all(&message, "[url redacted]");
    let message = AUTH.get_or_init(|| Regex::new(r#"(?i)\b(?:Bearer|Basic)[\t ]+[^\s\"'<>;,]+"#).unwrap())
        .replace_all(&message, "[authorization redacted]");
    // Prefixed names too: auth_token, access_token, client_secret, x-api-key.
    let message = SECRET.get_or_init(|| Regex::new(r#"(?i)(\b(?:[a-z0-9]+[_-])*(?:password|passwd|token|api[_-]?key|apikey|authorization|secret)\b[\"']?\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^\s,;]+)"#).unwrap())
        .replace_all(&message, "$1[redacted]");
    let mut bounded: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
    if message.chars().count() > MAX_MESSAGE_CHARS { bounded.push_str(" [truncated]"); }
    bounded
}

/// A value that must never reach the log, such as a saved provider key or password.
pub(super) fn protect(secret: &str) {
    let secret = secret.trim();
    if secret.len() < 6 { return; }
    let mut secrets = SECRETS.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if secrets.iter().any(|known| known == secret) { return; }
    if secrets.len() >= 32 { secrets.remove(0); }
    secrets.push(secret.to_string());
}

/// Registers the credentials saved in Windows Credential Manager.
pub(super) fn protect_saved_secrets() {
    for name in ["real-debrid", "torbox", "alldebrid", "ps4-ftp"] {
        if let Ok(value) = super::secret(name).and_then(|entry| entry.get_password().map_err(|error| error.to_string())) { protect(&value); }
    }
}

/// Prefer a portable log beside SSPI.exe; installed/read-only folders fall back
/// to the existing application's roaming data directory.
pub(super) fn initialize() {
    let mut directories = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() { directories.push(parent.join("logs")); }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        directories.push(PathBuf::from(appdata).join("com.simpleps5installer.gamesearch").join("logs"));
    }
    for directory in directories {
        match Log::open(&directory) {
            Ok(log) => { let _ = LOG.set(Mutex::new(log)); break; }
            Err(error) => eprintln!("Session log {}: {error}", directory.display()),
        }
    }
    if LOG.get().is_some() {
        let (sender, receiver) = mpsc::sync_channel(QUEUE);
        let started = std::thread::Builder::new().name("session-log".into()).spawn(move || writer(receiver));
        if started.is_ok() { let _ = WRITER.set(Mutex::new(sender)); }
    }
    write("startup", &format!("SSPI Windows Manager {} started; PS5 receiver {}; PS4 receiver {}", env!("CARGO_PKG_VERSION"), super::RECEIVER_VERSION, super::PS4_RECEIVER_VERSION));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write("panic", &info.to_string());
        previous(info);
    }));
}

fn writer(receiver: mpsc::Receiver<Queued>) {
    while let Ok(first) = receiver.recv() {
        let (mut lines, mut flushed) = (Vec::new(), Vec::new());
        for entry in std::iter::once(first).chain(receiver.try_iter().take(QUEUE)) {
            match entry { Queued::Line(line) => lines.push(line), Queued::Flush(done) => flushed.push(done) }
        }
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            if let Ok(line) = record("session-log", &format!("{dropped} records were dropped while the log file was busy")) { lines.push(line); }
        }
        if let Some(log) = LOG.get().filter(|_| !lines.is_empty()) {
            let log = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Err(error) = log.append_lines(&lines) { eprintln!("Session log: {error}"); }
        }
        for done in flushed { let _ = done.send(()); }
    }
}

/// Waits briefly until queued records are written; used before the process can exit.
pub(super) fn flush() {
    let Some(writer) = WRITER.get() else { return; };
    let deadline = Instant::now() + Duration::from_secs(2);
    let (done, wait) = mpsc::channel();
    let mut entry = Queued::Flush(done);
    loop {
        match writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).try_send(entry) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(back)) if Instant::now() < deadline => entry = back,
            Err(_) => return,
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = wait.recv_timeout(deadline.saturating_duration_since(Instant::now()));
}

pub(super) fn location() -> Option<String> {
    LOG.get()?.lock().ok().map(|log| log.path.display().to_string())
}

#[tauri::command]
pub(super) fn show_session_log() -> Result<(), String> {
    let path = location().ok_or_else(|| "SSPI could not create a diagnostic log in its installation or application-data folder. Check that one of those folders is writable.".to_string())?;
    if !Path::new(&path).is_file() {
        write("diagnostics", "Diagnostic log opened from SSPI");
        flush();
    }
    if !Path::new(&path).is_file() {
        return Err(format!("The diagnostic log could not be created: {path}"));
    }
    std::process::Command::new("explorer.exe")
        .arg(format!("/select,{path}"))
        .spawn()
        .map_err(|error| format!("Could not show the diagnostic log at {path}: {error}"))?;
    Ok(())
}

pub(super) fn write(scope: &str, message: &str) {
    let Some(log) = LOG.get() else { return; };
    let Ok(mut line) = record(scope, message) else { return; };
    // A panic hook must not wait for a lock held by the panicking thread; its record is written
    // before the hook returns, or queued while the writer holds the file.
    if std::thread::panicking() {
        match log.try_lock() {
            Ok(log) => { let _ = log.append_lines(&[line]); }
            Err(_) => if let Some(writer) = WRITER.get().and_then(|writer| writer.try_lock().ok()) { let _ = writer.try_send(Queued::Line(line)); },
        }
        return;
    }
    if let Some(writer) = WRITER.get() {
        match writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).try_send(Queued::Line(line)) {
            Ok(()) => return,
            Err(mpsc::TrySendError::Full(_)) => { DROPPED.fetch_add(1, Ordering::Relaxed); return; }
            Err(mpsc::TrySendError::Disconnected(Queued::Line(back))) => line = back,
            Err(mpsc::TrySendError::Disconnected(_)) => return,
        }
    }
    let log = log.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(error) = log.append_lines(&[line]) { eprintln!("Session log: {error}"); }
}

/// The message with each run of digits as `#`, so counters alone don't make a new record.
fn message_shape(message: &str) -> String {
    let mut shape = String::with_capacity(message.len());
    for c in message.chars() {
        if !c.is_ascii_digit() { shape.push(c); } else if !shape.ends_with('#') { shape.push('#'); }
    }
    shape
}

pub(super) fn progress(p: &super::Progress) {
    if LOG.get().is_none() { return; }
    static LAST: OnceLock<Mutex<HashMap<String, (String, String, bool, bool, Instant)>>> = OnceLock::new();
    let Ok(mut last) = LAST.get_or_init(|| Mutex::new(HashMap::new())).lock() else { return; };
    let shape = message_shape(&p.message);
    if last.get(&p.job_id).is_some_and(|(stage, message, paused, priority, at)| {
        stage == &p.stage && message == &shape && *paused == p.paused && *priority == p.priority && at.elapsed() < Duration::from_secs(10)
    }) { return; }
    if last.len() >= 1024 && !last.contains_key(&p.job_id) {
        if let Some(oldest) = last.iter().min_by_key(|(_, (_, _, _, _, at))| *at).map(|(id, _)| id.clone()) { last.remove(&oldest); }
    }
    last.insert(p.job_id.clone(), (p.stage.clone(), shape, p.paused, p.priority, Instant::now()));
    drop(last);
    write("delivery", &format!("job={} target={} stage={} paused={} priority={} bytes={}/{} {}",
        p.job_id, p.target, p.stage, p.paused, p.priority, p.bytes_done, p.bytes_total, p.message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_removed_and_multiline_records_remain_single_lines() {
        let message = sanitize("https://user:pass@example.test/path?token=abc Bearer abc Basic xyz password=hidden token:\"two words\" api_key='secret value'\nnext");
        for secret in ["user:pass", "abc", "xyz", "hidden", "two words", "secret value"] { assert!(!message.contains(secret), "{message}"); }
        assert!(message.contains("next"));
        let json = serde_json::to_string(&message).unwrap();
        assert!(!json.contains('\n'));
        assert!(sanitize(&"é".repeat(MAX_MESSAGE_CHARS + 1)).ends_with(" [truncated]"));
    }

    #[test]
    fn saved_keys_and_prefixed_credential_names_are_removed() {
        protect("A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6");
        protect("short");
        let message = sanitize("api.example.test/rest?auth_token=first {\"access_token\":\"second\",\"refresh_token\": \"third\"} client_secret=fourth X-Api-Key: fifth \
            key A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6 failed; tokens remain short");
        for secret in ["first", "second", "third", "fourth", "fifth", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6"] { assert!(!message.contains(secret), "{message}"); }
        assert!(message.contains("tokens remain short"), "{message}");
    }

    #[test]
    fn counters_alone_do_not_change_a_progress_record() {
        assert_eq!(message_shape("Extracting 1.20 / 5.00 GiB"), message_shape("Extracting 10.75 / 5.00 GiB"));
        assert_ne!(message_shape("Extracting 1.20 / 5.00 GiB"), message_shape("Extraction failed at 1.20 GiB"));
    }

    fn test_log() -> (PathBuf, Log) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../Build-Output/Windows Manager/tests/session-log").join(uuid::Uuid::new_v4().to_string());
        let log = Log::open(&root).unwrap();
        (root, log)
    }

    #[test]
    fn rotation_retains_previous_log_and_new_record() {
        let (root, log) = test_log();
        OpenOptions::new().write(true).open(&log.path).unwrap().set_len(MAX_FILE_BYTES).unwrap();
        log.append("loader", "failed\nsecond line").unwrap();
        assert_eq!(fs::metadata(log.path.with_extension("previous.log")).unwrap().len(), MAX_FILE_BYTES);
        let text = fs::read_to_string(&log.path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let record: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(record["message"], "failed\nsecond line");
        fs::remove_file(&log.path).unwrap();
        log.append("loader", "recreated").unwrap();
        assert!(fs::read_to_string(&log.path).unwrap().contains("recreated"));
        // A second rotation replaces the older previous file.
        OpenOptions::new().write(true).open(&log.path).unwrap().set_len(MAX_FILE_BYTES).unwrap();
        log.append("loader", "third").unwrap();
        assert_eq!(fs::metadata(log.path.with_extension("previous.log")).unwrap().len(), MAX_FILE_BYTES);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_blocked_rotation_keeps_logging_within_twice_the_limit() {
        let (root, log) = test_log();
        fs::create_dir(log.path.with_extension("previous.log")).unwrap();
        OpenOptions::new().write(true).open(&log.path).unwrap().set_len(MAX_FILE_BYTES).unwrap();
        log.append("loader", "kept while rotation is blocked").unwrap();
        assert!(fs::metadata(&log.path).unwrap().len() > MAX_FILE_BYTES);
        OpenOptions::new().write(true).open(&log.path).unwrap().set_len(2 * MAX_FILE_BYTES).unwrap();
        assert!(log.append("loader", "over the hard limit").is_err());
        assert_eq!(fs::metadata(&log.path).unwrap().len(), 2 * MAX_FILE_BYTES);
        fs::remove_dir_all(root).unwrap();
    }
}
