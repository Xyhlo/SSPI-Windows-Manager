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
        let provider = candidate.provider.as_str();
        let key = token(provider, None)?;
        let result = match provider {
            "torbox" => torbox(http, &key, url).await,
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
fn ready_file(job: &Value) -> Result<Option<&Value>, ApiError> {
    let Some(files) = job["files"].as_array() else {
        return Ok(None);
    };
    if files.len() > 1 {
        return Err(ApiError::local(
            "TorBox returned multiple files; select one package or archive volume",
        ));
    }
    if job["download_finished"] != true && job["download_present"] != true {
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
    let deadline = Instant::now() + Duration::from_secs(600);
    while Instant::now() < deadline {
        let response = request(
            "torbox",
            http.get(format!("{TB}/webdl/mylist"))
                .bearer_auth(key)
                .query(&[("id", download.id.as_str()), ("bypass_cache", "true")]),
        )
        .await?;
        let job = find_job(&response["data"], &download.id)?;
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
        let data = json!([{"id":9,"files":[{"id":0}],"download_finished":true}]);
        assert!(ready_file(find_job(&data, "9").unwrap()).unwrap().is_some());
        assert!(find_job(&data, "8").is_err());
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
