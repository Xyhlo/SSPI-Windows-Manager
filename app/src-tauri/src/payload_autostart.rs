//! Opt-in payload autostart: the chosen payloads go to the console's loader in their saved order,
//! each after its own delay, on Run now or a confirmed console wake. Never on app startup.
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::PathBuf, sync::{Mutex, OnceLock}, time::{SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};

use crate::{payloads, AppState};

const MAX_ATTEMPTS: u8 = 3;
const RETRY_DELAYS: [u64; 3] = [0, 15_000, 45_000];
const START_GRACE_MS: u64 = 5_000;
const BUSY_RETRY_MS: u64 = 5_000;
const MAX_STEPS: usize = 16;
const MAX_DELAY_MS: u64 = 120_000;
const CONNECT_FAILED: &str = "Could not connect to the payload loader.";
const CONNECT_TIMEOUT: &str = "The payload loader connection timed out.";
const SEND_BUSY: &str = "Another payload is being sent. Wait for it to finish.";
const STOPPED: &str = "Stopped. The remaining payloads were not sent.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Step {
    payload_id: String,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default)]
    process_name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sequence {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    steps: Vec<Step>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    /// 2.23 kept one payload per console. It is read once and replaced by `sequences` on the next save.
    #[serde(default, skip_serializing)]
    payloads: BTreeMap<String, String>,
    #[serde(default)]
    sequences: BTreeMap<String, Sequence>,
}

impl Config {
    fn sequence(&self, target: &str) -> Sequence {
        if let Some(sequence) = self.sequences.get(target) { return sequence.clone(); }
        match self.payloads.get(target) {
            Some(id) => Sequence { enabled: true, steps: vec![Step { payload_id: id.clone(), delay_ms: 0, process_name: String::new() }] },
            None => Sequence::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StepStatus {
    payload_id: String,
    delay_ms: u64,
    process_name: String,
    /// pending · waiting · sending · sent · verified · failed · skipped
    state: String,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Status {
    target: String,
    enabled: bool,
    steps: Vec<StepStatus>,
    /// off · ready · waiting · unavailable · sending · done · failed · stopped
    phase: String,
    message: String,
    current: Option<usize>,
    attempts: u8,
    last_attempt_at: Option<u64>,
    next_attempt_at: Option<u64>,
}

#[derive(Clone)]
struct Run {
    generation: u64,
    status: Status,
    stop_after_current: bool,
    endpoint: Option<String>,
    automatic: bool,
}

fn runs() -> &'static Mutex<BTreeMap<String, Run>> {
    static RUNS: OnceLock<Mutex<BTreeMap<String, Run>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 }
fn root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_config_dir().map(|p| p.join("payload-autostart.json")).map_err(|e| e.to_string())
}
fn read_config(app: &AppHandle) -> Result<Config, String> {
    match fs::read(root(app)?) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "Payload autostart settings are unreadable. Restore or remove payload-autostart.json in the app configuration folder.".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("Could not read payload autostart settings: {e}")),
    }
}

/// The configured order for this session, not yet scheduled.
fn idle(target: &str, sequence: &Sequence, generation: u64) -> Run {
    let on = sequence.enabled && !sequence.steps.is_empty();
    Run { generation, stop_after_current: false, endpoint: None, automatic: false, status: Status {
        target: target.into(), enabled: sequence.enabled,
        steps: sequence.steps.iter().map(|step| StepStatus { payload_id: step.payload_id.clone(), delay_ms: step.delay_ms, process_name: step.process_name.clone(), state: "pending".into(), message: String::new() }).collect(),
        phase: if on { "ready" } else { "off" }.into(),
        message: if on { "Ready for Run now or a confirmed console wake with a fresh process list." } else { "Autostart is off." }.into(),
        current: None, attempts: 0, last_attempt_at: None, next_attempt_at: None,
    } }
}

fn schedule(run: &mut Run, at: u64, grace: u64) {
    let status = &mut run.status;
    let Some(first) = status.steps.first_mut() else { return; };
    first.state = "waiting".into();
    status.next_attempt_at = Some(at + grace + first.delay_ms);
    status.phase = "waiting".into();
    status.message = "Starting soon.".into();
    status.current = Some(0);
}

