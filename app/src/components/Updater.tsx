import { useEffect, useState } from "react"
import { runUpdateAction, startUpdater, updateBusy, updaterHasUserError, updaterSupported, useUpdater } from "@/lib/updater"
import "./updater.css"

function size(bytes: number) { return `${(bytes / 1048576).toFixed(1)} MB` }

export function UpdatePanel({ disabled = false }: { disabled?: boolean }) {
  const update = useUpdater()
  useEffect(() => disabled ? undefined : startUpdater(), [disabled])
  const busy = updateBusy(update)
  const supported = updaterSupported() && !disabled
  return <section className="sspi-update-panel" aria-label="Application updates">
    <div className="opt-section">Application updates</div>
    <p className="opt-note">SSPI checks every 10 minutes while running. Updates include the app, its tools and console receiver payloads.</p>
    <div className="sspi-update-summary">
      <strong>{update.currentVersion ? `SSPI ${update.currentVersion}` : "SSPI"}</strong>
      <span>{update.available ? `Update ${update.available.version} · build ${update.available.build}` : ""}</span>
    </div>
    <p className="sspi-update-message" role={update.stage === "error" ? "alert" : "status"}>
      {!supported ? "Update checks are available in the installed app." : update.message || "Check for the latest update."}
    </p>
    {update.stage === "downloading" && <div className="sspi-update-progress">
      <progress max={update.total || 1} value={update.downloaded} aria-label="Update download" />
      <span>{size(update.downloaded)} / {size(update.total)}</span>
    </div>}
    {update.available?.notes && <p className="sspi-update-notes">{update.available.notes}</p>}
    <div className="sspi-update-actions">
      <button type="button" className="btn sm" disabled={!supported || busy} onClick={() => void runUpdateAction("check_for_updates")}>{update.stage === "checking" ? "Checking…" : "Check for updates"}</button>
      {update.available && update.stage !== "ready" && <button type="button" className="btn primary sm" disabled={!supported || busy} onClick={() => void runUpdateAction("download_update")}>{update.stage === "downloading" ? "Downloading…" : update.stage === "verifying" ? "Verifying…" : "Download update"}</button>}
      {update.stage === "ready" && <button type="button" className="btn primary sm" disabled={!supported} onClick={() => void runUpdateAction("install_update")}>Restart and install</button>}
    </div>
    <p className="opt-note">Finish or cancel active jobs before installing. SSPI closes and reopens; settings, saved keys and downloads stay in place. Updated receivers take effect when you reload them on the console.</p>
    {update.available?.restart && <p className="opt-note">{update.available.restart}</p>}
  </section>
}

/** Mount once with the app; checks continue when Options is closed. */
export function Updater() {
  const update = useUpdater()
  const [dismissed, setDismissed] = useState("")
  useEffect(startUpdater, [])
  const userError = updaterHasUserError()
  const key = `${update.available?.build}:${update.stage}:${userError ? update.message : ""}`
  if (dismissed === key || (!userError && (!update.available || !["available", "downloading", "verifying", "ready", "installing"].includes(update.stage)))) return null
  return <aside className="sspi-update-banner" aria-label="SSPI update" role="status">
    <span>{update.stage === "available" ? `SSPI ${update.available?.version} update available` : update.message}</span>
    {update.stage === "available" && <button type="button" className="btn sm" onClick={() => void runUpdateAction("download_update")}>Download</button>}
    {update.stage === "ready" && <button type="button" className="btn primary sm" onClick={() => void runUpdateAction("install_update")}>Restart and install</button>}
    {update.stage === "error" && userError && <button type="button" className="btn sm" onClick={() => void runUpdateAction(update.available ? "download_update" : "check_for_updates")}>Retry</button>}
    {update.stage === "downloading" && <progress max={update.total || 1} value={update.downloaded} aria-label="Update download" />}
    <button type="button" className="sspi-update-dismiss" aria-label="Dismiss update notification" onClick={() => setDismissed(key)}>×</button>
  </aside>
}
