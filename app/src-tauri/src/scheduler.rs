//! Transfer scheduling: how many games download, extract, package and install at once, how the
//! download connection budget is shared, and which game goes first.
//!
//! - Downloads hold one of `Limits::downloads` slots while their bytes move and extraction one of
//!   `Limits::extractions`; a paused game lends its slot until it resumes. Packaging and console
//!   delivery have one slot each.
//! - The connection budget is split across downloading files in proportion to the bytes each game
//!   still has to fetch, so they finish at about the same time.
//! - One game can be the priority. It skips the download queue and takes about 90 % of the
//!   connections (so about 90 % of the bandwidth when the line is the limit; connections its
//!   server refuses go to the others), and it goes first for extraction, packaging and the
//!   console. While it downloads, extracts or packages, the other games' extraction and
//!   packaging processes run at background priority.
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{watch, Notify};

/// Share of the connections the priority game gets while others download.
const PRIORITY_SHARE: f64 = 0.9;
/// Weight floor so a nearly finished file still gets its fair connection.
const MIN_WEIGHT: f64 = 8. * 1024. * 1024.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Limits { pub downloads: usize, pub extractions: usize, pub connections: usize }
impl Default for Limits { fn default() -> Self { Self { downloads: 4, extractions: 2, connections: 16 } } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gate { Download, Extraction, Packaging, Console }
impl Gate {
    fn index(self) -> usize { match self { Self::Download => 0, Self::Extraction => 1, Self::Packaging => 2, Self::Console => 3 } }
}

#[derive(Default)]
struct GateState { holders: Vec<String>, waiters: Vec<(u64, String)> }

struct State {
    limits: Limits,
    priority: Option<String>,
    paused: HashSet<String>,
    gates: [GateState; 4],
    downloads: HashMap<String, Arc<Share>>,
    seq: u64,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State {
    limits: Limits::default(), priority: None, paused: HashSet::new(),
    gates: Default::default(), downloads: HashMap::new(), seq: 0,
}));
static CHANGED: LazyLock<Notify> = LazyLock::new(Notify::new);

fn state() -> MutexGuard<'static, State> { STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }
fn changed(mut state: MutexGuard<'static, State>) {
    rebalance(&mut state);
    drop(state);
    CHANGED.notify_waiters();
}

/* ---------------------------------------------------------------- configuration and job state */

pub(super) fn configure(limits: Limits) {
    let mut s = state();
    s.limits = Limits { downloads: limits.downloads.clamp(1, 8), extractions: limits.extractions.clamp(1, 4), connections: limits.connections.clamp(1, 32) };
    changed(s);
}

/// Makes `job` the only priority game, or clears the priority.
pub(super) fn set_priority(job: Option<String>) {
    let mut s = state();
    s.priority = job;
    changed(s);
}

pub(super) fn set_paused(job: &str, paused: bool) {
    let mut s = state();
    if paused { s.paused.insert(job.to_string()); } else { s.paused.remove(job); }
    changed(s);
}
/// The job stopped (finished, failed or was cancelled): forget its pause.
pub(super) fn finished(job: &str) { set_paused(job, false); }

/// Whether `job`'s extraction and packaging should give way: another game is the priority and is
/// downloading, extracting or packaging right now. Not while the priority game waits for a slot
/// (perhaps the one this job holds) or is with the console, so giving way never slows it down.
pub(super) fn should_yield(job: &str) -> bool {
    let s = state();
    let Some(priority) = s.priority.as_deref().filter(|priority| *priority != job && !s.paused.contains(*priority)) else { return false; };
    [Gate::Download, Gate::Extraction, Gate::Packaging].into_iter()
        .any(|gate| s.gates[gate.index()].holders.iter().any(|holder| holder == priority))
}

/* ---------------------------------------------------------------- slots */

