//! embedded-catalog-v1 reader: the static catalog format used by
//! community-built `.gssource` packages (sharded index + package files).
//!
//! Port of the PS4 application's `PackageSourceEngineStatic`, adapted to the
//! Windows Manager's source layout:
//!
//!   <source dir>/source.json          declarative manifest (schema, engine)
//!   <source dir>/catalog.json         shard lists + counts + normalization id
//!   <source dir>/index/NNN.json       {"titles":[{titleId,name,region,icon,
//!                                     searchText,kinds,releaseCount,
//!                                     packagesFile}]}
//!   <source dir>/packages/XX.json     {"titles":{TITLE_ID:[{url,kind,label,
//!                                     hoster,version,groupId,accessType,
//!                                     sourcePageUrl,archivePassword,...}]}}
//!
//! A global source can also carry `catalog.json.platformCatalog`: titles for
//! several platforms (PS4 and PS5 homebrew) with their package rows inline and
//! icons packed into `artworkFiles`. The legacy index stays PS4-only for the
//! PS4 application; this reader prefers the platform catalog when present.
//!
//! Search reads only the index (~2.3 MB for 7k titles), so queries are
//! in-memory and instant; package shards are read lazily with a small cache.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_META: u64 = 2 * 1024 * 1024;
const MAX_SHARD: u64 = 2 * 1024 * 1024;
const MAX_TITLES: usize = 60_000;
const SHARD_CACHE: usize = 2;
const MAX_ARTWORK: u64 = 4 * 1024 * 1024;
const MAX_ART_SLICE: u64 = 512 * 1024;

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CatalogFile {
    #[serde(default)]
    format: String,
    #[serde(default)]
    index_files: Vec<String>,
    #[serde(default)]
    package_files: Vec<String>,
    #[serde(default)]
    artwork_files: Vec<String>,
    #[serde(default)]
    counts: Counts,
    #[serde(default)]
    platform_catalog: Option<PlatformCatalog>,
}

/// Multi-platform title list with inline package rows (global homebrew).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PlatformCatalog {
    #[serde(default)]
    titles: Vec<IndexTitle>,
    #[serde(default)]
    packages: HashMap<String, Vec<PackageRow>>,
}

/// A JPEG slice inside one of the catalog's artwork pack files.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtRef {
    file: String,
    offset: u64,
    size: u64,
    #[serde(default)]
    sha256: String,
}

/// Platform tags for a homebrew title. `runsOn` lists the consoles the app
/// is known to run on; `unverifiedOn` the ones it may run on untested.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HomebrewTitle {
    pub platform: String,
    #[serde(default)]
    pub runs_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_on: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub developer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
}

/// How a homebrew package is installed: `pkg` (PS4 package), `folder` (a
/// ZIP whose `archiveRoot` goes to /data/homebrew/<installDir>) or `payload`
/// (an ELF, optionally `archiveMember` inside a ZIP, added to Payloads).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HomebrewPackage {
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub runs_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_root: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub install_dir: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub archive_member: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub member_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_size: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub payload_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpacked_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
}

