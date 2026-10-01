/* =====================================================================
   Processes — everything running on the console, with payloads and the
   game picked out, plus which homebrew services answer on the network.
   Apps and elfldr payloads can be stopped (closed) or ended (forced).
   ===================================================================== */
import { useEffect, useMemo, useRef, useState } from "react"
import { Icon } from "../../Icon"
import { toast } from "../../toasts"
import { ExportMenu } from "./ExportMenu"
import { consoleProcesses, consoleProcessControl, probeDebugServices } from "@/lib/console-api"
import type { ConsoleProcess, DebugService, ProcessAction, ProcessList } from "@/lib/console-types"
import { exportName, fmtDuration, processesExport, processKind, type ProcessKind } from "@/lib/diagnostics"
import { errorText, fmtBytes } from "@/lib/format"
import type { ConsoleKind } from "@/types"

type Sort = "memory" | "cpu" | "name" | "pid"
type Props = { target: ConsoleKind; host: string; port: number; demo: boolean }
/** A stop in progress or just finished, per PID; drives the row's animation. */
type Stopping = { action: ProcessAction; phase: "working" | "done" | "leaving" | "failed"; message?: string }
const KIND_LABEL: Record<ProcessKind, string> = { payload: "Payload", game: "Game", system: "System", process: "" }
const SERVICE_KIND: Record<DebugService["kind"], string> = { klog: "Kernel log", ftp: "FTP", debugger: "Debugger", installer: "Installer" }
const DONE_MS = 950, LEAVE_MS = 420, ARM_MS = 3200

const verb = (process: ConsoleProcess, action: ProcessAction) =>
  action === "end" ? { now: "Ending", past: "Ended" } : process.control === "app" ? { now: "Closing", past: "Closed" } : { now: "Stopping", past: "Stopped" }

