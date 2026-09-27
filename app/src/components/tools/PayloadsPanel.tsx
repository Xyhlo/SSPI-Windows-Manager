/* =====================================================================
   Payloads — the binloader manager. One list, like search results: the
   selected payload is sent to the console's loader with Enter or Send.
   Built-in receivers come first; added files are kept by the app.
   ===================================================================== */
import { open as openDialog } from "@tauri-apps/plugin-dialog"
import { useEffect, useMemo, useRef, useState, type ChangeEvent } from "react"
import { Collapse } from "../Collapse"
import { Seg } from "../Controls"
import { Icon } from "../Icon"
import type { Hint } from "../Shell"
import { toast } from "../toasts"
import { addPayloads, listPayloads, removePayload, sendPayload, updatePayload } from "@/lib/console-api"
import { loaderEndpoint } from "@/lib/console-helpers"
import type { PayloadEntry, PayloadTarget } from "@/lib/console-types"
import { errorText, fmtBytes } from "@/lib/format"
import { isTyping, setPanelKeys } from "@/lib/keys"
import { clamp } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"

type Props = { target: ConsoleKind; settings: Settings; demo: boolean; onReceiverLoaded: (target: ConsoleKind) => void; onHints: (hints: Hint[]) => void }
type SessionSend = { id: string; name: string; at: number; result: string; ok: boolean }

const PREVIEW = "Offline preview: nothing was sent to the console."
const time = (at: number) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
const when = (at: number) => {
  const day = new Date(at), today = new Date()
  return day.toDateString() === today.toDateString() ? time(at) : day.toLocaleDateString([], { month: "short", day: "numeric" })
}

export function PayloadsPanel({ target, settings, demo, onReceiverLoaded, onHints }: Props) {
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
  const fileRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    let live = true
    setLoading(true)
    listPayloads({ demo }).then(result => { if (live) { setPayloads(result); setError("") } }).catch(reason => { if (live) setError(errorText(reason)) }).finally(() => { if (live) setLoading(false) })
    return () => { live = false }
  }, [demo])

  const shown = useMemo(() => payloads
    .filter(payload => payload.target === "any" || payload.target === target)
    .sort((a, b) => Number(b.builtin) - Number(a.builtin) || a.name.localeCompare(b.name)), [payloads, target])
  const current = shown.find(payload => payload.id === selected) || shown[0]

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
      setHistory(old => [{ id: `${entry.id}-${Date.now()}`, name: entry.name, at: Date.now(), result: result.message, ok: true }, ...old].slice(0, 20))
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

  /* ---------------------------------------------------------------- keys and dock */
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
  useEffect(() => {
    onHints([
      { key: "Enter", label: current ? `Send ${current.name}` : "Send", disabled: !current || !endpoint.host || !!busy, run: () => current && void send(current) },
      { key: "A", glyph: "A", face: "square", label: "Add payloads", run: () => void choose() },
      ...(current && !current.builtin ? [{ key: "R", glyph: "R", face: "neutral", label: "Rename", run: () => setEditing(current.id) } as Hint, { key: "Delete", label: "Remove", run: () => setRemoving(current.id) } as Hint] : []),
    ])
  }, [current?.id, endpoint.host, busy])

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
                  {entry.lastSentAt ? <><b>Sent {when(entry.lastSentAt)}</b><small>{entry.lastResult}</small></> : <b>Not sent yet</b>}
                </span>
                <button type="button" className={`btn sm ${isCurrent ? "primary" : ""} pl-send`} disabled={!endpoint.host || !!busy} onClick={event => { event.stopPropagation(); setSelected(entry.id); void send(entry) }}>
                  {busy === entry.id ? <span className="spinner" /> : <Icon name="send" />}{busy === entry.id ? "Sending" : "Send"}
                </button>
              </div>
              <Collapse open={editing === entry.id} className="pl-drawer">
                <PayloadEditor entry={entry} busy={busy === entry.id} onCancel={() => setEditing("")} onSave={draft => void save(entry, draft)} />
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
      {history.length > 0 && (
        <details className="tech pl-log">
          <summary>This session: {history.length === 1 ? "1 send" : `${history.length} sends`}, last {history[0].name} at {time(history[0].at)}</summary>
          <div className="tech-body">
            {history.map(item => <p key={item.id} className={item.ok ? "" : "fail"}><span>{time(item.at)}</span>{item.name}: {item.result}</p>)}
          </div>
        </details>
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
