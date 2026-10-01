/* =====================================================================
   PS4 themes: one look applied to everything the console lets a theme
   change, plus the game icons the receiver can mask.
   - A real system theme (build_ps4_theme): a still wallpaper, the 10
     content-area tiles (Library, TV & Video, browser, folders...) and the
     9 function-area icons, each from SSPI's art or the user's own PNG.
   - Game icons: the same shape and border written over each title's
     icon through the receiver, which keeps the originals.
   Stock renderers draw an approximation of the console's default look
   so the preview can show what changes.
   ===================================================================== */
import type { ThemePackageRequest } from "./console-types"
import { ART, CONTENT_SLOTS, FUNCTION_SLOTS, type ContentSlot, type FunctionSlot, type SlotId, type Stroke } from "./ps4-icons"

export type IconShape = "square" | "rounded" | "squircle" | "circle" | "hexagon" | "octagon"
export type GlyphStyle = "neon" | "line"
export type Backplate = "dark" | "glass" | "none"
/** How a custom image sits in its slot: masked to fill the tile, centred on the theme's tile, or exactly as supplied. */
export type Fit = "fill" | "glyph" | "asis"
export type ThemeSpec = {
  name: string
  /** "neon" draws SSPI's retro sunset; "image" uses the stored wallpaper. */
  wallpaper: "neon" | "image"
  dim: number
  glyph: GlyphStyle
  primary: string
  secondary: string
  backplate: Backplate
  tile: string
  shape: IconShape
  radius: number
  border: number
  /** Empty follows the primary colour. */
  borderColor: string
  glass: boolean
  /** Top-row icons as bare symbols (like the console's) or on the theme's tiles. */
  topRow: "glyph" | "tile"
  focus: string
  fit: Partial<Record<SlotId, Fit>>
}
/** Custom art per slot, plus the wallpaper, as loaded images. */
export type ThemeImages = Partial<Record<SlotId | "wallpaper", HTMLImageElement>>

export const SHAPES: Array<[IconShape, string]> = [["square", "Square"], ["rounded", "Rounded"], ["squircle", "Squircle"], ["circle", "Circle"], ["hexagon", "Hexagon"], ["octagon", "Octagon"]]
export const PRESETS: Record<"neon" | "glass" | "mono", { label: string; spec: Partial<ThemeSpec> }> = {
  neon: { label: "Retro neon", spec: { wallpaper: "neon", glyph: "neon", primary: "#ff3ec8", secondary: "#38e8ff", backplate: "dark", tile: "#1d0b38", shape: "rounded", radius: 0.2, border: 6, borderColor: "", glass: false, topRow: "glyph", focus: "#ff3ec8", dim: 0 } },
  glass: { label: "Frosted glass", spec: { glyph: "line", primary: "#ffffff", secondary: "#9ad7ff", backplate: "glass", shape: "squircle", radius: 0.22, border: 4, borderColor: "#ffffff", glass: true, topRow: "glyph", focus: "#ffffff" } },
  mono: { label: "Minimal", spec: { glyph: "line", primary: "#f2f2f2", secondary: "#f2f2f2", backplate: "none", shape: "rounded", radius: 0.2, border: 0, borderColor: "", glass: false, topRow: "glyph", focus: "#f2f2f2" } },
}
export const DEFAULT_SPEC: ThemeSpec = { name: "SSPI Retro Neon", dim: 0, fit: {}, ...(PRESETS.neon.spec as Omit<ThemeSpec, "name" | "fit" | "dim">) }

export function makeCanvas(w: number, h = w) {
  const canvas = document.createElement("canvas")
  canvas.width = w
  canvas.height = h
  return canvas
}
const tick = () => new Promise(resolve => setTimeout(resolve, 0))
const pngData = (canvas: HTMLCanvasElement) => canvas.toDataURL("image/png")
export const borderOf = (spec: ThemeSpec) => spec.borderColor || spec.primary

/* ---------------------------------------------------------------- colour */

