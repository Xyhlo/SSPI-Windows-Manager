//! ShadowMount Plus exFAT images built from a staged PS5 dump, with optional Lizard (AMPR/LZ4)
//! asset packing. Everything runs on the PC; the console only receives the finished image.

use crate::{ampr_index, ampr_pack, exfat};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const RUNTIME_NAME: &str = "libSceAmpr.sprx";
/// The pack-capable ampr_emu runtime this build ships (0.4.2.1 test-pack).
pub(crate) const RUNTIME_VERSION: &str = "0.4.2.1";

/// Cancellation stays the bare `cancelled` the job runner recognises.
fn context(prefix: &str, error: String) -> String { if error == "cancelled" { error } else { format!("{prefix}: {error}") } }
fn gib(bytes: u64) -> String { format!("{:.2} GiB", bytes as f64 / 1_073_741_824.) }

/// The pack-capable `libSceAmpr.sprx` shipped under `resources/ampr`.
pub(crate) fn ampr_runtime() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("SSPI_AMPR_RUNTIME") {
        let path = PathBuf::from(value);
        if path.is_file() { return Some(path); }
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        let path = dir.join("resources").join("ampr").join(RUNTIME_NAME);
        if path.is_file() { return Some(path); }
    }
    #[cfg(debug_assertions)]
    {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../SDK/ampr").join(RUNTIME_NAME);
        if path.is_file() { return Some(path); }
    }
    None
}

/// `PPSA12345-v01.000.000.exfat`, or `PPSA12345.exfat` when param.json has no usable version.
pub(crate) fn image_name(root: &Path, title_id: &str) -> String {
    let version = fs::read_to_string(root.join("sce_sys").join("param.json")).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get("contentVersion").and_then(Value::as_str).map(str::to_string))
        .filter(|v| !v.is_empty() && v.len() <= 16 && v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    match version {
        Some(version) => format!("{title_id}-v{version}.exfat"),
        None => format!("{title_id}.exfat"),
    }
}

pub(crate) struct Request<'a> {
    /// Disposable staging tree; Lizard packing changes it in place.
    pub(crate) staged: &'a Path,
    pub(crate) output_dir: &'a Path,
    pub(crate) file_name: &'a str,
    pub(crate) label: &'a str,
    pub(crate) lizard: bool,
    /// Pack-capable `libSceAmpr.sprx` ([`ampr_runtime`]); required when Lizard runs.
    pub(crate) runtime: Option<&'a Path>,
    pub(crate) workers: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Built {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) payload_bytes: u64,
    pub(crate) files: u64,
    pub(crate) lizard: Option<Value>,
    pub(crate) warnings: Vec<String>,
    pub(crate) engine: Value,
}

/// Stage timings and measured I/O in the packaging engine's `stages-io-v1` shape, so the
/// Downloads panel draws the image build with the same strip as an FPKG build.
struct Telemetry {
    stages: Vec<(&'static str, &'static str)>,
    seconds: Vec<f64>,
    details: Vec<Option<String>>,
    active: Option<usize>,
    started: Instant,
    stage_started: Instant,
    read: u64,
    write: u64,
    sample: (Instant, u64, u64),
    rates: (Option<f64>, Option<f64>),
    input_bytes: u64,
    workers: usize,
}

