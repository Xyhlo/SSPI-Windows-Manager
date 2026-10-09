/* =====================================================================
   Updates — a card that rises in when a new SSPI is out, fills while it
   downloads, draws a check when it's ready, and Options → Updates with
   the installed build, the release waiting and its notes.
   ===================================================================== */
import { useEffect, useState } from "react"
import { invoke } from "@tauri-apps/api/core"
import { errorText } from "@/lib/format"
import { toast } from "./toasts"
import logo from "@/assets/sspi-logo.svg"
import { runUpdateAction, startUpdater, updateBusy, updaterHasUserError, updaterSupported, useUpdater, type UpdateStatus } from "@/lib/updater"
import { CheckDraw, Icon } from "./Icon"
import "./updater.css"

const mb = (bytes: number) => `${(bytes / 1048576).toFixed(bytes >= 1048576 * 100 ? 0 : 1)} MB`
const percent = (s: UpdateStatus) => s.total ? Math.min(100, Math.round((s.downloaded / s.total) * 100)) : 0
const ago = (at: number) => {
  if (!at) return "not yet"
  const minutes = Math.round((Date.now() - at) / 60000)
  return minutes < 1 ? "just now" : minutes < 60 ? `${minutes} min ago` : new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
}

function Progress({ update }: { update: UpdateStatus }) {
  const verifying = update.stage === "verifying"
  return (
    <div className="up-progress">
      <span className={`progress ${verifying ? "indeterminate" : ""}`}><i className="fill" style={{ width: `${percent(update)}%` }} /></span>
      <span className="up-progress-text">{verifying ? "Checking every file" : `${mb(update.downloaded)} of ${mb(update.total)}`}<b>{verifying ? "" : `${percent(update)}%`}</b></span>
    </div>
  )
}

function StageIcon({ update }: { update: UpdateStatus }) {
  const stage = update.stage
  return (
    <span className={`up-icon stage-${stage}`}>
      {stage === "ready" ? <CheckDraw on /> : stage === "installing" || stage === "verifying" ? <span className="spinner" />
        : stage === "error" ? <Icon name="alert" /> : <Icon name={stage === "downloading" ? "download" : "sparkle"} />}
    </span>
  )
}

/** Options → Updates. */
export function UpdatePanel({ demo = false }: { demo?: boolean }) {
  const update = useUpdater(demo)
  useEffect(() => demo ? undefined : startUpdater(), [demo])
  const busy = updateBusy(update)
  const supported = demo || updaterSupported()
  const act = (command: Parameters<typeof runUpdateAction>[0]) => void runUpdateAction(command, false, demo)
  const release = update.available
  const pill = !supported ? ["", "Installed app only"]
    : update.stage === "checking" ? ["act", "Checking"]
    : update.stage === "error" ? ["fail", "Couldn't update"]
    : update.stage === "ready" ? ["good", "Ready to install"]
    : release ? ["warn", `${release.version} available`]
    : update.stage === "current" ? ["good", "Up to date"] : ["", "Not checked yet"]

  return (
    <section className="up-panel" aria-label="Application updates">
      <p className="lede">SSPI checks for updates every 10 minutes while it's open. Updates include the app, its packaging tools and the console receivers.</p>
      <div className="up-hero">
        <img src={logo} alt="" className="up-logo" />
        <div className="up-hero-text">
          <strong>SSPI {update.currentVersion || "for Windows"}</strong>
          <span>{update.currentBuild ? `Build ${update.currentBuild} · ` : ""}{update.channel === "stable" ? "Stable" : "Development"} channel · checked {ago(update.lastChecked)}</span>
        </div>
        <span className={`up-pill ${pill[0]} swap-fade`} key={pill[1]}><span className={`dot ${pill[0]}`} />{pill[1]}</span>
        <button type="button" className="btn sm" disabled={!supported || busy} onClick={() => act("check_for_updates")}>
          {update.stage === "checking" ? <span className="spinner" /> : <Icon name="refresh" />}Check now
        </button>
      </div>

      {release && (
        <div className={`up-release stage-${update.stage}`}>
          <div className="up-release-head">
            <StageIcon update={update} />
            <div className="up-release-title">
              <strong>SSPI {release.version}</strong>
              <span>Build {release.build}{release.publishedAt ? ` · ${new Date(release.publishedAt).toLocaleDateString([], { month: "short", day: "numeric" })}` : ""} · {mb(release.package.size)}</span>
            </div>
            <div className="up-release-actions">
              {update.stage === "ready" || update.stage === "installing"
                ? <button type="button" className="btn primary sm" disabled={!supported || update.stage === "installing"} onClick={() => act("install_update")}>{update.stage === "installing" ? <span className="spinner" /> : <Icon name="retry" />}Restart and install</button>
                : <button type="button" className="btn primary sm" disabled={!supported || busy} onClick={() => act("download_update")}>{update.stage === "downloading" ? <span className="spinner" /> : <Icon name="download" />}{update.stage === "downloading" ? "Downloading" : update.stage === "verifying" ? "Checking" : update.stage === "error" ? "Try again" : "Download"}</button>}
            </div>
          </div>
          {(update.stage === "downloading" || update.stage === "verifying") && <Progress update={update} />}
          {release.notes && <p className="up-notes">{release.notes}</p>}
          {release.restart && <p className="up-restart"><Icon name="info" />{release.restart}</p>}
        </div>
      )}
      {update.stage === "error" && update.message && <p className="up-error" role="alert"><Icon name="alert" />{update.message}</p>}
      {!supported && <p className="opt-note">Update checks run in the installed SSPI app.</p>}
      <p className="opt-note">Installing waits until downloads, packaging and transfers finish or are cancelled. SSPI closes, replaces its files and reopens; settings, saved keys, payloads and downloads stay where they are. Reload the receiver on your consoles afterwards to pick up an updated one.</p>
      <button type="button" className="btn sm" disabled={demo || !supported} onClick={() => void invoke("show_session_log").catch(error => toast({ tone: "error", title: "The diagnostic log couldn't be opened", text: errorText(error) }))}><Icon name="folder" />Show Windows diagnostic log</button>
    </section>
  )
}