function rgb(hex: string): [number, number, number] {
  const n = parseInt(hex.replace("#", "").padEnd(6, "0").slice(0, 6), 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}
const css = ([r, g, b]: number[], a = 1) => `rgba(${Math.round(r)},${Math.round(g)},${Math.round(b)},${a})`
const mix = (hex: string, to: string, t: number) => { const a = rgb(hex), b = rgb(to); return css(a.map((v, i) => v + (b[i] - v) * t)) }
const alpha = (hex: string, a: number) => css(rgb(hex), a)

/* ---------------------------------------------------------------- shapes */

/** Traces the icon shape in the square (x, y, w). Every shape is symmetric, so a border inset by half its width follows it. */
export function shapePath(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, shape: IconShape, radius: number) {
  const cx = x + w / 2, cy = y + w / 2, a = w / 2
  ctx.beginPath()
  switch (shape) {
    case "circle": ctx.arc(cx, cy, a, 0, Math.PI * 2); return
    case "squircle":
      for (let i = 0; i <= 120; i++) {
        const t = (i / 120) * Math.PI * 2, c = Math.cos(t), s = Math.sin(t)
        const px = cx + a * Math.sign(c) * Math.abs(c) ** 0.5, py = cy + a * Math.sign(s) * Math.abs(s) ** 0.5
        if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py)
      }
      ctx.closePath()
      return
    case "hexagon": {
      const h = a * 0.5
      for (const [i, [px, py]] of [[0, -a], [a, -h], [a, h], [0, a], [-a, h], [-a, -h]].entries()) i ? ctx.lineTo(cx + px, cy + py) : ctx.moveTo(cx + px, cy + py)
      ctx.closePath()
      return
    }
    case "octagon": {
      const k = a * Math.tan(Math.PI / 8)
      for (const [i, [px, py]] of [[k, -a], [a, -k], [a, k], [k, a], [-k, a], [-a, k], [-a, -k], [-k, -a]].entries()) i ? ctx.lineTo(cx + px, cy + py) : ctx.moveTo(cx + px, cy + py)
      ctx.closePath()
      return
    }
    case "rounded": ctx.roundRect(x, y, w, w, Math.min(0.5, Math.max(0, radius)) * w); return
    default: ctx.rect(x, y, w, w)
  }
}
/** A shape inset by `d`: the same centre, so a rounded corner's radius shrinks by `d`. */
function insetShape(ctx: CanvasRenderingContext2D, size: number, d: number, spec: Pick<ThemeSpec, "shape" | "radius">) {
  shapePath(ctx, d, d, size - d * 2, spec.shape, Math.max(0, spec.radius * size - d) / (size - d * 2))
}

/* ---------------------------------------------------------------- strokes */

type Look = "neon" | "line" | "stock"
const paths = new Map<string, Path2D>()
const path2d = (d: string) => { let p = paths.get(d); if (!p) { p = new Path2D(d); paths.set(d, p) } return p }

/** Draws an icon's strokes into a box of `box` pixels at (x, y). */
export function paintArt(ctx: CanvasRenderingContext2D, art: Stroke[], look: Look, colors: [string, string], x: number, y: number, box: number) {
  const u = box / 512
  ctx.save()
  ctx.translate(x, y)
  ctx.scale(u, u)
  ctx.lineCap = "round"
  ctx.lineJoin = "round"
  for (const stroke of art) {
    const p = path2d(stroke.d), w = stroke.w ?? 20
    const color = look === "stock" ? "#ffffff" : look === "line" ? (stroke.tone ? colors[1] : "#ffffff") : colors[stroke.tone]
    if (look !== "neon") {
      ctx.strokeStyle = color; ctx.fillStyle = color
      ctx.lineWidth = w * (look === "stock" ? 1.15 : 1)
      if (stroke.fill) ctx.fill(p)
      ctx.stroke(p)
      continue
    }
    // Neon: a wide soft bloom, the coloured tube, then its hot white core.
    ctx.shadowColor = color
    ctx.strokeStyle = color
    ctx.fillStyle = color
    ctx.globalAlpha = 0.5
    ctx.shadowBlur = 34 * u
    ctx.lineWidth = w * 1.3
    if (stroke.fill) ctx.fill(p)
    ctx.stroke(p)
    ctx.globalAlpha = 1
    ctx.shadowBlur = 12 * u
    ctx.lineWidth = w
    if (stroke.fill) ctx.fill(p)
    ctx.stroke(p)
    ctx.shadowBlur = 0
    ctx.strokeStyle = mix(color, "#ffffff", 0.72)
    ctx.fillStyle = ctx.strokeStyle
    ctx.lineWidth = w * 0.36
    if (stroke.fill) { ctx.globalAlpha = 0.55; ctx.fill(p); ctx.globalAlpha = 1 }
    ctx.stroke(p)
  }
  ctx.restore()
}

