/* =====================================================================
   Crash timeline — kernel messages, payload logs and crash-report files
   read from the console, as one list: newest first, repeats folded into
   one line with a count. A row opens to the recorded lines and what they
   do and don't show; kernel records link to their line in the kernel log.
   ===================================================================== */
import { useEffect, useMemo, useRef, useState } from "react"
import { Collapse } from "../../Collapse"
import { Seg } from "../../Controls"
import { Icon } from "../../Icon"
import { toast } from "../../toasts"
import { ExportMenu } from "./ExportMenu"
import { consoleKernelLog, consoleLogFiles, consoleReadLog } from "@/lib/console-api"
import {
  buildCrashTimeline, crashTimelineExport, isTimelineIssue, readTimelineHistory, retainTimeline, saveTimelineHistory, selectTimelineLogs,
  timelineCounts, timelineEndpoint, timelineFromHistory, timelineSections, type TimelineCapture, type TimelineEvent, type TimelineHistory, type TimelineRow,
} from "@/lib/crash-timeline"
import { exportName } from "@/lib/diagnostics"
import { errorText } from "@/lib/format"
import type { ConsoleKind } from "@/types"
import "./crash-timeline.css"

type Props = {
  target: ConsoleKind; host: string; port: number; demo: boolean; available?: boolean
  /** Opens the kernel log at this record. */
  onShowInKernelLog?: (event: TimelineEvent) => void
}
const MAX_LOG_BYTES = 64 * 1024
const KIND: Record<TimelineEvent["kind"], string> = { panic: "Panic", crash: "Crash", error: "Error", warning: "Warning", report: "Report", restart: "Restart", activity: "Activity" }
const clock = (at: number) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })

