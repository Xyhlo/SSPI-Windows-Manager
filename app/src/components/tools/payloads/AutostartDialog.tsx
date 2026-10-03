/* =====================================================================
   Autostart order — the payloads SSPI starts manually or on a verified wake, top to
   bottom, each after its own wait. Drag rows to reorder, drag from the
   library to add, drag back to the library to remove. Alt+↑/↓ moves the
   focused payload, Delete removes it.
   ===================================================================== */
import * as RD from "@radix-ui/react-dialog"
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent, type ReactNode, type RefObject } from "react"
import { createPortal } from "react-dom"
import { Switch } from "../../Controls"
import { Icon } from "../../Icon"
import type { PayloadEntry } from "@/lib/console-types"
import { errorText, fmtBytes } from "@/lib/format"
import { AUTOSTART_MAX_DELAY_MS, AUTOSTART_MAX_STEPS, type AutostartOrder, type AutostartStatus } from "@/lib/launcher-api"
import { SPRINGS, clamp, motionOK } from "@/lib/motion"
import type { ConsoleKind } from "@/types"

type Item = { payloadId: string; delayMs: number; processName?: string }
type Drag = { from: "seq" | "lib"; id: string; x: number; y: number; dx: number; dy: number; w: number; over: number | null; overLib: boolean; origin: number }
type Props = {
  open: boolean; target: ConsoleKind; loader: string; payloads: PayloadEntry[]; status?: AutostartStatus
  /** A payload to add when the editor opens (from a row's menu). */
  adding?: string
  onClose: () => void; onSave: (enabled: boolean, order: AutostartOrder) => Promise<void>
}

const DEFAULT_DELAY = 2000
const seconds = (ms: number) => `${Math.round(ms / 100) / 10}`.replace(/\.0$/, "")

