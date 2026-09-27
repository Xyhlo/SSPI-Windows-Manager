use super::*;
use std::sync::OnceLock;

struct Update { endpoint: ReceiverEndpoint, progress: Progress }
static PS4_ART: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
static LAST: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
pub(super) fn cache_ps4_artwork(job: &str, bytes: Vec<u8>) {
    let mut art = PS4_ART.get_or_init(Default::default).lock().unwrap();
    if art.len() > 32 { art.clear(); }
    art.insert(job.into(), bytes);
    LAST.get_or_init(Default::default).lock().unwrap().remove(job);
}


// One bounded worker keeps notification I/O off the transfer and UI paths.
pub(super) fn observe(app: &AppHandle, p: &Progress) {
    if !matches!(p.stage.as_str(), "downloading" | "extracting" | "packaging" | "uploading" | "installing" | "mounting" | "complete" | "failed" | "cancelled") || !title_id(&p.title_id) { return; }
    let Some(state) = app.try_state::<AppState>() else { return; };
    if state.retry.lock().unwrap().records.get(&p.job_id).is_some_and(|r| r.package_only) { return; }
    let mut last = LAST.get_or_init(Default::default).lock().unwrap();
    let stage = format!("{}:{}", p.stage, p.paused);
    if last.get(&p.job_id).is_some_and(|(old, time)| old == &stage && (terminal_stage(&p.stage) || time.elapsed() < Duration::from_secs(30))) { return; }
    if last.len() > 512 { last.retain(|_, (_, time)| time.elapsed() < Duration::from_secs(3600)); }
    static TX: OnceLock<tokio::sync::mpsc::Sender<Update>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Update>(64);
        tauri::async_runtime::spawn(async move {
            let mut art = HashMap::<String, Vec<u8>>::new();
            let mut last_speed = HashMap::<String, f64>::new();
            while let Some(update) = rx.recv().await {
                if update.progress.speed_bps > 0. { last_speed.insert(update.progress.job_id.clone(), update.progress.speed_bps); }
                let speed = last_speed.get(&update.progress.job_id).copied().unwrap_or_default();
                let _ = tokio::time::timeout(Duration::from_secs(8), send(&update, speed, &mut art)).await;
                if terminal_stage(&update.progress.stage) { last_speed.remove(&update.progress.job_id); }
            }
        });
        tx
    });
    let settings = state.settings.lock().unwrap().clone();
    let endpoint = if p.target == "ps4" { ReceiverEndpoint::ps4(&settings) } else { ReceiverEndpoint::ps5(&settings) };
    if tx.try_send(Update { endpoint, progress: p.clone() }).is_ok() {
        last.insert(p.job_id.clone(), (stage, Instant::now()));
    }
}

fn status(p: &Progress, speed: f64) -> String {
    let label = if p.paused { "Paused" } else { match p.stage.as_str() {
        "downloading" => if p.bytes_done == 0 { "Download started" } else { "Download in progress" }, "extracting" => "Extraction in progress", "packaging" => "Packaging in progress",
        "uploading" => if p.target == "ps4" { "Sending to PS4" } else { "Sending to PS5" }, "installing" => "Installing", "mounting" => "Preparing game", "complete" => "Installation completed",
        "failed" => "Needs attention — check SSPI", "cancelled" => "Transfer cancelled", _ => "Preparing download",
    }};
    let mut text = label.to_owned();
    if !terminal_stage(&p.stage) && p.bytes_total > 0 {
        text.push_str(&format!(" · {:.0}%", (p.bytes_done as f64 / p.bytes_total as f64 * 100.).clamp(0., 100.)));
    }
    if speed.is_finite() && speed > 0. && !terminal_stage(&p.stage) { text.push_str(&format!(" · Last reported {:.1} MB/s", speed / 1_000_000.)); }
    text
}

fn payload(p: &Progress, speed: f64, icon: bool) -> String {
    let icon = if icon { serde_json::json!({"type":"Url", "parameters":{"url":format!("/data/SSPI/artwork/{}.png",p.title_id)}}) }
        else { serde_json::json!({"type":"Predefined","parameters":{"icon":"download"}}) };
    let name: String = p.title.chars().filter(|c| !c.is_control()).take(160).collect();
    let hash = p.job_id.bytes().fold(2166136261u32, |hash, b| (hash ^ b as u32).wrapping_mul(16777619));
    // The notification API's isLogged argument is false for every progress toast.
    serde_json::json!({"rawData":{"viewTemplateType":"InteractiveToastTemplateB", "channelType":"Downloads", "useCaseId":"IDC", "toastOverwriteType":"No", "isImmediate":true,"priority":100,
        "viewData":{"icon":icon,"message":{"body": if name.is_empty() { p.title_id.clone() } else { name }},"subMessage":{"body":status(p,speed)}}},
        "localNotificationId":format!("{hash}")}).to_string()
}

fn image_bytes(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() <= 512 * 1024 && (bytes.starts_with(b"\x89PNG\r\n\x1a\n") || bytes.starts_with(b"\xff\xd8\xff")) { bytes } else { vec![] }
}

