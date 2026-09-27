/* Motion language shared by the WebGL stage and the DOM: springs, eased curves and a frame ticker. */

export const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value))
export const lerp = (a: number, b: number, t: number) => a + (b - a) * t
export const damp = (a: number, b: number, lambda: number, dt: number) => lerp(a, b, 1 - Math.exp(-lambda * dt))
export const sleep = (ms: number) => new Promise<void>(resolve => window.setTimeout(resolve, ms))

export const E = {
  outCubic: (t: number) => 1 - (1 - t) ** 3,
  inOutCubic: (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2),
  inCubic: (t: number) => t * t * t,
  outBack: (t: number) => { const c1 = 1.35, c3 = c1 + 1; return 1 + c3 * (t - 1) ** 3 + c1 * (t - 1) ** 2 },
}

/** Resolves when a Web Animation finishes, or after a timeout if the window is not painting. */
export const settle = (animation: Animation, ms?: number) => Promise.race([
  animation.finished.then(() => undefined, () => undefined),
  sleep(ms ?? (Number(animation.effect?.getTiming().duration) || 300) + 60),
])

/** Spring integrated with fixed substeps, so it behaves the same at any frame rate. */
export class Spring {
  value: number
  target: number
  velocity = 0
  constructor(value = 0, private k = 180, private c = 24, private m = 1) {
    this.value = value
    this.target = value
  }
  set(target: number) { this.target = target; return this }
  snap(value: number) { this.value = this.target = value; this.velocity = 0; return this }
  step(dt: number) {
    const steps = Math.max(1, Math.ceil(dt / (1 / 240)))
    const h = dt / steps
    for (let i = 0; i < steps; i++) {
      const force = -this.k * (this.value - this.target) - this.c * this.velocity
      this.velocity += (force / this.m) * h
      this.value += this.velocity * h
    }
    return this.value
  }
  get settled() { return Math.abs(this.value - this.target) < 5e-4 && Math.abs(this.velocity) < 5e-3 }
}

/** A spring expressed as a CSS linear() easing so DOM transitions share the WebGL motion. */
function springCurve(stiffness: number, damping: number) {
  let x = 0, v = 0, t = 0
  const dt = 1 / 240, points = [0]
  while (t < 2.5) {
    for (let i = 0; i < 4; i++) { const f = -stiffness * (x - 1) - damping * v; v += f * dt; x += v * dt; t += dt }
    points.push(x)
    if (Math.abs(x - 1) < 0.0015 && Math.abs(v) < 0.01) break
  }
  points[points.length - 1] = 1
  const samples = Math.min(48, points.length)
  const out: string[] = []
  for (let i = 0; i < samples; i++) out.push(points[Math.round((i / (samples - 1)) * (points.length - 1))].toFixed(4))
  return { easing: `linear(${out.join(", ")})`, ms: Math.round(t * 1000) }
}

export const SPRINGS = {
  snappy: springCurve(520, 38),
  soft: springCurve(190, 24),
  bouncy: springCurve(300, 17),
}

;(() => {
  const supported = typeof CSS !== "undefined" && CSS.supports("transition-timing-function", "linear(0, 1)")
  const root = document.documentElement.style
  for (const [name, curve] of Object.entries(SPRINGS)) {
    if (supported) root.setProperty(`--spring-${name}`, curve.easing)
    else curve.easing = "cubic-bezier(.2,.9,.25,1)"
    root.setProperty(`--spring-${name}-ms`, `${curve.ms}ms`)
  }
})()

const reducedQuery = window.matchMedia("(prefers-reduced-motion: reduce)")
// The saved setting arrives asynchronously; the last known value applies to the intro at launch.
let reduceSetting = (() => { try { return window.localStorage.getItem("sspi.reduceMotion") === "1" } catch { return false } })()
document.documentElement.classList.toggle("reduce-motion", reduceSetting)
export const setReduceMotion = (value: boolean) => {
  reduceSetting = value
  document.documentElement.classList.toggle("reduce-motion", value)
}
export const motionOK = () => !reduceSetting && !reducedQuery.matches

