//! Opt-in, bounded payload delivery through the normal library sender.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::{Mutex, OnceLock}, time::{SystemTime, UNIX_EPOCH}};
use tauri::{AppHandle, Manager};

use crate::{payloads, AppState};

const MAX_ATTEMPTS: u8 = 3;
const DELAYS: [u64; 3] = [0, 15_000, 45_000];

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    payloads: BTreeMap<String, String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Status {
    target: String,
    payload_id: Option<String>,
    phase: String,
    message: String,
    attempts: u8,
    last_attempt_at: Option<u64>,
    next_attempt_at: Option<u64>,
}

#[derive(Clone)]
struct Run {
    generation: u64,
    status: Status,
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

fn armed(target: &str, id: Option<String>, generation: u64) -> Run {
    let enabled = id.is_some();
    Run { generation, status: Status {
        target: target.into(), payload_id: id,
        phase: if enabled { "waiting" } else { "off" }.into(),
        message: if enabled { "Waiting to send through the configured loader." } else { "Autostart is off." }.into(),
        attempts: 0, last_attempt_at: None, next_attempt_at: enabled.then(|| now() + 5_000),
    } }
}

#[tauri::command]
pub(super) fn get_payload_autostart() -> Vec<Status> {
    let map = runs().lock().unwrap_or_else(|p| p.into_inner());
    ["ps5", "ps4"].iter().map(|target| map.get(*target).cloned().unwrap_or_else(|| armed(target, None, 0)).status).collect()
}

#[tauri::command]
pub(super) fn set_payload_autostart(app: AppHandle, target: String, payload_id: Option<String>) -> Result<Vec<Status>, String> {
    if !matches!(target.as_str(), "ps4" | "ps5") { return Err("Choose PS4 or PS5.".into()); }
    let payload_id = payload_id.filter(|s| !s.is_empty());
    if let Some(id) = &payload_id {
        let entries = payloads::list_payloads(app.clone())?;
        let entry = entries.iter().find(|e| &e.id == id).ok_or("That payload is no longer in the library.")?;
        if entry.target != "any" && entry.target != target { return Err("That payload belongs to the other console.".into()); }
    }
    // Serializes both read-modify-write and scheduling with the worker.
    let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
    let mut config = read_config(&app)?;
    match &payload_id { Some(id) => { config.payloads.insert(target.clone(), id.clone()); }, None => { config.payloads.remove(&target); } }
    let path = root(&app)?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?).map_err(|e| format!("Could not save autostart: {e}"))?;
    fs::rename(&temp, path).map_err(|e| format!("Could not save autostart: {e}"))?;
    let generation = map.get(&target).map_or(1, |r| r.generation + 1);
    map.insert(target.clone(), armed(&target, payload_id, generation));
    drop(map);
    Ok(get_payload_autostart())
}

fn claim(map: &mut BTreeMap<String, Run>, target: &str, at: u64) -> Option<Run> {
    let run = map.get_mut(target)?;
    if !matches!(run.status.phase.as_str(), "waiting" | "unavailable") || run.status.attempts >= MAX_ATTEMPTS || run.status.next_attempt_at.is_none_or(|next| at < next) { return None; }
    run.status.phase = "sending".into();
    run.status.message = "Sending through the configured loader…".into();
    run.status.attempts += 1;
    run.status.last_attempt_at = Some(at);
    run.status.next_attempt_at = None;
    Some(run.clone())
}

fn finish(map: &mut BTreeMap<String, Run>, target: &str, generation: u64, outcome: Result<(bool, String), String>, at: u64) {
    let Some(run) = map.get_mut(target).filter(|r| r.generation == generation) else { return; };
    match outcome {
        Ok((verified, message)) => {
            run.status.phase = if verified { "verified" } else { "sent" }.into();
            run.status.message = message;
        }
        Err(message) => {
            // Retry only failures known to precede payload bytes. A partial write or
            // unverified execution must never result in another automatic send.
            let unavailable = message == "Could not connect to the payload loader." || message == "The payload loader connection timed out.";
            let busy = message == "Another payload is being sent. Wait for it to finish.";
            if busy { run.status.attempts = run.status.attempts.saturating_sub(1); }
            run.status.phase = if unavailable || busy { "unavailable" } else { "failed" }.into();
            run.status.message = message;
            if (unavailable || busy) && run.status.attempts < MAX_ATTEMPTS {
                run.status.next_attempt_at = Some(at + if busy { 5_000 } else { DELAYS[run.status.attempts as usize] });
            } else if unavailable {
                run.status.message.push_str(" Autostart stopped after 3 attempts. Use Send when the loader is ready.");
            }
        }
    }
}

