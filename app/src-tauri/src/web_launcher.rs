//! Local DNS/HTTPS host for SSPI's bundled console interfaces. The Python host is read as data.
use base64::Engine;
use serde::Serialize;
use std::{collections::BTreeMap, io::{Cursor, Read}, net::{IpAddr, Ipv4Addr, SocketAddr}, sync::{Arc, Mutex, OnceLock}, time::{Duration, SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, State};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, net::{TcpListener, UdpSocket}, sync::{watch, Semaphore}, task::JoinHandle, time::timeout};
use tokio_rustls::{rustls::{self, pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer}}, TlsAcceptor};
use crate::AppState;

const BUNDLED_HOST: &[u8] = include_bytes!("../resources/web-launcher/webkit-autoloader-host.py");
const LICENSE: &[u8] = include_bytes!("../resources/web-launcher/LICENSE");
const DNS_NAME: &str = "manuals.playstation.net";
const MAX_SOURCE: usize = 64 * 1024 * 1024;
const MAX_ASSETS: usize = 96 * 1024 * 1024;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebStatus {
    running: bool,
    phase: String,
    version: String,
    available_version: Option<String>,
    address: String,
    url: String,
    message: String,
    requests: u64,
    last_client: Option<String>,
    last_request_at: Option<u64>,
    logs: Vec<String>,
}
impl Default for WebStatus {
    fn default() -> Self { Self { running: false, phase: "idle".into(), version: String::new(), available_version: None, address: String::new(), url: String::new(), message: "Ready to host SSPI Web Launcher.".into(), requests: 0, last_client: None, last_request_at: None, logs: Vec::new() } }
}
struct Assets {
    version: String,
    files: BTreeMap<String, Arc<[u8]>>,
    tls: Arc<rustls::ServerConfig>,
}
#[derive(Default)]
struct Service {
    status: WebStatus,
    assets: Option<Arc<Assets>>,
    stop: Option<watch::Sender<bool>>,
    tasks: Vec<JoinHandle<()>>,
    generation: u64,
}
fn service() -> &'static Mutex<Service> {
    static SERVICE: OnceLock<Mutex<Service>> = OnceLock::new();
    SERVICE.get_or_init(|| Mutex::new(Service::default()))
}
fn operation() -> &'static tokio::sync::Mutex<()> {
    static OP: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    OP.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
fn log(status: &mut WebStatus, message: impl Into<String>) {
    status.logs.push(message.into());
    if status.logs.len() > 30 { status.logs.remove(0); }
}

