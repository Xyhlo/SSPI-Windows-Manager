import { useEffect, useMemo, useRef, useState } from "react"
import { Icon } from "../../Icon"
import { ExportMenu } from "./ExportMenu"
import { consoleKernelLog, consoleLogFiles, consoleReadLog } from "@/lib/console-api"
import { buildCrashTimeline, crashTimelineExport, readTimelineHistory, retainTimeline, saveTimelineHistory, selectTimelineLogs, timelineEndpoint, timelineFromHistory, type TimelineCapture, type TimelineEvent, type TimelineHistory } from "@/lib/crash-timeline"
import { exportName } from "@/lib/diagnostics"
import { errorText } from "@/lib/format"
import type { ConsoleKind } from "@/types"
import "./crash-timeline.css"

type Props = { target: ConsoleKind; host: string; port: number; demo: boolean; available?: boolean }
const MAX_LOG_BYTES = 64 * 1024

export function CrashTimelineView({ target, host, port, demo, available = true }: Props) {
  const [capture, setCapture] = useState<TimelineCapture | null>(null)
  const [history, setHistory] = useState<TimelineHistory | null>(null)
  const [storageNote, setStorageNote] = useState("")
  const [loading, setLoading] = useState(false)
  const [filter, setFilter] = useState("")
  const [issuesOnly, setIssuesOnly] = useState(false)
  const [progress, setProgress] = useState("")
  const generation = useRef(0)
  const historyRef = useRef<TimelineHistory | null>(null)
  const endpointRef = useRef("")
  const endpointKey = timelineEndpoint(target, host, port, demo)

  const refresh = async () => {
    if (!available) return
    const current = ++generation.current
    setLoading(true); setProgress("Reading kernel and file list…")
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
    catch { setStorageNote("New evidence is available in this view, but could not be saved on this PC. Export it before closing the app.") }
  }

  useEffect(() => {
    setCapture(null); setFilter(""); setIssuesOnly(false); setStorageNote("")
    if (endpointRef.current !== endpointKey) {
      historyRef.current = null
      try { historyRef.current = readTimelineHistory(localStorage, endpointKey) }
      catch { setStorageNote("Saved evidence could not be read. A new snapshot can still be collected.") }
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
  const matches = (event: TimelineEvent) => {
    if (issuesOnly && !["panic", "crash", "error", "report"].includes(event.kind)) return false
    const needle = filter.trim().toLowerCase()
    return !needle || `${event.summary} ${event.excerpt} ${event.detail} ${event.source}`.toLowerCase().includes(needle)
  }
  const dated = timeline?.dated.filter(matches) || []
  const undated = timeline?.undated.map(group => ({ ...group, events: group.events.filter(matches) })).filter(group => group.events.length) || []
  const visibleCount = dated.length + undated.reduce((total, group) => total + group.events.length, 0)

  return <div className="crash-timeline">
    <div className="timeline-head">
      <div>
        <h3>Crash timeline</h3>
        <p>Kernel messages, payload logs and crash-report files from your {target.toUpperCase()}.</p>
      </div>
      <div className="timeline-actions">
        <ExportMenu name={exportName(target, "crash-timeline", "txt").replace(/\.txt$/, "")} demo={demo} disabled={!timeline || !history}
          build={format => timeline && history ? crashTimelineExport(timeline, history, target, format) : ""} />
        <button type="button" className="btn sm" disabled={loading || !available} onClick={() => void refresh()}>
          {loading ? <span className="spinner" /> : <Icon name="refresh" />}Refresh
        </button>
      </div>
    </div>
    <div className="timeline-state" role="status" aria-live="polite">
      {loading ? `${progress}${history ? " · showing retained evidence" : ""}` : history && timeline ? `${timeline.eventCount} retained records · last checked ${new Date(history.capturedAt).toLocaleString()}${demo ? " · preview data" : ""}` : ""}
    </div>
    {!available && <p className="timeline-offline"><Icon name="info" />The receiver is unavailable for diagnostics. Previously saved evidence remains below.</p>}
    {storageNote && <p className="timeline-offline" role="alert"><Icon name="warn" />{storageNote}</p>}
    {!history || !timeline ? loading ? <div className="skeleton kl-skeleton" /> : <div className="empty-state"><Icon name="rows" /><h3>No saved timeline for this console</h3><p>Load a receiver with diagnostics support to collect its available records.</p></div> : <>
      <div className="timeline-filters">
        <label className="field"><Icon name="search" /><input value={filter} onChange={event => setFilter(event.target.value)} placeholder="Filter timeline" aria-label="Filter crash timeline" /></label>
        <button type="button" className="btn sm" aria-pressed={issuesOnly} onClick={() => setIssuesOnly(value => !value)}><Icon name="alert" />{issuesOnly ? "Failures & reports" : "All events"}</button>
        <span>{timeline.issueCount} failure message{timeline.issueCount === 1 ? "" : "s"} · {timeline.reportCount} report file{timeline.reportCount === 1 ? "" : "s"}</span>
      </div>
      <div className="timeline-scroll">
        <details className="timeline-coverage" open={timeline.eventCount === 0 && timeline.coverage.length > 0 ? true : undefined}>
          <summary><Icon name={timeline.coverage.length ? "info" : "checkCircle"} /><span>{timeline.coverage.length ? `Coverage & missing information · ${timeline.coverage.length} notes` : "Sources read"}</span><Icon name="chevD" /></summary>
          <div>
            <p>{capture ? `${capture.kernel ? "Kernel snapshot" : "No kernel snapshot"} · ${capture.logs.length} text log${capture.logs.length === 1 ? "" : "s"} · ${capture.files ? `${capture.files.files.filter(file => file.kind === "crash").length} crash-report file records` : "No file list"}.` : "Showing evidence saved by an earlier read of this console address."}</p>
            {timeline.coverage.length > 0 && <ul>{timeline.coverage.map(note => <li key={note}>{note}</li>)}</ul>}
            <p>Up to 200 records are retained for each of the four most recently checked console addresses, within a storage size limit. Binary dumps are listed, not decoded. A missing message is not proof that an event did not happen.</p>
          </div>
        </details>
        {dated.length > 0 && <section className="timeline-section" aria-label="Dated events">
          <div className="timeline-section-title"><h4>Dated records</h4><span>Oldest first · your local time</span></div>
          <p className="timeline-note">Log times and file modification times are labelled separately. Console and payload clocks may differ.</p>
          <ol className="timeline-events">{dated.map(event => <Event key={event.id} event={event} checkedAt={history.capturedAt} />)}</ol>
        </section>}
        {undated.length > 0 && <section className="timeline-section" aria-label="Events without a comparable timestamp">
          <div className="timeline-section-title"><h4>Time not comparable</h4><span>Source order within each first capture</span></div>
          <p className="timeline-note">These messages cannot be placed reliably between the dated records.</p>
          {undated.map(group => <div className="timeline-source" key={group.source}>
            <h5 title={group.source}>{group.source}</h5>
            <ol className="timeline-events">{group.events.map(event => <Event key={event.id} event={event} checkedAt={history.capturedAt} />)}</ol>
          </div>)}
        </section>}
        {!visibleCount && <div className="empty-state"><Icon name={timeline.eventCount ? "search" : "rows"} />
          <h3>{timeline.eventCount ? "No matching events" : "No matching crash or activity records"}</h3>
          <p>{timeline.eventCount ? "Change the filter to see the other recorded events." : "The captured sources contain no recognised panic, failure, restart or activity messages. Check the coverage above and the raw logs for more context."}</p>
        </div>}
      </div>
    </>}
  </div>
}

function Event({ event, checkedAt }: { event: TimelineEvent; checkedAt: number }) {
  const time = event.timestamp == null ? event.timeLabel : new Date(event.timestamp).toLocaleString()
  const icon = event.kind === "panic" || event.kind === "crash" || event.kind === "error" ? "alert" : event.kind === "warning" || event.kind === "report" ? "warn" : "rows"
  return <li className={`timeline-event kind-${event.kind}`}>
    <span className="timeline-marker"><Icon name={icon} /></span>
    <details>
      <summary>
        <span className="timeline-event-time">{time}{event.timeBasis === "file" && <em>File modified</em>}{event.timeBasis === "log" && <em>Log timestamp</em>}</span>
        <span className="timeline-event-heading"><strong>{event.summary}</strong><span className="timeline-recorded">{event.lastObservedAt && event.lastObservedAt < checkedAt ? "Earlier capture" : "Recorded"}</span></span>
        <span className="timeline-excerpt">{event.excerpt}</span>
        <span className="timeline-origin">{event.source}{event.line ? ` · line ${event.line}` : ""}</span>
        <Icon name="chevD" />
      </summary>
      <div className="timeline-detail">
        <p className="timeline-detail-label">Recorded evidence</p>
        <pre>{event.detail}</pre>
        <p className="timeline-detail-label">Interpretation</p>
        <p>{event.interpretation}</p>
        {event.firstObservedAt && <p className="timeline-observed">First observed by SSPI: {new Date(event.firstObservedAt).toLocaleString()}<br />Last observed: {new Date(event.lastObservedAt!).toLocaleString()}. These are read times, not event times.</p>}
      </div>
    </details>
  </li>
}