export function AutostartDialog({ open, target, loader, payloads, status, adding, onClose, onSave }: Props) {
  const [order, setOrder] = useState<Item[]>([])
  const [enabled, setEnabled] = useState(true)
  const [search, setSearch] = useState("")
  const [drag, setDrag] = useState<Drag | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState("")
  const [fresh, setFresh] = useState("")
  const seqRef = useRef<HTMLOListElement>(null)
  const libRef = useRef<HTMLElement>(null)
  const name = target.toUpperCase()
  const byId = useMemo(() => new Map(payloads.map(entry => [entry.id, entry])), [payloads])

  useEffect(() => {
    if (!open) return
    const saved = (status?.steps || []).map(step => ({ payloadId: step.payloadId, delayMs: step.delayMs, processName: step.processName || "" }))
    const next = adding && !saved.some(item => item.payloadId === adding) ? [...saved, { payloadId: adding, delayMs: saved.length ? DEFAULT_DELAY : 0 }] : saved
    setOrder(next); setEnabled(status?.enabled || !saved.length || !!adding); setSearch(""); setError(""); setDrag(null); setFresh(adding || "")
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open])

  const inOrder = new Set(order.map(item => item.payloadId))
  const needle = search.trim().toLowerCase()
  const library = payloads.filter(entry => !needle || `${entry.name} ${entry.fileName} ${entry.notes || ""}`.toLowerCase().includes(needle))
  const full = order.length >= AUTOSTART_MAX_STEPS
  const total = order.reduce((sum, item) => sum + item.delayMs, 0)
  const missing = order.filter(item => !byId.has(item.payloadId)).length

  const add = (id: string, at?: number) => {
    if (inOrder.has(id) || full) return
    setOrder(old => {
      if (old.some(item => item.payloadId === id) || old.length >= AUTOSTART_MAX_STEPS) return old
      const next = [...old]; next.splice(at ?? old.length, 0, { payloadId: id, delayMs: old.length ? DEFAULT_DELAY : 0 }); return next
    })
    setFresh(id)
  }
  const remove = (id: string) => setOrder(old => old.filter(item => item.payloadId !== id))
  const move = (id: string, to: number) => setOrder(old => {
    const from = old.findIndex(item => item.payloadId === id)
    if (from < 0) return old
    const next = [...old]; const [item] = next.splice(from, 1); next.splice(clamp(to, 0, next.length), 0, item); return next
  })
  const setDelay = (id: string, ms: number) => setOrder(old => old.map(item => item.payloadId === id ? { ...item, delayMs: clamp(Math.round(ms / 500) * 500, 0, AUTOSTART_MAX_DELAY_MS) } : item))

  /* ---------------------------------------------------------------- drag */
  const pointer = (event: ReactPointerEvent<HTMLElement>, from: Drag["from"], id: string) => {
    if (event.button !== 0 || (event.target as HTMLElement).closest("button, input")) return
    if (from === "lib" && (inOrder.has(id) || full)) return
    const row = event.currentTarget, rect = row.getBoundingClientRect()
    const startX = event.clientX, startY = event.clientY
    const origin = order.findIndex(item => item.payloadId === id)
    let started = false
    const overAt = (x: number, y: number): Pick<Drag, "over" | "overLib"> => {
      const list = seqRef.current?.getBoundingClientRect()
      const lib = libRef.current?.getBoundingClientRect()
      const inList = !!list && x > list.left - 48 && x < list.right + 48 && y > list.top - 28 && y < list.bottom + 28
      const overLib = from === "seq" && !!lib && !inList && x > lib.left && x < lib.right && y > lib.top && y < lib.bottom
      if (!inList) return { over: from === "seq" && !overLib ? origin : null, overLib }
      const rows = Array.from(seqRef.current!.querySelectorAll<HTMLElement>("[data-row]"))
      return { over: rows.filter(el => { const r = el.getBoundingClientRect(); return r.top + r.height / 2 < y }).length, overLib: false }
    }
    const moveTo = (e: PointerEvent) => {
      if (!started) {
        if (Math.hypot(e.clientX - startX, e.clientY - startY) < 5) return
        started = true
        document.body.classList.add("as-dragging")
      }
      setDrag({ from, id, x: e.clientX, y: e.clientY, dx: startX - rect.left, dy: startY - rect.top, w: rect.width, origin, ...overAt(e.clientX, e.clientY) })
    }
    const end = (e: PointerEvent) => {
      window.removeEventListener("pointermove", moveTo); window.removeEventListener("pointerup", end); window.removeEventListener("pointercancel", end)
      document.body.classList.remove("as-dragging")
      if (!started) return
      const { over, overLib } = overAt(e.clientX, e.clientY)
      if (from === "seq" && overLib) remove(id)
      else if (from === "seq" && over != null) move(id, over)
      else if (from === "lib" && over != null) add(id, over)
      setDrag(null)
    }
    window.addEventListener("pointermove", moveTo); window.addEventListener("pointerup", end); window.addEventListener("pointercancel", end)
  }

  // The list as drawn: the dragged row leaves its slot and a gap opens where it would land.
  const shown: Array<Item | "gap"> = order.filter(item => !(drag?.from === "seq" && item.payloadId === drag.id))
  if (drag && drag.over != null) shown.splice(clamp(drag.over, 0, shown.length), 0, "gap")
  useFlip(seqRef, shown.map(item => item === "gap" ? "gap" : item.payloadId).join("|"))
  let position = 0

  const keyRow = (event: ReactKeyboardEvent, id: string, index: number) => {
    if (event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault(); move(id, index + (event.key === "ArrowUp" ? -1 : 1))
      requestAnimationFrame(() => seqRef.current?.querySelector<HTMLElement>(`[data-row="${CSS.escape(id)}"]`)?.focus())
    } else if (event.key === "Delete" || event.key === "Backspace") { event.preventDefault(); remove(id) }
  }

  const save = async () => {
    setSaving(true); setError("")
    try { await onSave(enabled, order.filter(item => byId.has(item.payloadId))); onClose() }
    catch (reason) { setError(errorText(reason)) }
    finally { setSaving(false) }
  }

  const dragged = drag && byId.get(drag.id)
  return (
    <RD.Root open={open} onOpenChange={next => { if (!next && !saving) onClose() }}>
      <RD.Portal>
        <RD.Overlay className="scrim" />
        <RD.Content className="dialog wide as-dialog" aria-describedby={undefined} onPointerDownOutside={event => { if (drag) event.preventDefault() }}>
          <header className="as-head">
            <div>
              <RD.Title asChild><h2>Autostart order</h2></RD.Title>
              <p className="lede">Run now, or after SSPI observes your {name} wake from rest mode. Each process is checked before sending to the {loader}. Use the exact name shown in Running payloads; unnamed steps run only when you press Run now.</p>
            </div>
            <label className="as-enable"><span>{enabled ? "On" : "Off"}</span><Switch label="Autostart on" checked={enabled} onChange={setEnabled} /></label>
          </header>

          <div className={`as-body ${enabled ? "" : "is-off"}`}>
            <section className="as-seq" aria-label="Autostart order">
              <div className="as-col-head">
                <strong>Order</strong>
                <span>{order.length ? `${order.length} payload${order.length === 1 ? "" : "s"} · ${seconds(total)} s of waits` : "Empty"}</span>
              </div>
              <ol ref={seqRef} className={`as-list scroll ${drag?.from === "lib" && drag.over != null ? "is-target" : ""}`}>
                {shown.map(item => item === "gap"
                  ? <li key="gap" data-flip="gap" className="as-gap" aria-hidden />
                  : <SeqRow key={item.payloadId} item={item} index={position++} entry={byId.get(item.payloadId)} fresh={fresh === item.payloadId}
                      onPointerDown={event => pointer(event, "seq", item.payloadId)} onKeyDown={event => keyRow(event, item.payloadId, order.indexOf(item))}
                      onDelay={ms => setDelay(item.payloadId, ms)} onProcessName={processName => setOrder(old => old.map(row => row.payloadId === item.payloadId ? { ...row, processName } : row))} onRemove={() => remove(item.payloadId)} />)}
                {!shown.length && <li className="as-empty"><Icon name="grip" /><strong>Drag payloads here</strong><span>or press + in the library. They'll be sent in this order.</span></li>}
              </ol>
            </section>

            <section ref={libRef} className={`as-lib ${drag?.overLib ? "is-drop" : ""}`} aria-label="Payload library">
              <label className="field as-search"><Icon name="search" /><input value={search} onChange={event => setSearch(event.target.value)} placeholder={`Search ${payloads.length} payloads`} aria-label="Search payloads" /></label>
              <ul className="as-lib-list scroll">
                {library.map(entry => {
                  const added = inOrder.has(entry.id)
                  return (
                    <li key={entry.id} className={`as-lib-row ${added ? "is-added" : ""}`} onPointerDown={event => pointer(event, "lib", entry.id)}
                      onDoubleClick={() => add(entry.id)}>
                      <Glyph entry={entry} />
                      <span className="as-name"><strong>{entry.name}</strong><span>{entry.fileName} · {fmtBytes(entry.size)}</span></span>
                      {added ? <span className="as-added"><Icon name="check" />In order</span>
                        : <button type="button" className="btn sm icon ghost" aria-label={`Add ${entry.name}`} title={full ? `Up to ${AUTOSTART_MAX_STEPS} payloads` : "Add to the end"} disabled={full} onClick={() => add(entry.id)}><Icon name="plus" /></button>}
                    </li>
                  )
                })}
                {!library.length && <li className="as-lib-empty">{payloads.length ? "No payloads match." : `Add payloads for your ${name} first.`}</li>}
              </ul>
              <div className="as-drop-hint" aria-hidden><Icon name="trash" />Drop to remove from the order</div>
            </section>
          </div>

          {(error || missing > 0) && <div className="inline-error"><Icon name="alert" /><span>{error || `${missing} payload${missing === 1 ? " is" : "s are"} no longer in the library and will be left out.`}</span></div>}
          <footer className="as-foot">
            <span className="as-hint"><span><kbd className="face square">Alt</kbd><kbd className="face square">↑↓</kbd>move</span><span><kbd className="face square">Del</kbd>remove</span></span>
            <button type="button" className="btn ghost" disabled={saving} onClick={onClose}>Cancel</button>
            <button type="button" className="btn primary" disabled={saving || (enabled && !order.length)} onClick={() => void save()}>{saving ? <span className="spinner" /> : <Icon name="check" />}{enabled ? "Save order" : "Save, autostart off"}</button>
          </footer>
        </RD.Content>
      </RD.Portal>
      {drag && dragged && createPortal(
        <div className={`as-ghost ${drag.overLib ? "is-removing" : ""}`} style={{ left: drag.x - drag.dx, top: drag.y - drag.dy, width: drag.w } as CSSProperties}>
          <Glyph entry={dragged} /><span className="as-name"><strong>{dragged.name}</strong><span>{dragged.fileName}</span></span>
        </div>, document.body,
      )}
    </RD.Root>
  )
}

function SeqRow({ item, index, entry, fresh, onPointerDown, onKeyDown, onDelay, onProcessName, onRemove }: {
  item: Item; index: number; entry?: PayloadEntry; fresh: boolean
  onPointerDown: (event: ReactPointerEvent<HTMLElement>) => void; onKeyDown: (event: ReactKeyboardEvent) => void
  onDelay: (ms: number) => void; onProcessName: (value: string) => void; onRemove: () => void
}) {
  return (
    <li data-row={item.payloadId} data-flip={item.payloadId} tabIndex={0} className={`as-row ${fresh ? "is-fresh" : ""} ${entry ? "" : "is-missing"}`}
      onPointerDown={onPointerDown} onKeyDown={onKeyDown} aria-label={`${index + 1}. ${entry?.name || "Missing payload"}, waits ${seconds(item.delayMs)} seconds`}>
      <span className="as-grip" aria-hidden><Icon name="grip" /></span>
      <span className="as-num">{index + 1}</span>
      {entry ? <Glyph entry={entry} /> : <span className="pl-glyph sm"><Icon name="alert" /></span>}
      <span className="as-name"><strong>{entry?.name || "Missing payload"}</strong><span>{entry ? entry.fileName : "Removed from the library"}</span>
        {!entry?.builtin && <input className="as-process-name" aria-label={`Process name for ${entry?.name || "payload"}`} placeholder="Exact process name" maxLength={39} value={item.processName || ""} onChange={event => onProcessName(event.target.value)} onKeyDown={event => event.stopPropagation()} />}
      </span>
      <Delay ms={item.delayMs} label={index === 0 ? "after start" : "after previous"} onChange={onDelay} />
      <button type="button" className="btn sm icon ghost as-x" aria-label={`Remove ${entry?.name || "payload"} from the order`} onClick={onRemove}><Icon name="x" /></button>
    </li>
  )
}

/** Wait before this payload, in half-second steps (Shift steps by 5 s). */
function Delay({ ms, label, onChange }: { ms: number; label: string; onChange: (ms: number) => void }) {
  const [text, setText] = useState(seconds(ms))
  useEffect(() => setText(seconds(ms)), [ms])
  const commit = () => { const value = Number(text.replace(",", ".")); if (Number.isFinite(value)) onChange(value * 1000); else setText(seconds(ms)) }
  return (
    <span className="as-delay" title={`Wait before sending, ${label}`}>
      <Icon name="clock" />
      <button type="button" aria-label="Shorter wait" disabled={ms <= 0} onClick={event => onChange(ms - (event.shiftKey ? 5000 : 500))}><Icon name="minus" /></button>
      <input value={text} inputMode="decimal" aria-label={`Wait in seconds, ${label}`} onChange={event => setText(event.target.value)} onBlur={commit}
        onKeyDown={event => { if (event.key === "Enter") { event.preventDefault(); commit() } event.stopPropagation() }} />
      <span className="as-unit">s</span>
      <button type="button" aria-label="Longer wait" disabled={ms >= AUTOSTART_MAX_DELAY_MS} onClick={event => onChange(ms + (event.shiftKey ? 5000 : 500))}><Icon name="plus" /></button>
    </span>
  )
}

export function Glyph({ entry, size = "sm" }: { entry: PayloadEntry; size?: "sm" | "md" }): ReactNode {
  const ext = entry.fileName.split(".").pop()?.toUpperCase() || "ELF"
  return <span className={`pl-glyph ${size} ${entry.builtin ? "builtin" : ""}`}>{entry.builtin ? <Icon name="shield" /> : ext}</span>
}

/** Rows slide to their new places when the order changes (FLIP); new rows grow in. */
function useFlip(ref: RefObject<HTMLElement | null>, key: string) {
  const tops = useRef(new Map<string, number>())
  useLayoutEffect(() => {
    const list = ref.current
    if (!list) return
    const next = new Map<string, number>()
    for (const node of Array.from(list.querySelectorAll<HTMLElement>("[data-flip]"))) {
      const id = node.dataset.flip!
      next.set(id, node.offsetTop)
      const before = tops.current.get(id)
      if (!motionOK()) continue
      if (before == null && tops.current.size && id !== "gap") node.animate([{ opacity: 0, transform: "scale(.96)" }, { opacity: 1, transform: "none" }], { duration: SPRINGS.soft.ms, easing: SPRINGS.soft.easing })
      else if (before != null && Math.abs(before - node.offsetTop) > 1) node.animate([{ transform: `translateY(${before - node.offsetTop}px)` }, { transform: "none" }], { duration: SPRINGS.snappy.ms, easing: SPRINGS.snappy.easing })
    }
    tops.current = next
  }, [key, ref])
}
