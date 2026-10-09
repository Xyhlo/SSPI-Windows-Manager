//! Segmented downloads. A file is fetched as fixed-size pieces over several HTTP connections at
//! once, each piece written in place by one writer thread. The scheduler decides how many
//! connections the download may use and can change that while it runs; pausing closes the
//! connections and keeps what was written. Until it is complete the file is `<part>.seg`, and its
//! completed pieces are recorded beside it in `<part>.map`, so Retry and the next session continue
//! where they stopped. (Earlier versions resume `<part>` by its length, so they never see a file
//! with gaps.) A server that ignores Range requests gets a single stream.
//!
//! Debrid CDNs limit connections and requests per file without always saying so: beyond the limit
//! a request can go unanswered, stop midway or slow to a trickle. So each connection asks once for
//! a long run of pieces, a download starts with a few connections and adds more while all of them
//! receive, and a connection the server holds back while the others receive is closed: its pieces
//! go to the others and the download stays at the connections that work.
use super::*;
use scheduler::{Share, Ticket};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
use tokio::task::JoinSet;

const PIECE: u64 = 8 * 1024 * 1024;
/// A connection hands its bytes to the writer at this size, or after `HAND_OVER` at the latest.
const WRITE_BATCH: usize = 1024 * 1024;
const HAND_OVER: Duration = Duration::from_secs(1);
/// The map is saved this often; each save flushes the file first.
const MAP_EVERY: Duration = Duration::from_secs(10);
/// Nothing arrived on any connection, and nothing was written, for this long: stop (Retry continues).
const STALL: Duration = if cfg!(test) { Duration::from_secs(3) } else { Duration::from_secs(60) };
/// A request unanswered, or a response silent, this long while nothing else arrives either: it failed.
const SILENT: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(30) };
/// A request unanswered, or a response silent, this long while other connections receive: the
/// server is holding it back. It is closed and its pieces go to the others.
const HELD: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(10) };
/// Another connection receives when its last bytes are this recent.
const RECEIVING: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(3) };
/// Rates are compared over this window: a connection `SLOW_FACTOR` times slower than the best
/// one, while that moves at least `SLOW_FLOOR` bytes a second, is replaced.
const SLOW_WINDOW: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(8) };
const SLOW_FACTOR: f64 = 8.;
const SLOW_FLOOR: f64 = 256. * 1024.;
/// Failed requests in a row, with nothing arriving on other connections, before the download gives up.
const PIECE_ATTEMPTS: u32 = 5;
/// Failed requests in a row while other connections receive: the server is turning this one away.
const LIMIT_STRIKES: u32 = 3;
/// Connections a download starts with; their number doubles each `RAMP_EVERY` while all receive.
const RAMP_FIRST: usize = 4;
const RAMP_EVERY: Duration = if cfg!(test) { Duration::from_millis(500) } else { Duration::from_secs(2) };
/// The most pieces one request asks for.
const RUN_MOST: usize = 128;
/// How long a server may refuse every connection before the download gives up.
const BUSY_GIVE_UP: Duration = Duration::from_secs(120);
/// The longest a refusal (or its Retry-After) holds off new connections.
const BUSY_WAIT_MAX: Duration = Duration::from_secs(60);
/// After the server refused connections, one more is tried this often; after it held them back
/// without a word, at `LIMIT_PROBE`.
const CAP_PROBE: Duration = Duration::from_secs(5);
const LIMIT_PROBE: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(30) };
/// A connection cap this high no longer limits anything.
const CAP_LIFTED: usize = 32;

const LINK_REFUSED: &str = "The server refused the download link";

/// The download a fetch belongs to: its job and its place in the set.
pub(super) struct Context<'a> {
    pub http: &'a Client,
    pub app: &'a AppHandle,
    pub job: &'a str,
    pub cancel: &'a watch::Receiver<bool>,
    pub message: &'a str,
    pub title: &'a str,
    pub icon: &'a Option<String>,
    /// Bytes of the set's earlier files, and the whole set's size.
    pub set_done: u64,
    pub set_total: u64,
}

/// What a download needs from the app; tests supply their own.
trait Host: Send + Sync {
    fn paused(&self) -> bool;
    /// `done` bytes of this file are on disk, arriving at `speed` over `connections`.
    fn progress(&self, done: u64, speed: f64, connections: usize);
    fn guard(&self, part: &Path, remaining: u64) -> Result<(), String>;
}

struct AppHost<'a> { ctx: &'a Context<'a>, ticket: &'a Ticket, set_total: u64 }
impl Host for AppHost<'_> {
    fn paused(&self) -> bool {
        self.ctx.app.state::<AppState>().jobs.lock().unwrap().get(self.ctx.job).is_some_and(|p| p.paused)
    }
    fn progress(&self, done: u64, speed: f64, connections: usize) {
        let ctx = self.ctx;
        let all = ctx.set_done + done;
        self.ticket.set_remaining(self.set_total.saturating_sub(all));
        emit(ctx.app, Progress {
            job_id: ctx.job.to_owned(), stage: "downloading".into(),
            progress: fraction(all, self.set_total), bytes_done: all, bytes_total: self.set_total, speed_bps: speed,
            eta_seconds: (self.set_total > all && speed > 1.).then(|| ((self.set_total - all) as f64 / speed) as u64),
            message: ctx.message.into(), title: ctx.title.into(), icon: ctx.icon.clone(),
            connections: (connections > 0).then_some(connections as u32), ..Default::default()
        });
    }
    fn guard(&self, part: &Path, remaining: u64) -> Result<(), String> { storage::guard_bytes(part, remaining, "download") }
}

/// The completed-piece map kept beside a partial download.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
struct Map { version: u32, total: u64, piece: u64, prefix: usize, done: Vec<usize> }
impl Map {
    fn bytes(&self) -> u64 { (0..self.prefix).chain(self.done.iter().copied()).map(|index| piece_len(self.total, index)).sum() }
}

fn suffixed(part: &Path, suffix: &str) -> PathBuf {
    let mut name = part.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
/// Where a download's bytes go until it is complete.
fn data_path(part: &Path) -> PathBuf { suffixed(part, ".seg") }
fn map_path(part: &Path) -> PathBuf { suffixed(part, ".map") }
fn map_draft(part: &Path) -> PathBuf { suffixed(part, ".map.tmp") }
/// Every file a download of `part` can leave behind.
pub(super) fn partial_files(part: &Path) -> [PathBuf; 4] { [part.to_path_buf(), data_path(part), map_path(part), map_draft(part)] }
/// Whether a download of `part` left anything to continue from.
pub(super) fn retained(part: &Path) -> bool { part.is_file() || data_path(part).is_file() }
/// A segmented download's working file, which is never a usable archive on its own.
pub(super) fn working_file(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("seg") || extension.eq_ignore_ascii_case("map"))
}

/// Whether a download stopped because the server refused its link (expired or revoked).
pub(super) fn link_expired(error: &str) -> bool { error.starts_with(LINK_REFUSED) }
/// Whether a download stopped because the server stopped sending or stayed busy. A fresh attempt,
/// with a newly unlocked link where there is one, continues from the completed pieces; drive,
/// space and cancel errors are not this.
pub(super) fn stalled(error: &str) -> bool { error.starts_with(STALLED) || error.starts_with(STAYED_BUSY) }
const STALLED: &str = "Download stalled for";
const STAYED_BUSY: &str = "The server stayed busy";
/// Whether to unlock an expired link again, with `kept` bytes on disk now and `before` when the
/// last renewal began: the first time always, then only after the last link got pieces through.
pub(super) fn renew_link(renewals: u32, before: u64, kept: u64) -> bool { renewals == 0 || (renewals < LINK_RENEWALS && kept > before) }
const LINK_RENEWALS: u32 = 20;
fn link_refused(status: reqwest::StatusCode) -> String {
    format!("{LINK_REFUSED} (HTTP {}); it may have expired. Retry continues from the completed pieces.", status.as_u16())
}

fn content_range_total(value: &str) -> Option<u64> {
    value.strip_prefix("bytes ")?.split('/').nth(1)?.trim().parse().ok()
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_owned)
}

/// The server's Retry-After, in seconds (the date form is rare and falls back to backing off).
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    header(response, reqwest::header::RETRY_AFTER)?.trim().parse().ok().map(Duration::from_secs)
}

fn busy_status(status: u16) -> bool { matches!(status, 429 | 503 | 509) }

fn fraction(done: u64, total: u64) -> f64 { if total > 0 { (done as f64 / total as f64).clamp(0., 0.99) } else { 0. } }

async fn send(request: reqwest::RequestBuilder, cancel: &watch::Receiver<bool>, wait: Duration) -> Result<reqwest::Response, String> {
    let mut cancelled = cancel.clone();
    tokio::select! {
        result = tokio::time::timeout(wait, request.send()) => result
            .map_err(|_| network_error("Package download request timed out"))?
            .map_err(|_| network_error("Package download request")),
        _ = cancelled.changed() => Err("cancelled".into()),
    }
}

async fn wait_or_cancel(wait: Duration, cancel: &watch::Receiver<bool>) -> Result<(), String> {
    let mut cancelled = cancel.clone();
    tokio::select! {
        _ = tokio::time::sleep(wait) => Ok(()),
        _ = cancelled.changed() => Err("cancelled".into()),
    }
}

/// Downloads `url` into `part`, resuming retained pieces. Returns the first bytes of the file, the
/// content type and the server's file name.
pub(super) async fn fetch(ctx: &Context<'_>, url: &str, part: &Path, size_hint: u64) -> Result<(Vec<u8>, String, Option<String>), String> {
    let resumed = resumed_bytes(part);
    emit(ctx.app, Progress {
        job_id: ctx.job.into(), stage: "downloading".into(),
        progress: fraction(ctx.set_done + resumed, ctx.set_total), bytes_done: ctx.set_done + resumed, bytes_total: ctx.set_total,
        message: if resumed > 0 { format!("{} — connecting (resumed {} MB)", ctx.message, resumed / 1_048_576) } else { format!("{} — connecting", ctx.message) },
        title: ctx.title.into(), icon: ctx.icon.clone(), ..Default::default()
    });
    let probe = probe(ctx.http, ctx.cancel, url, size_hint).await?;
    // A FAT32 drive would fail at 4 GiB, after most of the download.
    archives::fat_file_limit(part, probe.total, "This download")?;
    let set_total = ctx.set_total.max(ctx.set_done + probe.total);
    let settings = ctx.app.state::<AppState>().settings.lock().unwrap().clone();
    let plan = storage::download_plan(&settings, set_total, ctx.set_done + resumed,
        !probe.filename.as_deref().unwrap_or(url).to_ascii_lowercase().ends_with(".pkg"));
    storage::publish(ctx.app, ctx.job, plan)?;
    let (content_type, filename) = (probe.content_type.clone(), probe.filename.clone());
    // The file holds a share of the connections only while its bytes move.
    let ticket = scheduler::register_download(ctx.job, set_total.saturating_sub(ctx.set_done + resumed));
    download(&AppHost { ctx, ticket: &ticket, set_total }, ctx.http, ctx.cancel, ticket.share(), url, part, probe).await?;
    drop(ticket);
    let mut magic = [0; 8];
    let mut reader = fs::File::open(part).await.map_err(redact)?;
    let read = reader.read(&mut magic).await.map_err(redact)?;
    Ok((magic[..read].to_vec(), content_type, filename))
}

