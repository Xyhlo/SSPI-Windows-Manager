//! Authenticated host availability and bounded link preparation for enabled providers.
use super::{secret, Client, Settings, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const TB: &str = "https://api.torbox.app/v1/api";
const AD: &str = "https://api.alldebrid.com/v4";
const RD: &str = "https://api.real-debrid.com/rest/1.0";
const PROVIDERS: [&str; 3] = ["real-debrid", "torbox", "alldebrid"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProviderHosts {
    pub provider: String,
    pub enabled: bool,
    pub configured: bool,
    pub state: String,
    pub detail: String,
    pub supported: Vec<String>,
    pub unavailable: Vec<String>,
    #[serde(default)]
    pub unknown: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HostState {
    Supported,
    Unavailable,
    Unsupported,
    Unknown,
}

fn flags(settings: &Settings, provider: &str) -> (bool, bool) {
    match provider {
        "real-debrid" => (
            settings.real_debrid_enabled,
            settings.real_debrid_configured,
        ),
        "torbox" => (settings.torbox_enabled, settings.torbox_configured),
        "alldebrid" => (settings.alldebrid_enabled, settings.alldebrid_configured),
        _ => (false, false),
    }
}

pub(super) fn enabled(settings: &Settings) -> Vec<&'static str> {
    PROVIDERS
        .into_iter()
        .filter(|provider| flags(settings, provider) == (true, true))
        .collect()
}

pub(super) fn token(provider: &str, supplied: Option<String>) -> Result<String, String> {
    if !PROVIDERS.contains(&provider) {
        return Err("Unknown link provider".into());
    }
    let value = match supplied.filter(|value| !value.trim().is_empty()) {
        Some(value) => value,
        None => secret(provider)?
            .get_password()
            .map_err(|_| format!("Connect {provider} in Settings"))?,
    };
    if value.trim().is_empty() {
        return Err(format!("Connect {provider} in Settings"));
    }
    Ok(value.trim().to_string())
}

#[derive(Debug)]
pub(super) struct ApiError {
    pub(super) message: String,
    host_failure: bool,
}
impl ApiError {
    fn local(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            host_failure: false,
        }
    }
}

fn response_error(provider: &str, value: &Value, status: u16) -> ApiError {
    let raw = value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .unwrap_or("UPSTREAM_ERROR");
    let code: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    let host_failure = matches!(
        code.as_str(),
        "UNSUPPORTED_SITE"
            | "HOSTER_UNAVAILABLE"
            | "HOST_UNAVAILABLE"
            | "LINK_HOST_NOT_SUPPORTED"
            | "LINK_HOST_UNAVAILABLE"
            | "LINK_HOST_FULL"
            | "LINK_HOST_LIMIT_REACHED"
            | "LINK_HOST_ERROR"
            | "hoster_not_free"
            | "hoster_unavailable"
            | "file_unavailable"
            | "unavailable_file"
    );
    ApiError {
        message: format!("{provider}: {code} (HTTP {status})"),
        host_failure,
    }
}

pub(super) async fn request(provider: &str, builder: reqwest::RequestBuilder) -> Result<Value, ApiError> {
    let mut response = builder
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| {
            ApiError::local(format!(
                "{provider}: network request failed or timed out; retry later"
            ))
        })?;
    let status = response.status().as_u16();
    // Real-Debrid answers an empty list with 204 No Content.
    if status == 204 { return Ok(Value::Array(Vec::new())); }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ApiError::local(format!("{provider}: response interrupted")))?
    {
        if body.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err(ApiError::local(format!(
                "{provider}: response exceeds size limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| {
        ApiError::local(format!("{provider}: invalid JSON response (HTTP {status})"))
    })?;
    let success = match provider {
        "torbox" => {
            value["success"].as_bool() == Some(true) || value.as_str().is_some_and(http_url)
        }
        "alldebrid" => value["status"] == "success",
        _ => value.get("error").is_none(),
    };
    if !(200..300).contains(&status) || !success {
        return Err(response_error(provider, &value, status));
    }
    Ok(value)
}

pub(super) async fn verify(
    http: &Client,
    provider: &str,
    supplied: Option<String>,
) -> Result<String, String> {
    let key = token(provider, supplied)?;
    let endpoint = match provider {
        "torbox" => format!("{TB}/user/me"),
        "alldebrid" => format!("{AD}/user"),
        _ => format!("{RD}/user"),
    };
    let value = request(provider, http.get(endpoint).bearer_auth(key))
        .await
        .map_err(|error| error.message)?;
    match provider {
        "torbox" => {
            let plan = number(&value["data"]["plan"])
                .ok_or("TorBox returned no recognized account plan")?;
            Ok(if plan == 0 {
                "TorBox verified · free account; host access depends on plan and allowance".into()
            } else {
                format!("TorBox verified · plan {plan}; host availability and daily allowances still apply")
            })
        }
        "alldebrid" => {
            let user = &value["data"]["user"];
            let premium = user["isPremium"]
                .as_bool()
                .ok_or("AllDebrid returned no account status")?;
            Ok(format!(
                "AllDebrid verified · {} account",
                if premium {
                    "premium"
                } else if user["isTrial"] == true {
                    "trial"
                } else {
                    "free"
                }
            ))
        }
        _ => Ok(format!(
            "Real-Debrid verified · {} account",
            value["type"].as_str().unwrap_or("connected")
        )),
    }
}

pub(super) async fn hosts(http: &Client, settings: &Settings) -> Vec<ProviderHosts> {
    let (rd, tb, ad) = tokio::join!(
        load_hosts(http, settings, "real-debrid"),
        load_hosts(http, settings, "torbox"),
        load_hosts(http, settings, "alldebrid")
    );
    vec![rd, tb, ad]
}

async fn load_hosts(http: &Client, settings: &Settings, provider: &str) -> ProviderHosts {
    let (enabled, configured) = flags(settings, provider);
    let mut result = ProviderHosts {
        provider: provider.into(),
        enabled,
        configured,
        ..Default::default()
    };
    if !enabled {
        result.state = "disabled".into();
        result.detail = "Provider is disabled".into();
        return result;
    }
    if !configured {
        result.state = "unconfigured".into();
        result.detail = "Connect the provider in Settings".into();
        return result;
    }
    let endpoint = match provider {
        "torbox" => format!("{TB}/webdl/hosters"),
        "alldebrid" => "https://api.alldebrid.com/v4.1/user/hosts".into(),
        _ => format!("{RD}/hosts/status"),
    };
    let response = match token(provider, None) {
        Ok(key) => request(provider, http.get(endpoint).bearer_auth(key))
            .await
            .map_err(|error| error.message),
        Err(error) => Err(error),
    };
    match response.and_then(|value| parse_hosts(provider, &value)) {
        Ok(rows) => {
            result.state = "ready".into();
            result.detail =
                "Live host/account availability; this does not indicate whether a file is cached"
                    .into();
            for (domain, state) in rows {
                match state {
                    HostState::Supported => result.supported.push(domain),
                    HostState::Unavailable | HostState::Unsupported => {
                        result.unavailable.push(domain)
                    }
                    HostState::Unknown => result.unknown.push(domain),
                }
            }
        }
        Err(error) => {
            result.state = "unknown".into();
            result.detail = format!("Host availability is unknown: {error}");
        }
    }
    result
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
fn limit_reached(row: &Value, limit: &str, used: &str) -> bool {
    match (number(&row[limit]), number(&row[used])) {
        (Some(limit), Some(used)) => limit > 0 && used >= limit,
        _ => false,
    }
}
fn domain(value: &str) -> Option<String> {
    let name = value.trim().trim_end_matches('.').to_ascii_lowercase();
    let parsed = reqwest::Url::parse(&format!("https://{name}/")).ok()?;
    (name.contains('.')
        && parsed.host_str() == Some(&name)
        && parsed.path() == "/"
        && parsed.username().is_empty()
        && parsed.port().is_none())
    .then_some(name)
}
fn parse_hosts(provider: &str, value: &Value) -> Result<BTreeMap<String, HostState>, String> {
    let mut rows = BTreeMap::new();
    let mut add = |name: &str, state| {
        if let Some(name) = domain(name) {
            rows.insert(name, state);
        }
    };
    match provider {
        "torbox" => {
            if value["success"] != true {
                return Err("TorBox inventory was not successful".into());
            }
            for row in value["data"]
                .as_array()
                .ok_or("Invalid TorBox host inventory")?
                .iter()
                .take(4096)
            {
                let quota = limit_reached(row, "daily_link_limit", "daily_link_used")
                    || limit_reached(row, "daily_bandwidth_limit", "daily_bandwidth_used");
                let state = if quota {
                    HostState::Unavailable
                } else {
                    match row["status"].as_bool() {
                        Some(true) => HostState::Supported,
                        Some(false) => HostState::Unavailable,
                        None => HostState::Unknown,
                    }
                };
                if let Some(domains) = row["domains"].as_array() {
                    for name in domains.iter().take(128).filter_map(Value::as_str) {
                        add(name, state);
                    }
                }
            }
        }
        "alldebrid" => {
            if value["status"] != "success" {
                return Err("AllDebrid inventory was not successful".into());
            }
            for row in value["data"]["hosts"]
                .as_object()
                .ok_or("Invalid AllDebrid host inventory")?
                .values()
                .take(4096)
            {
                let quota =
                    number(&row["quota"]) == Some(0) || number(&row["limitSimuDl"]) == Some(0);
                let state = if quota || row["status"] == false {
                    HostState::Unavailable
                } else if row.get("status").is_none() || row["status"] == true {
                    HostState::Supported
                } else {
                    HostState::Unknown
                };
                if let Some(domains) = row["domains"].as_array() {
                    for name in domains.iter().take(128).filter_map(Value::as_str) {
                        add(name, state);
                    }
                }
            }
        }
        "real-debrid" => {
            for (name, row) in value
                .as_object()
                .ok_or("Invalid Real-Debrid host inventory")?
                .iter()
                .take(4096)
            {
                let supported = row["supported"]
                    .as_bool()
                    .or_else(|| number(&row["supported"]).map(|n| n == 1));
                let state = if supported == Some(false) || row["status"] == "unsupported" {
                    HostState::Unsupported
                } else if supported == Some(true) && row["status"] == "up" {
                    HostState::Supported
                } else if row["status"] == "down" {
                    HostState::Unavailable
                } else {
                    HostState::Unknown
                };
                add(name, state);
            }
        }
        _ => return Err("Unknown provider".into()),
    }
    if rows.is_empty() {
        return Err("Empty or unrecognized host inventory".into());
    }
    Ok(rows)
}

fn host_state(inventory: &ProviderHosts, url: &str) -> HostState {
    let Ok(url) = reqwest::Url::parse(url) else {
        return HostState::Unknown;
    };
    let host = url
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let matches = |domain: &String| host == *domain || host.ends_with(&format!(".{domain}"));
    let mut candidates = Vec::new();
    for (domains, state) in [
        (&inventory.supported, HostState::Supported),
        (&inventory.unavailable, HostState::Unavailable),
        (&inventory.unknown, HostState::Unknown),
    ] {
        candidates.extend(
            domains
                .iter()
                .filter(|domain| matches(domain))
                .map(|domain| (domain.len(), state)),
        );
    }
    candidates
        .into_iter()
        .max_by_key(|item| item.0)
        .map(|item| item.1)
        .unwrap_or(HostState::Unknown)
}

pub(super) fn http_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

pub(super) async fn resolve(
    http: &Client,
    settings: &Settings,
    url: &str,
    preferred: Option<&str>,
    progress: &(dyn Fn(Option<PreparationProgress>) + Send + Sync),
) -> Result<(String, Option<String>, Option<u64>), String> {
    if !http_url(url) {
        return Err("Invalid provider link".into());
    }
    // A TorBox account file (debrid file browser) is requested directly, not unlocked as a hoster link.
    if let Some(result) = super::cloud::torbox_file(http, url).await {
        return result;
    }
    let allowed = enabled(settings);
    if allowed.is_empty() {
        return Err("Enable and connect a link provider in Settings".into());
    }
    let inventories = hosts(http, settings).await;
    let mut candidates: Vec<_> = inventories
        .iter()
        .filter(|item| allowed.contains(&item.provider.as_str()))
        .filter(|item| {
            matches!(
                host_state(item, url),
                HostState::Supported | HostState::Unknown
            )
        })
        .collect();
    candidates.sort_by_key(|item| {
        (
            preferred != Some(item.provider.as_str()),
            host_state(item, url) != HostState::Supported,
        )
    });
    let mut errors = Vec::new();
    for candidate in candidates {
        progress(None);
        let provider = candidate.provider.as_str();
        let key = token(provider, None)?;
        let result = match provider {
            "torbox" => torbox(http, &key, url, progress).await,
            "alldebrid" => alldebrid(http, &key, url).await,
            _ => super::unrestrict_hoster(http, url)
                .await
                .map_err(|message| ApiError {
                    host_failure: ["hoster", "unavailable file", "file unavailable"]
                        .iter()
                        .any(|needle| message.to_ascii_lowercase().contains(needle)),
                    message,
                }),
        };
        match result {
            Ok(result) => return Ok(result),
            Err(error) if error.host_failure => errors.push(error.message),
            Err(error) => return Err(error.message),
        }
    }
    if errors.is_empty() {
        Err("No enabled provider currently supports this host with available account allowance. Refresh host status or choose another mirror.".into())
    } else {
        Err(errors.join("; "))
    }
}

#[derive(Clone)]
struct Pending {
    id: String,
    until: Instant,
}
static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
static CREATE_GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
fn pending(key: &str) -> Option<Pending> {
    let mut cache = PENDING.get_or_init(Default::default).lock().unwrap();
    cache.retain(|_, value| value.until > Instant::now());
    cache.get(key).cloned()
}
fn remember(key: String, id: String) {
    let mut cache = PENDING.get_or_init(Default::default).lock().unwrap();
    cache.retain(|_, value| value.until > Instant::now());
    if cache.len() >= 256 {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, value)| value.until)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        key,
        Pending {
            id,
            until: Instant::now() + Duration::from_secs(6 * 3600),
        },
    );
}
fn id(value: &Value) -> Option<String> {
    number(value).map(|number| number.to_string())
}
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreparationProgress {
    pub provider: String,
    pub state: String,
    pub progress: Option<f64>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed_bps: f64,
    pub eta_seconds: Option<u64>,
}

