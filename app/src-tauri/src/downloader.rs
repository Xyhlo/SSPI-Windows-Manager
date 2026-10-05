//! Segmented downloads. A file is fetched as fixed-size pieces over several HTTP connections at
//! once, each piece written in place by one writer thread. The scheduler decides how many
//! connections the download may use and can change that while it runs; pausing closes the
//! connections and keeps what was written. Until it is complete the file is `<part>.seg`, and its
//! completed pieces are recorded beside it in `<part>.map`, so Retry and the next session continue
//! where they stopped. (Earlier versions resume `<part>` by its length, so they never see a file
//! with gaps.) A server that ignores Range requests gets a single stream.
use super::*;
use scheduler::{Share, Ticket};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use tokio::task::JoinSet;

const PIECE: u64 = 8 * 1024 * 1024;
/// A connection hands its bytes to the writer at this size, or after `HAND_OVER` at the latest.
const WRITE_BATCH: usize = 1024 * 1024;
const HAND_OVER: Duration = Duration::from_secs(1);
/// The map is saved this often; each save flushes the file first.
const MAP_EVERY: Duration = Duration::from_secs(10);
/// No byte arrived on any connection for this long: stop (Retry continues).
const STALL: Duration = if cfg!(test) { Duration::from_secs(3) } else { Duration::from_secs(60) };
/// Failed requests in a row before a connection gives up on the download.
const PIECE_ATTEMPTS: u32 = 5;
/// How long a server may refuse every connection before the download gives up.
const BUSY_GIVE_UP: Duration = Duration::from_secs(120);
/// The longest a refusal (or its Retry-After) holds off new connections.
const BUSY_WAIT_MAX: Duration = Duration::from_secs(60);
/// After the server refused connections, one more is tried this often.
const CAP_PROBE: Duration = Duration::from_secs(5);
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
fn resumed_bytes(part: &Path) -> u64 {
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

/// Per piece: bytes on disk (`filled`, what the map may claim) and bytes handed to the writer
/// (`fetched`, where the next connection continues, so queued bytes are never fetched twice).
struct Pieces { total: u64, filled: Vec<u64>, fetched: Vec<u64>, claimed: Vec<bool>, written: u64, next: usize }
impl Pieces {
    fn new(total: u64, map: Option<&Map>) -> Self {
        let count = pieces_for(total);
        let mut pieces = Self { total, filled: vec![0; count], fetched: vec![0; count], claimed: vec![false; count], written: 0, next: 0 };
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
    fn open(&self, index: usize) -> bool { !self.claimed[index] && self.fetched[index] < self.len(index) }
    /// The next piece nobody is fetching, from where its fetched bytes end.
    fn claim(&mut self) -> Option<(usize, u64, u64)> {
        let index = (self.next..self.filled.len()).find(|index| self.open(*index))?;
        self.claimed[index] = true;
        let start = index as u64 * PIECE;
        Some((index, start + self.fetched[index], start + self.len(index)))
    }
    fn release(&mut self, index: usize) { self.claimed[index] = false; }
    fn claimable(&self) -> bool { (self.next..self.filled.len()).any(|index| self.open(index)) }
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
}

/// Shared by the supervisor, its connection tasks and the writer thread.
struct Shared {
    url: String,
    http: Client,
    cancel: watch::Receiver<bool>,
    share: Arc<Share>,
    pieces: Mutex<Pieces>,
    /// Pause, cancel or failure: every connection stops at once.
    stop: watch::Sender<bool>,
    /// Connections asked to leave because the connection target went down.
    shed: AtomicUsize,
    active: AtomicUsize,
    /// Bytes received this session, written yet or not.
    arrived: AtomicU64,
    /// Connections the server accepts (`usize::MAX` until it refuses one).
    cap: AtomicUsize,
    busy: Mutex<Busy>,
    fatal: Mutex<Option<String>>,
}

impl Shared {
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
            self.cap.fetch_min(others_open.max(1), Ordering::Relaxed);
            self.cap.load(Ordering::Relaxed)
        };
        self.share.limit_connections(cap);
    }
    /// Some time after the last refusal, allows one more connection to see if the server takes it.
    fn probe_cap(&self) {
        let cap = {
            let mut busy = self.busy.lock().unwrap();
            let cap = self.cap.load(Ordering::Relaxed);
            if cap == usize::MAX || busy.probed.is_some_and(|at| at.elapsed() < CAP_PROBE) { return; }
            busy.probed = Some(Instant::now());
            let raised = if cap + 1 >= CAP_LIFTED { usize::MAX } else { cap + 1 };
            self.cap.store(raised, Ordering::Relaxed);
            raised
        };
        self.share.limit_connections(cap);
    }
    /// Whether new connections must wait for the server, as of `now`.
    fn holding_off(&self, now: Instant) -> bool { self.busy.lock().unwrap().until.is_some_and(|until| until > now) }
}