/// What one byte of the file says: whether the server serves ranges, the size and the name.
struct Probe { response: reqwest::Response, ranged: bool, total: u64, content_type: String, filename: Option<String> }

async fn probe(http: &Client, cancel: &watch::Receiver<bool>, url: &str, size_hint: u64) -> Result<Probe, String> {
    // A busy server or a dropped connection gets two more tries before the download stops.
    let mut attempt = 0u32;
    let response = loop {
        attempt += 1;
        let wait = match send(http.get(url).header(reqwest::header::RANGE, "bytes=0-0"), cancel, Duration::from_secs(45)).await {
            Ok(response) if response.status().is_success() => break response,
            Ok(response) if matches!(response.status().as_u16(), 401 | 403 | 404 | 410) => return Err(link_refused(response.status())),
            Ok(response) if attempt < 3 && (busy_status(response.status().as_u16()) || response.status().is_server_error()) => retry_after(&response),
            Ok(response) => return Err(format!("Download failed: HTTP {}", response.status())),
            Err(error) if error != "cancelled" && attempt < 3 => None,
            Err(error) => return Err(error),
        };
        wait_or_cancel(wait.unwrap_or(Duration::from_secs(2 * u64::from(attempt))).min(Duration::from_secs(30)), cancel).await?;
    };
    let status = response.status();
    let content_type = header(&response, reqwest::header::CONTENT_TYPE).unwrap_or_default();
    let filename = header(&response, reqwest::header::CONTENT_DISPOSITION).as_deref().and_then(disposition_filename);
    let ranged = (status == reqwest::StatusCode::PARTIAL_CONTENT).then(|| header(&response, reqwest::header::CONTENT_RANGE).as_deref().and_then(content_range_total)).flatten().filter(|total| *total > 0);
    let total = ranged.unwrap_or_else(|| if status == reqwest::StatusCode::OK { response.content_length().unwrap_or(size_hint) } else { size_hint });
    Ok(Probe { ranged: ranged.is_some(), total, response, content_type, filename })
}

async fn download(host: &dyn Host, http: &Client, cancel: &watch::Receiver<bool>, share: Arc<Share>, url: &str, part: &Path, probe: Probe) -> Result<(), String> {
    if probe.ranged {
        drop(probe.response);
        return segmented(host, http, cancel, share, url, part, probe.total).await;
    }
    // The server sent the whole file, or a range without a usable size: stream it from the start.
    let response = if probe.response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        drop(probe.response);
        send(http.get(url), cancel, Duration::from_secs(45)).await?
    } else { probe.response };
    if !response.status().is_success() { return Err(format!("Download failed: HTTP {}", response.status())); }
    let total = response.content_length().unwrap_or(probe.total);
    single_stream(host, cancel, response, part, total).await
}

/// Bytes already on disk for `part`: complete pieces from its map, or an earlier version's partial file.
pub(super) fn resumed_bytes(part: &Path) -> u64 {
    if let Some(map) = read_map(part) { return map.bytes(); }
    if data_path(part).exists() { return 0; }
    std::fs::metadata(part).map(|m| m.len()).unwrap_or(0)
}

fn pieces_for(total: u64) -> usize { total.div_ceil(PIECE) as usize }
fn piece_len(total: u64, index: usize) -> u64 { PIECE.min(total - index as u64 * PIECE) }

/// The map of `part`'s partial file, keeping only pieces that lie within the file.
fn read_map(part: &Path) -> Option<Map> {
    let map: Map = serde_json::from_slice(&std::fs::read(map_path(part)).ok()?).ok()?;
    let length = std::fs::metadata(data_path(part)).ok().filter(|meta| meta.is_file())?.len();
    if map.version != 1 || map.piece != PIECE || map.total == 0 { return None; }
    let count = pieces_for(map.total);
    // A drive that lost the end of the file lost those pieces with it.
    let within = |index: usize| index < count && index as u64 * PIECE + piece_len(map.total, index) <= length;
    let prefix = (0..map.prefix.min(count)).take_while(|index| within(*index)).count();
    let mut done: Vec<usize> = map.done.iter().copied().filter(|index| *index >= prefix && within(*index)).collect();
    done.sort_unstable();
    done.dedup();
    Some(Map { prefix, done, ..map })
}

/// An earlier version's partial file (`part` itself, contiguous from the start) becomes the data
/// file and keeps its whole pieces. So does a finished `part` whose record wasn't updated yet.
fn adopt(part: &Path, total: u64) -> Option<Map> {
    let data = data_path(part);
    if data.exists() { return None; }
    let length = std::fs::metadata(part).ok().filter(|meta| meta.is_file())?.len();
    if length == 0 || length > total { return None; }
    std::fs::rename(part, &data).ok()?;
    let prefix = if length == total { pieces_for(total) } else { (length / PIECE) as usize };
    Some(Map { version: 1, total, piece: PIECE, prefix, done: vec![] })
}

async fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(redact(error)),
        _ => Ok(()),
    }
}

/// The complete file takes its final name.
async fn finish(data: &Path, part: &Path) -> Result<(), String> {
    let mut attempt = 0u64;
    loop {
        match fs::rename(data, part).await {
            Ok(()) => return Ok(()),
            // A scanner can hold a new file for a moment.
            Err(_) if attempt < 5 => { attempt += 1; tokio::time::sleep(Duration::from_millis(200 * attempt)).await; }
            Err(error) => return Err(redact(error)),
        }
    }
}

/* ---------------------------------------------------------------- one stream */

async fn single_stream(host: &dyn Host, cancel: &watch::Receiver<bool>, mut response: reqwest::Response, part: &Path, total: u64) -> Result<(), String> {
    // This server can't resume, so the file starts over. The map goes first so it can never
    // describe the new bytes.
    remove_if_present(&map_path(part)).await?;
    remove_if_present(part).await?;
    let data = data_path(part);
    let mut file = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&data).await.map_err(redact)?;
    let meter = Meter::new(0);
    let mut done = 0u64;
    let mut checked = 0u64;
    let mut reported = Instant::now() - Duration::from_secs(1);
    loop {
        // This server can't resume, so a pause holds the connection rather than closing it.
        while host.paused() {
            if *cancel.borrow() { return Err("cancelled".into()); }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        let mut cancelled = cancel.clone();
        let next = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), response.chunk()) => result,
            _ = cancelled.changed() => return Err("cancelled".into()),
        };
        let chunk = match next {
            Err(_) => return Err("Download stalled for 30 seconds. Cancel and retry.".into()),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Err(_)) => return Err(network_error("Package download stream")),
        };
        if done.saturating_sub(checked) >= 64 * 1024 * 1024 || done == 0 {
            host.guard(part, total.saturating_sub(done).max(chunk.len() as u64))?;
            checked = done;
        }
        file.write_all(&chunk).await.map_err(redact)?;
        done += chunk.len() as u64;
        if reported.elapsed() >= Duration::from_millis(250) {
            reported = Instant::now();
            host.progress(done, meter.sample(done), 1);
        }
    }
    if total > 0 && done < total { return Err(format!("Download incomplete ({done} of {total} bytes). Retry downloads it again.")); }
    file.flush().await.map_err(redact)?;
    file.sync_all().await.map_err(redact)?;
    drop(file);
    finish(&data, part).await?;
    host.progress(done, 0., 0);
    Ok(())
}

/// Speed over the last few seconds.
struct Meter { samples: Mutex<std::collections::VecDeque<(Instant, u64)>> }
impl Meter {
    fn new(done: u64) -> Self { Self { samples: Mutex::new([(Instant::now(), done)].into()) } }
    fn sample(&self, done: u64) -> f64 {
        let mut samples = self.samples.lock().unwrap();
        let now = Instant::now();
        samples.push_back((now, done));
        while samples.len() > 2 && now.duration_since(samples[0].0) > Duration::from_secs(4) { samples.pop_front(); }
        let (at, first) = samples[0];
        let elapsed = now.duration_since(at).as_secs_f64();
        if elapsed < 0.5 { return 0.; }
        done.saturating_sub(first) as f64 / elapsed
    }
    fn reset(&self, done: u64) { *self.samples.lock().unwrap() = [(Instant::now(), done)].into(); }
}

/* ---------------------------------------------------------------- pieces */

