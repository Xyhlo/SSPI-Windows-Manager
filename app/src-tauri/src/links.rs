//! Pasted download links and debrid account files, queued as ordinary downloads.
//!
//! Links come from pasted text (hoster pages, direct files, personal debrid links) or from the
//! debrid file browser (`cloud.rs`). Each becomes a [`DeliveryRequest`]; hoster and debrid links
//! are unlocked by the existing provider pipeline when the download starts, so Retry can renew them.

use super::*;
use regex::Regex;
use std::sync::OnceLock;

const MAX_TEXT: usize = 2 * 1024 * 1024;
const MAX_LINKS: usize = 100;
const ARCHIVES: [&str; 5] = [".zip", ".rar", ".7z", ".7zip", ".pkg"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct LinkItem {
    pub url: String,
    /// File name when known (from the link path or the debrid listing).
    pub name: String,
    pub title_id: String,
    pub title: String,
    /// base | update | dlc | backport
    pub kind: String,
    /// Debrid service that owns this link (personal/account files); empty for hoster links.
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub size: Option<u64>,
    /// Direct file link that needs no provider.
    #[serde(default)]
    pub direct: bool,
    /// Archive set this link belongs to, by file name (`Game.part2.rar` -> `game`).
    #[serde(default)]
    pub set: String,
    #[serde(default)]
    pub part: Option<u32>,
}

fn url_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r#"(?i)https?://[^\s<>"']+"#).unwrap())
}