fn python_string<'a>(source: &'a str, name: &str, triple: bool) -> Result<&'a str, String> {
    let prefix = format!("{name} = {}", if triple { "\"\"\"" } else { "\"" });
    let start = source.lines().find(|line| line.starts_with(&prefix)).ok_or_else(|| format!("Upstream host is missing {name}."))?;
    let offset = start.as_ptr() as usize - source.as_ptr() as usize + prefix.len();
    let rest = &source[offset..];
    rest.split_once(if triple { "\"\"\"" } else { "\"" }).map(|(value, _)| value).ok_or_else(|| format!("Upstream host has an invalid {name}."))
}
fn pem(source: &str, label: &str) -> Result<Vec<u8>, String> {
    let start = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body = source.split_once(&start).and_then(|(_, rest)| rest.split_once(&end)).map(|(body, _)| body).ok_or("The upstream HTTPS identity is incomplete.")?;
    base64::engine::general_purpose::STANDARD.decode(body.chars().filter(|c| !c.is_whitespace()).collect::<String>()).map_err(|_| "The upstream HTTPS identity is invalid.".into())
}
fn safe_asset_path(path: &str) -> bool {
    !path.is_empty() && path.len() < 512 && !path.starts_with('/') && !path.contains(['\\', ':', '\0']) && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.starts_with('.'))
}
fn parse_assets(bytes: &[u8]) -> Result<Assets, String> {
    if bytes.len() > MAX_SOURCE { return Err("The bundled launcher exceeds 64 MiB.".into()); }
    let source = std::str::from_utf8(bytes).map_err(|_| "The upstream host is not UTF-8.".to_string())?;
    let version = python_string(source, "VERSION", false)?.to_string();
    if version.len() > 80 || version.contains(['<', '>', '"', '\'', '&']) { return Err("The upstream version is invalid.".into()); }
    let block = source.split_once("\nEMBEDDED_ZIP_B64 = (").and_then(|(_, body)| body.split_once("\n)")).map(|(body, _)| body).ok_or("The upstream release has no portable asset archive.")?;
    let mut encoded = String::new();
    for line in block.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let chunk = line.strip_prefix('"').and_then(|s| s.strip_suffix('"')).ok_or("The upstream asset archive is malformed.")?;
        if !chunk.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c)) { return Err("The upstream asset archive is malformed.".into()); }
        encoded.push_str(chunk);
    }
    let zipped = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| "Could not decode upstream runtime assets.".to_string())?;
    let mut archive = zip::ZipArchive::new(Cursor::new(zipped)).map_err(|_| "Could not read upstream runtime assets.".to_string())?;
    if archive.len() > 2048 { return Err("The upstream archive contains too many files.".into()); }
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
        if file.is_dir() { continue; }
        let name = file.name().to_string();
        // The official host includes its empty overrides-directory marker.
        if name == ".gitkeep" { continue; }
        if !safe_asset_path(&name) || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) { return Err("The upstream archive contains an unsafe path.".into()); }
        let size = usize::try_from(file.size()).map_err(|_| "The upstream archive is too large.")?;
        total = total.checked_add(size).ok_or("The upstream archive is too large.")?;
        if total > MAX_ASSETS || size > 64 * 1024 * 1024 { return Err("The upstream archive is too large.".into()); }
        let mut data = Vec::with_capacity(size);
        (&mut file).take(size as u64 + 1).read_to_end(&mut data).map_err(|e| e.to_string())?;
        if data.len() != size || files.insert(name, Arc::<[u8]>::from(data)).is_some() { return Err("The upstream archive has inconsistent or duplicate files.".into()); }
    }
    for name in ["index.html", "app.js", "payloads/payload.elf"] {
        if !files.contains_key(name) { return Err(format!("The upstream runtime is missing {name}.")); }
    }
    if !files["payloads/payload.elf"].starts_with(b"\x7fELF") { return Err("The upstream installer is not an ELF payload.".into()); }
    let cert = pem(python_string(source, "SSL_CERT_PEM", true)?, "CERTIFICATE")?;
    let key = pem(python_string(source, "SSL_KEY_PEM", true)?, "PRIVATE KEY")?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider).with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12]).map_err(|e| e.to_string())?
        .with_no_client_auth().with_single_cert(vec![CertificateDer::from(cert)], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key))).map_err(|e| format!("Could not load upstream HTTPS identity: {e}"))?;
    Ok(Assets { version, files, tls: Arc::new(tls) })
}
fn ensure_assets(_app: &AppHandle) -> Result<Arc<Assets>, String> {
    let mut svc = service().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(assets) = &svc.assets { return Ok(assets.clone()); }
    // Older versions downloaded stock hosts here. Always use the SSPI edition
    // that was verified and installed together with this Windows application.
    let assets = parse_assets(BUNDLED_HOST)?;
    svc.status.version = assets.version.clone();
    let assets = Arc::new(assets); svc.assets = Some(assets.clone()); Ok(assets)
}

fn detect_address(console: &str) -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    let target = console.parse::<Ipv4Addr>().unwrap_or(Ipv4Addr::new(8, 8, 8, 8));
    socket.connect((target, 9)).ok()?;
    match socket.local_addr().ok()?.ip() { IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip), _ => None }
}

#[tauri::command]
pub(super) fn get_web_launcher(app: AppHandle, state: State<'_, AppState>) -> Result<WebStatus, String> {
    ensure_assets(&app)?;
    let console = state.settings.lock().unwrap_or_else(|p| p.into_inner()).ps5_host.clone();
    let mut svc = service().lock().unwrap_or_else(|p| p.into_inner());
    if !svc.status.running && svc.status.address.is_empty() {
        svc.status.address = detect_address(&console).map(|p| p.to_string()).unwrap_or_default();
    }
    Ok(svc.status.clone())
}

