/* The autostart order at a glance: on/off, the chain of payloads with their waits,
   and the run as it happens — which payload is next, sending, sent or skipped. */
import { useEffect, useState } from "react"
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

  const summary = !status || !available ? "Send payloads in your chosen order whenever SSPI starts."
    : !steps.length ? "Send payloads in your chosen order whenever SSPI starts."
    : !status.enabled ? `${steps.length} payload${steps.length === 1 ? "" : "s"} saved · off`
    : status.phase === "ready" ? `${steps.length} payload${steps.length === 1 ? "" : "s"} · runs when SSPI starts`
    : status.phase === "waiting" ? (sent === 0 && status.attempts === 0 ? `Starting in ${wait} s` : `${nameOf(current)} in ${wait} s`)
    : status.phase === "unavailable" ? `Waiting for the ${loader} · retry ${status.attempts + 1} of 3 in ${wait} s`
    : status.phase === "sending" ? `Sending ${nameOf(current)} · ${(status.current ?? 0) + 1} of ${steps.length}`
    : status.phase === "done" ? `All ${steps.length} sent${status.lastAttemptAt ? ` at ${new Date(status.lastAttemptAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}` : ""}`
    : status.phase === "stopped" ? `Stopped after ${sent} of ${steps.length}`
    : status.phase === "failed" ? `Stopped at ${nameOf(steps.find(step => step.state === "failed"))}` : ""

  return (
    <section className={`as-card ${on ? "is-on" : ""} phase-${status?.phase || "off"}`} aria-label="Autostart">
      <div className="as-card-top">
        <span className="as-card-ico"><Icon name="bolt" />{running && <i className="as-card-pulse" />}</span>
        <div className="as-card-text">
          <strong>Autostart</strong>
          <span className="swap-fade" key={summary.replace(/\d+ s$/, "")} role="status">{summary}</span>
        </div>
        <div className="as-card-actions">
          {running && <button type="button" className="btn sm ghost" onClick={onStop}><Icon name="square" />Stop</button>}
          {!running && on && <button type="button" className="btn sm ghost" disabled={busy || !available} onClick={onRun}><Icon name="play" />Run now</button>}
          <button type="button" className="btn sm" disabled={busy || !available} onClick={onEdit}><Icon name="rows" />{steps.length ? "Edit order" : "Choose payloads"}</button>
          <Switch label="Autostart" checked={on} disabled={busy || !available} onChange={onToggle} />
        </div>
      </div>
      {steps.length > 0 && (
        <ol className={`as-chain ${on ? "" : "is-off"}`} aria-label="Autostart order">
          {steps.map((step, index) => (
            <li key={step.payloadId} className="as-link">
              {(index > 0 || step.delayMs > 0) && <span className="as-wire" data-state={step.state}><i />{step.delayMs > 0 && <em>{seconds(step.delayMs)} s</em>}</span>}
              <span className="as-chip" data-state={on ? step.state : "pending"} title={step.message || undefined}>
                <span className="as-chip-mark">
                  {step.state === "sending" ? <span className="spinner" /> : step.state === "sent" || step.state === "verified" ? <CheckDraw on /> : step.state === "failed" ? <Icon name="x" /> : index + 1}
                </span>
                <span className="as-chip-name">{nameOf(step)}</span>
              </span>
            </li>
          ))}
        </ol>
      )}
      {on && (status?.phase === "failed" || status?.phase === "unavailable") && status.message && <p className={`as-card-note ${status.phase === "failed" ? "fail" : ""}`}>{status.message}</p>}
    </section>
  )
}