/// Per piece: bytes on disk (`filled`, what the map may claim), bytes handed to the writer
/// (`fetched`, where the next connection continues, so queued bytes are never fetched twice) and
/// the connection fetching it (`owner`, 0 for none). A connection owns a run of consecutive
/// pieces from the one it is on, and gives each up once all of it is fetched.
struct Pieces { total: u64, filled: Vec<u64>, fetched: Vec<u64>, owner: Vec<u32>, written: u64, next: usize }
impl Pieces {
    fn new(total: u64, map: Option<&Map>) -> Self {
        let count = pieces_for(total);
        let mut pieces = Self { total, filled: vec![0; count], fetched: vec![0; count], owner: vec![0; count], written: 0, next: 0 };
        if let Some(map) = map {
            for index in (0..map.prefix.min(count)).chain(map.done.iter().copied().filter(|index| *index < count)) {
                if pieces.filled[index] == 0 { pieces.filled[index] = piece_len(total, index); pieces.written += pieces.filled[index]; }
            }
        }
        pieces.fetched.clone_from(&pieces.filled);
        pieces.advance();
        pieces
    }
    fn len(&self, index: usize) -> u64 { piece_len(self.total, index) }
    fn complete(&self, index: usize) -> bool { self.filled[index] >= self.len(index) }
    fn advance(&mut self) { while self.next < self.filled.len() && self.complete(self.next) { self.next += 1; } }
    fn open(&self, index: usize) -> bool { self.owner[index] == 0 && self.fetched[index] < self.len(index) }
    fn open_count(&self) -> usize { (self.next..self.filled.len()).filter(|index| self.open(*index)).count() }
    /// Claims pieces for connection `lane`: the first open one and the open ones after it, an even
    /// share of the open pieces among `share` connections (one piece when `share` is 0). With
    /// none open, the later half of the pieces another connection hasn't reached yet. Returns the
    /// first piece and the bytes to ask for, from where that piece's fetched bytes end.
    fn claim(&mut self, lane: u32, share: usize) -> Option<(usize, u64, u64)> {
        let count = self.filled.len();
        let (first, last) = match (self.next..count).find(|index| self.open(*index)) {
            Some(first) => {
                let most = if share == 0 { 1 } else { self.open_count().div_ceil(share).clamp(1, RUN_MOST) };
                let mut last = first;
                while last + 1 < count && last + 1 - first < most && self.open(last + 1) { last += 1; }
                (first, last)
            }
            None => self.unreached()?,
        };
        self.owner[first..=last].fill(lane);
        Some((first, first as u64 * PIECE + self.fetched[first], last as u64 * PIECE + self.len(last)))
    }
    /// The later half of the longest run of pieces its connection hasn't reached yet (the first
    /// piece of a run is the one its connection is on).
    fn unreached(&self) -> Option<(usize, usize)> {
        let count = self.owner.len();
        let mut best: Option<(usize, usize)> = None;
        let mut index = self.next;
        while index < count {
            let (lane, start) = (self.owner[index], index);
            while lane != 0 && index + 1 < count && self.owner[index + 1] == lane { index += 1; }
            let take = if lane == 0 { 0 } else { (index - start) / 2 };
            if take > 0 && best.is_none_or(|(from, to)| to + 1 - from < take) { best = Some((index + 1 - take, index)); }
            index += 1;
        }
        best
    }
    /// `lane` has fetched all of `index`. Whether the next piece is still its own.
    fn pass(&mut self, lane: u32, index: usize) -> bool {
        if self.owner[index] == lane { self.owner[index] = 0; }
        self.owner.get(index + 1) == Some(&lane)
    }
    fn release(&mut self, lane: u32) { for owner in self.owner.iter_mut().filter(|owner| **owner == lane) { *owner = 0; } }
    fn claimable(&self) -> bool { (self.next..self.filled.len()).any(|index| self.open(index)) || self.unreached().is_some() }
    /// Bytes up to `end` (absolute) of `index` are queued for the writer.
    fn sent(&mut self, index: usize, end: u64) {
        let upto = end.saturating_sub(index as u64 * PIECE).min(self.len(index));
        if upto > self.fetched[index] { self.fetched[index] = upto; }
    }
    /// Records bytes written up to `end` (absolute) in `index`. A piece is written contiguously
    /// from its start, so overlapping writes of the same bytes never count twice.
    fn wrote(&mut self, index: usize, end: u64) {
        let upto = end.saturating_sub(index as u64 * PIECE).min(self.len(index));
        if upto > self.filled[index] { self.written += upto - self.filled[index]; self.filled[index] = upto; }
        if upto > self.fetched[index] { self.fetched[index] = upto; }
        self.advance();
    }
    fn done(&self) -> bool { self.written >= self.total }
    fn map(&self) -> Map {
        let prefix = self.next;
        Map { version: 1, total: self.total, piece: PIECE, prefix, done: (prefix..self.filled.len()).filter(|index| self.complete(*index)).collect() }
    }
}

struct Chunk { piece: usize, offset: u64, data: Vec<u8> }

/// How the server has been refusing connections. Cleared when bytes arrive again.
#[derive(Default)]
struct Busy {
    /// Since when nothing has got through, rounds of that, and how many of them refused the link.
    since: Option<Instant>,
    rounds: u32,
    refusals: u32,
    /// No new connection opens before this.
    until: Option<Instant>,
    /// The last refusal, or the last time the connection cap was raised.
    probed: Option<Instant>,
    /// The cap comes from connections held back without a word, so it is tested less often.
    quiet: bool,
}

/// One connection as the supervisor watches it. Times are milliseconds on `Shared::clock`, 0 for
/// never.
struct Lane {
    id: u32,
    /// Set by the supervisor when the server holds this connection back: it closes.
    held: watch::Sender<bool>,
    /// When the current request was sent (0 between requests), when its response began, and
    /// when its last bytes arrived.
    asked: AtomicU64,
    answered: AtomicU64,
    heard: AtomicU64,
    /// Waiting for the writer rather than the server, and when that last ended.
    writing: AtomicBool,
    waited: AtomicU64,
    /// Bytes received over the connection's life.
    bytes: AtomicU64,
}
impl Lane {
    fn new(id: u32) -> Self {
        Self { id, held: watch::channel(false).0, asked: AtomicU64::new(0), answered: AtomicU64::new(0), heard: AtomicU64::new(0),
            writing: AtomicBool::new(false), waited: AtomicU64::new(0), bytes: AtomicU64::new(0) }
    }
    fn hold(&self) { self.held.send_replace(true); }
    fn is_held(&self) -> bool { *self.held.borrow() }
    /// Whether bytes arrived within `RECEIVING` of `clock`.
    fn receiving(&self, clock: u64) -> bool {
        let heard = self.heard.load(Ordering::Relaxed);
        heard > 0 && clock.saturating_sub(heard) <= RECEIVING.as_millis() as u64
    }
    /// How long the current request has been waiting on the server for anything, as of `clock`.
    fn silent(&self, clock: u64) -> Option<u64> {
        let asked = self.asked.load(Ordering::Relaxed);
        if asked == 0 || self.writing.load(Ordering::Relaxed) { return None; }
        let since = [&self.answered, &self.heard, &self.waited].iter().map(|at| at.load(Ordering::Relaxed)).fold(asked, u64::max);
        Some(clock.saturating_sub(since))
    }
}

/// Shared by the supervisor, its connection tasks and the writer thread.
struct Shared {
    url: String,
    http: Client,
    cancel: watch::Receiver<bool>,
    share: Arc<Share>,
    total: u64,
    /// Pieces far ahead cost nothing in a sparse file. Without one the drive fills every gap
    /// first, so each request asks for one piece and the writes stay near the start.
    sparse: bool,
    pieces: Mutex<Pieces>,
    /// Pause, cancel or failure: every connection stops at once.
    stop: watch::Sender<bool>,
    /// Connections asked to leave because the connection target went down.
    shed: AtomicUsize,
    active: AtomicUsize,
    /// The connections running now, and the next one's number.
    lanes: Mutex<Vec<Arc<Lane>>>,
    lane_ids: AtomicU32,
    /// Connections the download may use now (its share, within what the server accepts); runs
    /// are sized for this many.
    wanted: AtomicUsize,
    began: Instant,
    /// Bytes received this session, written yet or not.
    arrived: AtomicU64,
    /// Connections the server accepts (`usize::MAX` until it refuses one).
    cap: AtomicUsize,
    busy: Mutex<Busy>,
    fatal: Mutex<Option<String>>,
}

impl Shared {
    /// Milliseconds since the download began, from 1.
    fn clock(&self) -> u64 { self.began.elapsed().as_millis() as u64 + 1 }
    /// Connections other than `lane` that are receiving.
    fn receiving_besides(&self, lane: u32) -> usize {
        let clock = self.clock();
        self.lanes.lock().unwrap().iter().filter(|other| other.id != lane && other.receiving(clock)).count()
    }
    fn stopped(&self) -> bool { *self.stop.borrow() }
    fn halt(&self) { self.stop.send_replace(true); }
    fn fail(&self, error: String) {
        {
            let mut fatal = self.fatal.lock().unwrap();
            if fatal.is_none() { *fatal = Some(error); }
        }
        self.halt();
    }
    fn shed_one(&self) -> bool { self.shed.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1)).is_ok() }
    /// The server refused a connection, as busy or (`link` holds the error) by refusing the link.
    /// With `others_open` connections still working it limits connections: stay at what it
    /// accepted and try one more now and then. With none open nothing gets through: wait (its
    /// Retry-After, or longer each round) and retry with one connection. A busy server gets two
    /// minutes; a refused link one more try, since a limit can still be counting connections
    /// that just closed.
    fn refused(&self, retry_after: Option<Duration>, others_open: usize, link: Option<String>) {
        let now = Instant::now();
        let cap = {
            let mut busy = self.busy.lock().unwrap();
            let waiting = busy.until.is_some_and(|until| until > now);
            // Several connections refused at once count as one round.
            if others_open == 0 && !waiting {
                let since = *busy.since.get_or_insert(now);
                busy.rounds += 1;
                if link.is_some() { busy.refusals += 1; }
                if busy.refusals >= 2 || since.elapsed() > BUSY_GIVE_UP {
                    drop(busy);
                    self.fail(link.unwrap_or_else(|| "The server stayed busy (HTTP 503 or 429). Retry continues from the completed pieces.".into()));
                    return;
                }
            }
            if !waiting {
                let backoff = if others_open > 0 || link.is_some() { Duration::from_secs(1) } else { Duration::from_secs(1 << busy.rounds.min(5)) };
                busy.until = Some(now + retry_after.unwrap_or(backoff).min(BUSY_WAIT_MAX));
            }
            busy.probed = Some(now);
            busy.quiet = false;
            self.lower_cap(others_open, "refused a connection")
        };
        self.share.limit_connections(cap);
    }
    /// The server holds connections back without refusing them (unanswered, silent, slowed down or
    /// cut off while `receiving` others get bytes): stay at those, and try one more now and then.
    fn limited(&self, receiving: usize) {
        let cap = {
            let mut busy = self.busy.lock().unwrap();
            busy.probed = Some(Instant::now());
            busy.quiet = true;
            self.lower_cap(receiving, "held connections back")
        };
        self.share.limit_connections(cap);
    }
    /// Caps the connections at `most` (at least one); a lower cap goes to the session log.
    fn lower_cap(&self, most: usize, what: &str) -> usize {
        let before = self.cap.fetch_min(most.max(1), Ordering::Relaxed);
        let cap = before.min(most.max(1));
        if cap < before { session_log::write("download", &format!("The server {what}; this download continues with {cap} connection(s)")); }
        cap
    }
    /// Some time after the last refusal, allows one more connection to see if the server takes it.
    /// One the server takes lets the next follow at `CAP_PROBE`, so a passing slowdown is soon over.
    fn probe_cap(&self) {
        let cap = {
            let mut busy = self.busy.lock().unwrap();
            let cap = self.cap.load(Ordering::Relaxed);
            let every = if busy.quiet { LIMIT_PROBE } else { CAP_PROBE };
            if cap == usize::MAX || busy.probed.is_some_and(|at| at.elapsed() < every) { return; }
            busy.probed = Some(Instant::now());
            busy.quiet = false;
            let raised = if cap + 1 >= CAP_LIFTED { usize::MAX } else { cap + 1 };
            self.cap.store(raised, Ordering::Relaxed);
            raised
        };
        self.share.limit_connections(cap);
    }
    /// Whether new connections must wait for the server, as of `now`.
    fn holding_off(&self, now: Instant) -> bool { self.busy.lock().unwrap().until.is_some_and(|until| until > now) }
}