fn png_artwork(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_COVER_BYTES { return None; }
    if bytes.len() <= 512 * 1024 && pkg_meta::is_valid_png(bytes) { return Some(bytes.to_vec()); }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192); limits.max_image_height = Some(8192); limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    for size in [512, 256, 128] {
        let mut png = std::io::Cursor::new(Vec::new());
        decoded.thumbnail(size, size).write_to(&mut png, image::ImageFormat::Png).ok()?;
        let bytes = png.into_inner();
        if bytes.len() <= 512 * 1024 { return Some(bytes); }
    }
    None
}

pub(super) async fn artwork(source: Option<&str>, icon0: Option<&[u8]>) -> Option<Vec<u8>> {
    if let Some(native) = icon0 {
        let native = native.to_vec();
        if let Some(png) = tokio::task::spawn_blocking(move || png_artwork(&native)).await.ok().flatten() { return Some(png); }
    }
    let source = source.unwrap_or("");
    let bytes = if let Some((_, data)) = source.split_once(',').filter(|(h, _)| h.starts_with("data:image/") && h.ends_with(";base64")) {
        if data.len() as u64 > MAX_COVER_BYTES * 2 { vec![] } else { BASE64.decode(data).unwrap_or_default() }
    } else if valid_http(source) {
        let fetch = async {
            let client = Client::builder().timeout(Duration::from_secs(4)).build().ok()?;
            let mut response = client.get(source).send().await.ok()?.error_for_status().ok()?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.ok()? {
                if (bytes.len() + chunk.len()) as u64 > MAX_COVER_BYTES { return None; }
                bytes.extend_from_slice(&chunk);
            }
            Some(bytes)
        };
        fetch.await.unwrap_or_default()
    } else { vec![] };
    tokio::task::spawn_blocking(move || png_artwork(&bytes)).await.ok().flatten()
}

pub(super) async fn prime_ps4(endpoint: &ReceiverEndpoint, progress: Progress, image: Vec<u8>) -> Result<(), String> {
    cache_ps4_artwork(&progress.job_id, image.clone());
    let mut art = HashMap::from([(progress.job_id.clone(), image)]);
    tokio::time::timeout(Duration::from_secs(8), send(&Update { endpoint: endpoint.clone(), progress }, 0., &mut art)).await
        .map_err(|_| "PS4 artwork transfer timed out".to_string())?
}