fn capacity(s: &State, gate: Gate) -> usize {
    match gate { Gate::Download => s.limits.downloads, Gate::Extraction => s.limits.extractions, Gate::Packaging | Gate::Console => 1 }
}
fn occupied(s: &State, gate: Gate) -> usize {
    s.gates[gate.index()].holders.iter().filter(|job| match gate {
        // The priority game never takes a download slot from the others.
        Gate::Download if s.priority.as_deref() == Some(job.as_str()) => false,
        // A paused download or extraction moves no bytes, so its slot is lent until it resumes.
        Gate::Download | Gate::Extraction => !s.paused.contains(*job),
        Gate::Packaging | Gate::Console => true,
    }).count()
}

/// Ok when `job` (waiting with `seq`) may take a slot now, otherwise how many eligible games are ahead.
fn admission(s: &State, gate: Gate, job: &str, seq: u64) -> Result<(), usize> {
    let priority = |candidate: &str| s.priority.as_deref() == Some(candidate);
    let key = |candidate: &str, order: u64| (!priority(candidate), order);
    let mine = key(job, seq);
    let ahead = s.gates[gate.index()].waiters.iter()
        .filter(|(order, other)| other != job && !s.paused.contains(other) && key(other, *order) < mine)
        .count();
    // A paused game keeps its place in line but doesn't take a slot.
    if s.paused.contains(job) { return Err(ahead); }
    if gate == Gate::Download && priority(job) { return Ok(()); }
    if ahead < capacity(s, gate).saturating_sub(occupied(s, gate)) { Ok(()) } else { Err(ahead) }
}

/// A held slot; dropping it lets the next game in.
pub(super) struct Permit { gate: Gate, job: String }
impl Drop for Permit {
    fn drop(&mut self) {
        let mut s = state();
        let holders = &mut s.gates[self.gate.index()].holders;
        if let Some(index) = holders.iter().position(|job| *job == self.job) { holders.remove(index); }
        changed(s);
    }
}

/// Leaves the queue if waiting ends without a slot (cancelled, or the caller gave up).
struct Waiting { gate: Gate, seq: u64 }
impl Drop for Waiting {
    fn drop(&mut self) {
        let mut s = state();
        s.gates[self.gate.index()].waiters.retain(|(order, _)| *order != self.seq);
        changed(s);
    }
}

fn enqueue(gate: Gate, job: &str) -> Waiting {
    let mut s = state();
    s.seq += 1;
    let seq = s.seq;
    s.gates[gate.index()].waiters.push((seq, job.to_string()));
    Waiting { gate, seq }
}

fn try_admit(gate: Gate, job: &str, seq: u64) -> Result<Permit, usize> {
    let mut s = state();
    admission(&s, gate, job, seq)?;
    let g = &mut s.gates[gate.index()];
    g.waiters.retain(|(order, _)| *order != seq);
    g.holders.push(job.to_string());
    Ok(Permit { gate, job: job.to_string() })
}

/// Waits for a slot in async code. `waiting(ahead)` is called whenever the place in line changes.
pub(super) async fn acquire(gate: Gate, job: &str, cancel: &watch::Receiver<bool>, mut waiting: impl FnMut(usize)) -> Result<Permit, String> {
    let queued = enqueue(gate, job);
    let mut announced = None;
    loop {
        if *cancel.borrow() { return Err("cancelled".into()); }
        let wake = CHANGED.notified();
        match try_admit(gate, job, queued.seq) {
            Ok(permit) => return Ok(permit),
            Err(ahead) => if announced != Some(ahead) { announced = Some(ahead); waiting(ahead); },
        }
        let mut cancelled = cancel.clone();
        tokio::select! {
            _ = wake => {},
            _ = tokio::time::sleep(Duration::from_millis(250)) => {},
            _ = cancelled.changed() => return Err("cancelled".into()),
        }
    }
}