pub(super) fn record_manual(target: &str, id: &str, result: &Result<payloads::PayloadSendResult, String>) {
    if result.as_ref().is_err_and(|error| error == "Could not connect to the payload loader." || error == "The payload loader connection timed out.") { return; }
    let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(run) = map.get_mut(target).filter(|r| r.status.payload_id.as_deref() == Some(id)) {
        // Manual success satisfies pending autostart; an uncertain manual write
        // also cancels it so no later automatic send duplicates those bytes.
        run.generation += 1;
        match result {
            Ok(result) => { run.status.phase = if result.verified { "verified" } else { "sent" }.into(); run.status.message = result.message.clone(); }
            Err(error) => { run.status.phase = "failed".into(); run.status.message = error.clone(); }
        }
        run.status.next_attempt_at = None;
        run.status.last_attempt_at = Some(now());
    }
}

pub(super) fn start(app: AppHandle) {
    {
        let mut map = runs().lock().unwrap_or_else(|p| p.into_inner());
        match read_config(&app) {
            Ok(config) => for target in ["ps5", "ps4"] { map.insert(target.into(), armed(target, config.payloads.get(target).cloned(), 1)); },
            Err(error) => for target in ["ps5", "ps4"] { let mut run = armed(target, None, 1); run.status.phase = "failed".into(); run.status.message = error.clone(); map.insert(target.into(), run); },
        }
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            for target in ["ps5", "ps4"] {
                let run = { claim(&mut runs().lock().unwrap_or_else(|p| p.into_inner()), target, now()) };
                let Some(run) = run else { continue; };
                let settings = app.state::<AppState>().settings.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let outcome = payloads::send_configured(&app, &settings, run.status.payload_id.as_deref().unwrap_or_default(), target).await
                    .map(|result| (result.verified, result.message));
                finish(&mut runs().lock().unwrap_or_else(|p| p.into_inner()), target, run.generation, outcome, now());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> BTreeMap<String, Run> { let mut r = armed("ps5", Some("payload".into()), 1); r.status.next_attempt_at = Some(10); BTreeMap::from([("ps5".into(), r)]) }
    #[test]
    fn retry_is_throttled_and_stops_after_three_connection_failures() {
        let mut map = setup();
        assert!(claim(&mut map, "ps5", 9).is_none());
        for (at, next) in [(10, Some(15_010)), (15_010, Some(60_010)), (60_010, None)] {
            assert!(claim(&mut map, "ps5", at).is_some());
            assert!(claim(&mut map, "ps5", at).is_none());
            finish(&mut map, "ps5", 1, Err("Could not connect to the payload loader.".into()), at);
            assert_eq!(map["ps5"].status.next_attempt_at, next);
        }
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }
    #[test]
    fn a_sent_payload_or_partial_send_is_never_repeated() {
        for outcome in [Ok((false, "Sent".into())), Ok((true, "Verified".into())), Err("Could not send the complete payload.".into())] {
            let mut map = setup(); claim(&mut map, "ps5", 10);
            finish(&mut map, "ps5", 1, outcome, 10);
            assert!(claim(&mut map, "ps5", u64::MAX).is_none());
        }
    }
    #[test]
    fn config_change_invalidates_inflight_result_and_disabled_is_inert() {
        let mut map = setup(); claim(&mut map, "ps5", 10);
        map.insert("ps5".into(), armed("ps5", None, 2));
        finish(&mut map, "ps5", 1, Ok((true, "Verified".into())), 20);
        assert_eq!(map["ps5"].status.phase, "off");
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
    }
    #[test]
    fn uncertain_manual_send_cancels_pending_autostart() {
        runs().lock().unwrap().insert("ps5".into(), armed("ps5", Some("selected".into()), 12));
        record_manual("ps5", "selected", &Err("Could not send the complete payload.".into()));
        let mut map = runs().lock().unwrap();
        assert_eq!(map["ps5"].status.phase, "failed");
        assert!(claim(&mut map, "ps5", u64::MAX).is_none());
        map.remove("ps5");
    }
}
