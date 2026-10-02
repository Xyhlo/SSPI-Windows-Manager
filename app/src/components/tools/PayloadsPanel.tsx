/* =====================================================================
   Payloads — the binloader manager. One list, like search results: the
   selected payload is sent to the console's loader with Enter or Send.
   Built-in receivers come first; added files are kept by the app.
   ===================================================================== */
import { open as openDialog } from "@tauri-apps/plugin-dialog"
import { useEffect, useMemo, useRef, useState, type ChangeEvent, type ReactNode } from "react"
import { Collapse } from "../Collapse"
import { Seg } from "../Controls"
import { Icon } from "../Icon"
import { ActionBubbleMenu } from "../ActionBubbleMenu"
import { toast } from "../toasts"
import { addPayloads, listPayloads, removePayload, sendPayload, updatePayload } from "@/lib/console-api"
import { loaderEndpoint } from "@/lib/console-helpers"
import type { PayloadEntry, PayloadSendResult, PayloadTarget } from "@/lib/console-types"
import { errorText, fmtBytes, fmtSpeed } from "@/lib/format"
import { isTyping, setPanelKeys } from "@/lib/keys"
import { clamp } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { getAutostart, setAutostart, getPayloadCatalog, downloadCatalogPayload, launcherAvailable, type AutostartStatus, type PayloadCatalog } from "@/lib/launcher-api"
import "./WebLauncherPanel.css"

type Props = { target: ConsoleKind; settings: Settings; demo: boolean; onReceiverLoaded: (target: ConsoleKind) => void }
type SessionSend = { id: string; name: string; at: number; result: string; ok: boolean; totalMs?: number; bytesPerSecond?: number | null }
type Traced = PayloadSendResult & { at: number }