fn armed(target: &str, sequence: &Sequence, generation: u64, at: u64) -> Run {
    let mut run = idle(target, sequence, generation);
    run.automatic = true;
    if run.status.phase == "ready" { schedule(&mut run, at, START_GRACE_MS); }
    run
}

/// Saving or enabling an order never sends anything.
fn replace(previous: Option<&Run>, target: &str, sequence: &Sequence) -> Run {
    let generation = previous.map_or(1, |run| run.generation + 1);
    idle(target, sequence, generation)
}

fn validate(steps: &[Step]) -> Result<(), String> {
    if steps.len() > MAX_STEPS { return Err(format!("Autostart sends up to {MAX_STEPS} payloads.")); }
    let mut seen = BTreeSet::new();
    for step in steps {
        if step.payload_id.is_empty() || !seen.insert(step.payload_id.as_str()) { return Err("Each payload can appear once in the autostart order.".into()); }
        if step.delay_ms > MAX_DELAY_MS { return Err(format!("Delays can be up to {} seconds.", MAX_DELAY_MS / 1000)); }
        if step.process_name.len() >= 40 || !step.process_name.bytes().all(|c| (0x20..0x7f).contains(&c)) || step.process_name.trim() != step.process_name {
            return Err("Use the exact process name from Running payloads (up to 39 printable characters).".into());
        }
    }
    Ok(())
}

#[tauri::command]
pub(super) fn get_payload_autostart() -> Vec<Status> {
    let map = runs().lock().unwrap_or_else(|p| p.into_inner());
    ["ps5", "ps4"].iter().map(|target| map.get(*target).cloned().unwrap_or_else(|| idle(target, &Sequence::default(), 0)).status).collect()
}

#[tauri::command]
pub(super) fn set_payload_autostart(app: AppHandle, target: String, enabled: bool, steps: Vec<Step>) -> Result<Vec<Status>, String> {
    if !matches!(target.as_str(), "ps4" | "ps5") { return Err("Choose PS4 or PS5.".into()); }
    validate(&steps)?;
    if !steps.is_empty() {
        let entries = payloads::list_payloads(app.clone())?;
        for step in &steps {
            let entry = entries.iter().find(|e| e.id == step.payload_id).ok_or("A payload in this order is no longer in the library.")?;
            if entry.target != "any" && entry.target != target { return Err(format!("{} belongs to the other console.", entry.name)); }
        }
    }
    // Serializes both read-modify-write and scheduling with the worker.
    let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
    let mut config = read_config(&app)?;
    for other in ["ps5", "ps4"] {
        if !config.sequences.contains_key(other) { let migrated = config.sequence(other); config.sequences.insert(other.into(), migrated); }
    }
    let sequence = Sequence { enabled, steps };
    config.sequences.insert(target.clone(), sequence.clone());
    let path = root(&app)?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?).map_err(|e| format!("Could not save autostart: {e}"))?;
    fs::rename(&temp, path).map_err(|e| format!("Could not save autostart: {e}"))?;
    let run = replace(map.get(&target), &target, &sequence);
    map.insert(target, run);
    drop(map);
    Ok(get_payload_autostart())
}

#[tauri::command]
pub(super) fn run_payload_autostart(target: String) -> Result<Vec<Status>, String> {
    {
        let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
        let run = map.get_mut(&target).ok_or("Autostart isn't set up for this console.")?;
        if !run.status.enabled || run.status.steps.is_empty() { return Err("Turn on autostart and add payloads first.".into()); }
        if matches!(run.status.phase.as_str(), "waiting" | "unavailable" | "sending") { return Err("Autostart is already running.".into()); }
        let steps = saved_steps(run);
        let mut next = idle(&target, &Sequence { enabled: true, steps }, run.generation + 1);
        schedule(&mut next, now(), 0);
        *run = next;
    }
    Ok(get_payload_autostart())
}

#[tauri::command]
pub(super) fn stop_payload_autostart(target: String) -> Vec<Status> {
    {
        let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(run) = map.get_mut(&target) { stop(run); }
    }
    get_payload_autostart()
}