#[tauri::command]
pub(super) async fn start_web_launcher(app: AppHandle, state: State<'_, AppState>, address: Option<String>) -> Result<WebStatus, String> {
    let _operation = operation().lock().await;
    let assets = ensure_assets(&app)?;
    { let svc = service().lock().unwrap_or_else(|p| p.into_inner()); if svc.status.running { return Ok(svc.status.clone()); } }
    let console = state.settings.lock().unwrap_or_else(|p| p.into_inner()).ps5_host.clone();
    let ip = address.filter(|s| !s.trim().is_empty()).map(|s| s.trim().parse::<Ipv4Addr>().map_err(|_| "Enter this PC's local IPv4 address.".to_string())).transpose()?.or_else(|| detect_address(&console)).ok_or("Could not find this PC's LAN address. Enter it in Host details.")?;
    if ip.is_unspecified() || ip.is_loopback() || ip.is_broadcast() || ip.is_multicast() { return Err("Use this PC's reachable LAN IPv4 address.".into()); }
    std::net::UdpSocket::bind((ip, 0)).map_err(|_| "That address is not assigned to this PC. Use its LAN IPv4 address.".to_string())?;
    let result = bind_host(ip, assets).await;
    if let Err(error) = &result { let mut svc = service().lock().unwrap_or_else(|p| p.into_inner()); svc.status.phase = "error".into(); svc.status.message = error.clone(); log(&mut svc.status, error.clone()); }
    result
}

async fn bind_host(ip: Ipv4Addr, assets: Arc<Assets>) -> Result<WebStatus, String> {
    // No listeners are spawned until both required sockets have bound successfully.
    let dns = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 53)).await.map_err(|e| format!("DNS port 53 is unavailable: {e}. Close another DNS host and try again."))?;
    let https = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 443)).await.map_err(|e| format!("HTTPS port 443 is unavailable: {e}. Close another web host and try again."))?;
    let http = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 80)).await;
    let (stop, receiver) = watch::channel(false);
    let mut svc = service().lock().unwrap_or_else(|p| p.into_inner());
    svc.generation += 1;
    let generation = svc.generation;
    svc.status.running = true; svc.status.phase = "ready".into(); svc.status.address = ip.to_string();
    svc.status.url = format!("http://{ip}/"); svc.status.requests = 0; svc.status.last_client = None; svc.status.last_request_at = None;
    svc.status.message = "DNS and HTTPS are ready. Open User's Guide on your PS5.".into();
    log(&mut svc.status, format!("Hosting SSPI {} on DNS 53 / HTTPS 443 at {ip}.", assets.version));
    svc.tasks.push(tokio::spawn(dns_loop(dns, ip, receiver.clone(), generation)));
    svc.tasks.push(tokio::spawn(http_loop(https, Some(TlsAcceptor::from(assets.tls.clone())), assets.clone(), receiver.clone(), generation)));
    match http {
        Ok(listener) => svc.tasks.push(tokio::spawn(http_loop(listener, None, assets, receiver, generation))),
        Err(error) => { svc.status.url = format!("https://{ip}/"); log(&mut svc.status, format!("HTTP preview on port 80 is unavailable: {error}. The PS5 DNS / HTTPS flow is ready.")); }
    }
    svc.stop = Some(stop);
    Ok(svc.status.clone())
}

#[tauri::command]
pub(super) async fn stop_web_launcher() -> Result<WebStatus, String> {
    let _operation = operation().lock().await;
    let tasks = {
        let mut svc = service().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stop) = svc.stop.take() { let _ = stop.send(true); }
        svc.status.running = false; svc.status.phase = "idle".into(); svc.status.message = "Host stopped. Restore your PS5's previous DNS settings when finished.".into();
        std::mem::take(&mut svc.tasks)
    };
    for task in tasks { let _ = task.await; }
    Ok(service().lock().unwrap_or_else(|p| p.into_inner()).status.clone())
}

fn record(generation: u64, peer: SocketAddr, path: &str, failed: bool) {
    if peer.ip().is_loopback() { return; }
    let mut svc = service().lock().unwrap_or_else(|p| p.into_inner());
    if svc.generation != generation || !svc.status.running { return; }
    svc.status.requests += 1; svc.status.last_client = Some(peer.ip().to_string()); svc.status.last_request_at = Some(now());
    if failed { log(&mut svc.status, format!("{}: asset unavailable ({})", peer.ip(), path.chars().take(140).collect::<String>())); }
    else if path.starts_with("/launch/") || path.starts_with("/app/") {
        svc.status.phase = "loading".into(); svc.status.message = "Serving the upstream autoloader. Follow progress on your PS5.".into();
    } else if svc.status.phase != "loading" {
        svc.status.phase = "connected".into(); svc.status.message = "A browser reached the SSPI launcher. Select Start WebKit Autoloader on your PS5.".into();
    }
}