/* ---------------------------------------------------------------- tiles */

function backplate(ctx: CanvasRenderingContext2D, size: number, spec: ThemeSpec) {
  if (spec.backplate === "glass") {
    const frost = ctx.createLinearGradient(0, 0, 0, size)
    frost.addColorStop(0, "rgba(255,255,255,.30)")
    frost.addColorStop(1, "rgba(255,255,255,.10)")
    ctx.fillStyle = frost
    ctx.fillRect(0, 0, size, size)
    return
  }
  if (spec.backplate !== "dark") return
  const ground = ctx.createLinearGradient(0, 0, 0, size)
  ground.addColorStop(0, mix(spec.tile, "#000000", 0.05))
  ground.addColorStop(1, mix(spec.tile, "#000000", 0.7))
  ctx.fillStyle = ground
  ctx.fillRect(0, 0, size, size)
  // A faint retro floor grid in the lower third. Kept sparse: busy detail makes the PNG too
  // large for the PS4, which rejects themes with tile files much over 128 KB.
  const horizon = size * 0.66
  ctx.save()
  ctx.strokeStyle = alpha(spec.primary, 0.18)
  ctx.lineWidth = Math.max(1, size / 200)
  for (let i = -3; i <= 3; i++) { ctx.beginPath(); ctx.moveTo(size / 2 + i * size * 0.08, horizon); ctx.lineTo(size / 2 + i * size * 0.3, size); ctx.stroke() }
  for (let k = 1; k <= 3; k++) { const y = horizon + (size - horizon) * (k / 3) ** 1.6; ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(size, y); ctx.stroke() }
  const shine = ctx.createLinearGradient(0, 0, 0, size * 0.4)
  shine.addColorStop(0, "rgba(255,255,255,.08)")
  shine.addColorStop(1, "rgba(255,255,255,0)")
  ctx.fillStyle = shine
  ctx.fillRect(0, 0, size, size)
  ctx.restore()
}
/** Border and glass over a finished tile; both stay inside the shape so its edge is exactly the mask. */
function finish(ctx: CanvasRenderingContext2D, size: number, spec: ThemeSpec) {
  const u = size / 512
  ctx.save()
  shapePath(ctx, 0, 0, size, spec.shape, spec.radius)
  ctx.clip()
  if (spec.glass) {
    ctx.save()
    ctx.beginPath()
    ctx.ellipse(size / 2, -size * 0.18, size * 0.9, size * 0.62, 0, 0, Math.PI * 2)
    ctx.clip()
    const sheen = ctx.createLinearGradient(0, 0, 0, size * 0.44)
    sheen.addColorStop(0, "rgba(255,255,255,.40)")
    sheen.addColorStop(1, "rgba(255,255,255,.04)")
    ctx.fillStyle = sheen
    ctx.fillRect(0, 0, size, size)
    ctx.restore()
  }
  const border = Math.max(0, Math.min(32, spec.border)) * u
  if (border) {
    const color = borderOf(spec)
    insetShape(ctx, size, border / 2, spec)
    ctx.lineWidth = border
    ctx.strokeStyle = color
    if (spec.glyph === "neon") { ctx.shadowColor = color; ctx.shadowBlur = 22 * u }
    ctx.stroke()
    ctx.shadowBlur = 0
    if (spec.glyph === "neon") { ctx.lineWidth = border * 0.35; ctx.strokeStyle = mix(color, "#ffffff", 0.7); ctx.stroke() }
  }
  if (spec.glass) {
    const rim = Math.max(1, 3 * u)
    insetShape(ctx, size, border + rim / 2, spec)
    ctx.lineWidth = rim
    ctx.strokeStyle = "rgba(255,255,255,.38)"
    ctx.stroke()
  }
  ctx.restore()
}
function cover(ctx: CanvasRenderingContext2D, image: CanvasImageSource & { width: number; height: number }, x: number, y: number, w: number, h: number, contain = false) {
  const scale = (contain ? Math.min : Math.max)(w / image.width, h / image.height)
  ctx.imageSmoothingQuality = "high"
  ctx.drawImage(image, x + (w - image.width * scale) / 2, y + (h - image.height * scale) / 2, image.width * scale, image.height * scale)
}
const colorsOf = (spec: ThemeSpec): [string, string] => [spec.primary, spec.secondary]