export function CrashTimelineView({ target, host, port, demo, available = true, onShowInKernelLog }: Props) {
  const [capture, setCapture] = useState<TimelineCapture | null>(null)
  const [history, setHistory] = useState<TimelineHistory | null>(null)
  const [storageNote, setStorageNote] = useState("")
  const [loading, setLoading] = useState(false)
  const [filter, setFilter] = useState("")
  const [show, setShow] = useState<"all" | "issues">("all")
  const [progress, setProgress] = useState("")
  const [open, setOpen] = useState("")
  const [coverage, setCoverage] = useState(false)
  const generation = useRef(0)
  const historyRef = useRef<TimelineHistory | null>(null)
  const endpointRef = useRef("")
  const endpointKey = timelineEndpoint(target, host, port, demo)

  const refresh = async () => {
    if (!available) return
    const current = ++generation.current
    setLoading(true); setProgress("Reading the kernel log and file list…")
    const endpoint = { target, host, port, demo }
    const next: TimelineCapture = { capturedAt: Date.now(), kernel: null, files: null, logs: [], unavailable: [], skippedLogs: 0 }
    const [kernel, files] = await Promise.allSettled([consoleKernelLog(endpoint), consoleLogFiles(endpoint)])
    if (generation.current !== current) return
    if (kernel.status === "fulfilled") next.kernel = kernel.value
    else next.unavailable.push({ source: "Kernel log", reason: errorText(kernel.reason) })
    if (files.status === "fulfilled") {
      next.files = files.value
      const selected = selectTimelineLogs(files.value.files)
      next.skippedLogs = files.value.files.filter(file => file.kind === "log" && file.size > 0).length - selected.length
      // Read sequentially: the small console receiver also handles ongoing transfers.
      for (const [index, file] of selected.entries()) {
        if (generation.current !== current) return
        setProgress(`Reading log ${index + 1} of ${selected.length}…`)
        try { next.logs.push(await consoleReadLog({ ...endpoint, path: file.path, maxBytes: MAX_LOG_BYTES })) }
        catch (reason) { next.unavailable.push({ source: file.path, reason: errorText(reason) }) }
      }
    } else next.unavailable.push({ source: "Log files", reason: errorText(files.reason) })
    if (generation.current !== current) return
    const saved = retainTimeline(historyRef.current, buildCrashTimeline(next), next.capturedAt)
    historyRef.current = saved
    setCapture(next); setHistory(saved); setLoading(false); setProgress("")
    try { saveTimelineHistory(localStorage, endpointKey, saved); setStorageNote("") }
    catch { setStorageNote("New records are shown here but couldn't be saved on this PC. Export them before closing SSPI.") }
  }

  useEffect(() => {
    setCapture(null); setFilter(""); setShow("all"); setStorageNote(""); setOpen("")
    if (endpointRef.current !== endpointKey) {
      historyRef.current = null
      try { historyRef.current = readTimelineHistory(localStorage, endpointKey) }
      catch { setStorageNote("Saved records couldn't be read. A new read still works.") }
      endpointRef.current = endpointKey
    }
    setHistory(historyRef.current)
    if (available) void refresh()
    else { setLoading(false); setProgress("") }
    return () => { generation.current++ }
    // Endpoint changes start a new collection; late results never cross consoles.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, host, port, demo, available])

  const timeline = useMemo(() => history ? timelineFromHistory(history) : null, [history])
  const counts = useMemo(() => timelineCounts(history?.events || []), [history])
  const sections = useMemo(() => {
    if (!timeline) return []
    const needle = filter.trim().toLowerCase()
    return timelineSections(timeline, event => (show === "all" || isTimelineIssue(event)) &&
      (!needle || `${event.summary} ${event.excerpt} ${event.detail} ${event.source}`.toLowerCase().includes(needle)))
  }, [timeline, filter, show])
  const shownRows = sections.reduce((total, section) => total + section.rows.length, 0)
  const name = target.toUpperCase()

  const copy = async (row: TimelineRow) => {
    try { await navigator.clipboard.writeText(row.event.detail); toast({ tone: "success", title: "Record copied", text: `${row.event.source}${row.event.line ? `, line ${row.event.line}` : ""}` }) }
    catch (reason) { toast({ tone: "error", title: "The record wasn't copied", text: errorText(reason) }) }
  }

  return (
    <div className="ct">
      <div className="kl-bar">
        <div className="kl-source">
          <span className={`dot ${loading ? "act" : history ? "good" : ""}`} />
          <strong>Crash timeline</strong>
          <span className="ct-state" role="status" aria-live="polite">
            {loading ? progress : history ? `${history.events.length} records · checked ${clock(history.capturedAt)}${demo ? " · preview" : ""}` : ""}
          </span>
        </div>
        <div className="kl-actions">
          <label className="field kl-filter"><Icon name="search" /><input value={filter} onChange={event => setFilter(event.target.value)} placeholder="Filter records" aria-label="Filter the crash timeline" /></label>
          <Seg label="Records shown" value={show} options={[["all", "All"], ["issues", "Issues"]]} onChange={setShow} />
          <ExportMenu name={exportName(target, "crash-timeline", "txt").replace(/\.txt$/, "")} demo={demo} disabled={!timeline || !history}
            build={format => timeline && history ? crashTimelineExport(timeline, history, target, format) : ""} />
          <button type="button" className="btn sm icon" title="Read the console again" aria-label="Read the console again" disabled={loading || !available} onClick={() => void refresh()}>
            {loading ? <span className="spinner" /> : <Icon name="refresh" />}
          </button>
        </div>
      </div>

      {!available && <p className="kl-note"><Icon name="info" />The receiver isn't answering, so these are the records saved from earlier reads.</p>}
      {storageNote && <p className="kl-note warn" role="alert"><Icon name="warn" />{storageNote}</p>}

      {history && timeline && (
        <div className="kl-summary ct-summary">
          <span className={counts.panics ? "fail" : ""}><b>{counts.panics}</b> kernel panic{counts.panics === 1 ? "" : "s"}</span>
          <span className={counts.crashes ? "fail" : ""}><b>{counts.crashes}</b> crash{counts.crashes === 1 ? "" : "es"}</span>
          <span className={counts.errors ? "warn" : ""}><b>{counts.errors}</b> error{counts.errors === 1 ? "" : "s"}</span>
          <span className={counts.reports ? "warn" : ""}><b>{counts.reports}</b> crash report{counts.reports === 1 ? "" : "s"}</span>
          <span><b>{counts.restarts}</b> restart{counts.restarts === 1 ? "" : "s"}</span>
          <button type="button" className="link ct-cov-toggle" aria-expanded={coverage} onClick={() => setCoverage(value => !value)}>
            <Icon name="info" />What was read{timeline.coverage.length ? ` · ${timeline.coverage.length} note${timeline.coverage.length === 1 ? "" : "s"}` : ""}<Icon name="chevD" />
          </button>
        </div>
      )}
      {timeline && (
        <Collapse open={coverage} className="ct-coverage-wrap">
          <div className="ct-coverage">
            <p>{capture ? `${capture.kernel ? "Kernel log snapshot" : "No kernel log"} · ${capture.logs.length} text log${capture.logs.length === 1 ? "" : "s"} · ${capture.files ? `${capture.files.files.filter(file => file.kind === "crash").length} crash-report files listed` : "no file list"}.` : "Records saved from an earlier read of this console."}</p>
            {timeline.coverage.length > 0 && <ul>{timeline.coverage.map(note => <li key={note}>{note}</li>)}</ul>}
            <p className="ct-fine">Up to 200 records are kept for each of the last four console addresses. Repeated messages are folded into one line with a count; that count is not a number of crashes. A missing message doesn't prove nothing happened.</p>
          </div>
        </Collapse>
      )}

      {!history || !timeline ? (loading
        ? <div className="ct-skeleton">{Array.from({ length: 7 }, (_, n) => <div key={n} className="skeleton" style={{ animationDelay: `${n * 80}ms` }} />)}</div>
        : <div className="empty-state"><Icon name="rows" /><h3>No timeline for this {name} yet</h3><p>Load a receiver with diagnostics to read its kernel log, payload logs and crash reports.</p></div>
      ) : (
        <div className="ct-scroll scroll">
          {sections.map(section => (
            <section key={section.key} className="ct-section">
              <header className="ct-section-head"><strong>{section.title}</strong><span>{section.subtitle}</span></header>
              <ol className="ct-rows">
                {section.rows.map((row, n) => (
                  <Row key={row.key} row={row} index={n} open={open === row.key} dated={section.key.startsWith("day:")}
                    onToggle={() => setOpen(value => value === row.key ? "" : row.key)} onCopy={() => void copy(row)}
                    onShow={onShowInKernelLog && row.event.source === "Kernel log" ? () => onShowInKernelLog(row.event) : undefined} />
                ))}
              </ol>
            </section>
          ))}
          {!shownRows && (
            <div className="empty-state">
              <Icon name={history.events.length ? "search" : "checkCircle"} />
              <h3>{history.events.length ? "Nothing matches" : "No crashes or failures recorded"}</h3>
              <p>{history.events.length ? "Change the filter or show all records." : "The logs read from this console have no panics, failures, restarts or crash reports. Open What was read for what was covered."}</p>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

function Row({ row, index, open, dated, onToggle, onCopy, onShow }: {
  row: TimelineRow; index: number; open: boolean; dated: boolean; onToggle: () => void; onCopy: () => void; onShow?: () => void
}) {
  const { event } = row
  const when = dated && event.timestamp != null ? new Date(event.timestamp).toLocaleTimeString([], { hour: "numeric", minute: "2-digit", second: "2-digit" })
    : event.line ? `line ${event.line}` : "—"
  const lines = row.lines && row.lines[0] !== row.lines[1] ? `lines ${row.lines[0]}–${row.lines[1]}` : row.lines ? `line ${row.lines[0]}` : ""
  return (
    <li className={`ct-row kind-${event.kind} ${open ? "is-open" : ""}`} style={{ animationDelay: `${Math.min(index, 14) * 18}ms` }}>
      <button type="button" className="ct-head" aria-expanded={open} onClick={onToggle}>
        <span className="ct-time">{when}</span>
        <span className="ct-kind">{KIND[event.kind]}</span>
        <span className="ct-text">{event.excerpt}</span>
        {row.count > 1 && <span className="ct-count" title={`${row.count} similar records`}>×{row.count}</span>}
        <Icon name="chevD" />
      </button>
      <Collapse open={open} className="ct-drawer">
        <div className="ct-detail">
          <pre>{event.detail}</pre>
          <p className="ct-why">{event.interpretation}</p>
          <div className="ct-foot">
            <span className="ct-meta">
              {event.source}{lines ? ` · ${lines}` : ""}{row.count > 1 ? ` · ${row.count} similar records, latest shown` : ""}
              {event.timeBasis === "file" ? " · file modified time" : event.timeBasis === "local" ? " · time zone unknown" : ""}
            </span>
            <span className="ct-actions">
              {onShow && <button type="button" className="btn sm" onClick={onShow}><Icon name="rows" />Show in kernel log</button>}
              <button type="button" className="btn sm ghost" onClick={onCopy}><Icon name="copy" />Copy</button>
            </span>
          </div>
        </div>
      </Collapse>
    </li>
  )
}
