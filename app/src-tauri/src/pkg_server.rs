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
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::{timeout, Duration},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const STREAM_BUFFER_BYTES: usize = 1024 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

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
impl Registered {
    fn matches(&self, metadata: Option<&std::fs::Metadata>) -> bool {
        let same = metadata.and_then(|m| FileIdentity::read(m).ok()) == Some(self.identity);
        if !same && !self.changed.swap(true, Ordering::Relaxed) {
            eprintln!("PS4 PKG source changed or disappeared; refusing to serve stale package data.");
        }
        same && !self.changed.load(Ordering::Relaxed)
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
    version: String,
    range: Option<String>,
    connection: Option<String>,
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
            return Err(format!("PKG server is already bound to port {bound_port}"));
        }
    }

    let listener = TcpListener::bind(("0.0.0.0", port)).await
        .map_err(|_| format!("Unable to bind the PKG server to port {port}; check Windows Firewall and port use"))?;
    {
        let mut state = lock_state();
        state.port = Some(port);
    }
    tokio::spawn(accept_loop(listener));
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

async fn accept_loop(listener: TcpListener) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(connection) => connection,
            Err(_) => break,
        };
        match permits.clone().try_acquire_owned() {
            Ok(permit) => {
                tokio::spawn(async move {
                    let _permit = permit;
                    serve_connection(&mut stream).await;
                });
            }
            Err(_) => {
                let _ = send_bytes(&mut stream, 503, "", &[], 0, false, true).await;
            }
        }
    }
}

async fn serve_connection(stream: &mut TcpStream) {
    let mut pending = Vec::with_capacity(1024);
    loop {
        let request = match read_request(stream, &mut pending).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(()) => {
                let _ = send_bytes(stream, 400, "", &[], 0, false, true).await;
                return;
            }
        };
        let close = request_close(&request);
        if handle_request(stream, request, close).await {
            return;
        }
    }
}

async fn read_request(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Result<Option<Request>, ()> {
    read_request_with_timeout(stream, pending, IDLE_TIMEOUT).await
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
    let text = std::str::from_utf8(header).map_err(|_| ())?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().ok_or(())?.split_whitespace();
    let method = first.next().ok_or(())?;
    let target = first.next().ok_or(())?;
    let version = first.next().ok_or(())?;
    if first.next().is_some() || (version != "HTTP/1.1" && version != "HTTP/1.0") {
        return Err(());
    }

    let mut range: Option<String> = None;
    let mut connection: Option<String> = None;
    let mut _host = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or(())?;
        if name.is_empty() || name.bytes().any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-') {
            return Err(());
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("Range") {
            range = Some(match range {
                Some(previous) => format!("{previous},{value}"),
                None => value.to_string(),
            });
        } else if name.eq_ignore_ascii_case("Connection") {
            connection = Some(match connection {
                Some(previous) => format!("{previous},{value}"),
                None => value.to_string(),
            });
        } else if name.eq_ignore_ascii_case("Host") {
            _host = Some(value.to_string());
        }
    }
    Ok(Request { method: method.to_string(), target: target.to_string(), version: version.to_string(), range, connection })
}

fn request_close(request: &Request) -> bool {
    let connection = request.connection.as_deref().unwrap_or("");
    let has = |wanted: &str| connection.split(',').any(|part| part.trim().eq_ignore_ascii_case(wanted));
    if has("close") {
        true
    } else if request.version == "HTTP/1.0" {
        !has("keep-alive")
    } else {
        false
    }
}

