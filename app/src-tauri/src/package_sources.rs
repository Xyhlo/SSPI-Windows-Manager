mod local_recipe;
pub mod community;
use crate::static_catalog;
use local_recipe::{scoped_titles, scoped_packages, source_link, utf8_window};
use regex::Regex;
use reqwest::{redirect, Client};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager};
use url::Url;
use zip::ZipArchive;

// Static catalogs (DLPS PS4 Static and friends) ship a full sharded catalog
// inside the .gssource; they are read, not executed, and are verified file by
// file at install time.
const MAX_ARCHIVE: usize = 8 * 1024 * 1024;
const MAX_EXPANDED: u64 = 64 * 1024 * 1024;
const MAX_ENTRY: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;
const MAX_REQUESTS: usize = 32;
const MAX_ITEMS: usize = 128;
const MAX_RESULTS: usize = 100;
const MAX_STEPS: usize = 24;
const ENGINE_API_VERSION: &str = "5.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub engine_type: String,
    pub enabled: bool,
    pub trust: String,
    pub install_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTitle {
    pub title_id: String,
    pub name: String,
    pub region: String,
    pub icon: Option<String>,
    pub source_id: String,
    pub source_name: String,
    pub source_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePackage {
    pub kind: String,
    pub label: String,
    pub url: String,
    pub access_type: String,
    pub source_id: String,
    pub source_name: String,
    pub source_version: String,
    pub candidate_id: String,
    pub group_id: String,
    pub hoster: String,
    pub version: String,
    pub firmware: String,
    pub source_page_url: String,
    pub expected_size: Option<u64>,
    pub expected_sha256: String,
    pub expected_content_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_set_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_part_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_part_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_format_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intermediate_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referer: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    #[serde(default)]
    schema: String,
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    version: String,
    #[serde(default)]
    minimum_api_version: String,
    #[serde(default)]
    maximum_api_version: String,
    #[serde(default)]
    capabilities: Vec<String>,
    engine: Engine,
    #[serde(default)]
    permissions: Permissions,
    #[serde(default)]
    origins: Vec<String>,
    #[serde(default)]
    files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Engine {
    #[serde(rename = "type")]
    engine_type: String,
    #[serde(default, alias = "entryFile")]
    entry: String,
    #[serde(default)]
    search: Option<RemoteOperation>,
    #[serde(default)]
    resolve: Option<RemoteOperation>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Permissions {
    #[serde(default)]
    network_origins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestFile {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteOperation {
    url: String,
    #[serde(default)]
    results_path: String,
    #[serde(default)]
    fields: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Registry {
    sources: Vec<RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryEntry {
    id: String,
    name: String,
    description: String,
    version: String,
    engine_type: String,
    enabled: bool,
    trust: String,
    install_url: String,
    archive_sha256: String,
}

#[derive(Debug, Clone, Default)]
struct WorkItem {
    url: String,
    parent_url: String,
    source_page: String,
    value: String,
    name: String,
    title_id: String,
    region: String,
    image: String,
    html: String,
    kind: String,
    package_version: String,
    firmware: String,
    group_id: String,
    hoster: String,
    label: String,
    intermediate_url: String,
    archive_set_id: String,
    archive_part_number: Option<u32>,
    archive_part_count: Option<u32>,
    archive_file_name: String,
    archive_format_hint: String,
    archive_password: String,
    mirror_id: String,
    expected_size: Option<u64>,
    diagnostics: Vec<String>,
    score: f64,
}

fn source_root(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .join("sources"))
}

fn registry_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(source_root(app)?.join("registry.json"))
}

fn load_registry(app: &AppHandle) -> Result<Registry, String> {
    let path = registry_path(app)?;
    if !path.is_file() {
        return Ok(Registry::default());
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| format!("Package Source registry is invalid: {error}"))
}

fn save_registry(app: &AppHandle, registry: &Registry) -> Result<(), String> {
    let path = registry_path(app)?;
    let parent = path
        .parent()
        .ok_or("Package Source registry has no parent")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let next = parent.join("registry.json.next");
    fs::write(
        &next,
        serde_json::to_vec_pretty(registry).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if path.exists() {
        let backup = parent.join("registry.json.bak");
        let _ = fs::copy(&path, backup);
        fs::remove_file(&path).map_err(|error| error.to_string())?;
    }
    fs::rename(next, path).map_err(|error| error.to_string())
}

fn source_dir(app: &AppHandle, id: &str, version: &str) -> Result<PathBuf, String> {
    Ok(source_root(app)?.join("installed").join(id).join(version))
}

fn id_value(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte))
}

fn safe_archive_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !value.contains('\0')
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn executable_name(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        ".exe", ".dll", ".so", ".prx", ".sprx", ".elf", ".self", ".bat", ".cmd", ".ps1", ".sh",
        ".py", ".js",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

fn executable_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ") || bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"#!")
}

fn origins(descriptor: &Descriptor) -> Vec<String> {
    if descriptor.permissions.network_origins.is_empty() {
        descriptor.origins.clone()
    } else {
        descriptor.permissions.network_origins.clone()
    }
}

fn parse_origin(value: &str) -> Option<(String, String, u16)> {
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some((
        url.scheme().to_owned(),
        url.host_str()?.to_ascii_lowercase(),
        url.port_or_known_default()?,
    ))
}

fn allowed(origins: &[String], url: &Url) -> bool {
    origins.iter().any(|origin| {
        parse_origin(origin).is_some_and(|(scheme, host, port)| {
            scheme == url.scheme()
                && url
                    .host_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case(&host))
                && url.port_or_known_default() == Some(port)
        })
    })
}

fn same_site_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.port_or_known_default() == right.port_or_known_default()
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left_host, right_host)| {
                left_host
                    .trim_start_matches("www.")
                    .eq_ignore_ascii_case(right_host.trim_start_matches("www."))
            })
}

/// SSPI community catalogs: `embedded-catalog-v1`, and `embedded-catalog-refresh-v1`, the same
/// data plus an online "recent posts" refresh. The refresh is not run here: it would contact
/// catalog websites, so the catalog stays as shipped in the file.
fn embedded_catalog(engine: &str) -> bool {
    matches!(engine, "embedded-catalog-v1" | "embedded-catalog-refresh-v1")
}

fn validate_descriptor(descriptor: &Descriptor) -> Result<(), String> {
    if !descriptor.schema.is_empty() && descriptor.schema != "gamesearch.source/v1" {
        return Err("Package Source schema is not gamesearch.source/v1".into());
    }
    if !id_value(&descriptor.id, 128) || !id_value(&descriptor.version, 64) {
        return Err("Package Source id or version is invalid".into());
    }
    if descriptor.name.trim().is_empty() || descriptor.name.len() > 96 {
        return Err("Package Source name is invalid".into());
    }
    if !matches!(
        descriptor.engine.engine_type.as_str(),
        "recipe-v1" | "recipe-v2" | "remote-api-v1" | "embedded-catalog-v1" | "embedded-catalog-refresh-v1"
    ) {
        return Err("Package Source engine is unsupported".into());
    }
    if (!descriptor.minimum_api_version.is_empty()
        && version_cmp(ENGINE_API_VERSION, &descriptor.minimum_api_version).is_lt())
        || (!descriptor.maximum_api_version.is_empty()
            && version_cmp(ENGINE_API_VERSION, &descriptor.maximum_api_version).is_gt())
    {
        return Err(format!(
            "unsupported-engine-version: source requires {}..{} but engine is {ENGINE_API_VERSION}",
            if descriptor.minimum_api_version.is_empty() {
                "any"
            } else {
                &descriptor.minimum_api_version
            },
            if descriptor.maximum_api_version.is_empty() {
                "any"
            } else {
                &descriptor.maximum_api_version
            }
        ));
    }
    if embedded_catalog(&descriptor.engine.engine_type) {
        // Static catalogs are data-only: no search-time network origins.
        // Package downloads go through the normal hoster/resolver pipeline.
        let entry = descriptor.engine.entry.trim();
        if entry.is_empty() || !entry.ends_with(".json") || !safe_archive_path(entry) {
            return Err("Static catalog entry file is invalid".into());
        }
        return Ok(());
    }
    let network = origins(descriptor);
    if network.is_empty() || network.iter().any(|value| parse_origin(value).is_none()) {
        return Err("Package Source network origins are invalid".into());
    }
    Ok(())
}

fn validate_recipe(engine_type: &str, recipe: &Value) -> Result<(), String> {
    let expected = if engine_type == "recipe-v2" {
        "gamesearch.recipe/v2"
    } else {
        "gamesearch.recipe/v1"
    };
    match recipe.get("schema").and_then(Value::as_str) {
        Some(schema) if schema == expected => {}
        Some("gamesearch.recipe/v2") if engine_type != "recipe-v2" => {
            return Err(
                "unsupported-engine-version: recipe-v2 cannot run on a recipe-v1 runtime".into(),
            );
        }
        _ => {
            return Err(format!(
                "unsupported-engine-version: recipe schema must be {expected}"
            ));
        }
    }
    for key in ["searchSteps", "resolveSteps", "catalogSteps"] {
        if recipe
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|steps| steps.len() > MAX_STEPS)
        {
            return Err("Recipe exceeds 24 steps".into());
        }
    }
    Ok(())
}

fn version_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let mut l = left.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
    let mut r = right
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    for _ in 0..3 {
        match l.next().unwrap_or(0).cmp(&r.next().unwrap_or(0)) {
            std::cmp::Ordering::Equal => continue,
            result => return result,
        }
    }
    std::cmp::Ordering::Equal
}

fn catalog_name(id: &str, name: &str) -> String {
    let value = format!("{id} {name}").to_ascii_lowercase();
    if value.contains("ps4") { "Global PS4".into() }
    else if value.contains("ps5") { "Global PS5".into() }
    else if value.contains("dlps") { "Global Catalog".into() }
    else { name.into() }
}

fn summary(entry: &RegistryEntry) -> SourceSummary {
    SourceSummary {
        id: entry.id.clone(),
        name: catalog_name(&entry.id, &entry.name),
        description: if entry.engine_type == "embedded-catalog-v1" { "Global game catalog. Downloads use your connected services.".into() } else { entry.description.replace("DLPS", "Global").replace("dlps", "Global") },
        version: entry.version.clone(),
        engine_type: entry.engine_type.clone(),
        enabled: entry.enabled,
        trust: entry.trust.clone(),
        install_url: entry.install_url.clone(),
    }
}

pub fn list(app: &AppHandle) -> Result<Vec<SourceSummary>, String> {
    Ok(load_registry(app)?.sources.iter().map(summary).collect())
}

pub async fn install(
    app: &AppHandle,
    http: &Client,
    install_url: &str,
) -> Result<Vec<SourceSummary>, String> {
    let url = Url::parse(install_url).map_err(|_| "Package Source URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Package Source URL must use HTTP or HTTPS".into());
    }
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|_| "Package Source download failed")?;
    if !response.status().is_success() {
        return Err(format!(
            "Package Source returned HTTP {}",
            response.status()
        ));
    }
    if response.content_length().unwrap_or(0) > MAX_ARCHIVE as u64 {
        return Err("Package Source exceeds the 4 MiB limit".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Package Source download body failed")?;
    if bytes.len() > MAX_ARCHIVE {
        return Err("Package Source exceeds the 4 MiB limit".into());
    }
    install_archive(app, bytes.to_vec(), install_url)
}

pub fn install_from_path(app: &AppHandle, path: &str) -> Result<Vec<SourceSummary>, String> {
    let path = PathBuf::from(path);
    if !path.is_file() {
        return Err("Selected file was not found".into());
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != "gssource" && extension != "zip" {
        return Err("Choose a .gssource file".into());
    }
    let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_ARCHIVE as u64 {
        return Err("Package Source exceeds the 4 MiB limit".into());
    }
    let bytes = fs::read(&path).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_ARCHIVE {
        return Err("Package Source exceeds the 4 MiB limit".into());
    }
    install_archive(app, bytes, &path.to_string_lossy())
}

fn install_archive(
    app: &AppHandle,
    bytes: Vec<u8>,
    install_url: &str,
) -> Result<Vec<SourceSummary>, String> {
    let (archive_hash, descriptor, files) = validate_archive(bytes)?;
    install_validated_archive(app, archive_hash, descriptor, files, install_url)
}

fn validate_archive(bytes: Vec<u8>) -> Result<(String, Descriptor, HashMap<String, Vec<u8>>), String> {
    if bytes.len() > MAX_ARCHIVE {
        return Err("Package Source exceeds the 8 MiB limit".into());
    }
    let archive_hash = sha256(&bytes);
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Package Source is not a ZIP")?;
    if zip.len() == 0 || zip.len() > MAX_ENTRIES {
        return Err("Package Source entry count is invalid".into());
    }
    let mut expanded = 0u64;
    let mut names = HashSet::new();
    let mut files = HashMap::<String, Vec<u8>>::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|error| error.to_string())?;
        let name = entry.name().replace('\\', "/");
        if entry.is_dir() {
            continue;
        }
        if !safe_archive_path(&name)
            || !names.insert(name.to_lowercase())
            || executable_name(&name)
            || entry.encrypted()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(format!("Unsafe Package Source entry: {name}"));
        }
        if entry.size() > MAX_ENTRY
            || (entry.compressed_size() > 0 && entry.size() / entry.compressed_size() > 20)
        {
            return Err(format!("Package Source entry exceeds limits: {name}"));
        }
        expanded += entry.size();
        if expanded > MAX_EXPANDED {
            return Err("Package Source expanded size exceeds 16 MiB".into());
        }
        let mut content = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut content)
            .map_err(|error| error.to_string())?;
        if executable_magic(&content) {
            return Err(format!("Executable content is forbidden: {name}"));
        }
        files.insert(name, content);
    }
    let descriptor_bytes = files
        .get("source.json")
        .ok_or("Package Source has no source.json")?;
    if descriptor_bytes.len() > 256 * 1024 {
        return Err("source.json exceeds 256 KiB".into());
    }
    let descriptor: Descriptor = serde_json::from_slice(descriptor_bytes)
        .map_err(|error| format!("source.json is invalid: {error}"))?;
    validate_descriptor(&descriptor)?;
    if !descriptor.files.is_empty() {
        let declared = descriptor
            .files
            .iter()
            .map(|item| item.path.to_lowercase())
            .collect::<HashSet<_>>();
        for name in files.keys() {
            if name != "source.json"
                && name != "signature.ed25519"
                && !declared.contains(&name.to_lowercase())
            {
                return Err(format!("Undeclared Package Source file: {name}"));
            }
        }
        for item in &descriptor.files {
            let content = files
                .get(&item.path)
                .ok_or_else(|| format!("Declared file is missing: {}", item.path))?;
            if content.len() as u64 != item.size
                || !sha256(content).eq_ignore_ascii_case(&item.sha256)
            {
                return Err(format!("Declared file failed validation: {}", item.path));
            }
        }
    }
    if matches!(
        descriptor.engine.engine_type.as_str(),
        "recipe-v1" | "recipe-v2"
    ) {
        let entry = if descriptor.engine.entry.is_empty() {
            "recipe.json"
        } else {
            &descriptor.engine.entry
        };
        let recipe = files.get(entry).ok_or("Recipe entry file is missing")?;
        if recipe.len() > 256 * 1024 {
            return Err("Recipe exceeds 256 KiB".into());
        }
        let value: Value = serde_json::from_slice(recipe).map_err(|_| "Recipe JSON is invalid")?;
        validate_recipe(&descriptor.engine.engine_type, &value)?;
    }
    Ok((archive_hash, descriptor, files))
}

