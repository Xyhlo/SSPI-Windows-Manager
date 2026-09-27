/* =====================================================================
   Toasts: typed success / info / warning / error. Errors stay until they
   are dismissed. The stack collapses into depth and fans out on hover;
   a ring counts down the time left; a swipe to the right dismisses.
   ===================================================================== */
import { iconSvg, type IconName } from "./Icon"
import { Spring, clamp, motionOK, onFrame } from "@/lib/motion"
import { caseThumb, type CoverSpec } from "@/stage/art"

export type ToastTone = "success" | "info" | "warning" | "error"
export type ToastAction = { label: string; run: () => void }
export type ToastOptions = { tone?: ToastTone; title: string; text?: string; game?: CoverSpec | null; actions?: ToastAction[]; duration?: number }

type Item = {
  el: HTMLElement; total: number; remaining: number; height: number
  y: Spring; x: Spring; s: Spring; o: Spring
  dragging: boolean; dismissing: boolean; circle: SVGCircleElement | null
}

const esc = (value: string) => value.replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!))
const CIRC = 2 * Math.PI * 12
const toneIcon: Record<ToastTone, IconName> = { success: "check", error: "x", warning: "warn", info: "info" }
const toneGlyph: Record<ToastTone, IconName> = { success: "checkCircle", error: "alert", warning: "warn", info: "info" }

const items: Item[] = []
let expanded = false
let started = false

function host() { return document.getElementById("toasts") }

function start() {
  if (started) return
  started = true
  onFrame(update)
}

export function toast(options: ToastOptions) {
  const container = host()
  if (!container) { console.warn(options.title, options.text || ""); return }
  start()
  const tone = options.tone || "info"
  const el = document.createElement("div")
  el.className = `toast ${tone}`
  el.setAttribute("role", tone === "error" ? "alert" : "status")
  const glyph = options.game ? `<img alt=""><span class="badge">${iconSvg(toneIcon[tone])}</span>` : iconSvg(toneGlyph[tone])
  const actions = options.actions || []
  el.innerHTML = `<div class="t-icon">${glyph}</div>
    <div class="t-body"><div class="t-title">${esc(options.title)}</div>${options.text ? `<div class="t-text">${esc(options.text)}</div>` : ""}
    ${actions.length ? `<div class="t-actions">${actions.map((a, i) => `<button type="button" class="btn sm ${i ? "ghost" : ""}" data-i="${i}">${esc(a.label)}</button>`).join("")}</div>` : ""}</div>
    <button type="button" class="t-close" aria-label="Dismiss notification"><svg class="timer" viewBox="0 0 28 28"><circle cx="14" cy="14" r="12" stroke-dasharray="${CIRC}" stroke-dashoffset="0"/></svg>${iconSvg("x")}</button>`
  if (options.game) {
    const img = el.querySelector(".t-icon img") as HTMLImageElement
    void caseThumb(options.game).then(url => { img.src = url }).catch(() => img.remove())
  }
  container.appendChild(el)
  const total = options.duration ?? (tone === "error" ? Infinity : 5600)
  const motion = motionOK()
  const item: Item = {
    el, total, remaining: total, height: 0,
    y: new Spring(-26, 260, 26), x: new Spring(motion ? 60 : 0, 300, 28), s: new Spring(0.92, 300, 24), o: new Spring(0, 260, 30),
    dragging: false, dismissing: false, circle: el.querySelector(".timer circle"),
  }
  if (!motion) { item.y.snap(0); item.x.snap(0); item.s.snap(1); item.o.snap(1) }
  items.unshift(item)
  el.querySelector(".t-close")!.addEventListener("click", () => dismiss(item))
  el.querySelectorAll<HTMLButtonElement>(".t-actions button").forEach(btn => btn.addEventListener("click", () => { actions[Number(btn.dataset.i)]?.run(); dismiss(item) }))
  el.addEventListener("pointerenter", () => { expanded = true })
  el.addEventListener("pointerleave", () => { expanded = false })
  bindSwipe(item)
  while (items.filter(i => !i.dismissing).length > 5) dismiss(items.filter(i => !i.dismissing).pop())
  requestAnimationFrame(() => { item.height = el.offsetHeight })
}

function dismiss(item?: Item) {
  if (!item || item.dismissing) return
  item.dismissing = true
  item.x.set(motionOK() ? 420 : 0)
  item.o.set(0)
  window.setTimeout(() => {
    item.el.remove()
    const index = items.indexOf(item)
    if (index >= 0) items.splice(index, 1)
    if (!items.length) expanded = false
  }, motionOK() ? 420 : 0)
}

function bindSwipe(item: Item) {
  let startX = 0, lastX = 0, lastT = 0, v = 0
  item.el.addEventListener("pointerdown", event => {
    if ((event.target as Element).closest("button")) return
    item.dragging = true
    startX = lastX = event.clientX
    lastT = performance.now()
    v = 0
    item.el.setPointerCapture(event.pointerId)
  })
  item.el.addEventListener("pointermove", event => {
    if (!item.dragging) return
    const now = performance.now()
    v = (event.clientX - lastX) / Math.max(1, now - lastT)
    lastX = event.clientX
    lastT = now
    const dx = event.clientX - startX
    item.x.snap(dx > 0 ? dx : dx * 0.25)
  })
  const end = () => {
    if (!item.dragging) return
    item.dragging = false
    if (item.x.value > 110 || v > 0.6) dismiss(item)
    else item.x.set(0)
  }
  item.el.addEventListener("pointerup", end)
  item.el.addEventListener("pointercancel", end)
}

function update(dt: number) {
  if (!items.length) return
  let y = 0, depth = 0
  const live = items.filter(i => !i.dismissing)
  for (const item of items) {
    if (!item.height) item.height = item.el.offsetHeight || 84
    if (!item.dismissing) {
      if (expanded || live.length === 1) { item.y.set(y); item.s.set(1); item.o.set(1); y += item.height + 10 }
      else { item.y.set(depth * 12); item.s.set(1 - depth * 0.05); item.o.set(depth > 2 ? 0 : 1 - depth * 0.22) }
      if (!item.dragging && item.x.target !== 0) item.x.set(0)
      depth++
      if (!expanded && Number.isFinite(item.total) && !document.hidden) {
        item.remaining -= dt * 1000
        if (item.remaining <= 0) dismiss(item)
      }
      if (item.circle) {
        if (Number.isFinite(item.total)) item.circle.setAttribute("stroke-dashoffset", String(CIRC * (1 - Math.max(0, item.remaining) / item.total)))
        else item.circle.style.opacity = "0"
      }
    }
    const yy = item.y.step(dt), xx = item.dragging ? item.x.value : item.x.step(dt), ss = item.s.step(dt), oo = item.o.step(dt)
    item.el.style.transform = `translate3d(${xx}px, ${yy}px, 0) scale(${ss})`
    item.el.style.opacity = String(clamp(oo, 0, 1))
    item.el.style.zIndex = String(100 - items.indexOf(item))
    item.el.style.pointerEvents = oo > 0.3 ? "auto" : "none"
  }
}