async fn handle_request(stream: &mut TcpStream, request: Request, close: bool) -> bool {
    let route = parse_route(&request.target);
    let registered = route.and_then(|(_, token)| lock_state().tokens.get(token).cloned());
    if request.method != "GET" && request.method != "HEAD" {
        if let Some(entry) = registered.as_ref() {
            touch_request(entry);
        }
        let _ = send_bytes(stream, 405, "Allow: GET, HEAD\r\n", &[], 0, false, true).await;
        return true;
    }

    let Some((kind, _)) = route else {
        let _ = send_bytes(stream, 404, "", &[], 0, request.method == "HEAD", close).await;
        return close;
    };
    let Some(entry) = registered else {
        let _ = send_bytes(stream, 404, "", &[], 0, request.method == "HEAD", close).await;
        return close;
    };
    touch_request(&entry);

    if matches!(kind, RouteKind::Manifest | RouteKind::Package)
        && !entry.matches(tokio::fs::metadata(&entry.path).await.ok().as_ref()) {
        let _ = send_bytes(stream, 409, "Cache-Control: no-store\r\n", &[], 0, request.method == "HEAD", true).await;
        return true;
    }

    match kind {
        RouteKind::Manifest => {
            let body = pkg_meta::manifest_json(&entry.meta, &entry.package_url);
            let body = body.into_bytes();
            let extra = "Content-Type: application/json\r\nCache-Control: no-store\r\n";
            if send_bytes(stream, 200, extra, &body, body.len() as u64, request.method == "HEAD", close).await.is_err() {
                return true;
            }
            if request.method != "HEAD" {
                add_sent(&entry, body.len() as u64);
            }
            close
        }
        RouteKind::Package => {
            serve_package(stream, &entry, request.range.as_deref(), request.method == "HEAD", close).await
        }
        RouteKind::Icon => {
            let Some(icon) = entry.icon.as_ref() else {
                let _ = send_bytes(stream, 404, "", &[], 0, request.method == "HEAD", close).await;
                return close;
            };
            if send_bytes(stream, 200, "Content-Type: image/png\r\n", icon, icon.len() as u64, request.method == "HEAD", close).await.is_err() {
                return true;
            }
            if request.method != "HEAD" {
                add_sent(&entry, icon.len() as u64);
            }
            close
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

async fn serve_package(stream: &mut TcpStream, entry: &Registered, range: Option<&str>, head: bool, close: bool) -> bool {
    let file_size = entry.meta.file_size;
    let (status, start, body_size, content_range) = match parse_range(range, file_size) {
        ParsedRange::Full => (200, 0, file_size, None),
        ParsedRange::Partial { start, end } => (206, start, end - start + 1, Some(format!("Content-Range: bytes {start}-{end}/{file_size}\r\n"))),
        ParsedRange::Unsatisfiable => {
            let extra = format!("Content-Range: bytes */{file_size}\r\nAccept-Ranges: bytes\r\n");
            let _ = send_bytes(stream, 416, &extra, &[], 0, head, close).await;
            return close;
        }
    };
    if head {
        let mut extra = String::from("Accept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nCache-Control: no-transform\r\n");
        if let Some(content_range) = content_range { extra.push_str(&content_range); }
        let _ = send_headers(stream, status, &extra, body_size, close).await;
        return close;
    }

    let mut file = match File::open(&entry.path).await {
        Ok(file) => file,
        Err(_) => {
            entry.matches(None);
            let _ = send_bytes(stream, 409, "Cache-Control: no-store\r\n", &[], 0, false, true).await;
            return true;
        }
    };
    if !entry.matches(file.metadata().await.ok().as_ref()) {
        let _ = send_bytes(stream, 409, "Cache-Control: no-store\r\n", &[], 0, false, true).await;
        return true;
    }
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        let _ = send_bytes(stream, 503, "Retry-After: 1\r\n", &[], 0, false, close).await;
        return close;
    }

    let mut extra = String::from("Accept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nCache-Control: no-transform\r\n");
    if let Some(content_range) = content_range { extra.push_str(&content_range); }
    if send_headers(stream, status, &extra, body_size, close).await.is_err() {
        return true;
    }

    let mut remaining = body_size;
    let mut buffer = vec![0u8; STREAM_BUFFER_BYTES];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let read = match file.read(&mut buffer[..wanted]).await {
            Ok(read) if read > 0 => read,
            _ => return true,
        };
        if write_response(stream, &buffer[..read], WRITE_TIMEOUT).await.is_err() {
            return true;
        }
        add_sent(entry, read as u64);
        remaining -= read as u64;
    }
    close
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
    close: bool,
) -> tokio::io::Result<()> {
    send_headers(stream, status, extra, content_length, close).await?;
    if !head && !body.is_empty() {
        write_response(stream, body, WRITE_TIMEOUT).await?;
    }
    Ok(())
}

async fn send_headers(stream: &mut TcpStream, status: u16, extra: &str, content_length: u64, close: bool) -> tokio::io::Result<()> {
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
    let connection = if close { "close" } else { "keep-alive" };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\n{extra}Content-Length: {content_length}\r\nConnection: {connection}\r\n\r\n"
    );
    write_response(stream, headers.as_bytes(), WRITE_TIMEOUT).await
}

async fn write_response(stream: &mut TcpStream, bytes: &[u8], idle: Duration) -> tokio::io::Result<()> {
    for chunk in bytes.chunks(STREAM_BUFFER_BYTES) {
        timeout(idle, stream.write_all(chunk)).await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "PKG response write stalled"))??;
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
    async fn http_round_trips_ranges_keepalive_icons_and_activity() {
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

        let mut keepalive = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        keepalive.write_all(format!("GET {prefix}.pkg.json HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n").as_bytes()).await.unwrap();
        let manifest = read_response(&mut keepalive, false).await;
        assert_eq!(manifest.status, 200);
        assert!(String::from_utf8_lossy(&manifest.body).contains(&served.package_url));
        keepalive.write_all(format!("GET {prefix}.pkg HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let second = read_response(&mut keepalive, false).await;
        assert_eq!(second.status, 200);
        assert_eq!(second.body, pkg);

        let icon_response = request(&format!("/icon/{token}.png"), "GET", None).await;
        assert_eq!(icon_response.status, 200);
        assert!(icon_response.headers.contains("Content-Type: image/png"));
        assert_eq!(icon_response.body, icon);

        let activity = activity(&token).unwrap();
        assert!(activity.requests >= 9, "requests={}", activity.requests);
        assert!(activity.bytes_sent >= (pkg.len() as u64) * 2 + icon.len() as u64);
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