/** Mounted once with the app; checks continue while Options is closed. */
export function Updater({ demo = false }: { demo?: boolean }) {
  const update = useUpdater(demo)
  const [dismissed, setDismissed] = useState("")
  useEffect(() => demo ? undefined : startUpdater(), [demo])
  const userError = !demo && updaterHasUserError()
  const key = `${update.available?.build}:${update.stage === "downloading" || update.stage === "verifying" ? "progress" : update.stage}:${userError ? update.message : ""}`
  const visible = dismissed !== key && (userError || (!!update.available && ["available", "downloading", "verifying", "ready", "installing"].includes(update.stage)))
  const [shown, setShown] = useState(visible)
  const [leaving, setLeaving] = useState(false)
  useEffect(() => {
    if (visible) { setShown(true); setLeaving(false) }
    else if (shown) setLeaving(true)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible])
  if (!shown) return null
  const act = (command: Parameters<typeof runUpdateAction>[0]) => void runUpdateAction(command, false, demo)
  const version = update.available?.version
  const title = update.stage === "error" ? "The update didn't finish"
    : update.stage === "ready" ? `SSPI ${version} is ready`
    : update.stage === "installing" ? "Restarting SSPI…"
    : update.stage === "downloading" || update.stage === "verifying" ? `Downloading SSPI ${version}`
    : `SSPI ${version} is available`
  const text = update.stage === "error" ? update.message
    : update.stage === "ready" ? "Restart to finish. Settings and downloads stay where they are."
    : update.stage === "installing" ? "SSPI closes, installs the update and reopens."
    : update.stage === "available" ? update.available?.notes?.split(/(?<=\.)\s/)[0] || "A new version is ready to download." : ""

  return (
    <aside className={`up-card stage-${update.stage} ${leaving ? "is-leaving" : ""}`} aria-label="SSPI update" role="status"
      onAnimationEnd={event => { if (event.target === event.currentTarget && leaving) { setShown(false); setLeaving(false) } }}>
      <StageIcon update={update} />
      <div className="up-card-body swap-fade" key={update.stage === "verifying" ? "downloading" : update.stage}>
        <strong>{title}</strong>
        {text && <p>{text}</p>}
        {(update.stage === "downloading" || update.stage === "verifying") && <Progress update={update} />}
        <div className="up-card-actions">
          {update.stage === "available" && <button type="button" className="btn primary sm" onClick={() => act("download_update")}><Icon name="download" />Download</button>}
          {update.stage === "ready" && <button type="button" className="btn primary sm" onClick={() => act("install_update")}><Icon name="retry" />Restart and install</button>}
          {update.stage === "error" && <button type="button" className="btn sm" onClick={() => act(update.available ? "download_update" : "check_for_updates")}><Icon name="retry" />Try again</button>}
          {(update.stage === "available" || update.stage === "ready") && <button type="button" className="btn ghost sm" onClick={() => setDismissed(key)}>Later</button>}
        </div>
      </div>
      <button type="button" className="t-close" aria-label="Dismiss update notice" onClick={() => setDismissed(key)}><Icon name="x" /></button>
    </aside>
  )
}