/// How a request ended. `Held`: the supervisor closed it because the server held it back.
enum PieceEnd { Done, Stopped, Held, Busy(Option<Duration>), Refused(String), Retry(String) }

/// Hands batched bytes, which end at `offset`, to the writer. False when the writer is gone.
async fn hand_over(shared: &Shared, lane: &Lane, writes: &tokio::sync::mpsc::Sender<Chunk>, index: usize, batch: &mut Vec<u8>, offset: u64) -> bool {
    if batch.is_empty() { return true; }
    let data = std::mem::replace(batch, Vec::with_capacity(WRITE_BATCH));
    // A full queue is the drive's pace, not the server's.
    lane.writing.store(true, Ordering::Relaxed);
    let queued = writes.send(Chunk { piece: index, offset: offset - data.len() as u64, data }).await.is_ok();
    lane.writing.store(false, Ordering::Relaxed);
    lane.waited.store(shared.clock(), Ordering::Relaxed);
    if queued { shared.pieces.lock().unwrap().sent(index, offset); }
    queued
}

/// The end (exclusive) of the bytes a Content-Range header says are coming.
fn content_range_end(value: &str) -> Option<u64> {
    value.strip_prefix("bytes ")?.split('/').next()?.split_once('-')?.1.trim().parse::<u64>().ok()?.checked_add(1)
}

/// Fetches bytes `start..end` with one request: piece `first` from `start`, then the pieces after
/// it, as long as they are still this connection's.
#[allow(clippy::too_many_arguments)]
async fn fetch_run(shared: &Shared, lane: &Lane, held: &mut watch::Receiver<bool>, stop: &mut watch::Receiver<bool>,
    writes: &tokio::sync::mpsc::Sender<Chunk>, first: usize, start: u64, end: u64) -> PieceEnd {
    let request = shared.http.get(&shared.url).header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1));
    lane.answered.store(0, Ordering::Relaxed);
    lane.asked.store(shared.clock(), Ordering::Relaxed);
    let sent = tokio::select! {
        result = send(request, &shared.cancel, SILENT) => result,
        _ = stop.wait_for(|stop| *stop) => return PieceEnd::Stopped,
        _ = held.wait_for(|held| *held) => return PieceEnd::Held,
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(error) if error == "cancelled" => return PieceEnd::Stopped,
        Err(error) => return PieceEnd::Retry(error),
    };
    lane.answered.store(shared.clock(), Ordering::Relaxed);
    match response.status().as_u16() {
        206 => {}
        status if busy_status(status) => return PieceEnd::Busy(retry_after(&response)),
        200 => { shared.fail("The server stopped answering ranged requests. Retry continues from the completed pieces.".into()); return PieceEnd::Stopped; }
        401 | 403 | 404 | 410 => return PieceEnd::Refused(link_refused(response.status())),
        _ => return PieceEnd::Retry(format!("Download failed: HTTP {}", response.status())),
    }
    let range = header(&response, reqwest::header::CONTENT_RANGE).unwrap_or_default();
    if let Err(error) = validate_download_range(start, &range) {
        shared.fail(error);
        return PieceEnd::Stopped;
    }
    // A server may send less than it was asked for; the rest is asked for again.
    let end = content_range_end(&range).filter(|served| *served > start).map_or(end, |served| served.min(end));
    let piece_end = |index: usize| (index as u64 * PIECE + piece_len(shared.total, index)).min(end);
    let (mut index, mut offset) = (first, start);
    let mut until = piece_end(index);
    let mut batch: Vec<u8> = Vec::with_capacity(WRITE_BATCH);
    let mut handed = Instant::now();
    let ended = loop {
        if shared.shed_one() { break PieceEnd::Stopped; }
        let mut cancelled = shared.cancel.clone();
        let next = tokio::select! {
            result = tokio::time::timeout(SILENT, response.chunk()) => result,
            _ = cancelled.changed() => break PieceEnd::Stopped,
            _ = stop.wait_for(|stop| *stop) => break PieceEnd::Stopped,
            _ = held.wait_for(|held| *held) => break PieceEnd::Held,
        };
        let chunk = match next {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break PieceEnd::Retry(format!("The connection closed {} bytes early", end - offset)),
            Err(_) => break PieceEnd::Retry(format!("Download stalled for {} seconds", SILENT.as_secs())),
            Ok(Err(_)) => break PieceEnd::Retry(network_error("Package download stream")),
        };
        let mut data = &chunk[..];
        while !data.is_empty() && offset < end {
            let take = (data.len() as u64).min(until - offset) as usize;
            batch.extend_from_slice(&data[..take]);
            data = &data[take..];
            offset += take as u64;
            shared.arrived.fetch_add(take as u64, Ordering::Relaxed);
            lane.bytes.fetch_add(take as u64, Ordering::Relaxed);
            if offset < until { continue; }
            if !hand_over(shared, lane, writes, index, &mut batch, offset).await { return PieceEnd::Stopped; }
            handed = Instant::now();
            if offset >= end { return PieceEnd::Done; }
            // The next piece is this connection's unless an idle one took it over.
            if !shared.pieces.lock().unwrap().pass(lane.id, index) { return PieceEnd::Done; }
            index += 1;
            until = piece_end(index);
        }
        lane.heard.store(shared.clock(), Ordering::Relaxed);
        // Slow connections hand over what they have every second, so progress and the map follow.
        if batch.len() >= WRITE_BATCH || handed.elapsed() >= HAND_OVER {
            if !hand_over(shared, lane, writes, index, &mut batch, offset).await { return PieceEnd::Stopped; }
            handed = Instant::now();
        }
    };
    // Whatever arrived is kept: the piece continues from there.
    hand_over(shared, lane, writes, index, &mut batch, offset).await;
    ended
}

/// One connection: fetches runs of pieces until none are left or it is told to stop.
async fn connection(shared: Arc<Shared>, writes: tokio::sync::mpsc::Sender<Chunk>, lane: Arc<Lane>) {
    let mut stop = shared.stop.subscribe();
    let mut held = lane.held.subscribe();
    let mut failures = 0;
    loop {
        if shared.stopped() || lane.is_held() || shared.shed_one() { break; }
        let share = if shared.sparse { shared.wanted.load(Ordering::Relaxed).max(1) } else { 0 };
        let Some((first, start, end)) = shared.pieces.lock().unwrap().claim(lane.id, share) else { break; };
        let before = lane.bytes.load(Ordering::Relaxed);
        let result = fetch_run(&shared, &lane, &mut held, &mut stop, &writes, first, start, end).await;
        lane.asked.store(0, Ordering::Relaxed);
        shared.pieces.lock().unwrap().release(lane.id);
        if lane.bytes.load(Ordering::Relaxed) > before { failures = 0; }
        let others_open = shared.active.load(Ordering::Relaxed).saturating_sub(1);
        match result {
            PieceEnd::Done => {}
            PieceEnd::Stopped | PieceEnd::Held => break,
            PieceEnd::Busy(retry_after) => { shared.refused(retry_after, others_open, None); break; }
            // Refused while other connections use the same link, the server limits connections;
            // refused with none open, the link itself may have stopped working.
            PieceEnd::Refused(error) => { shared.refused(None, others_open, Some(error)); break; }
            PieceEnd::Retry(error) => {
                failures += 1;
                // While other connections receive, the server is turning this one away: the
                // others keep its pieces. With nothing arriving anywhere, the download is failing.
                let receiving = shared.receiving_besides(lane.id);
                if receiving > 0 {
                    if failures >= LIMIT_STRIKES { shared.limited(receiving); break; }
                } else if failures >= PIECE_ATTEMPTS {
                    shared.fail(format!("{error}. Retry continues from the completed pieces."));
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500 << failures.min(4))) => {},
                    _ = stop.wait_for(|stop| *stop) => break,
                    _ = held.wait_for(|held| *held) => break,
                }
            }
        }
    }
    shared.lanes.lock().unwrap().retain(|other| other.id != lane.id);
    shared.active.fetch_sub(1, Ordering::Relaxed);
}

