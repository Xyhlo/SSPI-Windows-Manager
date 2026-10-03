//! SSPI community directory: authenticated envelopes, immutable archive snapshots,
//! and the same declarative archive validation used by local source installation.

use super::{load_registry, SourceSummary, MAX_ARCHIVE};
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use reqwest::{redirect, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use tauri::AppHandle;
use url::Url;

const API: &str = "https://amptis.com/SSPI/sources/api/";
// Public application identifier and envelope seed; neither is a user credential.
const APP_IDENTIFIER: &str = "SSPI-community-v1";
const ENVELOPE_SEED: &str = "SSPI community source envelope v1 · shared directory";
const MAX_METADATA: usize = 8192;
const MAX_PAGE_BYTES: usize = 400_000;
const MAX_PAGE_ENTRIES: usize = 30;
const MAX_PAGES: usize = 20;
const LIST_BUDGET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityEntry {
    pub id: String,
    pub source: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub size: u64,
    pub date: String,
    pub revision: u64,
    pub installed: bool,
}

#[derive(Debug, Serialize)]
pub struct CommunityListing {
    pub entries: Vec<CommunityEntry>,
    pub warning: Option<String>,
}

#[derive(Deserialize)]
struct DirectoryPage {
    items: Vec<Value>,
    #[serde(default)]
    next: String,
}

fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn maximum_encoded(maximum: usize) -> usize {
    (maximum + 80).div_ceil(3) * 4
}

fn open_envelope(encoded: &str, maximum: usize) -> Result<Vec<u8>, String> {
    if encoded.len() > maximum_encoded(maximum) {
        return Err("Community source envelope exceeds its size limit".into());
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid community source envelope")?;
    if bytes.len() < 65
        || bytes.len() > maximum + 80
        || bytes[0] != 1
        || (bytes.len() - 49) % 16 != 0
    {
        return Err("Invalid community source envelope".into());
    }
    let keys = Sha512::digest(ENVELOPE_SEED.as_bytes());
    let tag_at = bytes.len() - 32;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&keys[32..]).map_err(|_| "Invalid envelope key")?;
    mac.update(&bytes[..tag_at]);
    // Authenticate the complete version/IV/ciphertext before parsing padding or decrypting.
    mac.verify_slice(&bytes[tag_at..])
        .map_err(|_| "Community source authentication failed")?;
    let mut ciphertext = bytes[17..tag_at].to_vec();
    let plain = cbc::Decryptor::<aes::Aes256>::new_from_slices(&keys[..32], &bytes[1..17])
        .map_err(|_| "Invalid community source envelope")?
        .decrypt_padded_mut::<Pkcs7>(&mut ciphertext)
        .map_err(|_| "Invalid community source padding")?;
    if plain.len() > maximum {
        return Err("Community source exceeds its size limit".into());
    }
    Ok(plain.to_vec())
}

fn display_text(value: Option<&Value>, maximum: usize) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(maximum)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn unsigned(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
}

fn parse_entry(row: &Value) -> Result<CommunityEntry, String> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or("Invalid community source ID")?;
    let size = unsigned(row.get("size"))
        .filter(|size| *size > 0 && *size <= MAX_ARCHIVE as u64)
        .ok_or("Invalid community source size")?;
    let encoded = row
        .get("meta")
        .and_then(Value::as_str)
        .ok_or("Missing source metadata")?;
    let metadata: Value = serde_json::from_slice(&open_envelope(encoded, MAX_METADATA)?)
        .map_err(|_| "Invalid community source metadata")?;
    if metadata.get("id").and_then(Value::as_str) != Some(id) {
        return Err("Community source metadata identity mismatch".into());
    }
    let source = match row.get("source") {
        None | Some(Value::Null) => id,
        Some(Value::String(source)) if source.is_empty() => id,
        Some(Value::String(source)) if valid_id(source) => source,
        _ => return Err("Invalid community source permanent ID".into()),
    };
    let mut tags = Vec::new();
    if let Some(values) = metadata.get("tags").and_then(Value::as_array) {
        for value in values.iter().take(5) {
            let tag = display_text(Some(value), 24);
            if !tag.is_empty() && !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    let name = display_text(metadata.get("name"), 80);
    Ok(CommunityEntry {
        id: id.to_owned(),
        source: source.to_owned(),
        name: if name.is_empty() {
            "Community source".into()
        } else {
            name
        },
        description: display_text(
            metadata
                .get("description")
                .or_else(|| metadata.get("message")),
            500,
        ),
        tags,
        size,
        date: display_text(row.get("date"), 32),
        revision: unsigned(row.get("revision"))
            .unwrap_or(1)
            .clamp(1, 1_000_000),
        installed: false,
    })
}

fn parse_page(bytes: &[u8]) -> Result<(Vec<CommunityEntry>, String, usize), String> {
    if bytes.len() > MAX_PAGE_BYTES {
        return Err("Community directory page is too large".into());
    }
    let page: DirectoryPage =
        serde_json::from_slice(bytes).map_err(|_| "Invalid community directory response")?;
    if page.items.len() > MAX_PAGE_ENTRIES || (!page.next.is_empty() && !valid_id(&page.next)) {
        return Err("Invalid community directory page".into());
    }
    let mut entries = Vec::new();
    let mut skipped = 0;
    for row in page.items {
        match parse_entry(&row) {
            Ok(entry) => entries.push(entry),
            Err(_) => skipped += 1,
        }
    }
    Ok((entries, page.next, skipped))
}

fn allowed_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("amptis.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn client() -> Result<Client, String> {
    Client::builder()
        .https_only(true)
        .user_agent("GameSearch/0.1")
        .connect_timeout(Duration::from_secs(10))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 4 || !allowed_redirect(attempt.url()) {
                attempt.error("Community directory redirect refused")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| "Could not initialize the community directory connection".into())
}

async fn request(
    client: &Client,
    path: &str,
    maximum: usize,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(format!("{API}{path}"))
        .bearer_auth(APP_IDENTIFIER)
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "Community directory timed out. Try again."
            } else {
                "Could not connect to the community directory. Check the connection and try again."
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "Community directory returned HTTP {}. Try refreshing the list.",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|size| size > maximum as u64)
    {
        return Err("Community directory response exceeds its size limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Community directory response was interrupted")?
    {
        if chunk.len() > maximum.saturating_sub(bytes.len()) {
            return Err("Community directory response exceeds its size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn directory(client: &Client) -> Result<CommunityListing, String> {
    let started = Instant::now();
    let mut entries = Vec::new();
    let mut ids = HashSet::new();
    let mut cursors = HashSet::new();
    let mut after = String::new();
    let mut skipped = 0;
    let mut warnings = Vec::new();
    for page_index in 0..MAX_PAGES {
        let remaining = LIST_BUDGET.saturating_sub(started.elapsed());
        let result = if remaining.is_zero() {
            Err("Community directory listing timed out".to_owned())
        } else {
            request(
                client,
                &format!("list?after={after}"),
                MAX_PAGE_BYTES,
                remaining.min(Duration::from_secs(30)),
            )
            .await
            .and_then(|bytes| parse_page(&bytes))
        };
        let (page, next, invalid) = match result {
            Ok(value) => value,
            Err(error) if !entries.is_empty() => {
                warnings.push(error);
                break;
            }
            Err(error) => return Err(error),
        };
        skipped += invalid;
        for entry in page {
            if ids.insert(entry.id.clone()) {
                entries.push(entry);
            }
        }
        if next.is_empty() {
            break;
        }
        if !cursors.insert(next.clone()) {
            warnings.push("The directory repeated a page; showing entries received so far.".into());
            break;
        }
        if page_index + 1 == MAX_PAGES {
            warnings.push(
                "The directory reached the page limit; showing entries received so far.".into(),
            );
            break;
        }
        after = next;
    }
    if skipped > 0 {
        warnings.push(format!(
            "Skipped {skipped} invalid community source {}.",
            if skipped == 1 { "entry" } else { "entries" }
        ));
    }
    Ok(CommunityListing {
        entries,
        warning: if warnings.is_empty() {
            None
        } else {
            Some(warnings.join(" "))
        },
    })
}

fn mark_installed(entries: &mut [CommunityEntry], hashes: &HashSet<String>) {
    for entry in entries {
        entry.installed = hashes.contains(&entry.id);
    }
}

pub async fn list(app: &AppHandle) -> Result<CommunityListing, String> {
    let mut listing = directory(&client()?).await?;
    let hashes = load_registry(app)?
        .sources
        .into_iter()
        .map(|source| source.archive_sha256)
        .collect();
    mark_installed(&mut listing.entries, &hashes);
    Ok(listing)
}

fn verify_download(response: &[u8], entry: &CommunityEntry) -> Result<Vec<u8>, String> {
    #[derive(Deserialize)]
    struct FileEnvelope {
        blob: String,
    }
    if response.len() > maximum_encoded(MAX_ARCHIVE) + 1024 {
        return Err("Community source response exceeds its size limit".into());
    }
    let file: FileEnvelope =
        serde_json::from_slice(response).map_err(|_| "Invalid community source download")?;
    let bytes = open_envelope(&file.blob, MAX_ARCHIVE)?;
    if bytes.len() as u64 != entry.size || super::sha256(&bytes) != entry.id {
        return Err("Community source identity mismatch. Refresh the list and try again.".into());
    }
    Ok(bytes)
}

async fn download(client: &Client, entry: &CommunityEntry) -> Result<Vec<u8>, String> {
    let response = request(
        client,
        &format!("file/{}", entry.id),
        maximum_encoded(MAX_ARCHIVE) + 1024,
        Duration::from_secs(45),
    )
    .await?;
    verify_download(&response, entry)
}

pub async fn install(app: &AppHandle, id: &str) -> Result<Vec<SourceSummary>, String> {
    if !valid_id(id) {
        return Err("Invalid community source ID".into());
    }
    let client = client()?;
    // Resolve the selected immutable hash again. Never silently install a newer
    // revision if the directory changed while the user was choosing a source.
    let entry = directory(&client)
        .await?
        .entries
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or("This community source changed or is no longer listed. Refresh the list.")?;
    let bytes = download(&client, &entry).await?;
    // Provenance follows the permanent source. Downloading above used its exact hash.
    let origin = format!("{API}file/{}", entry.source);
    let app = app.clone();
    tokio::task::spawn_blocking(move || super::install_archive(&app, bytes, &origin))
        .await
        .map_err(|_| "Community source installation was interrupted")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    use serde_json::json;
    use std::{
        fs,
        io::{Cursor, Write},
        path::PathBuf,
    };

    fn seal(plain: &[u8]) -> String {
        let keys = Sha512::digest(ENVELOPE_SEED.as_bytes());
        let iv = [0x5a; 16];
        let cipher = cbc::Encryptor::<aes::Aes256>::new_from_slices(&keys[..32], &iv)
            .unwrap()
            .encrypt_padded_vec_mut::<Pkcs7>(plain);
        let mut bytes = vec![1];
        bytes.extend_from_slice(&iv);
        bytes.extend_from_slice(&cipher);
        let mut mac = Hmac::<Sha256>::new_from_slice(&keys[32..]).unwrap();
        mac.update(&bytes);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        STANDARD.encode(bytes)
    }

    fn row(id: &str, size: u64) -> Value {
        json!({
            "id": id, "source": "b".repeat(64), "size": size, "date": "2026-10-02", "revision": 2,
            "meta": seal(&serde_json::to_vec(&json!({"id":id,"name":"Global","message":"Shared source",
                "tags":["PS5","PS5","\u{0000}Games",null]})).unwrap())
        })
    }

    #[test]
    fn python_protocol_vector_and_authentication_before_decryption() {
        // Generated independently with Python cryptography and the public UTF-8 seed.
        let vector = "AQABAgMEBQYHCAkKCwwNDg8ehjNWi66RW7D/NVdK0J4HtYUNX5jFBKCnSEakLoMy4oG5Tx8cb3vCdWeSakFGaS3WPVwa10sOjvJHLW8GtnmI";
        assert_eq!(
            open_envelope(vector, MAX_METADATA).unwrap(),
            b"SSPI envelope fixture\0\xff\n"
        );
        let bytes = STANDARD.decode(vector).unwrap();
        for index in [1, 17, bytes.len() - 33, bytes.len() - 1] {
            let mut changed = bytes.clone();
            changed[index] ^= 1;
            assert_eq!(
                open_envelope(&STANDARD.encode(changed), MAX_METADATA).unwrap_err(),
                "Community source authentication failed"
            );
        }
        assert!(open_envelope(vector, 8).is_err());
        assert!(open_envelope("not base64", MAX_METADATA).is_err());
        for size in 0..bytes.len() {
            assert!(open_envelope(&STANDARD.encode(&bytes[..size]), MAX_METADATA).is_err());
        }
    }

    #[test]
    fn authenticated_padding_and_size_limits() {
        assert!(open_envelope(&seal(&vec![42; MAX_METADATA + 1]), MAX_METADATA).is_err());
        assert!(
            open_envelope(&"a".repeat(maximum_encoded(MAX_METADATA) + 1), MAX_METADATA).is_err()
        );
        let mut bytes = STANDARD.decode(seal(b"x")).unwrap();
        // For a one-block ciphertext, changing the last IV byte makes padding zero.
        // Recompute the tag to verify that authenticated invalid padding is still refused.
        bytes[16] ^= 15;
        let keys = Sha512::digest(ENVELOPE_SEED.as_bytes());
        let tag_at = bytes.len() - 32;
        let mut mac = Hmac::<Sha256>::new_from_slice(&keys[32..]).unwrap();
        mac.update(&bytes[..tag_at]);
        bytes[tag_at..].copy_from_slice(&mac.finalize().into_bytes());
        assert!(open_envelope(&STANDARD.encode(bytes), MAX_METADATA)
            .unwrap_err()
            .contains("padding"));
    }

    #[test]
    fn malformed_rows_do_not_hide_good_entries() {
        let id = "a".repeat(64);
        let good = row(&id, 100);
        let mut bad_identity = good.clone();
        bad_identity["id"] = json!("c".repeat(64));
        let mut bad_size = good.clone();
        bad_size["size"] = json!(MAX_ARCHIVE + 1);
        let mut bad_source = good.clone();
        bad_source["source"] = json!("../bad");
        let mut bad_meta = good.clone();
        bad_meta["meta"] = json!("not base64");
        let response = serde_json::to_vec(
            &json!({"items":[good,bad_identity,bad_size,bad_source,bad_meta],"next":""}),
        )
        .unwrap();
        let (entries, next, skipped) = parse_page(&response).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(skipped, 4);
        assert!(next.is_empty());
        assert_eq!(entries[0].description, "Shared source");
        assert_eq!(entries[0].tags, ["PS5", "Games"]);
        assert_eq!(entries[0].revision, 2);
        let mut old = row(&id, 100);
        old.as_object_mut().unwrap().remove("source");
        old["size"] = json!("100");
        assert_eq!(parse_entry(&old).unwrap().source, id);
    }

    #[test]
    fn pages_and_redirects_stay_bounded() {
        assert!(parse_page(
            &serde_json::to_vec(&json!({"items":[],"next":"https://example.invalid"})).unwrap()
        )
        .is_err());
        assert!(parse_page(
            &serde_json::to_vec(&json!({"items":vec![Value::Null; MAX_PAGE_ENTRIES + 1]})).unwrap()
        )
        .is_err());
        assert!(parse_page(&vec![b' '; MAX_PAGE_BYTES + 1]).is_err());
        assert!(allowed_redirect(&Url::parse(API).unwrap()));
        for bad in [
            "http://amptis.com/SSPI/sources/api/",
            "https://other.invalid/",
            "https://user@amptis.com/",
            "https://amptis.com:8443/",
            "https://amptis.com.evil.invalid/",
        ] {
            assert!(!allowed_redirect(&Url::parse(bad).unwrap()));
        }
        assert!(!valid_id(&"A".repeat(64)));
        assert!(!valid_id(&"0".repeat(65)));
        assert!(valid_id(&"0123456789abcdef".repeat(4)));
    }

    fn archive(extra: Option<(&str, &[u8])>) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("source.json", options).unwrap();
        zip.write_all(br#"{"schema":"gamesearch.source/v1","id":"community-fixture","name":"Fixture","version":"1.0","engine":{"type":"remote-api-v1"},"origins":["https://example.invalid"]}"#).unwrap();
        if let Some((name, contents)) = extra {
            zip.start_file(name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn exact_snapshot_then_existing_archive_validation() {
        let bytes = archive(None);
        let mut entry =
            parse_entry(&row(&super::super::sha256(&bytes), bytes.len() as u64)).unwrap();
        let response = serde_json::to_vec(&json!({"blob":seal(&bytes)})).unwrap();
        let verified = verify_download(&response, &entry).unwrap();
        let (_, descriptor, _) = super::super::validate_archive(verified).unwrap();
        assert_eq!(descriptor.id, "community-fixture");
        entry.size += 1;
        assert!(verify_download(&response, &entry).is_err());
        entry.size -= 1;
        entry.id = "f".repeat(64);
        assert!(verify_download(&response, &entry).is_err());
        for (name, contents) in [
            ("../escape.json", b"{}".as_slice()),
            ("run.exe", b"MZ".as_slice()),
            ("data.json", b"\x7fELF".as_slice()),
        ] {
            assert!(super::super::validate_archive(archive(Some((name, contents)))).is_err());
        }
        assert!(super::super::validate_archive(b"not a ZIP".to_vec()).is_err());
    }

    #[test]
    fn installed_hash_and_disabled_upgrade_selection_are_preserved() {
        let mut entries = vec![parse_entry(&row(&"a".repeat(64), 100)).unwrap()];
        mark_installed(&mut entries, &HashSet::from(["a".repeat(64)]));
        assert!(entries[0].installed);
        mark_installed(&mut entries, &HashSet::from(["b".repeat(64)]));
        assert!(!entries[0].installed); // A previous revision is not the selected archive.
        let mut registry = super::super::Registry::default();
        assert!(super::super::enabled_after_install(&registry, "fixture"));
        registry.sources.push(super::super::RegistryEntry {
            id: "fixture".into(),
            name: "Fixture".into(),
            description: String::new(),
            version: "1".into(),
            engine_type: "remote-api-v1".into(),
            enabled: false,
            trust: "unsigned-dev".into(),
            install_url: "old".into(),
            archive_sha256: "a".repeat(64),
        });
        assert!(!super::super::enabled_after_install(&registry, "fixture"));
        assert!(super::super::enabled_after_install(&registry, "another"));
        registry.sources[0].enabled = true;
        assert!(super::super::enabled_after_install(&registry, "fixture"));
    }

    #[tokio::test]
    #[ignore = "Read-only live Amptis directory/download check; writes fixtures only to Build-Output"]
    async fn live_directory_downloads_validate_without_installing() {
        let output = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../Build-Output/Windows Manager/community-source-tests")
            .join(format!("live-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&output).unwrap();
        let output = output.canonicalize().unwrap();
        let client = client().unwrap();
        let listing = directory(&client).await.unwrap();
        fs::write(
            output.join("listing.json"),
            serde_json::to_vec_pretty(&listing).unwrap(),
        )
        .unwrap();
        assert!(!listing.entries.is_empty(), "Live directory is empty");
        let mut records = Vec::new();
        let mut failures = Vec::new();
        for entry in listing.entries.iter().take(3) {
            let result = async {
                let bytes = download(&client, entry).await?;
                let archive_path = output.join(format!("{}.gssource", entry.id));
                fs::write(&archive_path, &bytes).map_err(|error| error.to_string())?;
                let (hash, descriptor, files) = super::super::validate_archive(bytes)?;
                let extracted = output.join(&entry.id);
                fs::create_dir_all(&extracted).map_err(|error| error.to_string())?;
                for (name, content) in &files {
                    let target = extracted.join(name);
                    fs::create_dir_all(target.parent().unwrap()).map_err(|error| error.to_string())?;
                    fs::write(target, content).map_err(|error| error.to_string())?;
                }
                if super::super::embedded_catalog(&descriptor.engine.engine_type) {
                    let declared = descriptor.files.iter().map(|file| (file.path.clone(), file.size, file.sha256.clone())).collect::<Vec<_>>();
                    crate::static_catalog::validate_installed(&extracted, &declared)?;
                }
                Ok::<_, String>(json!({"id":entry.id,"source":entry.source,"archive":archive_path,"sha256":hash,
                    "size":entry.size,"files":files.len(),"descriptorId":descriptor.id,
                    "version":descriptor.version,"engine":descriptor.engine.engine_type,"validated":true}))
            }.await;
            match result {
                Ok(record) => records.push(record),
                Err(error) => {
                    failures.push(error.clone());
                    records.push(json!({"id":entry.id,"error":error,"validated":false}));
                }
            }
        }
        let evidence = output.join("evidence.json");
        fs::write(&evidence, serde_json::to_vec_pretty(&json!({"status":if failures.is_empty(){"passed"}else{"failed"},
            "directoryEntries":listing.entries.len(),"warning":listing.warning,"records":records,"registryChanged":false})).unwrap()).unwrap();
        println!("Live community evidence: {}", evidence.display());
        assert!(
            failures.is_empty(),
            "Live source validation failed: {}",
            failures.join("; ")
        );
    }
}
