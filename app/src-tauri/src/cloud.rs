//! Files already stored in the user's debrid accounts (Real-Debrid downloads and torrents,
//! TorBox torrents and web downloads, AllDebrid magnets), browsed like folders.
//!
//! A file is handed to the download queue as an account link that the provider pipeline unlocks
//! when the download starts, so Retry renews it: Real-Debrid and AllDebrid share links, and a
//! TorBox `requestdl` address without the key (the key is added only when it is requested).

use super::links::{item_for, LinkItem};
use super::*;

const RD: &str = "https://api.real-debrid.com/rest/1.0";
const TB: &str = "https://api.torbox.app/v1/api";
const AD: &str = "https://api.alldebrid.com";
const PAGE: usize = 50;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct CloudEntry {
    pub name: String,
    /// Folder to open (folders), or empty for files.
    pub folder: String,
    /// The download for a file.
    pub link: Option<LinkItem>,
    pub size: Option<u64>,
    pub detail: String,
    pub ready: bool,
}

fn text(value: &Value, key: &str) -> String {
    match &value[key] { Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), _ => String::new() }
}
fn bytes(value: &Value, key: &str) -> Option<u64> { value[key].as_u64().or_else(|| text(value, key).parse().ok()).filter(|n| *n > 0) }
fn truthy(value: &Value, key: &str) -> bool { value[key].as_bool() == Some(true) || text(value, key) == "1" }
fn safe_id(id: &str) -> Result<&str, String> {
    if id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err("Invalid account item ID".into()); }
    Ok(id)
}
fn rows(value: &Value) -> Vec<Value> { value.as_array().cloned().unwrap_or_default() }
fn gib(size: Option<u64>) -> String { size.map(|n| format!(" · {:.2} GB", n as f64 / 1_073_741_824.)).unwrap_or_default() }
fn file(name: String, url: &str, provider: &str, service: &str, size: Option<u64>) -> Option<CloudEntry> {
    debrid::http_url(url).then(|| CloudEntry { link: Some(item_for(url, &name, provider, size)), detail: format!("Ready in {service}{}", gib(size)), name, size, ready: true, folder: String::new() })
}
fn folder(name: String, folder: String, ready: bool, detail: String) -> CloudEntry { CloudEntry { name, folder, ready, detail, ..Default::default() } }

fn torbox_ready(job: &Value) -> bool { truthy(job, "download_present") && (truthy(job, "download_finished") || truthy(job, "cached")) }
fn torbox_detail(job: &Value) -> String {
    if torbox_ready(job) { "Ready in TorBox".into() } else {
        let state = text(job, "download_state");
        format!("Not ready: {}", if state.is_empty() { "TorBox has not confirmed stored files".into() } else { state })
    }
}

/// AllDebrid magnet files form a tree: `n` name, `s` size, `l` link, `e` children.
fn alldebrid_files(output: &mut Vec<CloudEntry>, entries: &Value, prefix: &str, depth: usize) {
    if depth > 16 { return; }
    for entry in rows(entries) {
        if output.len() >= 2000 { return; }
        let name = format!("{prefix}{}", text(&entry, "n"));
        let link = text(&entry, "l");
        match file(name.clone(), &link, "alldebrid", "AllDebrid", bytes(&entry, "s")) {
            Some(item) => output.push(item),
            None => alldebrid_files(output, &entry["e"], &format!("{name}/"), depth + 1),
        }
    }
}