fn stop(run: &mut Run) {
    match run.status.phase.as_str() {
        "waiting" | "unavailable" => {
            run.generation += 1;
            if let Some(index) = run.status.current { for step in &mut run.status.steps[index..] { step.state = "skipped".into(); } }
            run.status.phase = "stopped".into();
            run.status.message = STOPPED.into();
            run.status.current = None;
            run.status.next_attempt_at = None;
        }
        // The payload on the wire can't be recalled; nothing follows it.
        "sending" => { run.stop_after_current = true; run.status.message = "Stopping after the current payload…".into(); }
        _ => {}
    }
}

fn claim(map: &mut BTreeMap<String, Run>, target: &str, at: u64) -> Option<(u64, String)> {
    let run = map.get_mut(target)?;
    let status = &mut run.status;
    if !matches!(status.phase.as_str(), "waiting" | "unavailable") || status.attempts >= MAX_ATTEMPTS || status.next_attempt_at.is_none_or(|next| at < next) { return None; }
    let step = status.steps.get_mut(status.current?)?;
    step.state = "sending".into();
    step.message.clear();
    let id = step.payload_id.clone();
    status.phase = "sending".into();
    status.message = "Sending through the configured loader…".into();
    status.attempts += 1;
    status.last_attempt_at = Some(at);
    status.next_attempt_at = None;
    Some((run.generation, id))
}

fn complete(run: &mut Run, index: usize, verified: bool, message: String, at: u64) {
    let status = &mut run.status;
    status.steps[index].state = if verified { "verified" } else { "sent" }.into();
    status.steps[index].message = message;
    status.attempts = 0;
    let next = index + 1;
    if next < status.steps.len() && run.stop_after_current {
        for step in &mut status.steps[next..] { step.state = "skipped".into(); }
        status.phase = "stopped".into();
        status.message = STOPPED.into();
        status.current = None;
        status.next_attempt_at = None;
    } else if next < status.steps.len() {
        status.steps[next].state = "waiting".into();
        status.phase = "waiting".into();
        status.message = "Waiting before the next payload.".into();
        status.current = Some(next);
        status.next_attempt_at = Some(at + status.steps[next].delay_ms);
    } else {
        status.phase = "done".into();
        status.message = "Order complete. Already-running processes were kept running.".into();
        status.current = None;
        status.next_attempt_at = None;
    }
}

/// Stops the run at `index`; later payloads are skipped, never sent out of order.
fn halt(run: &mut Run, index: usize, message: String) {
    let status = &mut run.status;
    status.steps[index].state = "failed".into();
    status.steps[index].message = message.clone();
    for step in &mut status.steps[index + 1..] { step.state = "skipped".into(); }
    let skipped = index + 1 < status.steps.len();
    status.phase = if run.stop_after_current { "stopped" } else { "failed" }.into();
    status.message = if skipped { format!("{message} The remaining payloads were not sent.") } else { message };
    status.current = None;
    status.next_attempt_at = None;
}

fn loader_connect_failed(message: &str) -> bool {
    [CONNECT_FAILED, CONNECT_TIMEOUT].into_iter().any(|prefix| {
        message.strip_prefix(prefix).is_some_and(|detail| detail.is_empty() || detail.starts_with(' '))
    })
}

fn finish(map: &mut BTreeMap<String, Run>, target: &str, generation: u64, outcome: Result<(bool, String), String>, at: u64) {
    let Some(run) = map.get_mut(target).filter(|r| r.generation == generation) else { return; };
    let Some(index) = run.status.current else { return; };
    match outcome {
        Ok((verified, message)) => complete(run, index, verified, message, at),
        Err(message) => {
            // Retry only failures known to precede payload bytes. A partial write or
            // unverified execution must never result in another automatic send.
            let unavailable = loader_connect_failed(&message);
            let busy = message == SEND_BUSY;
            if busy { run.status.attempts = run.status.attempts.saturating_sub(1); }
            if (unavailable || busy) && run.status.attempts < MAX_ATTEMPTS && !run.stop_after_current {
                let status = &mut run.status;
                status.steps[index].state = "waiting".into();
                status.steps[index].message = message.clone();
                status.phase = "unavailable".into();
                status.message = message;
                status.next_attempt_at = Some(at + if busy { BUSY_RETRY_MS } else { RETRY_DELAYS[status.attempts as usize] });
            } else if unavailable && !run.stop_after_current {
                halt(run, index, format!("{message} Autostart stopped after {MAX_ATTEMPTS} attempts. Use Send when the loader is ready."));
            } else {
                halt(run, index, message);
            }
        }
    }
}