/* ---------------------------------------------------------------- frame ticker */
type FrameFn = (dt: number, now: number) => void
const frameFns = new Set<FrameFn>()
let last = performance.now()
let running = false

function frame(now: number) {
  window.requestAnimationFrame(frame)
  const dt = Math.min(0.1, Math.max(0, (now - last) / 1000))
  last = now
  for (const fn of [...frameFns]) {
    try { fn(dt, now) } catch (error) { console.error(error) }
  }
}

/** Runs `fn` every animation frame until the returned function is called. */
export function onFrame(fn: FrameFn) {
  frameFns.add(fn)
  if (!running) { running = true; last = performance.now(); window.requestAnimationFrame(frame) }
  return () => { frameFns.delete(fn) }
}

if (import.meta.env.DEV) {
  // Development only: advance every frame subscriber in fixed 60 Hz steps (for previews that throttle animation frames).
  let offset = 0
  ;(window as unknown as { __sspiStep: (seconds: number) => number }).__sspiStep = (seconds: number) => {
    const steps = Math.round(seconds * 60)
    for (let i = 0; i < steps; i++) {
      offset += 1000 / 60
      for (const fn of [...frameFns]) {
        try { fn(1 / 60, performance.now() + offset) } catch (error) { console.error(error) }
      }
    }
    last = performance.now()
    return steps
  }
}

/** Sliding indicator for tab strips and segmented controls, with an optional stretchy "liquid" move. */
export function glide(thumb: HTMLElement | null, target: HTMLElement | null, container: HTMLElement | null, options: { inset?: number; liquid?: boolean; instant?: boolean } = {}) {
  if (!thumb || !target || !container) return
  const c = container.getBoundingClientRect(), r = target.getBoundingClientRect()
  const inset = options.inset || 0
  const left = r.left - c.left + inset, width = Math.max(4, r.width - inset * 2)
  const from = thumb.dataset.left != null ? { l: Number(thumb.dataset.left), w: Number(thumb.dataset.width) } : null
  thumb.dataset.left = String(left)
  thumb.dataset.width = String(width)
  thumb.style.width = `${width}px`
  thumb.style.transform = `translateX(${left}px)`
  if (!from || !motionOK() || options.instant || (Math.abs(from.l - left) < 0.5 && Math.abs(from.w - width) < 0.5)) return
  const stretchL = Math.min(from.l, left), stretchW = Math.max(from.l + from.w, left + width) - stretchL
  thumb.animate(options.liquid ? [
    { transform: `translateX(${from.l}px)`, width: `${from.w}px` },
    { transform: `translateX(${stretchL}px)`, width: `${stretchW}px`, offset: 0.45 },
    { transform: `translateX(${left}px)`, width: `${width}px` },
  ] : [
    { transform: `translateX(${from.l}px)`, width: `${from.w}px` },
    { transform: `translateX(${left}px)`, width: `${width}px` },
  ], { duration: options.liquid ? 520 : SPRINGS.snappy.ms, easing: options.liquid ? "cubic-bezier(.3,.7,.1,1)" : SPRINGS.snappy.easing })
}

/** Material-style ripple for .btn elements, sized from the press point. */
function ripple(event: PointerEvent, el: HTMLElement) {
  if (!motionOK()) return
  const r = el.getBoundingClientRect(), size = Math.max(r.width, r.height) * 2.2
  const dot = document.createElement("span")
  dot.className = "ripple"
  dot.style.cssText = `width:${size}px;height:${size}px;left:${(event.clientX || r.left + r.width / 2) - r.left - size / 2}px;top:${(event.clientY || r.top + r.height / 2) - r.top - size / 2}px`
  el.appendChild(dot)
  void settle(dot.animate([{ transform: "scale(0)", opacity: 0.2 }, { transform: "scale(1)", opacity: 0 }], { duration: 620, easing: "cubic-bezier(.2,.8,.2,1)" })).then(() => dot.remove())
}
document.addEventListener("pointerdown", event => {
  const btn = (event.target as Element | null)?.closest?.(".btn") as HTMLButtonElement | null
  if (btn && !btn.disabled) ripple(event, btn)
})
