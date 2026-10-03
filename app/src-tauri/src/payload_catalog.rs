//! Uses Payload Manager's published repository format; downloads only import local copies.
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::OnceLock, time::{Duration, SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};
use crate::payloads::{self, PayloadEntry};

const CATALOG: &str = "https://itsplk.github.io/ps5-payloads-mirror/payloads.json";
const DAY_MS: u64 = 86_400_000;
const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct CatalogPayload {
    name: String,
    filename: String,
    url: String,
    source: String,
    description: String,
    version: String,
    category: String,
    last_update: String,
    checksum: String,
    installed: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CatalogResult {
    entries: Vec<CatalogPayload>,
    checked_at: u64,
    stale: bool,
    warning: Option<String>,
}
fn operation() -> &'static tokio::sync::Mutex<()> {
    static OP: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    OP.get_or_init(|| tokio::sync::Mutex::new(()))
}
fn root(app: &AppHandle) -> Result<PathBuf, String> { app.path().app_config_dir().map(|p| p.join("payload-catalog")).map_err(|e| e.to_string()) }
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
fn valid_url(raw: &str) -> bool { url::Url::parse(raw).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some() && u.username().is_empty() && u.password().is_none()) }
fn valid_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.is_empty() && name.len() <= 180 && !name.contains(['/', '\\', ':', '\0']) && !name.starts_with('.') && (lower.ends_with(".elf") || lower.ends_with(".bin"))
}
fn parse_catalog(bytes: &[u8]) -> Result<Vec<CatalogPayload>, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| "The upstream payload catalog is unreadable.".to_string())?;
    let items = value.as_array().or_else(|| value.get("payloads").and_then(|v| v.as_array())).ok_or("The upstream catalog has no payload list.")?;
    if items.len() > 2000 { return Err("The upstream payload catalog has too many entries.".into()); }
    let mut entries = Vec::new();
    for item in items {
        let Ok(mut entry) = serde_json::from_value::<CatalogPayload>(item.clone()) else { continue; };
        if !valid_filename(&entry.filename) || !valid_url(&entry.url) || entry.name.trim().is_empty() || entry.name.len() > 200 || entry.description.len() > 4000 { continue; }
        if !entry.checksum.is_empty() && (entry.checksum.len() != 64 || !entry.checksum.bytes().all(|b| b.is_ascii_hexdigit())) { continue; }
        if !valid_url(&entry.source) { entry.source.clear(); }
        entry.installed = false;
        if !entries.iter().any(|e: &CatalogPayload| e.filename == entry.filename) { entries.push(entry); }
    }
    if entries.is_empty() { return Err("The upstream catalog has no supported ELF or BIN payloads.".into()); }
    Ok(entries)
}
fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().user_agent("SSPI-Windows-Payload-Catalog").timeout(Duration::from_secs(90)).redirect(reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 || attempt.url().scheme() != "https" { attempt.stop() } else { attempt.follow() }
    })).build().map_err(|e| e.to_string())
}
async fn download(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().await.map_err(|e| format!("Could not reach the payload repository: {e}"))?.error_for_status().map_err(|e| format!("Payload repository download failed: {e}"))?;
    if response.content_length().is_some_and(|n| n > limit as u64) { return Err("The repository file exceeds its size limit.".into()); }
    let mut stream = response.bytes_stream(); let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await { let chunk = chunk.map_err(|e| e.to_string())?; if bytes.len() + chunk.len() > limit { return Err("The repository file exceeds its size limit.".into()); } bytes.extend_from_slice(&chunk); }
    Ok(bytes)
}
async fn catalog_at(app: &AppHandle, refresh: bool) -> Result<CatalogResult, String> {
    let root = root(app)?; let path = root.join("catalog.json");
    let cached = fs::read(&path).ok().and_then(|b| serde_json::from_slice::<CatalogResult>(&b).ok());
    let due = refresh || cached.as_ref().is_none_or(|c| now().saturating_sub(c.checked_at) >= DAY_MS);
    if !due { return Ok(cached.unwrap()); }
    let result = async { let bytes = download(&client()?, CATALOG, 4 * 1024 * 1024).await?; parse_catalog(&bytes) }.await;
    match result {
        Ok(entries) => {
            let catalog = CatalogResult { entries, checked_at: now(), stale: false, warning: None };
            fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            let temp = path.with_extension("json.tmp"); fs::write(&temp, serde_json::to_vec(&catalog).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?; fs::rename(temp, path).map_err(|e| e.to_string())?;
            Ok(catalog)
        }
        Err(error) => match cached { Some(mut cached) => { cached.stale = true; cached.warning = Some(format!("Showing the last saved catalog. {error}")); Ok(cached) }, None => Err(error) },
    }
}
#[tauri::command]
pub(super) async fn get_payload_catalog(app: AppHandle, refresh: bool) -> Result<CatalogResult, String> {
    let _operation = operation().lock().await;
    let mut catalog = catalog_at(&app, refresh).await?;
    let library = payloads::list_payloads(app)?;
    for entry in &mut catalog.entries { entry.installed = !entry.checksum.is_empty() && library.iter().any(|p| p.sha256.eq_ignore_ascii_case(&entry.checksum)); }
    Ok(catalog)
}
#[tauri::command]
pub(super) async fn download_catalog_payload(app: AppHandle, filename: String) -> Result<Vec<PayloadEntry>, String> {
    let _operation = operation().lock().await;
    let catalog = catalog_at(&app, false).await?;
    let entry = catalog.entries.iter().find(|e| e.filename == filename).ok_or("That payload is not in the current catalog. Refresh and try again.")?;
    if !valid_filename(&entry.filename) || !valid_url(&entry.url) { return Err("The catalog entry is invalid.".into()); }
    let bytes = download(&client()?, &entry.url, MAX_PAYLOAD).await?;
    if bytes.is_empty() || (filename.to_ascii_lowercase().ends_with(".elf") && !bytes.starts_with(b"\x7fELF")) { return Err("The repository returned an invalid payload file.".into()); }
    let hash = crate::sha256_hex(&bytes);
    if !entry.checksum.is_empty() && !hash.eq_ignore_ascii_case(&entry.checksum) { return Err("The payload SHA-256 did not match the catalog. Nothing was imported.".into()); }
    let existing = payloads::list_payloads(app.clone())?;
    if existing.iter().any(|payload| payload.sha256 == hash) { return Ok(existing); }
    let folder = root(&app)?.join(uuid::Uuid::new_v4().to_string()); fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join(&entry.filename);
    let result = (|| {
        fs::write(&path, bytes).map_err(|e| e.to_string())?;
        let entries = payloads::add_payloads(app.clone(), vec![path.to_string_lossy().into_owned()], "ps5".into())?;
        if let Some(imported) = entries.iter().find(|p| p.sha256 == hash) {
            let name: String = format!("{} {}", entry.name, entry.version).trim().chars().take(80).collect();
            let notes = format!("Payload Manager catalog; {}. {}", if entry.checksum.is_empty() { "no published checksum" } else { "SHA-256 verified" }, entry.source).chars().take(500).collect();
            payloads::update_payload(app.clone(), imported.id.clone(), Some(name), None, Some(notes))
        } else { Ok(entries) }
    })();
    let _ = fs::remove_file(path); let _ = fs::remove_dir(folder);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upstream_array_and_documented_object_catalogs_work() {
        let entry = r#"{"name":"FTP Server","filename":"ftpsrv.elf","url":"https://example.test/ftpsrv.elf","version":"v1","last_update":"2026-10-01","category":"Networking & Servers"}"#;
        assert_eq!(parse_catalog(format!("[{entry}]").as_bytes()).unwrap()[0].version, "v1");
        assert_eq!(parse_catalog(format!("[{entry}]").as_bytes()).unwrap()[0].last_update, "2026-10-01");
        assert_eq!(parse_catalog(format!("{{\"name\":\"repo\",\"payloads\":[{entry}]}}").as_bytes()).unwrap().len(), 1);
    }
    #[test]
    fn invalid_paths_schemes_and_checksums_are_rejected() {
        for name in ["../a.elf", "c:\\a.elf", "/a.bin", "a.js", ".hidden.elf"] { assert!(!valid_filename(name)); }
        assert!(!valid_url("file:///a.elf")); assert!(!valid_url("https://user:secret@example.test/a.elf"));
        assert!(parse_catalog(br#"[{"name":"X","filename":"a.elf","url":"https://example.test/a.elf","checksum":"bad"}]"#).is_err());
    }
}