enum PieceEnd { Done, Stopped, Busy(Option<Duration>), Refused(String), Retry(String) }

/// Hands batched bytes, which end at `offset`, to the writer. False when the writer is gone.
async fn hand_over(shared: &Shared, writes: &tokio::sync::mpsc::Sender<Chunk>, index: usize, batch: &mut Vec<u8>, offset: u64) -> bool {
    if batch.is_empty() { return true; }
    let data = std::mem::replace(batch, Vec::with_capacity(WRITE_BATCH));
    if writes.send(Chunk { piece: index, offset: offset - data.len() as u64, data }).await.is_err() { return false; }
    shared.pieces.lock().unwrap().sent(index, offset);
    true
}

async fn fetch_piece(shared: &Shared, stop: &mut watch::Receiver<bool>, writes: &tokio::sync::mpsc::Sender<Chunk>, index: usize, start: u64, end: u64) -> PieceEnd {
    let request = shared.http.get(&shared.url).header(reqwest::header::RANGE, format!("bytes={start}-{}", end - 1));
    let sent = tokio::select! {
        result = send(request, &shared.cancel, Duration::from_secs(30)) => result,
        _ = stop.wait_for(|stop| *stop) => return PieceEnd::Stopped,
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(error) if error == "cancelled" => return PieceEnd::Stopped,
        Err(error) => return PieceEnd::Retry(error),
    };
    match response.status().as_u16() {
        206 => {}
        status if busy_status(status) => return PieceEnd::Busy(retry_after(&response)),
        200 => { shared.fail("The server stopped answering ranged requests. Retry continues from the completed pieces.".into()); return PieceEnd::Stopped; }
        401 | 403 | 404 | 410 => return PieceEnd::Refused(link_refused(response.status())),
        _ => return PieceEnd::Retry(format!("Download failed: HTTP {}", response.status())),
    }
    if let Err(error) = validate_download_range(start, &header(&response, reqwest::header::CONTENT_RANGE).unwrap_or_default()) {
        shared.fail(error);
        return PieceEnd::Stopped;
    }
    let mut offset = start;
    let mut batch: Vec<u8> = Vec::with_capacity(WRITE_BATCH);
    let mut handed = Instant::now();
    let ended = loop {
        if shared.shed_one() { break PieceEnd::Stopped; }
        let mut cancelled = shared.cancel.clone();
        let next = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), response.chunk()) => result,
            _ = cancelled.changed() => break PieceEnd::Stopped,
            _ = stop.wait_for(|stop| *stop) => break PieceEnd::Stopped,
        };
        let chunk = match next {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break PieceEnd::Retry(format!("The connection closed {} bytes early", end - offset)),
            Err(_) => break PieceEnd::Retry("Download stalled for 30 seconds".into()),
            Ok(Err(_)) => break PieceEnd::Retry(network_error("Package download stream")),
        };
        let take = (chunk.len() as u64).min(end - offset) as usize;
        batch.extend_from_slice(&chunk[..take]);
        offset += take as u64;
        shared.arrived.fetch_add(take as u64, Ordering::Relaxed);
        // Slow connections hand over what they have every second, so progress and the map follow.
        if batch.len() >= WRITE_BATCH || offset >= end || handed.elapsed() >= HAND_OVER {
            if !hand_over(shared, writes, index, &mut batch, offset).await { return PieceEnd::Stopped; }
            handed = Instant::now();
        }
        if offset >= end { return PieceEnd::Done; }
    };
    // Whatever arrived is kept: the piece continues from there.
    hand_over(shared, writes, index, &mut batch, offset).await;
    ended
}