/** A content-area tile (512 for the package): SSPI's art or the user's image in the theme's style. */
export function renderContent(slot: ContentSlot, spec: ThemeSpec, custom: HTMLImageElement | null, size: number) {
  const canvas = makeCanvas(size)
  const ctx = canvas.getContext("2d")!
  const fit = spec.fit[slot] ?? "fill"
  if (custom && fit === "asis") { cover(ctx, custom, 0, 0, size, size, true); return canvas }
  if (slot === "discoverlay" && !custom) { paintArt(ctx, ART.discoverlay, spec.glyph, colorsOf(spec), 0, 0, size); return canvas }
  ctx.save()
  shapePath(ctx, 0, 0, size, spec.shape, spec.radius)
  ctx.clip()
  if (custom && fit === "fill") cover(ctx, custom, 0, 0, size, size)
  else backplate(ctx, size, spec)
  ctx.restore()
  if (custom && fit === "glyph") cover(ctx, custom, size * 0.16, size * 0.16, size * 0.68, size * 0.68, true)
  // The art is drawn a little larger than its 512 box so it fills a tile the way the console's icons do.
  else if (!custom) paintArt(ctx, ART[slot], spec.glyph, colorsOf(spec), -size * 0.08, -size * 0.08, size * 1.16)
  finish(ctx, size, spec)
  return canvas
}
/** A function-area icon (128 for the package); `glow` gives the focused 152 px version with a halo. */
export function renderFunction(slot: FunctionSlot, spec: ThemeSpec, custom: HTMLImageElement | null, size: number) {
  const canvas = makeCanvas(size)
  const ctx = canvas.getContext("2d")!
  const fit = spec.fit[slot] ?? "asis"
  if (custom && fit === "asis") { cover(ctx, custom, 0, 0, size, size, true); return canvas }
  if (spec.topRow === "tile" || custom) {
    ctx.save()
    shapePath(ctx, 0, 0, size, spec.shape, spec.radius)
    ctx.clip()
    backplate(ctx, size, spec)
    ctx.restore()
    if (custom) cover(ctx, custom, size * 0.16, size * 0.16, size * 0.68, size * 0.68, true)
    else paintArt(ctx, ART[slot], spec.glyph, colorsOf(spec), size * 0.12, size * 0.12, size * 0.76)
    finish(ctx, size, spec)
    return canvas
  }
  paintArt(ctx, ART[slot], spec.glyph, colorsOf(spec), size * 0.04, size * 0.04, size * 0.92)
  return canvas
}
function glowOf(icon: HTMLCanvasElement, color: string) {
  const glow = makeCanvas(152)
  const ctx = glow.getContext("2d")!
  ctx.shadowColor = color
  ctx.shadowBlur = 16
  ctx.drawImage(icon, 12, 12, 128, 128)
  ctx.shadowBlur = 6
  ctx.drawImage(icon, 12, 12, 128, 128)
  return glow
}
/** A game icon in the theme's shape and border, for the receiver to write over the title's icon. */
export function renderGameIcon(spec: ThemeSpec, art: CanvasImageSource & { width: number; height: number }, size = 512) {
  const canvas = makeCanvas(size)
  const ctx = canvas.getContext("2d")!
  ctx.save()
  shapePath(ctx, 0, 0, size, spec.shape, spec.radius)
  ctx.clip()
  cover(ctx, art, 0, 0, size, size)
  ctx.restore()
  finish(ctx, size, spec)
  return canvas
}

/* ---------------------------------------------------------------- the console's default look (preview only) */