/// Waits for a slot on a blocking thread. `checkpoint` is polled while waiting (it may pause).
pub(super) fn acquire_blocking(gate: Gate, job: &str, checkpoint: &dyn Fn() -> Result<(), String>, waiting: &dyn Fn(usize)) -> Result<Permit, String> {
    let queued = enqueue(gate, job);
    let mut announced = None;
    loop {
        checkpoint()?;
        match try_admit(gate, job, queued.seq) {
            Ok(permit) => return Ok(permit),
            Err(ahead) => if announced != Some(ahead) { announced = Some(ahead); waiting(ahead); },
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Runs `work` in an extraction slot. While another game is the priority, the extraction
/// processes this thread drives run at background priority (see `yield_point`).
pub(super) fn extraction_slot<T>(job: &str, checkpoint: &dyn Fn() -> Result<(), String>, waiting: &dyn Fn(usize), work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    // Clears the thread's note however this ends, including a wait cancelled before any work.
    let _foreground = Foreground;
    let _permit = acquire_blocking(Gate::Extraction, job, checkpoint, waiting)?;
    checkpoint()?;
    yield_point(job);
    work()
}

/* ---------------------------------------------------------------- background priority */

thread_local! { static YIELDING: Cell<bool> = const { Cell::new(false) }; }

/// Called from a job's blocking checkpoints: notes whether the worker processes this thread
/// drives should run at background priority now. Only those processes change priority; threads
/// of this process keep theirs, so a lock one of them holds never waits behind background work.
pub(super) fn yield_point(job: &str) { YIELDING.with(|cell| cell.set(should_yield(job))); }
/// Whether the worker processes this thread drives should run at background priority.
pub(super) fn yielding() -> bool { YIELDING.with(Cell::get) }
/// Clears the note when the work ends, however it ends.
struct Foreground;
impl Drop for Foreground { fn drop(&mut self) { YIELDING.with(|cell| cell.set(false)); } }

/* ---------------------------------------------------------------- download shares */

/// One downloading file's share of the connection budget.
pub(super) struct Share {
    job: String,
    remaining: AtomicU64,
    target: AtomicUsize,
    cap: AtomicUsize,
}

impl Share {
    fn new(job: &str, remaining: u64) -> Self {
        Self { job: job.into(), remaining: AtomicU64::new(remaining), target: AtomicUsize::new(1), cap: AtomicUsize::new(usize::MAX) }
    }
    /// Connections this download may use now.
    pub(super) fn connections(&self) -> usize { self.target.load(Ordering::Relaxed).max(1) }
    /// Bytes the game still has to fetch, across all of its files; this sets its share.
    pub(super) fn set_remaining(&self, bytes: u64) { self.remaining.store(bytes, Ordering::Relaxed); }
    /// Keeps this download at `max` connections (its server refused more); `usize::MAX` lifts the limit.
    pub(super) fn limit_connections(&self, max: usize) {
        let max = max.max(1);
        if self.cap.swap(max, Ordering::Relaxed) != max { changed(state()); }
    }
    fn cap(&self) -> usize { self.cap.load(Ordering::Relaxed).max(1) }
    fn weight(&self) -> f64 { (self.remaining.load(Ordering::Relaxed) as f64).max(MIN_WEIGHT) }
}

/// Registration of a downloading file; dropping it releases its connections to the others.
pub(super) struct Ticket(Arc<Share>);
impl std::ops::Deref for Ticket { type Target = Share; fn deref(&self) -> &Share { &self.0 } }
impl Ticket {
    /// A handle for the download's connection tasks; the registration still ends with the ticket.
    pub(super) fn share(&self) -> Arc<Share> { self.0.clone() }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut s = state();
        if s.downloads.get(&self.0.job).is_some_and(|share| Arc::ptr_eq(share, &self.0)) { s.downloads.remove(&self.0.job); }
        changed(s);
    }
}

/// Registers a file `job` is about to fetch; the game still has `remaining` bytes to download.
pub(super) fn register_download(job: &str, remaining: u64) -> Ticket {
    let share = Arc::new(Share::new(job, remaining));
    let mut s = state();
    s.downloads.insert(job.to_string(), share.clone());
    changed(s);
    Ticket(share)
}

/// Splits `budget` connections across `shares` by remaining bytes (Sainte-Laguë), at least one
/// each and never above a share's cap.
fn split(shares: &[Arc<Share>], budget: usize) -> HashMap<String, usize> {
    let mut assigned: HashMap<String, usize> = shares.iter().map(|share| (share.job.clone(), 1)).collect();
    for _ in shares.len()..budget {
        let next = shares.iter().filter(|share| assigned[&share.job] < share.cap())
            .max_by(|a, b| (a.weight() / (assigned[&a.job] as f64 + 0.5)).total_cmp(&(b.weight() / (assigned[&b.job] as f64 + 0.5))));
        let Some(share) = next else { break; };
        *assigned.get_mut(&share.job).unwrap() += 1;
    }
    assigned
}

fn rebalance(s: &mut State) {
    let budget = s.limits.connections.max(1);
    let active: Vec<Arc<Share>> = s.downloads.values().filter(|share| !s.paused.contains(&share.job)).cloned().collect();
    let first = s.priority.as_deref().and_then(|job| active.iter().find(|share| share.job == job)).cloned();
    let targets = match first {
        Some(first) if active.len() > 1 => {
            let others: Vec<_> = active.iter().filter(|share| share.job != first.job).cloned().collect();
            // At least one connection for each other game; whatever the priority game's server
            // won't take goes to them too.
            let rest = ((budget as f64) * (1. - PRIORITY_SHARE)).round().max(others.len() as f64) as usize;
            let mine = budget.saturating_sub(rest).max(1).min(first.cap());
            let mut targets = split(&others, budget.saturating_sub(mine));
            targets.insert(first.job.clone(), mine);
            targets
        }
        _ => split(&active, budget),
    };
    for share in &active { share.target.store(targets.get(&share.job).copied().unwrap_or(1), Ordering::Relaxed); }
}

/// Refreshes the connection split every second as the remaining bytes change.
pub(super) fn start() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        tauri::async_runtime::spawn(async {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop { interval.tick().await; rebalance(&mut state()); }
        });
    });
}