const PREVIEW = "Offline preview: nothing was sent to the console."
const time = (at: number) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
const fmtMs = (ms: number) => ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`
/** One monospace line of header facts; raw payloads have none to show. */
const techLine = (entry: PayloadEntry) => entry.elf ? `${entry.elf.class} ${entry.elf.endian} · ${entry.elf.machine} · ${entry.elf.kind} · entry ${entry.elf.entry}` : "Raw binary, no ELF header"
const when = (at: number) => {
  const day = new Date(at), today = new Date()
  return day.toDateString() === today.toDateString() ? time(at) : day.toLocaleDateString([], { month: "short", day: "numeric" })
}

export function PayloadsPanel({ target, settings, demo, onReceiverLoaded }: Props) {
  const endpoint = loaderEndpoint(settings, target)
  const name = target.toUpperCase()
  const loader = target === "ps4" ? "GoldHEN BinLoader" : "ELF loader"
  const [payloads, setPayloads] = useState<PayloadEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState("")
  const [busy, setBusy] = useState("")
  const [selected, setSelected] = useState("")
  const [editing, setEditing] = useState("")
  const [removing, setRemoving] = useState("")
  const [history, setHistory] = useState<SessionSend[]>([])
  const [traces, setTraces] = useState<Record<string, Traced>>({})
  const [autostart, setAutostartState] = useState<AutostartStatus[]>([])
  const [autoBusy, setAutoBusy] = useState(false)
  const [autoError, setAutoError] = useState("")
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
    const poll = async () => {
      if (fetching) return
      fetching = true; const generation = autoGeneration.current
      try { const next = await getAutostart(demo); if (live && generation === autoGeneration.current) setAutostartState(next) }
      catch (reason) { if (live) setAutoError(errorText(reason)) }
      finally { fetching = false }
    }
    void poll(); const timer = window.setInterval(() => void poll(), 2000)
    return () => { live = false; clearInterval(timer) }
  }, [demo])

  const shown = useMemo(() => payloads
    .filter(payload => payload.target === "any" || payload.target === target)
    .sort((a, b) => Number(b.builtin) - Number(a.builtin) || a.name.localeCompare(b.name)), [payloads, target])
  const current = shown.find(payload => payload.id === selected) || shown[0]
  const auto = autostart.find(item => item.target === target)
  const changeAutostart = async (id: string) => {
    autoGeneration.current += 1; setAutoBusy(true); setAutoError("")
    try { setAutostartState(await setAutostart(target, id || null, demo)) }
    catch (reason) { setAutoError(errorText(reason)) }
    finally { autoGeneration.current += 1; setAutoBusy(false) }
  }

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
    try {
      const result = await sendPayload({ id: entry.id, target, host: endpoint.host, port: endpoint.port, demo })
      setPayloads(old => old.map(item => item.id === entry.id ? { ...item, lastSentAt: Date.now(), lastResult: result.message } : item))
      setHistory(old => [{ id: `${entry.id}-${Date.now()}`, name: entry.name, at: Date.now(), result: result.message, ok: true, totalMs: result.totalMs, bytesPerSecond: result.bytesPerSecond }, ...old].slice(0, 20))
      setTraces(old => ({ ...old, [entry.id]: { ...result, at: Date.now() } }))
      toast({ tone: "success", title: entry.builtin && result.verified ? `${name} receiver is running` : `${entry.name} sent`, text: result.message })
      if (entry.builtin && result.verified) onReceiverLoaded(target)
    } catch (reason) {
      const message = errorText(reason)
      setPayloads(old => old.map(item => item.id === entry.id ? { ...item, lastSentAt: Date.now(), lastResult: message } : item))
      setHistory(old => [{ id: `${entry.id}-${Date.now()}`, name: entry.name, at: Date.now(), result: message, ok: false }, ...old].slice(0, 20))
      toast({ tone: "error", title: `${entry.name} wasn't sent`, text: `${message} Check that ${loader} is running on your ${name}.` })
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
        <button type="button" className="btn sm" disabled={busy === "add"} onClick={() => void choose()}>{busy === "add" ? <span className="spinner" /> : <Icon name="plus" />}Add payloads</button>
        <input ref={fileRef} type="file" accept=".elf,.bin" multiple hidden onChange={(event: ChangeEvent<HTMLInputElement>) => { const files = Array.from(event.currentTarget.files || []); event.currentTarget.value = ""; void addPaths(files.map(file => `preview/${file.name}`)) }} />
      </div>
      {error && <p className="pl-error" role="alert">{error}</p>}
      <div className="pl-autostart">
        <label className="pl-autostart-label" htmlFor="payload-autostart">Autostart</label>
        <select id="payload-autostart" value={auto?.payloadId || ""} disabled={loading || autoBusy || !!busy || !launcherAvailable(demo)} onChange={event => void changeAutostart(event.target.value)}>
          <option value="">Off</option>
          {shown.map(entry => <option key={entry.id} value={entry.id}>{entry.name}</option>)}
          {auto?.payloadId && !shown.some(entry => entry.id === auto.payloadId) && <option value={auto.payloadId}>Payload unavailable</option>}
        </select>
        <span className="pl-autostart-status" role="status">{autoBusy ? "Saving…" : autoError || auto?.message || "Sends once when SSPI starts."}</span>
        <details className="pl-autostart-details"><summary>Details</summary><p>Off by default, with one payload per console. Choosing a payload arms it now and on each SSPI launch. Connection failures retry twice, then stop. A completed or partial send is never repeated automatically. The console’s loader must already be running and reachable over your network.</p>{auto?.lastAttemptAt && <p>Last attempt {when(auto.lastAttemptAt)} · {auto.attempts} of 3{auto.nextAttemptAt ? ` · next ${time(auto.nextAttemptAt)}` : ""}. {auto.phase === "sent" ? "Bytes were sent; execution was not verified." : auto.phase === "verified" ? "The SSPI receiver answered after loading." : ""}</p>}</details>
      </div>
      <div ref={listRef} className="pl-list scroll" role="listbox" aria-label={`Payloads for your ${name}`}>
        {loading && Array.from({ length: 3 }, (_, n) => <div key={n} className="skeleton" style={{ height: 68, flex: "none" }} />)}
        {!loading && shown.map((entry, n) => {
          const isCurrent = entry.id === current?.id
          const ext = entry.fileName.split(".").pop()?.toUpperCase() || "ELF"
          const failed = !!entry.lastResult && entry.lastSentAt && !/sent|running|loaded|verified|preview/i.test(entry.lastResult)
          return (
            <div key={entry.id} className={`pl-item ${isCurrent ? "is-current" : ""}`}>
              <div
                role="option" tabIndex={isCurrent ? 0 : -1} aria-selected={isCurrent} data-id={entry.id}
                className="pl-row enter" style={{ animationDelay: `${Math.min(n, 10) * 30}ms` }}
                onClick={() => setSelected(entry.id)} onDoubleClick={() => void send(entry)} onFocus={() => setSelected(entry.id)}
              >
                <span className={`pl-glyph ${entry.builtin ? "builtin" : ""}`}>{entry.builtin ? <Icon name="shield" /> : ext}</span>
                <span className="pl-main">
                  <strong>{entry.name}</strong>
                  <span>{entry.fileName}&ensp;{fmtBytes(entry.size)}{entry.builtin ? <>&ensp;Built in</> : entry.target === "any" ? <>&ensp;Any console</> : null}</span>
                </span>
                <span className={`pl-last ${failed ? "fail" : ""}`}>
                  {entry.lastSentAt ? <><b>{failed ? "Failed" : "Sent"} {when(entry.lastSentAt)}</b>{isCurrent && <small>{entry.lastResult}</small>}</> : <b>Not sent yet</b>}
                </span>
                {isCurrent && !entry.builtin && (
                  <span className="pl-tools" onClick={event => event.stopPropagation()}>
                    <ActionBubbleMenu label={`Actions for ${entry.name}`} actions={[
                      { id: "rename", label: "Rename", icon: "pencil", disabled: !!busy, onSelect: () => { setRemoving(""); setEditing(editing === entry.id ? "" : entry.id) } },
                      { id: "autostart", label: auto?.payloadId === entry.id ? "Disable autostart" : "Autostart", icon: "play", disabled: autoBusy || !!busy, checked: auto?.payloadId === entry.id, onSelect: () => changeAutostart(auto?.payloadId === entry.id ? "" : entry.id) },
                      { id: "remove", label: "Remove", icon: "trash", disabled: !!busy, onSelect: () => { setEditing(""); setRemoving(removing === entry.id ? "" : entry.id) } },
                    ]} />
                  </span>
                )}
                <button type="button" className={`btn sm ${isCurrent ? "primary" : ""} pl-send`} disabled={!endpoint.host || !!busy} onClick={event => { event.stopPropagation(); setSelected(entry.id); void send(entry) }}>
                  {busy === entry.id ? <span className="spinner" /> : <Icon name="send" />}{busy === entry.id ? "Sending" : "Send"}
                </button>
              </div>
              <Collapse open={editing === entry.id} className="pl-drawer">
                <PayloadEditor entry={entry} busy={busy === entry.id} onCancel={() => setEditing("")} onSave={draft => void save(entry, draft)} />
              </Collapse>
              <Collapse open={isCurrent && editing !== entry.id && removing !== entry.id} className="pl-drawer">
                <details className="pl-inspect"><summary>Payload details{traces[entry.id] ? " and last send" : ""}</summary><p className="pl-tech">{techLine(entry)}</p><PayloadSheet entry={entry} destination={endpoint.host ? `${endpoint.host}:${endpoint.port}` : ""} loader={loader} trace={traces[entry.id]} sending={busy === entry.id} /></details>
              </Collapse>
              <Collapse open={removing === entry.id} className="pl-drawer">
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
      {target === "ps5" && <CatalogBrowser demo={demo} onImported={setPayloads} />}
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

function CatalogBrowser({ demo, onImported }: { demo: boolean; onImported: (entries: PayloadEntry[]) => void }) {
  const [catalog, setCatalog] = useState<PayloadCatalog | null>(null)
  const [search, setSearch] = useState("")
  const [busy, setBusy] = useState("")
  const [error, setError] = useState("")
  const refresh = async (force: boolean) => {
    if (busy) return
    setBusy("refresh"); setError("")
    try { setCatalog(await getPayloadCatalog(force, demo)) }
    catch (reason) { setError(errorText(reason)) }
    finally { setBusy("") }
  }
  const download = async (filename: string) => {
    if (busy) return
    if (demo) { toast({ tone: "info", title: "Offline preview", text: "No payload was downloaded." }); return }
    setBusy(filename); setError("")
    try { onImported(await downloadCatalogPayload(filename)); setCatalog(await getPayloadCatalog(false, demo)); toast({ tone: "success", title: "Payload added", text: "The selected repository version is in your library. Use Send when ready." }) }
    catch (reason) { setError(errorText(reason)) }
    finally { setBusy("") }
  }
  const entries = catalog?.entries.filter(item => `${item.name} ${item.category} ${item.description}`.toLowerCase().includes(search.trim().toLowerCase())) || []
  return <details className="tech pl-catalog" onToggle={event => { if (event.currentTarget.open && !catalog && !busy) void refresh(false) }}>
    <summary>Payload catalog · PS5</summary>
    <div className="tech-body">
      <div className="pl-catalog-top"><input className="field" placeholder="Find a payload" aria-label="Search payload catalog" value={search} onChange={e => setSearch(e.target.value)} /><button type="button" className="btn sm" disabled={!!busy || !launcherAvailable(demo)} onClick={() => void refresh(true)}>{busy === "refresh" ? "Loading…" : "Refresh"}</button></div>
      <p>From <a href="https://github.com/itsPLK/ps5-payloads-mirror" target="_blank" rel="noreferrer">Payload Manager’s repository</a>. Refresh checks available versions. Downloads add a local copy; your existing payloads and autostart selection stay as chosen.</p>
      {(error || catalog?.warning) && <p className="pl-error" role="alert">{error || catalog?.warning}</p>}
      <div className="pl-catalog-list">
        {entries.map(entry => <div className="pl-catalog-item" key={entry.filename}><div><strong>{entry.name} {entry.version}</strong><p>{entry.description}</p><small>{entry.checksum ? "SHA-256 checked on download" : "No published checksum"}{entry.source && <> · <a href={entry.source} target="_blank" rel="noreferrer">Source & compatibility</a></>}</small></div><button type="button" className="btn sm" disabled={!!busy || entry.installed} onClick={() => void download(entry.filename)}>{busy === entry.filename ? "Downloading…" : entry.installed ? "Added" : "Download"}</button></div>)}
        {!entries.length && <p className="pl-catalog-empty">{busy === "refresh" ? "Loading repository…" : catalog ? "No payloads match this search." : "Open the catalog in the SSPI app to download payloads."}</p>}
      </div>
    </div>
  </details>
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
                  <span className="d">{step.detail}</span>
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
