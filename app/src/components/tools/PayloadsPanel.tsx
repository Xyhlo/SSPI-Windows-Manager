/* =====================================================================
   Payloads — the binloader manager. One list, like search results: the
   selected payload is sent to the console's loader with Enter or Send.
   Built-in receivers come first; added files are kept by the app.
   ===================================================================== */
import { open as openDialog } from "@tauri-apps/plugin-dialog"
import { invoke } from "@tauri-apps/api/core"
import { useEffect, useMemo, useRef, useState, type ChangeEvent, type ReactNode } from "react"
import { Collapse } from "../Collapse"
import { Seg } from "../Controls"
import { Icon } from "../Icon"
import { ActionMenu } from "../ActionMenu"
import { toast } from "../toasts"
import { addPayloads, listPayloads, removePayload, sendPayload, updatePayload } from "@/lib/console-api"
import { loaderEndpoint } from "@/lib/console-helpers"
import type { PayloadEntry, PayloadSendResult, PayloadTarget } from "@/lib/console-types"
import { errorText, fmtBytes, fmtSpeed } from "@/lib/format"
import { isTyping, setPanelKeys } from "@/lib/keys"
import { clamp } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { autostartRunning, getAutostart, launcherAvailable, runAutostart, setAutostart, stopAutostart, type AutostartOrder, type AutostartStatus } from "@/lib/launcher-api"
import { AutostartCard } from "./payloads/AutostartCard"
import { AutostartDialog, Glyph } from "./payloads/AutostartDialog"
import { CatalogDialog } from "./payloads/CatalogDialog"
import "./payloads/payloads.css"

type Props = { target: ConsoleKind; settings: Settings; demo: boolean; onReceiverLoaded: (target: ConsoleKind) => void; onOpenProcesses: () => void }
type SessionSend = { id: string; name: string; at: number; result: string; ok: boolean; totalMs?: number; bytesPerSecond?: number | null }
type Traced = PayloadSendResult & { at: number }