async fn send(update: &Update, speed: f64, art: &mut HashMap<String, Vec<u8>>) -> Result<(), String> {
    let endpoint = &update.endpoint; let p = &update.progress;
    let mut socket = TcpStream::connect((endpoint.host.as_str(), endpoint.port)).await.map_err(redact)?;
    let (_, reply) = frame(&mut socket, 0x53, &[]).await?;
    let config: Value = serde_json::from_slice(&reply).map_err(redact)?;
    if !config["capabilities"].as_array().is_some_and(|caps| caps.iter().any(|c| c == "progress-notifications")) { return Ok(()); }
    let key = if endpoint.console == "PS4" { p.job_id.clone() } else { p.icon.clone().unwrap_or_default() };
    if endpoint.console == "PS4" {
        if let Some(bytes) = PS4_ART.get_or_init(Default::default).lock().unwrap().get(&p.job_id).cloned() { art.insert(key.clone(), bytes); }
    }
    if !art.contains_key(&key) {
        let source = p.icon.as_deref().unwrap_or("");
        let bytes = if let Some(data) = source.strip_prefix("data:image/png;base64,").or_else(|| source.strip_prefix("data:image/png;sspi-case=1;base64,")).or_else(|| source.strip_prefix("data:image/jpeg;base64,")) {
            image_bytes(BASE64.decode(data).unwrap_or_default())
        } else if valid_http(source) {
            let client = Client::builder().timeout(Duration::from_secs(4)).build().map_err(redact)?;
            let mut response = client.get(source).send().await.map_err(redact)?.error_for_status().map_err(redact)?;
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(redact)? { if bytes.len() + chunk.len() > 512*1024 { bytes.clear(); break; } bytes.extend_from_slice(&chunk); }
            image_bytes(bytes)
        } else { vec![] };
        if art.len() > 32 { art.clear(); }
        art.insert(key.clone(), bytes);
    }
    let image = &art[&key];
    let json = payload(p, speed, !image.is_empty());
    let mut body = vec![u8::from(p.stage == "complete")];
    body.extend_from_slice(p.title_id.as_bytes()); body.push(0);
    body.extend_from_slice(&(image.len() as u32).to_le_bytes()); body.extend_from_slice(image);
    body.extend_from_slice(json.as_bytes()); body.push(0);
    let (code, reply) = frame(&mut socket, 0x58, &body).await?;
    if code != 1 { return Err(String::from_utf8_lossy(&reply).into()); }
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn progress_payload_escapes_names_and_reports_measured_speed() {
        let p = Progress { job_id:"abc".into(), title_id:"PPSA03016".into(), title:"Title \"with quotes\"".into(), stage:"extracting".into(), bytes_done:50, bytes_total:100, ..Default::default() };
        let value: Value = serde_json::from_str(&payload(&p, 120_000_000., true)).unwrap();
        assert_eq!(value["rawData"]["viewData"]["message"]["body"], p.title);
        let text = value["rawData"]["viewData"]["subMessage"]["body"].as_str().unwrap();
        assert!(text.contains("50%") && text.contains("120.0 MB/s"));
        assert!(!status(&Progress { stage:"failed".into(), ..p.clone() }, 0.).contains("completed"));
        assert_eq!(status(&Progress { stage:"complete".into(), ..p }, 0.), "Installation completed");
    }
    #[test] fn rejects_nonimage_and_oversized_artwork() {
        assert!(image_bytes(b"<html>error</html>".to_vec()).is_empty());
        assert!(image_bytes(vec![0; 512*1024+1]).is_empty());
        assert_eq!(image_bytes(b"\x89PNG\r\n\x1a\ncontent".to_vec()).len(),15);
    }
    #[tokio::test] async fn ps4_artwork_normalizes_catalog_jpeg_and_falls_back_to_pkg_png() {
        let original = image::DynamicImage::new_rgb8(8, 8);
        let mut jpeg = std::io::Cursor::new(Vec::new()); original.write_to(&mut jpeg, image::ImageFormat::Jpeg).unwrap();
        let source = format!("data:image/jpeg;base64,{}", BASE64.encode(jpeg.into_inner()));
        let art = artwork(Some(&source), None).await.unwrap(); assert!(pkg_meta::is_valid_png(&art)); assert!(art.len() <= 512*1024);
        let fallback = artwork(Some("data:image/png;base64,bm90IGFuIGltYWdl"), Some(&art)).await.unwrap(); assert_eq!(fallback, art);
        assert_eq!(artwork(Some(&format!("data:image/png;base64,{}", BASE64.encode(&art))), None).await.unwrap(), art);
    }
    #[tokio::test] async fn ps4_artwork_prefers_native_icon_and_uses_catalog_only_as_fallback() {
        let image = image::DynamicImage::new_rgb8(8,8);
        let mut bytes = std::io::Cursor::new(Vec::new()); image.write_to(&mut bytes,image::ImageFormat::Png).unwrap(); let native = bytes.into_inner();
        let mut jpeg = std::io::Cursor::new(Vec::new()); image::DynamicImage::new_rgb8(16,24).write_to(&mut jpeg,image::ImageFormat::Jpeg).unwrap();
        let catalog = format!("data:image/jpeg;base64,{}", BASE64.encode(jpeg.into_inner()));
        assert_eq!(artwork(Some(&catalog),Some(&native)).await.unwrap(),native);
        assert_eq!(artwork(Some("http://127.0.0.1:1/not-fetched"),Some(&native)).await.unwrap(),native);
        let fallback = artwork(Some(&catalog),Some(b"invalid icon0")).await.unwrap();
        assert!(pkg_meta::is_valid_png(&fallback)); assert_ne!(fallback,native); assert_eq!(fallback,artwork(Some(&catalog),None).await.unwrap());
    }
    #[tokio::test] async fn wire_marks_only_completion_as_logged_and_transports_artwork() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for logged in [0u8, 1] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = [0;5]; socket.read_exact(&mut header).await.unwrap();
                assert_eq!(header[0],0x53);
                let config = br#"{"capabilities":["progress-notifications"]}"#;
                socket.write_all(&[3]).await.unwrap(); socket.write_all(&(config.len() as u32).to_le_bytes()).await.unwrap(); socket.write_all(config).await.unwrap();
                socket.read_exact(&mut header).await.unwrap(); assert_eq!(header[0],0x58);
                let mut body = vec![0;u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize]; socket.read_exact(&mut body).await.unwrap();
                assert_eq!(body[0],logged); assert_eq!(&body[1..11],b"PPSA03016\0");
                let image_size=u32::from_le_bytes(body[11..15].try_into().unwrap()) as usize;
                assert_eq!(&body[15..15+image_size],b"\x89PNG\r\n\x1a\ncontent");
                let value:Value=serde_json::from_slice(&body[15+image_size..body.len()-1]).unwrap();
                assert!(value["rawData"]["viewData"]["icon"]["parameters"]["url"].as_str().unwrap().contains("PPSA03016.png"));
                socket.write_all(&[1,2,0,0,0,b'O',b'K']).await.unwrap();
            }
        });
        let mut art=HashMap::new();
        for stage in ["extracting","complete"] {
            let update=Update { endpoint: ReceiverEndpoint::ps5(&Settings {ps5_host:"127.0.0.1".into(),ps5_port:port,..Settings::default()}), progress: Progress {
                title_id:"PPSA03016".into(),job_id:"test".into(),stage:stage.into(),icon:Some(format!("data:image/png{};base64,{}",if stage == "complete" { ";sspi-case=1" } else { "" },BASE64.encode(b"\x89PNG\r\n\x1a\ncontent"))),..Default::default()} };
            send(&update,0.,&mut art).await.unwrap();
        }
        server.await.unwrap();
    }
}