/// One connection: fetches pieces until none are left or it is told to stop.
async fn connection(shared: Arc<Shared>, writes: tokio::sync::mpsc::Sender<Chunk>) {
    let mut stop = shared.stop.subscribe();
    let mut failures = 0;
    loop {
        if shared.stopped() || shared.shed_one() { break; }
        let Some((index, start, end)) = shared.pieces.lock().unwrap().claim() else { break; };
        let result = fetch_piece(&shared, &mut stop, &writes, index, start, end).await;
        shared.pieces.lock().unwrap().release(index);
        let others_open = shared.active.load(Ordering::Relaxed).saturating_sub(1);
        match result {
            PieceEnd::Done => failures = 0,
            PieceEnd::Stopped => break,
            PieceEnd::Busy(retry_after) => { shared.refused(retry_after, others_open, None); break; }
            // Refused while other connections use the same link, the server limits connections;
            // refused with none open, the link itself may have stopped working.
            PieceEnd::Refused(error) => { shared.refused(None, others_open, Some(error)); break; }
            PieceEnd::Retry(error) => {
                failures += 1;
                if failures >= PIECE_ATTEMPTS { shared.fail(format!("{error}. Retry continues from the completed pieces.")); break; }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500 << failures.min(4))) => {},
                    _ = stop.wait_for(|stop| *stop) => break,
                }
            }
        }
    }
    shared.active.fetch_sub(1, Ordering::Relaxed);
}

fn open_data(data: &Path, total: u64) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(data).map_err(redact)?;
    if file.metadata().map_err(redact)?.len() > total { file.set_len(total).map_err(redact)?; }
    make_sparse(&file);
    Ok(file)
}