/// What the supervisor keeps about its connections: their byte counts over the last
/// `SLOW_WINDOW`, and when it last replaced slow ones.
#[derive(Default)]
struct Watch { samples: HashMap<u32, VecDeque<(Instant, u64)>>, slow_at: Option<Instant> }
impl Watch {
    /// Closes the connections the server holds back while others receive: a request unanswered
    /// or a response silent for `HELD`, or one far slower than the rest. The download then stays
    /// at the connections that receive. Returns how many receive.
    fn observe(&mut self, shared: &Shared, now: Instant) -> usize {
        let lanes = shared.lanes.lock().unwrap().clone();
        let clock = shared.clock();
        self.samples.retain(|id, _| lanes.iter().any(|lane| lane.id == *id));
        for lane in &lanes {
            let samples = self.samples.entry(lane.id).or_default();
            samples.push_back((now, lane.bytes.load(Ordering::Relaxed)));
            while samples.len() > 2 && now.duration_since(samples[1].0) >= SLOW_WINDOW { samples.pop_front(); }
        }
        let receiving = lanes.iter().filter(|lane| lane.receiving(clock)).count();
        if receiving == 0 { return 0; }
        let mut held = 0;
        for lane in lanes.iter().filter(|lane| !lane.is_held() && !lane.receiving(clock)) {
            if lane.silent(clock).is_some_and(|silent| silent > HELD.as_millis() as u64) { lane.hold(); held += 1; }
        }
        // Rates over a full window. The best connection shows what the server and the line can
        // do; one far below it, over a whole window of its own response, is being slowed down.
        let rates: Vec<(&Arc<Lane>, f64)> = lanes.iter().filter(|lane| !lane.is_held()).filter_map(|lane| {
            let (at, bytes) = *self.samples.get(&lane.id)?.front()?;
            let span = now.duration_since(at);
            (span >= SLOW_WINDOW.mul_f64(0.9)).then(|| (lane, lane.bytes.load(Ordering::Relaxed).saturating_sub(bytes) as f64 / span.as_secs_f64()))
        }).collect();
        let best = rates.iter().map(|(_, rate)| *rate).fold(0., f64::max);
        let mut slow = 0;
        if rates.len() >= 2 && best >= SLOW_FLOOR && self.slow_at.is_none_or(|at| now.duration_since(at) >= SLOW_WINDOW) {
            for (lane, rate) in &rates {
                let answered = lane.answered.load(Ordering::Relaxed);
                let whole = answered > 0 && lane.asked.load(Ordering::Relaxed) > 0 && clock.saturating_sub(answered) >= SLOW_WINDOW.as_millis() as u64;
                if whole && rate * SLOW_FACTOR < best { lane.hold(); slow += 1; }
            }
            if slow > 0 { self.slow_at = Some(now); }
        }
        if held + slow > 0 { shared.limited(lanes.iter().filter(|lane| !lane.is_held() && lane.receiving(clock)).count()); }
        receiving
    }
}

/// The data file, and whether it is sparse.
fn open_data(data: &Path, total: u64) -> Result<(std::fs::File, bool), String> {
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(data).map_err(redact)?;
    if file.metadata().map_err(redact)?.len() > total { file.set_len(total).map_err(redact)?; }
    let sparse = make_sparse(&file);
    Ok((file, sparse))
}

/// Pieces land out of order; a sparse file stores them without zero-filling the gaps first.
fn make_sparse(file: &std::fs::File) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        extern "system" {
            fn DeviceIoControl(handle: *mut std::ffi::c_void, code: u32, input: *const std::ffi::c_void, input_size: u32,
                output: *mut std::ffi::c_void, output_size: u32, returned: *mut u32, overlapped: *mut std::ffi::c_void) -> i32;
        }
        const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
        let mut returned = 0u32;
        // FAT and exFAT drives have no sparse files and fill gaps instead.
        unsafe { DeviceIoControl(file.as_raw_handle() as _, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut()) != 0 }
    }
    #[cfg(not(windows))]
    { let _ = file; true }
}

fn write_at(file: &std::fs::File, mut offset: u64, mut data: &[u8]) -> std::io::Result<()> {
    #[cfg(windows)]
    use std::os::windows::fs::FileExt;
    #[cfg(unix)]
    use std::os::unix::fs::FileExt;
    while !data.is_empty() {
        #[cfg(windows)]
        let written = file.seek_write(data, offset)?;
        #[cfg(unix)]
        let written = file.write_at(data, offset)?;
        if written == 0 { return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "the drive accepted no bytes")); }
        data = &data[written..];
        offset += written as u64;
    }
    Ok(())
}

/// Saves the map after flushing the data it describes, so it never claims bytes the drive lost.
fn save_map(file: &std::fs::File, part: &Path, map: &Map) -> Result<(), String> {
    file.sync_data().map_err(redact)?;
    let draft = map_draft(part);
    std::fs::write(&draft, serde_json::to_vec(map).map_err(redact)?).map_err(redact)?;
    std::fs::rename(&draft, map_path(part)).map_err(redact)
}