export function stockContent(slot: ContentSlot, size: number) {
  const canvas = makeCanvas(size)
  const ctx = canvas.getContext("2d")!
  if (slot === "discoverlay") { paintArt(ctx, ART.discoverlay, "stock", ["#fff", "#fff"], 0, 0, size); return canvas }
  const ground = ctx.createLinearGradient(0, 0, 0, size)
  ground.addColorStop(0, "#1c4fc1")
  ground.addColorStop(0.62, "#1543ad")
  ground.addColorStop(1, "#0f3896")
  ctx.fillStyle = ground
  ctx.fillRect(0, 0, size, size)
  paintArt(ctx, ART[slot], "stock", ["#fff", "#fff"], -size * 0.06, -size * 0.07, size * 1.12)
  return canvas
}
export function stockFunction(slot: FunctionSlot, size: number) {
  const canvas = makeCanvas(size)
  paintArt(canvas.getContext("2d")!, ART[slot], "stock", ["#fff", "#fff"], size * 0.04, size * 0.04, size * 0.92)
  return canvas
}
/** The default blue background with its light wave. */
export function drawStockWallpaper(ctx: CanvasRenderingContext2D, w: number, h: number) {
  const ground = ctx.createLinearGradient(0, 0, w, h)
  ground.addColorStop(0, "#0d47b8")
  ground.addColorStop(0.55, "#0a3aa6")
  ground.addColorStop(1, "#06267a")
  ctx.fillStyle = ground
  ctx.fillRect(0, 0, w, h)
  ctx.save()
  ctx.lineCap = "round"
  for (const [width, a, dx] of [[w * 0.09, 0.16, 0], [w * 0.03, 0.22, w * 0.03], [w * 0.012, 0.3, w * 0.07]] as const) {
    ctx.beginPath()
    ctx.moveTo(w * 0.36 + dx, h * 1.05)
    ctx.bezierCurveTo(w * 0.42 + dx, h * 0.45, w * 0.7, h * 0.05, w * 1.05, h * 0.02)
    ctx.strokeStyle = `rgba(80,150,255,${a})`
    ctx.lineWidth = width
    ctx.stroke()
  }
  ctx.restore()
}

/* ---------------------------------------------------------------- wallpaper */

