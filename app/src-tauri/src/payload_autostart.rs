//! Opt-in payload autostart: the chosen payloads go to the console's loader in their saved order,
//! each after its own delay, once per SSPI launch or when asked to run now. Bounded, never repeated.
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
            Some(id) => Sequence { enabled: true, steps: vec![Step { payload_id: id.clone(), delay_ms: 0 }] },
            None => Sequence::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StepStatus {
    payload_id: String,
    delay_ms: u64,
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
    Run { generation, stop_after_current: false, status: Status {
        target: target.into(), enabled: sequence.enabled,
        steps: sequence.steps.iter().map(|step| StepStatus { payload_id: step.payload_id.clone(), delay_ms: step.delay_ms, state: "pending".into(), message: String::new() }).collect(),
        phase: if on { "ready" } else { "off" }.into(),
        message: if on { "Runs the next time SSPI starts." } else { "Autostart is off." }.into(),
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
    if run.status.phase == "ready" { schedule(&mut run, at, START_GRACE_MS); }
    run
}

/// Whether this session's run already put bytes on the wire (or may have).
fn touched(status: &Status) -> bool {
    status.steps.iter().any(|step| matches!(step.state.as_str(), "sending" | "sent" | "verified" | "failed"))
}

/// A saved order replaces an untouched run and schedules it; after a send it waits for the next launch.
fn replace(previous: Option<&Run>, target: &str, sequence: &Sequence, at: u64) -> Run {
    let generation = previous.map_or(1, |run| run.generation + 1);
    match previous {
        Some(run) if touched(&run.status) => idle(target, sequence, generation),
        _ => armed(target, sequence, generation, at),
    }
}

fn validate(steps: &[Step]) -> Result<(), String> {
    if steps.len() > MAX_STEPS { return Err(format!("Autostart sends up to {MAX_STEPS} payloads.")); }
    let mut seen = BTreeSet::new();
    for step in steps {
        if step.payload_id.is_empty() || !seen.insert(step.payload_id.as_str()) { return Err("Each payload can appear once in the autostart order.".into()); }
        if step.delay_ms > MAX_DELAY_MS { return Err(format!("Delays can be up to {} seconds.", MAX_DELAY_MS / 1000)); }
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
    let run = replace(map.get(&target), &target, &sequence, now());
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
        let steps = run.status.steps.iter().map(|step| Step { payload_id: step.payload_id.clone(), delay_ms: step.delay_ms }).collect();
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
        let sent = status.steps.len();
        status.phase = "done".into();
        status.message = format!("Sent {sent} payload{}.", if sent == 1 { "" } else { "s" });
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

fn finish(map: &mut BTreeMap<String, Run>, target: &str, generation: u64, outcome: Result<(bool, String), String>, at: u64) {
    let Some(run) = map.get_mut(target).filter(|r| r.generation == generation) else { return; };
    let Some(index) = run.status.current else { return; };
    match outcome {
        Ok((verified, message)) => complete(run, index, verified, message, at),
        Err(message) => {
            // Retry only failures known to precede payload bytes. A partial write or
            // unverified execution must never result in another automatic send.
            let unavailable = message == CONNECT_FAILED || message == CONNECT_TIMEOUT;
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
    if result.as_ref().is_err_and(|error| error == CONNECT_FAILED || error == CONNECT_TIMEOUT || error == SEND_BUSY) { return; }
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
        let at = now();
        match read_config(&app) {
            Ok(config) => for target in ["ps5", "ps4"] { map.insert(target.into(), armed(target, &config.sequence(target), 1, at)); },
            Err(error) => for target in ["ps5", "ps4"] {
                let mut run = idle(target, &Sequence::default(), 1);
                run.status.phase = "failed".into();
                run.status.message = error.clone();
                map.insert(target.into(), run);
            },
        }
    }
    tauri::async_runtime::spawn(async move {
        loop {
            // Short ticks keep each payload's delay close to what the user set.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            for target in ["ps5", "ps4"] {
                let claimed = { claim(&mut runs().lock().unwrap_or_else(|p| p.into_inner()), target, now()) };
                let Some((generation, id)) = claimed else { continue; };
                let settings = app.state::<AppState>().settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let outcome = payloads::send_configured(&app, &settings, &id, target).await.map(|result| (result.verified, result.message));
                finish(&mut runs().lock().unwrap_or_else(|p| p.into_inner()), target, generation, outcome, now());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn steps(list: &[(&str, u64)]) -> Vec<Step> { list.iter().map(|(id, delay)| Step { payload_id: (*id).into(), delay_ms: *delay }).collect() }
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
    fn saving_after_a_send_waits_for_the_next_launch() {
        let mut map = setup(&[("a", 0), ("b", 0)]);
        let untouched = replace(map.get("ps5"), "ps5", &sequence(&[("b", 0), ("a", 0)]), 100);
        assert_eq!(untouched.status.phase, "waiting");
        claim(&mut map, "ps5", START_GRACE_MS);
        finish(&mut map, "ps5", 1, Ok((false, "Sent".into())), START_GRACE_MS);
        let later = replace(map.get("ps5"), "ps5", &sequence(&[("b", 0), ("a", 0)]), 100);
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
        assert!(validate(&many.iter().map(|(id, delay)| Step { payload_id: id.clone(), delay_ms: *delay }).collect::<Vec<_>>()).is_err());
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
        assert_eq!(runs().lock().unwrap()["ps5"].status.current, Some(0));
        record_manual("ps5", "first", &Err("Could not send the complete payload.".into()));
        let mut map = runs().lock().unwrap();
        assert_eq!(map["ps5"].status.phase, "failed");
        assert_eq!(states(&map), ["failed", "skipped"]);
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
        map.remove("ps5");
    }
}