fn dns_response(query: &[u8], ip: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 || query.len() > 512 || query[2] & 0x80 != 0 || query[4..6] != [0, 1] { return None; }
    let mut cursor = 12; let mut labels = Vec::new();
    loop {
        let length = *query.get(cursor)? as usize; cursor += 1;
        if length == 0 { break; }
        if length > 63 { return None; }
        labels.push(std::str::from_utf8(query.get(cursor..cursor + length)?).ok()?.to_ascii_lowercase()); cursor += length;
    }
    let kind = u16::from_be_bytes(query.get(cursor..cursor + 2)?.try_into().ok()?);
    let class = u16::from_be_bytes(query.get(cursor + 2..cursor + 4)?.try_into().ok()?); cursor += 4;
    if class != 1 || cursor > 271 { return None; }
    let matches = labels.join(".") == DNS_NAME;
    let answer = matches && kind == 1;
    let mut response = Vec::from(&query[..2]);
    response.extend_from_slice(&[0x81, if matches { 0x80 } else { 0x83 }, 0, 1, 0, u8::from(answer), 0, 0, 0, 0]);
    response.extend_from_slice(&query[12..cursor]);
    if answer { response.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]); response.extend_from_slice(&ip.octets()); }
    Some(response)
}
async fn dns_loop(socket: UdpSocket, ip: Ipv4Addr, mut stop: watch::Receiver<bool>, generation: u64) {
    let mut buffer = [0u8; 512];
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            received = socket.recv_from(&mut buffer) => match received {
                Ok((length, peer)) => if let Some(response) = dns_response(&buffer[..length], ip) { let _ = socket.send_to(&response, peer).await; },
                Err(error) => { server_failed(generation, format!("DNS host stopped: {error}")); break; }
            }
        }
    }
}
fn server_failed(generation: u64, message: String) {
    let mut svc = service().lock().unwrap_or_else(|p| p.into_inner());
    if svc.generation == generation { svc.status.phase = "error".into(); svc.status.running = false; svc.status.message = message.clone(); log(&mut svc.status, message); if let Some(stop) = &svc.stop { let _ = stop.send(true); } }
}
async fn http_loop(listener: TcpListener, tls: Option<TlsAcceptor>, assets: Arc<Assets>, mut stop: watch::Receiver<bool>, generation: u64) {
    let concurrency = Arc::new(Semaphore::new(24));
    let mut clients = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            incoming = listener.accept() => match incoming {
                Ok((stream, peer)) => {
                    let Ok(permit) = concurrency.clone().try_acquire_owned() else { continue; };
                    let tls = tls.clone(); let assets = assets.clone();
                    clients.spawn(async move {
                        let _permit = permit;
                        let _ = timeout(Duration::from_secs(30), async move {
                            if let Some(tls) = tls {
                                if let Ok(stream) = tls.accept(stream).await { let _ = serve(stream, peer, assets, generation).await; }
                            } else { let _ = serve(stream, peer, assets, generation).await; }
                        }).await;
                    });
                }
                Err(error) => { server_failed(generation, format!("Web host stopped: {error}")); break; }
            }
        }
    }
    clients.abort_all(); while clients.join_next().await.is_some() {}
}