function seeded(seed: number) { return () => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed / 4294967296 } }
/** SSPI's retro neon scene: a striped sunset over a glowing grid, scaled to any size. */
export function drawNeonWallpaper(ctx: CanvasRenderingContext2D, w: number, h: number, spec: Pick<ThemeSpec, "primary" | "secondary">) {
  const k = w / 1920, horizon = h * 0.6
  const sky = ctx.createLinearGradient(0, 0, 0, horizon)
  sky.addColorStop(0, "#05010f")
  sky.addColorStop(0.55, "#1a0736")
  sky.addColorStop(0.86, mix(spec.primary, "#2a0640", 0.62))
  sky.addColorStop(1, mix(spec.primary, "#ffffff", 0.08))
  ctx.fillStyle = sky
  ctx.fillRect(0, 0, w, horizon)
  const random = seeded(7)
  for (let i = 0; i < 170; i++) {
    const x = random() * w, y = random() * horizon * 0.82, r = (random() * 1.6 + 0.4) * k
    ctx.fillStyle = `rgba(255,255,255,${0.25 + random() * 0.6})`
    ctx.fillRect(x, y, r, r)
  }
  // The sun: a warm gradient disc, cut into bands towards the horizon.
  const sx = w * 0.7, sr = h * 0.25, sy = horizon - sr * 0.12
  ctx.save()
  ctx.beginPath()
  ctx.rect(0, 0, w, horizon)
  ctx.clip()
  ctx.shadowColor = spec.primary
  ctx.shadowBlur = 90 * k
  const sun = ctx.createLinearGradient(0, sy - sr, 0, sy + sr)
  sun.addColorStop(0, "#ffe27a")
  sun.addColorStop(0.5, "#ff8a5c")
  sun.addColorStop(1, spec.primary)
  ctx.fillStyle = sun
  ctx.beginPath()
  ctx.arc(sx, sy, sr, 0, Math.PI * 2)
  ctx.fill()
  ctx.shadowBlur = 0
  ctx.globalCompositeOperation = "destination-out"
  for (let i = 0; i < 7; i++) {
    const t = i / 6, y = sy - sr * 0.08 + t * sr * 1.05, band = (3 + t * 15) * k
    ctx.fillRect(sx - sr, y, sr * 2, band)
  }
  ctx.restore()
  // Two ridges of mountains with a neon rim.
  for (const [base, height, color, seed] of [[horizon, h * 0.16, "#0e0420", 11], [horizon, h * 0.1, "#160630", 23]] as const) {
    const rnd = seeded(seed)
    ctx.beginPath()
    ctx.moveTo(0, base)
    for (let x = 0; x <= w; x += w / 14) ctx.lineTo(x, base - height * (0.25 + rnd() * 0.75))
    ctx.lineTo(w, base)
    ctx.closePath()
    ctx.fillStyle = color
    ctx.fill()
    ctx.strokeStyle = alpha(spec.secondary, 0.75)
    ctx.lineWidth = 2.2 * k
    ctx.shadowColor = spec.secondary
    ctx.shadowBlur = 14 * k
    ctx.stroke()
    ctx.shadowBlur = 0
  }
  // The floor and its perspective grid.
  const floor = ctx.createLinearGradient(0, horizon, 0, h)
  floor.addColorStop(0, "#14042b")
  floor.addColorStop(1, "#05010d")
  ctx.fillStyle = floor
  ctx.fillRect(0, horizon, w, h - horizon)
  ctx.save()
  ctx.strokeStyle = alpha(spec.primary, 0.75)
  ctx.shadowColor = spec.primary
  ctx.shadowBlur = 10 * k
  ctx.lineWidth = 2 * k
  for (let i = -26; i <= 26; i++) { ctx.beginPath(); ctx.moveTo(w / 2 + i * 38 * k, horizon); ctx.lineTo(w / 2 + i * 260 * k, h); ctx.stroke() }
  for (let i = 1; i <= 14; i++) { const y = horizon + (h - horizon) * (i / 14) ** 2.1; ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(w, y); ctx.stroke() }
  ctx.restore()
  const glow = ctx.createLinearGradient(0, horizon - 40 * k, 0, horizon + 40 * k)
  glow.addColorStop(0, alpha(spec.primary, 0))
  glow.addColorStop(0.5, alpha(spec.primary, 0.85))
  glow.addColorStop(1, alpha(spec.primary, 0))
  ctx.fillStyle = glow
  ctx.fillRect(0, horizon - 40 * k, w, 80 * k)
  // Darker at the top left, where the console puts the menu and the tiles.
  const shade = ctx.createLinearGradient(0, 0, w * 0.6, h * 0.5)
  shade.addColorStop(0, "rgba(0,0,0,.35)")
  shade.addColorStop(1, "rgba(0,0,0,0)")
  ctx.fillStyle = shade
  ctx.fillRect(0, 0, w, h)
}
export function drawWallpaper(ctx: CanvasRenderingContext2D, w: number, h: number, spec: ThemeSpec, image: HTMLImageElement | null | undefined, extra = { blur: 0, dim: 0 }) {
  ctx.save()
  if (extra.blur) ctx.filter = `blur(${extra.blur * (w / 1920)}px)`
  if (spec.wallpaper === "image" && image) cover(ctx, image, -extra.blur, -extra.blur, w + extra.blur * 2, h + extra.blur * 2)
  else if (extra.blur) {
    const sharp = makeCanvas(w, h)
    drawNeonWallpaper(sharp.getContext("2d")!, w, h, spec)
    ctx.drawImage(sharp, 0, 0)
  } else drawNeonWallpaper(ctx, w, h, spec)
  ctx.filter = "none"
  const dim = Math.max(0, Math.min(0.8, spec.dim + extra.dim))
  if (dim) { ctx.fillStyle = `rgba(0,0,0,${dim})`; ctx.fillRect(0, 0, w, h) }
  ctx.restore()
}

/* ---------------------------------------------------------------- importing images */

/**
 * A user's image at the slot's size (or 1920 × 1080 for the wallpaper), keeping its transparency.
 * `clear` is the share of fully transparent pixels: a glyph on a clear background sits best on a tile.
 */