/// Pieces land out of order; a sparse file stores them without zero-filling the gaps first.
fn make_sparse(file: &std::fs::File) {
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
        // Best effort: FAT and exFAT drives have no sparse files and fill gaps instead.
        unsafe { DeviceIoControl(file.as_raw_handle() as _, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut()); }
    }
    #[cfg(not(windows))]
    let _ = file;
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
    let file = open_data(&data, total)?;
    let pieces = Pieces::new(total, map.as_ref());
    let mut saved = pieces.map();
    save_map(&file, part, &saved)?;
    let (writes, mut queue) = tokio::sync::mpsc::channel::<Chunk>(32);
    let shared = Arc::new(Shared {
        url: url.into(), http: http.clone(), cancel: cancel.clone(), share, pieces: Mutex::new(pieces),
        stop: watch::channel(false).0, shed: AtomicUsize::new(0), active: AtomicUsize::new(0), arrived: AtomicU64::new(0),
        cap: AtomicUsize::new(usize::MAX), busy: Mutex::new(Busy::default()), fatal: Mutex::new(None),
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
    let mut last_arrival = (Instant::now(), 0u64);
    let mut saved_at = Instant::now();
    let mut saving: Option<tokio::task::JoinHandle<Result<(), String>>> = None;
    let mut guarded_at = Instant::now() - Duration::from_secs(10);
    let mut paused = false;
    let outcome: Result<(), String> = loop {
        while connections.try_join_next().is_some() {}
        let written = shared.pieces.lock().unwrap().written;
        let arrived = shared.arrived.load(Ordering::Relaxed);
        if let Some(error) = shared.fatal.lock().unwrap().clone() { break Err(error); }
        if *cancel.borrow() { break Err("cancelled".into()); }
        if shared.pieces.lock().unwrap().done() { break Ok(()); }
        let now = Instant::now();
        if arrived > last_arrival.1 {
            last_arrival = (now, arrived);
            let mut busy = shared.busy.lock().unwrap();
            (busy.since, busy.rounds, busy.refusals) = (None, 0, 0);
        }
        if host.paused() {
            if !paused {
                // Pausing closes the connections; their pieces continue from the bytes they sent.
                paused = true;
                shared.halt();
                host.progress(written, 0., 0);
            }
            last_arrival.0 = now;
        } else {
            if paused {
                if shared.active.load(Ordering::Relaxed) > 0 { tokio::time::sleep(Duration::from_millis(50)).await; continue; }
                paused = false;
                shared.stop.send_replace(false);
                shared.shed.store(0, Ordering::Relaxed);
                meter.reset(arrived);
            }
            let holding_off = shared.holding_off(now);
            // Waiting out a busy server with no connection open isn't a stall.
            if holding_off && shared.active.load(Ordering::Relaxed) == 0 { last_arrival.0 = now; }
            if last_arrival.0.elapsed() > STALL { break Err("Download stalled for 60 seconds. Retry continues from the completed pieces.".into()); }
            shared.probe_cap();
            // Follow the scheduler's connection target, within what the server accepts.
            let target = shared.share.connections().min(shared.cap.load(Ordering::Relaxed)).max(1);
            let running = shared.active.load(Ordering::Relaxed).saturating_sub(shared.shed.load(Ordering::Relaxed));
            if running < target && !holding_off && shared.pieces.lock().unwrap().claimable() {
                for _ in running..target {
                    if shared.shed_one() { continue; }
                    shared.active.fetch_add(1, Ordering::Relaxed);
                    connections.spawn(connection(shared.clone(), writes.clone()));
                }
            } else if running > target {
                shared.shed.fetch_add(running - target, Ordering::Relaxed);
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
        assert_eq!(pieces.claim(), Some((1, PIECE, 2 * PIECE)));
        pieces.wrote(1, PIECE + 100);
        pieces.release(1);
        assert_eq!(pieces.claim(), Some((1, PIECE + 100, 2 * PIECE)), "a released piece continues where its bytes end");
        assert_eq!(pieces.claim(), Some((3, 3 * PIECE, total)));
        assert_eq!(pieces.claim(), None);
        pieces.wrote(3, total);
        pieces.wrote(1, 2 * PIECE);
        assert!(pieces.done());
        assert_eq!(pieces.map(), Map { version: 1, total, piece: PIECE, prefix: 4, done: vec![] });
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

    /* A small HTTP/1.1 file server: serves ranges or ignores them, refuses connections beyond a
       limit, can answer piece requests busy or forbidden, and can drop every Nth response halfway. */
    struct Server {
        data: Vec<u8>, ranges: bool, limit: usize, over_limit: u16, busy: AtomicUsize, forbid: bool,
        drop_every: usize, chunk: usize, pace: Duration, requests: AtomicUsize, open: AtomicUsize, served: AtomicU64,
    }
    impl Server {
        fn new(data: Vec<u8>) -> Self {
            Self { data, ranges: true, limit: 0, over_limit: 503, busy: AtomicUsize::new(0), forbid: false, drop_every: 0, chunk: 64 * 1024,
                pace: Duration::ZERO, requests: AtomicUsize::new(0), open: AtomicUsize::new(0), served: AtomicU64::new(0) }
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
    async fn respond(mut socket: tokio::net::TcpStream, server: Arc<Server>) {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") { if socket.read(&mut byte).await.unwrap_or(0) == 0 { return; } head.push(byte[0]); }
        let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
        let number = server.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let open = server.open.fetch_add(1, Ordering::SeqCst) + 1;
        struct Close(Arc<Server>);
        impl Drop for Close { fn drop(&mut self) { self.0.open.fetch_sub(1, Ordering::SeqCst); } }
        let _close = Close(server.clone());
        let total = server.data.len() as u64;
        let range = head.lines().find_map(|line| line.strip_prefix("range: bytes=")).map(|value| {
            let (start, end) = value.trim().split_once('-').unwrap();
            (start.parse::<u64>().unwrap(), end.parse::<u64>().ok().unwrap_or(total - 1).min(total - 1))
        });
        let piece = range.is_some_and(|range| range != (0, 0));
        if server.limit > 0 && open > server.limit { return refuse(&mut socket, server.over_limit, "").await; }
        if piece && server.forbid { return refuse(&mut socket, 403, "").await; }
        if piece && server.busy.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
            return refuse(&mut socket, 503, "Retry-After: 1\r\n").await;
        }
        let (status, start, end) = match range { Some((start, end)) if server.ranges => ("206 Partial Content", start, end), _ => ("200 OK", 0, total - 1) };
        let mut response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/x-rar-compressed\r\nContent-Disposition: attachment; filename=\"game.rar\"\r\nConnection: close\r\n", end - start + 1);
        if status.starts_with("206") { response += &format!("Content-Range: bytes {start}-{end}/{total}\r\n"); }
        response += "\r\n";
        if socket.write_all(response.as_bytes()).await.is_err() { return; }
        let body = &server.data[start as usize..=end as usize];
        let cut = if server.drop_every > 0 && number % server.drop_every == 0 && body.len() > 1 { body.len() / 2 } else { body.len() };
        for chunk in body[..cut].chunks(server.chunk) {
            if socket.write_all(chunk).await.is_err() { return; }
            server.served.fetch_add(chunk.len() as u64, Ordering::SeqCst);
            if !server.pace.is_zero() { tokio::time::sleep(server.pace).await; }
        }
    }

    #[derive(Default)]
    struct TestHost { paused: AtomicBool, reports: AtomicUsize, most_connections: AtomicUsize, partial: AtomicBool, total: AtomicU64 }
    impl Host for TestHost {
        fn paused(&self) -> bool { self.paused.load(Ordering::SeqCst) }
        fn progress(&self, done: u64, _speed: f64, connections: usize) {
            self.reports.fetch_add(1, Ordering::SeqCst);
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
}