pub(super) fn record_manual(target: &str, id: &str, result: &Result<payloads::PayloadSendResult, String>) {
    if result.as_ref().is_err_and(|error| loader_connect_failed(error) || error == SEND_BUSY) { return; }
    let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
    let Some(run) = map.get_mut(target) else { return; };
    if !matches!(run.status.phase.as_str(), "waiting" | "unavailable") { return; }
    let Some(index) = run.status.current.filter(|&i| run.status.steps.get(i).is_some_and(|step| step.payload_id == id)) else { return; };
    // A manual send of the next payload takes its turn: success moves the order on, and an
    // uncertain write stops it so no later automatic send duplicates those bytes.
    run.generation += 1;
    let at = now();
    match result {
        Ok(result) => complete(run, index, result.verified, result.message.clone(), at),
        Err(error) => halt(run, index, error.clone()),
    }
    run.status.last_attempt_at = Some(at);
}

pub(super) fn start(app: AppHandle) {
    {
        let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
        match read_config(&app) {
            Ok(config) => for target in ["ps5", "ps4"] { map.insert(target.into(), idle(target, &config.sequence(target), 1)); },
            Err(error) => for target in ["ps5", "ps4"] {
                let mut run = idle(target, &Sequence::default(), 1);
                run.status.phase = "failed".into();
                run.status.message = error.clone();
                map.insert(target.into(), run);
            },
        }
    }
    watch_wakes(app.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            // Short ticks keep each payload's delay close to what the user set.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            for target in ["ps5", "ps4"] {
                let claimed = { claim(&mut runs().lock().unwrap_or_else(|p| p.into_inner()), target, now()) };
                let Some((generation, id)) = claimed else { continue; };
                let settings = app.state::<AppState>().settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let endpoint = endpoint(&settings, target);
                let (process_name, automatic) = {
                    let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
                    let Some(run) = map.get_mut(target).filter(|run| run.generation == generation) else { continue; };
                    if run.endpoint.as_ref().is_some_and(|old| old != &endpoint) {
                        let index = run.status.current.unwrap();
                        halt(run, index, "Console address changed. Use Run now after checking the new console.".into());
                        continue;
                    }
                    run.endpoint = Some(endpoint);
                    (run.status.steps[run.status.current.unwrap()].process_name.clone(), run.automatic)
                };
                let outcome = payloads::send_autostart(&app, &settings, &id, target, &process_name, generation, automatic).await;
                finish(&mut runs().lock().unwrap_or_else(|p| p.into_inner()), target, generation, outcome, now());
            }
        }
    });
}

fn saved_steps(run: &Run) -> Vec<Step> {
    run.status.steps.iter().map(|step| Step { payload_id: step.payload_id.clone(), delay_ms: step.delay_ms, process_name: step.process_name.clone() }).collect()
}

fn endpoint(settings: &crate::Settings, target: &str) -> String {
    if target == "ps4" { format!("{}:{}:{}",settings.ps4_host,settings.ps4_receiver_port,settings.ps4_loader_port) }
    else { format!("{}:{}:{}",settings.ps5_host,settings.ps5_port,settings.ps5_loader_port) }
}

pub(super) fn is_current(app: &AppHandle, target: &str, generation: u64) -> bool {
    let settings = app.state::<AppState>().settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let key = endpoint(&settings,target);
    runs().lock().unwrap_or_else(|p| p.into_inner()).get(target).is_some_and(|run| run.generation == generation && !run.stop_after_current && run.endpoint.as_deref() == Some(key.as_str()))
}