export async function normalizeImage(src: string, target: SlotId | "wallpaper") {
  const image = await new Promise<HTMLImageElement>((resolve, reject) => { const i = new Image(); i.onload = () => resolve(i); i.onerror = () => reject(new Error("That file isn't an image this app can read.")); i.src = src })
  if (target === "wallpaper") {
    const canvas = makeCanvas(1920, 1080)
    cover(canvas.getContext("2d")!, image, 0, 0, 1920, 1080)
    return { url: canvas.toDataURL("image/jpeg", 0.92), clear: 0 }
  }
  const size = CONTENT_SLOTS.some(slot => slot.id === target) ? 512 : 128
  const canvas = makeCanvas(size)
  const ctx = canvas.getContext("2d")!
  cover(ctx, image, 0, 0, size, size, true)
  const pixels = ctx.getImageData(0, 0, size, size).data
  let clear = 0
  for (let i = 3; i < pixels.length; i += 4) if (pixels[i] < 16) clear++
  return { url: pngData(canvas), clear: clear / (size * size) }
}

/* ---------------------------------------------------------------- the package */

const argb = (hex: string) => `#FF${hex.replace("#", "").toUpperCase().padEnd(6, "0").slice(0, 6)}`
/** A 16-character [A-Z0-9] label from the theme name; the same name replaces the same theme. */
export function themeLabel(name: string) {
  let hash = 2166136261
  for (const c of name.trim().toLowerCase()) hash = Math.imul(hash ^ c.charCodeAt(0), 16777619) >>> 0
  const tail = hash.toString(36).toUpperCase().padStart(7, "0").slice(-7)
  return `SSPI${(name.toUpperCase().replace(/[^A-Z0-9]/g, "") + "THEME").slice(0, 5)}${tail}`.padEnd(16, "0").slice(0, 16)
}

export async function buildThemeRequest(spec: ThemeSpec, images: ThemeImages, onStep: (text: string) => void): Promise<ThemePackageRequest> {
  onStep("Drawing the wallpaper")
  const home = makeCanvas(1920, 1080)
  drawWallpaper(home.getContext("2d")!, 1920, 1080, spec, images.wallpaper)
  // Settings and the top menu sit on a softer, darker copy of the home screen.
  const fn = makeCanvas(1920, 1080)
  drawWallpaper(fn.getContext("2d")!, 1920, 1080, spec, images.wallpaper, { blur: 28, dim: 0.16 })
  await tick()
  onStep("Drawing the icons")
  const contentIcons: Record<string, string> = {}
  for (const slot of CONTENT_SLOTS) {
    contentIcons[slot.id] = pngData(renderContent(slot.id as ContentSlot, spec, images[slot.id] ?? null, 512))
    await tick()
  }
  const functionIcons: ThemePackageRequest["functionIcons"] = {}
  for (const slot of FUNCTION_SLOTS) {
    const icon = renderFunction(slot.id as FunctionSlot, spec, images[slot.id] ?? null, 128)
    functionIcons[slot.id] = { icon: pngData(icon), glow: pngData(glowOf(icon, spec.focus)) }
  }
  // Settings > Themes shows this preview: the wallpaper with a row of the new tiles.
  const preview = makeCanvas(740, 416)
  const pctx = preview.getContext("2d")!
  pctx.drawImage(home, 0, 0, 740, 416)
  for (const [i, slot] of (["tvvideo", "library", "browser", "gallery", "livefromps"] as ContentSlot[]).entries()) {
    pctx.drawImage(renderContent(slot, spec, images[slot] ?? null, 160), 40 + i * 92, 92, 84, 84)
  }
  const crop = makeCanvas(512)
  crop.getContext("2d")!.drawImage(home, 420, 0, 1080, 1080, 0, 0, 512, 512)
  return {
    title: spec.name.trim() || "SSPI theme",
    label: themeLabel(spec.name),
    home: pngData(home), function: pngData(fn), preview: pngData(preview), icon0: pngData(renderGameIcon(spec, crop)),
    contentIcons, functionIcons,
    colors: { themeColor: 0, font: "#FFFFFFFF", fontShadow: "#FF000000", focus: argb(spec.focus), homeDimmer: "#00FFFFFF", functionDimmer: "#00FFFFFF", titleDimmer: "#00FFFFFF" },
    animation: null,
  }
}