impl Telemetry {
    fn new(lizard: bool, workers: usize) -> Self {
        let mut stages = Vec::new();
        if lizard { stages.push(("lizard", "Lizard asset packing")); }
        stages.extend([("layout", "Plan the exFAT layout"), ("write", "Write the exFAT image"), ("verify", "Verify the image")]);
        let now = Instant::now();
        Self { seconds: vec![0.; stages.len()], details: vec![None; stages.len()], stages, active: None, started: now, stage_started: now,
               read: 0, write: 0, sample: (now, 0, 0), rates: (None, None), input_bytes: 0, workers }
    }
    fn finish_active(&mut self) {
        if let Some(i) = self.active.take() { self.seconds[i] = self.stage_started.elapsed().as_secs_f64(); }
    }
    fn begin(&mut self, id: &str) {
        self.finish_active();
        self.active = self.stages.iter().position(|(stage, _)| *stage == id);
        self.stage_started = Instant::now();
        self.sample = (self.stage_started, self.read, self.write);
        self.rates = (None, None);
    }
    fn detail(&mut self, id: &str, text: String) {
        if let Some(i) = self.stages.iter().position(|(stage, _)| *stage == id) { self.details[i] = Some(text); }
    }
    fn measure(&mut self) {
        let dt = self.sample.0.elapsed().as_secs_f64();
        if dt >= 1.0 {
            let rate = |now: u64, then: u64| (now > then).then(|| (now - then) as f64 / dt);
            self.rates = (rate(self.read, self.sample.1), rate(self.write, self.sample.2));
            self.sample = (Instant::now(), self.read, self.write);
        }
    }
    fn snapshot(&mut self, progress: Option<f64>, files: Option<(u64, u64)>, current: &str) -> Value {
        self.measure();
        let active = self.active;
        let stage_seconds = self.stage_started.elapsed().as_secs_f64();
        let stages: Vec<Value> = self.stages.iter().enumerate().map(|(i, (id, label))| {
            let state = match active { Some(a) if a == i => "active", Some(a) if i < a => "done", None if self.seconds[i] > 0. => "done", _ => "pending" };
            let seconds = if state == "active" { stage_seconds } else { self.seconds[i] };
            let mut stage = json!({ "id": id, "label": label, "state": state, "seconds": seconds, "detail": self.details[i] });
            if state == "active" { stage["progress"] = json!(progress); }
            stage
        }).collect();
        let eta = progress.filter(|p| *p > 0.02 && *p < 1.).map(|p| stage_seconds * (1. - p) / p);
        json!({
            "stage": active.map(|i| self.stages[i].0).unwrap_or("done"),
            "stageIndex": active.map(|i| i + 1).unwrap_or(self.stages.len()),
            "stageCount": self.stages.len(),
            "stageProgress": progress,
            "stages": stages,
            "io": { "readBytes": self.read, "writeBytes": self.write, "readBps": self.rates.0, "writeBps": self.rates.1 },
            "inputBytes": self.input_bytes,
            "compressedBytes": null,
            "files": files.map(|(done, total)| json!({ "done": done, "total": total })),
            "currentFile": (!current.is_empty()).then_some(current),
            "currentFileProgress": null,
            "etaSeconds": eta,
            "level": 0, "workers": self.workers, "pfs": "exFAT", "blockKiB": exfat::CLUSTER / 1024,
            "elapsedSeconds": self.started.elapsed().as_secs_f64(),
        })
    }
}

/// Replaces the dump's AMPR runtime with the pack-capable build. The staged copy may be a hard
/// link to the original dump, so the old file is unlinked, never overwritten.
fn install_runtime(root: &Path, runtime: &Path) -> Result<PathBuf, String> {
    let existing = ["fakelib", "fakelib2"].iter().map(|dir| root.join(dir).join(RUNTIME_NAME)).find(|path| path.is_file());
    let target = existing.clone().unwrap_or_else(|| root.join("fakelib").join(RUNTIME_NAME));
    if let Some(old) = &existing { fs::remove_file(old).map_err(|e| format!("Cannot replace {}: {e}", old.display()))?; }
    fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::copy(runtime, &target).map_err(|e| format!("Cannot install the Lizard runtime: {e}"))?;
    Ok(target)
}