fn decode_path(raw: &str) -> Option<String> {
    let raw = raw.split('?').next()?.split('#').next()?;
    if !raw.starts_with('/') || raw.len() > 2048 { return None; }
    let bytes = raw.as_bytes(); let mut out = Vec::new(); let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = (*bytes.get(i + 1)? as char).to_digit(16)?; let low = (*bytes.get(i + 2)? as char).to_digit(16)?;
            out.push((high * 16 + low) as u8); i += 3;
        } else { out.push(bytes[i]); i += 1; }
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded.contains(['\\', '\0', ':', '%']) || decoded.chars().any(char::is_control) || decoded.split('/').any(|p| p == "." || p == ".." || p.starts_with('.')) { return None; }
    Some(decoded)
}
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") { "html" | "htm" => "text/html; charset=utf-8", "js" | "mjs" => "application/javascript; charset=utf-8", "css" => "text/css; charset=utf-8", "json" => "application/json", "svg" => "image/svg+xml", "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "woff" => "font/woff", "woff2" => "font/woff2", "txt" | "appcache" => "text/plain; charset=utf-8", _ => "application/octet-stream" }
}
fn route(raw: &str, assets: &Assets) -> (u16, &'static str, Arc<[u8]>) {
    let Some(path) = decode_path(raw) else { return (400, "text/plain", Arc::from(&b"Invalid path"[..])); };
    if path == "/" || path == "/index.html" || (path.starts_with("/document/") && path.ends_with("/ps5/index.html")) {
        return (200, "text/html; charset=utf-8", assets.files["index.html"].clone());
    }
    if path == "/upstream/LICENSE" { return (200, "text/plain; charset=utf-8", Arc::from(LICENSE)); }
    if path == "/health" { return (200, "text/plain", Arc::from(&b"SSPI Web Launcher"[..])); }
    let mut relative = path.trim_start_matches('/');
    if let Some((_, rest)) = relative.strip_prefix("document/").and_then(|s| s.split_once("/ps5/")) { relative = rest; }
    if let Some(rest) = relative.strip_prefix("launch/") { relative = rest; }
    else if let Some(rest) = relative.strip_prefix("app/") { relative = rest; }
    if relative.is_empty() { relative = "index.html"; }
    if relative == "selected_exploit" { return (200, "text/plain", assets.files.get(relative).cloned().unwrap_or_else(|| Arc::from(&b"relapse\n"[..]))); }
    match assets.files.get(relative) { Some(data) => (200, content_type(relative), data.clone()), None => (404, "text/plain", Arc::from(&b"Asset not found"[..])) }
}
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, peer: SocketAddr, assets: Arc<Assets>, generation: u64) -> std::io::Result<()> {
    let mut request = Vec::new(); let mut chunk = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).await?; if count == 0 { return Ok(()); }
        request.extend_from_slice(&chunk[..count]); if request.len() > 8192 { return Ok(()); }
    }
    let head = String::from_utf8_lossy(&request); let mut first = head.lines().next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default(); let path = first.next().unwrap_or_default();
    let (code, kind, body) = if matches!(method, "GET" | "HEAD") { route(path, &assets) } else { (405, "text/plain", Arc::from(&b"Method not allowed"[..])) };
    record(generation, peer, path, code >= 400);
    let reason = match code { 200 => "OK", 400 => "Bad Request", 404 => "Not Found", _ => "Method Not Allowed" };
    let headers = format!("HTTP/1.1 {code} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n", body.len());
    stream.write_all(headers.as_bytes()).await?;
    if method != "HEAD" { stream.write_all(&body).await?; }
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_release_is_complete_and_preserves_assets() {
        let assets = parse_assets(BUNDLED_HOST).unwrap();
        assert!(!assets.version.is_empty()); assert!(assets.files.len() > 5);
        assert_eq!(route("/launch/app.js", &assets).2.as_ref(), assets.files["app.js"].as_ref());
        assert_eq!(route("/app/payloads/payload.elf", &assets).2.as_ref(), assets.files["payloads/payload.elf"].as_ref());
        assert_eq!(route("/document/en/ps5/index.html", &assets).0, 200);
        assert!(assets.version.contains("-sspi-"));
        assert_eq!(route("/", &assets).2.as_ref(), assets.files["index.html"].as_ref());
        assert_eq!(route("/document/en/ps5/style.css", &assets).2.as_ref(), assets.files["style.css"].as_ref());
        assert_eq!(route("/document/en/ps5/sspi-logo.png", &assets).2.as_ref(), assets.files["sspi-logo.png"].as_ref());
        assert!(String::from_utf8_lossy(&route("/", &assets).2).contains("SSPI Web Launcher"));
        let build: serde_json::Value = serde_json::from_slice(&assets.files["sspi-build.json"]).unwrap();
        assert_eq!(build["edition"], "sspi");
        assert_eq!(build["version"], assets.version);
        assert_eq!(build["payloads"]["installer.elf"], crate::sha256_hex(&assets.files["payloads/payload.elf"]));
        assert_eq!(build["payloads"]["elfldr-ps5.elf"], crate::sha256_hex(&assets.files["shared/elfldr-ps5.elf"]));
        assert_eq!(build["sourcesSha256"], crate::sha256_hex(&assets.files["sspi-launcher-sources.zip"]));
    }
    #[test]
    fn traversal_and_ambiguous_paths_cannot_access_files() {
        let assets = parse_assets(BUNDLED_HOST).unwrap();
        for path in ["/../host.py", "/%2e%2e/host.py", "/launch/%252e%252e/file", "/launch/a%5cb", "/C:/test", "/bad%00", "/%ff"] { assert_eq!(route(path, &assets).0, 400, "{path}"); }
        assert_eq!(route("/missing.elf", &assets).0, 404);
        assert!(safe_asset_path("shared/elfldr.elf")); assert!(!safe_asset_path("a/../file"));
    }
    fn query(name: &str, kind: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') { q.push(label.len() as u8); q.extend_from_slice(label.as_bytes()); }
        q.push(0); q.extend_from_slice(&kind.to_be_bytes()); q.extend_from_slice(&[0, 1]); q
    }
    #[test]
    fn dns_maps_guide_and_blocks_other_domains_without_forwarding() {
        let ip = Ipv4Addr::new(192, 168, 1, 5);
        let answer = dns_response(&query(DNS_NAME, 1), ip).unwrap(); assert_eq!(&answer[answer.len() - 4..], &ip.octets()); assert_eq!(answer[7], 1);
        let blocked = dns_response(&query("example.com", 1), ip).unwrap(); assert_eq!(blocked[3] & 15, 3); assert_eq!(blocked[7], 0);
        assert_eq!(dns_response(&query(DNS_NAME, 28), ip).unwrap()[7], 0);
        assert!(dns_response(&[0; 13], ip).is_none());
    }
    #[tokio::test]
    async fn fake_http_serves_assets_head_and_stops_cleanly() {
        let assets = Arc::new(parse_assets(BUNDLED_HOST).unwrap());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(); let addr = listener.local_addr().unwrap();
        let (stop, receiver) = watch::channel(false); let task = tokio::spawn(http_loop(listener, None, assets, receiver, 999));
        let client = reqwest::Client::new();
        let response = client.get(format!("http://{addr}/launch/app.js")).send().await.unwrap(); assert!(response.status().is_success()); assert!(response.bytes().await.unwrap().len() > 100);
        let response = client.head(format!("http://{addr}/")).send().await.unwrap(); assert!(response.status().is_success()); assert!(response.bytes().await.unwrap().is_empty());
        stop.send(true).unwrap(); timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    }
    #[tokio::test]
    async fn upstream_certificate_serves_https_and_dns_socket_is_released() {
        let assets = Arc::new(parse_assets(BUNDLED_HOST).unwrap());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(); let addr = listener.local_addr().unwrap();
        let dns = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(); let dns_addr = dns.local_addr().unwrap();
        let (stop, receiver) = watch::channel(false);
        let web = tokio::spawn(http_loop(listener, Some(TlsAcceptor::from(assets.tls.clone())), assets, receiver.clone(), 999));
        let dns = tokio::spawn(dns_loop(dns, Ipv4Addr::LOCALHOST, receiver, 999));
        let client = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
        assert_eq!(client.get(format!("https://{addr}/health")).send().await.unwrap().text().await.unwrap(), "SSPI Web Launcher");
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        socket.send_to(&query(DNS_NAME, 1), dns_addr).await.unwrap();
        let mut response = [0; 512]; let (count, _) = timeout(Duration::from_secs(2), socket.recv_from(&mut response)).await.unwrap().unwrap();
        assert_eq!(&response[count - 4..count], &[127, 0, 0, 1]);
        stop.send(true).unwrap();
        timeout(Duration::from_secs(2), web).await.unwrap().unwrap(); timeout(Duration::from_secs(2), dns).await.unwrap().unwrap();
        assert!(UdpSocket::bind(dns_addr).await.is_ok());
        assert!(TcpListener::bind(addr).await.is_ok());
    }
}