/// Lists one folder. Top-level folders: `rd-downloads`, `rd-torrents`, `tb-torrents`, `tb-webdl`,
/// `ad-magnets`; opened items: `rd-torrent/<id>`, `tb-folder/<torrents|webdl>/<id>`, `ad-magnet/<id>`.
pub(super) async fn list(http: &Client, folder_path: &str, page: usize) -> Result<Vec<CloudEntry>, String> {
    let parts: Vec<&str> = folder_path.split('/').collect();
    let call = |provider: &'static str, builder: reqwest::RequestBuilder| async move {
        debrid::request(provider, builder).await.map_err(|error| error.message)
    };
    let mut output = Vec::new();
    match parts.as_slice() {
        ["rd-downloads"] => {
            let key = debrid::token("real-debrid", None)?;
            for row in rows(&call("real-debrid", http.get(format!("{RD}/downloads?limit={PAGE}&page={}", page + 1)).bearer_auth(&key)).await?) {
                let (link, direct) = (text(&row, "link"), text(&row, "download"));
                let entry = if debrid::http_url(&link) { file(text(&row, "filename"), &link, "real-debrid", "Real-Debrid", bytes(&row, "filesize")) }
                    else { file(text(&row, "filename"), &direct, "", "Real-Debrid", bytes(&row, "filesize")) };
                output.extend(entry);
            }
        }
        ["rd-torrents"] => {
            let key = debrid::token("real-debrid", None)?;
            for row in rows(&call("real-debrid", http.get(format!("{RD}/torrents?limit={PAGE}&page={}", page + 1)).bearer_auth(&key)).await?) {
                let ready = text(&row, "status") == "downloaded";
                output.push(folder(text(&row, "filename"), format!("rd-torrent/{}", safe_id(&text(&row, "id"))?), ready,
                    if ready { format!("Ready in Real-Debrid{}", gib(bytes(&row, "bytes"))) } else { format!("Not ready: {}", text(&row, "status")) }));
            }
        }
        ["rd-torrent", id] => {
            let key = debrid::token("real-debrid", None)?;
            let info = call("real-debrid", http.get(format!("{RD}/torrents/info/{}", safe_id(id)?)).bearer_auth(&key)).await?;
            if text(&info, "status") != "downloaded" { return Err("This torrent is still preparing in Real-Debrid".into()); }
            let selected: Vec<Value> = rows(&info["files"]).into_iter().filter(|f| truthy(f, "selected")).collect();
            let links = rows(&info["links"]);
            for (n, link) in links.iter().enumerate() {
                let Some(link) = link.as_str() else { continue };
                let entry = selected.get(n).filter(|_| selected.len() == links.len());
                let mut name = entry.map(|f| text(f, "path").trim_start_matches('/').to_string()).unwrap_or_default();
                if name.is_empty() { name = format!("{}{}", text(&info, "filename"), if links.len() > 1 { format!(" - file {}", n + 1) } else { String::new() }); }
                output.extend(file(name, link, "real-debrid", "Real-Debrid", entry.and_then(|f| bytes(f, "bytes"))));
            }
        }
        [kind @ ("tb-torrents" | "tb-webdl")] => {
            let key = debrid::token("torbox", None)?;
            let api = if *kind == "tb-torrents" { "torrents" } else { "webdl" };
            let response = call("torbox", http.get(format!("{TB}/{api}/mylist?limit={PAGE}&offset={}", page * PAGE)).bearer_auth(&key)).await?;
            for job in rows(&response["data"]) {
                output.push(folder(text(&job, "name"), format!("tb-folder/{api}/{}", safe_id(&text(&job, "id"))?), torbox_ready(&job), torbox_detail(&job)));
            }
        }
        ["tb-folder", api @ ("torrents" | "webdl"), id] => {
            let key = debrid::token("torbox", None)?;
            let response = call("torbox", http.get(format!("{TB}/{api}/mylist?id={}", safe_id(id)?)).bearer_auth(&key)).await?;
            let jobs = if response["data"].is_object() { vec![response["data"].clone()] } else { rows(&response["data"]) };
            let id_key = if *api == "torrents" { "torrent_id" } else { "web_id" };
            for job in jobs.iter().filter(|job| text(job, "id") == *id) {
                for item in rows(&job["files"]) {
                    let file_id = text(&item, "id");
                    if safe_id(&file_id).is_err() { continue; }
                    let infected = truthy(&item, "infected");
                    // The key is never stored in the link; it is added when the download is requested.
                    let url = format!("{TB}/{api}/requestdl?{id_key}={id}&file_id={file_id}&zip_link=false");
                    let size = bytes(&item, "size");
                    output.push(CloudEntry {
                        name: text(&item, "name"), link: Some(item_for(&url, &text(&item, "name"), "torbox", size)), size,
                        ready: !infected && torbox_ready(job),
                        detail: if infected { "Unavailable: TorBox flagged this file".into() } else { format!("{}{}", torbox_detail(job), gib(size)) },
                        folder: String::new(),
                    });
                }
            }
        }
        ["ad-magnets"] => {
            let key = debrid::token("alldebrid", None)?;
            let response = call("alldebrid", http.post(format!("{AD}/v4.1/magnet/status")).bearer_auth(&key).form(&Vec::<(String, String)>::new())).await?;
            for magnet in rows(&response["data"]["magnets"]).into_iter().skip(page * PAGE).take(PAGE) {
                let ready = text(&magnet, "statusCode") == "4";
                output.push(folder(text(&magnet, "filename"), format!("ad-magnet/{}", safe_id(&text(&magnet, "id"))?), ready,
                    if ready { format!("Ready in AllDebrid{}", gib(bytes(&magnet, "size"))) } else { format!("Not ready: {}", text(&magnet, "status")) }));
            }
        }
        ["ad-magnet", id] => {
            let key = debrid::token("alldebrid", None)?;
            let response = call("alldebrid", http.post(format!("{AD}/v4/magnet/files")).bearer_auth(&key).form(&[("id[]", safe_id(id)?)])).await?;
            for magnet in rows(&response["data"]["magnets"]).iter().filter(|m| text(m, "id") == *id) {
                alldebrid_files(&mut output, &magnet["files"], "", 0);
            }
        }
        _ => return Err("Unknown debrid folder".into()),
    }
    Ok(output)
}