/// Builds `<output_dir>/<file_name>` from the staged dump: optional Lizard packing, then the
/// exFAT image, then a full read-back check. `emit` receives telemetry snapshots and a message.
pub(crate) fn build(request: &Request, cancel: &dyn Fn() -> Result<(), String>,
                    emit: &mut dyn FnMut(Value, String)) -> Result<Built, String> {
    let root = request.staged;
    let mut warnings = Vec::new();
    let lizard_possible = request.lizard && root.join("ampr_emu.index").is_file();
    if request.lizard && !lizard_possible {
        warnings.push("Lizard packing was skipped: this dump has no ampr_emu.index. Lizard needs a backport that uses the AMPR emulator. The image holds the game's files unpacked.".into());
    }
    let mut meter = Telemetry::new(lizard_possible, request.workers);
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut due = move || { let ready = last.elapsed() >= Duration::from_millis(250); if ready { last = Instant::now(); } ready };
    fs::create_dir_all(request.output_dir).map_err(|e| format!("Cannot create {}: {e}", request.output_dir.display()))?;
    let output = request.output_dir.join(request.file_name);
    // Checked before any work: a finished image is never replaced.
    if output.exists() { return Err(format!("{} already exists. Remove it or rename it, then retry.", output.display())); }
    let stem = request.file_name.trim_end_matches(".exfat");
    let crc = request.output_dir.join(format!("{stem}.ampr_assets.index.crc"));
    let result = (|| {
        let mut lizard = None;
        if lizard_possible {
            meter.begin("lizard");
            let runtime = request.runtime.ok_or("The Lizard runtime (resources/ampr/libSceAmpr.sprx) is missing. Reinstall the complete SSPI distribution.")?;
            let installed = install_runtime(root, runtime)?;
            for warning in ampr_index::prepare_staged(root, cancel).map_err(|e| context("Lizard/AMPR index", e))? { warnings.push(warning); }
            emit(meter.snapshot(None, None, ""), format!("Installed the pack-capable AMPR runtime {RUNTIME_VERSION} at {}", installed.strip_prefix(root).unwrap_or(&installed).display()));
            let _ = fs::remove_file(&crc);
            let outcome = ampr_pack::pack(root, &crc, request.workers, &mut |p| {
                cancel()?;
                if p.stage == "pack" { meter.read = p.done; meter.input_bytes = p.total; }
                if due() {
                    let progress = (p.total > 0).then(|| p.done as f64 / p.total as f64);
                    let label = match p.stage { "plan" => "Choosing files to pack", "pack" => "Compressing assets", "finish" => "Writing the pack index", _ => "Checking every packed block" };
                    emit(meter.snapshot(if p.stage == "pack" || p.stage == "verify" { progress } else { None }, None, p.current), label.into());
                }
                Ok(())
            })?;
            warnings.extend(outcome.warnings.iter().cloned());
            let saved = outcome.packed_bytes.saturating_sub(outcome.stored_bytes);
            meter.detail("lizard", format!("{} of {} files packed, {} saved", outcome.files_packed, outcome.files_total, gib(saved)));
            lizard = Some(json!({
                "runtime": RUNTIME_VERSION, "filesTotal": outcome.files_total, "filesPacked": outcome.files_packed,
                "filesAutoLoose": outcome.files_auto_loose, "packedBytes": outcome.packed_bytes, "storedBytes": outcome.stored_bytes,
                "chunks": outcome.chunks, "sharedChunks": outcome.shared_chunks, "packs": outcome.packs,
                "crcPath": crc.display().to_string(),
            }));
        }

        meter.begin("layout");
        cancel()?;
        emit(meter.snapshot(None, None, ""), "Planning the exFAT layout".into());
        let plan = exfat::plan(root, request.label)?;
        meter.input_bytes = plan.payload_bytes;
        meter.detail("layout", format!("{} files, {} folders", plan.files, plan.directories));
        crate::storage::guard_bytes(request.output_dir, plan.image_bytes, "exFAT image")?;

        meter.begin("write");
        let partial = request.output_dir.join(format!("{}.partial", request.file_name));
        let _ = fs::remove_file(&partial);
        let total_files = plan.files;
        let hashes = exfat::write(&plan, &partial, |p| {
            cancel()?;
            meter.write = p.written;
            if due() {
                emit(meter.snapshot(Some(p.written as f64 / p.total.max(1) as f64), Some((p.files_done, total_files)), p.current),
                     format!("Writing the exFAT image · {} of {} files", p.files_done, total_files));
            }
            Ok(())
        })?;
        meter.write = plan.image_bytes;
        meter.detail("write", gib(plan.image_bytes));

        meter.begin("verify");
        let map: HashMap<String, [u8; 32]> = hashes.into_iter().collect();
        let image_read = meter.read;
        let report = exfat::verify::verify(&partial, &exfat::verify::expected(&plan), &map, &mut |hashed, current| {
            cancel()?;
            meter.read = image_read + hashed;
            if due() {
                emit(meter.snapshot(Some(hashed as f64 / plan.payload_bytes.max(1) as f64), None, current), "Reading the image back and checking every file".into());
            }
            Ok(())
        });
        let report = match report { Ok(report) => report, Err(error) => { let _ = fs::remove_file(&partial); return Err(context("exFAT verification failed", error)); } };
        let mismatch = if report.hashed_bytes != plan.payload_bytes { Some(format!("checked {} of {} bytes", report.hashed_bytes, plan.payload_bytes)) }
            else if report.label != request.label { Some(format!("the volume label reads {:?}", report.label)) }
            else if report.cluster_count != plan.cluster_count || report.used_clusters != plan.used_clusters {
                Some(format!("{} of {} clusters in use; the plan has {} of {}", report.used_clusters, report.cluster_count, plan.used_clusters, plan.cluster_count))
            } else { None };
        if let Some(mismatch) = mismatch {
            let _ = fs::remove_file(&partial);
            return Err(format!("exFAT verification failed: {mismatch}."));
        }
        meter.read = image_read + report.hashed_bytes;
        meter.detail("verify", format!("{} entries, {} checked", report.entries.len(), gib(report.hashed_bytes)));
        fs::rename(&partial, &output).map_err(|e| { let _ = fs::remove_file(&partial); format!("Cannot finish {}: {e}", output.display()) })?;
        meter.finish_active();
        let engine = meter.snapshot(Some(1.), Some((total_files, total_files)), "");
        emit(engine.clone(), format!("exFAT image verified: {}", output.display()));
        Ok(Built { bytes: plan.image_bytes, payload_bytes: plan.payload_bytes, files: plan.files, path: output, lizard, warnings: warnings.clone(), engine })
    })();
    if result.is_err() { let _ = fs::remove_file(&crc); }
    result
}

#[cfg(test)]
mod tests;
