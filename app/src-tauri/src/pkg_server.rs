use crate::pkg_meta::{self, PkgMeta};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex, MutexGuard, OnceLock},
    time::{Instant, SystemTime},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::{timeout, Duration},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const STREAM_BUFFER_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
// BGFT can stop reading a body while it prepares or verifies on its own disk. SSPI PS4's
// console-tested server allows 90 s without progress; every accepted byte restarts it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(90);
// Over capacity, a connection waits this long for a slot before it is answered 503.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_DRAIN: Duration = Duration::from_secs(2);
const LIMITS: Limits = Limits { connections: MAX_CONNECTIONS, queue: QUEUE_TIMEOUT };

#[derive(Clone, Copy)]
struct Limits { connections: usize, queue: Duration }

#[derive(Debug, Clone)]
pub(super) struct Served {
    #[allow(dead_code)]
    pub manifest_url: String,
    #[allow(dead_code)]
    pub package_url: String,
    #[allow(dead_code)]
    pub icon_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Activity {
    #[allow(dead_code)]
    pub bytes_sent: u64,
    #[allow(dead_code)]
    pub requests: u64,
    #[allow(dead_code)]
    pub idle_ms: u64,
}

#[derive(Default)]
struct ServerState {
    port: Option<u16>,
    tokens: HashMap<String, Arc<Registered>>,
}

struct Registered {
    path: PathBuf,
    identity: FileIdentity,
    changed: AtomicBool,
    meta: PkgMeta,
    icon: Option<Vec<u8>>,
    package_url: String,
    activity: Mutex<ActivityState>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity { length: u64, modified: SystemTime }
impl FileIdentity {
    fn read(metadata: &std::fs::Metadata) -> std::io::Result<Self> {
        Ok(Self { length: metadata.len(), modified: metadata.modified()? })
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Source { Same, Changed, Unavailable }
impl Registered {
    // A changed or deleted source is refused for good. One that can't be opened right now
    // (a sharing violation, a network share hiccup) is retried by the client instead.
    fn check(&self, metadata: std::io::Result<std::fs::Metadata>) -> Source {
        let same = match metadata {
            Ok(metadata) => FileIdentity::read(&metadata).ok() == Some(self.identity),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound && !self.changed.load(Ordering::Relaxed) => return Source::Unavailable,
            Err(_) => false,
        };
        if !same && !self.changed.swap(true, Ordering::Relaxed) {
            eprintln!("PS4 PKG source changed or disappeared; refusing to serve stale package data.");
        }
        if same && !self.changed.load(Ordering::Relaxed) { Source::Same } else { Source::Changed }
    }
}

struct ActivityState {
    bytes_sent: u64,
    requests: u64,
    last_request: Instant,
}

#[derive(Debug, Clone, Copy)]
enum RouteKind {
    Manifest,
    Package,
    Icon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedRange {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

struct Request {
    method: String,
    target: String,
    range: Option<String>,
}

static STATE: OnceLock<Mutex<ServerState>> = OnceLock::new();
static START_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn state() -> &'static Mutex<ServerState> {
    STATE.get_or_init(|| Mutex::new(ServerState::default()))
}

fn lock_state() -> MutexGuard<'static, ServerState> {
    state().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_activity(entry: &Registered) -> MutexGuard<'_, ActivityState> {
    entry.activity.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[allow(dead_code)]
pub(super) async fn ensure_started(port: u16) -> Result<(), String> {
    let _start = START_LOCK.lock().await;
    {
        let state = lock_state();
        if let Some(bound_port) = state.port {
            if bound_port == port {
                return Ok(());
            }
            if !state.tokens.is_empty() {
                return Err(format!("PKG server is already bound to port {bound_port} while packages are registered"));
            }
            return Err(format!("The PC download port is still {bound_port}. Restart SSPI to use port {port}."));
        }
    }

    let listener = TcpListener::bind(("0.0.0.0", port)).await
        .map_err(|_| format!("Unable to bind the PKG server to port {port}; check Windows Firewall and port use"))?;
    {
        let mut state = lock_state();
        state.port = Some(port);
    }
    tokio::spawn(async move {
        let _lifetime = ServerLifetime;
        accept_loop(listener, LIMITS).await
    });
    Ok(())
}

#[allow(dead_code)]
pub(super) fn register(
    token: &str,
    pkg: &Path,
    icon_png: Option<Vec<u8>>,
    host: IpAddr,
    port: u16,
) -> Result<Served, String> {
    if !valid_token(token) {
        return Err("Invalid PKG server token".into());
    }
    let path = std::fs::canonicalize(pkg).map_err(|_| "Unable to locate the PKG source".to_string())?;
    let identity = std::fs::metadata(&path).and_then(|m| FileIdentity::read(&m))
        .map_err(|_| "Unable to read the PKG source identity".to_string())?;
    let meta = pkg_meta::read(&path)?;
    if std::fs::metadata(&path).and_then(|m| FileIdentity::read(&m)).ok() != Some(identity) {
        return Err("PKG source changed while being registered".into());
    }
    let host = match host {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    let base = format!("http://{host}:{port}");
    let package_url = format!("{base}/pkg/{token}.pkg");
    let manifest_url = format!("{base}/pkg/{token}.pkg.json");
    let icon = icon_png
        .filter(|bytes| pkg_meta::is_valid_png(bytes))
        .or_else(|| meta.icon0.clone());
    let icon_url = icon.as_ref().map(|_| format!("{base}/icon/{token}.png"));
    let entry = Arc::new(Registered {
        path,
        identity,
        changed: AtomicBool::new(false),
        meta,
        icon,
        package_url: package_url.clone(),
        activity: Mutex::new(ActivityState {
            bytes_sent: 0,
            requests: 0,
            last_request: Instant::now(),
        }),
    });
    lock_state().tokens.insert(token.to_string(), entry);
    Ok(Served { manifest_url, package_url, icon_url })
}

#[allow(dead_code)]
pub(super) fn unregister(token: &str) {
    lock_state().tokens.remove(token);
}

// Also protect folders containing a served source from recursive job cleanup.
pub(super) fn is_served(path: &Path) -> bool {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    lock_state().tokens.values().any(|entry| entry.path == path || entry.path.starts_with(&path))
}

#[allow(dead_code)]
pub(super) fn activity(token: &str) -> Option<Activity> {
    let entry = lock_state().tokens.get(token).cloned()?;
    let activity = lock_activity(&entry);
    Some(Activity {
        bytes_sent: activity.bytes_sent,
        requests: activity.requests,
        idle_ms: activity.last_request.elapsed().as_millis().min(u64::MAX as u128) as u64,
    })
}

struct ServerLifetime;
impl Drop for ServerLifetime {
    fn drop(&mut self) { lock_state().port = None; }
}
async fn accept_loop(listener: TcpListener, limits: Limits) {
    let permits = Arc::new(Semaphore::new(limits.connections));
    // Connections waiting for a slot, or for their 503, are bounded separately.
    let queued = Arc::new(Semaphore::new(limits.connections));
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("PKG server connection failed: {error}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        if let Ok(permit) = permits.clone().try_acquire_owned() {
            tokio::spawn(async move {
                let _permit = permit;
                serve_connection(&mut stream).await;
            });
            continue;
        }
        let Ok(waiting) = queued.clone().try_acquire_owned() else { continue; };
        let permits = permits.clone();
        tokio::spawn(async move {
            match timeout(limits.queue, permits.acquire_owned()).await {
                Ok(Ok(permit)) => {
                    drop(waiting);
                    let _permit = permit;
                    serve_connection(&mut stream).await;
                }
                _ => {
                    // Read the request first so the reply isn't lost to a reset.
                    let _ = read_request_with_timeout(&mut stream, &mut Vec::new(), CLOSE_DRAIN).await;
                    let _ = send_bytes(&mut stream, 503, "Retry-After: 1\r\n", &[], 0, false).await;
                    finish(&mut stream).await;
                }
            }
        });
    }
}

// One response per connection, as SSPI PS4's console-tested BGFT server does: a pooled
// connection can't go stale while BGFT pauses, and idle connections never hold a slot.
async fn serve_connection(stream: &mut TcpStream) {
    let mut pending = Vec::with_capacity(1024);
    match read_request(stream, &mut pending).await {
        Ok(Some(request)) => handle_request(stream, request).await,
        Ok(None) => {}
        Err(()) => { let _ = send_bytes(stream, 400, "", &[], 0, false).await; }
    }
    finish(stream).await;
}

// FIN first, then a bounded drain, so unread request bytes can't turn the close into a
// reset that discards the end of the response.
async fn finish(stream: &mut TcpStream) {
    let _ = stream.shutdown().await;
    let _ = timeout(CLOSE_DRAIN, async {
        let mut sink = [0u8; 4096];
        let mut drained = 0;
        while drained < 64 * 1024 {
            match stream.read(&mut sink).await {
                Ok(0) | Err(_) => break,
                Ok(read) => drained += read,
            }
        }
    }).await;
}

async fn read_request(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Result<Option<Request>, ()> {
    read_request_with_timeout(stream, pending, REQUEST_TIMEOUT).await
}

async fn read_request_with_timeout(stream: &mut TcpStream, pending: &mut Vec<u8>, duration: Duration) -> Result<Option<Request>, ()> {
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        if let Some(end) = header_end(pending) {
            if end + 4 > MAX_HEADER_BYTES {
                return Err(());
            }
            let header: Vec<u8> = pending.drain(..end + 4).collect();
            return parse_request(&header[..end]).map(Some);
        }
        if pending.len() >= MAX_HEADER_BYTES {
            return Err(());
        }
        let mut buffer = [0u8; 2048];
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await {
            Ok(Ok(read)) => read,
            _ => return Ok(None),
        };
        if read == 0 {
            return Ok(None);
        }
        pending.extend_from_slice(&buffer[..read]);
    }
}

fn header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_request(header: &[u8]) -> Result<Request, ()> {
    // Only the request line and Range are used; other header values may carry any octets.
    let text = String::from_utf8_lossy(header);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().ok_or(())?.split_whitespace();
    let method = first.next().ok_or(())?;
    let target = first.next().ok_or(())?;
    let version = first.next().ok_or(())?;
    if first.next().is_some() || (version != "HTTP/1.1" && version != "HTTP/1.0") {
        return Err(());
    }

    let mut range: Option<String> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(())?;
        // RFC 7230 token characters, so names such as X_Name are accepted.
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)) {
            return Err(());
        }
        if name.eq_ignore_ascii_case("Range") {
            let value = value.trim();
            range = Some(match range {
                Some(previous) => format!("{previous},{value}"),
                None => value.to_string(),
            });
        }
    }
    Ok(Request { method: method.to_string(), target: target.to_string(), range })
}

async fn source_ready(stream: &mut TcpStream, entry: &Registered, metadata: std::io::Result<std::fs::Metadata>, head: bool) -> bool {
    match entry.check(metadata) {
        Source::Same => true,
        Source::Changed => { let _ = send_bytes(stream, 409, "Cache-Control: no-store\r\n", &[], 0, head).await; false }
        Source::Unavailable => { let _ = send_bytes(stream, 503, "Retry-After: 5\r\nCache-Control: no-store\r\n", &[], 0, head).await; false }
    }
}

async fn handle_request(stream: &mut TcpStream, request: Request) {
    let route = parse_route(&request.target);
    let registered = route.and_then(|(_, token)| lock_state().tokens.get(token).cloned());
    let head = request.method == "HEAD";
    if request.method != "GET" && !head {
        if let Some(entry) = registered.as_ref() {
            touch_request(entry);
        }
        let _ = send_bytes(stream, 405, "Allow: GET, HEAD\r\n", &[], 0, false).await;
        return;
    }

    let (Some((kind, _)), Some(entry)) = (route, registered) else {
        let _ = send_bytes(stream, 404, "", &[], 0, head).await;
        return;
    };
    touch_request(&entry);

    if matches!(kind, RouteKind::Manifest | RouteKind::Package)
        && !source_ready(stream, &entry, tokio::fs::metadata(&entry.path).await, head).await {
        return;
    }

    match kind {
        RouteKind::Manifest => {
            let body = pkg_meta::manifest_json(&entry.meta, &entry.package_url).into_bytes();
            let extra = "Content-Type: application/json\r\nCache-Control: no-store\r\n";
            if send_bytes(stream, 200, extra, &body, body.len() as u64, head).await.is_ok() && !head {
                add_sent(&entry, body.len() as u64);
            }
        }
        RouteKind::Package => serve_package(stream, &entry, request.range.as_deref(), head).await,
        RouteKind::Icon => {
            let Some(icon) = entry.icon.as_ref() else {
                let _ = send_bytes(stream, 404, "", &[], 0, head).await;
                return;
            };
            if send_bytes(stream, 200, "Content-Type: image/png\r\n", icon, icon.len() as u64, head).await.is_ok() && !head {
                add_sent(&entry, icon.len() as u64);
            }
        }
    }
}

fn parse_route(target: &str) -> Option<(RouteKind, &str)> {
    let path = target.split_once('?').map(|(path, _)| path).unwrap_or(target);
    if let Some(route) = path.strip_prefix("/pkg/") {
        if let Some(token) = route.strip_suffix(".pkg.json") {
            return Some((RouteKind::Manifest, token));
        }
        if let Some(token) = route.strip_suffix(".pkg") {
            return Some((RouteKind::Package, token));
        }
    }
    if let Some(route) = path.strip_prefix("/icon/") {
        if let Some(token) = route.strip_suffix(".png") {
            return Some((RouteKind::Icon, token));
        }
    }
    None
}

fn valid_token(token: &str) -> bool {
    (16..=64).contains(&token.len()) && token.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn touch_request(entry: &Registered) {
    let mut activity = lock_activity(entry);
    activity.requests = activity.requests.saturating_add(1);
    activity.last_request = Instant::now();
}

fn add_sent(entry: &Registered, bytes: u64) {
    let mut activity = lock_activity(entry);
    activity.bytes_sent = activity.bytes_sent.saturating_add(bytes);
}

async fn serve_package(stream: &mut TcpStream, entry: &Registered, range: Option<&str>, head: bool) {
    let file_size = entry.meta.file_size;
    let (status, start, body_size, content_range) = match parse_range(range, file_size) {
        ParsedRange::Full => (200, 0, file_size, None),
        ParsedRange::Partial { start, end } => (206, start, end - start + 1, Some(format!("Content-Range: bytes {start}-{end}/{file_size}\r\n"))),
        ParsedRange::Unsatisfiable => {
            let extra = format!("Content-Range: bytes */{file_size}\r\nAccept-Ranges: bytes\r\n");
            let _ = send_bytes(stream, 416, &extra, &[], 0, head).await;
            return;
        }
    };
    let mut extra = String::from("Accept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nCache-Control: no-transform\r\n");
    if let Some(content_range) = content_range { extra.push_str(&content_range); }
    if head {
        let _ = send_headers(stream, status, &extra, body_size).await;
        return;
    }

    let mut file = match File::open(&entry.path).await {
        Ok(file) => file,
        Err(error) => {
            source_ready(stream, entry, Err(error), false).await;
            return;
        }
    };
    if !source_ready(stream, entry, file.metadata().await, false).await {
        return;
    }
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        let _ = send_bytes(stream, 503, "Retry-After: 1\r\n", &[], 0, false).await;
        return;
    }
    if send_headers(stream, status, &extra, body_size).await.is_err() {
        return;
    }

    let mut remaining = body_size;
    let mut buffer = vec![0u8; body_size.min(STREAM_BUFFER_BYTES as u64) as usize];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let read = match file.read(&mut buffer[..wanted]).await {
            Ok(read) if read > 0 => read,
            _ => return,
        };
        if write_response(stream, &buffer[..read], WRITE_TIMEOUT).await.is_err() {
            return;
        }
        add_sent(entry, read as u64);
        remaining -= read as u64;
    }
}

fn parse_range(header: Option<&str>, length: u64) -> ParsedRange {
    let Some(header) = header else { return ParsedRange::Full; };
    let header = header.trim();
    let Some((unit, value)) = header.split_once('=') else { return ParsedRange::Unsatisfiable; };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return ParsedRange::Unsatisfiable;
    }
    // SSPI serves a full response for multi-range requests.
    if value.contains(',') {
        return ParsedRange::Full;
    }
    let value = value.trim();
    let Some((left, right)) = value.split_once('-') else { return ParsedRange::Unsatisfiable; };
    if length == 0 {
        return ParsedRange::Unsatisfiable;
    }
    if left.is_empty() {
        let Ok(suffix) = right.parse::<u64>() else { return ParsedRange::Unsatisfiable; };
        if suffix == 0 {
            return ParsedRange::Unsatisfiable;
        }
        let count = suffix.min(length);
        return ParsedRange::Partial { start: length - count, end: length - 1 };
    }
    let Ok(start) = left.parse::<u64>() else { return ParsedRange::Unsatisfiable; };
    if start >= length {
        return ParsedRange::Unsatisfiable;
    }
    let end = if right.is_empty() {
        length - 1
    } else {
        let Ok(end) = right.parse::<u64>() else { return ParsedRange::Unsatisfiable; };
        if end < start {
            return ParsedRange::Unsatisfiable;
        }
        end.min(length - 1)
    };
    ParsedRange::Partial { start, end }
}