/// The scheduler is process-wide: tests that change it run one at a time, on their own job
/// names, and put the defaults back when they finish.
#[cfg(test)]
pub(super) struct TestSerial(#[allow(dead_code)] MutexGuard<'static, ()>);
#[cfg(test)]
pub(super) fn test_serial() -> TestSerial {
    static SERIAL: Mutex<()> = Mutex::new(());
    let guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    reset_for_tests();
    TestSerial(guard)
}
#[cfg(test)]
impl Drop for TestSerial { fn drop(&mut self) { reset_for_tests(); } }
#[cfg(test)]
fn reset_for_tests() {
    let mut s = state();
    s.limits = Limits::default(); s.priority = None; s.paused.clear();
    changed(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial() -> TestSerial { test_serial() }
    fn no_cancel() -> watch::Receiver<bool> { watch::channel(false).1 }
    fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap() }

    #[test]
    fn an_extraction_wait_is_cancellable_and_the_slot_returns_after_an_error() {
        use std::sync::mpsc;
        let _serial = serial();
        configure(Limits { downloads: 4, extractions: 1, connections: 16 });
        let (started_tx, started_rx) = mpsc::channel(); let (release_tx, release_rx) = mpsc::channel::<()>();
        let first = std::thread::spawn(move || extraction_slot("extract-first", &|| Ok(()), &|_| {}, || {
            started_tx.send(()).unwrap(); release_rx.recv_timeout(Duration::from_secs(5)).unwrap(); Err::<(), _>("test error".to_string())
        }));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let result = extraction_slot("extract-second", &|| if cancelled.load(Ordering::Relaxed) { Err("cancelled".into()) } else { Ok(()) },
            &|_| cancelled.store(true, Ordering::Relaxed), || -> Result<(), String> { panic!("A second extraction must wait") });
        assert_eq!(result, Err("cancelled".into()));
        release_tx.send(()).unwrap(); assert!(first.join().unwrap().is_err());
        assert!(extraction_slot("extract-third", &|| Ok(()), &|_| {}, || Ok(())).is_ok());
        let s = state(); let gate = &s.gates[Gate::Extraction.index()];
        assert!(!gate.waiters.iter().any(|(_, job)| job.starts_with("extract-")) && !gate.holders.iter().any(|job| job.starts_with("extract-")));
    }

    #[test]
    fn two_extractions_run_together_by_default() {
        let _serial = serial();
        let (started_tx, started_rx) = std::sync::mpsc::channel(); let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let first = std::thread::spawn(move || extraction_slot("together-first", &|| Ok(()), &|_| {}, || {
            started_tx.send(()).unwrap(); release_rx.recv_timeout(Duration::from_secs(5)).unwrap(); Ok::<(), String>(())
        }));
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(extraction_slot("together-second", &|| Ok(()), &|_| panic!("The second extraction must not wait"), || Ok(())).is_ok());
        release_tx.send(()).unwrap(); first.join().unwrap().unwrap();
    }

    #[test]
    fn connections_follow_remaining_bytes_and_add_up_to_the_budget() {
        let _serial = serial();
        let big = register_download("split-big", 30 << 30);
        let small = register_download("split-small", 10 << 30);
        assert_eq!(big.connections() + small.connections(), 16);
        assert_eq!((big.connections(), small.connections()), (12, 4));
        let third = register_download("split-third", 10 << 30);
        assert_eq!(big.connections() + small.connections() + third.connections(), 16);
        assert!(big.connections() > small.connections() && small.connections() == third.connections());
        drop(big); drop(small);
        assert_eq!(third.connections(), 16, "the last download gets every connection back");
    }

    #[test]
    fn a_refused_connection_count_caps_a_download_until_it_is_lifted() {
        let _serial = serial();
        let capped = register_download("cap-a", 20 << 30);
        let other = register_download("cap-b", 1 << 30);
        capped.limit_connections(3);
        assert_eq!((capped.connections(), other.connections()), (3, 13), "the others take what the capped download can't use");
        capped.limit_connections(usize::MAX);
        assert_eq!(capped.connections() + other.connections(), 16);
        assert!(capped.connections() > 3, "the split follows the remaining bytes again");
    }

    #[test]
    fn the_priority_game_takes_most_connections_and_paused_games_none() {
        let _serial = serial();
        let first = register_download("prio-first", 1 << 30);
        let second = register_download("prio-second", 40 << 30);
        let third = register_download("prio-third", 40 << 30);
        set_priority(Some("prio-first".into()));
        assert_eq!(first.connections(), 14);
        assert_eq!(second.connections() + third.connections(), 2);
        first.limit_connections(4);
        assert_eq!(first.connections(), 4);
        assert_eq!(second.connections() + third.connections(), 12, "connections the priority game's server refuses go to the others");
        first.limit_connections(usize::MAX);
        set_paused("prio-first", true);
        assert_eq!(second.connections() + third.connections(), 16, "a paused priority game leaves the budget to the others");
    }

    #[test]
    fn slots_admit_in_order_with_the_priority_game_first_and_skip_paused_waiters() {
        let _serial = serial();
        configure(Limits { downloads: 1, extractions: 1, connections: 16 });
        let cancel = no_cancel();
        runtime().block_on(async {
            let holder = acquire(Gate::Extraction, "slot-holder", &cancel, |_| {}).await.unwrap();
            let first_seq = enqueue(Gate::Extraction, "slot-early");
            let late_seq = enqueue(Gate::Extraction, "slot-late");
            assert_eq!(admission(&state(), Gate::Extraction, "slot-late", late_seq.seq), Err(1));
            set_priority(Some("slot-late".into()));
            assert_eq!(admission(&state(), Gate::Extraction, "slot-late", late_seq.seq), Err(0), "no free slot yet");
            drop(holder);
            assert!(admission(&state(), Gate::Extraction, "slot-late", late_seq.seq).is_ok(), "the priority game goes first");
            assert_eq!(admission(&state(), Gate::Extraction, "slot-early", first_seq.seq), Err(1));
            set_priority(None);
            set_paused("slot-early", true);
            assert!(admission(&state(), Gate::Extraction, "slot-late", late_seq.seq).is_ok(), "a paused game doesn't hold up the line");
            assert_eq!(admission(&state(), Gate::Extraction, "slot-early", first_seq.seq), Err(0));
        });
    }

    #[test]
    fn paused_downloads_and_extractions_lend_their_slot_and_packaging_does_not() {
        let _serial = serial();
        configure(Limits { downloads: 1, extractions: 1, connections: 16 });
        let cancel = no_cancel();
        runtime().block_on(async {
            for gate in [Gate::Download, Gate::Extraction, Gate::Packaging] {
                let held = acquire(gate, "lend-first", &cancel, |_| {}).await.unwrap();
                let waiting = enqueue(gate, "lend-second");
                assert_eq!(admission(&state(), gate, "lend-second", waiting.seq), Err(0));
                set_paused("lend-first", true);
                assert_eq!(admission(&state(), gate, "lend-second", waiting.seq).is_ok(), gate != Gate::Packaging, "{gate:?}");
                set_paused("lend-first", false);
                drop(held);
            }
        });
    }

    #[test]
    fn the_priority_game_skips_the_download_queue() {
        let _serial = serial();
        configure(Limits { downloads: 1, extractions: 2, connections: 16 });
        let cancel = no_cancel();
        runtime().block_on(async {
            let first = acquire(Gate::Download, "dl-first", &cancel, |_| {}).await.unwrap();
            set_priority(Some("dl-third".into()));
            let third = acquire(Gate::Download, "dl-third", &cancel, |_| {}).await.unwrap();
            assert_eq!(state().gates[0].holders.iter().filter(|job| job.starts_with("dl-")).count(), 2, "the priority game downloads beside the full slot");
            let waiting = enqueue(Gate::Download, "dl-second");
            assert_eq!(admission(&state(), Gate::Download, "dl-second", waiting.seq), Err(0), "and doesn't take the slot from the others");
            drop((first, third));
        });
    }

    #[test]
    fn a_cancelled_waiter_leaves_the_queue() {
        let _serial = serial();
        configure(Limits { downloads: 1, extractions: 1, connections: 16 });
        let (cancel_tx, cancel) = watch::channel(false);
        runtime().block_on(async {
            let holder = acquire(Gate::Packaging, "cancel-holder", &no_cancel(), |_| {}).await.unwrap();
            let waiter = acquire(Gate::Packaging, "cancel-waiter", &cancel, |_| {});
            let stop = async { tokio::time::sleep(Duration::from_millis(50)).await; cancel_tx.send(true).unwrap(); };
            let (result, ()) = tokio::join!(waiter, stop);
            assert_eq!(result.err().as_deref(), Some("cancelled"));
            assert!(!state().gates[Gate::Packaging.index()].waiters.iter().any(|(_, job)| job.starts_with("cancel-")));
            drop(holder);
        });
    }

    #[test]
    fn others_give_way_only_while_the_priority_game_is_working() {
        let _serial = serial();
        configure(Limits { downloads: 4, extractions: 1, connections: 16 });
        let cancel = no_cancel();
        runtime().block_on(async {
            let other = acquire(Gate::Extraction, "yield-other", &cancel, |_| {}).await.unwrap();
            set_priority(Some("yield-first".into()));
            assert!(!should_yield("yield-other"), "the priority game is only waiting, perhaps for this very slot");
            let download = acquire(Gate::Download, "yield-first", &cancel, |_| {}).await.unwrap();
            assert!(should_yield("yield-other") && !should_yield("yield-first"));
            set_paused("yield-first", true);
            assert!(!should_yield("yield-other"), "a paused priority game holds nobody back");
            set_paused("yield-first", false);
            drop(download);
            let console = acquire(Gate::Console, "yield-first", &cancel, |_| {}).await.unwrap();
            assert!(!should_yield("yield-other"), "the console stage doesn't compete for this PC's disk");
            drop(console);
            let packaging = acquire(Gate::Packaging, "yield-first", &cancel, |_| {}).await.unwrap();
            assert!(should_yield("yield-other"));
            set_priority(None);
            assert!(!should_yield("yield-other"));
            drop((packaging, other));
        });
        // The note a checkpoint leaves on its thread is cleared when the extraction ends.
        set_priority(Some("note-first".into()));
        runtime().block_on(async {
            let _download = acquire(Gate::Download, "note-first", &cancel, |_| {}).await.unwrap();
            assert!(extraction_slot("note-other", &|| Ok(()), &|_| {}, || Ok(yielding())).unwrap());
            assert!(!yielding());
        });
    }
}