fn watch_wakes(app: AppHandle) {
    for target in ["ps5", "ps4"] {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut watch = crate::payload_wake::Watch::default();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let run = runs().lock().unwrap_or_else(|p| p.into_inner()).get(target).cloned();
                let Some(run) = run.filter(|r| r.status.enabled && !r.status.steps.is_empty()) else { watch = Default::default(); continue; };
                let settings = app.state::<AppState>().settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let key = endpoint(&settings,target);
                let (host,port) = if target == "ps4" { (&settings.ps4_host,settings.ps4_receiver_port) } else { (&settings.ps5_host,settings.ps5_port) };
                let power = crate::payload_wake::observe(target,host).await;
                if !watch.update(&format!("{key}:{}",run.generation),power) {
                    if !watch.pending() && run.status.message.starts_with("Console woke;") {
                        if let Some(current) = runs().lock().unwrap_or_else(|p| p.into_inner()).get_mut(target).filter(|r| r.generation==run.generation) {
                            current.status.message = "Wake check ended without a known process state. Use Run now when ready.".into();
                        }
                    }
                    continue;
                }
                if matches!(run.status.phase.as_str(),"waiting"|"unavailable"|"sending") { watch.consume(); continue; }
                if let Err(error) = crate::console_diagnostics::running_process_names(target,host,port).await {
                    if let Some(current) = runs().lock().unwrap_or_else(|p| p.into_inner()).get_mut(target).filter(|r| r.generation==run.generation) {
                        current.status.message = format!("Console woke; waiting for a complete process list. {error}");
                    }
                    continue;
                }
                let current_settings = app.state::<AppState>().settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if endpoint(&current_settings,target) != key { continue; }
                let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
                if let Some(current) = map.get_mut(target).filter(|r| r.generation==run.generation && !matches!(r.status.phase.as_str(),"waiting"|"unavailable"|"sending")) {
                    *current = armed(target,&Sequence { enabled:true, steps:saved_steps(&run) },run.generation+1,now());
                    current.endpoint = Some(key);
                    current.status.message = "Console wake confirmed; checking each payload before starting it.".into();
                }
                watch.consume();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn steps(list: &[(&str, u64)]) -> Vec<Step> { list.iter().map(|(id, delay)| Step { payload_id: (*id).into(), delay_ms: *delay, process_name: String::new() }).collect() }
    fn sequence(list: &[(&str, u64)]) -> Sequence { Sequence { enabled: true, steps: steps(list) } }
    fn setup(list: &[(&str, u64)]) -> BTreeMap<String, Run> { BTreeMap::from([("ps5".into(), armed("ps5", &sequence(list), 1, 0))]) }
    fn states(map: &BTreeMap<String, Run>) -> Vec<&str> { map["ps5"].status.steps.iter().map(|step| step.state.as_str()).collect() }

    #[test]
    fn retry_is_throttled_and_stops_after_three_connection_failures() {
        let mut map = setup(&[("payload", 0), ("later", 0)]);
        assert!(claim(&mut map, "ps5", START_GRACE_MS - 1).is_none());
        let mut at = START_GRACE_MS;
        for next in [Some(START_GRACE_MS + 15_000), Some(START_GRACE_MS + 60_000), None] {
            assert!(claim(&mut map, "ps5", at).is_some());
            assert!(claim(&mut map, "ps5", at).is_none());
            finish(&mut map, "ps5", 1, Err(CONNECT_FAILED.into()), at);
            assert_eq!(map["ps5"].status.next_attempt_at, next);
            at = next.unwrap_or(at);
        }
        assert_eq!(map["ps5"].status.phase, "failed");
        assert_eq!(states(&map), ["failed", "skipped"]);
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }

    #[test]
    fn connection_diagnostics_keep_retries_without_retrying_send_failures() {
        for prefix in [CONNECT_FAILED, CONNECT_TIMEOUT] {
            for detail in [" Diagnostic log: C:\\SSPI\\logs\\SSPI.log", " Windows socket error 10061: connection refused. Diagnostic log: C:\\SSPI\\logs\\SSPI.log"] {
                let message = format!("{prefix}{detail}");
                let mut map = setup(&[("payload", 0), ("later", 0)]);
                claim(&mut map, "ps5", START_GRACE_MS);
                finish(&mut map, "ps5", 1, Err(message.clone()), START_GRACE_MS);
                assert_eq!(map["ps5"].status.phase, "unavailable");
                assert_eq!(map["ps5"].status.message, message);
                assert_eq!(map["ps5"].status.next_attempt_at, Some(START_GRACE_MS + RETRY_DELAYS[1]));
                assert_eq!(states(&map), ["waiting", "pending"]);
            }
        }
        for message in [
            format!("Could not send the complete payload. {CONNECT_FAILED}"),
            format!("{CONNECT_FAILED}Extra text without a separator"),
        ] {
            let mut map = setup(&[("payload", 0), ("later", 0)]);
            claim(&mut map, "ps5", START_GRACE_MS);
            finish(&mut map, "ps5", 1, Err(message), START_GRACE_MS);
            assert_eq!(map["ps5"].status.phase, "failed");
            assert!(claim(&mut map, "ps5", u64::MAX).is_none());
        }
    }

    #[test]
    fn payloads_go_in_order_after_their_own_delays() {
        let mut map = setup(&[("a", 0), ("b", 2_000), ("c", 500)]);
        assert_eq!(claim(&mut map, "ps5", START_GRACE_MS).map(|c| c.1), Some("a".into()));
        finish(&mut map, "ps5", 1, Ok((true, "Verified".into())), 6_000);
        assert!(claim(&mut map, "ps5", 7_999).is_none());
        assert_eq!(claim(&mut map, "ps5", 8_000).map(|c| c.1), Some("b".into()));
        finish(&mut map, "ps5", 1, Ok((false, "Sent".into())), 8_100);
        assert!(claim(&mut map, "ps5", 8_599).is_none());
        assert_eq!(claim(&mut map, "ps5", 8_600).map(|c| c.1), Some("c".into()));
        finish(&mut map, "ps5", 1, Ok((false, "Sent".into())), 8_700);
        assert_eq!(map["ps5"].status.phase, "done");
        assert_eq!(states(&map), ["verified", "sent", "sent"]);
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }

    #[test]
    fn a_sent_payload_or_partial_send_is_never_repeated() {
        for outcome in [Ok((false, "Sent".into())), Ok((true, "Verified".into())), Err("Could not send the complete payload.".into())] {
            let mut map = setup(&[("payload", 0)]);
            claim(&mut map, "ps5", START_GRACE_MS);
            finish(&mut map, "ps5", 1, outcome, START_GRACE_MS);
            assert!(claim(&mut map, "ps5", u64::MAX).is_none());
        }
    }

    #[test]
    fn a_failed_payload_skips_the_rest_of_the_order() {
        let mut map = setup(&[("a", 0), ("b", 0), ("c", 0)]);
        claim(&mut map, "ps5", START_GRACE_MS);
        finish(&mut map, "ps5", 1, Ok((false, "Sent".into())), START_GRACE_MS);
        claim(&mut map, "ps5", START_GRACE_MS);
        finish(&mut map, "ps5", 1, Err("Could not send the complete payload.".into()), START_GRACE_MS);
        assert_eq!(states(&map), ["sent", "failed", "skipped"]);
        assert_eq!(map["ps5"].status.phase, "failed");
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }

    #[test]
    fn a_busy_sender_retries_without_using_attempts() {
        let mut map = setup(&[("a", 0)]);
        for at in [START_GRACE_MS, START_GRACE_MS + BUSY_RETRY_MS, START_GRACE_MS + 2 * BUSY_RETRY_MS, START_GRACE_MS + 3 * BUSY_RETRY_MS] {
            assert!(claim(&mut map, "ps5", at).is_some());
            finish(&mut map, "ps5", 1, Err(SEND_BUSY.into()), at);
            assert_eq!(map["ps5"].status.attempts, 0);
        }
        assert_eq!(map["ps5"].status.phase, "unavailable");
    }

    #[test]
    fn config_change_invalidates_inflight_result_and_disabled_is_inert() {
        let mut map = setup(&[("payload", 0)]);
        claim(&mut map, "ps5", START_GRACE_MS);
        map.insert("ps5".into(), armed("ps5", &Sequence::default(), 2, 0));
        finish(&mut map, "ps5", 1, Ok((true, "Verified".into())), 20);
        assert_eq!(map["ps5"].status.phase, "off");
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }

    #[test]
    fn startup_and_saving_never_schedule_a_send() {
        let mut startup = BTreeMap::from([("ps5".into(), idle("ps5",&sequence(&[("a",0)]),1))]);
        assert!(claim(&mut startup,"ps5",u64::MAX).is_none());
        let mut map = setup(&[("a", 0), ("b", 0)]);
        let untouched = replace(map.get("ps5"), "ps5", &sequence(&[("b", 0), ("a", 0)]));
        assert_eq!(untouched.status.phase, "ready");
        claim(&mut map, "ps5", START_GRACE_MS);
        finish(&mut map, "ps5", 1, Ok((false, "Sent".into())), START_GRACE_MS);
        let later = replace(map.get("ps5"), "ps5", &sequence(&[("b", 0), ("a", 0)]));
        assert_eq!(later.status.phase, "ready");
        assert_eq!(later.generation, 2);
        map.insert("ps5".into(), later);
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }

    #[test]
    fn stopping_skips_what_has_not_been_sent() {
        let mut map = setup(&[("a", 0), ("b", 0)]);
        claim(&mut map, "ps5", START_GRACE_MS);
        stop(map.get_mut("ps5").unwrap());
        assert_eq!(map["ps5"].status.phase, "sending");
        finish(&mut map, "ps5", 1, Ok((false, "Sent".into())), START_GRACE_MS);
        assert_eq!(map["ps5"].status.phase, "stopped");
        assert_eq!(states(&map), ["sent", "skipped"]);

        let mut waiting = setup(&[("a", 0), ("b", 0)]);
        stop(waiting.get_mut("ps5").unwrap());
        assert_eq!(states(&waiting), ["skipped", "skipped"]);
        assert!(claim(&mut waiting, "ps5", u64::MAX).is_none());
    }

    #[test]
    fn the_order_is_validated() {
        assert!(validate(&steps(&[("a", 0), ("b", MAX_DELAY_MS)])).is_ok());
        assert!(validate(&steps(&[("a", 0), ("a", 0)])).is_err());
        assert!(validate(&steps(&[("a", MAX_DELAY_MS + 1)])).is_err());
        let many: Vec<(String, u64)> = (0..=MAX_STEPS).map(|n| (n.to_string(), 0)).collect();
        assert!(validate(&many.iter().map(|(id, delay)| Step { payload_id: id.clone(), delay_ms: *delay, process_name: String::new() }).collect::<Vec<_>>()).is_err());
    }

    #[test]
    fn single_payload_settings_become_a_one_step_order() {
        let config: Config = serde_json::from_str(r#"{"payloads":{"ps5":"kstuff"}}"#).unwrap();
        let sequence = config.sequence("ps5");
        assert!(sequence.enabled);
        assert_eq!(sequence.steps, steps(&[("kstuff", 0)]));
        assert!(!config.sequence("ps4").enabled);
        let saved = serde_json::to_string(&Config { payloads: config.payloads.clone(), sequences: BTreeMap::from([("ps5".into(), sequence)]) }).unwrap();
        assert!(!saved.contains("\"payloads\""));
    }

    #[test]
    fn manual_sends_take_the_current_turn() {
        runs().lock().unwrap().insert("ps5".into(), armed("ps5", &sequence(&[("first", 0), ("second", 1_000)]), 12, 0));
        record_manual("ps5", "other", &Err("Could not send the complete payload.".into()));
        record_manual("ps5", "first", &Err(SEND_BUSY.into()));
        for prefix in [CONNECT_FAILED, CONNECT_TIMEOUT] {
            record_manual("ps5", "first", &Err(format!("{prefix} Windows socket error 10061. Diagnostic log: C:\\SSPI\\logs\\SSPI.log")));
            let map = runs().lock().unwrap();
            assert_eq!(map["ps5"].generation, 12);
            assert_eq!(map["ps5"].status.phase, "waiting");
        }
        assert_eq!(runs().lock().unwrap()["ps5"].status.current, Some(0));
        record_manual("ps5", "first", &Err("Could not send the complete payload.".into()));
        let mut map = runs().lock().unwrap();
        assert_eq!(map["ps5"].status.phase, "failed");
        assert_eq!(states(&map), ["failed", "skipped"]);
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
        map.remove("ps5");
    }
}