fn positive_number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite() && *n >= 0.)
}

fn torbox_progress(job: &Value) -> PreparationProgress {
    let state = job["download_state"].as_str().unwrap_or("").to_ascii_lowercase();
    let progress = positive_number(&job["progress"])
        .filter(|v| *v <= 1. && !matches!(state.as_str(), "queued" | "pending"));
    let total = positive_number(&job["size"]).unwrap_or(0.) as u64;
    let done = positive_number(&job["total_downloaded"])
        .unwrap_or_else(|| progress.unwrap_or(0.) * total as f64) as u64;
    let downloading = state == "downloading";
    PreparationProgress {
        provider: "TorBox".into(),
        state: match state.as_str() {
            "downloading" => "Downloading to TorBox",
            "queued" | "pending" => "Queued on TorBox",
            "paused" | "stalled" => "Waiting on TorBox",
            "completed" | "cached" | "finished" => "Preparing download link",
            "processing" | "checking" => "Processing on TorBox",
            _ => "Preparing on TorBox",
        }.into(),
        progress,
        bytes_done: if total > 0 { done.min(total) } else { done },
        bytes_total: total,
        speed_bps: if downloading { positive_number(&job["download_speed"]).unwrap_or(0.) } else { 0. },
        eta_seconds: if downloading { positive_number(&job["eta"]).filter(|v| *v > 0.).map(|v| v.ceil() as u64) } else { None },
    }
}

