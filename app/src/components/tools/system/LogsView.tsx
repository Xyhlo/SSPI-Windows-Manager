/* =====================================================================
   Logs & crashes — log and settings files other payloads keep under /data
   and /user/data, grouped by payload, and the console's crash reports.
   Selecting a log shows its tail with errors highlighted.
   ===================================================================== */
import { useEffect, useMemo, useState } from "react"
import { Icon } from "../../Icon"
import { consoleLogFiles, consoleReadLog } from "@/lib/console-api"
import { ExportMenu } from "./ExportMenu"
import type { LogFile, LogFileList, LogTail } from "@/lib/console-types"
import { analyzeKernelLog, exportName, fmtAge, groupLogFiles, logListExport, textLogExport } from "@/lib/diagnostics"
import { errorText, fmtBytes } from "@/lib/format"
import type { ConsoleKind } from "@/types"

type Props = { target: ConsoleKind; host: string; port: number; demo: boolean }
const fileName = (path: string) => path.slice(path.lastIndexOf("/") + 1)

export function LogsView({ target, host, port, demo }: Props) {
  const [list, setList] = useState<LogFileList | null>(null)
  const [error, setError] = useState("")
  const [loading, setLoading] = useState(false)
  const [selected, setSelected] = useState<LogFile | null>(null)
  const [tail, setTail] = useState<LogTail | null>(null)
  const [tailError, setTailError] = useState("")
  const [reading, setReading] = useState(false)
  const [limit, setLimit] = useState(256 * 1024)
  const [copied, setCopied] = useState(false)

  const load = async () => {
    setLoading(true); setError("")
    try { setList(await consoleLogFiles({ target, host, port, demo })) } catch (reason) { setError(errorText(reason)) }
    finally { setLoading(false) }
  }
  useEffect(() => { setList(null); setSelected(null); setTail(null); void load() /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [target, host, port, demo])

  const read = async (file: LogFile, maxBytes = limit) => {
    setSelected(file); setTailError("")
    if (file.kind === "crash") { setTail(null); return }
    setReading(true)
    try { setTail(await consoleReadLog({ target, host, port, demo, path: file.path, maxBytes })) } catch (reason) { setTail(null); setTailError(errorText(reason)) }
    finally { setReading(false) }
  }
  const more = () => { if (!selected) return; const next = Math.min(limit * 4, 1024 * 1024); setLimit(next); void read(selected, next) }
  const copy = async () => {
    if (!tail) return
    try { await navigator.clipboard.writeText(tail.text); setCopied(true); window.setTimeout(() => setCopied(false), 1400) } catch { setTailError("Copying to the clipboard failed.") }
  }

  const groups = useMemo(() => groupLogFiles(list?.files || []), [list])
  const analysis = useMemo(() => tail ? analyzeKernelLog(tail.text) : null, [tail])
  const severities = useMemo(() => new Map(analysis?.findings.map(f => [f.line, f.severity]) || []), [analysis])
  const crashes = list?.files.filter(f => f.kind === "crash") || []

  if (error && !list) return <div className="empty-state"><Icon name="alert" /><h3>Payload logs didn't load</h3><p>{error}</p></div>
  if (!list) return <div className="skeleton kl-skeleton" />
  if (!list.files.length) return (
    <div className="empty-state"><Icon name="files" /><h3>No payload logs or crash reports</h3>
      <p>Nothing under /data or /user/data looks like a log, settings file or crash report.</p>
      <div className="row"><button type="button" className="btn sm" disabled={loading} onClick={() => void load()}><Icon name="refresh" />Check again</button></div>
    </div>
  )

  return (
    <div className="lv">
      <aside className="lv-list">
        <div className="lv-list-head">
          <span>{list.files.length} file{list.files.length === 1 ? "" : "s"}{crashes.length ? `, ${crashes.length} crash report${crashes.length === 1 ? "" : "s"}` : ""}</span>
          <span className="lv-list-tools">
            <ExportMenu label="List" name={exportName(target, "log-files", "txt").replace(/\.txt$/, "")} demo={demo} build={format => logListExport(list.files, format)} />
            <button type="button" className="btn sm icon" title="Refresh" aria-label="Refresh the file list" disabled={loading} onClick={() => void load()}><Icon name="refresh" /></button>
          </span>
        </div>
        {(list.truncated || list.incomplete) && <p className="sys-note">{list.truncated ? "The list stopped at its limit." : "Some folders couldn't be read."}</p>}
        <div className="lv-groups">
          {groups.map(group => (
            <div key={group.owner} className={`lv-group ${group.owner === "Crash reports" ? "crash" : ""}`}>
              <p className="lv-owner">{group.owner}</p>
              {group.files.map(file => (
                <button key={file.path} type="button" className="lv-file" aria-pressed={selected?.path === file.path} onClick={() => void read(file)} title={file.path}>
                  <Icon name={file.kind === "crash" ? "alert" : file.kind === "config" ? "sliders" : "rows"} />
                  <span className="lv-file-name">{fileName(file.path)}</span>
                  <span className="lv-file-meta">{fmtBytes(file.size)} · {fmtAge(file.modified)}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      </aside>

      <section className="lv-view">
        {!selected ? (
          <div className="empty-state"><Icon name="rows" /><h3>Choose a file</h3><p>Logs open at their newest lines. Crash reports show where they are so you can copy them over FTP.</p></div>
        ) : selected.kind === "crash" ? (
          <div className="lv-crash">
            <p className="sys-label">{fileName(selected.path)}</p>
            <dl className="sys-rows">
              <div><dt>Location</dt><dd className="pv-mono">{selected.path}</dd></div>
              <div><dt>Size</dt><dd>{fmtBytes(selected.size)}</dd></div>
              <div><dt>Written</dt><dd>{new Date(selected.modified * 1000).toLocaleString()} ({fmtAge(selected.modified)})</dd></div>
            </dl>
            <p className="sys-note">{target === "ps4" ? "orbisdmp and orbisstate" : "Crash"} files are binary crash dumps. Copy them with FTP to analyse them on a PC.</p>
          </div>
        ) : (
          <>
            <div className="kl-bar">
              <div className="kl-source">
                <strong className="pv-mono">{selected.path}</strong>
                {tail && <span>{tail.offset ? `last ${fmtBytes(tail.bytes)} of ${fmtBytes(tail.size)}` : fmtBytes(tail.size)}{analysis && (analysis.errors || analysis.panics) ? `, ${analysis.errors + analysis.panics} error line${analysis.errors + analysis.panics === 1 ? "" : "s"}` : ""}</span>}
              </div>
              <div className="kl-actions">
                {tail && tail.offset > 0 && limit < 1024 * 1024 && <button type="button" className="btn sm" disabled={reading} onClick={more}><Icon name="plus" />Load more</button>}
                <ExportMenu name={`${target}-${fileName(selected.path).replace(/\.[^.]*$/, "")}`} demo={demo} disabled={!tail} build={format => textLogExport(tail?.text ?? "", format)} />
                <button type="button" className="btn sm icon" title="Reload" aria-label="Reload the file" disabled={reading} onClick={() => void read(selected)}><Icon name="refresh" /></button>
                <button type="button" className="btn sm icon" title={copied ? "Copied" : "Copy"} aria-label="Copy the file" disabled={!tail} onClick={() => void copy()}><Icon name={copied ? "check" : "copy"} /></button>
              </div>
            </div>
            {tailError ? <div className="empty-state"><Icon name="alert" /><h3>The file didn't open</h3><p>{tailError}</p></div>
              : !tail || !analysis ? <div className="skeleton kl-skeleton" />
              : (
                <div className="kl-body lv-body">
                  {analysis.lines.map((line, index) => <div key={index} className={`kl-line ${severities.get(index) || ""}`}><span className="kl-no">{index + 1}</span><span className="kl-text">{line || " "}</span></div>)}
                </div>
              )}
          </>
        )}
      </section>
    </div>
  )
}