async fn send_bytes(
    stream: &mut TcpStream,
    status: u16,
    extra: &str,
    body: &[u8],
    content_length: u64,
    head: bool,
) -> tokio::io::Result<()> {
    send_headers(stream, status, extra, content_length).await?;
    if !head && !body.is_empty() {
        write_response(stream, body, WRITE_TIMEOUT).await?;
    }
    Ok(())
}

async fn send_headers(stream: &mut TcpStream, status: u16, extra: &str, content_length: u64) -> tokio::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        416 => "Range Not Satisfiable",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\n{extra}Content-Length: {content_length}\r\nConnection: close\r\n\r\n"
    );
    write_response(stream, headers.as_bytes(), WRITE_TIMEOUT).await
}

// `idle` bounds each wait for the peer to accept more bytes, not the whole body: a slow
// but steady reader keeps its connection, one that accepts nothing for `idle` loses it.
async fn write_response<W: AsyncWrite + Unpin>(stream: &mut W, bytes: &[u8], idle: Duration) -> tokio::io::Result<()> {
    let mut written = 0;
    while written < bytes.len() {
        match timeout(idle, stream.write(&bytes[written..])).await {
            Ok(Ok(0)) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(Ok(count)) => written += count,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "PKG response write stalled")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{fs, net::{IpAddr, Ipv4Addr, SocketAddr}};
    use uuid::Uuid;

    // One process-wide server, shared with the receiver tests; it must outlive each test runtime.
    fn test_port() -> u16 {
        crate::pkg_server_test_port()
    }

    async fn start_server() -> u16 {
        let port = test_port();
        ensure_started(port).await.unwrap();
        port
    }

    fn test_package() -> (PathBuf, Vec<u8>) {
        let mut bytes = vec![0u8; 0x1000];
        bytes[..4].copy_from_slice(b"\x7fCNT");
        bytes[0x40..0x40 + 36].copy_from_slice(b"UP0000-CUSA12345_00-ABCDEFGHIJKLMNOP");
        let len = bytes.len() as u64;
        bytes[0x430..0x438].copy_from_slice(&len.to_be_bytes());
        let digest = Sha256::digest(&bytes[..0xfe0]);
        bytes[0xfe0..].copy_from_slice(&digest);
        let path = crate::test_output_root().join(format!("sspi-pkg-server-{}.pkg", Uuid::new_v4().simple()));
        fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    fn tiny_png() -> Vec<u8> {
        // A compact one-pixel PNG used only by this route test.
        use crate::pkg_meta::is_valid_png;
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = vec![0u8; 13];
        ihdr[..4].copy_from_slice(&1u32.to_be_bytes());
        ihdr[4..8].copy_from_slice(&1u32.to_be_bytes());
        ihdr[8] = 8;
        ihdr[9] = 6;
        add_png_chunk(&mut png, b"IHDR", &ihdr);
        add_png_chunk(&mut png, b"IDAT", &[0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00, 0x05, 0x00, 0x01]);
        add_png_chunk(&mut png, b"IEND", &[]);
        assert!(is_valid_png(&png));
        png
    }

    fn add_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = png_crc32(&png[png.len() - data.len() - 4..]);
        png.extend_from_slice(&crc.to_be_bytes());
    }

    fn png_crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= *byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    struct Response {
        status: u16,
        headers: String,
        body: Vec<u8>,
    }

    async fn read_response(stream: &mut TcpStream, head: bool) -> Response {
        let mut header = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
            if header.ends_with(b"\r\n\r\n") { break; }
            assert!(header.len() <= MAX_HEADER_BYTES);
        }
        let header_text = String::from_utf8(header).unwrap();
        let status = header_text.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        let length = header_text.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("Content-Length").then(|| value.trim().parse::<usize>().unwrap())
        }).unwrap();
        let mut body = vec![0u8; if head { 0 } else { length }];
        if !body.is_empty() {
            stream.read_exact(&mut body).await.unwrap();
        }
        Response { status, headers: header_text, body }
    }

    async fn request(path: &str, method: &str, range: Option<&str>) -> Response {
        let port = test_port();
        let mut stream = TcpStream::connect(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).await.unwrap();
        let mut headers = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if let Some(range) = range { headers.push_str(&format!("Range: {range}\r\n")); }
        headers.push_str("\r\n");
        stream.write_all(headers.as_bytes()).await.unwrap();
        read_response(&mut stream, method == "HEAD").await
    }

    #[test]
    fn range_parser_matches_single_and_suffix_ranges() {
        assert_eq!(parse_range(None, 10), ParsedRange::Full);
        assert_eq!(parse_range(Some("bytes=2-5"), 10), ParsedRange::Partial { start: 2, end: 5 });
        assert_eq!(parse_range(Some("bytes=5-"), 10), ParsedRange::Partial { start: 5, end: 9 });
        assert_eq!(parse_range(Some("bytes=-3"), 10), ParsedRange::Partial { start: 7, end: 9 });
        assert_eq!(parse_range(Some("bytes=-30"), 10), ParsedRange::Partial { start: 0, end: 9 });
        assert_eq!(parse_range(Some("bytes=2-30"), 10), ParsedRange::Partial { start: 2, end: 9 });
        assert_eq!(parse_range(Some("bytes=0-1,4-5"), 10), ParsedRange::Full);
        assert_eq!(parse_range(Some("bytes=10-"), 10), ParsedRange::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 10), ParsedRange::Unsatisfiable);
        assert_eq!(parse_range(Some("items=0-1"), 10), ParsedRange::Unsatisfiable);
    }

    #[tokio::test]
    async fn http_round_trips_ranges_icons_closes_and_activity() {
        let port = start_server().await;
        let (path, pkg) = test_package();
        let token = Uuid::new_v4().simple().to_string();
        let icon = tiny_png();
        let served = register(&token, &path, Some(icon.clone()), IpAddr::V4(Ipv4Addr::LOCALHOST), port).unwrap();
        let prefix = format!("/pkg/{token}");

        let full = request(&format!("{prefix}.pkg"), "GET", None).await;
        assert_eq!(full.status, 200);
        assert_eq!(full.body, pkg);
        assert!(full.headers.contains(&format!("Content-Length: {}", pkg.len())));
        assert!(full.headers.contains("Accept-Ranges: bytes"));

        let head = request(&format!("{prefix}.pkg"), "HEAD", None).await;
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());
        assert!(head.headers.contains(&format!("Content-Length: {}", pkg.len())));

        let partial = request(&format!("{prefix}.pkg"), "GET", Some("bytes=2-5")).await;
        assert_eq!(partial.status, 206);
        assert_eq!(partial.body, pkg[2..6]);
        assert!(partial.headers.contains("Content-Range: bytes 2-5/4096"));

        let suffix = request(&format!("{prefix}.pkg"), "GET", Some("bytes=-3")).await;
        assert_eq!(suffix.status, 206);
        assert_eq!(suffix.body, pkg[pkg.len() - 3..]);

        let unsatisfiable = request(&format!("{prefix}.pkg"), "GET", Some("bytes=4096-")).await;
        assert_eq!(unsatisfiable.status, 416);
        assert!(unsatisfiable.headers.contains("Content-Range: bytes */4096"));

        let unknown = request("/pkg/aaaaaaaaaaaaaaaa.pkg", "GET", None).await;
        assert_eq!(unknown.status, 404);
        let post = request(&format!("{prefix}.pkg"), "POST", None).await;
        assert_eq!(post.status, 405);

        // A keep-alive request still gets one response and an orderly close, never a reset.
        let mut reused = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        reused.write_all(format!("GET {prefix}.pkg.json HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\nX_Client-Tag: a.b\r\n\r\n").as_bytes()).await.unwrap();
        let manifest = read_response(&mut reused, false).await;
        assert_eq!(manifest.status, 200);
        assert!(manifest.headers.contains("Connection: close"));
        assert!(String::from_utf8_lossy(&manifest.body).contains(&served.package_url));
        let mut rest = Vec::new();
        assert_eq!(timeout(Duration::from_secs(5), reused.read_to_end(&mut rest)).await.unwrap().unwrap(), 0);

        let icon_response = request(&format!("/icon/{token}.png"), "GET", None).await;
        assert_eq!(icon_response.status, 200);
        assert!(icon_response.headers.contains("Content-Type: image/png"));
        assert_eq!(icon_response.body, icon);

        let activity = activity(&token).unwrap();
        assert!(activity.requests >= 8, "requests={}", activity.requests);
        assert!(activity.bytes_sent >= pkg.len() as u64 + icon.len() as u64);
        unregister(&token);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn pipelined_request_cannot_reset_away_the_first_response() {
        let port = start_server().await;
        let (path, pkg) = test_package();
        let token = Uuid::new_v4().simple().to_string();
        register(&token, &path, None, IpAddr::V4(Ipv4Addr::LOCALHOST), port).unwrap();
        let get = format!("GET /pkg/{token}.pkg HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(format!("{get}{get}").as_bytes()).await.unwrap();
        // Let the server close while the second request is still unread.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let response = read_response(&mut stream, false).await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body, pkg);
        let mut rest = Vec::new();
        assert_eq!(timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await.unwrap().unwrap(), 0);
        unregister(&token);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn slow_steady_reader_keeps_its_response() {
        // The reader takes 8 KiB about every 10-16 ms, so draining the MiB takes at least
        // two budgets while no single wait comes close to one. A write may time out only
        // if the reader really paused for a whole budget; when the machine itself stalls
        // that long, the attempt proves nothing and is repeated.
        let budget = Duration::from_millis(500);
        let body: Vec<u8> = (0..1024 * 1024).map(|n| (n % 251) as u8).collect();
        for _ in 0..3 {
            // A bounded pipe accepts partial writes, as a full TCP send buffer does on most stacks.
            let (mut writer, mut reader) = tokio::io::duplex(8 * 1024);
            let read = tokio::spawn(async move {
                let (mut received, mut chunk, mut longest, mut last) = (Vec::new(), vec![0u8; 8 * 1024], Duration::ZERO, Instant::now());
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let count = reader.read(&mut chunk).await.unwrap();
                    longest = longest.max(last.elapsed());
                    last = Instant::now();
                    if count == 0 { return (received, longest); }
                    received.extend_from_slice(&chunk[..count]);
                }
            });
            let started = Instant::now();
            let written = write_response(&mut writer, &body, budget).await;
            let elapsed = started.elapsed();
            drop(writer);
            let (received, longest) = read.await.unwrap();
            if longest + Duration::from_millis(50) >= budget { continue; }
            written.unwrap();
            assert_eq!(received, body);
            assert!(elapsed > budget, "the reader did not apply backpressure");
            return;
        }
        eprintln!("skipping: the machine stalled for a whole write budget in every attempt");
    }

    async fn private_server(limits: Limits) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(accept_loop(listener, limits));
        port
    }

    #[tokio::test]
    async fn full_server_queues_a_connection_then_answers_a_readable_503() {
        let (path, pkg) = test_package();
        let token = Uuid::new_v4().simple().to_string();
        register(&token, &path, None, IpAddr::V4(Ipv4Addr::LOCALHOST), 9).unwrap();
        let get = format!("GET /pkg/{token}.pkg HTTP/1.1\r\nHost: localhost\r\nRange: bytes=0-63\r\n\r\n");

        // A free slot is handed to the waiting connection.
        let port = private_server(Limits { connections: 1, queue: Duration::from_secs(5) }).await;
        let holder = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut waiting = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        waiting.write_all(get.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        drop(holder);
        let served = timeout(Duration::from_secs(5), read_response(&mut waiting, false)).await.unwrap();
        assert_eq!(served.status, 206);
        assert_eq!(served.body, pkg[..64]);

        // Without one, the client gets a readable 503 and an orderly close.
        let port = private_server(Limits { connections: 1, queue: Duration::from_millis(200) }).await;
        let _holder = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut refused = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        refused.write_all(get.as_bytes()).await.unwrap();
        let busy = timeout(Duration::from_secs(5), read_response(&mut refused, false)).await.unwrap();
        assert_eq!(busy.status, 503);
        assert!(busy.headers.contains("Retry-After: 1") && busy.headers.contains("Connection: close"));
        let mut rest = Vec::new();
        assert_eq!(timeout(Duration::from_secs(5), refused.read_to_end(&mut rest)).await.unwrap().unwrap(), 0);
        unregister(&token);
        let _ = fs::remove_file(path);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn locked_source_is_retried_instead_of_refused_for_good() {
        use std::os::windows::fs::OpenOptionsExt;
        let port = start_server().await;
        let (path, pkg) = test_package();
        let token = Uuid::new_v4().simple().to_string();
        register(&token, &path, None, IpAddr::V4(Ipv4Addr::LOCALHOST), port).unwrap();
        let lock = fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
        let busy = request(&format!("/pkg/{token}.pkg"), "GET", Some("bytes=0-63")).await;
        assert_eq!(busy.status, 503);
        assert!(busy.headers.contains("Retry-After"));
        drop(lock);
        let served = request(&format!("/pkg/{token}.pkg"), "GET", Some("bytes=0-63")).await;
        assert_eq!(served.status, 206);
        assert_eq!(served.body, pkg[..64]);
        assert!(!lock_state().tokens[&token].changed.load(Ordering::Relaxed));
        unregister(&token);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn trickling_header_cannot_extend_total_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let result = timeout(Duration::from_secs(1), read_request_with_timeout(&mut stream, &mut vec![], Duration::from_millis(100))).await.unwrap();
            assert!(matches!(result, Ok(None)));
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(b"GET / HTTP/1.1\r\nX-Slow: ").await.unwrap();
        let writer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(15)).await;
                if client.write_all(b"x").await.is_err() { break; }
            }
        });
        server.await.unwrap(); writer.abort();
    }

    #[tokio::test]
    async fn stalled_response_reader_hits_write_idle_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            timeout(Duration::from_secs(3), async {
                let buffer = vec![0; STREAM_BUFFER_BYTES];
                for _ in 0..128 {
                    match write_response(&mut stream, &buffer, Duration::from_millis(50)).await {
                        Ok(()) => {},
                        Err(error) => { assert_eq!(error.kind(), std::io::ErrorKind::TimedOut); return; },
                    }
                }
                panic!("stalled reader did not apply backpressure");
            }).await.unwrap();
        });
        let socket = tokio::net::TcpSocket::new_v4().unwrap(); socket.set_recv_buffer_size(1024).unwrap();
        let _reader = socket.connect(address).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn changed_or_missing_source_rejects_manifest_and_package_get_and_head() {
        let port = start_server().await;
        for missing in [false, true] {
            let (path, original) = test_package(); let token = Uuid::new_v4().simple().to_string();
            register(&token, &path, None, IpAddr::V4(Ipv4Addr::LOCALHOST), port).unwrap();
            assert!(is_served(&path));
            assert_eq!(request(&format!("/pkg/{token}.pkg.json"), "HEAD", None).await.status, 200);
            if missing { fs::remove_file(&path).unwrap(); }
            else {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                let mut bytes = original.clone(); bytes[0x200] ^= 1; fs::write(&path, bytes).unwrap();
                fs::OpenOptions::new().write(true).open(&path).unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified + Duration::from_secs(2))).unwrap();
                assert_eq!(fs::metadata(&path).unwrap().len(), original.len() as u64);
            }
            for suffix in [".pkg", ".pkg.json"] {
                for method in ["GET", "HEAD"] {
                    let response = request(&format!("/pkg/{token}{suffix}"), method, None).await;
                    assert_eq!(response.status, 409); assert!(response.body.is_empty());
                }
            }
            assert!(lock_state().tokens[&token].changed.load(Ordering::Relaxed));
            unregister(&token); let _ = fs::remove_file(path);
        }
    }
}