fn ready_file(job: &Value) -> Result<Option<&Value>, ApiError> {
    let Some(files) = job["files"].as_array() else {
        return Ok(None);
    };
    if files.len() > 1 {
        return Err(ApiError::local(
            "TorBox returned multiple files; select one package or archive volume",
        ));
    }
    if job["download_present"] != true
        || (job["download_finished"] != true && job["download_state"] != "cached") {
        return Ok(None);
    }
    match files.first() {
        Some(file) if id(&file["id"]).is_some() => Ok(Some(file)),
        Some(_) => Err(ApiError::local("TorBox returned an invalid file ID")),
        None => Ok(None),
    }
}
fn find_job<'a>(data: &'a Value, expected: &str) -> Result<&'a Value, ApiError> {
    let matches = |job: &Value| {
        id(&job["id"])
            .or_else(|| id(&job["webdownload_id"]))
            .is_some_and(|id| id == expected)
    };
    if data.is_object() && matches(data) {
        return Ok(data);
    }
    if let Some(job) = data
        .as_array()
        .and_then(|jobs| jobs.iter().find(|job| matches(job)))
    {
        return Ok(job);
    }
    Err(ApiError::local(
        "TorBox did not return the requested download; preparation was retained for retry",
    ))
}

async fn torbox(
    http: &Client,
    key: &str,
    url: &str,
    progress: &(dyn Fn(Option<PreparationProgress>) + Send + Sync),
) -> Result<(String, Option<String>, Option<u64>), ApiError> {
    let cache_key = super::sha256_hex(format!("torbox\n{key}\n{url}").as_bytes());
    let gate = CREATE_GATE.get_or_init(Default::default).lock().await;
    let download = match pending(&cache_key) {
        Some(pending) => pending,
        None => {
            let created = request(
                "torbox",
                http.post(format!("{TB}/webdl/createwebdownload"))
                    .bearer_auth(key)
                    .form(&[("link", url), ("as_queued", "false")]),
            )
            .await?;
            let data = &created["data"];
            let id = id(&data["webdownload_id"])
                .or_else(|| id(&data["webdownloadId"]))
                .or_else(|| id(&data["id"]))
                .ok_or_else(|| ApiError::local("TorBox returned no valid web download ID"))?;
            remember(cache_key.clone(), id.clone());
            Pending {
                id,
                until: Instant::now() + Duration::from_secs(6 * 3600),
            }
        }
    };
    drop(gate);
    let deadline = Instant::now() + Duration::from_secs(6 * 3600);
    while Instant::now() < deadline {
        let response = request(
            "torbox",
            http.get(format!("{TB}/webdl/mylist"))
                .bearer_auth(key)
                .query(&[("id", download.id.as_str()), ("bypass_cache", "true")]),
        )
        .await?;
        let job = find_job(&response["data"], &download.id)?;
        progress(Some(torbox_progress(job)));
        if let Some(file) = ready_file(job)? {
            let file_id = id(&file["id"]).unwrap();
            let link = request(
                "torbox",
                http.get(format!("{TB}/webdl/requestdl"))
                    .bearer_auth(key)
                    .query(&[
                        ("token", key),
                        ("web_id", download.id.as_str()),
                        ("file_id", file_id.as_str()),
                        ("zip_link", "false"),
                    ]),
            )
            .await?;
            let direct = link
                .as_str()
                .or_else(|| link["data"].as_str())
                .filter(|url| http_url(url))
                .ok_or_else(|| ApiError::local("TorBox returned no valid download URL"))?;
            return Ok((
                direct.into(),
                file["name"].as_str().map(str::to_string),
                number(&file["size"]),
            ));
        }
        let state = job["download_state"]
            .as_str()
            .unwrap_or("")
            .to_ascii_lowercase();
        if state.contains("error") || state.contains("failed") {
            return Err(ApiError::local(
                "TorBox host transfer failed; choose another mirror",
            ));
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Err(ApiError::local("TorBox is still preparing this file; retry shortly. Its preparation ID is retained for this app session."))
}

async fn alldebrid(
    http: &Client,
    key: &str,
    url: &str,
) -> Result<(String, Option<String>, Option<u64>), ApiError> {
    let cache_key = super::sha256_hex(format!("alldebrid\n{key}\n{url}").as_bytes());
    let initial = if let Some(pending) = pending(&cache_key) {
        serde_json::json!({"data":{"delayed":pending.id}})
    } else {
        request(
            "alldebrid",
            http.post(format!("{AD}/link/unlock"))
                .bearer_auth(key)
                .form(&[("link", url)]),
        )
        .await?
    };
    let mut data = initial["data"].clone();
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        if let Some(link) = data["link"].as_str().filter(|url| http_url(url)) {
            return Ok((
                link.into(),
                data["filename"].as_str().map(str::to_string),
                number(&data["filesize"]),
            ));
        }
        let delayed = id(&data["delayed"])
            .or_else(|| pending(&cache_key).map(|pending| pending.id))
            .ok_or_else(|| {
                ApiError::local("AllDebrid returned no single-file link or delayed preparation ID")
            })?;
        remember(cache_key.clone(), delayed.clone());
        if number(&data["status"]) == Some(3) {
            return Err(ApiError::local(
                "AllDebrid could not prepare this file; choose another mirror",
            ));
        }
        if Instant::now() >= deadline {
            return Err(ApiError::local(
                "AllDebrid is still preparing this file; retry later",
            ));
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        data = request(
            "alldebrid",
            http.post(format!("{AD}/link/delayed"))
                .bearer_auth(key)
                .form(&[("id", delayed)]),
        )
        .await?["data"]
            .clone();
    }
}

// ---- Magnets: the provider fetches the torrent; the PC downloads only the chosen file ----

const MAGNET_POLL: Duration = Duration::from_secs(10);
const MAGNET_WAIT: Duration = Duration::from_secs(6 * 3600);

type Resolved = (String, Option<String>, Option<u64>);

fn forget(key: &str) {
    PENDING.get_or_init(Default::default).lock().unwrap().remove(key);
}

/// A torrent the provider could not fetch; another enabled provider may still manage it.
fn torrent_failed(message: String) -> ApiError {
    ApiError { message, host_failure: true }
}

/// Resolves the file `wanted` (a package row's file name) inside a magnet's torrent to a direct
/// download URL. The torrent is added to the first enabled provider (the preferred one first)
/// and polled until the provider holds it, which can take hours for a torrent nobody has cached.
pub(super) async fn resolve_magnet(
    http: &Client,
    settings: &Settings,
    magnet: &str,
    wanted: Option<&str>,
    size: Option<u64>,
    preferred: Option<&str>,
    progress: &(dyn Fn(Option<PreparationProgress>) + Send + Sync),
) -> Result<Resolved, String> {
    let hash = super::magnet::info_hash(magnet).ok_or("Invalid magnet link")?;
    let mut providers = enabled(settings);
    if providers.is_empty() {
        return Err("Magnet links download through Real-Debrid, TorBox or AllDebrid. Enable and connect one in Settings.".into());
    }
    providers.sort_by_key(|provider| preferred != Some(*provider));
    let mut errors = Vec::new();
    for provider in providers {
        progress(None);
        let key = token(provider, None)?;
        let result = match provider {
            "torbox" => torbox_magnet(http, &key, magnet, &hash, wanted, size, progress).await,
            "alldebrid" => alldebrid_magnet(http, &key, magnet, &hash, wanted, size, progress).await,
            _ => real_debrid_magnet(http, &key, magnet, &hash, wanted, size, progress).await,
        };
        match result {
            Ok(result) => return Ok(result),
            Err(error) if error.host_failure => errors.push(error.message),
            Err(error) => return Err(error.message),
        }
    }
    Err(errors.join("; "))
}

fn pick_file(files: &[(String, u64)], wanted: Option<&str>, size: Option<u64>) -> Result<usize, ApiError> {
    super::magnet::pick(files, wanted, size).map_err(ApiError::local)
}

fn base_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

struct RdFile {
    id: u64,
    path: String,
    bytes: u64,
    selected: bool,
}

fn rd_files(info: &Value) -> Vec<RdFile> {
    info["files"]
        .as_array()
        .into_iter()
        .flatten()
        .take(10_000)
        .filter_map(|file| {
            Some(RdFile {
                id: number(&file["id"])?,
                path: file["path"].as_str()?.to_string(),
                bytes: number(&file["bytes"]).unwrap_or(0),
                selected: number(&file["selected"]) == Some(1),
            })
        })
        .collect()
}

fn rd_listing(files: &[&RdFile]) -> Vec<(String, u64)> {
    files.iter().map(|file| (file.path.clone(), file.bytes)).collect()
}

fn rd_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn rd_progress(info: &Value) -> PreparationProgress {
    let status = info["status"].as_str().unwrap_or("");
    let total = number(&info["bytes"]).unwrap_or(0);
    let progress = positive_number(&info["progress"]).filter(|p| *p <= 100.).map(|p| p / 100.);
    let downloading = status == "downloading";
    PreparationProgress {
        provider: "Real-Debrid".into(),
        state: match status {
            "magnet_conversion" => "Reading the torrent on Real-Debrid",
            "waiting_files_selection" => "Choosing the file on Real-Debrid",
            "queued" => "Queued on Real-Debrid",
            "downloading" => "Downloading to Real-Debrid",
            "compressing" | "uploading" => "Processing on Real-Debrid",
            "downloaded" => "Preparing download link",
            _ => "Preparing on Real-Debrid",
        }
        .into(),
        progress: progress.filter(|_| status != "magnet_conversion"),
        bytes_done: (progress.unwrap_or(0.) * total as f64) as u64,
        bytes_total: total,
        speed_bps: if downloading { positive_number(&info["speed"]).unwrap_or(0.) } else { 0. },
        eta_seconds: None,
    }
}

/// A torrent already in the Real-Debrid account that holds the wanted file, or is still waiting
/// for its files to be chosen.
async fn rd_existing(http: &Client, key: &str, hash: &str, wanted: Option<&str>, size: Option<u64>) -> Result<Option<String>, ApiError> {
    let list = request("real-debrid", http.get(format!("{RD}/torrents")).bearer_auth(key).query(&[("limit", "100")])).await?;
    let ids: Vec<String> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["hash"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(hash)))
        .filter(|row| !matches!(row["status"].as_str(), Some("magnet_error" | "error" | "virus" | "dead")))
        .filter_map(|row| row["id"].as_str().filter(|id| rd_valid_id(id)).map(str::to_string))
        .take(5)
        .collect();
    for id in ids {
        let info = request("real-debrid", http.get(format!("{RD}/torrents/info/{id}")).bearer_auth(key)).await?;
        let files = rd_files(&info);
        let chosen: Vec<&RdFile> = files.iter().filter(|file| file.selected).collect();
        if info["status"] == "waiting_files_selection"
            || !chosen.is_empty() && super::magnet::pick(&rd_listing(&chosen), wanted, size).is_ok()
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

async fn real_debrid_magnet(
    http: &Client,
    key: &str,
    magnet: &str,
    hash: &str,
    wanted: Option<&str>,
    size: Option<u64>,
    progress: &(dyn Fn(Option<PreparationProgress>) + Send + Sync),
) -> Result<Resolved, ApiError> {
    let cache_key = super::sha256_hex(format!("rd-magnet\n{key}\n{hash}\n{}", wanted.unwrap_or("")).as_bytes());
    let gate = CREATE_GATE.get_or_init(Default::default).lock().await;
    let id = match pending(&cache_key) {
        Some(pending) => pending.id,
        None => {
            let id = match rd_existing(http, key, hash, wanted, size).await? {
                Some(id) => id,
                None => {
                    let added = request("real-debrid", http.post(format!("{RD}/torrents/addMagnet")).bearer_auth(key).form(&[("magnet", magnet)])).await?;
                    added["id"]
                        .as_str()
                        .filter(|id| rd_valid_id(id))
                        .ok_or_else(|| ApiError::local("Real-Debrid returned no torrent ID"))?
                        .to_string()
                }
            };
            remember(cache_key.clone(), id.clone());
            id
        }
    };
    drop(gate);
    let deadline = Instant::now() + MAGNET_WAIT;
    while Instant::now() < deadline {
        let info = match request("real-debrid", http.get(format!("{RD}/torrents/info/{id}")).bearer_auth(key)).await {
            Ok(info) => info,
            Err(error) => {
                // The torrent may have been deleted from the account: add it again on retry.
                forget(&cache_key);
                return Err(error);
            }
        };
        let status = info["status"].as_str().unwrap_or("");
        if matches!(status, "magnet_error" | "error" | "virus" | "dead") {
            forget(&cache_key);
            return Err(torrent_failed(format!("Real-Debrid could not fetch this torrent ({status})")));
        }
        let files = rd_files(&info);
        if status == "waiting_files_selection" {
            let all: Vec<&RdFile> = files.iter().collect();
            let file = all[pick_file(&rd_listing(&all), wanted, size)?];
            request("real-debrid", http.post(format!("{RD}/torrents/selectFiles/{id}")).bearer_auth(key).form(&[("files", file.id.to_string())])).await?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        progress(Some(rd_progress(&info)));
        if status == "downloaded" {
            let chosen: Vec<&RdFile> = files.iter().filter(|file| file.selected).collect();
            let index = pick_file(&rd_listing(&chosen), wanted, size)?;
            let links: Vec<&str> = info["links"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            // One link per chosen file, in file order. Real-Debrid can also pack them into one archive.
            let link = match links.len() {
                n if n == chosen.len() => links[index],
                1 => links[0],
                _ => return Err(ApiError::local("Real-Debrid returned links that do not match the torrent's files")),
            };
            return super::unrestrict_hoster(http, link).await.map_err(ApiError::local);
        }
        tokio::time::sleep(MAGNET_POLL).await;
    }
    Err(ApiError::local("Real-Debrid is still downloading this torrent; retry later"))
}

async fn torbox_magnet(
    http: &Client,
    key: &str,
    magnet: &str,
    hash: &str,
    wanted: Option<&str>,
    size: Option<u64>,
    progress: &(dyn Fn(Option<PreparationProgress>) + Send + Sync),
) -> Result<Resolved, ApiError> {
    let failed = |job: &Value| {
        let state = job["download_state"].as_str().unwrap_or("").to_ascii_lowercase();
        state.contains("error") || state.contains("failed")
    };
    let cache_key = super::sha256_hex(format!("tb-magnet\n{key}\n{hash}").as_bytes());
    let gate = CREATE_GATE.get_or_init(Default::default).lock().await;
    let torrent = match pending(&cache_key) {
        Some(pending) => pending.id,
        None => {
            let list = request("torbox", http.get(format!("{TB}/torrents/mylist")).bearer_auth(key).query(&[("bypass_cache", "true")])).await?;
            let existing = list["data"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|job| job["hash"].as_str().is_some_and(|h| h.eq_ignore_ascii_case(hash)) && !failed(job))
                .and_then(|job| id(&job["id"]));
            let id = match existing {
                Some(id) => id,
                None => {
                    let created = request(
                        "torbox",
                        // seed=3: never seed from the account. allow_zip=false: list every file.
                        http.post(format!("{TB}/torrents/createtorrent")).bearer_auth(key).form(&[
                            ("magnet", magnet),
                            ("seed", "3"),
                            ("allow_zip", "false"),
                            ("as_queued", "false"),
                        ]),
                    )
                    .await?;
                    id(&created["data"]["torrent_id"])
                        .or_else(|| id(&created["data"]["id"]))
                        .ok_or_else(|| ApiError::local("TorBox returned no torrent ID"))?
                }
            };
            remember(cache_key.clone(), id.clone());
            id
        }
    };
    drop(gate);
    let deadline = Instant::now() + MAGNET_WAIT;
    while Instant::now() < deadline {
        let response = request(
            "torbox",
            http.get(format!("{TB}/torrents/mylist")).bearer_auth(key).query(&[("id", torrent.as_str()), ("bypass_cache", "true")]),
        )
        .await;
        let job = match response.as_ref().map(|value| find_job(&value["data"], &torrent)) {
            Ok(Ok(job)) => job,
            Ok(Err(error)) => {
                forget(&cache_key);
                return Err(error);
            }
            Err(_) => {
                forget(&cache_key);
                return Err(response.unwrap_err());
            }
        };
        if failed(job) {
            forget(&cache_key);
            return Err(torrent_failed("TorBox could not fetch this torrent".into()));
        }
        progress(Some(torbox_progress(job)));
        if job["download_present"] == true && (job["download_finished"] == true || job["download_state"] == "cached") {
            // TorBox flags files its scanner rejected; they cannot be downloaded.
            let files: Vec<&Value> = job["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|file| file["infected"] != true && id(&file["id"]).is_some())
                .take(10_000)
                .collect();
            let listing: Vec<(String, u64)> = files
                .iter()
                .map(|file| (file["name"].as_str().unwrap_or("").to_string(), number(&file["size"]).unwrap_or(0)))
                .collect();
            let index = pick_file(&listing, wanted, size)?;
            let file_id = id(&files[index]["id"]).unwrap();
            let link = request(
                "torbox",
                http.get(format!("{TB}/torrents/requestdl")).bearer_auth(key).query(&[
                    ("token", key),
                    ("torrent_id", torrent.as_str()),
                    ("file_id", file_id.as_str()),
                    ("zip_link", "false"),
                ]),
            )
            .await?;
            let direct = link
                .as_str()
                .or_else(|| link["data"].as_str())
                .filter(|url| http_url(url))
                .ok_or_else(|| ApiError::local("TorBox returned no valid download URL"))?;
            let (name, bytes) = &listing[index];
            return Ok((direct.into(), Some(base_name(name)), Some(*bytes).filter(|n| *n > 0)));
        }
        tokio::time::sleep(MAGNET_POLL).await;
    }
    Err(ApiError::local("TorBox is still downloading this torrent; retry later"))
}

fn alldebrid_files(nodes: &Value, prefix: &str, depth: usize, out: &mut Vec<(String, u64, String)>) {
    if depth > 32 {
        return;
    }
    for node in nodes.as_array().into_iter().flatten().take(10_000) {
        if out.len() >= 10_000 {
            return;
        }
        let name = format!("{prefix}{}", node["n"].as_str().unwrap_or(""));
        match node["l"].as_str().filter(|link| http_url(link)) {
            Some(link) => out.push((name, number(&node["s"]).unwrap_or(0), link.to_string())),
            None => alldebrid_files(&node["e"], &format!("{name}/"), depth + 1, out),
        }
    }
}

fn alldebrid_progress(magnet: &Value) -> PreparationProgress {
    let code = number(&magnet["statusCode"]).unwrap_or(0);
    let total = number(&magnet["size"]).unwrap_or(0);
    let done = number(&magnet["downloaded"]).unwrap_or(0);
    PreparationProgress {
        provider: "AllDebrid".into(),
        state: match code {
            0 => "Queued on AllDebrid",
            1 => "Downloading to AllDebrid",
            2 | 3 => "Processing on AllDebrid",
            _ => "Preparing download link",
        }
        .into(),
        progress: (code == 1 && total > 0).then(|| (done as f64 / total as f64).min(1.)),
        bytes_done: done.min(total.max(done)),
        bytes_total: total,
        speed_bps: if code == 1 { positive_number(&magnet["downloadSpeed"]).unwrap_or(0.) } else { 0. },
        eta_seconds: None,
    }
}

async fn alldebrid_magnet(
    http: &Client,
    key: &str,
    magnet: &str,
    hash: &str,
    wanted: Option<&str>,
    size: Option<u64>,
    progress: &(dyn Fn(Option<PreparationProgress>) + Send + Sync),
) -> Result<Resolved, ApiError> {
    let cache_key = super::sha256_hex(format!("ad-magnet\n{key}\n{hash}").as_bytes());
    let magnet_id = match pending(&cache_key) {
        Some(pending) => pending.id,
        None => {
            // Uploading a magnet the account already holds returns the same ID.
            let uploaded = request("alldebrid", http.post(format!("{AD}/magnet/upload")).bearer_auth(key).form(&[("magnets[]", magnet)])).await?;
            let entry = &uploaded["data"]["magnets"][0];
            if let Some(code) = entry["error"]["code"].as_str() {
                return Err(ApiError::local(format!("AllDebrid could not add this magnet ({code})")));
            }
            let id = id(&entry["id"]).ok_or_else(|| ApiError::local("AllDebrid returned no magnet ID"))?;
            remember(cache_key.clone(), id.clone());
            id
        }
    };
    let deadline = Instant::now() + MAGNET_WAIT;
    while Instant::now() < deadline {
        let status = match request("alldebrid", http.post("https://api.alldebrid.com/v4.1/magnet/status").bearer_auth(key).form(&[("id", magnet_id.as_str())])).await {
            Ok(status) => status,
            Err(error) => {
                forget(&cache_key);
                return Err(error);
            }
        };
        // Queried by ID, `magnets` is one object rather than a list.
        let entry = match &status["data"]["magnets"] {
            Value::Array(list) => list.first().cloned().unwrap_or_default(),
            other => other.clone(),
        };
        let code = number(&entry["statusCode"]);
        if code.is_some_and(|code| code >= 5) {
            forget(&cache_key);
            return Err(torrent_failed(format!(
                "AllDebrid could not fetch this torrent ({})",
                entry["status"].as_str().unwrap_or("error")
            )));
        }
        progress(Some(alldebrid_progress(&entry)));
        if code == Some(4) {
            let listed = request("alldebrid", http.post(format!("{AD}/magnet/files")).bearer_auth(key).form(&[("id[]", magnet_id.as_str())])).await?;
            let entry = listed["data"]["magnets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| id(&entry["id"]).as_deref() == Some(magnet_id.as_str()))
                .ok_or_else(|| ApiError::local("AllDebrid did not list this magnet's files"))?;
            let mut files = Vec::new();
            alldebrid_files(&entry["files"], "", 0, &mut files);
            let listing: Vec<(String, u64)> = files.iter().map(|(name, size, _)| (name.clone(), *size)).collect();
            let index = pick_file(&listing, wanted, size)?;
            return alldebrid(http, key, &files[index].2).await;
        }
        tokio::time::sleep(MAGNET_POLL).await;
    }
    Err(ApiError::local("AllDebrid is still downloading this torrent; retry later"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn torbox_inventory_accounts_for_status_and_quota() {
        let rows = parse_hosts(
            "torbox",
            &json!({"success":true,"data":[
            {"domains":["ready.example"],"status":true},
            {"domains":["down.example"],"status":false},
            {"domains":["quota.example"],"status":true,"daily_link_limit":3,"daily_link_used":3}]}),
        )
        .unwrap();
        assert_eq!(rows["ready.example"], HostState::Supported);
        assert_eq!(rows["down.example"], HostState::Unavailable);
        assert_eq!(rows["quota.example"], HostState::Unavailable);
    }
    #[test]
    fn alldebrid_inventory_uses_account_allowance() {
        let rows = parse_hosts(
            "alldebrid",
            &json!({"status":"success","data":{"hosts":{
            "one":{"domains":["one.example","alias.example"],"quota":0},
            "two":{"domains":["two.example"],"status":true,"quota":128}}}}),
        )
        .unwrap();
        assert_eq!(rows["alias.example"], HostState::Unavailable);
        assert_eq!(rows["two.example"], HostState::Supported);
    }
    #[test]
    fn real_debrid_inventory_and_subdomains_are_precise() {
        let rows = parse_hosts("real-debrid", &json!({"files.example":{"supported":1,"status":"up"},"down.example":{"supported":1,"status":"down"}})).unwrap();
        assert_eq!(rows["files.example"], HostState::Supported);
        let inventory = ProviderHosts {
            state: "ready".into(),
            supported: vec!["files.example".into()],
            ..Default::default()
        };
        assert_eq!(
            host_state(&inventory, "https://cdn.files.example/a"),
            HostState::Supported
        );
        assert_eq!(
            host_state(&inventory, "https://notfiles.example/a"),
            HostState::Unknown
        );
        let mut specific = inventory.clone();
        specific.unknown.push("cdn.files.example".into());
        assert_eq!(
            host_state(&specific, "https://cdn.files.example/a"),
            HostState::Unknown
        );
        assert_eq!(
            serde_json::to_value(&specific).unwrap()["unknown"][0],
            "cdn.files.example"
        );
        assert_eq!(
            host_state(&ProviderHosts::default(), "https://files.example/a"),
            HostState::Unknown
        );
    }
    #[test]
    fn torbox_requires_ready_single_file_and_correct_job() {
        assert!(
            ready_file(&json!({"files":[{"id":0}],"download_finished":false}))
                .unwrap()
                .is_none()
        );
        assert!(
            ready_file(&json!({"files":[{"id":0},{"id":1}],"download_finished":true})).is_err()
        );
        let data = json!([{"id":9,"files":[{"id":0}],"download_finished":true,"download_present":true}]);
        assert!(ready_file(find_job(&data, "9").unwrap()).unwrap().is_some());
        assert!(find_job(&data, "8").is_err());
        assert!(ready_file(&json!({"files":[{"id":0}],"download_present":true,"download_finished":false})).unwrap().is_none());
        assert!(ready_file(&json!({"files":[{"id":0}],"download_present":true,"download_state":"cached"})).unwrap().is_some());
    }
    #[test]
    fn torbox_preparation_reports_remote_metrics_and_unknowns() {
        let p = torbox_progress(&json!({"download_state":"downloading", "progress":0.05,
            "size":1_000_000_000, "download_speed":799_000, "eta":1620}));
        assert_eq!(p.state, "Downloading to TorBox");
        assert_eq!(p.progress, Some(0.05));
        assert_eq!(p.bytes_done, 50_000_000);
        assert_eq!(p.speed_bps, 799_000.);
        assert_eq!(p.eta_seconds, Some(1620));
        let queued = torbox_progress(&json!({"download_state":"queued", "progress":null, "download_speed":42, "eta":20}));
        assert_eq!(queued.progress, None);
        assert_eq!(queued.speed_bps, 0.);
        assert_eq!(queued.eta_seconds, None);
        let malformed = torbox_progress(&json!({"download_state":"downloading", "progress":50, "size":-1, "download_speed":"NaN", "eta":-1}));
        assert_eq!(malformed.progress, None);
        assert_eq!(malformed.bytes_total, 0);
        assert_eq!(malformed.speed_bps, 0.);
    }
    #[test]
    fn auth_and_quota_errors_do_not_trigger_host_fallback() {
        assert!(!response_error("torbox", &json!({"error":"AUTH_ERROR"}), 401).host_failure);
        assert!(
            !response_error(
                "alldebrid",
                &json!({"error":{"code":"AUTH_BAD_APIKEY"}}),
                401
            )
            .host_failure
        );
        assert!(
            response_error(
                "alldebrid",
                &json!({"error":{"code":"LINK_HOST_UNAVAILABLE"}}),
                200
            )
            .host_failure
        );
        assert!(parse_hosts("torbox", &json!({"success":false,"data":[]})).is_err());
        assert!(!http_url("https://secret@example.com/file"));
    }
}