/// Catalog counts from `catalog.json`; returned by install validation so the
/// receipt can show how many titles/releases the source advertises.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    #[serde(default)]
    pub titles: u64,
    #[serde(default)]
    pub ready_titles: u64,
    #[serde(default)]
    pub releases: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexShard {
    #[serde(default)]
    titles: Vec<IndexTitle>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IndexTitle {
    title_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    search_text: String,
    /// Present in the catalog schema; kept for future kind filtering.
    #[serde(default)]
    #[allow(dead_code)]
    kinds: Vec<String>,
    #[serde(default)]
    release_count: u64,
    #[serde(default)]
    packages_file: String,
    #[serde(default)]
    art: Option<ArtRef>,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    runs_on: Vec<String>,
    #[serde(default)]
    unverified_on: Vec<String>,
    #[serde(default)]
    category: String,
    #[serde(default)]
    developer: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    version: String,
}

impl IndexTitle {
    fn homebrew(&self) -> Option<HomebrewTitle> {
        (!self.platform.is_empty()).then(|| HomebrewTitle {
            platform: self.platform.clone(),
            runs_on: if self.runs_on.is_empty() {
                vec![self.platform.clone()]
            } else {
                self.runs_on.clone()
            },
            unverified_on: self.unverified_on.clone(),
            category: self.category.clone(),
            developer: self.developer.clone(),
            description: self.description.clone(),
            version: self.version.clone(),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageShard {
    #[serde(default)]
    titles: HashMap<String, Vec<PackageRow>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageRow {
    #[serde(default)]
    id: String,
    /// Present in the catalog schema; the lookup key is the map key.
    #[serde(default)]
    #[allow(dead_code)]
    title_id: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    hoster: String,
    #[serde(default)]
    version: String,
    #[serde(default, alias = "firmware")]
    required_firmware: String,
    #[serde(default)]
    group_id: String,
    #[serde(default)]
    access_type: String,
    #[serde(default)]
    source_page_url: String,
    #[serde(default)]
    archive_password: String,
    #[serde(default)]
    archive_passwords: Vec<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    volumes: Vec<PackageVolume>,
    #[serde(default)]
    archive_set_id: Option<String>,
    #[serde(default)]
    archive_part_number: Option<u32>,
    #[serde(default)]
    archive_part_count: Option<u32>,
    #[serde(default)]
    archive_file_name: Option<String>,
    #[serde(default)]
    archive_format_hint: Option<String>,
    #[serde(default)]
    mirror_id: Option<String>,
    #[serde(default, alias = "containerUrl")]
    intermediate_url: Option<String>,
    #[serde(default, alias = "size")]
    expected_size: Value,
    #[serde(default)]
    diagnostics: Vec<String>,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    content_id: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    format: String,
    #[serde(default)]
    archive_root: Option<String>,
    #[serde(default)]
    install_dir: String,
    #[serde(default)]
    layout: String,
    #[serde(default)]
    archive_member: String,
    #[serde(default)]
    member_sha256: String,
    #[serde(default)]
    member_size: Option<u64>,
    #[serde(default)]
    payload_name: String,
    #[serde(default)]
    unpacked_size: Option<u64>,
    #[serde(default)]
    file_count: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageVolume {
    url: String,
    #[serde(default, alias = "archiveFileName")]
    name: String,
    #[serde(default)]
    access_type: String,
    #[serde(default)]
    archive_part_number: Option<u32>,
    #[serde(default, alias = "size")]
    expected_size: Value,
}

#[derive(Debug, Clone)]
pub struct CatalogTitle {
    pub title_id: String,
    pub name: String,
    pub region: String,
    pub icon: Option<String>,
    #[allow(dead_code)]
    pub release_count: u64,
    pub homebrew: Option<HomebrewTitle>,
    rank: i32,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogPackage {
    pub id: String,
    pub url: String,
    pub kind: String,
    pub label: String,
    pub hoster: String,
    pub version: String,
    pub firmware: String,
    pub group_id: String,
    pub access_type: String,
    pub source_page_url: String,
    pub archive_password: Option<String>,
    pub archive_passwords: Vec<String>,
    pub file_name: Option<String>,
    pub archive_set_id: Option<String>,
    pub archive_part_number: Option<u32>,
    pub archive_part_count: Option<u32>,
    pub archive_format_hint: Option<String>,
    pub mirror_id: Option<String>,
    pub intermediate_url: Option<String>,
    pub expected_size: Option<u64>,
    pub diagnostics: Vec<String>,
    pub sha256: String,
    pub content_id: String,
    pub homebrew: Option<HomebrewPackage>,
}

pub struct Catalog {
    root: PathBuf,
    titles: Vec<IndexTitle>,
    by_id: HashMap<String, usize>,
    shards: Mutex<Vec<(String, Arc<PackageShard>)>>,
    index_files: Vec<String>,
    package_files: Vec<String>,
    artwork_files: Vec<String>,
    platform: Option<PlatformCatalog>,
    pub counts: Counts,
    #[allow(dead_code)]
    pub format: String,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let metadata = std::fs::metadata(path).map_err(|_| format!("catalog file missing: {name}"))?;
    if metadata.len() > MAX_META {
        return Err(format!("catalog file too large: {name}"));
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| format!("catalog json invalid: {name}: {error}"))
}

/// Load and validate a catalog directory. Verifies the manifest identity and
/// shard declarations; the caller decides whether full hash validation was
/// already done at install time.
pub fn load_source(root: &Path) -> Result<Catalog> {
    let manifest = read_json::<Value>(&root.join("source.json"))?;
    let manifest_id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
    if manifest_id.is_empty() {
        return Err("source.json has no id".into());
    }
    let entry = manifest
        .get("engine")
        .and_then(|engine| engine.get("entry"))
        .and_then(Value::as_str)
        .unwrap_or("catalog.json");
    let catalog = read_json::<CatalogFile>(&root.join(entry))?;
    if !catalog.format.is_empty() && catalog.format != "embedded-catalog-v1" {
        return Err(format!(
            "catalog format is not embedded-catalog-v1: {}",
            catalog.format
        ));
    }
    let platform = catalog
        .platform_catalog
        .filter(|platform| !platform.titles.is_empty());
    if platform.is_none()
        && (catalog.index_files.is_empty() || catalog.package_files.is_empty())
    {
        return Err("catalog declares no index or package files".into());
    }
    if catalog.artwork_files.len() > 16
        || !catalog.artwork_files.iter().all(|file| safe_relative(file))
    {
        return Err("catalog declares invalid artwork files".into());
    }
    let counts = match &platform {
        Some(platform) => Counts {
            titles: platform.titles.len() as u64,
            ready_titles: platform.titles.len() as u64,
            releases: platform.packages.values().map(|rows| rows.len() as u64).sum(),
        },
        None => catalog.counts,
    };
    Ok(Catalog {
        root: root.to_path_buf(),
        titles: Vec::new(),
        by_id: HashMap::new(),
        shards: Mutex::new(Vec::new()),
        index_files: catalog.index_files,
        package_files: catalog.package_files,
        artwork_files: catalog.artwork_files,
        platform,
        counts,
        format: catalog.format,
    })
}

impl Catalog {
    pub fn load(&mut self) -> Result<()> {
        let mut titles = Vec::new();
        if let Some(platform) = &self.platform {
            titles = platform.titles.clone();
            if titles.len() > MAX_TITLES {
                return Err("catalog exceeds the title limit".into());
            }
            self.attach_art(&mut titles);
        }
        for file in self.index_files.iter().filter(|_| self.platform.is_none()) {
            let bytes = self.read_contained(file)?;
            let shard: IndexShard = serde_json::from_slice(&bytes)
                .map_err(|error| format!("catalog json invalid: {file}: {error}"))?;
            titles.extend(shard.titles);
            if titles.len() > MAX_TITLES {
                return Err("catalog exceeds the title limit".into());
            }
        }
        self.by_id = titles
            .iter()
            .enumerate()
            .map(|(index, title)| (title.title_id.to_ascii_uppercase(), index))
            .collect();
        self.titles = titles;
        Ok(())
    }

    /// Turn each title's artwork slice into a JPEG data URL. Art is
    /// decoration: a missing file or a slice that fails its hash is skipped.
    fn attach_art(&self, titles: &mut [IndexTitle]) {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let mut packs: HashMap<String, Option<Vec<u8>>> = HashMap::new();
        for title in titles.iter_mut() {
            let Some(art) = &title.art else { continue };
            if !self.artwork_files.contains(&art.file) || art.size == 0 || art.size > MAX_ART_SLICE {
                continue;
            }
            let pack = packs
                .entry(art.file.clone())
                .or_insert_with(|| self.read_limited(&art.file, MAX_ARTWORK).ok());
            let Some(pack) = pack else { continue };
            let (start, end) = (art.offset as usize, (art.offset + art.size) as usize);
            let Some(slice) = pack.get(start..end) else { continue };
            let digest: String = Sha256::digest(slice)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if !art.sha256.is_empty() && !digest.eq_ignore_ascii_case(&art.sha256) {
                continue;
            }
            title.icon = format!(
                "data:image/jpeg;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(slice)
            );
        }
    }

    fn read_contained(&self, file: &str) -> Result<Vec<u8>> {
        self.read_limited(file, MAX_SHARD)
    }

    fn read_limited(&self, file: &str, limit: u64) -> Result<Vec<u8>> {
        if !safe_relative(file) {
            return Err(format!("catalog references an invalid path: {file}"));
        }
        let path = self.root.join(file);
        let metadata =
            std::fs::metadata(&path).map_err(|_| format!("catalog file missing: {file}"))?;
        if metadata.len() > limit {
            return Err(format!("catalog file too large: {file}"));
        }
        std::fs::read(path).map_err(|error| error.to_string())
    }

    /// Search the in-memory index. Ranking mirrors the PS4 engine:
    /// 0 exact title id, 1 exact name, 2 name prefix, 3 all words present.
    pub fn search(&self, query: &str, region: &str, limit: usize) -> Vec<CatalogTitle> {
        let normalized = normalize_search(query);
        let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
        let region = normalize_region(region);
        let mut matches: Vec<CatalogTitle> = Vec::new();
        for title in self.titles.iter() {
            if !region.is_empty()
                && !title.region.is_empty()
                && !title.region.eq_ignore_ascii_case(&region)
            {
                continue;
            }
            let id = normalize_search(&title.title_id);
            let name = normalize_search(&title.name);
            let text = if title.search_text.is_empty() {
                format!("{name} {id}")
            } else {
                normalize_search(&title.search_text)
            };
            let rank = if normalized.is_empty() {
                4
            } else if id == normalized {
                0
            } else if name == normalized {
                1
            } else if name.starts_with(&normalized) {
                2
            } else if words.iter().all(|word| text.contains(word)) {
                3
            } else {
                -1
            };
            if rank < 0 {
                continue;
            }
            matches.push(CatalogTitle {
                title_id: title.title_id.to_ascii_uppercase(),
                name: title.name.clone(),
                region: title.region.clone(),
                icon: (!title.icon.is_empty()).then(|| title.icon.clone()),
                release_count: title.release_count,
                homebrew: title.homebrew(),
                rank,
            });
        }
        matches.sort_by(|a, b| {
            a.rank
                .cmp(&b.rank)
                .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
                .then_with(|| a.title_id.cmp(&b.title_id))
                .then_with(|| a.region.cmp(&b.region))
        });
        matches.truncate(limit);
        matches
    }

    /// Load the package shard for a title and return its rows.
    pub fn resolve(&self, title_id: &str, region: &str) -> Result<Vec<CatalogPackage>> {
        let normalized = title_id.trim().to_ascii_uppercase();
        let index = match self.by_id.get(&normalized) {
            Some(index) => *index,
            None => return Ok(Vec::new()),
        };
        let title = &self.titles[index];
        let homebrew = title.homebrew();
        let region = normalize_region(region);
        let shard;
        let rows = if let Some(platform) = &self.platform {
            match platform.packages.get(&normalized) {
                Some(rows) => rows,
                None => return Ok(Vec::new()),
            }
        } else {
            if title.packages_file.is_empty() {
                return Err("index entry has no packagesFile".into());
            }
            shard = self.shard(&title.packages_file.clone())?;
            match shard.titles.get(&normalized) {
                Some(rows) => rows,
                None => return Ok(Vec::new()),
            }
        };
        Ok(rows
            .iter()
            .filter(|row| {
                region.is_empty()
                    || row.region.is_empty()
                    || row.region.eq_ignore_ascii_case(&region)
            })
            .flat_map(|row| {
                let package = CatalogPackage {
                id: row.id.clone(),
                url: row.url.clone(),
                kind: row.kind.clone(),
                label: row.label.clone(),
                hoster: row.hoster.clone(),
                version: row.version.clone(),
                firmware: row.required_firmware.clone(),
                group_id: row.group_id.clone(),
                access_type: row.access_type.clone(),
                source_page_url: row.source_page_url.clone(),
                archive_password: (!row.archive_password.is_empty())
                    .then(|| row.archive_password.clone()),
                archive_passwords: row.archive_passwords.clone(),
                file_name: row.archive_file_name.clone().or_else(|| (!row.name.is_empty()).then(|| row.name.clone())),
                archive_set_id: row.archive_set_id.clone(),
                archive_part_number: row.archive_part_number,
                archive_part_count: row.archive_part_count,
                archive_format_hint: row.archive_format_hint.clone(),
                mirror_id: row.mirror_id.clone(),
                intermediate_url: row.intermediate_url.clone(),
                expected_size: json_size(&row.expected_size),
                diagnostics: row.diagnostics.clone(),
                sha256: row.sha256.clone(),
                content_id: row.content_id.clone(),
                homebrew: homebrew_package(row, homebrew.as_ref()),
                };
                expand_volumes(package, &row.volumes)
            })
            .collect())
    }

    fn shard(&self, file: &str) -> Result<Arc<PackageShard>> {
        {
            let cache = self
                .shards
                .lock()
                .map_err(|_| "catalog shard cache is poisoned".to_string())?;
            if let Some((_, shard)) = cache.iter().find(|(path, _)| path == file) {
                return Ok(shard.clone());
            }
        }
        let bytes = self.read_contained(file)?;
        let shard: PackageShard = serde_json::from_slice(&bytes)
            .map_err(|error| format!("catalog json invalid: {file}: {error}"))?;
        let shard = Arc::new(shard);
        let mut cache = self
            .shards
            .lock()
            .map_err(|_| "catalog shard cache is poisoned".to_string())?;
        cache.retain(|(path, _)| path != file);
        cache.insert(0, (file.to_string(), shard.clone()));
        cache.truncate(SHARD_CACHE);
        Ok(shard)
    }
}

fn homebrew_package(row: &PackageRow, title: Option<&HomebrewTitle>) -> Option<HomebrewPackage> {
    let title = title?;
    Some(HomebrewPackage {
        platform: if row.platform.is_empty() {
            title.platform.clone()
        } else {
            row.platform.clone()
        },
        format: if row.format.is_empty() {
            "pkg".into()
        } else {
            row.format.to_ascii_lowercase()
        },
        runs_on: title.runs_on.clone(),
        archive_root: row.archive_root.clone(),
        install_dir: row.install_dir.clone(),
        layout: row.layout.clone(),
        archive_member: row.archive_member.clone(),
        member_sha256: row.member_sha256.clone(),
        member_size: row.member_size,
        payload_name: row.payload_name.clone(),
        unpacked_size: row.unpacked_size,
        file_count: row.file_count,
    })
}

fn json_size(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn expand_volumes(package: CatalogPackage, volumes: &[PackageVolume]) -> Vec<CatalogPackage> {
    if volumes.is_empty() { return vec![package]; }
    volumes.iter().enumerate().map(|(index, volume)| {
        let mut part = package.clone();
        if index > 0 { part.id = format!("{}-volume-{}", package.id, index + 1); }
        part.url = volume.url.clone();
        part.hoster = url::Url::parse(&volume.url).ok().and_then(|url| url.host_str().map(|host| host.trim_start_matches("www.").to_owned())).unwrap_or_else(|| package.hoster.clone());
        if !volume.access_type.is_empty() { part.access_type = volume.access_type.clone(); }
        part.file_name = (!volume.name.is_empty()).then(|| volume.name.clone());
        part.archive_set_id = Some(package.archive_set_id.clone().unwrap_or_else(|| format!("catalog-volumes-{}", package.id)));
        part.mirror_id = Some(package.mirror_id.clone().unwrap_or_else(|| part.hoster.clone()));
        // Part numbers come from explicit metadata or filenames downstream, never array order.
        part.archive_part_number = volume.archive_part_number;
        part.expected_size = json_size(&volume.expected_size);
        part
    }).collect()
}

fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\0')
        && !value.contains("..")
        && !value.starts_with('/')
        && !value.contains('\\')
}

pub fn normalize_region(region: &str) -> String {
    region.trim().to_ascii_uppercase()
}

/// Approximation of the catalog's `nfkd-lower-alnum-spaces-v1` normalization:
/// lowercase, fold common Latin diacritics, keep alphanumerics, single spaces.
pub fn normalize_search(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut space = false;
    for original in value.chars() {
        for folded in fold(original) {
            if folded.is_alphanumeric() {
                if space && !out.is_empty() {
                    out.push(' ');
                }
                out.extend(folded.to_lowercase());
                space = false;
            } else {
                space = true;
            }
        }
    }
    out
}

fn fold(c: char) -> Vec<char> {
    match c {
        'À'..='Å' | 'à'..='å' | 'Ā' | 'ā' | 'Ă' | 'ă' | 'Ą' | 'ą' => vec!['a'],
        'Æ' | 'æ' => vec!['a', 'e'],
        'Ç' | 'ç' | 'Ć' | 'ć' | 'Ĉ' | 'ĉ' | 'Ċ' | 'ċ' | 'Č' | 'č' => vec!['c'],
        'Ð' | 'ð' | 'Ď' | 'ď' | 'Đ' | 'đ' => vec!['d'],
        'È'..='Ë' | 'è'..='ë' | 'Ē' | 'ē' | 'Ĕ' | 'ĕ' | 'Ė' | 'ė' | 'Ę' | 'ę' | 'Ě' | 'ě' => {
            vec!['e']
        }
        'Ĝ' | 'ĝ' | 'Ğ' | 'ğ' | 'Ġ' | 'ġ' | 'Ģ' | 'ģ' => vec!['g'],
        'Ĥ' | 'ĥ' | 'Ħ' | 'ħ' => vec!['h'],
        'Ì'..='Ï' | 'ì'..='ï' | 'Ĩ' | 'ĩ' | 'Ī' | 'ī' | 'Ĭ' | 'ĭ' | 'Į' | 'į' | 'İ' | 'ı' => {
            vec!['i']
        }
        'Ĵ' | 'ĵ' => vec!['j'],
        'Ķ' | 'ķ' => vec!['k'],
        'Ĺ' | 'ĺ' | 'Ļ' | 'ļ' | 'Ľ' | 'ľ' | 'Ł' | 'ł' => vec!['l'],
        'Ñ' | 'ñ' | 'Ń' | 'ń' | 'Ņ' | 'ņ' | 'Ň' | 'ň' => vec!['n'],
        'Ò'..='Ö' | 'ò'..='ö' | 'Ø' | 'ø' | 'Ō' | 'ō' | 'Ŏ' | 'ŏ' | 'Ő' | 'ő' => vec!['o'],
        'Œ' | 'œ' => vec!['o', 'e'],
        'Ŕ' | 'ŕ' | 'Ŗ' | 'ŗ' | 'Ř' | 'ř' => vec!['r'],
        'Ś' | 'ś' | 'Ŝ' | 'ŝ' | 'Ş' | 'ş' | 'Š' | 'š' | 'ß' => vec!['s'],
        'Ţ' | 'ţ' | 'Ť' | 'ť' | 'Ŧ' | 'ŧ' => vec!['t'],
        'Ù'..='Ü' | 'ù'..='ü' | 'Ũ' | 'ũ' | 'Ū' | 'ū' | 'Ŭ' | 'ŭ' | 'Ů' | 'ů' | 'Ű' | 'ű'
        | 'Ų' | 'ų' => vec!['u'],
        'Ŵ' | 'ŵ' => vec!['w'],
        'Ý' | 'ý' | 'ÿ' | 'Ŷ' | 'ŷ' => vec!['y'],
        'Ź' | 'ź' | 'Ż' | 'ż' | 'Ž' | 'ž' => vec!['z'],
        'Þ' | 'þ' => vec!['t', 'h'],
        other => vec![other],
    }
}

/// Shared catalog cache keyed by install directory.
static CATALOGS: OnceLock<Mutex<HashMap<PathBuf, Arc<Catalog>>>> = OnceLock::new();

pub fn cached(root: &Path) -> Result<Arc<Catalog>> {
    let key = root.to_path_buf();
    let cache = CATALOGS.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let guard = cache
            .lock()
            .map_err(|_| "catalog cache is poisoned".to_string())?;
        if let Some(catalog) = guard.get(&key) {
            return Ok(catalog.clone());
        }
    }
    let mut catalog = load_source(root)?;
    catalog.load()?;
    let catalog = Arc::new(catalog);
    let mut guard = cache
        .lock()
        .map_err(|_| "catalog cache is poisoned".to_string())?;
    guard.insert(key, catalog.clone());
    Ok(catalog)
}

pub fn invalidate(root: &Path) {
    if let Some(cache) = CATALOGS.get() {
        if let Ok(mut guard) = cache.lock() {
            guard.remove(root);
        }
    }
}

/// Full validation used at install time: every declared file must exist,
/// match its size and sha256, and the shard lists must line up with the
/// declarations. Returns the catalog counts for the install receipt.
pub fn validate_installed(root: &Path, declared: &[(String, u64, String)]) -> Result<Counts> {
    use sha2::{Digest, Sha256};
    let catalog = load_source(root)?;
    let mut declared_map: HashMap<&str, (u64, &str)> = HashMap::new();
    for (path, size, hash) in declared {
        declared_map.insert(path.as_str(), (*size, hash.as_str()));
    }
    let all = catalog
        .index_files
        .iter()
        .chain(catalog.package_files.iter())
        .map(|file| (file, MAX_SHARD))
        .chain(catalog.artwork_files.iter().map(|file| (file, MAX_ARTWORK)));
    for (file, limit) in all {
        let (size, hash) = declared_map
            .get(file.as_str())
            .ok_or_else(|| format!("catalog references an undeclared file: {file}"))?;
        let bytes = catalog.read_limited(file, limit)?;
        if bytes.len() as u64 != *size {
            return Err(format!("file size mismatch: {file}"));
        }
        let digest = Sha256::digest(&bytes);
        let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if !actual.eq_ignore_ascii_case(hash) {
            return Err(format!("file hash mismatch: {file}"));
        }
    }
    if !declared_map.contains_key("catalog.json") {
        return Err("catalog.json is not declared in source.json".into());
    }
    Ok(catalog.counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_catalog_loads_art_tags_and_inline_packages() {
        use sha2::{Digest, Sha256};
        let root = std::env::temp_dir().join(format!("sspi-platform-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("artwork")).unwrap();
        let art = b"\xff\xd8 not really a jpeg \xff\xd9";
        let mut pack = b"pad".to_vec();
        pack.extend_from_slice(art);
        std::fs::write(root.join("artwork/000.bin"), &pack).unwrap();
        let art_sha: String = Sha256::digest(art).iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(root.join("source.json"), r#"{"id":"org.sspi.homebrew"}"#).unwrap();
        let catalog = serde_json::json!({
            "format": "embedded-catalog-v1",
            "artworkFiles": ["artwork/000.bin"],
            "platformCatalog": {
                "titles": [
                    {"titleId": "PPSA99008", "name": "Eden", "kinds": ["base"], "releaseCount": 1, "searchText": "eden",
                     "platform": "ps5", "category": "Emulators",
                     "art": {"file": "artwork/000.bin", "offset": 3, "size": art.len(), "sha256": art_sha}},
                    {"titleId": "SSHB00001", "name": "Bad art", "kinds": ["base"], "releaseCount": 1, "searchText": "bad art",
                     "platform": "ps4", "runsOn": ["ps4", "ps5"],
                     "art": {"file": "artwork/000.bin", "offset": 0, "size": 4, "sha256": "00"}}
                ],
                "packages": {
                    "PPSA99008": [{"titleId": "PPSA99008", "kind": "base", "platform": "ps5", "format": "folder", "name": "Eden.zip",
                        "url": "https://api.github.com/repos/o/r/releases/assets/1", "accessType": "Direct", "hoster": "GitHub",
                        "version": "1.0", "size": 10, "sha256": "ab", "archiveRoot": "PPSA99008", "installDir": "PPSA99008", "layout": "title"}],
                    "SSHB00001": [{"titleId": "SSHB00001", "kind": "base", "name": "x.pkg", "url": "https://example.test/x.pkg",
                        "accessType": "Direct", "contentId": "UP0000-SSHB00001_00-HOMEBREW00000000"}]
                }
            }
        });
        std::fs::write(root.join("catalog.json"), catalog.to_string()).unwrap();
        let mut loaded = load_source(&root).unwrap();
        assert_eq!((loaded.counts.titles, loaded.counts.releases), (2, 2));
        loaded.load().unwrap();
        let eden = loaded.search("eden", "", 5);
        assert_eq!(eden[0].icon.as_deref().map(|icon| icon.starts_with("data:image/jpeg;base64,")), Some(true));
        let tags = eden[0].homebrew.clone().unwrap();
        assert_eq!((tags.platform.as_str(), tags.runs_on.clone(), tags.category.as_str()), ("ps5", vec!["ps5".to_string()], "Emulators"));
        let bad = loaded.search("bad art", "", 5);
        assert!(bad[0].icon.as_deref().unwrap_or("").is_empty(), "a slice that fails its hash is skipped");
        assert_eq!(bad[0].homebrew.as_ref().unwrap().runs_on, ["ps4", "ps5"]);
        let rows = loaded.resolve("PPSA99008", "").unwrap();
        let folder = rows[0].homebrew.clone().unwrap();
        assert_eq!((folder.format.as_str(), folder.layout.as_str(), folder.install_dir.as_str()), ("folder", "title", "PPSA99008"));
        assert_eq!(rows[0].sha256, "ab");
        let pkg = loaded.resolve("SSHB00001", "").unwrap();
        let pkg_tags = pkg[0].homebrew.clone().unwrap();
        assert_eq!((pkg_tags.format.as_str(), pkg_tags.platform.as_str()), ("pkg", "ps4"));
        assert_eq!(pkg[0].content_id, "UP0000-SSHB00001_00-HOMEBREW00000000");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn embedded_volumes_keep_all_files_and_parent_release_metadata() {
        let base = CatalogPackage { id: "base-1".into(), kind: "base".into(), group_id: "release-1".into(),
            label: "Game + DLC + Backport".into(), archive_password: Some("password".into()),
            archive_part_count: Some(2), ..Default::default() };
        let volumes: Vec<PackageVolume> = serde_json::from_value(serde_json::json!([
            {"url":"https://example.com/one","name":"game.part01.rar","archivePartNumber":1,"size":100},
            {"url":"https://example.com/two","name":"game.part02.rar","archivePartNumber":2,"size":50}
        ])).unwrap();
        let rows = expand_volumes(base, &volumes);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].id, rows[1].id);
        assert_eq!(rows[0].archive_set_id, rows[1].archive_set_id);
        assert_eq!(rows[1].archive_part_number, Some(2));
        assert_eq!(rows[1].expected_size, Some(50));
        assert!(rows.iter().all(|row| row.kind == "base" && row.group_id == "release-1" && row.archive_password.as_deref() == Some("password")));
    }

    #[test]
    fn volume_order_does_not_invent_part_numbers() {
        let volumes: Vec<PackageVolume> = serde_json::from_value(serde_json::json!([
            {"url":"https://example.com/one"}, {"url":"https://example.com/two"}
        ])).unwrap();
        let rows = expand_volumes(CatalogPackage::default(), &volumes);
        assert!(rows.iter().all(|row| row.archive_part_number.is_none() && row.archive_part_count.is_none()));
    }

    #[test]
    fn normalization_matches_catalog_style() {
        assert_eq!(normalize_search("#KILLALLZOMBIES"), "killallzombies");
        assert_eq!(normalize_search("Persona  5  Royal"), "persona 5 royal");
        assert_eq!(normalize_search("Pokémon Épée"), "pokemon epee");
        assert_eq!(normalize_search("CUSA00849"), "cusa00849");
    }

    #[test]
    fn ranking_prefers_exact_id_and_prefix() {
        let mut catalog = Catalog {
            root: PathBuf::from("."),
            titles: vec![
                IndexTitle {
                    title_id: "CUSA00001".into(),
                    name: "Alpha".into(),
                    region: "US".into(),
                    icon: String::new(),
                    search_text: "alpha cusa00001".into(),
                    kinds: vec![],
                    release_count: 1,
                    packages_file: "packages/00.json".into(),
                    ..IndexTitle::default()
                },
                IndexTitle {
                    title_id: "CUSA00002".into(),
                    name: "Alpha Protocol".into(),
                    region: "EU".into(),
                    icon: String::new(),
                    search_text: "alpha protocol cusa00002".into(),
                    kinds: vec![],
                    release_count: 2,
                    packages_file: "packages/00.json".into(),
                    ..IndexTitle::default()
                },
            ],
            by_id: HashMap::new(),
            shards: Mutex::new(vec![]),
            index_files: vec![],
            package_files: vec![],
            artwork_files: vec![],
            platform: None,
            counts: Counts::default(),
            format: String::new(),
        };
        catalog.by_id = catalog
            .titles
            .iter()
            .enumerate()
            .map(|(index, title)| (title.title_id.clone(), index))
            .collect();
        let exact = catalog.search("CUSA00001", "", 10);
        assert_eq!(exact[0].rank, 0);
        let prefix = catalog.search("alpha p", "", 10);
        // Both match (the word "p" is a substring of "alpha"), but the name
        // prefix hit must rank first — same semantics as the PS4 engine.
        assert_eq!(prefix.len(), 2);
        assert_eq!(prefix[0].title_id, "CUSA00002");
        assert_eq!(prefix[0].rank, 2);
        let region = catalog.search("alpha", "EU", 10);
        assert_eq!(region.len(), 1);
        assert_eq!(region[0].region, "EU");
        let none = catalog.search("zzzz", "", 10);
        assert!(none.is_empty());
    }

    #[test]
    fn relative_paths_are_content_checked() {
        assert!(safe_relative("packages/00.json"));
        assert!(!safe_relative("../escape.json"));
        assert!(!safe_relative("/absolute.json"));
        assert!(!safe_relative("dir\\win.json"));
    }

    /// Checks a built homebrew source, run manually:
    ///   set SSPI_HOMEBREW_FIXTURE=<extracted .gssource dir>
    ///   cargo test --lib static_catalog::tests::homebrew_fixture -- --ignored --nocapture
    #[test]
    #[ignore]
    fn homebrew_fixture_loads_every_title_and_row() {
        let Some(root) = std::env::var_os("SSPI_HOMEBREW_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        let manifest: Value = read_json(&root.join("source.json")).unwrap();
        let declared: Vec<(String, u64, String)> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| (row["path"].as_str().unwrap().into(), row["size"].as_u64().unwrap(), row["sha256"].as_str().unwrap().into()))
            .collect();
        let counts = validate_installed(&root, &declared).unwrap();
        let mut catalog = load_source(&root).unwrap();
        catalog.load().unwrap();
        let mut art = 0;
        let mut rows = 0;
        for title in catalog.titles.clone() {
            let hit = catalog.search(&title.title_id, "", 3);
            let hit = hit.iter().find(|hit| hit.title_id == title.title_id).unwrap();
            assert!(hit.homebrew.is_some(), "{} has no platform tags", title.title_id);
            art += usize::from(hit.icon.as_deref().is_some_and(|icon| icon.starts_with("data:image/jpeg")));
            for row in catalog.resolve(&title.title_id, "").unwrap() {
                let tags = row.homebrew.unwrap();
                assert!(matches!(tags.format.as_str(), "pkg" | "folder" | "payload"), "{}", row.url);
                assert!(!row.sha256.is_empty(), "{} has no checksum", row.url);
                rows += 1;
            }
        }
        println!("homebrew fixture: titles={} releases={} rows={rows} art={art}", counts.titles, counts.releases);
        assert_eq!(rows as u64, counts.releases);
    }

    /// End-to-end check against a real community catalog, run manually:
    ///   set SSPI_CATALOG_FIXTURE=<extracted .gssource dir>
    ///   cargo test --lib static_catalog::tests::fixture -- --ignored --nocapture
    #[test]
    #[ignore]
    fn fixture_catalog_validates_and_searches() {
        let Some(root) = std::env::var_os("SSPI_CATALOG_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        let manifest: Value = read_json(&root.join("source.json")).unwrap();
        let declared: Vec<(String, u64, String)> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["path"].as_str().unwrap().to_string(),
                    row["size"].as_u64().unwrap(),
                    row["sha256"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let counts = validate_installed(&root, &declared).unwrap();
        println!(
            "fixture counts: titles={} ready={} releases={}",
            counts.titles, counts.ready_titles, counts.releases
        );
        assert!(counts.titles > 0 && counts.releases > 0);
        let mut catalog = load_source(&root).unwrap();
        catalog.load().unwrap();
        assert_eq!(catalog.titles.len() as u64, counts.ready_titles);

        let hits = catalog.search("killallzombies", "", 10);
        assert!(hits.iter().any(|h| h.title_id == "CUSA00849"), "expected CUSA00849 in {hits:?}");

        let packages = catalog.resolve("CUSA00856", "US").unwrap();
        assert!(!packages.is_empty());
        assert!(packages
            .iter()
            .any(|p| p.archive_password.as_deref() == Some("[DLPSGAME.COM]")));
        println!("fixture resolve: {} rows for CUSA00856", packages.len());
    }
}