const PREVIEW = "Offline preview: nothing was sent to the console."
const time = (at: number) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
const fmtMs = (ms: number) => ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`
/** One monospace line of header facts; raw payloads have none to show. */
const techLine = (entry: PayloadEntry) => entry.elf ? `${entry.elf.class} ${entry.elf.endian} · ${entry.elf.machine} · ${entry.elf.kind} · entry ${entry.elf.entry}` : "Raw binary, no ELF header"
const ordinal = (n: number) => `${n}${n % 10 === 1 && n % 100 !== 11 ? "st" : n % 10 === 2 && n % 100 !== 12 ? "nd" : n % 10 === 3 && n % 100 !== 13 ? "rd" : "th"}`
const when = (at: number) => {
  const day = new Date(at), today = new Date()
  return day.toDateString() === today.toDateString() ? time(at) : day.toLocaleDateString([], { month: "short", day: "numeric" })
}

export function PayloadsPanel({ target, settings, demo, onReceiverLoaded, onOpenProcesses }: Props) {
  const endpoint = loaderEndpoint(settings, target)
  const name = target.toUpperCase()
  const loader = target === "ps4" ? "GoldHEN BinLoader" : "ELF loader"
  const [payloads, setPayloads] = useState<PayloadEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")
  const [busy, setBusy] = useState("")
  // The Start button shows a check for a moment after a successful send instead of a toast.
  const [started, setStarted] = useState("")
  const startedTimer = useRef<number>()
  useEffect(() => () => window.clearTimeout(startedTimer.current), [])
  const [selected, setSelected] = useState("")
  const [editing, setEditing] = useState("")
  const [removing, setRemoving] = useState("")
  const [history, setHistory] = useState<SessionSend[]>([])
  const [traces, setTraces] = useState<Record<string, Traced>>({})
  const [autostart, setAutostartState] = useState<AutostartStatus[]>([])
  const [autoBusy, setAutoBusy] = useState(false)
  const [ordering, setOrdering] = useState<{ adding?: string } | null>(null)
  const [catalogOpen, setCatalogOpen] = useState(false)
  const autoGeneration = useRef(0)
  const fileRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    let live = true
    setLoading(true)
    listPayloads({ demo }).then(result => { if (live) { setPayloads(result); setError("") } }).catch(reason => { if (live) setError(errorText(reason)) }).finally(() => { if (live) setLoading(false) })
    return () => { live = false }
  }, [demo])

  useEffect(() => {
    let live = true, fetching = false
    if (!launcherAvailable(demo)) return
    let timer = 0
    // Polls quickly while an order is running, so each payload's state shows as it changes.
    const poll = async () => {
      if (fetching) return
      fetching = true; const generation = autoGeneration.current
      let running = false
      try { const next = await getAutostart(demo); running = next.some(item => autostartRunning(item)); if (live && generation === autoGeneration.current) setAutostartState(next) }
      catch { /* The card keeps its last state; the next poll retries. */ }
      finally { fetching = false }
      if (live) timer = window.setTimeout(() => void poll(), running ? 500 : 2000)
    }
    void poll()
    return () => { live = false; clearTimeout(timer) }
  }, [demo])

  const shown = useMemo(() => payloads
    .filter(payload => payload.target === "any" || payload.target === target)
    .sort((a, b) => Number(b.builtin) - Number(a.builtin) || a.name.localeCompare(b.name)), [payloads, target])
  const current = shown.find(payload => payload.id === selected) || shown[0]
  const auto = autostart.find(item => item.target === target)
  const order = (status?: AutostartStatus): AutostartOrder => (status?.steps || []).map(step => ({ payloadId: step.payloadId, delayMs: step.delayMs, processName: step.processName || "" }))
  const autoAction = async (action: () => Promise<AutostartStatus[]>, failure: string) => {
    autoGeneration.current += 1; setAutoBusy(true)
    try { setAutostartState(await action()) }
    catch (reason) { toast({ tone: "error", title: failure, text: errorText(reason) }); throw reason }
    finally { autoGeneration.current += 1; setAutoBusy(false) }
  }
  const saveOrder = (enabled: boolean, next: AutostartOrder) => autoAction(() => setAutostart(target, enabled, next, demo), "Autostart wasn't saved").then(() => {
    toast({ tone: "success", title: enabled ? "Autostart order saved" : "Autostart is off", text: enabled ? `${next.length} payload${next.length === 1 ? "" : "s"} will be sent in this order${demo ? " (offline preview)" : ""}.` : "The order is kept for later." })
  })
  const toggleAutostart = (on: boolean) => {
    if (on && !auto?.steps.length) { setOrdering({}); return }
    void autoAction(() => setAutostart(target, on, order(auto), demo), "Autostart wasn't changed").catch(() => undefined)
  }
  const autostartPlace = (id: string) => (auto?.steps.findIndex(step => step.payloadId === id) ?? -1) + 1

  const addPaths = async (paths: string[]) => {
    if (!paths.length) return
    setBusy("add")
    try {
      const next = await addPayloads({ paths, target, demo })
      setPayloads(next)
      setError("")
      toast({ tone: "success", title: paths.length === 1 ? "Payload added" : `${paths.length} payloads added`, text: demo ? PREVIEW : "Copies are kept in the app's payload folder." })
    } catch (reason) { toast({ tone: "error", title: "The payload wasn't added", text: errorText(reason) }) }
    finally { setBusy("") }
  }
  const choose = async () => {
    if (demo) { fileRef.current?.click(); return }
    try {
      const picked = await openDialog({ multiple: true, title: "Add payloads", filters: [{ name: "Payloads", extensions: ["elf", "bin"] }] })
      await addPaths(Array.isArray(picked) ? picked : picked ? [picked] : [])
    } catch (reason) { toast({ tone: "error", title: "The files couldn't be opened", text: errorText(reason) }) }
  }
  const send = async (entry: PayloadEntry) => {
    if (!endpoint.host || busy) return
    setBusy(entry.id)
    setStarted("")
    try {
      const result = await sendPayload({ id: entry.id, target, host: endpoint.host, port: endpoint.port, demo })
      setPayloads(old => old.map(item => item.id === entry.id ? { ...item, lastSentAt: Date.now(), lastResult: result.message } : item))
      setHistory(old => [{ id: `${entry.id}-${Date.now()}`, name: entry.name, at: Date.now(), result: result.message, ok: true, totalMs: result.totalMs, bytesPerSecond: result.bytesPerSecond }, ...old].slice(0, 20))
      setTraces(old => ({ ...old, [entry.id]: { ...result, at: Date.now() } }))
      window.clearTimeout(startedTimer.current)
      setStarted(entry.id)
      startedTimer.current = window.setTimeout(() => setStarted(""), 2500)
      if (entry.builtin && result.verified) onReceiverLoaded(target)
    } catch (reason) {
      const message = errorText(reason)
      setTraces(old => { const next = { ...old }; delete next[entry.id]; return next })
      setPayloads(old => old.map(item => item.id === entry.id ? { ...item, lastSentAt: Date.now(), lastResult: message } : item))
      setHistory(old => [{ id: `${entry.id}-${Date.now()}`, name: entry.name, at: Date.now(), result: message, ok: false }, ...old].slice(0, 20))
      toast({ tone: "error", title: `${entry.name}: start failed`, text: message.split("\n")[0] })
    } finally { setBusy("") }
  }
  const save = async (entry: PayloadEntry, draft: { name: string; target: PayloadTarget; notes: string }) => {
    setBusy(entry.id)
    try { setPayloads(await updatePayload({ id: entry.id, ...draft, demo })); setEditing("") }
    catch (reason) { toast({ tone: "error", title: "The changes weren't saved", text: errorText(reason) }) }
    finally { setBusy("") }
  }
  const remove = async (entry: PayloadEntry) => {
    setBusy(entry.id)
    try { setPayloads(await removePayload({ id: entry.id, demo })); setRemoving(""); toast({ tone: "success", title: `${entry.name} removed`, text: demo ? PREVIEW : "The stored copy was deleted." }) }
    catch (reason) { toast({ tone: "error", title: "The payload wasn't removed", text: errorText(reason) }) }
    finally { setBusy("") }
  }

  /* ---------------------------------------------------------------- keys */
  const live = useRef({ shown, current, editing, removing, send, choose })
  live.current = { shown, current, editing, removing, send, choose }
  useEffect(() => setPanelKeys(event => {
    const { shown, current, editing, removing, send, choose } = live.current
    if (isTyping() || editing || event.ctrlKey || event.altKey || event.metaKey) return false
    const k = event.key
    const i = current ? shown.indexOf(current) : -1
    if ((k === "ArrowDown" || k === "ArrowUp") && shown.length) {
      event.preventDefault()
      const next = shown[clamp(i + (k === "ArrowDown" ? 1 : -1), 0, shown.length - 1)]
      setSelected(next.id)
      listRef.current?.querySelector<HTMLElement>(`[data-id="${CSS.escape(next.id)}"]`)?.focus()
      return true
    }
    if (k === "Enter" && current && !removing && !(document.activeElement as HTMLElement | null)?.closest?.("button:not(.pl-row)")) { event.preventDefault(); void send(current); return true }
    if ((k === "a" || k === "A")) { event.preventDefault(); void choose(); return true }
    if ((k === "r" || k === "R") && current && !current.builtin) { event.preventDefault(); setEditing(current.id); setRemoving(""); return true }
    if (k === "Delete" && current && !current.builtin) { event.preventDefault(); setRemoving(current.id); setEditing(""); return true }
    return false
  }), [])

  /* ---------------------------------------------------------------- render */
  return (
    <div className="pl">
      <div className="pl-head">
        <p className="pl-target">
          <span className={`dot ${endpoint.host ? "good" : ""}`} />
          <span>{endpoint.host ? <>Sends to <strong>{endpoint.host}:{endpoint.port}</strong>, the {loader} on your {name}</> : <>Add your {name}'s address in Options, Consoles to send payloads</>}</span>
        </p>
        <button type="button" className="btn sm ghost" title="Running payloads, with Stop and End, in System" onClick={onOpenProcesses}><Icon name="cpu" />Processes</button>
        {target === "ps5" && <button type="button" className="btn sm ghost" onClick={() => setCatalogOpen(true)}><Icon name="globe" />Catalog</button>}
        <button type="button" className="btn sm" disabled={busy === "add"} onClick={() => void choose()}>{busy === "add" ? <span className="spinner" /> : <Icon name="plus" />}Add payloads</button>
        <input ref={fileRef} type="file" accept=".elf,.bin" multiple hidden onChange={(event: ChangeEvent<HTMLInputElement>) => { const files = Array.from(event.currentTarget.files || []); event.currentTarget.value = ""; void addPaths(files.map(file => `preview/${file.name}`)) }} />
      </div>
      {error && <p className="pl-error" role="alert">{error}</p>}
      <div><button type="button" className="btn ghost sm" disabled={demo} onClick={() => void invoke("show_session_log").catch(reason => toast({ tone: "error", title: "The diagnostic log couldn't be opened", text: errorText(reason) }))}><Icon name="folder" />Show Windows diagnostic log</button></div>
      <AutostartCard status={auto} payloads={shown} loader={loader} busy={autoBusy || loading} available={launcherAvailable(demo)}
        onToggle={toggleAutostart} onEdit={() => setOrdering({})}
        onRun={() => void autoAction(() => runAutostart(target, demo), "Autostart didn't start").catch(() => undefined)}
        onStop={() => void autoAction(() => stopAutostart(target, demo), "Autostart didn't stop").catch(() => undefined)} />
      <div ref={listRef} className="pl-list scroll" role="listbox" aria-label={`Payloads for your ${name}`}>
        {loading && Array.from({ length: 3 }, (_, n) => <div key={n} className="skeleton" style={{ height: 68, flex: "none" }} />)}
        {!loading && shown.map((entry, n) => {
          const isCurrent = entry.id === current?.id
          const failed = !!entry.lastResult && entry.lastSentAt && (/^SSPI receiver startup failed:|^The payload loader rejected|^Could not|^Sending the payload timed out|^The payload loader connection timed out/.test(entry.lastResult) || !/sent|running|loaded|verified|preview/i.test(entry.lastResult))
          return (
            <div key={entry.id} className={`pl-item ${isCurrent ? "is-current" : ""}`}>
              <div
                role="option" tabIndex={isCurrent ? 0 : -1} aria-selected={isCurrent} data-id={entry.id}
                className="pl-row enter" style={{ animationDelay: `${Math.min(n, 10) * 30}ms` }}
                onClick={() => setSelected(entry.id)} onDoubleClick={() => void send(entry)} onFocus={() => setSelected(entry.id)}
              >
                <Glyph entry={entry} size="md" />
                <span className="pl-main">
                  <strong>{entry.name}</strong>
                  <span>{entry.fileName}&ensp;{fmtBytes(entry.size)}{entry.builtin ? <>&ensp;Built in</> : entry.target === "any" ? <>&ensp;Any console</> : null}{autostartPlace(entry.id) > 0 && auto?.enabled && <span className="pl-auto-tag"><Icon name="bolt" />Autostart {autostartPlace(entry.id)}</span>}</span>
                </span>
                <span className={`pl-last ${failed ? "fail" : ""}`}>
                  {entry.lastSentAt ? <><b>{failed ? "Failed" : "Sent"} {when(entry.lastSentAt)}</b>{isCurrent && <small>{entry.lastResult}</small>}</> : <b>Not sent yet</b>}
                </span>
                {isCurrent && (
                  <span className="pl-tools" onClick={event => event.stopPropagation()}>
                    <ActionMenu label={`Actions for ${entry.name}`} trigger={{ icon: "more", title: "More actions" }} actions={[
                      autostartPlace(entry.id) > 0
                        ? { id: "autostart", label: "Edit autostart order", description: `Sent ${ordinal(autostartPlace(entry.id))} after a confirmed console wake`, icon: "bolt", disabled: autoBusy, onSelect: () => setOrdering({}) }
                        : { id: "autostart", label: "Add to autostart", description: "Choose where it goes in the order", icon: "bolt", disabled: autoBusy || !launcherAvailable(demo), onSelect: () => setOrdering({ adding: entry.id }) },
                      ...(!entry.builtin ? [
                        { id: "rename", label: "Rename", description: "Name, console and notes", icon: "pencil" as const, disabled: !!busy, onSelect: () => { setRemoving(""); setEditing(editing === entry.id ? "" : entry.id) } },
                        { id: "remove", label: "Remove", description: "Deletes the copy SSPI keeps", icon: "trash" as const, tone: "danger" as const, disabled: !!busy, onSelect: () => { setEditing(""); setRemoving(removing === entry.id ? "" : entry.id) } },
                      ] : []),
                    ]} />
                  </span>
                )}
                <button type="button" className={`btn sm ${isCurrent ? "primary" : ""} pl-send`} disabled={!endpoint.host || !!busy} onClick={event => { event.stopPropagation(); setSelected(entry.id); void send(entry) }}>
                  {busy === entry.id ? <><span className="spinner" />Starting</> : started === entry.id ? <><Icon name="check" />Started</> : <><Icon name="play" />Start</>}
                </button>
              </div>
              <Collapse open={editing === entry.id} className="pl-drawer" reveal>
                <PayloadEditor entry={entry} busy={busy === entry.id} onCancel={() => setEditing("")} onSave={draft => void save(entry, draft)} />
              </Collapse>
              <Collapse open={isCurrent && editing !== entry.id && removing !== entry.id} className="pl-drawer">
                {entry.lastResult && <PayloadDiagnostic text={entry.lastResult} label="Last result — full details" />}
                <details className="pl-inspect"><summary>Payload details{traces[entry.id] ? " and last send" : ""}</summary><p className="pl-tech">{techLine(entry)}</p><PayloadSheet entry={entry} destination={endpoint.host ? `${endpoint.host}:${endpoint.port}` : ""} loader={loader} trace={traces[entry.id]} sending={busy === entry.id} /></details>
              </Collapse>
              <Collapse open={removing === entry.id} className="pl-drawer" reveal>
                <div className="remove-confirm">
                  <span>Remove {entry.name}? The copy the app keeps is deleted; the original file stays where it was.</span>
                  <button type="button" className="btn sm danger" disabled={busy === entry.id} onClick={() => void remove(entry)}>{busy === entry.id ? "Removing" : "Remove"}</button>
                  <button type="button" className="btn sm ghost" onClick={() => setRemoving("")}>Back</button>
                </div>
              </Collapse>
            </div>
          )
        })}
        {!loading && !shown.length && (
          <div className="empty-state">
            <Icon name="send" />
            <h3>No payloads for your {name} yet</h3>
            <p>Add ELF or BIN payloads to keep them here and send them to the {loader} with one key.</p>
            <div className="row"><button type="button" className="btn sm" onClick={() => void choose()}><Icon name="plus" />Add payloads</button></div>
          </div>
        )}
      </div>
      <AutostartDialog open={!!ordering} adding={ordering?.adding} target={target} loader={loader} payloads={shown} status={auto}
        onClose={() => setOrdering(null)} onSave={saveOrder} />
      {target === "ps5" && <CatalogDialog open={catalogOpen} demo={demo} onClose={() => setCatalogOpen(false)} onImported={setPayloads} />}
      {history.length > 0 && (
        <details className="tech pl-log">
          <summary>This session: {history.length === 1 ? "1 send" : `${history.length} sends`}, last {history[0].name} at {time(history[0].at)}</summary>
          <div className="tech-body">
            {history.map(item => <p key={item.id} className={item.ok ? "" : "fail"}><span>{time(item.at)}</span>{item.name}: {item.result}{item.totalMs != null ? ` (${fmtMs(item.totalMs)}${item.bytesPerSecond ? `, ${fmtSpeed(item.bytesPerSecond)}` : ""})` : ""}</p>)}
          </div>
        </details>
      )}
    </div>
  )
}

/** Header facts for the selected payload and, after a send, what the send measured. */
function PayloadSheet({ entry, destination, loader, trace, sending }: { entry: PayloadEntry; destination: string; loader: string; trace?: Traced; sending: boolean }) {
  const elf = entry.elf
  const copy = async () => {
    try { await navigator.clipboard.writeText(entry.sha256); toast({ tone: "success", title: "SHA-256 copied", text: entry.sha256 }) }
    catch (reason) { toast({ tone: "error", title: "The hash wasn't copied", text: errorText(reason) }) }
  }
  const row = (key: string, value: ReactNode) => <div className="pl-kv"><span className="k">{key}</span><span className="v">{value}</span></div>
  return (
    <div className="pl-sheet">
      <div className="pl-facts">
        {row("Format", elf ? `${elf.class} ${elf.endian} · ${elf.kind}` : "Raw binary")}
        {row("Machine", elf?.machine || "–")}
        {row("Entry point", elf?.entry || "–")}
        {row("Segments", elf ? `${elf.loadable} of ${elf.segments} loadable · ${fmtBytes(elf.loadableBytes)} in memory` : "–")}
        {row("Size", `${fmtBytes(entry.size)} (${entry.size.toLocaleString()} bytes)`)}
        {row("Sends to", destination ? `${destination} · ${loader}` : "No console address")}
        <div className="pl-kv wide"><span className="k">SHA-256</span><span className="v hash">{entry.sha256}<button type="button" className="btn sm ghost icon" aria-label="Copy SHA-256" title="Copy" onClick={() => void copy()}><Icon name="copy" /></button></span></div>
      </div>
      {(trace || sending) && (
        <div className="pl-trace" aria-live="polite">
          <div className="pl-trace-head">
            <strong>{sending ? "Sending" : "Last send"}</strong>
            {trace && !sending && <span>{time(trace.at)} · {fmtMs(trace.totalMs ?? 0)} total{trace.bytesPerSecond ? ` · ${fmtBytes(trace.bytes)} at ${fmtSpeed(trace.bytesPerSecond)}` : ""}{trace.verified ? " · verified" : ""}</span>}
          </div>
          {sending ? <p className="pl-trace-wait"><span className="spinner" />Waiting for the loader and the console.</p> : (
            <ol>
              {(trace?.steps || []).map((step, n) => (
                <li key={n} className={step.ok ? "" : "fail"}>
                  <Icon name={step.ok ? "check" : "x"} />
                  <span className="l">{step.label}</span>
                  <div style={{ minWidth: 0 }}><PayloadDiagnostic text={step.detail} label={step.detail.split("\n")[0]} /></div>
                  <span className="t">{fmtMs(step.ms)}</span>
                </li>
              ))}
            </ol>
          )}
        </div>
      )}
    </div>
  )
}

function PayloadDiagnostic({ text, label }: { text: string; label: string }) {
  const copy = async () => {
    try { await navigator.clipboard.writeText(text); toast({ tone: "success", title: "Details copied", text: "The full diagnostic text is on the clipboard." }) }
    catch (reason) { toast({ tone: "error", title: "Details weren't copied", text: errorText(reason) }) }
  }
  return <details style={{ minWidth: 0 }}>
    <summary style={{ overflowWrap: "anywhere", cursor: "pointer" }}>{label}</summary>
    <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: 320, overflowY: "auto", userSelect: "text" }}>{text}</pre>
    <button type="button" className="btn sm ghost" onClick={() => void copy()}><Icon name="copy" />Copy details</button>
  </details>
}

function PayloadEditor({ entry, busy, onCancel, onSave }: { entry: PayloadEntry; busy: boolean; onCancel: () => void; onSave: (draft: { name: string; target: PayloadTarget; notes: string }) => void }) {
  const [name, setName] = useState(entry.name)
  const [target, setTarget] = useState<PayloadTarget>(entry.target)
  const [notes, setNotes] = useState(entry.notes || "")
  useEffect(() => { setName(entry.name); setTarget(entry.target); setNotes(entry.notes || "") }, [entry.id])
  return (
    <div className="pl-editor" onKeyDown={event => { if (event.key === "Escape") { event.stopPropagation(); onCancel() } }}>
      <label className="field"><Icon name="pencil" /><input value={name} maxLength={80} aria-label="Payload name" autoFocus onChange={event => setName(event.target.value)} onKeyDown={event => { if (event.key === "Enter" && name.trim()) onSave({ name: name.trim(), target, notes }) }} /></label>
      <Seg label="Console" value={target} options={[["ps5", "PS5"], ["ps4", "PS4"], ["any", "Any"]]} onChange={setTarget} />
      <label className="field pl-notes"><input value={notes} maxLength={500} placeholder="Notes" aria-label="Notes" onChange={event => setNotes(event.target.value)} /></label>
      <button type="button" className="btn sm primary" disabled={busy || !name.trim()} onClick={() => onSave({ name: name.trim(), target, notes })}>Save</button>
      <button type="button" className="btn sm ghost" onClick={onCancel}>Cancel</button>
    </div>
  )
}