fn install_validated_archive(
    app: &AppHandle,
    archive_hash: String,
    descriptor: Descriptor,
    files: HashMap<String, Vec<u8>>,
    install_url: &str,
) -> Result<Vec<SourceSummary>, String> {
    let destination = source_dir(app, &descriptor.id, &descriptor.version)?;
    let mut registry = load_registry(app)?;
    let enabled = enabled_after_install(&registry, &descriptor.id);
    if let Some(existing) = registry
        .sources
        .iter_mut()
        .find(|item| item.id == descriptor.id && item.version == descriptor.version)
    {
        if existing.archive_sha256 != archive_hash {
            return Err("This source id/version is already installed with different bytes".into());
        }
        existing.install_url = install_url.to_owned();
        save_registry(app, &registry)?;
        return Ok(registry.sources.iter().map(summary).collect());
    }
    let staging = source_root(app)?
        .join("staging")
        .join(format!("{}-{}-next", descriptor.id, descriptor.version));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    for (name, content) in files {
        let target = staging.join(&name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(target, content).map_err(|error| error.to_string())?;
    }
    if destination.exists() {
        fs::remove_dir_all(&staging).map_err(|error| error.to_string())?;
        return Err("Package Source destination already exists".into());
    }
    if embedded_catalog(&descriptor.engine.engine_type) {
        // Data-only catalogs: every shard must match its declaration before
        // the source becomes visible to search.
        let declared: Vec<(String, u64, String)> = descriptor
            .files
            .iter()
            .map(|item| (item.path.clone(), item.size, item.sha256.clone()))
            .collect();
        static_catalog::validate_installed(&staging, &declared)?;
    }
    fs::create_dir_all(destination.parent().ok_or("Invalid source destination")?)
        .map_err(|error| error.to_string())?;
    static_catalog::invalidate(&destination);
    fs::rename(staging, &destination).map_err(|error| error.to_string())?;
    registry.sources.retain(|item| item.id != descriptor.id);
    registry.sources.push(RegistryEntry {
        id: descriptor.id,
        name: descriptor.name,
        description: descriptor.description,
        version: descriptor.version,
        engine_type: descriptor.engine.engine_type,
        enabled,
        trust: "unsigned-dev".into(),
        install_url: install_url.to_owned(),
        archive_sha256: archive_hash,
    });
    save_registry(app, &registry)?;
    Ok(registry.sources.iter().map(summary).collect())
}

fn enabled_after_install(registry: &Registry, source_id: &str) -> bool {
    registry.sources.iter().find(|source| source.id == source_id)
        .map(|source| source.enabled).unwrap_or(true)
}

pub fn set_enabled(app: &AppHandle, id: &str, enabled: bool) -> Result<Vec<SourceSummary>, String> {
    let mut registry = load_registry(app)?;
    let source = registry
        .sources
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or("Package Source is not installed")?;
    source.enabled = enabled;
    save_registry(app, &registry)?;
    Ok(registry.sources.iter().map(summary).collect())
}

pub fn remove(app: &AppHandle, id: &str) -> Result<Vec<SourceSummary>, String> {
    if !id_value(id, 128) {
        return Err("Package Source id is invalid".into());
    }
    let mut registry = load_registry(app)?;
    if !registry.sources.iter().any(|item| item.id == id) {
        return Err("Package Source is not installed".into());
    }
    registry.sources.retain(|item| item.id != id);
    save_registry(app, &registry)?;
    let owned = source_root(app)?.join("installed").join(id);
    if owned.is_dir() {
        fs::remove_dir_all(owned).map_err(|error| error.to_string())?;
    }
    Ok(registry.sources.iter().map(summary).collect())
}

fn load_source(
    app: &AppHandle,
    entry: &RegistryEntry,
) -> Result<(Descriptor, Option<Value>), String> {
    let directory = source_dir(app, &entry.id, &entry.version)?;
    let descriptor: Descriptor = serde_json::from_slice(
        &fs::read(directory.join("source.json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    validate_descriptor(&descriptor)?;
    let recipe = if matches!(
        descriptor.engine.engine_type.as_str(),
        "recipe-v1" | "recipe-v2"
    ) {
        let name = if descriptor.engine.entry.is_empty() {
            "recipe.json"
        } else {
            &descriptor.engine.entry
        };
        let recipe: Value = serde_json::from_slice(
            &fs::read(directory.join(name)).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        validate_recipe(&descriptor.engine.engine_type, &recipe)?;
        Some(recipe)
    } else {
        None
    };
    Ok((descriptor, recipe))
}

fn source_client(descriptor: &Descriptor) -> Result<Client, String> {
    let network = origins(descriptor);
    let redirects = network.clone();
    Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) GameSearch/0.2")
        .timeout(Duration::from_secs(30))
        .redirect(redirect::Policy::custom(move |attempt| {
            if allowed(&redirects, attempt.url()) && attempt.previous().len() < 8 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

async fn get_text(
    client: &Client,
    descriptor: &Descriptor,
    url: &str,
    referer: &str,
) -> Result<(String, String), String> {
    let parsed = Url::parse(url).map_err(|_| "Recipe produced an invalid URL")?;
    let network = origins(descriptor);
    if !allowed(&network, &parsed) {
        return Err(format!(
            "HTTP origin is not permitted: {}",
            parsed.host_str().unwrap_or("")
        ));
    }
    if !referer.is_empty() {
        let referer_url = Url::parse(referer).map_err(|_| "Recipe referer is invalid")?;
        if !allowed(&network, &referer_url) {
            return Err("Recipe referer origin is not permitted".into());
        }
    }
    let mut request = client.get(parsed);
    if !referer.is_empty() {
        request = request.header(reqwest::header::REFERER, referer);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Source request returned HTTP {}",
            response.status()
        ));
    }
    if !allowed(&network, response.url()) {
        return Err("Source redirect left the declared origins".into());
    }
    if response.content_length().unwrap_or(0) > MAX_RESPONSE as u64 {
        return Err("Source response exceeds 2 MiB".into());
    }
    let final_url = response.url().to_string();
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    if bytes.len() > MAX_RESPONSE {
        return Err("Source response exceeds 2 MiB".into());
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), final_url))
}

fn replace_common(
    template: &str,
    query: &str,
    title_id: &str,
    name: &str,
    region: &str,
    limit: usize,
) -> String {
    template
        .replace("{query}", &encode(query))
        .replace("{titleId}", &encode(title_id))
        .replace("{titleid}", &encode(title_id))
        .replace("{name}", &encode(name))
        .replace("{normalizedName}", &encode(&normalize_name(name)))
        .replace("{region}", &encode(region))
        .replace("{limit}", &limit.to_string())
        .replace("{cursor}", "")
}

fn replace_raw(template: &str, query: &str, title_id: &str, name: &str, region: &str) -> String {
    template
        .replace("{query}", query)
        .replace("{titleId}", title_id)
        .replace("{titleid}", title_id)
        .replace("{name}", name)
        .replace("{normalizedName}", &normalize_name(name))
        .replace("{region}", region)
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn value_at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    path.split('.')
        .try_fold(value, |current, part| current.get(part))
}

fn string_at(value: &Value, path: &str) -> String {
    value_at(value, path)
        .and_then(|item| match item {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn field<'a>(operation: &'a RemoteOperation, name: &str, fallback: &'a str) -> &'a str {
    operation
        .fields
        .get(name)
        .map(String::as_str)
        .unwrap_or(fallback)
}

async fn remote_search(
    descriptor: &Descriptor,
    query: &str,
    limit: usize,
) -> Result<Vec<SourceTitle>, String> {
    let operation = descriptor
        .engine
        .search
        .as_ref()
        .ok_or("Source has no search operation")?;
    let url = replace_common(&operation.url, query, "", query, "", limit);
    let (body, _) = get_text(&source_client(descriptor)?, descriptor, &url, "").await?;
    let json: Value =
        serde_json::from_str(&body).map_err(|_| "Source search returned invalid JSON")?;
    let rows = value_at(&json, &operation.results_path)
        .and_then(Value::as_array)
        .ok_or("Source search result path is not an array")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let title_id =
                string_at(row, field(operation, "titleId", "title_id")).to_ascii_uppercase();
            let name = string_at(row, field(operation, "name", "name"));
            (valid_title_id(&title_id) && !name.is_empty()).then(|| SourceTitle {
                title_id,
                name,
                region: string_at(row, field(operation, "region", "region")),
                icon: Some(string_at(row, field(operation, "icon", "icon")))
                    .filter(|item| !item.is_empty()),
                source_id: descriptor.id.clone(),
                source_name: catalog_name(&descriptor.id, &descriptor.name),
                source_version: descriptor.version.clone(),
            })
        })
        .take(limit)
        .collect())
}

async fn remote_resolve(
    descriptor: &Descriptor,
    title_id: &str,
    name: &str,
    region: &str,
) -> Result<Vec<SourcePackage>, String> {
    let operation = descriptor
        .engine
        .resolve
        .as_ref()
        .ok_or("Source has no resolve operation")?;
    let url = replace_common(&operation.url, "", title_id, name, region, 100);
    let (body, _) = get_text(&source_client(descriptor)?, descriptor, &url, "").await?;
    let json: Value =
        serde_json::from_str(&body).map_err(|_| "Source resolve returned invalid JSON")?;
    let rows = value_at(&json, &operation.results_path)
        .and_then(Value::as_array)
        .ok_or("Source resolve result path is not an array")?;
    Ok(rows
        .iter()
        .filter_map(|row| package_from_remote(descriptor, operation, row, title_id))
        .take(MAX_RESULTS)
        .collect())
}

fn package_from_remote(
    descriptor: &Descriptor,
    operation: &RemoteOperation,
    row: &Value,
    requested_id: &str,
) -> Option<SourcePackage> {
    let url = string_at(row, field(operation, "url", "url"));
    let parsed = Url::parse(&url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    let row_id = string_at(row, field(operation, "titleId", "title_id")).to_ascii_uppercase();
    if !row_id.is_empty() && row_id != requested_id {
        return None;
    }
    let kind = normalize_kind(&string_at(row, field(operation, "kind", "kind")));
    let label = string_at(row, field(operation, "label", "label"));
    let candidate_id = string_at(row, field(operation, "id", "id"));
    Some(SourcePackage {
        kind,
        label: if label.is_empty() {
            "Package mirror".into()
        } else {
            label
        },
        url,
        access_type: string_at(row, field(operation, "accessType", "access_type")),
        source_id: descriptor.id.clone(),
        source_name: catalog_name(&descriptor.id, &descriptor.name),
        source_version: descriptor.version.clone(),
        candidate_id,
        group_id: string_at(row, field(operation, "groupId", "mirror_group")),
        hoster: string_at(row, field(operation, "hoster", "hoster")),
        version: string_at(row, field(operation, "version", "version")),
        firmware: string_at(row, field(operation, "firmware", "firmware")),
        source_page_url: string_at(row, field(operation, "sourcePageUrl", "source_page")),
        expected_size: string_at(row, field(operation, "size", "size"))
            .parse()
            .ok(),
        expected_sha256: string_at(row, field(operation, "sha256", "sha256")),
        expected_content_id: string_at(row, field(operation, "contentId", "content_id")),
        archive_set_id: None,
        archive_part_number: None,
        archive_part_count: None,
        archive_file_name: None,
        archive_format_hint: None,
        archive_password: None,
        mirror_id: None,
        intermediate_url: None,
        referer: None,
        diagnostics: Vec::new(),
    })
}

fn steps<'a>(recipe: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    recipe
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("Recipe has no {key}"))
}

fn step_strings(step: &Value, key: &str) -> Vec<String> {
    step.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn step_string(step: &Value, key: &str) -> String {
    step.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

async fn recipe_search(
    descriptor: &Descriptor,
    recipe: &Value,
    query: &str,
    limit: usize,
) -> Result<Vec<SourceTitle>, String> {
    let client = source_client(descriptor)?;
    let pipeline = steps(recipe, if query.is_empty() && recipe.get("catalogSteps").is_some() { "catalogSteps" } else { "searchSteps" })?;
    if pipeline.len() > MAX_STEPS {
        return Err("Recipe exceeds 24 steps".into());
    }
    let mut requests = 0usize;
    let mut items = Vec::<WorkItem>::new();
    let mut json = String::new();
    let mut output = Vec::new();
    for step in pipeline {
        let op = step_string(step, "op");
        match op.as_str() {
            "input.queries" => {
                let mut seen = HashSet::new();
                items = step_strings(step, "templates")
                    .into_iter()
                    .map(|template| replace_raw(&template, query, "", query, ""))
                    .filter(|value| {
                        !value.trim().is_empty() && seen.insert(value.to_ascii_lowercase())
                    })
                    .map(|value| WorkItem {
                        value,
                        ..Default::default()
                    })
                    .collect();
            }
            "http.get" => {
                let template = step_string(step, "url");
                let inputs = if template.contains("{url}") || template.contains("{value}") {
                    std::mem::take(&mut items)
                } else {
                    vec![WorkItem::default()]
                };
                let mut fetched = Vec::new();
                let mut last_failure = None;
                let continue_on_error =
                    step.get("continueOnError").and_then(Value::as_bool) == Some(true);
                for input in inputs {
                    let url = if template.contains("{url}") || template.contains("{value}") {
                        expand_item(&template, "", query, "", &input)
                    } else {
                        replace_common(&template, query, "", query, "", limit)
                    };
                    let referer = expand_item(&step_string(step, "referer"), "", query, "", &input);
                    // Final host links are data for the debrid provider. They never
                    // consume a catalog HTTP request or disappear at the request cap.
                    if continue_on_error && Url::parse(&url).ok().is_some_and(|u| !allowed(&origins(descriptor), &u)) {
                        fetched.push(input);
                        continue;
                    }
                    requests += 1;
                    if requests > MAX_REQUESTS { return Err("Recipe exceeds 32 HTTP requests".into()); }
                    match get_text(&client, descriptor, &url, &referer).await {
                        Ok((body, final_url)) => {
                            json = body.clone();
                            let mut item = input;
                            item.url = final_url;
                            item.html = body;
                            fetched.push(item);
                        }
                        Err(error)
                            if continue_on_error
                                && error.contains("HTTP origin is not permitted") =>
                        {
                            fetched.push(input);
                        }
                        Err(error) if continue_on_error => {
                            last_failure = Some(error);
                        }
                        Err(error) => return Err(error),
                    }
                }
                if fetched.is_empty() && step.get("failIfAllFailed").and_then(Value::as_bool) == Some(true) {
                    if let Some(error) = last_failure { return Err(error); }
                }
                items = fetched;
            }
            "json.select" => {
                let value: Value =
                    serde_json::from_str(&json).map_err(|_| "Recipe JSON response is invalid")?;
                let required = step_string(step, "requireBool");
                if !required.is_empty()
                    && value_at(&value, &required).and_then(Value::as_bool) != Some(true)
                {
                    return Err("Recipe JSON response was rejected".into());
                }
                let path = step
                    .get("array")
                    .and_then(Value::as_str)
                    .unwrap_or("results");
                let rows = value_at(&value, path)
                    .and_then(Value::as_array)
                    .ok_or("Recipe JSON array is missing")?;
                let title_field = step
                    .get("titleId")
                    .and_then(Value::as_str)
                    .unwrap_or("titleId");
                let name_field = step.get("name").and_then(Value::as_str).unwrap_or("name");
                let region_field = step
                    .get("region")
                    .and_then(Value::as_str)
                    .unwrap_or("region");
                let image_field = step.get("image").and_then(Value::as_str).unwrap_or("image");
                items = rows
                    .iter()
                    .filter_map(|row| {
                        let title_id = string_at(row, title_field).to_ascii_uppercase();
                        valid_title_id(&title_id).then(|| WorkItem {
                            title_id,
                            name: string_at(row, name_field),
                            region: normalize_region(&string_at(row, region_field)),
                            image: string_at(row, image_field),
                            ..Default::default()
                        })
                    })
                    .take(MAX_ITEMS)
                    .collect();
            }
            "html.articles" => {
                items = parse_articles(
                    descriptor,
                    &items,
                    step.get("limit").and_then(Value::as_u64).unwrap_or(30) as usize,
                    Some(step),
                )
            }
            "html.decode-base64-fragments" => items = decode_fragments(items, step)?,
            "html.extract-titles" => items = title_items(&items, step),
            "html.extract-scoped-titles" => {
                items = scoped_titles(&items, step);
                for item in items.iter().take(limit.min(MAX_RESULTS)) {
                    output.push(SourceTitle { title_id: item.title_id.clone(), name: item.name.clone(),
                        region: item.region.clone(), icon: Some(item.image.clone()).filter(|s|!s.is_empty()),
                        source_id: descriptor.id.clone(), source_name: catalog_name(&descriptor.id, &descriptor.name), source_version: descriptor.version.clone() });
                }
            },
            "items.dedupe" => {
                items = dedupe(
                    items,
                    step.get("field").and_then(Value::as_str).unwrap_or("url"),
                )
            }
            "rank.title-match" => {
                let minimum = step
                    .get("minimumScore")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.35);
                let take = step.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize;
                for item in &mut items {
                    item.score = match_score(query, &item.name);
                }
                items.retain(|item| item.score >= minimum);
                items.sort_by(|a, b| b.score.total_cmp(&a.score));
                items.truncate(take.min(MAX_ITEMS));
            }
            "items.take" => items.truncate(
                step.get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(MAX_ITEMS as u64)
                    .min(MAX_ITEMS as u64) as usize,
            ),
            "emit.title" => {
                for item in &items {
                    if output.len() >= limit.min(MAX_RESULTS) {
                        break;
                    }
                    if !valid_title_id(&item.title_id) {
                        continue;
                    }
                    output.push(SourceTitle {
                        title_id: item.title_id.clone(),
                        name: if item.name.is_empty() {
                            item.title_id.clone()
                        } else {
                            item.name.clone()
                        },
                        region: if item.region.is_empty() {
                            "?".into()
                        } else {
                            item.region.clone()
                        },
                        icon: Some(item.image.clone()).filter(|value| !value.is_empty()),
                        source_id: descriptor.id.clone(),
                        source_name: catalog_name(&descriptor.id, &descriptor.name),
                        source_version: descriptor.version.clone(),
                    });
                }
            }
            _ => return Err(format!("Unsupported search recipe operation: {op}")),
        }
    }
    Ok(output)
}

async fn recipe_resolve(
    descriptor: &Descriptor,
    recipe: &Value,
    title_id: &str,
    name: &str,
    region: &str,
) -> Result<Vec<SourcePackage>, String> {
    let client = source_client(descriptor)?;
    let pipeline = steps(recipe, "resolveSteps")?;
    if pipeline.len() > MAX_STEPS {
        return Err("Recipe exceeds 24 steps".into());
    }
    let mut requests = 0usize;
    let mut items = Vec::<WorkItem>::new();
    for step in pipeline {
        let op = step_string(step, "op");
        match op.as_str() {
            "input.queries" => {
                let mut seen = HashSet::new();
                items = step_strings(step, "templates")
                    .into_iter()
                    .map(|template| replace_raw(&template, "", title_id, name, region))
                    .filter(|value| {
                        !value.trim().is_empty() && seen.insert(value.to_ascii_lowercase())
                    })
                    .map(|value| WorkItem {
                        value,
                        ..Default::default()
                    })
                    .collect();
            }
            "http.get" => {
                let template = step_string(step, "url");
                let referer_template = step_string(step, "referer");
                let needs_item = template.contains("{url}") || template.contains("{value}");
                let inputs = if items.is_empty() && !needs_item {
                    vec![WorkItem::default()]
                } else {
                    items
                };
                let continue_on_error =
                    step.get("continueOnError").and_then(Value::as_bool) == Some(true);
                let mut fetched = Vec::new();
                let mut last_failure = None;
                for input in inputs.into_iter().take(MAX_ITEMS) {
                    let url = expand_item(&template, title_id, name, region, &input);
                    let referer = expand_item(&referer_template, title_id, name, region, &input);
                    // Final host links are data for the debrid provider. They never
                    // consume a catalog HTTP request or disappear at the request cap.
                    if continue_on_error && Url::parse(&url).ok().is_some_and(|u| !allowed(&origins(descriptor), &u)) {
                        fetched.push(input);
                        continue;
                    }
                    requests += 1;
                    if requests > MAX_REQUESTS { return Err("Recipe exceeds 32 HTTP requests".into()); }
                    match get_text(&client, descriptor, &url, &referer).await {
                        Ok((body, final_url)) => {
                            let mut result = input;
                            result.url = final_url;
                            result.html = body;
                            fetched.push(result);
                        }
                        Err(error)
                            if continue_on_error
                                && error.contains("HTTP origin is not permitted") =>
                        {
                            fetched.push(input);
                        }
                        Err(error) if continue_on_error => {
                            last_failure = Some(error);
                        }
                        Err(error) => return Err(error),
                    }
                }
                if fetched.is_empty() && step.get("failIfAllFailed").and_then(Value::as_bool) == Some(true) {
                    if let Some(error) = last_failure { return Err(error); }
                }
                items = fetched;
            }
            "html.articles" => {
                items = parse_articles(
                    descriptor,
                    &items,
                    step.get("limit").and_then(Value::as_u64).unwrap_or(30) as usize,
                    Some(step),
                )
            }
            "items.dedupe" => {
                items = dedupe(
                    items,
                    step.get("field").and_then(Value::as_str).unwrap_or("url"),
                )
            }
            "rank.title-match" => {
                let minimum = step
                    .get("minimumScore")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.35);
                let take = step.get("limit").and_then(Value::as_u64).unwrap_or(8) as usize;
                for item in &mut items {
                    item.score = match_score(name, &item.name);
                    let blob = format!("{} {}", item.name, item.url).to_ascii_lowercase();
                    if blob.contains(&title_id.to_ascii_lowercase()) {
                        item.score = (item.score + 0.35).min(1.0);
                    }
                }
                items.retain(|item| item.score >= minimum);
                items.sort_by(|left, right| right.score.total_cmp(&left.score));
                items.truncate(take.min(MAX_ITEMS));
                #[cfg(test)]
                for item in &items {
                    eprintln!("ranked {:.3} {} {}", item.score, item.name, item.url);
                }
            }
            "html.download-link" => items = parse_download_links(descriptor, &items, step),
            "html.package-table-links" => items = parse_package_tables(&items, title_id, step),
            "html.decode-base64-fragments" => items = decode_fragments(items, step)?,
            "html.package-sections" => items = parse_package_sections(&items, title_id, step),
            "html.scoped-packages" => items = scoped_packages(&items, title_id, step),
            "html.hoster-links" => items = parse_hoster_links(&items, step),
            "items.take" => items.truncate(
                step.get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(MAX_ITEMS as u64)
                    .min(MAX_ITEMS as u64) as usize,
            ),
            "emit.package" => {
                let output = emit_packages(descriptor, items, step);
                #[cfg(test)]
                eprintln!("recipe {op}: {} package(s)", output.len());
                return Ok(output);
            }
            _ => return Err(format!("Unsupported resolve recipe operation: {op}")),
        }
        #[cfg(test)]
        eprintln!("recipe {op}: {} item(s)", items.len());
    }
    Ok(Vec::new())
}

fn expand_item(
    template: &str,
    title_id: &str,
    name: &str,
    region: &str,
    item: &WorkItem,
) -> String {
    replace_common(template, "", title_id, name, region, 100)
        .replace("{url}", &item.url)
        .replace("{value}", &encode(&item.value))
        .replace("{parentUrl}", &item.parent_url)
        .replace("{sourcePage}", &item.source_page)
}

fn parse_articles(
    _descriptor: &Descriptor,
    documents: &[WorkItem],
    limit: usize,
    step: Option<&Value>,
) -> Vec<WorkItem> {
    let link_selector = Selector::parse("h2.entry-title a[href], h2.grid-title a[href], h2.post-title a[href], h3.entry-title a[href], h3.grid-title a[href], h2 a[href], h3 a[href]").unwrap();
    let image_selector = Selector::parse("img[src], img[data-src]").unwrap();
    let mut output = Vec::new();
    let selectors = step
        .map(|value| step_strings(value, "containerSelectors"))
        .unwrap_or_default();
    let selectors = if selectors.is_empty() {
        vec!["article".to_owned()]
    } else {
        selectors
    };
    for document in documents {
        let Ok(base) = Url::parse(&document.url) else {
            continue;
        };
        let html = Html::parse_document(&document.html);
        for selector in selectors.iter().filter(|selector| bounded_selector(selector)) {
            let Ok(parsed_selector) = Selector::parse(selector) else {
                continue;
            };
            for article in html.select(&parsed_selector) {
                let Some(link) = article.select(&link_selector).next() else {
                    continue;
                };
                let Some(href) = link.value().attr("href") else {
                    continue;
                };
                let Ok(url) = base.join(href) else {
                    continue;
                };
                if !same_site_origin(&base, &url) {
                    continue;
                }
                if let Some(pattern) = step.and_then(|v|v.get("urlPattern")).and_then(Value::as_str) {
                    if !Regex::new(pattern).ok().is_some_and(|r|r.is_match(url.as_str())) { continue; }
                }
                let name = collapse(&link.text().collect::<Vec<_>>().join(" "));
                if name.is_empty() {
                    continue;
                }
                let image = article
                    .select(&image_selector)
                    .next()
                    .and_then(|node| {
                        node.value()
                            .attr("src")
                            .or_else(|| node.value().attr("data-src"))
                    })
                    .and_then(|value| url.join(value).ok())
                    .map(|value| value.to_string())
                    .unwrap_or_default();
                output.push(WorkItem {
                    url: url.to_string(),
                    parent_url: document.url.clone(),
                    name,
                    image,
                    ..Default::default()
                });
                if output.len() >= limit.min(MAX_ITEMS) {
                    return output;
                }
            }
        }
    }
    output
}

fn decode_fragments(documents: Vec<WorkItem>, step: &Value) -> Result<Vec<WorkItem>, String> {
    let maximum = step
        .get("maximumDecodedBytes")
        .and_then(Value::as_u64)
        .unwrap_or(256 * 1024)
        .min(256 * 1024) as usize;
    let replace_inner = step.get("replaceInnerHtml").and_then(Value::as_bool) != Some(false);
    let continue_on_error = step.get("continueOnError").and_then(Value::as_bool) == Some(true);
    let mut total = 0usize;
    let selectors = step
        .get("selectors")
        .and_then(Value::as_array)
        .ok_or("decode-base64-fragments requires selectors")?;
    let mut output = Vec::new();
    for mut document in documents {
        let parsed = Html::parse_document(&document.html);
        let mut appended = String::new();
        let mut replacements = Vec::new();
        let mut failed = None;
        for definition in selectors {
            if definition.get("encoding").and_then(Value::as_str).unwrap_or("base64-utf8") != "base64-utf8" {
                return Err("unsupported fragment encoding".into());
            }
            let selector_text = definition
                .get("selector")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !bounded_selector(selector_text) {
                return Err("invalid bounded fragment selector".into());
            }
            let selector = Selector::parse(selector_text)
                .map_err(|_| "invalid bounded fragment selector")?;
            let attribute = definition
                .get("attribute")
                .and_then(Value::as_str)
                .ok_or("fragment attribute missing")?;
            for node in parsed.select(&selector) {
                let Some(value) = node.value().attr(attribute) else {
                    continue;
                };
                let decoded = match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    value,
                ) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        failed = Some("invalid base64 fragment".to_owned());
                        continue;
                    }
                };
                if decoded.len() > maximum || total.saturating_add(decoded.len()) > maximum {
                    failed = Some("decoded fragments exceed configured limit".into());
                    continue;
                }
                let text = match std::str::from_utf8(&decoded) {
                    Ok(text) => inert_html(text),
                    Err(_) => {
                        failed = Some("fragment is not UTF-8".into());
                        continue;
                    }
                };
                total += decoded.len();
                if replace_inner {
                    replacements.push((attribute.to_owned(), value.to_owned(), text));
                } else {
                    appended.push_str(&text);
                }
                document.diagnostics.push(format!(
                    "decoded inert fragment url={} parent={} selector={} attr={}",
                    document.url, document.parent_url, selector_text, attribute
                ));
            }
        }
        if let Some(error) = failed {
            if continue_on_error {
                document.diagnostics.push(error);
            } else {
                return Err(error);
            }
        }
        if replace_inner {
            for (attribute, payload, text) in replacements {
                document.html = inject_decoded_fragment(&document.html, &attribute, &payload, &text);
            }
        } else if !appended.is_empty() {
            document.html.push_str(&appended);
        }
        output.push(document);
    }
    Ok(output)
}

