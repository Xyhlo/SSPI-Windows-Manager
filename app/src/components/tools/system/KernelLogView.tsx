/* =====================================================================
   Kernel log — the receiver's snapshot of the kernel message buffer, or
   a live relay from a klog server another payload runs (GoldHEN, etaHEN,
   klogsrv). Panics and faults are pulled out and pinned above the log.
   ===================================================================== */
import { useEffect, useMemo, useRef, useState } from "react"
import { Icon } from "../../Icon"
import { consoleKernelLog, onKlogStream, probeDebugServices, startKlogStream, stopKlogStream } from "@/lib/console-api"
import type { DebugService, KernelLog } from "@/lib/console-types"
import { analyzeKernelLog, exportName, textLogExport, type LogFinding } from "@/lib/diagnostics"
import { errorText, fmtBytes } from "@/lib/format"
import { ExportMenu } from "./ExportMenu"
import type { ConsoleKind } from "@/types"

const TEXT_LIMIT = 1_000_000
const SHOWN_LINES = 3000

type Props = { target: ConsoleKind; host: string; port: number; demo: boolean }

export function KernelLogView({ target, host, port, demo }: Props) {
  const [snapshot, setSnapshot] = useState<KernelLog | null>(null)
  const [text, setText] = useState("")
  const [error, setError] = useState("")
  const [loading, setLoading] = useState(false)
  const [services, setServices] = useState<DebugService[]>([])
  const [live, setLive] = useState<{ id: number; service: DebugService } | null>(null)
  const [liveNote, setLiveNote] = useState("")
  const [filter, setFilter] = useState("")
  const [issuesOnly, setIssuesOnly] = useState(false)
  const [follow, setFollow] = useState(true)
  const [copied, setCopied] = useState(false)
  const body = useRef<HTMLDivElement>(null)
  const liveRef = useRef<number | null>(null)

  const loadSnapshot = async () => {
    setLoading(true); setError("")
    try {
      const log = await consoleKernelLog({ target, host, port, demo })
      setSnapshot(log)
      if (liveRef.current == null) setText(log.text)
    } catch (reason) { setError(errorText(reason)) }
    finally { setLoading(false) }
  }

  useEffect(() => {
    setSnapshot(null); setText(""); setServices([])
    void loadSnapshot()
    probeDebugServices({ target, host, demo }).then(setServices).catch(() => setServices([]))
    return () => { if (liveRef.current != null) void stopKlogStream({ id: liveRef.current, demo }); liveRef.current = null; setLive(null) }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, host, port, demo])

  // Live chunks arrive as events; only the current stream's are kept.
  useEffect(() => {
    let stop: (() => void) | null = null, closed = false
    void onKlogStream(demo, event => {
      if (event.id !== liveRef.current) return
      if (event.text) setText(old => { const next = old + event.text; return next.length > TEXT_LIMIT ? next.slice(next.length - TEXT_LIMIT) : next })
      if (event.closed) { liveRef.current = null; setLive(null); setLiveNote(event.error || "The kernel log server closed the connection.") }
    }).then(unlisten => { if (closed) unlisten(); else stop = unlisten })
    return () => { closed = true; stop?.() }
  }, [demo])

  const klogServers = services.filter(service => service.kind === "klog" && service.open)

  const goLive = async (service: DebugService) => {
    setLiveNote(""); setError("")
    try {
      const id = await startKlogStream({ target, host, port: service.port, demo })
      liveRef.current = id; setLive({ id, service }); setFollow(true)
      setText(old => `${old}${old && !old.endsWith("\n") ? "\n" : ""}── live from ${service.name}, port ${service.port} ──\n`)
    } catch (reason) { setLiveNote(errorText(reason)) }
  }
  const stopLive = async () => {
    const id = liveRef.current; liveRef.current = null; setLive(null)
    if (id != null) await stopKlogStream({ id, demo }).catch(() => false)
  }

  const analysis = useMemo(() => analyzeKernelLog(text), [text])
  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase()
    const issueLines = issuesOnly ? new Set(analysis.findings.flatMap(f => [f.line, ...f.context.map((_, i) => f.line + 1 + i)])) : null
    const rows: Array<{ index: number; text: string; severity: string }> = []
    const severities = new Map(analysis.findings.map(f => [f.line, f.severity]))
    for (let i = 0; i < analysis.lines.length; i++) {
      if (issueLines && !issueLines.has(i)) continue
      if (needle && !analysis.lines[i].toLowerCase().includes(needle)) continue
      rows.push({ index: i, text: analysis.lines[i], severity: severities.get(i) || "" })
    }
    return rows.length > SHOWN_LINES ? { rows: rows.slice(rows.length - SHOWN_LINES), hidden: rows.length - SHOWN_LINES } : { rows, hidden: 0 }
  }, [analysis, filter, issuesOnly])

  useEffect(() => {
    if (follow && body.current) body.current.scrollTop = body.current.scrollHeight
  }, [visible, follow])

  const jump = (finding: LogFinding) => {
    setFilter(""); setIssuesOnly(false); setFollow(false)
    requestAnimationFrame(() => body.current?.querySelector<HTMLElement>(`[data-line="${finding.line}"]`)?.scrollIntoView({ block: "center" }))
  }
  const copy = async () => {
    try { await navigator.clipboard.writeText(text); setCopied(true); window.setTimeout(() => setCopied(false), 1400) } catch { setError("Copying to the clipboard failed.") }
  }

  const panics = analysis.findings.filter(f => f.severity === "panic")
  const source = live ? `Live from ${live.service.name} (port ${live.service.port})`
    : snapshot?.source === "msgbuf" ? "Kernel message buffer snapshot"
    : snapshot ? "Read from /dev/klog by the receiver" : "Kernel log"

  return (
    <div className="kl">
      <div className="kl-bar">
        <div className="kl-source">
          <span className={`dot ${live ? "good" : snapshot ? "act" : ""}`} />
          <strong>{source}</strong>
          {snapshot && !live && <span>{fmtBytes(snapshot.bytes)}{snapshot.dropped ? ", oldest lines dropped" : ""}</span>}
        </div>
        <div className="kl-actions">
          <label className="field kl-filter"><Icon name="search" /><input value={filter} onChange={event => setFilter(event.target.value)} placeholder="Filter lines" aria-label="Filter kernel log lines" /></label>
          <button type="button" className="btn sm" aria-pressed={issuesOnly} onClick={() => setIssuesOnly(value => !value)}><Icon name="alert" />{issuesOnly ? "Issues only" : "All lines"}</button>
          <button type="button" className="btn sm" aria-pressed={follow} onClick={() => setFollow(value => !value)}><Icon name="chevD" />{follow ? "Following" : "Follow"}</button>
          {live
            ? <button type="button" className="btn sm ok" onClick={() => void stopLive()}><Icon name="pause" />Stop live</button>
            : klogServers.map(service => <button key={service.port} type="button" className="btn sm" onClick={() => void goLive(service)}><Icon name="signal" />Live: {service.name}</button>)}
          <button type="button" className="btn sm icon" title="Take a new snapshot" aria-label="Take a new snapshot" disabled={loading || !!live} onClick={() => void loadSnapshot()}><Icon name="refresh" /></button>
          <ExportMenu name={exportName(target, "kernel-log", "txt").replace(/\.txt$/, "")} demo={demo} disabled={!text} build={format => textLogExport(text, format)} />
          <button type="button" className="btn sm icon" title={copied ? "Copied" : "Copy the log"} aria-label="Copy the log" disabled={!text} onClick={() => void copy()}><Icon name={copied ? "check" : "copy"} /></button>
        </div>
      </div>

      {(snapshot?.busy && !live) && (
        <p className="kl-note"><Icon name="info" />Another payload holds /dev/klog, so this shows only what the receiver read before.{klogServers.length ? " Use Live to stream from it." : ` Enable the kernel log server in your ${target === "ps4" ? "GoldHEN" : "etaHEN or klogsrv"} settings to stream it here.`}</p>
      )}
      {liveNote && <p className="kl-note warn"><Icon name="warn" />{liveNote}</p>}
      {error && !text && <div className="empty-state"><Icon name="alert" /><h3>The kernel log didn't load</h3><p>{error}</p></div>}

      {text && (
        <>
          <div className="kl-summary">
            <span className={analysis.panics ? "fail" : ""}><b>{analysis.panics}</b> kernel panic{analysis.panics === 1 ? "" : "s"}</span>
            <span className={analysis.errors ? "warn" : ""}><b>{analysis.errors}</b> error{analysis.errors === 1 ? "" : "s"}</span>
            <span><b>{analysis.warnings}</b> warning{analysis.warnings === 1 ? "" : "s"}</span>
            <span className="kl-count">{analysis.lines.length.toLocaleString()} lines{visible.hidden ? `, showing the last ${SHOWN_LINES.toLocaleString()}` : ""}</span>
          </div>
          {panics.length > 0 && (
            <div className="kl-panics">
              {panics.slice(-3).reverse().map(finding => (
                <button key={finding.line} type="button" className="kl-panic" onClick={() => jump(finding)}>
                  <span className="kl-panic-head"><Icon name="alert" /><strong>{finding.text}</strong><em>line {finding.line + 1}</em></span>
                  {finding.context.length > 0 && <code>{finding.context.filter(Boolean).slice(0, 8).join("\n")}</code>}
                </button>
              ))}
              <p className="sys-note">A panic here may be from before the last restart.</p>
            </div>
          )}
          <div className="kl-body" ref={body} onWheel={() => setFollow(false)}>
            {visible.rows.length
              ? visible.rows.map(row => <div key={row.index} data-line={row.index} className={`kl-line ${row.severity}`}><span className="kl-no">{row.index + 1}</span><span className="kl-text">{row.text || " "}</span></div>)
              : <p className="kl-empty">No lines match.</p>}
          </div>
        </>
      )}
      {!text && !error && (loading ? <div className="skeleton kl-skeleton" /> : <div className="empty-state"><Icon name="rows" /><h3>The kernel log is empty</h3><p>Nothing has been written since the receiver last read it.{klogServers.length ? " Use Live to stream new lines." : ""}</p></div>)}
    </div>
  )
}