/// A TorBox account file link (`.../torrents|webdl/requestdl?torrent_id|web_id=..&file_id=..`)
/// becomes a fresh download address. Returns `None` for any other link.
pub(super) async fn torbox_file(http: &Client, url: &str) -> Option<Result<(String, Option<String>, Option<u64>), String>> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("api.torbox.app") { return None; }
    let api = match parsed.path() { "/v1/api/torrents/requestdl" => "torrents", "/v1/api/webdl/requestdl" => "webdl", _ => return None };
    let query: HashMap<String, String> = parsed.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let id_key = if api == "torrents" { "torrent_id" } else { "web_id" };
    let numeric = |key: &str| query.get(key).filter(|v| !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit())).cloned();
    let (Some(job), Some(file)) = (numeric(id_key), numeric("file_id")) else { return Some(Err("Invalid TorBox file link".into())) };
    Some(async {
        let key = debrid::token("torbox", None)?;
        let response = debrid::request("torbox", http.get(format!("{TB}/{api}/requestdl"))
            .query(&[("token", key.as_str()), (id_key, job.as_str()), ("file_id", file.as_str()), ("zip_link", "false")]))
            .await.map_err(|error| error.message)?;
        let link = response.as_str().map(str::to_string).unwrap_or_else(|| text(&response, "data"));
        if !debrid::http_url(&link) { return Err("This TorBox file is not ready to download yet; retry after it finishes preparing".into()); }
        Ok((link, None, None))
    }.await)
}

#[tauri::command]
pub(super) async fn list_cloud_files(state: State<'_, AppState>, folder: String, page: Option<usize>) -> Result<Vec<CloudEntry>, String> {
    let http = state.http.clone();
    list(&http, &folder, page.unwrap_or(0).min(1000)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alldebrid_trees_flatten_to_files_with_paths() {
        let tree = serde_json::json!([{ "n": "Game", "e": [
            { "n": "Game-CUSA01234.part1.rar", "s": 100, "l": "https://alldebrid.com/f/abc" },
            { "n": "Extras", "e": [{ "n": "readme.txt", "s": 1, "l": "https://alldebrid.com/f/def" }] }
        ]}]);
        let mut output = Vec::new();
        alldebrid_files(&mut output, &tree, "", 0);
        let names: Vec<_> = output.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Game/Game-CUSA01234.part1.rar", "Game/Extras/readme.txt"]);
        let link = output[0].link.as_ref().unwrap();
        assert_eq!((link.provider.as_str(), link.title_id.as_str(), link.part), ("alldebrid", "CUSA01234", Some(1)));
    }

    #[tokio::test]
    async fn torbox_file_links_are_recognised_without_a_key() {
        let http = Client::new();
        assert!(torbox_file(&http, "https://hoster.example/?x").await.is_none());
        assert!(torbox_file(&http, "https://api.torbox.app/v1/api/webdl/createwebdownload").await.is_none());
        let bad = torbox_file(&http, "https://api.torbox.app/v1/api/torrents/requestdl?torrent_id=12&file_id=x").await;
        assert_eq!(bad.unwrap().unwrap_err(), "Invalid TorBox file link");
        assert!(!"https://api.torbox.app/v1/api/torrents/requestdl?torrent_id=12&file_id=3&zip_link=false".contains("token="));
    }
}