async fn segmented(host: &dyn Host, http: &Client, cancel: &watch::Receiver<bool>, share: Arc<Share>, url: &str, part: &Path, total: u64) -> Result<(), String> {
    let data = data_path(part);
    let map = read_map(part).filter(|map| map.total == total).or_else(|| adopt(part, total));
    let (file, sparse) = open_data(&data, total)?;
    let pieces = Pieces::new(total, map.as_ref());
    session_log::write("download", &format!("{:.2} GiB in {} pieces, {:.2} GiB kept; {} file", total as f64 / 1_073_741_824.,
        pieces_for(total), pieces.written as f64 / 1_073_741_824., if sparse { "sparse" } else { "non-sparse (one piece per request)" }));
    let mut saved = pieces.map();
    save_map(&file, part, &saved)?;
    let (writes, mut queue) = tokio::sync::mpsc::channel::<Chunk>(32);
    let shared = Arc::new(Shared {
        url: url.into(), http: http.clone(), cancel: cancel.clone(), share, total, sparse, pieces: Mutex::new(pieces),
        stop: watch::channel(false).0, shed: AtomicUsize::new(0), active: AtomicUsize::new(0),
        lanes: Mutex::new(Vec::new()), lane_ids: AtomicU32::new(0), wanted: AtomicUsize::new(1), began: Instant::now(),
        arrived: AtomicU64::new(0), cap: AtomicUsize::new(usize::MAX), busy: Mutex::new(Busy::default()), fatal: Mutex::new(None),
    });
    // One thread writes every piece, so connections never block the runtime on the drive. It
    // ends when the last sender is dropped and its queue is written.
    let writer = {
        let file = file.try_clone().map_err(redact)?;
        let shared = shared.clone();
        std::thread::spawn(move || {
            while let Some(chunk) = queue.blocking_recv() {
                let end = chunk.offset + chunk.data.len() as u64;
                if let Err(error) = write_at(&file, chunk.offset, &chunk.data) {
                    shared.fail(format!("Download: could not write to the drive: {}", redact(error)));
                    // Keep receiving so no connection waits on a full queue.
                    while queue.blocking_recv().is_some() {}
                    return;
                }
                shared.pieces.lock().unwrap().wrote(chunk.piece, end);
            }
        })
    };
    let mut connections = JoinSet::new();
    let meter = Meter::new(0);
    // When bytes last arrived or reached the drive: a slow drive is not a stalled server.
    let (mut moved_at, mut last_arrived, mut last_written) = (Instant::now(), 0u64, 0u64);
    let mut saved_at = Instant::now();
    let mut saving: Option<tokio::task::JoinHandle<Result<(), String>>> = None;
    let mut guarded_at = Instant::now() - Duration::from_secs(10);
    let mut paused = false;
    // Connections start few and double while all of them receive.
    let (mut allowed, mut ramped_at) = (RAMP_FIRST, Instant::now());
    let mut lanes = Watch::default();
    let outcome: Result<(), String> = loop {
        while connections.try_join_next().is_some() {}
        let written = shared.pieces.lock().unwrap().written;
        let arrived = shared.arrived.load(Ordering::Relaxed);
        if let Some(error) = shared.fatal.lock().unwrap().clone() { break Err(error); }
        if *cancel.borrow() { break Err("cancelled".into()); }
        if shared.pieces.lock().unwrap().done() { break Ok(()); }
        let now = Instant::now();
        if arrived > last_arrived {
            (moved_at, last_arrived) = (now, arrived);
            let mut busy = shared.busy.lock().unwrap();
            (busy.since, busy.rounds, busy.refusals) = (None, 0, 0);
        }
        if written > last_written { (moved_at, last_written) = (now, written); }
        if host.paused() {
            if !paused {
                // Pausing closes the connections; their pieces continue from the bytes they sent.
                paused = true;
                shared.halt();
                host.progress(written, 0., 0);
            }
            moved_at = now;
        } else {
            if paused {
                if shared.active.load(Ordering::Relaxed) > 0 { tokio::time::sleep(Duration::from_millis(50)).await; continue; }
                paused = false;
                shared.stop.send_replace(false);
                shared.shed.store(0, Ordering::Relaxed);
                meter.reset(arrived);
                (allowed, ramped_at) = (RAMP_FIRST, now);
            }
            let holding_off = shared.holding_off(now);
            // Waiting out a busy server with no connection open isn't a stall.
            if holding_off && shared.active.load(Ordering::Relaxed) == 0 { moved_at = now; }
            if moved_at.elapsed() > STALL {
                let drive = shared.lanes.lock().unwrap().iter().any(|lane| lane.writing.load(Ordering::Relaxed));
                break Err(if drive { "The drive stopped accepting the download for 60 seconds. Retry continues from the completed pieces." }
                    else { "Download stalled for 60 seconds. Retry continues from the completed pieces." }.into());
            }
            shared.probe_cap();
            let receiving = lanes.observe(&shared, now);
            // Follow the scheduler's connection target, within what the server accepts.
            let wanted = shared.share.connections().min(shared.cap.load(Ordering::Relaxed)).max(1);
            shared.wanted.store(wanted, Ordering::Relaxed);
            // Connections that were closed or asked to leave no longer count.
            let leaving = shared.lanes.lock().unwrap().iter().filter(|lane| lane.is_held()).count();
            let running = shared.active.load(Ordering::Relaxed).saturating_sub(shared.shed.load(Ordering::Relaxed)).saturating_sub(leaving);
            let claimable = shared.pieces.lock().unwrap().claimable();
            if allowed < wanted && ramped_at.elapsed() >= RAMP_EVERY && running >= allowed && receiving >= running && !holding_off && claimable {
                (allowed, ramped_at) = ((allowed * 2).min(wanted), now);
            }
            allowed = allowed.min(wanted);
            let target = allowed;
            if running < target && !holding_off && claimable {
                for _ in running..target {
                    if shared.shed_one() { continue; }
                    shared.active.fetch_add(1, Ordering::Relaxed);
                    let lane = Arc::new(Lane::new(shared.lane_ids.fetch_add(1, Ordering::Relaxed) + 1));
                    shared.lanes.lock().unwrap().push(lane.clone());
                    connections.spawn(connection(shared.clone(), writes.clone(), lane));
                }
            } else if running > target {
                // The connections that aren't receiving go first; the others leave at their next chunk.
                let mut excess = running - target;
                let clock = shared.clock();
                for lane in shared.lanes.lock().unwrap().iter().filter(|lane| !lane.is_held() && !lane.receiving(clock)) {
                    if excess == 0 { break; }
                    lane.hold();
                    excess -= 1;
                }
                shared.shed.fetch_add(excess, Ordering::Relaxed);
            }
            host.progress(written, meter.sample(arrived), shared.active.load(Ordering::Relaxed));
            if guarded_at.elapsed() >= Duration::from_secs(5) {
                guarded_at = Instant::now();
                if let Err(error) = host.guard(part, total.saturating_sub(written)) { break Err(error); }
            }
        }
        if saved_at.elapsed() >= MAP_EVERY && saving.as_ref().is_none_or(|task| task.is_finished()) {
            saved_at = Instant::now();
            let snapshot = shared.pieces.lock().unwrap().map();
            if snapshot != saved {
                saved = snapshot.clone();
                let file = file.try_clone().map_err(redact)?;
                let path = part.to_path_buf();
                saving = Some(tokio::task::spawn_blocking(move || save_map(&file, &path, &snapshot)));
            }
        }
        let mut cancelled = cancel.clone();
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(250)) => {},
            _ = cancelled.changed() => {},
        }
    };
    // Stop every connection, then let the writer finish what they sent.
    shared.halt();
    while connections.join_next().await.is_some() {}
    drop(writes);
    tokio::task::spawn_blocking(move || writer.join()).await.map_err(redact)?.map_err(|_| "The download writer stopped unexpectedly".to_string())?;
    if let Some(task) = saving.take() { let _ = task.await; }
    let failure = shared.fatal.lock().unwrap().clone();
    let snapshot = shared.pieces.lock().unwrap().map();
    let complete = shared.pieces.lock().unwrap().done();
    let path = part.to_path_buf();
    match outcome.and_then(|()| failure.map_or(Ok(()), Err)) {
        Ok(()) if complete => {
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                file.sync_all().map_err(redact)?;
                if file.metadata().map_err(redact)?.len() < total { file.set_len(total).map_err(redact)?; }
                Ok(())
            }).await.map_err(redact)??;
            // Renamed before the map goes: a crash in between leaves a whole file that the next
            // attempt adopts, never an unmapped partial one.
            finish(&data, part).await?;
            let _ = fs::remove_file(map_path(part)).await;
            host.progress(total, 0., 0);
            Ok(())
        }
        result => {
            let _ = tokio::task::spawn_blocking(move || save_map(&file, &path, &snapshot)).await;
            Err(result.err().unwrap_or_else(|| "Download incomplete. Retry continues from the completed pieces.".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[test]
    fn pieces_resume_from_the_map_and_claim_from_written_bytes() {
        let total = 3 * PIECE + 5;
        let mut pieces = Pieces::new(total, Some(&Map { version: 1, total, piece: PIECE, prefix: 1, done: vec![2] }));
        assert_eq!(pieces.written, 2 * PIECE);
        assert_eq!(pieces.claim(1, 0), Some((1, PIECE, 2 * PIECE)));
        pieces.wrote(1, PIECE + 100);
        pieces.release(1);
        assert_eq!(pieces.claim(2, 0), Some((1, PIECE + 100, 2 * PIECE)), "a released piece continues where its bytes end");
        assert_eq!(pieces.claim(3, 0), Some((3, 3 * PIECE, total)));
        assert_eq!(pieces.claim(4, 0), None);
        pieces.wrote(3, total);
        pieces.wrote(1, 2 * PIECE);
        assert!(pieces.done());
        assert_eq!(pieces.map(), Map { version: 1, total, piece: PIECE, prefix: 4, done: vec![] });
    }

    #[test]
    fn runs_share_the_open_pieces_and_idle_connections_take_over_unreached_ones() {
        let total = 10 * PIECE;
        let mut pieces = Pieces::new(total, None);
        // Each request asks for a share of the open pieces: all of them for one connection, a
        // half of what is left for two.
        assert_eq!(pieces.claim(1, 1), Some((0, 0, 10 * PIECE)), "one request for the whole file");
        // Connections added later find none open and take the later half of the longest run its
        // connection hasn't reached (its first piece is the one it is on).
        assert_eq!(pieces.claim(2, 2), Some((6, 6 * PIECE, 10 * PIECE)));
        assert_eq!(pieces.claim(3, 3), Some((4, 4 * PIECE, 6 * PIECE)));
        // The first fetches piece 0, goes on through piece 3 and stops where its run now ends.
        for index in 0..4 { pieces.sent(index, (index as u64 + 1) * PIECE); }
        assert!(pieces.pass(1, 0) && pieces.pass(1, 1) && pieces.pass(1, 2));
        assert!(!pieces.pass(1, 3), "piece 4 belongs to the third connection now");
        // A connection that stops gives back what it hasn't fetched, which continues from its bytes.
        pieces.sent(6, 6 * PIECE + 77);
        pieces.release(2);
        assert_eq!(pieces.claim(4, 2), Some((6, 6 * PIECE + 77, 8 * PIECE)), "four open pieces between two connections");
        // Without a sparse file every request is one piece, so nothing is ever taken over.
        let mut single = Pieces::new(total, None);
        assert_eq!(single.claim(1, 0), Some((0, 0, PIECE)));
        assert_eq!(single.claim(2, 0), Some((1, PIECE, 2 * PIECE)));
        assert!(single.unreached().is_none());
        assert_eq!(content_range_end("bytes 100-199/1000"), Some(200));
        assert_eq!(content_range_end("bytes */1000"), None);
    }

    #[test]
    fn an_expired_link_is_renewed_while_each_new_one_gets_pieces_through() {
        assert!(renew_link(0, 0, 0), "the first renewal is always tried");
        assert!(renew_link(1, 5 * PIECE, 9 * PIECE), "the last link got pieces through");
        assert!(!renew_link(1, 5 * PIECE, 5 * PIECE), "a link that got nothing through isn't renewed again");
        assert!(!renew_link(LINK_RENEWALS, 0, PIECE), "renewals end");
    }

    #[test]
    fn a_stalled_or_busy_server_gets_a_new_attempt_but_drive_errors_do_not() {
        assert!(stalled("Download stalled for 60 seconds. Retry continues from the completed pieces."));
        assert!(stalled("Download stalled for 30 seconds. Retry continues from the completed pieces."));
        assert!(stalled("The server stayed busy (HTTP 503 or 429). Retry continues from the completed pieces."));
        assert!(!stalled("The drive stopped accepting the download for 60 seconds. Retry continues from the completed pieces."));
        assert!(!stalled("cancelled"));
        assert!(!stalled(&link_refused(reqwest::StatusCode::FORBIDDEN)));
    }

    #[test]
    fn overlapping_writes_never_count_twice() {
        let mut pieces = Pieces::new(PIECE, None);
        pieces.wrote(0, 1000);
        pieces.wrote(0, 600);
        pieces.wrote(0, 1000);
        assert_eq!(pieces.written, 1000);
    }

    #[test]
    fn content_range_totals_parse() {
        assert_eq!(content_range_total("bytes 0-0/12345"), Some(12345));
        assert_eq!(content_range_total("bytes 0-0/*"), None);
        assert_eq!(content_range_total(""), None);
    }

    #[test]
    fn a_map_keeps_only_pieces_inside_the_file() {
        let dir = workspace();
        let part = dir.join("download_short.part");
        let total = 4 * PIECE;
        // The map claims every piece, but the file only reaches into the third one.
        std::fs::write(map_path(&part), serde_json::to_vec(&Map { version: 1, total, piece: PIECE, prefix: 2, done: vec![3, 2, 3] }).unwrap()).unwrap();
        std::fs::File::create(data_path(&part)).unwrap().set_len(2 * PIECE + 10).unwrap();
        assert_eq!(read_map(&part), Some(Map { version: 1, total, piece: PIECE, prefix: 2, done: vec![] }));
        std::fs::File::create(data_path(&part)).unwrap().set_len(total).unwrap();
        assert_eq!(read_map(&part).unwrap().done, vec![2, 3], "sorted, without repeats");
        assert_eq!(resumed_bytes(&part), total);
        std::fs::remove_file(data_path(&part)).unwrap();
        assert_eq!(read_map(&part), None, "a map without its data file means nothing");
        assert_eq!(resumed_bytes(&part), 0);
        assert!(working_file(&data_path(&part)) && working_file(&map_path(&part)) && !working_file(&part));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /* A small HTTP/1.1 file server standing in for a debrid CDN: serves ranges or ignores them,
       refuses connections beyond a limit, can answer piece requests busy or forbidden, and can
       drop every Nth response halfway. Like a CDN it can also keep connections alive and hold
       requests back without answering: beyond a number of responses at once (`slots`, which
       then hang or trickle), beyond a request rate (`burst`, then one per `refill`), and after
       a number of requests (the link expires). `freeze_every` stops a response halfway without
       closing it, `first_chunk` slows down responses from the start of the file, and `range_cap`
       sends at most that many bytes of a range. Requested ranges are logged. */
    #[derive(Clone, Copy, PartialEq)]
    enum Over { Hang, Trickle, Reset }
    struct Server {
        data: Vec<u8>, ranges: bool, limit: usize, over_limit: u16, busy: AtomicUsize, forbid: bool,
        drop_every: usize, chunk: usize, pace: Duration, requests: AtomicUsize, open: AtomicUsize, served: AtomicU64,
        keep_alive: bool, slots: usize, over: Over, burst: usize, refill: Duration, freeze_every: usize, expire_after: usize,
        first_chunk: usize, range_cap: u64, asked: Mutex<Vec<(u64, u64)>>,
        ranged: AtomicUsize, serving: AtomicUsize, most_serving: AtomicUsize, tokens: Mutex<Option<(usize, Instant)>>,
    }
    impl Server {
        fn new(data: Vec<u8>) -> Self {
            Self { data, ranges: true, limit: 0, over_limit: 503, busy: AtomicUsize::new(0), forbid: false, drop_every: 0, chunk: 64 * 1024,
                pace: Duration::ZERO, requests: AtomicUsize::new(0), open: AtomicUsize::new(0), served: AtomicU64::new(0),
                keep_alive: false, slots: 0, over: Over::Hang, burst: 0, refill: Duration::ZERO, freeze_every: 0, expire_after: 0,
                first_chunk: 0, range_cap: 0, asked: Mutex::new(Vec::new()),
                ranged: AtomicUsize::new(0), serving: AtomicUsize::new(0), most_serving: AtomicUsize::new(0), tokens: Mutex::new(None) }
        }
        /// A CDN that keeps connections alive.
        fn cdn(data: Vec<u8>) -> Self { Self { keep_alive: true, ..Self::new(data) } }
        /// Waits for a request token; false when none ever comes.
        async fn token(&self) -> bool {
            if self.burst == 0 { return true; }
            loop {
                let wait = {
                    let mut tokens = self.tokens.lock().unwrap();
                    let (left, since) = tokens.get_or_insert((self.burst, Instant::now()));
                    if !self.refill.is_zero() {
                        while since.elapsed() >= self.refill { *left += 1; *since += self.refill; }
                    }
                    if *left > 0 { *left -= 1; return true; }
                    if self.refill.is_zero() { return false; }
                    self.refill.saturating_sub(since.elapsed())
                };
                tokio::time::sleep(wait).await;
            }
        }
    }
    async fn serve(server: Arc<Server>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await { tokio::spawn(respond(socket, server.clone())); }
        });
        format!("http://{address}/game.rar")
    }
    async fn refuse(socket: &mut tokio::net::TcpStream, status: u16, extra: &str) {
        let reason = match status { 403 => "Forbidden", 429 => "Too Many Requests", _ => "Service Unavailable" };
        let _ = socket.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n").as_bytes()).await;
    }
    /// Holds a request without answering until the client gives up on it.
    async fn hold(socket: &mut tokio::net::TcpStream) { let _ = socket.read(&mut [0u8; 1]).await; }
    async fn respond(mut socket: tokio::net::TcpStream, server: Arc<Server>) {
        loop {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") { if socket.read(&mut byte).await.unwrap_or(0) == 0 { return; } head.push(byte[0]); }
            if !answer(&mut socket, &server, &String::from_utf8_lossy(&head).to_ascii_lowercase()).await || !server.keep_alive { return; }
        }
    }
    /// Answers one request; false when the connection can't carry another.
    async fn answer(socket: &mut tokio::net::TcpStream, server: &Arc<Server>, head: &str) -> bool {
        let number = server.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let open = server.open.fetch_add(1, Ordering::SeqCst) + 1;
        struct Close<'a>(&'a AtomicUsize);
        impl Drop for Close<'_> { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::SeqCst); } }
        let _close = Close(&server.open);
        let total = server.data.len() as u64;
        let range = head.lines().find_map(|line| line.strip_prefix("range: bytes=")).map(|value| {
            let (start, end) = value.trim().split_once('-').unwrap();
            (start.parse::<u64>().unwrap(), end.parse::<u64>().ok().unwrap_or(total - 1).min(total - 1))
        });
        let piece = range.is_some_and(|range| range != (0, 0));
        if server.limit > 0 && open > server.limit { refuse(socket, server.over_limit, "").await; return false; }
        if piece && server.forbid { refuse(socket, 403, "").await; return false; }
        if piece && server.busy.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            refuse(socket, 503, "Retry-After: 1\r\n").await;
            return false;
        }
        let ranged = if piece { server.ranged.fetch_add(1, Ordering::SeqCst) + 1 } else { 0 };
        if let Some((start, end)) = range.filter(|_| piece) { server.asked.lock().unwrap().push((start, end + 1)); }
        if piece && server.expire_after > 0 && ranged > server.expire_after { refuse(socket, 403, "").await; return false; }
        if piece && !server.token().await { hold(socket).await; return false; }
        let mut pace = server.pace;
        let mut chunk = if range.is_some_and(|(start, _)| piece && start == 0) && server.first_chunk > 0 { server.first_chunk } else { server.chunk };
        let range = range.map(|(start, end)| (start, if piece && server.range_cap > 0 { end.min(start + server.range_cap - 1) } else { end }));
        // Responses beyond the slots don't hold one: they hang, are cut off, or trickle at 4 KiB/s.
        let serving = if piece { server.serving.fetch_add(1, Ordering::SeqCst) + 1 } else { 0 };
        let mut slot = piece.then(|| Close(&server.serving));
        if piece && server.slots > 0 && serving > server.slots {
            drop(slot.take());
            match server.over {
                Over::Hang => { hold(socket).await; return false; }
                Over::Reset => return false,
                Over::Trickle => (pace, chunk) = (Duration::from_millis(250), 1024),
            }
        } else if piece {
            server.most_serving.fetch_max(serving, Ordering::SeqCst);
        }
        let (status, start, end) = match range { Some((start, end)) if server.ranges => ("206 Partial Content", start, end), _ => ("200 OK", 0, total - 1) };
        let mut response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/x-rar-compressed\r\nContent-Disposition: attachment; filename=\"game.rar\"\r\nConnection: {}\r\n",
            end - start + 1, if server.keep_alive { "keep-alive" } else { "close" });
        if status.starts_with("206") { response += &format!("Content-Range: bytes {start}-{end}/{total}\r\n"); }
        response += "\r\n";
        if socket.write_all(response.as_bytes()).await.is_err() { return false; }
        let body = &server.data[start as usize..=end as usize];
        let dropped = server.drop_every > 0 && number % server.drop_every == 0 && body.len() > 1;
        let frozen = piece && server.freeze_every > 0 && ranged % server.freeze_every == 0 && body.len() > 1;
        let cut = if dropped || frozen { body.len() / 2 } else { body.len() };
        for chunk in body[..cut].chunks(chunk) {
            if socket.write_all(chunk).await.is_err() { return false; }
            server.served.fetch_add(chunk.len() as u64, Ordering::SeqCst);
            if !pace.is_zero() { tokio::time::sleep(pace).await; }
        }
        if frozen { hold(socket).await; return false; }
        cut == body.len()
    }

    #[derive(Default)]
    struct TestHost { paused: AtomicBool, reports: AtomicUsize, first_connections: AtomicUsize, most_connections: AtomicUsize, partial: AtomicBool, total: AtomicU64 }
    impl Host for TestHost {
        fn paused(&self) -> bool { self.paused.load(Ordering::SeqCst) }
        fn progress(&self, done: u64, _speed: f64, connections: usize) {
            self.reports.fetch_add(1, Ordering::SeqCst);
            if connections > 0 { let _ = self.first_connections.compare_exchange(0, connections, Ordering::SeqCst, Ordering::SeqCst); }
            self.most_connections.fetch_max(connections, Ordering::SeqCst);
            if done > 0 && done < self.total.load(Ordering::SeqCst) { self.partial.store(true, Ordering::SeqCst); }
        }
        fn guard(&self, _part: &Path, _remaining: u64) -> Result<(), String> { Ok(()) }
    }

    fn sample(bytes: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        (0..bytes).map(|_| { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state as u8 }).collect()
    }
    fn workspace() -> PathBuf {
        let dir = crate::test_output_root().join(format!("segmented-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    async fn run(server: &Arc<Server>, host: &TestHost, part: &Path, cancel: &watch::Receiver<bool>) -> Result<(), String> {
        let http = Client::new();
        let url = serve(server.clone()).await;
        host.total.store(server.data.len() as u64, Ordering::SeqCst);
        let ticket = scheduler::register_download(&format!("segmented-{}", Uuid::new_v4()), server.data.len() as u64);
        let probe = probe(&http, cancel, &url, 0).await?;
        download(host, &http, cancel, ticket.share(), &url, part, probe).await
    }
    fn finished_cleanly(part: &Path, data: &[u8]) {
        assert!(std::fs::read(part).unwrap() == data, "the file matches byte for byte");
        assert!(!map_path(part).exists() && !data_path(part).exists(), "only the finished file is left");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_segmented_download_is_byte_exact_over_several_connections() {
        let _serial = scheduler::test_serial();
        let server = Arc::new(Server::new(sample(3 * PIECE as usize + 12345)));
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_a.part");
        run(&server, &host, &part, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(host.most_connections.load(Ordering::SeqCst) > 1, "more than one connection was used");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn completed_pieces_and_earlier_partial_files_are_not_fetched_again() {
        let _serial = scheduler::test_serial();
        let data = sample(4 * PIECE as usize);
        let dir = workspace();
        // A map from an interrupted session: pieces 0 and 2 are on disk.
        let part = dir.join("download_map.part");
        let mut retained = vec![0u8; data.len()];
        retained[..PIECE as usize].copy_from_slice(&data[..PIECE as usize]);
        retained[2 * PIECE as usize..3 * PIECE as usize].copy_from_slice(&data[2 * PIECE as usize..3 * PIECE as usize]);
        std::fs::write(data_path(&part), &retained).unwrap();
        std::fs::write(map_path(&part), serde_json::to_vec(&Map { version: 1, total: data.len() as u64, piece: PIECE, prefix: 1, done: vec![2] }).unwrap()).unwrap();
        let server = Arc::new(Server::new(data.clone()));
        run(&server, &TestHost::default(), &part, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&part, &data);
        assert_eq!(server.served.load(Ordering::SeqCst), 2 * PIECE + 1, "only the two missing pieces and the probe byte");
        // An earlier version's partial file holds a contiguous start of the download.
        let legacy = dir.join("download_legacy.part");
        std::fs::write(&legacy, &data[..(PIECE + PIECE / 2) as usize]).unwrap();
        let server = Arc::new(Server::new(data.clone()));
        run(&server, &TestHost::default(), &legacy, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&legacy, &data);
        assert_eq!(server.served.load(Ordering::SeqCst), 3 * PIECE + 1, "the first whole piece was kept");
        // A finished file whose record wasn't updated (a crash right after the rename) is kept whole.
        let server = Arc::new(Server::new(data.clone()));
        run(&server, &TestHost::default(), &legacy, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&legacy, &data);
        assert_eq!(server.served.load(Ordering::SeqCst), 1, "nothing but the probe byte");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_server_without_ranges_gets_one_stream() {
        let _serial = scheduler::test_serial();
        let server = Arc::new(Server { ranges: false, ..Server::new(sample(PIECE as usize + 77)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_plain.part");
        // Pieces from an earlier attempt can't describe a stream that starts over.
        std::fs::write(map_path(&part), b"{}").unwrap();
        run(&server, &host, &part, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1, "the probe's own response carried the file");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn refused_and_dropped_connections_still_finish_the_file() {
        let _serial = scheduler::test_serial();
        for over_limit in [503, 403] {
            let server = Arc::new(Server { limit: 3, over_limit, drop_every: 4, ..Server::new(sample(6 * PIECE as usize)) });
            let (dir, host) = (workspace(), TestHost::default());
            let part = dir.join("download_flaky.part");
            run(&server, &host, &part, &watch::channel(false).1).await.unwrap_or_else(|error| panic!("HTTP {over_limit}: {error}"));
            finished_cleanly(&part, &server.data);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_busy_server_is_waited_out_without_hammering_it() {
        let _serial = scheduler::test_serial();
        let server = Arc::new(Server { busy: AtomicUsize::new(4), ..Server::new(sample(2 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_busy.part");
        let started = Instant::now();
        run(&server, &host, &part, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(started.elapsed() >= Duration::from_secs(2), "it waited as the server asked");
        assert!(server.requests.load(Ordering::SeqCst) < 20, "{} requests", server.requests.load(Ordering::SeqCst));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_refused_link_stops_the_download_as_expired() {
        let _serial = scheduler::test_serial();
        let server = Arc::new(Server { forbid: true, ..Server::new(sample(3 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_expired.part");
        let error = run(&server, &host, &part, &watch::channel(false).1).await.unwrap_err();
        assert!(link_expired(&error), "{error}");
        assert!(read_map(&part).is_some(), "the map stays for Retry");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_slow_trickle_is_progress_not_a_stall() {
        let _serial = scheduler::test_serial();
        // About 64 KiB/s on one connection: a whole write batch would take longer than a stall.
        let server = Arc::new(Server { chunk: 16 * 1024, pace: Duration::from_millis(250), ..Server::new(sample(320 * 1024)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_slow.part");
        run(&server, &host, &part, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(host.partial.load(Ordering::SeqCst), "progress moved before the piece was complete");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_closes_connections_and_cancel_keeps_the_map() {
        let _serial = scheduler::test_serial();
        let server = Arc::new(Server { pace: Duration::from_millis(20), ..Server::new(sample(12 * PIECE as usize)) });
        let dir = workspace();
        let host = Arc::new(TestHost::default());
        let part = dir.join("download_pause.part");
        host.paused.store(true, Ordering::SeqCst);
        let (cancel_tx, cancel) = watch::channel(false);
        let task = {
            let (server, host, part, cancel) = (server.clone(), host.clone(), part.clone(), cancel.clone());
            tokio::spawn(async move { run(&server, &host, &part, &cancel).await })
        };
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(server.requests.load(Ordering::SeqCst), 1, "a paused download opens no connections after the probe");
        host.paused.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(server.requests.load(Ordering::SeqCst) > 1, "resuming opens connections");
        // Pausing again closes them at once, even mid-chunk.
        host.paused.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(server.open.load(Ordering::SeqCst), 0, "no connection stays open while paused");
        host.paused.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(400)).await;
        cancel_tx.send(true).unwrap();
        assert_eq!(task.await.unwrap().unwrap_err(), "cancelled");
        assert!(read_map(&part).is_some(), "a cancelled download keeps its map for Retry");
        let resumed = resumed_bytes(&part);
        let server = Arc::new(Server::new(server.data.clone()));
        run(&server, &TestHost::default(), &part, &watch::channel(false).1).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(server.served.load(Ordering::SeqCst) <= server.data.len() as u64 - resumed + 1, "Retry skipped the completed pieces");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A download the user waits `limit` for.
    async fn run_within(limit: Duration, server: &Arc<Server>, host: &TestHost, part: &Path) -> Result<(), String> {
        tokio::time::timeout(limit, run(server, host, part, &watch::channel(false).1)).await
            .unwrap_or_else(|_| Err(format!("still downloading after {} s ({} of {} bytes on disk)", limit.as_secs(), resumed_bytes(part), server.data.len())))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_request_rate_limit_does_not_stall_the_download() {
        let _serial = scheduler::test_serial();
        // Many pieces per connection, as for a large file: 24 pieces over four connections.
        scheduler::configure(scheduler::Limits { connections: 4, ..Default::default() });
        // A CDN that answers a burst of 16 requests, then one more every four seconds.
        let server = Arc::new(Server { burst: 16, refill: Duration::from_secs(4), ..Server::cdn(sample(24 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_rate.part");
        run_within(Duration::from_secs(20), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(server.ranged.load(Ordering::SeqCst) <= 16, "{} requests for 24 pieces", server.ranged.load(Ordering::SeqCst));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connections_the_server_leaves_unanswered_go_to_the_others() {
        let _serial = scheduler::test_serial();
        // A CDN that serves three responses at once and never answers requests beyond that.
        let server = Arc::new(Server { slots: 3, pace: Duration::from_millis(2), ..Server::cdn(sample(24 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_held.part");
        run_within(Duration::from_secs(30), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_response_that_stops_midway_is_continued_elsewhere() {
        let _serial = scheduler::test_serial();
        // Every third response stops halfway and its connection stays open.
        let server = Arc::new(Server { freeze_every: 3, pace: Duration::from_millis(2), ..Server::cdn(sample(24 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_frozen.part");
        run_within(Duration::from_secs(30), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connections_slowed_to_a_trickle_are_replaced() {
        let _serial = scheduler::test_serial();
        // Three responses at once run at about 16 MB/s each; further ones trickle at 4 KiB/s.
        let server = Arc::new(Server { slots: 3, over: Over::Trickle, pace: Duration::from_millis(4), ..Server::cdn(sample(24 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_trickle.part");
        run_within(Duration::from_secs(40), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connections_the_server_cuts_off_do_not_fail_the_download() {
        let _serial = scheduler::test_serial();
        // Three responses at once at about 2 MB/s each; further connections are closed unanswered.
        let server = Arc::new(Server { slots: 3, over: Over::Reset, pace: Duration::from_millis(32), ..Server::cdn(sample(12 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_reset.part");
        run_within(Duration::from_secs(60), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn connections_start_few_and_grow_while_all_receive() {
        let _serial = scheduler::test_serial();
        // At most about 6 MB/s per connection, so the download outlasts a few ramp steps.
        let server = Arc::new(Server { pace: Duration::from_millis(10), ..Server::cdn(sample(24 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_ramp.part");
        run_within(Duration::from_secs(60), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(host.first_connections.load(Ordering::SeqCst) <= RAMP_FIRST, "started with {}", host.first_connections.load(Ordering::SeqCst));
        assert!(host.most_connections.load(Ordering::SeqCst) > RAMP_FIRST, "grew to {}", host.most_connections.load(Ordering::SeqCst));
        assert!(server.ranged.load(Ordering::SeqCst) < 24, "{} requests for 24 pieces", server.ranged.load(Ordering::SeqCst));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn idle_connections_take_over_pieces_a_slower_one_has_not_reached() {
        let _serial = scheduler::test_serial();
        // Two connections, each asking for a run of pieces; the run from the start of the file
        // arrives four times slower.
        scheduler::configure(scheduler::Limits { connections: 2, ..Default::default() });
        let server = Arc::new(Server { chunk: 256 * 1024, first_chunk: 64 * 1024, pace: Duration::from_millis(1), ..Server::cdn(sample(6 * PIECE as usize)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_takeover.part");
        run_within(Duration::from_secs(60), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        let asked = server.asked.lock().unwrap().clone();
        let (_, slow_end) = *asked.iter().find(|(start, _)| *start == 0).unwrap();
        assert!(slow_end > PIECE, "the first run is several pieces: {asked:?}");
        assert!(asked.iter().any(|(start, _)| *start > PIECE && *start < slow_end && start % PIECE == 0),
            "another connection took over pieces of that run: {asked:?}");
        assert!(server.served.load(Ordering::SeqCst) < server.data.len() as u64 + PIECE, "no piece was fetched twice");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_server_that_sends_less_than_asked_is_asked_again() {
        let _serial = scheduler::test_serial();
        // Every response ends after 3 MiB, inside a piece, with a Content-Range that says so.
        let server = Arc::new(Server { range_cap: 3 * 1024 * 1024, ..Server::cdn(sample(6 * PIECE as usize + 4321)) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_short.part");
        run_within(Duration::from_secs(30), &server, &host, &part).await.unwrap();
        finished_cleanly(&part, &server.data);
        assert!(server.served.load(Ordering::SeqCst) <= server.data.len() as u64 + 1, "nothing was fetched twice");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pausing_midway_through_runs_keeps_the_bytes_of_every_connection() {
        let _serial = scheduler::test_serial();
        scheduler::configure(scheduler::Limits { connections: 2, ..Default::default() });
        // At most about 3 MB/s per connection, each asking for a run of pieces.
        let server = Arc::new(Server { pace: Duration::from_millis(20), ..Server::cdn(sample(6 * PIECE as usize)) });
        let dir = workspace();
        let host = Arc::new(TestHost::default());
        let part = dir.join("download_pause_runs.part");
        let task = {
            let (server, host, part) = (server.clone(), host.clone(), part.clone());
            tokio::spawn(async move { run(&server, &host, &part, &watch::channel(false).1).await })
        };
        tokio::time::sleep(Duration::from_millis(1500)).await;
        host.paused.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(server.open.load(Ordering::SeqCst), 0, "no connection stays open while paused");
        let before = server.served.load(Ordering::SeqCst);
        assert!(before > 0 && before < server.data.len() as u64, "paused midway: {before} bytes");
        host.paused.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(60), task).await.expect("finished after resuming").unwrap().unwrap();
        finished_cleanly(&part, &server.data);
        assert!(server.served.load(Ordering::SeqCst) < server.data.len() as u64 + PIECE, "resuming continued from the bytes each connection had");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_link_that_expires_midway_stops_for_renewal_and_keeps_its_pieces() {
        let _serial = scheduler::test_serial();
        // The link stops working after its second piece request.
        let data = sample(24 * PIECE as usize);
        let server = Arc::new(Server { expire_after: 2, ..Server::cdn(data.clone()) });
        let (dir, host) = (workspace(), TestHost::default());
        let part = dir.join("download_renew.part");
        let error = run_within(Duration::from_secs(30), &server, &host, &part).await.unwrap_err();
        assert!(link_expired(&error), "{error}");
        let kept = resumed_bytes(&part);
        assert!(kept >= PIECE, "the pieces fetched before the link expired are kept");
        // A renewed link continues from them.
        let renewed = Arc::new(Server::cdn(data.clone()));
        run_within(Duration::from_secs(30), &renewed, &host, &part).await.unwrap();
        finished_cleanly(&part, &data);
        assert!(renewed.served.load(Ordering::SeqCst) <= data.len() as u64 - kept + 1, "only the missing pieces were fetched again");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