fn title_items(documents: &[WorkItem], step: &Value) -> Vec<WorkItem> {
    let patterns = step_strings(step, "titleIdPatterns");
    let regexes = if patterns.is_empty() {
        vec![Regex::new(r"(?i)\b(?:CUSA|PPSA)\d{5}\b").unwrap()]
    } else {
        patterns
            .iter()
            .filter_map(|pattern| Regex::new(pattern).ok())
            .collect()
    };
    let region_map = step_region_map(step);
    let mut out = Vec::new();
    for document in documents {
        let text = collapse(
            &Html::parse_document(&document.html)
                .root_element()
                .text()
                .collect::<Vec<_>>()
                .join(" "),
        );
        for regex in &regexes {
            for id in regex.find_iter(&text) {
                let title_id = id.as_str().to_ascii_uppercase();
                if valid_title_id(&title_id) {
                    let local = text
                        .get(id.start()..id.end().saturating_add(24).min(text.len()))
                        .unwrap_or(id.as_str());
                    out.push(WorkItem {
                        title_id,
                        name: document.name.clone(),
                        region: region_from(local, &region_map),
                        image: document.image.clone(),
                        url: document.url.clone(),
                        parent_url: document.parent_url.clone(),
                        archive_password: document.archive_password.clone(),
                        diagnostics: document.diagnostics.clone(),
                        ..Default::default()
                    });
                }
            }
        }
    }
    out
}