fn clean(value: &str) -> String {
    let text = value.replace("\\/", "/").replace("&amp;", "&").replace("&#38;", "&").replace("\\u0026", "&");
    let end = text.find(['<', '>', '"', '\'', '\r', '\n', '\t', ' ']).unwrap_or(text.len());
    text[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']).to_string()
}

fn file_name(url: &reqwest::Url) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let decoded = percent_decode(last);
    // Hosters often end in /file or /download; the real name is the segment before it.
    if matches!(decoded.to_ascii_lowercase().as_str(), "file" | "download" | "") {
        let segments: Vec<_> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
        if segments.len() >= 2 { return percent_decode(segments[segments.len() - 2]); }
    }
    decoded
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(byte) = value.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) { out.push(byte); i += 3; continue; }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `CUSA01234` / `PPSA01234` in a file name, with the text before it as the title.
pub(super) fn identity(name: &str) -> (String, String) {
    let upper = name.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    for i in 0..bytes.len().saturating_sub(8) {
        let prefix = &upper[i..i + 4];
        if (prefix == "CUSA" || prefix == "PPSA") && bytes[i + 4..i + 9].iter().all(u8::is_ascii_digit)
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
            && (i + 9 == bytes.len() || !bytes[i + 9].is_ascii_digit())
        {
            let trim: &[char] = &[' ', '-', '_', '.', '[', '(', ']', ')', '\t'];
            let mut title = name[..i].trim_matches(trim).to_string();
            if title.is_empty() {
                let rest = &name[i + 9..];
                let rest = rest.rsplit_once('.').filter(|(_, ext)| ext.len() <= 5).map(|(stem, _)| stem).unwrap_or(rest);
                title = rest.trim_matches(trim).to_string();
            }
            return (upper[i..i + 9].to_string(), title.replace(['_', '.'], " ").split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    (String::new(), String::new())
}

fn guess_kind(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.contains("backport") { "backport" }
    else if lower.contains("dlc") || lower.contains("addon") || lower.contains("add-on") { "dlc" }
    else if lower.contains("update") || lower.contains("patch") { "update" }
    else { "base" }
}

/// Archive volume naming: `name.part3.rar`, `name.rar` + `name.r00`, `name.7z.001`, `name.zip.001`, `name.z01`.
pub(super) fn volume(name: &str) -> Option<(String, u32)> {
    let lower = name.to_ascii_lowercase();
    static PATTERNS: OnceLock<[Regex; 4]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| [
        Regex::new(r"^(.+)\.part0*(\d+)\.rar$").unwrap(),
        Regex::new(r"^(.+)\.(?:7z|zip|rar)\.0*(\d+)$").unwrap(),
        Regex::new(r"^(.+)\.r(\d{2,3})$").unwrap(),
        Regex::new(r"^(.+)\.z(\d{2,3})$").unwrap(),
    ]);
    if let Some(c) = patterns[0].captures(&lower) { return Some((c[1].to_string(), c[2].parse().ok()?)); }
    if let Some(c) = patterns[1].captures(&lower) { return Some((c[1].to_string(), c[2].parse().ok()?)); }
    // Old-style sets: name.rar is volume 1, name.r00 volume 2, ...; name.zip is last after .z01...
    if let Some(c) = patterns[2].captures(&lower) { return Some((c[1].to_string(), c[2].parse::<u32>().ok()? + 2)); }
    if let Some(c) = patterns[3].captures(&lower) { return Some((c[1].to_string(), c[2].parse().ok()?)); }
    None
}

/// Every downloadable link in pasted text, de-duplicated, in order (at most 100).
pub(super) fn extract(text: &str) -> Result<Vec<LinkItem>, String> {
    if text.len() > MAX_TEXT { return Err("The pasted text is larger than 2 MiB".into()); }
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for found in url_pattern().find_iter(text) {
        let value = clean(found.as_str());
        let Ok(url) = reqwest::Url::parse(&value) else { continue };
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() { continue; }
        let path = url.path().to_ascii_lowercase();
        if path.ends_with(".torrent") { continue; }
        if !seen.insert(url.as_str().to_string()) { continue; }
        let name = file_name(&url);
        items.push(item_for(url.as_str(), &name, "", None));
        if items.len() >= MAX_LINKS { break; }
    }
    // Volumes of one archive keep their set only when several of its parts were pasted.
    let mut counts = HashMap::<String, usize>::new();
    for item in &items { if !item.set.is_empty() { *counts.entry(item.set.clone()).or_default() += 1; } }
    for item in &mut items { if counts.get(&item.set).copied().unwrap_or(0) < 2 { item.set.clear(); item.part = None; } }
    Ok(items)
}

/// One link with what its name tells us. Personal debrid links name their owning service.
pub(super) fn item_for(url: &str, name: &str, provider: &str, size: Option<u64>) -> LinkItem {
    let (title_id, title) = identity(name);
    let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
    let provider = if !provider.is_empty() { provider.to_string() }
        else if host.ends_with("real-debrid.com") { "real-debrid".into() }
        else if host.ends_with("alldebrid.com") { "alldebrid".into() }
        else if host.ends_with("torbox.app") { "torbox".into() }
        else { String::new() };
    let direct = provider.is_empty() && ARCHIVES.iter().any(|ext| reqwest::Url::parse(url).is_ok_and(|u| u.path().to_ascii_lowercase().ends_with(ext)));
    let (set, part) = volume(name).map(|(set, part)| (set, Some(part))).unwrap_or_default();
    LinkItem { url: url.to_string(), name: name.to_string(), title_id, title, kind: guess_kind(name).into(), provider, size, direct, set, part }
}

fn package_for(item: &LinkItem, label: &str) -> Package {
    let lower = item.name.to_ascii_lowercase();
    Package {
        kind: if matches!(item.kind.as_str(), "base" | "update" | "dlc" | "backport") { item.kind.clone() } else { "base".into() },
        label: label.to_string(),
        url: item.url.clone(),
        access_type: if item.direct { "Direct".into() } else { "HosterLanding".into() },
        hoster: reqwest::Url::parse(&item.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default(),
        expected_size: item.size,
        archive_file_name: (!item.name.is_empty()).then(|| item.name.clone()),
        archive_format_hint: [".rar", ".zip", ".7z"].iter().find(|ext| lower.contains(*ext)).map(|ext| ext[1..].to_string()),
        ..Default::default()
    }
}

/// Builds one request per link, or one multi-volume request when `as_set` joins the links
/// (in the order given) into a single archive.
pub(super) fn requests(items: &[LinkItem], as_set: bool, active: &str) -> Result<Vec<DeliveryRequest>, String> {
    if items.is_empty() { return Err("Choose at least one link".into()); }
    for item in items {
        let url = reqwest::Url::parse(&item.url).map_err(|_| format!("Invalid link: {}", item.url))?;
        if !matches!(url.scheme(), "http" | "https") { return Err(format!("Only http and https links can be downloaded: {}", item.url)); }
        if !item.title_id.is_empty() && !super::title_id(&item.title_id) { return Err(format!("{} is not a CUSA or PPSA title ID", item.title_id)); }
    }
    let target = |id: &str| -> Option<String> {
        if id.starts_with("PPSA") { Some("ps5".into()) } else if id.starts_with("CUSA") { Some("ps4".into()) } else { Some(active.to_string()) }
    };
    let title_of = |item: &LinkItem| if item.title.is_empty() { item.name.clone() } else { item.title.clone() };
    let provider_of = |item: &LinkItem| (!item.provider.is_empty()).then(|| item.provider.clone());
    if as_set && items.len() > 1 {
        let first = &items[0];
        let set_id = format!("links-{}", &super::sha256_hex(items.iter().map(|i| i.url.as_str()).collect::<Vec<_>>().join("\n").as_bytes())[..16]);
        let count = items.len() as u32;
        let parts: Vec<Package> = items.iter().enumerate().map(|(i, item)| Package {
            archive_set_id: Some(set_id.clone()), archive_part_number: Some(i as u32 + 1), archive_part_count: Some(count),
            ..package_for(item, &format!("{} · part {}/{count}", title_of(first), i + 1))
        }).collect();
        let title_id = items.iter().map(|i| i.title_id.clone()).find(|id| !id.is_empty());
        return Ok(vec![DeliveryRequest {
            target: target(title_id.as_deref().unwrap_or("")), transport: None, package: parts[0].clone(),
            title_id, title_name: Some(title_of(first)), icon: None, archive_parts: parts, backport: None,
            provider: provider_of(first), package_dumps: None,
        }]);
    }
    Ok(items.iter().map(|item| DeliveryRequest {
        target: target(&item.title_id), transport: None,
        package: package_for(item, &if item.name.is_empty() { item.url.clone() } else { item.name.clone() }),
        title_id: (!item.title_id.is_empty()).then(|| item.title_id.clone()),
        title_name: Some(title_of(item)), icon: None, archive_parts: vec![], backport: None,
        provider: provider_of(item), package_dumps: None,
    }).collect())
}

#[tauri::command]
pub(super) fn parse_download_links(text: String) -> Result<Vec<LinkItem>, String> {
    extract(&text)
}

/// Queues pasted or browsed links. Returns the job IDs that started.
#[tauri::command]
pub(super) async fn start_link_downloads(app: AppHandle, state: State<'_, AppState>, items: Vec<LinkItem>, as_set: bool) -> Result<Vec<String>, String> {
    let active = state.settings.lock().unwrap().active_console.clone();
    let mut started = Vec::new();
    for request in requests(&items, as_set, &active)? {
        started.push(queue_delivery(app.clone(), &state, request, None).await?);
    }
    Ok(started)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_text_yields_downloadable_links_once_each() {
        let text = r#"Mirror 1: https://hoster.example/?abc123&amp;af=1, also <a href="https://www.mediafire.com/file/x1/Game-CUSA01234.part1.rar/file">p1</a>
            https://www.mediafire.com/file/x2/Game-CUSA01234.part2.rar/file
            https://example.com/files/Some%20Game%20PPSA01234%20v1.02.pkg). And again https://hoster.example/?abc123&amp;af=1
            magnet & torrent: https://site/x.torrent  ftp://nope/file.pkg  https://user:pw@host/file.pkg"#;
        let links = extract(text).unwrap();
        let urls: Vec<_> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(urls, [
            "https://hoster.example/?abc123&af=1",
            "https://www.mediafire.com/file/x1/Game-CUSA01234.part1.rar/file",
            "https://www.mediafire.com/file/x2/Game-CUSA01234.part2.rar/file",
            "https://example.com/files/Some%20Game%20PPSA01234%20v1.02.pkg",
        ]);
        assert_eq!((links[1].title_id.as_str(), links[1].title.as_str()), ("CUSA01234", "Game"));
        assert_eq!((links[1].set.as_str(), links[1].part, links[2].part), ("game-cusa01234", Some(1), Some(2)));
        assert!(links[3].direct && links[3].title_id == "PPSA01234" && links[3].title == "Some Game");
        assert!(!links[0].direct && links[0].set.is_empty());
    }

    #[test]
    fn archive_volumes_are_numbered_like_their_tools() {
        assert_eq!(volume("Game.part03.rar"), Some(("game".into(), 3)));
        assert_eq!(volume("Game.rar"), None);
        assert_eq!(volume("Game.r00"), Some(("game".into(), 2)));
        assert_eq!(volume("Game.7z.002"), Some(("game".into(), 2)));
        assert_eq!(volume("Game.z01"), Some(("game".into(), 1)));
    }

    #[test]
    fn debrid_links_name_their_service_and_sets_become_one_request() {
        let rd = item_for("https://real-debrid.com/d/ABCDEF", "Game-CUSA00001.part1.rar", "", Some(10));
        let rd2 = item_for("https://real-debrid.com/d/GHIJKL", "Game-CUSA00001.part2.rar", "", Some(10));
        assert_eq!(rd.provider, "real-debrid");
        let one = requests(&[rd.clone(), rd2.clone()], true, "ps5").unwrap();
        assert_eq!(one.len(), 1);
        let request = &one[0];
        assert_eq!((request.target.as_deref(), request.provider.as_deref()), (Some("ps4"), Some("real-debrid")));
        assert_eq!(request.archive_parts.len(), 2);
        assert!(request.archive_parts.iter().all(|p| p.access_type == "HosterLanding" && p.archive_part_count == Some(2)));
        assert_eq!(delivery_parts(request).unwrap().len(), 2);
        let separate = requests(&[rd, rd2], false, "ps5").unwrap();
        assert_eq!(separate.len(), 2);
        let direct = item_for("https://cdn.example/Game.pkg", "Game.pkg", "", None);
        let request = &requests(&[direct], false, "ps5").unwrap()[0];
        assert_eq!((request.package.access_type.as_str(), request.target.as_deref()), ("Direct", Some("ps5")));
        assert!(requests(&[LinkItem { title_id: "BAD12345".into(), ..item_for("https://x/y.pkg", "y.pkg", "", None) }], false, "ps5").is_err());
    }
}
