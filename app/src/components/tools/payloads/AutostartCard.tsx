/* The autostart order at a glance, drawn like a download's stage track: a start
   marker (a confirmed wake or Run now), then each payload in order with the wait
   before it on the link that leads to it. During a run the link to the next
   payload fills as its wait elapses, and each payload says what happened to it. */
import { useEffect, useState, type CSSProperties } from "react"
import { Switch } from "../../Controls"
import { CheckDraw, Icon } from "../../Icon"
import type { PayloadEntry } from "@/lib/console-types"
import { autostartRunning, type AutostartStatus, type AutostartStep } from "@/lib/launcher-api"

const seconds = (ms: number) => `${Math.round(ms / 100) / 10}`.replace(/\.0$/, "")

function useNow(active: boolean, target?: number | null) {
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    if (!active) return
    setNow(Date.now())
    const timer = window.setInterval(() => setNow(Date.now()), 250)
    return () => clearInterval(timer)
  }, [active, target])
  return now
}

type Badge = { text: string; tone: "" | "live" | "good" | "warn" | "fail" }

export function AutostartCard({ status, payloads, loader, busy, available, onToggle, onEdit, onRun, onStop }: {
  status?: AutostartStatus; payloads: PayloadEntry[]; loader: string; busy: boolean; available: boolean
  onToggle: (on: boolean) => void; onEdit: () => void; onRun: () => void; onStop: () => void
}) {
  const steps = status?.steps || []
  const on = !!status?.enabled && steps.length > 0
  const running = autostartRunning(status)
  const now = useNow(running && status?.nextAttemptAt != null, status?.nextAttemptAt)
  const nameOf = (step?: AutostartStep) => step ? payloads.find(entry => entry.id === step.payloadId)?.name || "Missing payload" : ""
  const current = status?.current != null ? steps[status.current] : undefined
  const wait = status?.nextAttemptAt ? Math.max(0, Math.ceil((status.nextAttemptAt - now) / 1000)) : 0
  const sent = steps.filter(step => step.state === "sent" || step.state === "verified").length
  const total = steps.reduce((sum, step) => sum + step.delayMs, 0)

  const badge: Badge = !steps.length ? { text: "Not set up", tone: "" }
    : !status?.enabled ? { text: "Off", tone: "" }
    : status.phase === "waiting" || status.phase === "sending" ? { text: "Running", tone: "live" }
    : status.phase === "unavailable" ? { text: "Waiting for loader", tone: "warn" }
    : status.phase === "done" ? { text: "Done", tone: "good" }
    : status.phase === "failed" ? { text: "Stopped", tone: "fail" }
    : status.phase === "stopped" ? { text: "Stopped", tone: "" }
    : { text: "On", tone: "good" }

  const summary = !status || !available || !steps.length ? "Send payloads in order after a confirmed console wake, or with Run now. Payloads that are already running are skipped."
    : !status.enabled ? `${steps.length} payload${steps.length === 1 ? "" : "s"} saved. Turn autostart on to run them after a confirmed wake.`
    : status.phase === "waiting" ? (sent === 0 && status.attempts === 0 ? `Starting in ${wait} s` : `${nameOf(current)} in ${wait} s`)
    : status.phase === "unavailable" ? `Waiting for the ${loader}. Retry ${status.attempts + 1} of 3 in ${wait} s.`
    : status.phase === "sending" ? `Checking and starting ${nameOf(current)}, ${(status.current ?? 0) + 1} of ${steps.length}`
    : status.phase === "done" ? "All done. Payloads that were already running kept running."
    : status.phase === "stopped" ? `Stopped after ${sent} of ${steps.length}.`
    : status.phase === "failed" ? `Stopped at ${nameOf(steps.find(step => step.state === "failed"))}.`
    : `${steps.length} payload${steps.length === 1 ? "" : "s"} over about ${seconds(total)} s after a confirmed wake.`

  /** How far the wait before step `index` has run, 0 to 1, while that step is the one being waited on. */
  const waited = (index: number) => {
    const step = steps[index]
    if (!running || status?.current !== index || step?.state !== "waiting" || !status.nextAttemptAt || step.delayMs <= 0) return null
    return Math.min(1, Math.max(0, 1 - (status.nextAttemptAt - now) / step.delayMs))
  }
  const caption = (step: AutostartStep, index: number) => {
    if (!on) return ""
    switch (step.state) {
      case "waiting": return running && status?.current === index ? (wait > 0 ? `In ${wait} s` : "Next") : "Queued"
      case "sending": return "Starting"
      case "sent": return "Started"
      case "verified": return "Running"
      case "skipped": return /running/i.test(step.message) ? "Already running" : "Skipped"
      case "failed": return "Failed"
      default: return running ? "Queued" : ""
    }
  }
  const startState = running || ["done", "stopped", "failed"].includes(status?.phase || "") ? "done" : "idle"

  return (
    <section className={`as-card ${on ? "is-on" : ""} phase-${status?.phase || "off"}`} aria-label="Autostart">
      <div className="as-card-top">
        <span className="as-card-ico"><Icon name="bolt" />{running && <i className="as-card-pulse" />}</span>
        <div className="as-card-text">
          <span className="as-card-title"><strong>Autostart</strong><em className={`as-badge ${badge.tone}`}>{badge.text}</em></span>
          <span className="swap-fade" key={summary.replace(/\d+ s\.?$/, "")} role="status">{summary}</span>
        </div>
        <div className="as-card-actions">
          {running && <button type="button" className="btn sm ghost" onClick={onStop}><Icon name="square" />Stop</button>}
          {!running && on && <button type="button" className="btn sm ghost" disabled={busy || !available} onClick={onRun}><Icon name="play" />Run now</button>}
          <button type="button" className="btn sm" disabled={busy || !available} onClick={onEdit}><Icon name={steps.length ? "pencil" : "plus"} />{steps.length ? "Edit order" : "Choose payloads"}</button>
          <Switch label="Autostart" checked={on} disabled={busy || !available} onChange={onToggle} />
        </div>
      </div>
      {steps.length > 0 && (
        <ol className={`as-track ${on ? "" : "is-off"}`} style={{ "--as-cols": steps.length + 1 } as CSSProperties} aria-label="Autostart order">
          <li className="as-step as-start" data-state={startState}>
            <span className="as-node"><Icon name="bolt" /></span>
            <span className="as-step-name">Start</span>
            <small>Wake or Run now</small>
          </li>
          {steps.map((step, index) => {
            const fill = waited(index)
            const state = on ? step.state : "pending"
            const linkDone = on && step.state !== "pending" && step.state !== "waiting"
            return (
              <li key={step.payloadId} className="as-step" data-state={state} title={step.message || undefined}>
                <span className={`as-link ${linkDone ? "is-done" : ""} ${fill != null ? "is-live" : ""}`} aria-hidden="true">
                  <i style={fill != null ? { width: `${fill * 100}%` } : undefined} />
                  {step.delayMs > 0 && <em>{seconds(step.delayMs)} s</em>}
                </span>
                <span className="as-node">
                  {state === "sending" ? <span className="spinner" /> : state === "sent" || state === "verified" ? <CheckDraw on /> : state === "failed" ? <Icon name="x" /> : state === "skipped" ? <Icon name="minus" /> : index + 1}
                </span>
                <span className="as-step-name">{nameOf(step)}</span>
                <small>{caption(step, index)}</small>
              </li>
            )
          })}
        </ol>
      )}
      {on && (status?.phase === "failed" || status?.phase === "unavailable") && status.message && <p className={`as-card-note ${status.phase === "failed" ? "fail" : ""}`}>{status.message}</p>}
    </section>
  )
}