fn parse_package_sections(documents: &[WorkItem], requested: &str, step: &Value) -> Vec<WorkItem> {
    let roots = step_strings(step, "rootSelectors");
    let roots = if roots.is_empty() {
        vec![".entry-content".to_owned()]
    } else {
        roots
    };
    let allowed_hosts = step_strings(step, "allowedLinkHosts")
        .into_iter()
        .map(|host| host.trim_start_matches("www.").to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let title_patterns = step_strings(step, "titleIdPatterns");
    let id_regex = if title_patterns.is_empty() {
        Regex::new(r"(?i)\b(?:CUSA|PPSA)\d{5}\b").unwrap()
    } else {
        Regex::new(&title_patterns.join("|"))
            .unwrap_or_else(|_| Regex::new(r"(?i)\b(?:CUSA|PPSA)\d{5}\b").unwrap())
    };
    let kind_rules = step_kind_rules(step);
    let version_regex = Regex::new(
        step.get("versionPattern")
            .and_then(Value::as_str)
            .unwrap_or(r"(?i)\bv?(\d{2}\.\d{3}(?:\.\d{3})?)\b"),
    )
    .ok();
    let size_regex = step
        .get("sizePattern")
        .and_then(Value::as_str)
        .and_then(|pattern| Regex::new(pattern).ok());
    let region_map = step_region_map(step);
    let group_mirrors = step
        .get("groupMirrorsBySection")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let preserve_source = step
        .get("preserveSourcePage")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let anchor = Selector::parse("a[href]").unwrap();
    let mut output = Vec::new();
    let mut seen_urls = HashSet::new();
    for document in documents {
        let html = Html::parse_document(&document.html);
        for value in roots.iter().filter(|selector| bounded_selector(selector)) {
            let Ok(selector) = Selector::parse(value) else {
                continue;
            };
            for root in html.select(&selector) {
                let root_html = root.html();
                for link in root.select(&anchor) {
                    let Some(href) = link.value().attr("href") else {
                        continue;
                    };
                    let Ok(url) = Url::parse(&document.url).and_then(|base| base.join(href)) else {
                        continue;
                    };
                    if !matches!(url.scheme(), "http" | "https") {
                        continue;
                    }
                    let label = collapse(&link.text().collect::<Vec<_>>().join(" "));
                    if unrelated_package_link(&url, &label) {
                        continue;
                    }
                    let host = url
                        .host_str()
                        .unwrap_or_default()
                        .trim_start_matches("www.")
                        .to_ascii_lowercase();
                    if !allowed_hosts.contains(&host) || !seen_urls.insert(url.to_string()) {
                        continue;
                    }
                    let preceding = preceding_text(&root_html, href);
                    let paragraph = paragraph_around(&root_html, href);
                    let found_id = id_regex
                        .find_iter(&preceding)
                        .last()
                        .map(|value| value.as_str().to_ascii_uppercase())
                        .filter(|value| valid_title_id(value));
                    if found_id.as_deref() != Some(&requested.to_ascii_uppercase()) {
                        continue;
                    }
                    let kind = {
                        let local = if paragraph.len() < 48 {
                            let start = preceding.len().saturating_sub(120);
                            format!("{} {}", utf8_window(&preceding, start, preceding.len()), paragraph)
                        } else {
                            paragraph.clone()
                        };
                        classify_package_kind(&local, &kind_rules)
                    };
                    let package_version = version_regex
                        .as_ref()
                        .and_then(|regex| regex.captures(&preceding))
                        .and_then(|capture| capture.get(1))
                        .map(|value| value.as_str().to_owned())
                        .unwrap_or_else(|| infer_version(&preceding));
                    let expected_size = size_regex
                        .as_ref()
                        .and_then(|regex| regex.captures(&preceding))
                        .and_then(|capture| capture.get(1))
                        .and_then(|value| parse_size_label(value.as_str()));
                    let group_key = if group_mirrors {
                        format!(
                            "{}:{}:{}:{}",
                            document.url, requested, kind, package_version
                        )
                    } else {
                        format!("{}:{}:{}:{}:{}", document.url, requested, kind, package_version, url)
                    };
                    let group_id = format!("section-{}", &sha256(group_key.as_bytes())[..20]);
                    output.push(WorkItem {
                        url: url.to_string(),
                        parent_url: document.url.clone(),
                        source_page: if preserve_source {
                            document.url.clone()
                        } else {
                            url.to_string()
                        },
                        intermediate_url: url.to_string(),
                        name: document.name.clone(),
                        title_id: requested.to_owned(),
                        region: region_from(&preceding, &region_map),
                        kind,
                        package_version,
                        firmware: infer_firmware(&preceding),
                        group_id,
                        label,
                        expected_size,
                        archive_password: document.archive_password.clone(),
                        diagnostics: document.diagnostics.clone(),
                        ..Default::default()
                    });
                }
            }
        }
    }
    output
}

fn archive_part_with(value: &str, patterns: &[String]) -> Option<u32> {
    let defaults = [
        r"(?i)\bpart[ ._-]*0*(\d+)\b".to_owned(),
        r"(?i)\.part0*(\d+)\.rar(?:$|[?#])".to_owned(),
        r"(?i)\.0*(\d{3})(?:$|[?#])".to_owned(),
        r"(?i)\.r(\d{2})(?:$|[?#])".to_owned(),
        r"(?i)\.z(\d{2})(?:$|[?#])".to_owned(),
    ];
    let used = if patterns.is_empty() {
        defaults.as_slice()
    } else {
        patterns
    };
    let lower = value.to_ascii_lowercase();
    let numeric_ok = lower.contains("part")
        || lower.contains("split")
        || lower.contains("volume")
        || lower.contains(".001");
    used.iter().find_map(|pattern| {
        if pattern.contains("\\d{3}") && !numeric_ok {
            return None;
        }
        Regex::new(pattern)
            .ok()?
            .captures(value)?
            .get(1)?
            .as_str()
            .parse()
            .ok()
            .filter(|part| *part > 0)
    })
}

fn archive_hint(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains(".7z") {
        "7z"
    } else if lower.contains(".zip") || Regex::new(r"\.z\d{2}(?:$|[?#])").unwrap().is_match(&lower)
    {
        "zip"
    } else if lower.contains(".rar") || Regex::new(r"\.r\d{2}(?:$|[?#])").unwrap().is_match(&lower)
    {
        "rar"
    } else if Regex::new(r"\.\d{3}(?:$|[?#])").unwrap().is_match(&lower) {
        "numeric-split"
    } else {
        "unknown"
    }
    .into()
}

fn size_from_text(text: &str) -> Option<u64> {
    let regex = Regex::new(r"(?i)([0-9]+(?:\.[0-9]+)?)\s*(KB|MB|GB|TB)").ok()?;
    let found = regex.captures_iter(text).find_map(|capture| {
        parse_size_label(&format!("{} {}", capture.get(1)?.as_str(), capture.get(2)?.as_str()))
    });
    found
}

fn nearest_archive_meta(root_html: &str, needle: &str) -> (String, Option<u64>) {
    let name_re = match Regex::new(
        r"(?i)((?:\[[^\]]+\]-?)?[A-Za-z0-9._-]+\.(?:part\d+\.(?:rar|zip|7z)|r\d{2}|z\d{2}|7z\.\d+|zip\.\d+|\d{3}|rar|zip|7z))",
    ) {
        Ok(value) => value,
        Err(_) => return (String::new(), None),
    };
    let at = match root_html.find(needle).or_else(|| {
        needle
            .rsplit('/')
            .next()
            .filter(|tail| !tail.is_empty())
            .and_then(|tail| root_html.find(tail))
    }) {
        Some(value) => value,
        None => return (String::new(), None),
    };
    let start = at.saturating_sub(400);
    let end = (at + needle.len() + 200).min(root_html.len());
    let before = collapse(&strip_tags(utf8_window(root_html, start, at)));
    let after = collapse(&strip_tags(utf8_window(root_html, at, end)));
    let name = name_re
        .find_iter(&before)
        .last()
        .or_else(|| name_re.find(utf8_window(&after, 0, 200)))
        .map(|found| found.as_str().to_owned())
        .unwrap_or_default();
    let window_start = name_re
        .find_iter(&before)
        .last()
        .map(|found| found.end())
        .unwrap_or(before.len().saturating_sub(120));
    let size = size_from_text(utf8_window(&before, window_start, before.len()))
        .or_else(|| size_from_text(utf8_window(&after, 0, 200)));
    (name, size)
}

fn placeholder_href(href: &str) -> bool {
    let trimmed = href.trim();
    trimmed.is_empty()
        || trimmed == "#"
        || trimmed.eq_ignore_ascii_case("javascript:void(0)")
        || trimmed.eq_ignore_ascii_case("javascript:void(0);")
        || trimmed.eq_ignore_ascii_case("javascript:;")
}

fn parse_hoster_links(documents: &[WorkItem], step: &Value) -> Vec<WorkItem> {
    let allowed_hosts = step_strings(step, "allowedHosts")
        .into_iter()
        .map(|host| host.trim_start_matches("www.").to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let roots = step_strings(step, "rootSelectors");
    let roots = if roots.is_empty() {
        vec!["body".to_owned()]
    } else {
        roots
    };
    let domain_attr = step
        .pointer("/secureLinkAttributes/domain")
        .and_then(Value::as_str)
        .unwrap_or("data-domain");
    let path_attr = step
        .pointer("/secureLinkAttributes/path")
        .and_then(Value::as_str)
        .unwrap_or("data-path");
    let part_patterns = step_strings(step, "partPatterns");
    let expand_hosts = step_strings(step, "expandHosts")
        .into_iter()
        .map(|host| host.trim_start_matches("www.").to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let maximum_links = step
        .get("maximumLinks")
        .and_then(Value::as_u64)
        .unwrap_or(MAX_ITEMS as u64)
        .min(MAX_ITEMS as u64) as usize;
    let anchor = Selector::parse("a").unwrap();
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for document in documents {
        let before = output.len();
        let mut document = document.clone();
        if let Some(password) = Regex::new(r"(?i)\bPassword\s*:\s*(\S+)").unwrap()
            .captures(&strip_tags(&document.html)).and_then(|c|c.get(1).map(|s|s.as_str().to_owned())) {
            document.archive_password = password.chars().take(128).collect();
        }
        let document_host = Url::parse(&document.url)
            .ok()
            .and_then(|url| url.host_str().map(|host| host.trim_start_matches("www.").to_ascii_lowercase()))
            .unwrap_or_default();
        if document.html.trim().is_empty() && allowed_hosts.contains(&document_host) {
            let mut passthrough = document.clone();
            passthrough.hoster = document_host;
            if passthrough.archive_format_hint.is_empty()
                || passthrough.archive_format_hint == "unknown"
            {
                passthrough.archive_format_hint = "rar".into();
            }
            output.push(passthrough);
            continue;
        }
        let html = Html::parse_document(&document.html);
        let mut skipped_hosts = Vec::new();
        for value in roots.iter().filter(|selector| bounded_selector(selector)) {
            let Ok(selector) = Selector::parse(value) else {
                continue;
            };
            for root in html.select(&selector) {
                let root_html = root.html();
                for link in root.select(&anchor) {
                    let href = link.value().attr("href").unwrap_or_default();
                    let raw = if let Some(url) = source_link(link, &document.url) { url } else if placeholder_href(href) {
                        match (link.value().attr(domain_attr), link.value().attr(path_attr)) {
                            (Some(domain), Some(path)) => format!("{domain}{path}"),
                            _ => continue,
                        }
                    } else {
                        href.to_owned()
                    };
                    let Ok(url) = Url::parse(&raw) else { continue };
                    if !matches!(url.scheme(), "http" | "https") {
                        continue;
                    }
                    let host = url
                        .host_str()
                        .unwrap_or_default()
                        .trim_start_matches("www.")
                        .to_ascii_lowercase();
                    if expand_hosts.contains(&host) {
                        if !seen.insert(url.to_string()) || output.len() >= maximum_links {
                            continue;
                        }
                        output.push(WorkItem {
                            url: url.to_string(),
                            parent_url: document.url.clone(),
                            source_page: document.source_page.clone(),
                            html: String::new(),
                            title_id: document.title_id.clone(),
                            name: document.name.clone(),
                            region: document.region.clone(),
                            kind: document.kind.clone(),
                            package_version: document.package_version.clone(),
                            firmware: document.firmware.clone(),
                            group_id: document.group_id.clone(),
                            expected_size: document.expected_size,
                            archive_password: document.archive_password.clone(),
                            diagnostics: document.diagnostics.clone(),
                            ..Default::default()
                        });
                        continue;
                    }
                    if !allowed_hosts.contains(&host) {
                        if !host.is_empty()
                            && !skipped_hosts
                                .iter()
                                .any(|existing: &String| existing == &host)
                        {
                            skipped_hosts.push(host);
                        }
                        continue;
                    }
                    if !seen.insert(url.to_string()) || output.len() >= maximum_links {
                        continue;
                    }
                    let label = collapse(&link.text().collect::<Vec<_>>().join(" "));
                    let needle = if placeholder_href(href) {
                        url.as_str()
                    } else {
                        href
                    };
                    let (near_name, near_size) = nearest_archive_meta(&root_html, needle);
                    let blob = format!("{label} {near_name} {url}");
                    let file_name = {
                        let from_text = near_name.clone();
                        if !from_text.is_empty() {
                            from_text
                        } else {
                            url.path_segments()
                                .and_then(|mut parts| parts.next_back())
                                .filter(|name| {
                                    !name.is_empty()
                                        && name.contains('.')
                                        && !name.eq_ignore_ascii_case("dir")
                                })
                                .unwrap_or_default()
                                .to_owned()
                        }
                    };
                    let part = archive_part_with(&blob, &part_patterns)
                        .or_else(|| archive_part_with(&file_name, &part_patterns));
                    let mirror_id = format!(
                        "mirror-{}",
                        &sha256(format!("{}:{}", document.group_id, host).as_bytes())[..16]
                    );
                    let set = if part.is_some() || file_name.to_ascii_lowercase().contains(".part")
                    {
                        format!(
                            "archive-{}",
                            &sha256(format!("{}:{}", document.group_id, host).as_bytes())[..20]
                        )
                    } else {
                        String::new()
                    };
                    let diagnostics = document.diagnostics.clone();
                    output.push(WorkItem {
                        url: url.to_string(),
                        parent_url: document.url.clone(),
                        source_page: document.source_page.clone(),
                        intermediate_url: document.url.clone(),
                        title_id: document.title_id.clone(),
                        name: document.name.clone(),
                        region: document.region.clone(),
                        kind: document.kind.clone(),
                        package_version: document.package_version.clone(),
                        firmware: document.firmware.clone(),
                        group_id: document.group_id.clone(),
                        hoster: host,
                        label: if !file_name.is_empty() {
                            file_name.clone()
                        } else if label.is_empty() {
                            "Hoster landing".into()
                        } else {
                            label
                        },
                        archive_part_number: part,
                        archive_set_id: set,
                        archive_file_name: file_name,
                        archive_format_hint: {
                            let hint = archive_hint(&blob);
                            if hint == "unknown" {
                                "rar".into()
                            } else {
                                hint
                            }
                        },
                        mirror_id,
                        expected_size: near_size.or(document.expected_size),
                        archive_password: document.archive_password.clone(),
                        diagnostics,
                        ..Default::default()
                    });
                }
            }
        }
        if let Ok(url_re) = Regex::new(
            r#"https?://(?:www\.)?[A-Za-z0-9.-]+\.[A-Za-z]{2,8}(?:/[^\s"'<>]*)?(?:\?[^\s"'<>]*)?"#,
        ) {
            for found in url_re.find_iter(&document.html) {
                let raw = found
                    .as_str()
                    .trim_end_matches(|character: char| {
                        matches!(character, '.' | ',' | ')' | ']' | ';' | ':')
                            || character == '\u{2014}'
                    })
                    .to_owned();
                let Ok(url) = Url::parse(&raw) else {
                    continue;
                };
                if !matches!(url.scheme(), "http" | "https") {
                    continue;
                }
                let host = url
                    .host_str()
                    .unwrap_or_default()
                    .trim_start_matches("www.")
                    .to_ascii_lowercase();
                if !allowed_hosts.contains(&host)
                    || !seen.insert(url.to_string())
                    || output.len() >= maximum_links
                {
                    continue;
                }
                let (near_name, near_size) = nearest_archive_meta(&document.html, url.as_str());
                let blob = format!("{near_name} {url}");
                let file_name = near_name.clone();
                let part = archive_part_with(&blob, &part_patterns)
                    .or_else(|| archive_part_with(&file_name, &part_patterns));
                let set = if part.is_some() || file_name.to_ascii_lowercase().contains(".part") {
                    format!(
                        "archive-{}",
                        &sha256(format!("{}:{}", document.group_id, host).as_bytes())[..20]
                    )
                } else {
                    String::new()
                };
                output.push(WorkItem {
                    url: url.to_string(),
                    parent_url: document.url.clone(),
                    source_page: document.source_page.clone(),
                    intermediate_url: document.url.clone(),
                    title_id: document.title_id.clone(),
                    name: document.name.clone(),
                    region: document.region.clone(),
                    kind: document.kind.clone(),
                    package_version: document.package_version.clone(),
                    firmware: document.firmware.clone(),
                    group_id: document.group_id.clone(),
                    hoster: host.clone(),
                    label: if file_name.is_empty() {
                        "Hoster landing".into()
                    } else {
                        file_name.clone()
                    },
                    archive_part_number: part,
                    archive_set_id: set,
                    archive_file_name: file_name.clone(),
                    archive_format_hint: {
                        let hint = archive_hint(&blob);
                        if hint == "unknown" {
                            "rar".into()
                        } else {
                            hint
                        }
                    },
                    mirror_id: format!(
                        "mirror-{}",
                        &sha256(format!("{}:{}", document.group_id, host).as_bytes())[..16]
                    ),
                    expected_size: near_size.or(document.expected_size),
                    archive_password: document.archive_password.clone(),
                    diagnostics: document.diagnostics.clone(),
                    ..Default::default()
                });
            }
        }
        if !skipped_hosts.is_empty() {
            let note = format!("skipped unknown host: {}", skipped_hosts.join(", "));
            for item in output.iter_mut().filter(|item| item.parent_url == document.url) {
                if !item.diagnostics.iter().any(|value| value == &note) {
                    item.diagnostics.push(note.clone());
                }
            }
        }
        if output.len() == before && allowed_hosts.contains(&document_host) {
            let mut passthrough = document.clone();
            passthrough.hoster = document_host;
            if passthrough.archive_format_hint.is_empty()
                || passthrough.archive_format_hint == "unknown"
            {
                passthrough.archive_format_hint = "rar".into();
            }
            output.push(passthrough);
        }
    }
    output.sort_by(|left, right| {
        left.archive_set_id
            .cmp(&right.archive_set_id)
            .then(left.archive_part_number.unwrap_or(0).cmp(&right.archive_part_number.unwrap_or(0)))
            .then(left.url.cmp(&right.url))
    });
    let mut parts_by_set = HashMap::<String, Vec<u32>>::new();
    for item in &output {
        if let (Some(part), false) = (item.archive_part_number, item.archive_set_id.is_empty()) {
            parts_by_set
                .entry(item.archive_set_id.clone())
                .or_default()
                .push(part);
        }
    }
    let mut set_meta = HashMap::<String, (Option<u32>, Vec<String>)>::new();
    for (set, mut parts) in parts_by_set {
        parts.sort_unstable();
        parts.dedup();
        let mut gaps = Vec::new();
        let mut duplicates = false;
        let mut seen = HashSet::new();
        for part in &parts {
            if !seen.insert(*part) {
                duplicates = true;
            }
        }
        let max = parts.last().copied().unwrap_or(0);
        if !parts.is_empty() && parts[0] != 1 {
            gaps.push("1".into());
        }
        for expected in 1..=max {
            if !parts.contains(&expected) {
                gaps.push(format!("{expected}"));
            }
        }
        let mut notes = Vec::new();
        if duplicates {
            notes.push("duplicate archive part numbers".into());
        }
        if !gaps.is_empty() {
            notes.push(format!(
                "incomplete archive set; missing Part.{}",
                gaps.join(", Part.")
            ));
        }
        let split_named = output.iter().any(|item| {
            item.archive_set_id == set
                && item
                    .archive_file_name
                    .to_ascii_lowercase()
                    .contains(".part")
        });
        let count = if gaps.is_empty() && parts[0] == 1 && !(split_named && max == 1) {
            Some(max)
        } else {
            None
        };
        if split_named && max == 1 {
            notes.push("incomplete archive set; missing complementary RAR volumes".into());
        }
        set_meta.insert(set, (count, notes));
    }
    for item in &mut output {
        if let Some((count, notes)) = set_meta.get(&item.archive_set_id) {
            item.archive_part_count = *count;
            for note in notes {
                if !item.diagnostics.iter().any(|value| value == note) {
                    item.diagnostics.push(note.clone());
                }
            }
        }
    }
    output
}

fn parse_download_links(
    descriptor: &Descriptor,
    documents: &[WorkItem],
    step: &Value,
) -> Vec<WorkItem> {
    let anchor_selector = Selector::parse("a[href]").unwrap();
    let image_selector = Selector::parse("img[src], img[data-src]").unwrap();
    let text_values = step_strings(step, "downloadAnchorTexts")
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let image_values = step_strings(step, "downloadImageContains")
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let prefixes = step_strings(step, "downloadPathPrefixes");
    let same_origin = step.get("sameOrigin").and_then(Value::as_bool) == Some(true);
    let mut output = Vec::new();
    for document in documents {
        let html = Html::parse_document(&document.html);
        let Ok(base) = Url::parse(&document.url) else {
            continue;
        };
        for anchor in html.select(&anchor_selector) {
            let Some(href) = anchor.value().attr("href") else {
                continue;
            };
            let Ok(url) = base.join(href) else { continue };
            if !allowed(&origins(descriptor), &url)
                || (same_origin && !same_site_origin(&url, &base))
            {
                continue;
            }
            let text = collapse(&anchor.text().collect::<Vec<_>>().join(" ")).to_ascii_lowercase();
            let image_match = anchor.select(&image_selector).any(|image| {
                image
                    .value()
                    .attr("src")
                    .or_else(|| image.value().attr("data-src"))
                    .is_some_and(|src| {
                        image_values
                            .iter()
                            .any(|needle| src.to_ascii_lowercase().contains(needle))
                    })
            });
            let matched = image_match
                || text_values
                    .iter()
                    .any(|needle| text == *needle || text.contains(needle))
                || prefixes.iter().any(|prefix| url.path().starts_with(prefix));
            #[cfg(test)]
            if text.contains("download") || url.path().contains("dll-") || image_match {
                eprintln!(
                    "download candidate matched={matched} text={text:?} url={}",
                    url
                );
            }
            if matched {
                output.push(WorkItem {
                    url: url.to_string(),
                    parent_url: document.url.clone(),
                    source_page: document.url.clone(),
                    name: document.name.clone(),
                    image: document.image.clone(),
                    ..Default::default()
                });
                break;
            }
        }
    }
    output
}

fn parse_package_tables(documents: &[WorkItem], title_id: &str, step: &Value) -> Vec<WorkItem> {
    let table_selector = Selector::parse("table").unwrap();
    let row_selector = Selector::parse("tr").unwrap();
    let cell_selector = Selector::parse("th, td").unwrap();
    let anchor_selector = Selector::parse("a[href]").unwrap();
    let blocked = step_strings(step, "blockedHosts")
        .into_iter()
        .map(|item| item.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let suffixes = step_strings(step, "blockedHostSuffixes");
    let drop_keys = step_strings(step, "dropQueryKeys")
        .into_iter()
        .map(|item| item.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let labels = step_strings(step, "hostLabels")
        .into_iter()
        .filter_map(|item| {
            item.split_once('=')
                .map(|(host, label)| (host.to_ascii_lowercase(), label.to_owned()))
        })
        .collect::<HashMap<_, _>>();
    let id_regex = Regex::new(r"(?i)\b(?:CUSA|PPSA)\d{5}\b").unwrap();
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for document in documents {
        let html = Html::parse_document(&document.html);
        for table in html.select(&table_selector) {
            let table_text = collapse(&table.text().collect::<Vec<_>>().join(" "));
            let table_ids = id_regex
                .find_iter(&table_text)
                .map(|item| item.as_str().to_ascii_uppercase())
                .collect::<Vec<_>>();
            if !table_ids.iter().any(|id| id == title_id) {
                continue;
            }
            let table_region = infer_region(&table_text);
            let mut running_id = (table_ids.len() == 1)
                .then(|| title_id.to_owned())
                .unwrap_or_default();
            for row in table.select(&row_selector) {
                let row_text = collapse(&row.text().collect::<Vec<_>>().join(" "));
                let row_ids = id_regex
                    .find_iter(&row_text)
                    .map(|item| item.as_str().to_ascii_uppercase())
                    .collect::<Vec<_>>();
                if !row_ids.is_empty() {
                    if !row_ids.iter().any(|id| id == title_id) {
                        running_id = row_ids[0].clone();
                        continue;
                    }
                    running_id = title_id.to_owned();
                } else if (!running_id.is_empty() && running_id != title_id)
                    || (table_ids.len() > 1 && running_id.is_empty())
                {
                    continue;
                }
                let first = row
                    .select(&cell_selector)
                    .next()
                    .map(|cell| collapse(&cell.text().collect::<Vec<_>>().join(" ")))
                    .unwrap_or_default();
                let lower = first.to_ascii_lowercase();
                if ["version", "voice", "password", "note", "info"]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix))
                {
                    continue;
                }
                let kind = infer_kind(if first.is_empty() { &row_text } else { &first });
                if kind == "unknown" && first.is_empty() {
                    continue;
                }
                let package_version = infer_version(&row_text);
                let firmware = infer_firmware(&row_text);
                let region = {
                    let row_region = infer_region(&row_text);
                    if row_region.is_empty() {
                        table_region.clone()
                    } else {
                        row_region
                    }
                };
                let group_id = format!(
                    "row-{}",
                    &sha256(
                        format!(
                            "{title_id}\n{}\n{first}\n{kind}\n{package_version}",
                            document.url
                        )
                        .as_bytes()
                    )[..20]
                );
                for anchor in row.select(&anchor_selector) {
                    let Some(href) = anchor.value().attr("href") else {
                        continue;
                    };
                    let Ok(mut url) = Url::parse(&document.url).and_then(|base| base.join(href))
                    else {
                        continue;
                    };
                    if !matches!(url.scheme(), "http" | "https") {
                        continue;
                    }
                    let host = url
                        .host_str()
                        .unwrap_or_default()
                        .trim_start_matches("www.")
                        .to_ascii_lowercase();
                    if blocked.contains(&host)
                        || suffixes.iter().any(|suffix| host.ends_with(suffix))
                    {
                        continue;
                    }
                    let kept = url
                        .query_pairs()
                        .filter(|(key, _)| !drop_keys.contains(&key.to_ascii_lowercase()))
                        .map(|(key, value)| (key.into_owned(), value.into_owned()))
                        .collect::<Vec<_>>();
                    url.set_query(None);
                    if !kept.is_empty() {
                        url.query_pairs_mut().extend_pairs(kept);
                    }
                    url.set_fragment(None);
                    if !seen.insert(url.to_string()) {
                        continue;
                    }
                    let anchor_label = collapse(&anchor.text().collect::<Vec<_>>().join(" "));
                    let hoster = labels.get(&host).cloned().unwrap_or_else(|| {
                        if anchor_label.is_empty() {
                            host.clone()
                        } else {
                            anchor_label
                        }
                    });
                    output.push(WorkItem {
                        url: url.to_string(),
                        source_page: document.url.clone(),
                        title_id: title_id.to_owned(),
                        name: document.name.clone(),
                        region: region.clone(),
                        kind: kind.clone(),
                        package_version: package_version.clone(),
                        firmware: firmware.clone(),
                        group_id: group_id.clone(),
                        hoster: hoster.clone(),
                        label: hoster,
                        ..Default::default()
                    });
                }
            }
        }
    }
    output
}

fn emit_packages(
    descriptor: &Descriptor,
    items: Vec<WorkItem>,
    step: &Value,
) -> Vec<SourcePackage> {
    let access = step
        .get("accessType")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    items
        .into_iter()
        .filter_map(|item| {
            Url::parse(&item.url).ok()?;
            let candidate_id = format!("{}:{}", descriptor.id, &sha256(item.url.as_bytes())[..24]);
            Some(SourcePackage {
                kind: normalize_kind(&item.kind),
                label: if item.label.is_empty() {
                    "Package mirror".into()
                } else {
                    item.label
                },
                url: item.url,
                access_type: normalize_access(access),
                source_id: descriptor.id.clone(),
                source_name: catalog_name(&descriptor.id, &descriptor.name),
                source_version: descriptor.version.clone(),
                candidate_id,
                group_id: item.group_id,
                hoster: item.hoster,
                version: item.package_version,
                firmware: item.firmware,
                source_page_url: item.source_page,
                expected_size: item.expected_size,
                expected_sha256: String::new(),
                expected_content_id: String::new(),
                archive_set_id: (!item.archive_set_id.is_empty()).then_some(item.archive_set_id),
                archive_part_number: item.archive_part_number,
                archive_part_count: item.archive_part_count,
                archive_file_name: (!item.archive_file_name.is_empty())
                    .then_some(item.archive_file_name),
                archive_password: (!item.archive_password.is_empty()).then_some(item.archive_password),
                archive_format_hint: (!item.archive_format_hint.is_empty())
                    .then_some(item.archive_format_hint),
                mirror_id: (!item.mirror_id.is_empty()).then_some(item.mirror_id),
                intermediate_url: (!item.intermediate_url.is_empty())
                    .then_some(item.intermediate_url),
                referer: (!item.parent_url.is_empty()).then_some(item.parent_url),
                diagnostics: item.diagnostics,
            })
        })
        .take(MAX_RESULTS)
        .collect()
}

fn dedupe(items: Vec<WorkItem>, field: &str) -> Vec<WorkItem> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| {
            let owned;
            let value = match field {
                "value" => &item.value,
                "titleId" => {
                    owned = if item.region.is_empty() {
                        item.title_id.clone()
                    } else {
                        format!("{}:{}", item.title_id, item.region)
                    };
                    &owned
                }
                _ => &item.url,
            };
            !value.is_empty() && seen.insert(value.to_ascii_lowercase())
        })
        .collect()
}

fn normalize_name(value: &str) -> String {
    let folded = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();
    collapse(&folded)
        .split(' ')
        .filter(|word| !matches!(*word, "ps4" | "ps5" | "pkg" | "fpkg" | "iso" | "game"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn match_score(target: &str, candidate: &str) -> f64 {
    let left = normalize_name(target);
    let right = normalize_name(candidate);
    let wanted = left.split_whitespace().collect::<HashSet<_>>();
    let actual = right.split_whitespace().collect::<HashSet<_>>();
    let coverage = if wanted.is_empty() {
        0.0
    } else {
        wanted.intersection(&actual).count() as f64 / wanted.len() as f64
    };
    let ratio = strsim::normalized_levenshtein(&left, &right);
    let lower = candidate.to_ascii_lowercase();
    let platform = if lower
        .split_whitespace()
        .any(|word| word == "ps4" || word == "ps5")
    {
        0.12
    } else {
        0.0
    };
    (coverage * 0.68 + ratio * 0.20 + platform - if lower.contains("demo") && !left.contains("demo") { 0.20 } else { 0.0 }).clamp(0.0, 1.0)
}

fn valid_title_id(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    upper.len() == 9
        && (upper.starts_with("CUSA") || upper.starts_with("PPSA"))
        && upper.as_bytes()[4..].iter().all(|byte| byte.is_ascii_digit())
}

fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded_selector(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 160
        && !trimmed.contains(':')
        && !trimmed.contains('*')
        && trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(
                    character,
                    ' ' | ',' | '.' | '#' | '-' | '_' | '[' | ']' | '=' | '"' | '\''
                )
        })
}

fn inert_html(input: &str) -> String {
    let scripts = Regex::new(
        r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>|<iframe\b[^>]*>.*?</iframe>|<object\b[^>]*>.*?</object>|<embed\b[^>]*/?>|<link\b[^>]*/?>",
    )
    .unwrap();
    let events = Regex::new(r#"(?i)\son\w+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#).unwrap();
    events
        .replace_all(&scripts.replace_all(input, ""), "")
        .into_owned()
}

fn inject_decoded_fragment(html: &str, attribute: &str, payload: &str, decoded: &str) -> String {
    let quoted = [
        format!("{attribute}=\"{payload}\""),
        format!("{attribute}='{payload}'"),
    ];
    for needle in quoted {
        if let Some(at) = html.find(&needle) {
            if let Some(rel) = html[at..].find('>') {
                let insert_at = at + rel + 1;
                return format!("{}{}{}", &html[..insert_at], decoded, &html[insert_at..]);
            }
        }
    }
    format!("{html}<div class=\"post-body entry-content\">{decoded}</div>")
}

fn preceding_text(html: &str, marker: &str) -> String {
    let at = html.find(marker).unwrap_or(html.len());
    collapse(&strip_tags(&html[..at]))
}

fn paragraph_around(html: &str, marker: &str) -> String {
    let at = html.find(marker).unwrap_or(0);
    let start = html[..at].rfind("<p").unwrap_or(at.saturating_sub(400));
    let end = html[at..]
        .find("</p>")
        .map(|offset| at + offset + 4)
        .unwrap_or((at + 500).min(html.len()));
    collapse(&strip_tags(&html[start..end]))
}

fn classify_package_kind(paragraph: &str, rules: &[(Regex, String)]) -> String {
    let lower = paragraph.to_ascii_lowercase();
    let game_line = Regex::new(r"(?i)\bgame\s*\(").ok().is_some_and(|regex| regex.is_match(&lower))
        || lower.starts_with("game ");
    if game_line {
        if lower.contains("exfat") {
            return "exfat".into();
        }
        return "base".into();
    }
    if lower.contains("backport") || lower.contains("bp 4.xx") || lower.contains("4.xx") {
        return "backport".into();
    }
    let kind = last_kind(paragraph, rules);
    if !kind.is_empty() {
        return kind;
    }
    let inferred = infer_kind(paragraph);
    if inferred == "unknown" {
        "base".into()
    } else {
        inferred
    }
}

fn strip_tags(value: &str) -> String {
    let regex = Regex::new(r"(?s)<[^>]+>").unwrap();
    regex.replace_all(value, " ").into_owned()
}

fn step_region_map(step: &Value) -> HashMap<String, String> {
    step.get("regionMap")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|mapped| (key.to_ascii_uppercase(), mapped.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn region_from(text: &str, map: &HashMap<String, String>) -> String {
    let upper = text.to_ascii_uppercase();
    for (from, to) in map {
        if upper.contains(from) {
            return to.clone();
        }
    }
    infer_region(text)
}

fn step_kind_rules(step: &Value) -> Vec<(Regex, String)> {
    step.get("kindRules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| {
                    Some((
                        Regex::new(rule.get("pattern").and_then(Value::as_str)?).ok()?,
                        rule.get("kind")?.as_str()?.to_ascii_lowercase(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn last_kind(text: &str, rules: &[(Regex, String)]) -> String {
    let mut best: Option<(usize, String)> = None;
    for (regex, kind) in rules {
        for matched in regex.find_iter(text) {
            if best
                .as_ref()
                .map(|(offset, _)| matched.start() >= *offset)
                .unwrap_or(true)
            {
                best = Some((matched.start(), kind.clone()));
            }
        }
    }
    best.map(|(_, kind)| kind).unwrap_or_default()
}

fn unrelated_package_link(url: &Url, label: &str) -> bool {
    let path = url.path().to_ascii_lowercase();
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    let label = label.to_ascii_lowercase();
    path.contains("/guide")
        || path.contains("/comment")
        || path.contains("/tag/")
        || path.contains("/category/")
        || host.contains("twitter.")
        || host.contains("facebook.")
        || host.contains("discord.")
        || matches!(
            label.as_str(),
            "guide" | "guides" | "comment" | "comments" | "share" | "facebook" | "twitter"
        )
}

fn parse_size_label(value: &str) -> Option<u64> {
    let regex = Regex::new(r"(?i)^\s*([0-9]+(?:\.[0-9]+)?)\s*(KB|MB|GB|TB)\s*$").ok()?;
    let capture = regex.captures(value.trim())?;
    let amount: f64 = capture.get(1)?.as_str().parse().ok()?;
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    let multiplier = match capture.get(2)?.as_str().to_ascii_uppercase().as_str() {
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((amount * multiplier) as u64)
}

fn normalize_region(value: &str) -> String {
    let upper = value.to_ascii_uppercase();
    if upper.contains("USA") || upper.contains(" US") || upper == "US" {
        "US".into()
    } else if upper.contains("EUR") || upper.contains("EUROPE") || upper == "EU" {
        "EU".into()
    } else if upper.contains("JPN") || upper.contains("JAPAN") || upper == "JP" {
        "JP".into()
    } else if upper.contains("ASIA") || upper == "AS" {
        "AS".into()
    } else {
        value.trim().to_owned()
    }
}

fn infer_region(value: &str) -> String {
    normalize_region(value)
        .split_whitespace()
        .find(|item| matches!(*item, "US" | "EU" | "JP" | "AS"))
        .unwrap_or_default()
        .to_owned()
}

fn infer_kind(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("backport")
        || lower.contains("bp 4.xx")
        || (lower.contains("fw required") && lower.contains("4.xx"))
    {
        "backport".into()
    } else if lower.contains("exfat") {
        "exfat".into()
    } else if lower.contains("dlc") || lower.contains("add-on") {
        "dlc".into()
    } else if lower.contains("update")
        && !lower.contains("game + update")
        && !lower.contains("game update")
    {
        "update".into()
    } else if lower.contains("game") || lower.contains("base") {
        "base".into()
    } else {
        "unknown".into()
    }
}

fn infer_version(value: &str) -> String {
    Regex::new(r"(?i)\bv(?:er(?:sion)?)?\s*([0-9]+(?:\.[0-9]+)+)")
        .unwrap()
        .captures(value)
        .and_then(|capture| capture.get(1))
        .map(|value| value.as_str().to_owned())
        .unwrap_or_default()
}

fn infer_firmware(value: &str) -> String {
    Regex::new(r"(?i)\b(?:firmware|fw|required(?:\s+firmware)?|minimum(?:\s+firmware)?)\s*[:=-]?\s*v?([0-9]{1,2}\.[0-9]{2})\+?")
        .unwrap()
        .captures(value)
        .and_then(|capture| capture.get(1))
        .map(|value| format!("{}+", value.as_str()))
        .unwrap_or_default()
}

fn normalize_kind(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "game" | "base" => "base".into(),
        "update" | "patch" => "update".into(),
        "dlc" => "dlc".into(),
        "backport" => "backport".into(),
        "exfat" => "exfat".into(),
        _ => "unknown".into(),
    }
}

fn normalize_access(value: &str) -> String {
    match value.to_ascii_lowercase().replace('_', "-").as_str() {
        "direct" => "Direct".into(),
        "hosterlanding" | "hoster-landing" => "HosterLanding".into(),
        _ => "Unknown".into(),
    }
}

/// Search a static catalog (in-memory index; install validated every shard).
fn static_search(
    directory: &std::path::Path,
    descriptor: &Descriptor,
    query: &str,
    limit: usize,
) -> Result<Vec<SourceTitle>, String> {
    let catalog = static_catalog::cached(directory)?;
    let titles = catalog.search(query, "", limit.min(MAX_RESULTS));
    Ok(titles
        .into_iter()
        .map(|title| SourceTitle {
            title_id: title.title_id,
            name: title.name,
            region: title.region,
            icon: title.icon,
            source_id: descriptor.id.clone(),
            source_name: catalog_name(&descriptor.id, &descriptor.name),
            source_version: descriptor.version.clone(),
        })
        .collect())
}

/// Resolve a title against a static catalog, carrying the catalog's archive
/// passwords so extraction can use them without a separate lookup.
fn static_resolve(
    directory: &std::path::Path,
    descriptor: &Descriptor,
    title_id: &str,
    region: &str,
) -> Result<Vec<SourcePackage>, String> {
    let catalog = static_catalog::cached(directory)?;
    let rows = catalog.resolve(title_id, region)?;
    let mut packages: Vec<SourcePackage> = rows
        .into_iter()
        .map(|row| SourcePackage {
            kind: if row.kind != "base" && row.label.to_ascii_lowercase().contains("backport") { "backport".into() } else { infer_kind(&row.kind).to_owned() },
            label: row.label.clone(),
            url: row.url.clone(),
            access_type: {
                let kind = row.access_type.to_ascii_lowercase();
                match kind.as_str() {
                    "direct" => "Direct".into(),
                    "hosterlanding" | "hoster-landing" => "HosterLanding".into(),
                    _ if row.url.contains("1fichier") => "HosterLanding".into(),
                    _ => row.access_type.clone(),
                }
            },
            source_id: descriptor.id.clone(),
            source_name: catalog_name(&descriptor.id, &descriptor.name),
            source_version: descriptor.version.clone(),
            candidate_id: row.id.clone(),
            group_id: if row.group_id.is_empty() {
                row.id.clone()
            } else {
                row.group_id.clone()
            },
            hoster: row.hoster.clone(),
            version: row.version.clone(),
            firmware: row.firmware.clone(),
            source_page_url: row.source_page_url.clone(),
            expected_size: row.expected_size,
            expected_sha256: String::new(),
            expected_content_id: String::new(),
            archive_set_id: row.archive_set_id.clone(),
            archive_part_number: row.archive_part_number,
            archive_part_count: row.archive_part_count,
            archive_file_name: row.file_name.clone(),
            archive_format_hint: row.archive_format_hint.clone(),
            archive_password: row.archive_password.clone(),
            mirror_id: row.mirror_id.clone(),
            intermediate_url: row.intermediate_url.clone(),
            referer: None,
            diagnostics: if row.archive_passwords.len() > 1 {
                let mut notes = row.diagnostics.clone(); notes.push(format!(
                    "archive password fallbacks: {}",
                    row.archive_passwords.len()
                )); notes
            } else {
                row.diagnostics.clone()
            },
        })
        .collect();
    annotate_static_parts(&mut packages);
    Ok(packages)
}

fn annotate_static_parts(packages: &mut [SourcePackage]) {
    let pattern = Regex::new(r"(?i)(?:[. _-]part[. _-]*(\d+))\.rar$").unwrap();
    let mut groups: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
    for (index, package) in packages.iter_mut().enumerate() {
        let name = package.archive_file_name.as_deref().unwrap_or("").replace('+', " ");
        let captures = pattern.captures(&name);
        let Some(number) = package.archive_part_number.or_else(|| captures.as_ref().and_then(|c| c[1].parse::<u32>().ok())).filter(|n| *n > 0 && *n <= 10000) else {
            if package.archive_set_id.is_some() { package.diagnostics.push("incomplete archive set: part numbers are not documented".into()); }
            continue;
        };
        let stem = pattern.replace(&name, "").to_ascii_lowercase();
        let set = package.archive_set_id.clone().unwrap_or_else(|| sha256(format!("{}|{}|{}|{}|{}", package.source_id, package.group_id, package.kind, package.hoster, stem).as_bytes()));
        package.archive_set_id = Some(set.clone());
        if package.mirror_id.is_none() { package.mirror_id = Some(package.hoster.clone()); }
        package.archive_part_number = Some(number);
        package.archive_format_hint = Some("rar".into());
        groups.entry(set).or_default().push((index, number));
    }
    for rows in groups.values() {
        let highest = rows.iter().map(|(_, n)| *n).max().unwrap_or(0);
        let numbers: HashSet<u32> = rows.iter().map(|(_, n)| *n).collect();
        let declared: HashSet<u32> = rows.iter().filter_map(|(index, _)| packages[*index].archive_part_count).collect();
        let count = if declared.len() == 1 { declared.iter().next().copied() } else if declared.is_empty() && highest >= 2 && numbers.len() == rows.len() && numbers.len() == highest as usize { Some(highest) } else { None };
        let incomplete = declared.len() > 1 || count.is_none_or(|count| count < 2 || numbers.len() != rows.len() || numbers.len() != count as usize || highest != count);
        for &(index, _) in rows {
            packages[index].archive_part_count = count;
            if incomplete { packages[index].diagnostics.push("incomplete archive set: this mirror does not list every part".into()); }
        }
    }
}

#[cfg(test)]
mod supplied_catalog_tests {
    use super::*;

    #[test]
    #[ignore = "requires SSPI_CATALOG_FIXTURE extracted from the supplied source"]
    fn validates_and_resolves_the_supplied_ps5_source() {
        let root = PathBuf::from(std::env::var_os("SSPI_CATALOG_FIXTURE").expect("source fixture directory"));
        let descriptor: Descriptor = serde_json::from_slice(&fs::read(root.join("source.json")).unwrap()).unwrap();
        validate_descriptor(&descriptor).unwrap();
        let declared: Vec<_> = descriptor.files.iter().map(|f| (f.path.clone(), f.size, f.sha256.clone())).collect();
        let counts = static_catalog::validate_installed(&root, &declared).unwrap();
        let catalog = static_catalog::cached(&root).unwrap();
        let titles = catalog.search("", "", 60000);
        assert_eq!(titles.len() as u64, counts.ready_titles);
        assert!(titles.iter().all(|t| t.title_id.starts_with("PPSA")));
        let mut rows = 0; let mut firmware = 0; let mut multipart = 0;
        for title in &titles {
            let packages = static_resolve(&root, &descriptor, &title.title_id, "").unwrap();
            assert!(!packages.is_empty(), "{} has no resolved packages", title.title_id);
            assert!(packages.iter().all(|p| p.url.starts_with("http")));
            firmware += packages.iter().filter(|p| !p.firmware.is_empty()).count();
            multipart += packages.iter().filter(|p| p.archive_part_number.is_some()).count();
            rows += packages.len();
        }
        assert!(firmware > 0 && multipart > 0);
        println!("Supplied source: {} searchable PS5 titles, {rows} resolved rows, {firmware} firmware fields, {multipart} multipart rows; every declared shard hash passed", titles.len());
    }

    /// The SSPI community PS4 catalog (`embedded-catalog-refresh-v1`) installs and resolves as a
    /// static catalog; its online refresh section is ignored.
    #[test]
    #[ignore = "requires SSPI_PS4_CATALOG_FIXTURE extracted from the supplied PS4 source"]
    fn validates_and_resolves_the_supplied_ps4_source() {
        let root = PathBuf::from(std::env::var_os("SSPI_PS4_CATALOG_FIXTURE").expect("source fixture directory"));
        let descriptor: Descriptor = serde_json::from_slice(&fs::read(root.join("source.json")).unwrap()).unwrap();
        assert_eq!(descriptor.engine.engine_type, "embedded-catalog-refresh-v1");
        validate_descriptor(&descriptor).unwrap();
        let declared: Vec<_> = descriptor.files.iter().map(|f| (f.path.clone(), f.size, f.sha256.clone())).collect();
        let counts = static_catalog::validate_installed(&root, &declared).unwrap();
        let catalog = static_catalog::cached(&root).unwrap();
        let mut titles = catalog.search("", "", 60000);
        // Resolve by shard so this full-catalog test does not repeatedly evict the small runtime cache.
        let manifest: Value = serde_json::from_slice(&fs::read(root.join("catalog.json")).unwrap()).unwrap();
        let mut shards = std::collections::HashMap::<String, String>::new();
        for file in manifest["indexFiles"].as_array().unwrap() {
            let index: Value = serde_json::from_slice(&fs::read(root.join(file.as_str().unwrap())).unwrap()).unwrap();
            for row in index["titles"].as_array().unwrap() {
                shards.insert(row["titleId"].as_str().unwrap().into(), row["packagesFile"].as_str().unwrap().into());
            }
        }
        titles.sort_by_key(|t| shards.get(&t.title_id).cloned().unwrap_or_default());
        assert_eq!(titles.len() as u64, counts.ready_titles);
        // PS4 titles plus PS2 classics packaged for PS4 (SLUS, SLES, SCUS, ...).
        assert!(titles.iter().all(|t| t.title_id.starts_with("CUSA") || t.title_id.starts_with('S')));
        let (mut rows, mut multipart, mut passwords) = (0, 0, 0);
        for title in &titles {
            let packages = static_resolve(&root, &descriptor, &title.title_id, "").unwrap();
            assert!(!packages.is_empty(), "{} has no resolved packages", title.title_id);
            multipart += packages.iter().filter(|p| p.archive_part_number.is_some()).count();
            passwords += packages.iter().filter(|p| p.archive_password.as_deref().is_some_and(|v| !v.is_empty())).count();
            rows += packages.len();
        }
        println!("Supplied PS4 source: {} searchable titles, {rows} resolved rows, {multipart} multipart rows, {passwords} with archive passwords", titles.len());
    }
}

/// Local indexes populate both platforms without waiting for a remote source.
pub fn home_titles(app: &AppHandle) -> Result<Vec<SourceTitle>, String> {
    let registry = load_registry(app)?;
    let mut results = Vec::new();
    for entry in registry.sources.iter().filter(|s| s.enabled && embedded_catalog(&s.engine_type)) {
        let (descriptor, _) = load_source(app, entry)?;
        let directory = source_dir(app, &entry.id, &entry.version)?;
        let catalog = static_catalog::cached(&directory)?;
        for title in catalog.search("", "", 60000) {
            results.push(SourceTitle { title_id: title.title_id, name: title.name, region: title.region, icon: title.icon,
                source_id: descriptor.id.clone(), source_name: catalog_name(&descriptor.id, &descriptor.name), source_version: descriptor.version.clone() });
        }
    }
    Ok(results)
}

pub async fn search(
    app: &AppHandle,
    query: &str,
    limit: usize,
) -> Result<Vec<SourceTitle>, String> {
    let registry = load_registry(app)?;
    struct SourceJob {
        name: String,
        directory: PathBuf,
        descriptor: Descriptor,
        recipe: Option<Value>,
    }
    let mut failures = Vec::new();
    let mut jobs = Vec::new();
    for entry in registry.sources.iter().filter(|item| item.enabled) {
        let (descriptor, recipe) = match load_source(app, entry) {
            Ok(value) => value,
            Err(error) => {
                failures.push(format!("{}: {error}", entry.name));
                continue;
            }
        };
        if !descriptor
            .capabilities
            .iter()
            .any(|capability| capability == "titles.search")
        {
            continue;
        }
        let directory = source_dir(app, &entry.id, &entry.version).unwrap_or_default();
        jobs.push(SourceJob { name: catalog_name(&entry.id, &entry.name), directory, descriptor, recipe });
    }
    // Sources resolve concurrently: latency is slowest-source, not sum-of-sources.
    let results = futures_util::future::join_all(jobs.into_iter().map(|job| async move {
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            if embedded_catalog(&job.descriptor.engine.engine_type) {
                static_search(&job.directory, &job.descriptor, query, limit)
            } else if matches!(
                job.descriptor.engine.engine_type.as_str(),
                "recipe-v1" | "recipe-v2"
            ) {
                recipe_search(&job.descriptor, job.recipe.as_ref().unwrap(), query, limit).await
            } else {
                remote_search(&job.descriptor, query, limit).await
            }
        })
        .await;
        (job.name, result)
    }))
    .await;
    let mut merged = Vec::new();
    for (name, result) in results {
        match result {
            Ok(Ok(mut titles)) => merged.append(&mut titles),
            Ok(Err(error)) => failures.push(format!("{name}: {error}")),
            Err(_) => failures.push(format!("{name}: timed out")),
        }
    }
    let mut seen = HashSet::new();
    merged.retain(|title| {
        seen.insert(format!(
            "{}:{}",
            title.title_id,
            title.region.to_ascii_uppercase()
        ))
    });
    merged.truncate(limit.min(MAX_RESULTS));
    if merged.is_empty() && !failures.is_empty() {
        return Err(failures.join("; "));
    }
    Ok(merged)
}

pub async fn resolve(
    app: &AppHandle,
    title_id: &str,
    name: &str,
    region: &str,
) -> Result<Vec<SourcePackage>, String> {
    let registry = load_registry(app)?;
    struct ResolveJob {
        name: String,
        directory: PathBuf,
        descriptor: Descriptor,
        recipe: Option<Value>,
    }
    let mut failures = Vec::new();
    let mut jobs = Vec::new();
    for entry in registry.sources.iter().filter(|item| item.enabled) {
        let (descriptor, recipe) = match load_source(app, entry) {
            Ok(value) => value,
            Err(error) => {
                failures.push(format!("{}: {error}", entry.name));
                continue;
            }
        };
        if !descriptor
            .capabilities
            .iter()
            .any(|capability| capability == "packages.resolve")
        {
            continue;
        }
        let directory = source_dir(app, &entry.id, &entry.version).unwrap_or_default();
        jobs.push(ResolveJob { name: catalog_name(&entry.id, &entry.name), directory, descriptor, recipe });
    }
    // Sources resolve concurrently: latency is slowest-source, not sum-of-sources.
    let results = futures_util::future::join_all(jobs.into_iter().map(|job| async move {
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            if embedded_catalog(&job.descriptor.engine.engine_type) {
                static_resolve(&job.directory, &job.descriptor, title_id, region)
            } else if matches!(
                job.descriptor.engine.engine_type.as_str(),
                "recipe-v1" | "recipe-v2"
            ) {
                recipe_resolve(
                    &job.descriptor,
                    job.recipe.as_ref().unwrap(),
                    title_id,
                    name,
                    region,
                )
                .await
            } else {
                remote_resolve(&job.descriptor, title_id, name, region).await
            }
        })
        .await;
        (job.name, result)
    }))
    .await;
    let mut merged = Vec::new();
    for (name, result) in results {
        match result {
            Ok(Ok(mut packages)) => merged.append(&mut packages),
            Ok(Err(error)) => failures.push(format!("{name}: {error}")),
            Err(_) => failures.push(format!("{name}: timed out")),
        }
    }
    let mut seen = HashSet::new();
    merged.retain(|package| {
        let key = if !package.expected_sha256.is_empty() {
            format!("sha:{}", package.expected_sha256)
        } else if !package.expected_content_id.is_empty() {
            format!("cid:{}", package.expected_content_id)
        } else if !package.candidate_id.is_empty() {
            format!("id:{}:{}", package.source_id, package.candidate_id)
        } else {
            format!("url:{}", package.url)
        };
        seen.insert(key)
    });
    merged.truncate(MAX_RESULTS);
    if merged.is_empty() && !failures.is_empty() {
        return Err(failures.join("; "));
    }
    Ok(merged)
}

pub fn has_enabled(app: &AppHandle) -> bool {
    load_registry(app)
        .map(|registry| registry.sources.iter().any(|source| source.enabled))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_origin_validation() {
        assert!(valid_title_id("CUSA17419"));
        assert!(valid_title_id("PPSA17419"));
        assert!(parse_origin("https://orbispatches.com").is_some());
        assert!(parse_origin("https://orbispatches.com/path").is_none());
    }

    #[test]
    fn ranking_and_package_inference() {
        assert!(match_score("Persona 5 Royal", "Persona 5 Royal PS4") > 0.8);
        assert_eq!(infer_kind("Game + Update v1.20"), "base");
        assert_eq!(infer_kind("Update v1.20"), "update");
        assert_eq!(infer_version("Update v1.20 EUR"), "1.20");
    }

    #[test]
    fn recipe_query_is_encoded_exactly_once() {
        let item = WorkItem {
            value: "CUSA17419 PS4".into(),
            ..Default::default()
        };
        let url = expand_item(
            "https://example.test/?s={value}",
            "CUSA17419",
            "Persona 5 Royal",
            "EU",
            &item,
        );
        let parsed = Url::parse(&url).unwrap();
        assert_eq!(
            parsed
                .query_pairs()
                .find(|(key, _)| key == "s")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some("CUSA17419 PS4")
        );
        assert!(!url.contains("%2520"));
    }

    #[test]
    fn v2_hoster_links_keep_parts_and_secure_reconstruction() {
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/41967".into(),
            source_page: "https://dlpsgame.com/game".into(),
            title_id: "PPSA19534".into(),
            kind: "base".into(),
            group_id: "base-1".into(),
            html: r##"<div class="post-content"><a href="https://1fichier.com/a/Part.01.rar">Part.01</a><a href="#" data-domain="https://1fichier." data-path="com/a/Part.12.rar">Part.12</a><a href="https://example.test/ad">ad</a></div>"##.into(),
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"secureLinkAttributes":{"domain":"data-domain","path":"data-path"},"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].archive_part_number, Some(1));
        assert_eq!(links[1].archive_part_number, Some(12));
        assert_eq!(links[1].archive_part_count, None);
        assert_eq!(links[0].archive_set_id, links[1].archive_set_id);
        assert!(links[0]
            .diagnostics
            .iter()
            .any(|value| value.contains("missing Part.")));
        assert_eq!(links[0].url.contains(".rar"), true);
        assert!(links.iter().all(|item| item.kind == "base"));
    }

    #[test]
    fn title_id_patterns_accept_ppsa_and_reject_invalid() {
        assert!(valid_title_id("PPSA19534"));
        assert!(valid_title_id("ppsa27625"));
        assert!(valid_title_id("CUSA17419"));
        assert!(!valid_title_id("PPSA1953"));
        assert!(!valid_title_id("PPSA195340"));
        assert!(!valid_title_id("XPPSA19534"));
        assert!(!valid_title_id("PPSA1953A"));
        assert!(valid_title_id("CUSA33096"));
    }

    #[test]
    fn title_region_comes_from_local_heading() {
        let document = WorkItem {
            name: "Resident Evil Requiem".into(),
            html: r#"<p>PPSA30803 – USA</p><p>PPSA31246 – EUR</p>"#.into(),
            ..Default::default()
        };
        let step = serde_json::json!({
            "titleIdPatterns": [r"\bPPSA\d{5}\b", r"\bCUSA\d{5}\b"],
            "regionMap": {"USA":"US","EUR":"EU"}
        });
        let titles = title_items(&[document], &step);
        let usa = titles.iter().find(|item| item.title_id == "PPSA30803").unwrap();
        let eur = titles.iter().find(|item| item.title_id == "PPSA31246").unwrap();
        assert_eq!(usa.region, "US");
        assert_eq!(eur.region, "EU");
    }

    #[test]
    fn combined_game_line_is_base_and_backport_line_stays_backport() {
        let rules = step_kind_rules(&serde_json::json!({
            "kindRules": [
                {"pattern":"(?i)backport","kind":"backport"},
                {"pattern":"(?i)\\bdlcs?\\b","kind":"dlc"},
                {"pattern":"(?i)\\bgame\\b","kind":"base"}
            ]
        }));
        assert_eq!(
            classify_package_kind("Game (v01.200) + DLC + Backport 4.xx+ : 1File Akia", &rules),
            "base"
        );
        assert_eq!(
            classify_package_kind("Backport 4.xx + DLC Fix (@BestPig) : Download", &rules),
            "backport"
        );
        assert_eq!(classify_package_kind("DLC : Download", &rules), "dlc");
    }

    fn b64(value: &str) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value.as_bytes())
    }

    #[test]
    fn decode_replaces_inner_html_for_package_roots() {
        let payload = b64(
            r#"<p>PPSA19534 / USA / v01.000.016 Size: 80 GB</p><p>Game</p><p><a href="https://downloadgameps3.net/archives/41967">1File</a></p>"#,
        );
        let document = WorkItem {
            url: "https://dlpsgame.com/battlefield-6/".into(),
            html: format!(
                r#"<div class="post-body entry-content"><div class="secure-data" data-payload="{payload}"></div></div>"#
            ),
            ..Default::default()
        };
        let step = serde_json::json!({
            "selectors":[{"selector":".secure-data[data-payload]","attribute":"data-payload","encoding":"base64-utf8"}],
            "replaceInnerHtml": true,
            "maximumDecodedBytes": 262144
        });
        let decoded = decode_fragments(vec![document], &step).unwrap();
        assert!(decoded[0].html.contains("downloadgameps3.net/archives/41967"));
        assert!(decoded[0].html.contains(r#"class="post-body entry-content""#) || decoded[0].html.contains("post-body"));
        let sections = parse_package_sections(
            &decoded,
            "PPSA19534",
            &serde_json::json!({
                "rootSelectors":[".post-body.entry-content",".entry-content"],
                "titleIdPatterns":[r"\bPPSA\d{5}\b"],
                "allowedLinkHosts":["downloadgameps3.net"],
                "kindRules":[
                    {"pattern":"(?i)backport","kind":"backport"},
                    {"pattern":"(?i)\\bgame\\b|\\bbase\\b","kind":"base"}
                ],
                "versionPattern": r"(?i)\bv?(\d{2}\.\d{3}(?:\.\d{3})?)\b",
                "sizePattern": r"(?i)\bsize\s*:\s*([0-9]+(?:\.[0-9]+)?\s*(?:KB|MB|GB|TB))",
                "regionMap":{"USA":"US"},
                "groupMirrorsBySection": true,
                "preserveSourcePage": true
            }),
        );
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].kind, "base");
        assert_eq!(sections[0].package_version, "01.000.016");
        assert_eq!(sections[0].region, "US");
        assert_eq!(sections[0].expected_size, Some(80 * 1024 * 1024 * 1024));
        assert_eq!(sections[0].source_page, "https://dlpsgame.com/battlefield-6/");
    }

    #[test]
    fn package_sections_keep_base_and_backport_separate() {
        let html = r#"
            <div class="post-body entry-content">
              <p>PPSA19534 / USA / v01.000.016 Size: 80 GB</p>
              <p>Game</p>
              <p>
                <a href="https://downloadgameps3.net/archives/1">1File</a>
                <a href="https://downloadgameps3.net/archives/2">Akia</a>
                <a href="https://downloadgameps3.net/archives/3">Viki</a>
                <a href="https://downloadgameps3.net/archives/4">Rootz</a>
                <a href="https://downloadgameps3.net/archives/5">Mediafire</a>
                <a href="https://downloadgameps3.net/archives/6">Data</a>
              </p>
              <p>Backport FW required: 4.xx</p>
              <p><a href="https://downloadgameps3.net/archives/42950">1File BP</a></p>
              <div class="menu"><a href="https://downloadgameps3.net/guides/x">Guide</a></div>
              <a href="https://twitter.com/x">social</a>
            </div>
        "#;
        let document = WorkItem {
            url: "https://dlpsgame.com/game".into(),
            html: html.into(),
            ..Default::default()
        };
        let step = serde_json::json!({
            "rootSelectors":[".post-body.entry-content"],
            "titleIdPatterns":[r"\bPPSA\d{5}\b"],
            "allowedLinkHosts":["downloadgameps3.net"],
            "kindRules":[
                {"pattern":"(?i)backport|fw\\s*(?:required)?\\s*:?\\s*4\\.xx","kind":"backport"},
                {"pattern":"(?i)\\bgame\\b|\\bbase\\b","kind":"base"}
            ],
            "versionPattern": r"(?i)\bv?(\d{2}\.\d{3}(?:\.\d{3})?)\b",
            "groupMirrorsBySection": true,
            "preserveSourcePage": true
        });
        let sections = parse_package_sections(&[document], "PPSA19534", &step);
        let base: Vec<_> = sections.iter().filter(|item| item.kind == "base").collect();
        let backport: Vec<_> = sections.iter().filter(|item| item.kind == "backport").collect();
        assert_eq!(base.len(), 6);
        assert_eq!(backport.len(), 1);
        assert!(base.iter().all(|item| item.group_id == base[0].group_id));
        assert_ne!(base[0].group_id, backport[0].group_id);
        assert!(!sections.iter().any(|item| item.url.contains("guides") || item.url.contains("twitter")));
    }

    #[test]
    fn hoster_secure_only_page_emits_all_allowlisted_hosts() {
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/42950".into(),
            source_page: "https://dlpsgame.com/game".into(),
            title_id: "PPSA19534".into(),
            kind: "backport".into(),
            group_id: "bp-1".into(),
            html: r##"<div class="post-content">
                <a href="#" data-domain="https://akirabox." data-path="com/f/a">Akia</a>
                <a href="#" data-domain="https://vikingfile." data-path="com/f/b">Viki</a>
                <a href="#" data-domain="https://rootz." data-path="so/f/c">Rootz</a>
                <a href="#" data-domain="https://www.mediafire." data-path="com/file/d">Mediafire</a>
                <a href="#" data-domain="https://datanodes." data-path="to/f/e">Data</a>
                <a href="#" data-domain="https://filekeeper." data-path="net/f/f">FileK</a>
                <a href="#" data-domain="https://1fichier." data-path="com/f/g">1File</a>
                <a href="https://ads.example/x">ad</a>
            </div>"##.into(),
            ..Default::default()
        };
        let step = serde_json::json!({
            "rootSelectors":[".post-content"],
            "allowedHosts":["1fichier.com","akirabox.com","datanodes.to","filekeeper.net","mediafire.com","rootz.so","vikingfile.com"],
            "secureLinkAttributes":{"domain":"data-domain","path":"data-path"},
            "maximumLinks":128
        });
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 7);
        assert!(links.iter().all(|item| item.archive_set_id.is_empty()));
        assert!(links.iter().any(|item| item.diagnostics.iter().any(|value| value.contains("ads.example"))));
    }

    #[test]
    fn contiguous_archive_parts_claim_count_and_missing_part_does_not() {
        let mut html = String::from(r#"<div class="post-content">"#);
        for part in 1..=12 {
            html.push_str(&format!(
                r#"<a href="https://1fichier.com/?p{part}">Part.{part:02}</a>"#
            ));
        }
        html.push_str("</div>");
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/41967".into(),
            title_id: "PPSA19534".into(),
            kind: "base".into(),
            group_id: "base-1".into(),
            html,
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 12);
        assert!(links.iter().all(|item| item.archive_part_count == Some(12)));
        assert_eq!(links[0].archive_part_number, Some(1));
        assert_eq!(links[11].archive_part_number, Some(12));

        let gapped = WorkItem {
            url: "https://downloadgameps3.net/archives/41967".into(),
            title_id: "PPSA19534".into(),
            kind: "base".into(),
            group_id: "base-1".into(),
            html: r#"<div class="post-content"><a href="https://1fichier.com/?1">Part.01</a><a href="https://1fichier.com/?7">Part.07</a><a href="https://1fichier.com/?12">Part.12</a></div>"#.into(),
            ..Default::default()
        };
        let gapped_links = parse_hoster_links(&[gapped], &step);
        assert!(gapped_links.iter().all(|item| item.archive_part_count.is_none()));
        assert!(gapped_links[0].diagnostics.iter().any(|value| value.contains("missing Part.")));
    }

    #[test]
    fn secure_split_complementary_part_is_preserved() {
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/44239".into(), kind: "base".into(), group_id: "base".into(),
            html: r##"<div class="post-content"><a href="https://1fichier.com/?first">1File - Part.01</a>
                <a href="#" data-d1="https://1fi" data-d2="chier.com" data-path="/?second">1File - Part.02</a></div>"##.into(),
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 2);
        assert_eq!(links[1].url, "https://1fichier.com/?second");
        assert!(links.iter().all(|item| item.archive_part_count == Some(2)));
    }

    #[test]
    fn nested_fichier_filenames_group_split_rars_and_sizes() {
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/16617".into(),
            title_id: "PPSA16617".into(),
            kind: "base".into(),
            group_id: "base-moria".into(),
            html: r#"<div class="post-content">
                Check all link befor download
                <p>[DLPSGAME.COM]-PPSA16617.part2.rar<br>451.81 MB<br>
                <a href="https://1fichier.com/?hjoizn3fo1bhesjsgfjp">https://1fichier.com/?hjoizn3fo1bhesjsgfjp</a> —</p>
                <p>[DLPSGAME.COM]-PPSA16617.part1.rar<br>9.90 GB<br>
                <a href="https://1fichier.com/?26bwbh6ppl2g9muth8y8">https://1fichier.com/?26bwbh6ppl2g9muth8y8</a> —</p>
            </div>"#.into(),
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].archive_part_number, Some(1));
        assert_eq!(links[1].archive_part_number, Some(2));
        assert_eq!(links[0].archive_set_id, links[1].archive_set_id);
        assert!(!links[0].archive_set_id.is_empty());
        assert_eq!(links[0].archive_part_count, Some(2));
        assert_eq!(
            links[0].archive_file_name,
            "[DLPSGAME.COM]-PPSA16617.part1.rar"
        );
        assert_eq!(
            links[1].archive_file_name,
            "[DLPSGAME.COM]-PPSA16617.part2.rar"
        );
        assert_eq!(links[0].expected_size, parse_size_label("9.90 GB"));
        assert_eq!(links[1].expected_size, parse_size_label("451.81 MB"));
    }

    #[test]
    fn seven_part_rar_set_claims_count_and_size_sum() {
        let mut html = String::from(r#"<div class="post-content">"#);
        for part in 1..=7 {
            html.push_str(&format!(
                r#"<p>[DLPSGAME.COM]-PPSA00007.part{part}.rar<br>1.00 GB<br><a href="https://1fichier.com/?p{part}">https://1fichier.com/?p{part}</a></p>"#
            ));
        }
        html.push_str("</div>");
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/7".into(),
            title_id: "PPSA00007".into(),
            kind: "base".into(),
            group_id: "base-7".into(),
            html,
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 7);
        assert!(links.iter().all(|item| item.archive_set_id == links[0].archive_set_id));
        assert!(links.iter().all(|item| item.archive_part_count == Some(7)));
        assert_eq!(links[0].archive_part_number, Some(1));
        assert_eq!(links[6].archive_part_number, Some(7));
        let sum: u64 = links.iter().filter_map(|item| item.expected_size).sum();
        assert_eq!(sum, parse_size_label("1.00 GB").unwrap() * 7);
    }

    #[test]
    fn seven_part_zip_split_groups() {
        let mut html = String::from(r#"<div class="post-content">"#);
        html.push_str(r#"<p>game.zip<br>10 MB<br><a href="https://1fichier.com/?z0">https://1fichier.com/?z0</a></p>"#);
        for part in 1..=7 {
            html.push_str(&format!(
                r#"<p>game.z{part:02}<br>10 MB<br><a href="https://1fichier.com/?z{part}">https://1fichier.com/?z{part}</a></p>"#
            ));
        }
        html.push_str("</div>");
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/zip7".into(),
            title_id: "PPSA00008".into(),
            kind: "base".into(),
            group_id: "base-zip".into(),
            html,
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert!(links.len() >= 7);
        assert!(links.iter().any(|item| item.archive_part_number == Some(1)));
        assert!(links.iter().any(|item| item.archive_file_name.to_ascii_lowercase().contains(".z0") || item.archive_file_name.to_ascii_lowercase().contains(".zip")));
    }

    #[test]
    fn gapped_seven_part_set_has_no_count() {
        let mut html = String::from(r#"<div class="post-content">"#);
        for part in [1, 2, 4, 5, 6, 7] {
            html.push_str(&format!(
                r#"<p>game.part{part}.rar<br>1 GB<br><a href="https://1fichier.com/?g{part}">https://1fichier.com/?g{part}</a></p>"#
            ));
        }
        html.push_str("</div>");
        let document = WorkItem {
            url: "https://downloadgameps3.net/archives/gap".into(),
            title_id: "PPSA00009".into(),
            kind: "base".into(),
            group_id: "base-gap".into(),
            html,
            ..Default::default()
        };
        let step = serde_json::json!({"rootSelectors":[".post-content"],"allowedHosts":["1fichier.com"],"maximumLinks":128});
        let links = parse_hoster_links(&[document], &step);
        assert_eq!(links.len(), 6);
        assert!(links.iter().all(|item| item.archive_part_count.is_none()));
        assert!(links[0].diagnostics.iter().any(|note| note.contains("missing Part.3")));
    }

    #[test]
    fn emit_package_keeps_hoster_landing_for_rar_paths() {
        let items = vec![WorkItem {
            url: "https://1fichier.com/a/game.part01.rar".into(),
            label: "Part.01".into(),
            kind: "base".into(),
            hoster: "1fichier.com".into(),
            archive_set_id: "archive-1".into(),
            archive_part_number: Some(1),
            archive_part_count: Some(12),
            archive_format_hint: "rar".into(),
            source_page: "https://dlpsgame.com/game".into(),
            intermediate_url: "https://downloadgameps3.net/archives/1".into(),
            parent_url: "https://downloadgameps3.net/archives/1".into(),
            ..Default::default()
        }];
        let packages = emit_packages(
            &Descriptor {
                schema: "gamesearch.source/v1".into(),
                id: "org.gamesearch.dlpsgame-ps5".into(),
                name: "DLPS".into(),
                description: String::new(),
                version: "1.0.0".into(),
                minimum_api_version: "5.0".into(),
                maximum_api_version: "6.0".into(),
                capabilities: vec!["packages.resolve".into()],
                engine: Engine {
                    engine_type: "recipe-v2".into(),
                    entry: "recipe.json".into(),
                    search: None,
                    resolve: None,
                },
                permissions: Permissions {
                    network_origins: vec!["https://dlpsgame.com".into()],
                },
                origins: Vec::new(),
                files: Vec::new(),
            },
            items,
            &serde_json::json!({"accessType":"HosterLanding","preserveArchiveParts":true}),
        );
        assert_eq!(packages[0].access_type, "HosterLanding");
        assert!(packages[0].url.contains(".rar"));
        assert_eq!(packages[0].archive_part_number, Some(1));
    }

    #[test]
    fn package_tables_keep_only_exact_title_and_external_mirrors() {
        let document = WorkItem {
            url: "https://www.superpsx.com/dll-persona-5-royal/".into(),
            html: r#"
                <table><tr><td>Game CUSA17419 v1.00 EU</td>
                    <td><a href="https://1fichier.com/?abc&utm_source=x">Download</a></td>
                    <td><a href="https://www.superpsx.com/internal">Ignore</a></td>
                </tr></table>
                <table><tr><td>Update CUSA00001 v9.99</td>
                    <td><a href="https://gofile.io/d/wrong">Wrong title</a></td>
                </tr></table>
            "#
            .into(),
            ..Default::default()
        };
        let step = serde_json::json!({
            "blockedHosts": ["superpsx.com"],
            "blockedHostSuffixes": [".superpsx.com"],
            "hostLabels": ["1fichier.com=OneFile"],
            "dropQueryKeys": ["utm_source"]
        });
        let packages = parse_package_tables(&[document], "CUSA17419", &step);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].kind, "base");
        assert_eq!(packages[0].package_version, "1.00");
        assert_eq!(packages[0].hoster, "OneFile");
        assert!(!packages[0].url.contains("utm_source"));
    }

    #[test]
    #[ignore = "live diagnostic for the hosted reference source"]
    fn hosted_reference_source_executes_end_to_end() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let bytes = reqwest::get("https://amptis.com/apk/gamesource.gssource")
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
            let descriptor_bytes = {
                let mut entry = archive.by_name("source.json").unwrap();
                let mut content = Vec::new();
                entry.read_to_end(&mut content).unwrap();
                content
            };
            let descriptor: Descriptor = serde_json::from_slice(&descriptor_bytes).unwrap();
            validate_descriptor(&descriptor).unwrap();
            let recipe_bytes = {
                let mut entry = archive.by_name(&descriptor.engine.entry).unwrap();
                let mut content = Vec::new();
                entry.read_to_end(&mut content).unwrap();
                content
            };
            let recipe: Value = serde_json::from_slice(&recipe_bytes).unwrap();

            let titles = recipe_search(&descriptor, &recipe, "Persona 5 Royal", 10)
                .await
                .unwrap();
            assert!(titles.iter().any(|title| title.title_id == "CUSA17419"));

            let packages =
                recipe_resolve(&descriptor, &recipe, "CUSA17419", "Persona 5 Royal", "EU")
                    .await
                    .unwrap();
            assert!(!packages.is_empty());
            assert!(packages
                .iter()
                .all(|package| package.access_type == "HosterLanding"));
        });
    }
}

pub fn migrate_bundled(app: &AppHandle) -> Result<(), String> {
    let marker = source_root(app)?.join("windows-bundle-3.2.0.applied");
    if marker.exists() { return Ok(()); }
    // Sources are installed by the user (Options > Sources): nothing is added automatically.
    let mut registry = load_registry(app)?;
    let retired = ["org.gamesearch.dlpsgame-ps5", "org.gamesearch.dlpsgame", "org.amptis.dlpsgame", "org.gamesearch.superpsx", "org.amptis.superpsx"];
    for source in &mut registry.sources { if retired.contains(&source.id.as_str()) { source.enabled = false; } }
    save_registry(app, &registry)?;
    fs::write(marker, b"3.2.0").map_err(|e|e.to_string())
}
