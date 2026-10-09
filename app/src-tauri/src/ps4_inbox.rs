use super::*;
use crate::ps4_protocol as protocol;
use suppaftp::{tokio::AsyncFtpStream, types::{FileType, FtpError}, Mode};
use tokio::{io::AsyncSeekExt, time::timeout};

static PS4_UPLOAD: AsyncMutex<()> = AsyncMutex::const_new(());
const ROOT_MISSING: &str = "Connected to the PS4, but SSPI's data folder wasn't found. Install and open SSPI once.";
const WAITING: &str = "Waiting for SSPI on the PS4 to pick up the upload";
const WORKER_MISSING: &str = "Uploaded. SSPI's background worker isn't running; open SSPI on the PS4 to install it.";
pub(super) const UNCONFIRMED: &str = "Uploaded to the PS4 inbox, but SSPI hasn't confirmed the install. Check SSPI on the PS4.";
const INVALID_DELIVERY: &str = "The saved PS4 delivery record is invalid. Remove this entry and start a new transfer.";

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(super) struct InboxFile {
    local: PathBuf,
    final_name: String,
    size: u64,
    primary: bool,
    renamed: bool,
    #[serde(default)] previous_name: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
struct Outcome {
    stage: String,
    message: String,
    receipt_job: Option<String>,
    retrigger: bool,
}
impl Outcome {
    fn new(stage: &str, message: &str) -> Self {
        Self { stage: stage.into(), message: message.into(), receipt_job: None, retrigger: false }
    }
    fn failed(message: String) -> Self {
        Self { retrigger: true, ..Self::new("failed", &message) }
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
struct Unit {
    index: usize,
    files: Vec<InboxFile>,
    outcome: Option<Outcome>,
    #[serde(default)] resident_job: Option<String>,
    #[serde(default)] last_error: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub(super) struct DeliveryState {
    root: String,
    generation: u32,
    units: Vec<Unit>,
}
fn valid_root(root: &str) -> bool { matches!(root, "/data/SSPI" | "/user/data/SSPI") }
fn valid_file(file: &InboxFile) -> bool {
    let kind = if file.primary { protocol::InboxKind::Primary } else { protocol::InboxKind::Continuation };
    protocol::validate_final_name(&file.final_name).is_ok() && protocol::inbox_kind(&file.final_name) == Some(kind)
        && file.previous_name.as_ref().is_none_or(|name| !file.renamed && name != &file.final_name
            && protocol::validate_final_name(name).is_ok() && protocol::inbox_kind(name) == Some(kind))
}
fn valid_unit(unit: &Unit) -> bool {
    if unit.files.iter().filter(|file| file.primary).count() != 1 || unit.files.iter().any(|file| !valid_file(file)) { return false; }
    let primary = unit.files.iter().find(|file| file.primary).unwrap();
    let key = protocol::set_key(&primary.final_name);
    unit.files.iter().all(|file| protocol::set_key(&file.final_name) == key)
}
pub(super) fn validate_delivery(delivery: &DeliveryState) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    if !valid_root(&delivery.root) || delivery.units.is_empty()
        || delivery.units.iter().enumerate().any(|(index, unit)| unit.index != index || !valid_unit(unit))
        || delivery.units.iter().flat_map(|unit| &unit.files).any(|file| {
            !names.insert(file.final_name.to_ascii_lowercase())
                || file.previous_name.as_ref().is_some_and(|name| !names.insert(name.to_ascii_lowercase()))
        }) {
        return Err(INVALID_DELIVERY.into());
    }
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Ps4Probe {
    root: String,
    inbox: String,
    worker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_build: Option<String>,
    message: String,
}

#[derive(Clone)]
struct Endpoint { host: String, port: u16, user: String, password: String }
impl Endpoint {
    fn new(host: String, port: u16, user: Option<String>, password: Option<String>) -> Self {
        Self { host: host.trim().into(), port,
            user: user.filter(|s| !s.trim().is_empty()).map(|s| s.trim().into()).unwrap_or_else(|| "anonymous".into()),
            password: password.filter(|s| !s.trim().is_empty())
                .or_else(|| secret("ps4-ftp").ok().and_then(|s| s.get_password().ok()))
                .unwrap_or_else(|| "anonymous@".into()) }
    }
    fn settings(settings: &Settings) -> Self {
        Self::new(settings.ps4_host.clone(), settings.ps4_ftp_port, Some(settings.ps4_ftp_user.clone()), None)
    }
    fn connection_message(&self) -> String {
        format!("Couldn't connect to the PS4's FTP server at {}:{}. Enable GoldHEN's FTP server and check the address.", self.host, self.port)
    }
}
#[derive(Clone, Copy)]
struct Timing { io: Duration, poll: Duration, heartbeat: Duration, heartbeat_sample: Duration, claim: Duration, idle: Duration, vanished: Duration, cleanup: Duration }
impl Default for Timing {
    fn default() -> Self {
        Self { io: Duration::from_secs(60), poll: Duration::from_secs(3), heartbeat: Duration::from_secs(180),
            heartbeat_sample: Duration::from_millis(2500), claim: Duration::from_secs(45 * 60), idle: Duration::from_secs(45 * 60),
            vanished: Duration::from_secs(15), cleanup: Duration::from_secs(120) }
    }
}
#[derive(Debug)]
struct Fault { message: String, retryable: bool, code: Option<u32> }
impl Fault {
    fn local(message: impl ToString) -> Self { Self { message: message.to_string(), retryable: false, code: None } }
    fn network(message: impl ToString) -> Self { Self { message: message.to_string(), retryable: true, code: None } }
}
impl From<FtpError> for Fault {
    fn from(error: FtpError) -> Self {
        let code = if let FtpError::UnexpectedResponse(response) = &error { Some(response.status.code()) } else { None };
        let retryable = matches!(error, FtpError::ConnectionError(_) | FtpError::BadResponse)
            || code.is_some_and(|code| (400..500).contains(&code));
        Self { message: error.to_string(), retryable, code }
    }
}
async fn ftp_op<T>(future: impl std::future::Future<Output = Result<T, FtpError>>, limit: Duration) -> Result<T, Fault> {
    timeout(limit, future).await.map_err(|_| Fault::network("PS4 FTP operation timed out."))?.map_err(Fault::from)
}
struct Session { ftp: AsyncFtpStream, endpoint: Endpoint, timing: Timing }
impl Session {
    async fn connect(endpoint: Endpoint, timing: Timing) -> Result<Self, Fault> {
        if endpoint.port == 0 || endpoint.host.is_empty() || [&endpoint.host, &endpoint.user, &endpoint.password].iter().any(|s| s.contains(['\r', '\n', '\0'])) {
            return Err(Fault::local(endpoint.connection_message()));
        }
        let connected = timeout(Duration::from_secs(10), async {
            let mut ftp = AsyncFtpStream::connect((endpoint.host.as_str(), endpoint.port)).await?;
            ftp.login(endpoint.user.as_str(), endpoint.password.as_str()).await?;
            ftp.set_mode(Mode::Passive);
            ftp.set_passive_nat_workaround(true);
            ftp.transfer_type(FileType::Binary).await?;
            Ok::<_, FtpError>(ftp)
        }).await.map_err(|_| Fault::network(endpoint.connection_message()))?
            .map_err(|error| { let mut fault = Fault::from(error); fault.message = endpoint.connection_message(); fault })?;
        Ok(Self { ftp: connected, endpoint, timing })
    }
    async fn reconnect(&mut self) -> Result<(), Fault> {
        *self = Self::connect(self.endpoint.clone(), self.timing).await?;
        Ok(())
    }
    async fn size(&mut self, path: &str) -> Result<Option<u64>, Fault> {
        match ftp_op(self.ftp.size(path), self.timing.io).await {
            Ok(size) => Ok(Some(size as u64)), Err(e) if e.code == Some(550) => Ok(None), Err(e) => Err(e),
        }
    }
    async fn directory(&mut self, path: &str) -> Result<bool, Fault> {
        match ftp_op(self.ftp.cwd(path), self.timing.io).await {
            Ok(()) => Ok(true), Err(e) if e.code == Some(550) => Ok(false), Err(e) => Err(e),
        }
    }
    async fn root(&mut self) -> Result<String, Fault> {
        let mut roots = Vec::new();
        for root in ["/data/SSPI", "/user/data/SSPI"] {
            if self.directory(root).await? {
                let heartbeat = self.size(&format!("{root}/resident/heartbeat.txt")).await?.is_some();
                roots.push((root, heartbeat));
            }
        }
        let root = roots.iter().find(|(_, heartbeat)| *heartbeat).or_else(|| roots.first()).map(|(root, _)| *root)
            .ok_or_else(|| Fault::local(ROOT_MISSING))?;
        let inbox = format!("{root}/pkg-rars");
        if !self.directory(&inbox).await? {
            if let Err(error) = ftp_op(self.ftp.mkdir(&inbox), self.timing.io).await {
                if !self.directory(&inbox).await? { return Err(error); }
            }
        }
        Ok(root.into())
    }
    async fn list(&mut self, path: &str) -> Result<Vec<protocol::ListEntry>, Fault> {
        match ftp_op(self.ftp.list(Some(path)), self.timing.io).await {
            Ok(lines) => Ok(lines.iter().filter_map(|line| protocol::parse_list_line(line))
                .filter(|entry| !entry.name.contains(['/', '\\', '\r', '\n', '\0']) && entry.name != "." && entry.name != "..").collect()),
            Err(e) if e.code == Some(550) => Ok(vec![]), Err(e) => Err(e),
        }
    }
    async fn text(&mut self, path: &str) -> Result<Option<String>, Fault> {
        let Some(size) = self.size(path).await? else { return Ok(None); };
        if size > 256 * 1024 { return Err(Fault::local("SSPI status file is too large.")); }
        let mut stream = match ftp_op(self.ftp.retr_as_stream(path), self.timing.io).await {
            Ok(stream) => stream, Err(e) if e.code == Some(550) => return Ok(None), Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        let read = timeout(self.timing.io, (&mut stream).take(256 * 1024 + 1).read_to_end(&mut bytes)).await
            .map_err(|_| Fault::network("PS4 FTP read timed out."))?.map_err(Fault::network);
        ftp_op(stream.finish(), self.timing.io).await?;
        read?;
        if bytes.len() > 256 * 1024 { return Err(Fault::local("SSPI status file is too large.")); }
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }
    async fn remove(&mut self, path: &str) -> Result<(), Fault> {
        match ftp_op(self.ftp.rm(path), self.timing.io).await {
            Ok(()) => Ok(()), Err(e) if e.code == Some(550) => Ok(()), Err(e) => Err(e),
        }
    }
    async fn rename(&mut self, from: &str, to: &str) -> Result<(), Fault> {
        ftp_op(self.ftp.rename(from, to), self.timing.io).await
    }
    async fn worker(&mut self, root: &str) -> Result<(String, Option<String>), Fault> {
        let path = format!("{root}/resident/heartbeat.txt");
        let first_text = self.text(&path).await?;
        let first = first_text.as_deref().and_then(protocol::parse_heartbeat);
        sleep(self.timing.heartbeat_sample).await;
        let second_text = self.text(&path).await?;
        let second = second_text.as_deref().and_then(protocol::parse_heartbeat);
        let worker = match (&first, &second) {
            (Some(a), Some(b)) if b.dotnet_ticks > a.dotnet_ticks => if b.inbox_ready() { "ready" } else { "starting" },
            (None, None) if first_text.is_none() && second_text.is_none() => "missing", _ => "stale",
        };
        let build = second.or(first).and_then(|heartbeat| heartbeat.cap("build").map(str::to_string));
        Ok((worker.into(), build))
    }
}
pub(super) async fn ready(settings: &Settings) -> Result<(), String> {
    let mut session = Session::connect(Endpoint::settings(settings), Timing::default()).await.map_err(|e| e.message)?;
    session.root().await.map_err(|e| e.message)?;
    Ok(())
}
pub(super) async fn probe(host: String, port: u16, user: Option<String>, password: Option<String>) -> Result<Ps4Probe, String> {
    let mut session = Session::connect(Endpoint::new(host, port, user, password), Timing::default()).await.map_err(|e| e.message)?;
    let root = session.root().await.map_err(|e| e.message)?;
    let inbox = format!("{root}/pkg-rars");
    let (worker, worker_build) = session.worker(&root).await.map_err(|e| e.message)?;
    let message = match worker.as_str() {
        "ready" => format!("Connected. SSPI inbox: {inbox}. Background worker is running."),
        "starting" => "Connected. SSPI's background worker is starting; uploads wait until it's ready.".into(),
        _ => "Connected. SSPI's background worker isn't running. Uploads install when SSPI is open on the PS4.".into(),
    };
    Ok(Ps4Probe { root, inbox, worker, worker_build, message })
}

struct Context<'a> { app: Option<&'a AppHandle>, job: &'a str, cancel: &'a watch::Receiver<bool> }
impl Context<'_> {
    async fn checkpoint(&self) -> Result<(), Fault> {
        if let Some(app) = self.app { transfer_checkpoint(app, self.job, self.cancel).await.map_err(Fault::local) }
        else if *self.cancel.borrow() { Err(Fault::local("cancelled")) } else { Ok(()) }
    }
    fn report(&self, stage: &str, message: &str, done: u64, total: u64) {
        if let Some(app) = self.app {
            emit(app, Progress { job_id: self.job.into(), target: "ps4".into(), stage: stage.into(), message: message.into(),
                bytes_done: done, bytes_total: total, progress: if stage == "complete" { 1. } else if total > 0 { (done as f64 / total as f64).min(0.99) } else { 0. },
                ..Default::default() });
        }
    }
    fn save(&self, delivery: &DeliveryState) -> Result<(), Fault> {
        if let Some(app) = self.app {
            let state = app.state::<AppState>();
            let mut store = state.retry.lock().unwrap();
            store.records.get_mut(self.job).ok_or_else(|| Fault::local("PS4 delivery journal is missing."))?.ps4_delivery = Some(delivery.clone());
            store.save(self.job).map_err(Fault::local)?;
        }
        Ok(())
    }
}
fn tag(job: &str) -> Result<String, String> {
    Uuid::parse_str(job).map(|id| id.simple().to_string()[..8].to_string()).map_err(|_| "Invalid job identity".into())
}
pub(super) fn validate_pkg(path: &Path) -> Result<(), String> {
    if path.is_dir() { return Err("PS4 delivery takes PKG files. Game folders can only be sent to a PS5.".into()); }
    let identity = fpkg::package_identity(path)?;
    if identity.to_ascii_uppercase().contains("PPSA") { return Err("PS5 games can't be installed on a PS4.".into()); }
    Ok(())
}
fn make_units(paths: Vec<PathBuf>, archive: bool, job: &str) -> Result<Vec<Unit>, String> {
    let tag = tag(job)?;
    let mut units = Vec::new();
    for path in paths {
        let name = path.file_name().and_then(|s| s.to_str()).ok_or("The upload file name isn't valid UTF-8.")?;
        let final_name = protocol::final_name(name, &tag, 0)?;
        let primary = protocol::inbox_kind(&final_name) == Some(protocol::InboxKind::Primary);
        let file = InboxFile { size: std::fs::metadata(&path).map_err(redact)?.len(), local: path, final_name, primary, renamed: false, previous_name: None };
        if !archive || units.is_empty() { units.push(Unit { index: units.len(), files: vec![], outcome: None, resident_job: None, last_error: String::new() }); }
        units.last_mut().unwrap().files.push(file);
    }
    if units.is_empty() || units.iter().any(|unit| unit.files.iter().filter(|file| file.primary).count() != 1) { return Err("The PS4 inbox needs one primary file per upload set.".into()); }
    let mut names = std::collections::HashSet::new();
    if units.iter().flat_map(|u| &u.files).any(|f| !names.insert(f.final_name.to_ascii_lowercase())) { return Err("The PS4 upload contains duplicate file names.".into()); }
    Ok(units)
}

async fn upload_attempt(session: &mut Session, root: &str, file: &InboxFile, context: &Context<'_>) -> Result<bool, Fault> {
    let final_path = format!("{root}/pkg-rars/{}", file.final_name);
    if session.size(&final_path).await? == Some(file.size) { return Ok(true); }
    let temporary = format!("{root}/pkg-rars/{}", protocol::temp_name(&file.final_name));
    let mut force_store = false;
    for attempt in 0..2 {
        context.checkpoint().await?;
        let remote = session.size(&temporary).await?;
        if remote == Some(file.size) { return Ok(false); }
        let mut offset = remote.filter(|size| *size < file.size).unwrap_or(0);
        if force_store { offset = 0; }
        let mut source = fs::File::open(&file.local).await.map_err(Fault::local)?;
        if source.metadata().await.map_err(Fault::local)?.len() != file.size { return Err(Fault::local("The local upload changed size. Start a new job for this file.")); }
        let opening = if offset > 0 { ftp_op(session.ftp.append_with_stream(&temporary), session.timing.io).await }
            else { ftp_op(session.ftp.put_with_stream(&temporary), session.timing.io).await };
        let mut stream = match opening {
            Ok(stream) => stream,
            Err(e) if offset > 0 && e.code.is_some_and(|code| (500..600).contains(&code)) => {
                session.reconnect().await?;
                offset = 0;
                ftp_op(session.ftp.put_with_stream(&temporary), session.timing.io).await?
            },
            Err(e) => return Err(e),
        };
        source.seek(std::io::SeekFrom::Start(offset)).await.map_err(Fault::local)?;
        let message = format!("Sending {} to the PS4 inbox", file.local.file_name().unwrap_or_default().to_string_lossy());
        context.report("uploading", &message, offset, file.size);
        let mut bytes = vec![0; 64 * 1024];
        loop {
            context.checkpoint().await?;
            let count = source.read(&mut bytes).await.map_err(Fault::local)?;
            if count == 0 { break; }
            let mut cancel = context.cancel.clone();
            tokio::select! {
                result = timeout(session.timing.io, stream.write_all(&bytes[..count])) => {
                    result.map_err(|_| Fault::network("PS4 FTP upload stalled."))?.map_err(Fault::network)?;
                },
                _ = cancel.changed() => return Err(Fault::local("cancelled")),
            }
            offset += count as u64;
            context.report("uploading", &message, offset, file.size);
        }
        ftp_op(stream.finish(), session.timing.io).await?;
        if verify_temporary(session, &temporary, file.size).await? { return Ok(false); }
        if attempt == 1 { return Err(Fault::local("PS4 FTP upload size doesn't match the local file after restarting.")); }
        force_store = true;
    }
    unreachable!()
}
async fn verify_temporary(session: &mut Session, path: &str, size: u64) -> Result<bool, Fault> {
    if session.size(path).await? == Some(size) { return Ok(true); }
    session.remove(path).await?;
    Ok(false)
}
async fn upload_file(session: &mut Session, root: &str, file: &InboxFile, context: &Context<'_>) -> Result<bool, Fault> {
    let mut reconnects = 0;
    loop {
        match upload_attempt(session, root, file, context).await {
            Ok(final_exists) => return Ok(final_exists),
            Err(error) if error.message == "cancelled" => {
                // A canceled stream may leave an unread transfer reply; use a clean control connection.
                if session.reconnect().await.is_ok() { let _ = session.remove(&format!("{root}/pkg-rars/{}", protocol::temp_name(&file.final_name))).await; }
                return Err(error);
            },
            Err(mut error) if error.retryable => {
                loop {
                    if reconnects >= 3 { return Err(error); }
                    context.checkpoint().await?;
                    reconnects += 1;
                    match session.reconnect().await { Ok(()) => break, Err(e) if e.retryable => error = e, Err(e) => return Err(e) }
                }
            },
            Err(error) => return Err(error),
        }
    }
}
async fn rename_file(session: &mut Session, root: &str, file: &InboxFile) -> Result<(), Fault> {
    if !valid_root(root) || !valid_file(file) { return Err(Fault::local(INVALID_DELIVERY)); }
    let destination = format!("{root}/pkg-rars/{}", file.final_name);
    let source = format!("{root}/pkg-rars/{}", file.previous_name.clone().unwrap_or_else(|| protocol::temp_name(&file.final_name)));
    for attempt in 0..=3 {
        let result = async {
            if session.size(&destination).await? == Some(file.size) { return Ok(()); }
            if session.size(&source).await? != Some(file.size) { return Err(Fault::local("The PS4 inbox upload is missing or changed size before handoff.")); }
            session.rename(&source, &destination).await
        }.await;
        match result {
            Err(e) if e.retryable && attempt < 3 => { session.reconnect().await?; },
            result => return result,
        }
    }
    unreachable!()
}
fn prepare_retrigger(delivery: &mut DeliveryState, index: usize, job: &str) -> Result<(), String> {
    delivery.generation = delivery.generation.checked_add(1).ok_or("PS4 retry generation is exhausted.")?;
    let tag = tag(job)?;
    let unit = &mut delivery.units[index];
    for file in &mut unit.files {
        let name = file.local.file_name().and_then(|s| s.to_str()).ok_or("The upload file name isn't valid UTF-8.")?;
        let new_name = protocol::final_name(name, &tag, delivery.generation)?;
        file.previous_name = Some(std::mem::replace(&mut file.final_name, new_name));
        file.renamed = false;
    }
    unit.outcome = None;
    unit.resident_job = None;
    unit.last_error.clear();
    Ok(())
}
async fn rename_unit(session: &mut Session, delivery: &mut DeliveryState, index: usize, context: &Context<'_>) -> Result<(), Fault> {
    let mut order: Vec<usize> = (0..delivery.units[index].files.len()).collect();
    order.sort_by_key(|&file| delivery.units[index].files[file].primary);
    for file_index in order {
        if delivery.units[index].files[file_index].renamed { continue; }
        context.checkpoint().await?;
        rename_file(session, &delivery.root, &delivery.units[index].files[file_index]).await?;
        let file = &mut delivery.units[index].files[file_index];
        file.renamed = true;
        file.previous_name = None;
        context.save(delivery)?;
    }
    Ok(())
}
fn rank(kind: &str) -> u8 { match kind.to_ascii_lowercase().as_str() { "base" => 0, "update" => 1, _ => 2 } }
pub(super) fn blocked_by_lower_rank(jobs: &HashMap<String, Progress>, job: &str, title: &str, wanted: u8) -> bool {
    !title.is_empty() && wanted > 0 && jobs.values().any(|other| other.job_id != job && other.target == "ps4"
        && other.title_id.eq_ignore_ascii_case(title) && !other.removed && !other.package_only && !terminal_stage(&other.stage)
        && other.stage != "removing" && rank(&other.package_kind) < wanted)
}
async fn wait_for_base(context: &Context<'_>, unit: &Unit, timing: Timing) -> Result<(), Fault> {
    let Some(app) = context.app else { return Ok(()); };
    loop {
        context.checkpoint().await?;
        let blocked = {
            let state = app.state::<AppState>();
            let jobs = state.jobs.lock().unwrap();
            let current = jobs.get(context.job).cloned().unwrap_or_default();
            let wanted = rank(&current.package_kind).max(unit.files.iter().map(|f| pkg_role(&f.local)).max().unwrap_or(0));
            blocked_by_lower_rank(&jobs, context.job, &current.title_id, wanted)
        };
        if !blocked { return Ok(()); }
        context.report("handoff", "Waiting for the base game to finish installing on the PS4", 0, 0);
        sleep(timing.poll).await;
    }
}

async fn find_claim(session: &mut Session, root: &str, file: &InboxFile) -> Result<Option<protocol::Claim>, Fault> {
    let directory = format!("{root}/ftp-inbox-claims");
    let prefix = protocol::claim_prefix(&file.final_name);
    let mut newest: Option<protocol::Claim> = None;
    for entry in session.list(&directory).await?.into_iter().filter(|e| !e.dir && e.name.starts_with(&prefix) && e.name.ends_with(".claim")) {
        if let Some(claim) = session.text(&format!("{directory}/{}", entry.name)).await?.and_then(|s| protocol::parse_claim(&s)) {
            if claim.name == file.final_name && claim.size == file.size && newest.as_ref().is_none_or(|previous| claim.mtime > previous.mtime) { newest = Some(claim); }
        }
    }
    Ok(newest)
}
fn valid_job(job: &str) -> bool { !job.is_empty() && job.len() <= 240 && job.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')) && job != "." && job != ".." }
async fn receipt(session: &mut Session, root: &str, job: &str) -> Result<bool, Fault> {
    if !valid_job(job) { return Ok(false); }
    Ok(session.text(&format!("{root}/resident/{}", protocol::receipt_name(job))).await?
        .and_then(|s| protocol::parse_install_receipt(&s)).is_some_and(|receipt| receipt.job == job))
}
async fn statuses(session: &mut Session, root: &str, job: &str) -> Result<Vec<protocol::StageStatus>, Fault> {
    let directory = format!("{root}/resident");
    let mut entries = session.list(&directory).await?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut statuses = Vec::new();
    for entry in entries.into_iter().filter(|e| !e.dir && e.name.starts_with("transfer-") && e.name.ends_with(".status")) {
        if let Some(status) = session.text(&format!("{directory}/{}", entry.name)).await?.and_then(|s| protocol::parse_stage_status(&s)) {
            if status.job == job { statuses.push(status); }
        }
    }
    Ok(statuses)
}
struct Observation { claim: Option<protocol::Claim>, receipt: Option<String>, statuses: Vec<protocol::StageStatus> }
async fn observe(session: &mut Session, delivery: &DeliveryState, index: usize) -> Result<Observation, Fault> {
    let unit = &delivery.units[index];
    if let Some(job) = &unit.resident_job {
        if receipt(session, &delivery.root, job).await? { return Ok(Observation { claim: None, receipt: Some(job.clone()), statuses: vec![] }); }
    }
    let primary = unit.files.iter().find(|f| f.primary).ok_or_else(|| Fault::local("PS4 upload primary is missing."))?;
    let claim = find_claim(session, &delivery.root, primary).await?;
    let job = claim.as_ref().filter(|c| c.owner == "resident" && valid_job(&c.job)).map(|c| c.job.as_str()).or(unit.resident_job.as_deref());
    let mut installed = None;
    let mut progress = vec![];
    if let Some(job) = job {
        if receipt(session, &delivery.root, job).await? { installed = Some(job.into()); }
        else { progress = statuses(session, &delivery.root, job).await?; }
    }
    Ok(Observation { claim, receipt: installed, statuses: progress })
}
fn claim_outcome(claim: &protocol::Claim, last_error: &str) -> Option<Outcome> {
    if claim.owner == "app" { return Some(Outcome::new("delivered", "SSPI on the PS4 took the upload. Follow its progress in SSPI.")); }
    match claim.state.as_str() {
        "installed" if claim.job.is_empty() => Some(Outcome::new("complete", "Already installed on the PS4; SSPI skipped this copy.")),
        "busy" => Some(Outcome { retrigger: true, ..Outcome::new("monitoring-ended", "The PS4 is already installing this package; SSPI skipped this copy.") }),
        "invalid" => Some(Outcome::failed("SSPI rejected the file as invalid.".into())),
        "unsupported-name" => Some(Outcome::failed("SSPI rejected the file name.".into())),
        "failed" | "canceled" => Some(Outcome::failed(failure_message(last_error))),
        _ => None,
    }
}
fn failure_message(last_error: &str) -> String {
    if last_error.is_empty() { "SSPI couldn't install the upload.".into() } else { format!("PS4: {last_error}") }
}
async fn track(session: &mut Session, delivery: &mut DeliveryState, index: usize, context: &Context<'_>) -> Result<Outcome, Fault> {
    let started = Instant::now();
    let mut changed = Instant::now();
    let mut signature = String::new();
    let mut seen_claim = delivery.units[index].resident_job.is_some();
    let mut seen_status = false;
    let mut vanished = None;
    let mut heartbeat_checked = false;
    let mut waiting = WAITING;
    context.report("handoff", waiting, 0, 0);
    loop {
        let observation = match observe(session, delivery, index).await {
            Ok(observation) => observation,
            Err(_) => {
                if changed.elapsed() >= session.timing.idle { return Ok(Outcome::new("monitoring-ended", UNCONFIRMED)); }
                sleep(session.timing.poll).await;
                let _ = session.reconnect().await;
                continue;
            },
        };
        if let Some(job) = observation.receipt {
            return Ok(Outcome { receipt_job: Some(job), ..Outcome::new("complete", "Installed on the PS4") });
        }
        let unit = &mut delivery.units[index];
        let previous_job = unit.resident_job.clone();
        let previous_error = unit.last_error.clone();
        if let Some(claim) = &observation.claim {
            seen_claim = true;
            if claim.owner == "resident" && valid_job(&claim.job) { unit.resident_job = Some(claim.job.clone()); }
        }
        let new_signature = format!("{:?}|{:?}", observation.claim, observation.statuses);
        if new_signature != signature { signature = new_signature; changed = Instant::now(); }
        for status in &observation.statuses { if !status.error.is_empty() { unit.last_error = status.error.clone(); } }
        let outcome = observation.claim.as_ref().and_then(|claim| claim_outcome(claim, &unit.last_error));
        let status_failed = observation.statuses.iter().any(|status| matches!(status.state.as_str(), "failed" | "canceled"));
        let last_error = unit.last_error.clone();
        if previous_job != unit.resident_job || previous_error != last_error { context.save(delivery)?; }
        if let Some(outcome) = outcome { return Ok(outcome); }
        if status_failed { return Ok(Outcome::failed(failure_message(&last_error))); }
        if !observation.statuses.is_empty() { seen_status = true; }
        if let Some(status) = observation.statuses.iter().rev().find(|status| status.state != "released") {
            vanished = None;
            context.report("installing", if status.state == "extracting" { "Extracting on the PS4" } else { "Installing on the PS4" },
                status.done.max(0) as u64, status.total.max(0) as u64);
        } else if seen_status {
            let disappeared = vanished.get_or_insert_with(Instant::now);
            if disappeared.elapsed() >= session.timing.vanished { return Ok(Outcome::failed(failure_message(&last_error))); }
        }
        if !seen_claim && started.elapsed() >= session.timing.heartbeat && !heartbeat_checked {
            if let Ok((worker, _)) = session.worker(&delivery.root).await {
                heartbeat_checked = true;
                if matches!(worker.as_str(), "missing" | "stale") { waiting = WORKER_MISSING; }
                context.report("handoff", waiting, 0, 0);
            }
        }
        if (!seen_claim && started.elapsed() >= session.timing.claim) || changed.elapsed() >= session.timing.idle {
            return Ok(Outcome::new("monitoring-ended", UNCONFIRMED));
        }
        sleep(session.timing.poll).await;
    }
}

async fn cleanup_unreferenced(session: &mut Session, root: &str, job: &str) -> Result<bool, Fault> {
    if !receipt(session, root, job).await? { return Ok(false); }
    let resident = format!("{root}/resident");
    for slot in 0..16 {
        if let Some(text) = session.text(&format!("{resident}/transfer-{slot}.job")).await? {
            match protocol::parse_stage_job_id(&text) {
                Some(id) if id != job => {},
                _ => return Ok(false),
            }
        }
    }
    for name in ["job.txt", "status.txt"] {
        if session.text(&format!("{resident}/{name}")).await?.is_some_and(|text| protocol::names_job(&text, job)) { return Ok(false); }
    }
    Ok(true)
}
async fn cleanup_remote(session: &mut Session, root: &str, unit: &Unit, job: &str) -> bool {
    if !valid_root(root) || !valid_unit(unit) { return false; }
    let limit = session.timing.cleanup;
    let result = timeout(limit, async {
      loop {
        match cleanup_unreferenced(session, root, job).await {
            Ok(true) => {
                for file in &unit.files {
                    if session.remove(&format!("{root}/pkg-rars/{}", file.final_name)).await.is_err() { return false; }
                }
                return true;
            },
            Ok(false) => {},
            Err(_) => { let _ = session.reconnect().await; },
        }
        sleep(session.timing.poll).await;
      }
    }).await;
    match result {
        Ok(cleaned) => cleaned,
        Err(_) => { let _ = session.reconnect().await; false },
    }
}
pub(super) async fn archive_volumes(primary: &Path, inputs: &[PathBuf], password: Option<&str>, request: &DeliveryRequest) -> Option<Vec<PathBuf>> {
    if password.is_some_and(|s| !s.is_empty()) || request.archive_parts.iter().any(|p| p.archive_password.as_deref().is_some_and(|s| !s.is_empty())) { return None; }
    let counts: Vec<u32> = request.archive_parts.iter().chain(std::iter::once(&request.package)).filter_map(|p| p.archive_part_count).collect();
    let (primary, inputs) = (primary.to_path_buf(), inputs.to_vec());
    // Directory and UnRAR checks block; keep them off the async workers that downloads share.
    tokio::task::spawn_blocking(move || {
        let name = primary.file_name()?.to_str()?;
        if !name.to_ascii_lowercase().ends_with(".rar") || protocol::inbox_kind(name) != Some(protocol::InboxKind::Primary) { return None; }
        let key = protocol::set_key(name)?;
        let mut present = std::fs::read_dir(primary.parent()?).ok()?.filter_map(Result::ok).map(|e| e.path())
            .filter(|p| p.file_name().and_then(|s| s.to_str()).and_then(protocol::set_key).as_ref() == Some(&key)).collect::<Vec<_>>();
        present.sort(); present.dedup();
        let mut volumes = if inputs.is_empty() { present.clone() } else { inputs };
        if !volumes.iter().any(|p| p == &primary) { return None; }
        volumes.sort(); volumes.dedup();
        if volumes != present { return None; }
        if volumes.iter().any(|path| !path.is_file() || path.file_name().and_then(|s| s.to_str()).is_none_or(|name|
            protocol::set_key(name).as_ref() != Some(&key) || protocol::final_name(name, "01234567", 0).is_err())) { return None; }
        if counts.iter().any(|count| *count as usize != volumes.len()) { return None; }
        let mut numbers: Vec<_> = volumes.iter().filter_map(|p| p.file_name().and_then(|s| s.to_str()).and_then(rar_part_from_name)).collect();
        if !numbers.is_empty() {
            numbers.sort_unstable();
            if numbers != (1..=volumes.len() as u32).collect::<Vec<_>>() { return None; }
        }
        // UnRAR walks all volumes without decompressing. Missing trailing volumes or encrypted entries force PC extraction.
        let _library = rar_control::library_lock(&|| Ok(())).ok()?;
        let archive = unrar::Archive::new(&primary).open_for_listing().ok()?;
        if archive.has_encrypted_headers() { return None; }
        let mut count = 0;
        for entry in archive {
            if entry.ok()?.is_encrypted() { return None; }
            count += 1;
        }
        (count > 0).then_some(volumes)
    }).await.ok().flatten()
}
async fn run_units(session: &mut Session, delivery: &mut DeliveryState, context: &Context<'_>, remove: bool, retry: bool) -> Result<Outcome, Fault> {
    validate_delivery(delivery).map_err(Fault::local)?;
    for index in 0..delivery.units.len() {
        if delivery.units[index].outcome.as_ref().is_some_and(|outcome| matches!(outcome.stage.as_str(), "complete" | "delivered")) { continue; }
        if retry && delivery.units[index].files.iter().all(|file| file.renamed) {
            let observation = observe(session, delivery, index).await?;
            if observation.receipt.is_none() {
                let last_error = observation.statuses.iter().rev().find(|status| !status.error.is_empty()).map(|status| status.error.as_str())
                    .unwrap_or(&delivery.units[index].last_error);
                if let Some(outcome) = observation.claim.as_ref().and_then(|claim| claim_outcome(claim, last_error)).filter(|outcome| outcome.retrigger) {
                    delivery.units[index].outcome = Some(outcome);
                }
            } else { delivery.units[index].outcome = None; }
        }
        if let Some(outcome) = &delivery.units[index].outcome {
            if outcome.retrigger {
                prepare_retrigger(delivery, index, context.job).map_err(Fault::local)?;
                context.save(delivery)?;
            }
        }
        if !delivery.units[index].files.iter().all(|file| file.renamed) {
            context.checkpoint().await?;
            let mut canceled = context.cancel.clone();
            let slot = tokio::select! {
                slot = PS4_UPLOAD.lock() => slot,
                _ = canceled.changed() => return Err(Fault::local("cancelled")),
            };
            for file_index in 0..delivery.units[index].files.len() {
                let file = &delivery.units[index].files[file_index];
                if file.renamed || file.previous_name.is_some() { continue; }
                let final_exists = upload_file(session, &delivery.root, file, context).await?;
                if final_exists {
                    delivery.units[index].files[file_index].renamed = true;
                    context.save(delivery)?;
                }
            }
            drop(slot);
            wait_for_base(context, &delivery.units[index], session.timing).await?;
            rename_unit(session, delivery, index, context).await?;
        }
        let mut outcome = track(session, delivery, index, context).await?;
        delivery.units[index].outcome = Some(outcome.clone());
        context.save(delivery)?;
        if remove {
            if let Some(job) = &outcome.receipt_job {
                if !cleanup_remote(session, &delivery.root, &delivery.units[index], job).await {
                    outcome.message.push_str(" Uploaded files were kept in the PS4 inbox because safe cleanup couldn't be confirmed.");
                    delivery.units[index].outcome = Some(outcome.clone());
                    context.save(delivery)?;
                }
            }
        }
        if !matches!(outcome.stage.as_str(), "complete" | "delivered") { return Ok(outcome); }
    }
    if delivery.units.iter().any(|unit| unit.outcome.as_ref().is_some_and(|outcome| outcome.stage == "delivered")) {
        return Ok(Outcome::new("delivered", if delivery.units.len() == 1 {
            "SSPI on the PS4 took the upload. Follow its progress in SSPI."
        } else { "SSPI on the PS4 took the uploads. Follow their progress in SSPI." }));
    }
    let installed = delivery.units.iter().any(|u| u.outcome.as_ref().is_some_and(|o| o.receipt_job.is_some()));
    let mut outcome = Outcome::new("complete", if installed { "Installed on the PS4" } else { "Already installed on the PS4; SSPI skipped this copy." });
    if delivery.units.iter().any(|u| u.outcome.as_ref().is_some_and(|o| o.message.contains("Uploaded files were kept"))) {
        outcome.message.push_str(" Uploaded files were kept in the PS4 inbox because safe cleanup couldn't be confirmed.");
    }
    Ok(outcome)
}
pub(super) async fn deliver(app: &AppHandle, settings: &Settings, job: &str, paths: Vec<PathBuf>, archive: bool, cancel: &watch::Receiver<bool>) -> Result<(), String> {
    let context = Context { app: Some(app), job, cancel };
    context.checkpoint().await.map_err(|e| e.message)?;
    let record = job_store::record(app, job).ok_or("PS4 delivery journal is missing.")?;
    let retry = record.ps4_delivery.is_some();
    if let Some(saved) = &record.ps4_delivery { validate_delivery(saved)?; }
    let mut session = Session::connect(Endpoint::settings(settings), Timing::default()).await.map_err(|e| e.message)?;
    let mut delivery = if let Some(saved) = record.ps4_delivery { saved } else {
        // Package reads block; keep them off the async workers that downloads share.
        let job_id = job.to_string();
        let units = tokio::task::spawn_blocking(move || {
            if !archive { for path in &paths { validate_pkg(path)?; } }
            if !archive && paths.iter().any(|path| pkg_meta::read(path).is_ok_and(|meta| meta.kind == "theme")) {
                return Err("PS4 themes install through the SSPI receiver. Switch PS4 delivery to the receiver in Settings > Consoles.".to_string());
            }
            make_units(if archive { paths } else { sort_pkgs(paths) }, archive, &job_id)
        }).await.map_err(|_| "The PS4 upload check stopped unexpectedly.".to_string())??;
        DeliveryState { root: session.root().await.map_err(|e| e.message)?, generation: 0, units }
    };
    context.save(&delivery).map_err(|e| e.message)?;
    let result = run_units(&mut session, &mut delivery, &context, settings.ps4_remove_after_install, retry).await;
    if result.as_ref().is_err_and(|e| e.message == "cancelled") && session.reconnect().await.is_ok() {
        for file in delivery.units.iter().flat_map(|unit| &unit.files).filter(|file| !file.renamed) {
            let _ = session.remove(&format!("{}/pkg-rars/{}", delivery.root, protocol::temp_name(&file.final_name))).await;
        }
    }
    let mut outcome = result.map_err(|e| e.message)?;
    if outcome.stage == "complete" {
        // Imported originals never enter the consumed-input list or package cleanup helper.
        if let Some(record) = job_store::record(app, job) {
            if !settings.keep_archives {
                let inputs = match &record.checkpoint {
                    Some(job_store::Checkpoint::Archive { inputs, .. } | job_store::Checkpoint::Extracted { inputs, .. }) => inputs.clone(),
                    _ => vec![],
                };
                // Deletes run on the blocking pool, never on an async worker the downloads share.
                let removed = tokio::task::spawn_blocking(move || archives::remove_consumed_inputs(&inputs, &[])).await.map_err(redact).and_then(|result| result);
                if let Err(error) = removed { outcome.message.push_str(&format!(" Local archives were kept: {error}")); }
            }
            if !settings.keep_packages {
                for file in delivery.units.iter().flat_map(|u| &u.files).filter(|f| f.local.extension().is_some_and(|e| e.eq_ignore_ascii_case("pkg"))) {
                    if let Some(error) = cleanup_delivered_package(app, job, &file.local).await { outcome.message.push_str(&format!(" Local package was kept: {error}")); }
                }
            }
        }
    }
    context.report(&outcome.stage, &outcome.message, 0, 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ps4_fake_ftp::{FakeFtp, FakeOptions};

    const JOB: &str = "01234567-89ab-4def-8012-0123456789ab";
    const ROOT: &str = "/data/SSPI";
    fn timing() -> Timing {
        Timing { io: Duration::from_secs(3), poll: Duration::from_millis(10), heartbeat: Duration::from_millis(200),
            heartbeat_sample: Duration::from_millis(20), claim: Duration::from_millis(800), idle: Duration::from_millis(800),
            vanished: Duration::from_millis(60), cleanup: Duration::from_millis(400) }
    }
    fn local(name: &str, bytes: &[u8]) -> PathBuf {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Build-Output/Windows Manager/ps4-tests").join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name); std::fs::write(&path, bytes).unwrap(); path
    }
    fn server(options: FakeOptions) -> FakeFtp {
        let server = FakeFtp::start(options);
        server.mkdir(ROOT); server.mkdir(&format!("{ROOT}/pkg-rars"));
        server.mkdir(&format!("{ROOT}/resident")); server.mkdir(&format!("{ROOT}/ftp-inbox-claims")); server
    }
    async fn session(server: &FakeFtp) -> Session {
        Session::connect(Endpoint { host: "127.0.0.1".into(), port: server.port(), user: "anonymous".into(), password: "anonymous@".into() }, timing()).await.unwrap()
    }
    fn delivery(names: &[&str], bytes: &[u8], archive: bool) -> DeliveryState {
        DeliveryState { root: ROOT.into(), generation: 0, units: make_units(names.iter().map(|name| local(name, bytes)).collect(), archive, JOB).unwrap() }
    }
    fn final_path(file: &InboxFile) -> String { format!("{ROOT}/pkg-rars/{}", file.final_name) }
    fn temp_path(file: &InboxFile) -> String { format!("{ROOT}/pkg-rars/{}", protocol::temp_name(&file.final_name)) }
    fn claim(server: &FakeFtp, file: &InboxFile, owner: &str, state: &str, job: &str, mtime: i64) {
        server.put(&format!("{ROOT}/ftp-inbox-claims/{}", protocol::claim_file_name(&file.final_name, file.size, mtime)),
            format!("{owner}\n{}\n{}\n{mtime}\n{job}\n{state}\n", file.final_name, file.size).as_bytes());
    }
    fn installed(server: &FakeFtp, job: &str) {
        server.put(&format!("{ROOT}/resident/{}", protocol::receipt_name(job)),
            format!("1\n{}\n{}\n128\ngen\n", protocol::b64_encode(job.as_bytes()), protocol::b64_encode(b"UP0000-CUSA12345_00-TEST000000000000")).as_bytes());
    }
    fn status(server: &FakeFtp, job: &str, state: &str, error: &str) {
        server.put(&format!("{ROOT}/resident/transfer-0.status"),
            format!("1\n{}\n{state}\n64\n128\n0\n{}\n1\n1\n64\ngen\nbuild\n", protocol::b64_encode(job.as_bytes()), protocol::b64_encode(error.as_bytes())).as_bytes());
    }
    #[tokio::test]
    async fn fresh_pkg_uses_temp_size_and_tag_then_rename() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"package bytes", false);
        let file = delivery.units[0].files[0].clone();
        assert!(file.final_name.contains("01234567"));
        assert!(!upload_file(&mut session, ROOT, &file, &context).await.unwrap());
        assert!(!server.exists(&final_path(&file)));
        assert_eq!(server.get(&temp_path(&file)).unwrap(), b"package bytes");
        rename_unit(&mut session, &mut delivery, 0, &context).await.unwrap();
        assert_eq!(server.get(&final_path(&file)).unwrap(), b"package bytes");
        let commands = server.commands();
        assert!(commands.contains(&format!("STOR {}", temp_path(&file))));
        assert!(commands.iter().filter(|s| *s == &format!("SIZE {}", temp_path(&file))).count() >= 2);
        assert!(commands.contains(&"PASV".into())); assert!(commands.contains(&"TYPE I".into()));
        assert!(!commands.iter().any(|s| s.starts_with("EPSV") || s.starts_with("NLST") || s.starts_with("MLSD")));
        assert!(upload_file(&mut session, ROOT, &file, &context).await.unwrap());
        assert_eq!(server.commands().iter().filter(|s| s.starts_with("STOR ")).count(), 1);
    }
    #[tokio::test]
    async fn rar_renames_continuations_before_primary() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.part1.rar", "game.part2.rar", "game.part3.rar"], b"rar bytes", true);
        for file in &delivery.units[0].files { upload_file(&mut session, ROOT, file, &context).await.unwrap(); }
        rename_unit(&mut session, &mut delivery, 0, &context).await.unwrap();
        let renames: Vec<_> = server.commands().into_iter().filter(|s| s.starts_with("RNTO ")).collect();
        assert_eq!(renames.len(), 3);
        assert!(renames[0].ends_with(".part2.rar")); assert!(renames[1].ends_with(".part3.rar")); assert!(renames[2].ends_with(".part1.rar"));
    }
    #[tokio::test]
    async fn dropped_upload_resumes_with_appe() {
        let server = server(FakeOptions { drop_after: Some(32768), ..FakeOptions::standard() }); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let bytes: Vec<_> = (0..400_000).map(|n| (n % 251) as u8).collect();
        let delivery = delivery(&["game.pkg"], &bytes, false); let file = &delivery.units[0].files[0];
        upload_file(&mut session, ROOT, file, &context).await.unwrap();
        assert_eq!(server.get(&temp_path(file)).unwrap(), bytes);
        assert!(server.commands().contains(&format!("APPE {}", temp_path(file))));
        assert!(!server.commands().iter().any(|s| s.starts_with("REST ")));
    }
    #[tokio::test]
    async fn ignored_rest_size_mismatch_is_deleted_and_restarted_with_stor() {
        let server = server(FakeOptions { ignore_rest: true, ..FakeOptions::standard() }); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let bytes = b"0123456789abcdefghij";
        let delivery = delivery(&["game.pkg"], bytes, false); let file = &delivery.units[0].files[0]; let path = temp_path(file);
        server.put(&path, &bytes[..10]);
        session.ftp.resume_transfer(10).await.unwrap();
        let mut stream = session.ftp.put_with_stream(&path).await.unwrap();
        stream.write_all(&bytes[10..]).await.unwrap(); stream.finish().await.unwrap();
        assert_eq!(session.size(&path).await.unwrap(), Some(10));
        assert!(!verify_temporary(&mut session, &path, file.size).await.unwrap());
        assert!(!server.exists(&path));
        upload_file(&mut session, ROOT, file, &context).await.unwrap();
        assert_eq!(server.get(&path).unwrap(), bytes);
        let commands = server.commands(); let deleted = commands.iter().position(|s| s == &format!("DELE {path}")).unwrap();
        assert!(commands[deleted + 1..].contains(&format!("STOR {path}")));
    }
    #[tokio::test]
    async fn appe_unavailable_falls_back_to_full_stor() {
        let server = server(FakeOptions { appe: false, ..FakeOptions::standard() }); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let delivery = delivery(&["game.pkg"], b"full package", false); let file = &delivery.units[0].files[0];
        server.put(&temp_path(file), b"full");
        upload_file(&mut session, ROOT, file, &context).await.unwrap();
        assert_eq!(server.get(&temp_path(file)).unwrap(), b"full package");
    }
    #[tokio::test]
    async fn appe_550_552_and_553_reconnect_and_restart_with_stor() {
        use tokio::{io::{AsyncBufReadExt, BufReader}, net::TcpListener};
        for code in [550, 552, 553] {
            let server = server(FakeOptions::standard());
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port(); let upstream_port = server.port();
            let rejected = Arc::new(AtomicU64::new(0)); let rejected_proxy = rejected.clone();
            // Intercept only APPE on the control connection; FakeFtp still handles the data channel and filesystem.
            let proxy = tokio::spawn(async move {
                for _ in 0..2 {
                    let (client, _) = listener.accept().await.unwrap(); let rejected = rejected_proxy.clone();
                    tokio::spawn(async move {
                        let upstream = TcpStream::connect(("127.0.0.1", upstream_port)).await.unwrap();
                        let (client_read, client_write) = client.into_split(); let (server_read, mut server_write) = upstream.into_split();
                        let client_write = AsyncMutex::new(client_write);
                        let commands = async {
                            let mut reader = BufReader::new(client_read); let mut line = String::new();
                            loop {
                                line.clear(); if reader.read_line(&mut line).await.unwrap_or(0) == 0 { break; }
                                if line.starts_with("APPE ") {
                                    rejected.fetch_add(1, Ordering::Relaxed);
                                    if client_write.lock().await.write_all(format!("{code} Append rejected\r\n").as_bytes()).await.is_err() { break; }
                                } else if server_write.write_all(line.as_bytes()).await.is_err() { break; }
                            }
                        };
                        let replies = async {
                            let mut reader = BufReader::new(server_read); let mut line = String::new();
                            loop {
                                line.clear(); if reader.read_line(&mut line).await.unwrap_or(0) == 0 { break; }
                                if client_write.lock().await.write_all(line.as_bytes()).await.is_err() { break; }
                            }
                        };
                        tokio::select! { _ = commands => {}, _ = replies => {} }
                    });
                }
            });
            let mut session = Session::connect(Endpoint { host: "127.0.0.1".into(), port, user: "anonymous".into(), password: "anonymous@".into() }, timing()).await.unwrap();
            let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
            let delivery = delivery(&["game.pkg"], b"full package", false); let file = &delivery.units[0].files[0];
            server.put(&temp_path(file), b"full");
            upload_file(&mut session, ROOT, file, &context).await.unwrap();
            timeout(Duration::from_secs(3), proxy).await.unwrap().unwrap();
            assert_eq!(rejected.load(Ordering::Relaxed), 1);
            assert_eq!(server.get(&temp_path(file)).unwrap(), b"full package");
            assert_eq!(server.commands().iter().filter(|s| s.starts_with("USER ")).count(), 2);
            assert_eq!(server.commands().iter().filter(|s| *s == &format!("STOR {}", temp_path(file))).count(), 1);
        }
    }
    #[tokio::test]
    async fn resident_receipt_wins_even_after_status_slot_is_reused() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false); let file = &delivery.units[0].files[0];
        claim(&server, file, "resident", "queued", "ftp_old", 1);
        claim(&server, file, "resident", "queued", "ftp_new", 2);
        installed(&server, "ftp_new"); status(&server, "unrelated", "failed", "not this job");
        let result = track(&mut session, &mut delivery, 0, &context).await.unwrap();
        assert_eq!(result.stage, "complete"); assert_eq!(result.receipt_job.as_deref(), Some("ftp_new"));
    }
    #[tokio::test]
    async fn installed_busy_invalid_and_app_claims_are_truthful() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        for (index, (owner, state, expected)) in [("resident", "installed", "complete"), ("resident", "busy", "monitoring-ended"),
            ("resident", "invalid", "failed"), ("resident", "unsupported-name", "failed"), ("app", "queued", "delivered")].into_iter().enumerate() {
            let mut delivery = delivery(&[&format!("game{index}.pkg")], b"pkg", false);
            claim(&server, &delivery.units[0].files[0], owner, state, "", 1);
            let result = track(&mut session, &mut delivery, 0, &context).await.unwrap();
            assert_eq!(result.stage, expected); assert!(result.receipt_job.is_none());
        }
    }
    #[tokio::test]
    async fn failed_claim_retry_renames_to_generation_one_without_upload() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false); let old = final_path(&delivery.units[0].files[0]);
        server.put(&old, b"pkg"); delivery.units[0].files[0].renamed = true;
        claim(&server, &delivery.units[0].files[0], "resident", "failed", "ftp_failed", 1);
        status(&server, "ftp_failed", "failed", "Base game is missing.");
        let outcome = track(&mut session, &mut delivery, 0, &context).await.unwrap();
        assert_eq!(outcome.message, "PS4: Base game is missing."); assert!(outcome.retrigger);
        delivery.units[0].outcome = Some(outcome);
        prepare_retrigger(&mut delivery, 0, JOB).unwrap();
        let saved = serde_json::to_vec(&delivery).unwrap(); let mut restored: DeliveryState = serde_json::from_slice(&saved).unwrap();
        rename_unit(&mut session, &mut restored, 0, &context).await.unwrap();
        assert_eq!(restored.generation, 1); assert!(restored.units[0].files[0].final_name.contains("01234567-1"));
        assert!(!server.exists(&old)); assert_eq!(server.get(&final_path(&restored.units[0].files[0])).unwrap(), b"pkg");
        assert!(!server.commands().iter().any(|s| s.starts_with("STOR ") || s.starts_with("APPE ")));
    }
    #[tokio::test]
    async fn cleanup_waits_for_stage_and_singleton_references() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let delivery = delivery(&["game.pkg"], b"pkg", false); let unit = &delivery.units[0]; let remote = final_path(&unit.files[0]);
        server.put(&remote, b"pkg"); installed(&server, "ftp_test");
        let stage = format!("{ROOT}/resident/transfer-7.job");
        server.put(&stage, format!("1\n{}\n", protocol::b64_encode(b"ftp_test")).as_bytes());
        assert!(!cleanup_remote(&mut session, ROOT, unit, "ftp_test").await); assert!(server.exists(&remote));
        server.remove(&stage);
        for singleton in ["job.txt", "status.txt"] {
            let path = format!("{ROOT}/resident/{singleton}"); server.put(&path, protocol::b64_encode(b"ftp_test").as_bytes());
            assert!(!cleanup_unreferenced(&mut session, ROOT, "ftp_test").await.unwrap()); server.remove(&path);
        }
        assert!(cleanup_remote(&mut session, ROOT, unit, "ftp_test").await); assert!(!server.exists(&remote));
    }
    #[tokio::test]
    async fn cleanup_keeps_sources_when_a_known_slot_is_unparseable() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let delivery = delivery(&["game.pkg"], b"pkg", false); let unit = &delivery.units[0]; let remote = final_path(&unit.files[0]);
        server.put(&remote, b"pkg"); installed(&server, "ftp_test");
        server.put(&format!("{ROOT}/resident/transfer-3.job"), b"unparseable job");
        assert!(!cleanup_remote(&mut session, ROOT, unit, "ftp_test").await);
        assert!(server.exists(&remote));
        assert!(!server.commands().iter().any(|s| s.starts_with("DELE ") || s.starts_with("LIST ")));
    }
    #[tokio::test]
    async fn cleanup_checks_known_slots_even_when_resident_list_returns_550() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let delivery = delivery(&["game.pkg"], b"pkg", false); let unit = &delivery.units[0]; let remote = final_path(&unit.files[0]);
        server.put(&remote, b"pkg"); installed(&server, "ftp_test");
        server.put(&format!("{ROOT}/resident/transfer-15.job"), format!("1\n{}\n", protocol::b64_encode(b"ftp_test")).as_bytes());
        let resident = format!("{ROOT}/resident"); server.remove(&resident);
        let error = ftp_op(session.ftp.list(Some(&resident)), session.timing.io).await.unwrap_err();
        assert_eq!(error.code, Some(550)); session.reconnect().await.unwrap();
        let before = server.commands().len();
        assert!(!cleanup_remote(&mut session, ROOT, unit, "ftp_test").await);
        assert!(server.exists(&remote));
        let commands = server.commands(); let checks = &commands[before..];
        assert!(checks.contains(&format!("RETR {ROOT}/resident/transfer-15.job")));
        assert!(!checks.iter().any(|s| s.starts_with("DELE ") || s.starts_with("LIST ")));
    }
    #[tokio::test]
    async fn unclaimed_upload_times_out_without_install_success() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        let result = track(&mut session, &mut delivery, 0, &context).await.unwrap();
        assert_eq!(result.stage, "monitoring-ended"); assert_eq!(result.message, UNCONFIRMED);
    }
    #[tokio::test]
    async fn units_wait_for_each_receipt_and_stop_on_app_handoff() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["base.pkg", "update.pkg"], b"pkg", false);
        let base = delivery.units[0].files[0].clone(); let update = delivery.units[1].files[0].clone();
        let worker = async {
            while !server.exists(&final_path(&base)) { sleep(Duration::from_millis(5)).await; }
            claim(&server, &base, "resident", "queued", "ftp_base", 1);
            status(&server, "ftp_base", "installing", "");
            sleep(Duration::from_millis(50)).await;
            assert!(!server.exists(&final_path(&update)));
            installed(&server, "ftp_base");
            while !server.exists(&final_path(&update)) { sleep(Duration::from_millis(5)).await; }
            claim(&server, &update, "app", "queued", "app_update", 1);
        };
        let (result, ()) = timeout(Duration::from_secs(4), async { tokio::join!(run_units(&mut session, &mut delivery, &context, false, false), worker) }).await.unwrap();
        assert_eq!(result.unwrap().stage, "delivered");
        assert_eq!(delivery.units[0].outcome.as_ref().unwrap().stage, "complete");
        assert_eq!(delivery.units[1].outcome.as_ref().unwrap().stage, "delivered");
    }
    #[tokio::test]
    async fn app_owned_first_unit_does_not_skip_later_uploads() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["base.pkg", "update.pkg"], b"pkg", false);
        let first = delivery.units[0].files[0].clone(); let second = delivery.units[1].files[0].clone();
        let worker = async {
            while !server.exists(&final_path(&first)) { sleep(Duration::from_millis(5)).await; }
            claim(&server, &first, "app", "queued", "app_base", 1);
            while !server.exists(&final_path(&second)) { sleep(Duration::from_millis(5)).await; }
            claim(&server, &second, "resident", "queued", "ftp_update", 1); installed(&server, "ftp_update");
        };
        let (result, ()) = timeout(Duration::from_secs(4), async { tokio::join!(run_units(&mut session, &mut delivery, &context, false, false), worker) }).await.unwrap();
        let outcome = result.unwrap();
        assert_eq!(outcome.stage, "delivered");
        assert_eq!(outcome.message, "SSPI on the PS4 took the uploads. Follow their progress in SSPI.");
        assert_eq!(delivery.units[0].outcome.as_ref().unwrap().stage, "delivered");
        assert_eq!(delivery.units[1].outcome.as_ref().unwrap().stage, "complete");
        assert_eq!(server.get(&final_path(&second)).unwrap(), b"pkg");
        assert_eq!(server.commands().iter().filter(|s| s.starts_with("RNTO ")).count(), 2);
    }
    #[tokio::test]
    async fn retry_skips_saved_delivered_units_and_keeps_singular_copy() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        delivery.units[0].files[0].renamed = true;
        delivery.units[0].outcome = Some(Outcome::new("delivered", "SSPI on the PS4 took the upload. Follow its progress in SSPI."));
        let commands = server.commands();
        let outcome = run_units(&mut session, &mut delivery, &context, true, true).await.unwrap();
        assert_eq!(outcome.stage, "delivered"); assert_eq!(outcome.message, "SSPI on the PS4 took the upload. Follow its progress in SSPI.");
        assert_eq!(server.commands(), commands);
    }
    #[tokio::test]
    async fn retry_discovers_a_late_failed_claim_and_retriggers_once() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        let file = &mut delivery.units[0].files[0]; file.renamed = true; server.put(&final_path(file), b"pkg");
        claim(&server, file, "resident", "invalid", "", 1);
        let mut next = file.clone(); next.final_name = protocol::final_name("game.pkg", "01234567", 1).unwrap();
        delivery.units[0].outcome = Some(Outcome::new("monitoring-ended", UNCONFIRMED));
        claim(&server, &next, "resident", "installed", "", 2);
        let outcome = run_units(&mut session, &mut delivery, &context, false, true).await.unwrap();
        assert_eq!(outcome.stage, "complete"); assert_eq!(delivery.generation, 1);
        assert!(server.exists(&final_path(&next))); assert!(!server.commands().iter().any(|s| s.starts_with("STOR ")));
    }
    #[tokio::test]
    async fn status_disappearance_waits_for_late_receipt_and_keeps_last_error() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        claim(&server, &delivery.units[0].files[0], "resident", "queued", "ftp_missing", 1);
        status(&server, "ftp_missing", "installing", "Installer stopped.");
        let worker = async {
            while !server.commands().iter().any(|s| s == &format!("RETR {ROOT}/resident/transfer-0.status")) { sleep(Duration::from_millis(5)).await; }
            sleep(Duration::from_millis(20)).await;
            server.remove(&format!("{ROOT}/resident/transfer-0.status"));
        };
        let (outcome, ()) = timeout(Duration::from_secs(3), async { tokio::join!(track(&mut session, &mut delivery, 0, &context), worker) }).await.unwrap();
        assert_eq!(outcome.unwrap().message, "PS4: Installer stopped.");
    }
    #[tokio::test]
    async fn released_status_without_receipt_fails_after_grace_and_keeps_error() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        delivery.units[0].last_error = "Installer stopped.".into();
        claim(&server, &delivery.units[0].files[0], "resident", "queued", "ftp_released", 1);
        status(&server, "ftp_released", "released", "");
        let started = Instant::now();
        let outcome = track(&mut session, &mut delivery, 0, &context).await.unwrap();
        assert!(started.elapsed() >= session.timing.vanished);
        assert_eq!(outcome.stage, "failed"); assert_eq!(outcome.message, "PS4: Installer stopped.");
        assert_eq!(delivery.units[0].last_error, "Installer stopped.");
    }
    #[tokio::test]
    async fn released_status_accepts_receipt_during_grace() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        session.timing.vanished = Duration::from_millis(250);
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        claim(&server, &delivery.units[0].files[0], "resident", "queued", "ftp_released", 1);
        status(&server, "ftp_released", "released", "");
        let worker = async {
            while !server.commands().iter().any(|s| s == &format!("RETR {ROOT}/resident/transfer-0.status")) { sleep(Duration::from_millis(5)).await; }
            sleep(Duration::from_millis(40)).await; installed(&server, "ftp_released");
        };
        let (outcome, ()) = timeout(Duration::from_secs(3), async { tokio::join!(track(&mut session, &mut delivery, 0, &context), worker) }).await.unwrap();
        let outcome = outcome.unwrap(); assert_eq!(outcome.stage, "complete"); assert_eq!(outcome.receipt_job.as_deref(), Some("ftp_released"));
    }
    #[tokio::test]
    async fn invalid_saved_delivery_is_rejected_before_ftp_commands() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let (_tx, cancel) = watch::channel(false); let context = Context { app: None, job: JOB, cancel: &cancel };
        let original = delivery(&["game.pkg"], b"pkg", false);
        let mut cases = Vec::new();
        let mut invalid = original.clone(); invalid.root = "/data/SSPI/../other".into(); cases.push(invalid);
        let mut invalid = original.clone(); invalid.units[0].files[0].final_name = "../game.pkg".into(); cases.push(invalid);
        let mut invalid = original.clone(); invalid.units[0].files[0].previous_name = Some("../old.pkg".into()); cases.push(invalid);
        let mut invalid = original.clone(); invalid.units.clear(); cases.push(invalid);
        let mut invalid = original.clone(); invalid.units[0].files.clear(); cases.push(invalid);
        let mut invalid = original.clone(); invalid.units[0].files[0].primary = false; cases.push(invalid);
        let mut invalid = original.clone(); invalid.units[0].files[0].final_name = "game.part2.rar".into(); cases.push(invalid);
        let mut invalid = original.clone(); invalid.units[0].index = 1; cases.push(invalid);
        let mut invalid = original.clone(); let duplicate = invalid.units[0].files[0].clone(); invalid.units[0].files.push(duplicate); cases.push(invalid);
        let mut invalid = delivery(&["game.part1.rar", "game.part2.rar"], b"rar", true);
        invalid.units[0].files[1].final_name = "another.part2.rar".into(); cases.push(invalid);
        let mut invalid = original.clone(); let mut duplicate = invalid.units[0].clone(); duplicate.index = 1; invalid.units.push(duplicate); cases.push(invalid);
        let commands = server.commands();
        for invalid in cases {
            let mut restored: DeliveryState = serde_json::from_slice(&serde_json::to_vec(&invalid).unwrap()).unwrap();
            let error = run_units(&mut session, &mut restored, &context, true, true).await.unwrap_err();
            assert_eq!(error.message, INVALID_DELIVERY); assert_eq!(server.commands(), commands);
        }
        let mut pending = delivery(&["game.part1.rar", "game.part2.rar"], b"rar", true);
        pending.root = "/user/data/SSPI".into(); prepare_retrigger(&mut pending, 0, JOB).unwrap();
        pending.units[0].files[1].renamed = true; pending.units[0].files[1].previous_name = None;
        validate_delivery(&pending).unwrap();
    }
    #[tokio::test]
    async fn invalid_remote_paths_cannot_rename_or_delete() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let mut delivery = delivery(&["game.pkg"], b"pkg", false);
        let commands = server.commands();
        assert_eq!(rename_file(&mut session, "/other", &delivery.units[0].files[0]).await.unwrap_err().message, INVALID_DELIVERY);
        assert!(!cleanup_remote(&mut session, "/other", &delivery.units[0], "ftp_test").await);
        delivery.units[0].files[0].previous_name = Some("../original.pkg".into());
        assert_eq!(rename_file(&mut session, ROOT, &delivery.units[0].files[0]).await.unwrap_err().message, INVALID_DELIVERY);
        assert!(!cleanup_remote(&mut session, ROOT, &delivery.units[0], "ftp_test").await);
        assert_eq!(server.commands(), commands);
    }
    /// Two stored RAR 4.x volumes, `stem.rar` and `stem.r00`, holding one split file.
    fn rar4_pair(directory: &Path, stem: &str, data: &[u8]) -> Vec<PathBuf> {
        fn crc32(bytes: &[u8]) -> u32 {
            !bytes.iter().fold(!0u32, |crc, &byte| (0..8).fold(crc ^ byte as u32, |c, _| (c >> 1) ^ (0xEDB8_8320 & (c & 1).wrapping_neg())))
        }
        fn block(kind: u8, flags: u16, body: &[u8]) -> Vec<u8> {
            let mut header = vec![kind]; header.extend(flags.to_le_bytes()); header.extend((7 + body.len() as u16).to_le_bytes()); header.extend(body);
            let mut out = (crc32(&header) as u16).to_le_bytes().to_vec(); out.extend(header); out
        }
        let parts: Vec<&[u8]> = data.chunks(data.len().div_ceil(2)).collect();
        parts.iter().enumerate().map(|(index, part)| {
            let (first, last) = (index == 0, index + 1 == parts.len());
            let mut body = Vec::new();
            body.extend((part.len() as u32).to_le_bytes()); body.extend((data.len() as u32).to_le_bytes()); body.push(2);
            body.extend((if last { crc32(data) } else { crc32(part) }).to_le_bytes()); body.extend(0x5B2A_0000u32.to_le_bytes());
            body.extend([29, 0x30]); body.extend(8u16.to_le_bytes()); body.extend(0x20u32.to_le_bytes()); body.extend(b"game.pkg");
            let mut volume = b"Rar!\x1a\x07\x00".to_vec();
            volume.extend(block(0x73, 0x0001 | if first { 0x0100 } else { 0 }, &[0; 6]));
            volume.extend(block(0x74, 0x8000 | if first { 0 } else { 0x01 } | if last { 0 } else { 0x02 }, &body));
            volume.extend_from_slice(part);
            volume.extend(block(0x7B, if last { 0 } else { 0x0001 }, &[]));
            let path = directory.join(if first { format!("{stem}.rar") } else { format!("{stem}.r{:02}", index - 1) });
            std::fs::write(&path, volume).unwrap(); path
        }).collect()
    }
    #[tokio::test]
    async fn complete_unencrypted_rar_sets_are_sent_whole_and_others_are_not() {
        let directory = local("probe.txt", b"").parent().unwrap().to_path_buf();
        let volumes = rar4_pair(&directory, "game", &(0..64 * 1024).map(|n| (n % 251) as u8).collect::<Vec<_>>());
        let mut sorted = volumes.clone(); sorted.sort();
        let request: DeliveryRequest = serde_json::from_value(json!({"target":"ps4","package":Package::default(),"titleId":null})).unwrap();
        assert_eq!(archive_volumes(&volumes[0], &[], None, &request).await, Some(sorted.clone()));
        assert_eq!(archive_volumes(&volumes[0], &volumes, None, &request).await, Some(sorted));
        assert_eq!(archive_volumes(&volumes[0], &[], Some("secret"), &request).await, None);
        let counted: DeliveryRequest = serde_json::from_value(json!({"target":"ps4","package":Package { archive_part_count: Some(3), ..Default::default() },"titleId":null})).unwrap();
        assert_eq!(archive_volumes(&volumes[0], &[], None, &counted).await, None);
        // A missing continuation volume forces extraction on the PC.
        std::fs::remove_file(&volumes[1]).unwrap();
        assert_eq!(archive_volumes(&volumes[0], &[], None, &request).await, None);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn pkg_handoff_rejects_ps5_content_identity() {
        let mut bytes = vec![0u8; 128]; bytes[..4].copy_from_slice(&[0x7f, b'C', b'N', b'T']);
        bytes[0x40..0x64].copy_from_slice(b"UP0000-PPSA12345_00-TEST000000000000");
        assert_eq!(validate_pkg(&local("game.pkg", &bytes)).unwrap_err(), "PS5 games can't be installed on a PS4.");
        bytes[0x40..0x64].copy_from_slice(b"UP0000-CUSA12345_00-TEST000000000000");
        assert!(validate_pkg(&local("game.pkg", &bytes)).is_ok());
    }
    #[tokio::test]
    async fn root_detection_prefers_heartbeat_and_creates_inbox() {
        let server = server(FakeOptions::standard()); server.mkdir("/user/data/SSPI");
        server.put("/user/data/SSPI/resident/heartbeat.txt", b"1\n123\nftpinbox=1 transfer=1 bgft=1 build=test\n");
        let mut session = session(&server).await;
        assert_eq!(session.root().await.unwrap(), "/user/data/SSPI");
        assert!(server.exists("/user/data/SSPI/pkg-rars"));
    }
    #[tokio::test]
    async fn worker_probe_distinguishes_ready_starting_stale_and_missing() {
        let server = server(FakeOptions::standard()); let mut session = session(&server).await;
        let path = format!("{ROOT}/resident/heartbeat.txt");
        assert_eq!(session.worker(ROOT).await.unwrap().0, "missing");
        server.put(&path, b"1\n100\nftpinbox=1 transfer=1 bgft=1 build=test\n");
        assert_eq!(session.worker(ROOT).await.unwrap().0, "stale");
        for (tick, caps, expected) in [(101, "ftpinbox=1 transfer=1 bgft=1 build=test", "ready"), (102, "ftpinbox=1 build=test", "starting")] {
            server.replace_after_next_retr(&path, format!("1\n{tick}\n{caps}\n").as_bytes());
            let (state, build) = session.worker(ROOT).await.unwrap();
            assert_eq!(state, expected); assert_eq!(build.as_deref(), Some("test"));
        }
    }
    #[test]
    fn ordering_only_waits_on_active_ps4_jobs_for_the_same_title() {
        let mut jobs = HashMap::new();
        jobs.insert("base".into(), Progress { job_id: "base".into(), target: "ps4".into(), title_id: "CUSA12345".into(), package_kind: "base".into(), stage: "installing".into(), ..Default::default() });
        assert!(blocked_by_lower_rank(&jobs, "update", "CUSA12345", 1));
        assert!(!blocked_by_lower_rank(&jobs, "update", "CUSA99999", 1));
        jobs.get_mut("base").unwrap().stage = "delivered".into();
        assert!(!blocked_by_lower_rank(&jobs, "update", "CUSA12345", 1));
        jobs.get_mut("base").unwrap().stage = "installing".into(); jobs.get_mut("base").unwrap().target = "ps5".into();
        assert!(!blocked_by_lower_rank(&jobs, "update", "CUSA12345", 1));
    }
}