export function ProcessesView({ target, host, port, demo }: Props) {
  const [list, setList] = useState<ProcessList | null>(null)
  const [services, setServices] = useState<DebugService[] | null>(null)
  const [error, setError] = useState("")
  const [loading, setLoading] = useState(false)
  const [query, setQuery] = useState("")
  const [sort, setSort] = useState<Sort>("memory")
  const [only, setOnly] = useState<"all" | "payloads">("all")
  const [stopping, setStopping] = useState<Record<number, Stopping>>({})
  const [gone, setGone] = useState<Set<number>>(new Set())
  const [armed, setArmed] = useState<number | null>(null)
  const timers = useRef<number[]>([])
  useEffect(() => () => timers.current.forEach(window.clearTimeout), [])
  const later = (ms: number, run: () => void) => { timers.current.push(window.setTimeout(run, ms)) }

  const load = async () => {
    setLoading(true); setError("")
    const [processes, found] = await Promise.allSettled([consoleProcesses({ target, host, port, demo }), probeDebugServices({ target, host, demo })])
    if (processes.status === "fulfilled") { setList(processes.value); setGone(new Set()); setStopping(old => Object.fromEntries(Object.entries(old).filter(([, s]) => s.phase === "working"))) }
    else setError(errorText(processes.reason))
    setServices(found.status === "fulfilled" ? found.value : [])
    setLoading(false)
  }
  useEffect(() => { setList(null); setServices(null); setStopping({}); setGone(new Set()); void load() /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [target, host, port, demo])

  const control = async (process: ConsoleProcess, action: ProcessAction) => {
    if (action === "end" && armed !== process.pid) { setArmed(process.pid); later(ARM_MS, () => setArmed(current => current === process.pid ? null : current)); return }
    setArmed(null)
    const { now, past } = verb(process, action)
    setStopping(old => ({ ...old, [process.pid]: { action, phase: "working", message: `${now}…` } }))
    try {
      const result = await consoleProcessControl({ target, host, port, demo, pid: process.pid, name: process.name, action })
      if (result.exited) {
        // Done: a check settles in, then the row folds away.
        setStopping(old => ({ ...old, [process.pid]: { action, phase: "done", message: past } }))
        later(DONE_MS, () => setStopping(old => ({ ...old, [process.pid]: { action, phase: "leaving", message: past } })))
        later(DONE_MS + LEAVE_MS, () => setGone(old => new Set(old).add(process.pid)))
      } else {
        setStopping(old => ({ ...old, [process.pid]: { action, phase: "failed", message: `Still running after ${(result.waitedMs / 1000).toFixed(0)} s` } }))
        toast({ tone: "warning", title: `${process.name} is still running`, text: action === "end" ? "The console didn't end it. Refresh to check again." : "Use End to force it to stop." })
      }
    } catch (reason) {
      setStopping(old => ({ ...old, [process.pid]: { action, phase: "failed", message: errorText(reason) } }))
    }
  }

  const rows = useMemo(() => {
    const needle = query.trim().toLowerCase()
    const items = (list?.processes || []).filter(process => !gone.has(process.pid)).map(process => ({ process, kind: processKind(process) }))
      .filter(({ process, kind }) => (only === "all" || kind === "payload" || kind === "game") &&
        (!needle || process.name.toLowerCase().includes(needle) || String(process.pid) === needle || process.titleId?.toLowerCase().includes(needle)))
    const by: Record<Sort, (a: ConsoleProcess, b: ConsoleProcess) => number> = {
      memory: (a, b) => b.rssBytes - a.rssBytes, cpu: (a, b) => b.cpuMs - a.cpuMs,
      name: (a, b) => a.name.localeCompare(b.name), pid: (a, b) => a.pid - b.pid,
    }
    return items.sort((a, b) => by[sort](a.process, b.process))
  }, [list, query, sort, only, gone])

  const counts = useMemo(() => {
    const kinds = (list?.processes || []).filter(p => !gone.has(p.pid)).map(processKind)
    return { all: kinds.length, payloads: kinds.filter(k => k === "payload").length, games: kinds.filter(k => k === "game").length }
  }, [list, gone])
  const showAuth = target === "ps5" && (list?.processes.some(p => p.authId) ?? false)
  const now = list?.capturedAt ?? 0
  const stoppable = list?.processes.some(p => p.control) ?? false

  return (
    <div className="pv">
      <div className="pv-services">
        <p className="sys-label">Homebrew services</p>
        <div className="pv-service-list">
          {services == null ? <span className="sys-note">Checking known ports…</span>
            : services.map(service => (
              <span key={service.port} className={`pv-service ${service.open ? "on" : ""}`} title={service.open ? `Answered in ${service.latencyMs ?? "?"} ms` : "No answer"}>
                <span className={`dot ${service.open ? "good" : ""}`} />
                <b>{service.name}</b><em>{SERVICE_KIND[service.kind]} · {service.port}</em>
              </span>
            ))}
        </div>
      </div>

      <div className="kl-bar">
        <div className="kl-source">
          <strong>{list ? `${counts.all} processes` : "Processes"}</strong>
          {list && <span>{counts.payloads} payload{counts.payloads === 1 ? "" : "s"}{counts.games ? `, ${counts.games === 1 ? "a game" : `${counts.games} games`} running` : ""}{list.truncated ? ", list truncated" : ""}</span>}
        </div>
        <div className="kl-actions">
          <label className="field kl-filter"><Icon name="search" /><input value={query} onChange={event => setQuery(event.target.value)} placeholder="Name, PID or title ID" aria-label="Filter processes" /></label>
          <button type="button" className="btn sm" aria-pressed={only === "payloads"} onClick={() => setOnly(value => value === "all" ? "payloads" : "all")}><Icon name="layers" />{only === "all" ? "All processes" : "Payloads and games"}</button>
          <select className="pv-sort" value={sort} onChange={event => setSort(event.target.value as Sort)} aria-label="Sort processes">
            <option value="memory">Sort by memory</option><option value="cpu">Sort by CPU time</option><option value="name">Sort by name</option><option value="pid">Sort by PID</option>
          </select>
          <ExportMenu name={exportName(target, "processes", "txt").replace(/\.txt$/, "")} demo={demo} disabled={!list}
            build={format => processesExport((list?.processes || []).filter(p => !gone.has(p.pid)), format, list?.capturedAt ?? Date.now(), target.toUpperCase())} />
          <button type="button" className="btn sm icon" title="Refresh" aria-label="Refresh processes" disabled={loading} onClick={() => void load()}><Icon name="refresh" /></button>
        </div>
      </div>
      {list && !stoppable && <p className="sys-note pv-note">Stopping processes needs the current receiver. Reload it from Tools &gt; Payloads to stop apps and payloads here.</p>}

      {error && !list ? <div className="empty-state"><Icon name="alert" /><h3>The process list didn't load</h3><p>{error}</p></div>
        : !list ? <div className="skeleton kl-skeleton" />
        : (
          <div className="pv-table" role="table" aria-label="Processes">
            <div className={`pv-row pv-head ${showAuth ? "auth" : ""}`} role="row">
              <span role="columnheader">Name</span><span role="columnheader">PID</span><span role="columnheader">Title</span><span role="columnheader">State</span>
              <span role="columnheader">Memory</span><span role="columnheader">Threads</span><span role="columnheader">CPU time</span><span role="columnheader">Running for</span>
              {showAuth && <span role="columnheader">Auth ID</span>}
              <span role="columnheader" className="pv-actions-head">Actions</span>
            </div>
            {rows.map(({ process, kind }) => {
              const state = stopping[process.pid]
              const busy = state?.phase === "working"
              return (
                <div key={process.pid} className={`pv-row ${kind} ${showAuth ? "auth" : ""} ${state ? `is-${state.phase}` : ""}`} data-action={state?.action} role="row">
                  <span className="pv-name" role="cell"><b title={process.name}>{process.name || "—"}</b>{KIND_LABEL[kind] && <i className={`tag ${kind}`}>{KIND_LABEL[kind]}</i>}</span>
                  <span role="cell">{process.pid}</span>
                  <span role="cell">{process.titleId || "—"}</span>
                  <span role="cell" className={process.state === "zombie" || process.state === "stopped" ? "warn" : ""}>{process.state}</span>
                  <span role="cell">{fmtBytes(process.rssBytes)}</span>
                  <span role="cell">{process.threads}</span>
                  <span role="cell">{fmtDuration(process.cpuMs)}</span>
                  <span role="cell">{process.startedAt && now ? fmtDuration(now - process.startedAt * 1000) : "—"}</span>
                  {showAuth && <span role="cell" className="pv-mono">{process.authId || "—"}</span>}
                  <span role="cell" className="pv-actions">
                    {state && state.phase !== "failed" ? (
                      <span className={`pv-status ${state.phase}`} role="status">
                        {state.phase === "working" ? <span className="pv-spin" /> : <Icon name="check" className="pv-check" />}{state.message}
                      </span>
                    ) : process.control ? (
                      <>
                        <button type="button" className="btn sm pv-stop" disabled={busy} title={process.control === "app" ? "Close it the way the console does" : "Ask the payload to exit"}
                          onClick={() => void control(process, "stop")}><Icon name="square" />{process.control === "app" ? "Close" : "Stop"}</button>
                        <button type="button" className={`btn sm pv-end ${armed === process.pid ? "armed" : ""}`} disabled={busy} title="Force it to end right away"
                          onClick={() => void control(process, "end")}><Icon name="x" />{armed === process.pid ? "Confirm" : "End"}</button>
                        {state?.phase === "failed" && <em className="pv-fail" title={state.message}>{state.message}</em>}
                      </>
                    ) : <span className="pv-locked" title={kind === "system" || kind === "process" ? "System processes can't be stopped from here" : "Not stoppable from here"}>—</span>}
                  </span>
                  {busy && <i className="pv-progress" aria-hidden="true" />}
                </div>
              )
            })}
            {!rows.length && <p className="kl-empty">No processes match.</p>}
          </div>
        )}
    </div>
  )
}
