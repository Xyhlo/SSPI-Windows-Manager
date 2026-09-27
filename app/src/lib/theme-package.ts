/* =====================================================================
   PS4 theme package art. Everything a PS4 theme shows is drawn here, by
   the same functions the studio's preview uses, so what you see in the
   studio is what the console gets: the wallpaper (cover, blur, dim),
   particles that loop seamlessly, the Settings background and the system
   app icons. The app packs these images into a system-theme PKG.
   ===================================================================== */
import type { ThemePackageRequest } from "./console-types"
import { loadImage } from "./image"
import { SYSTEM_ICONS, type ParticleKind, type SystemIcon, type ThemeMotion, type ThemeSpec } from "./theme"

/** Animated background presets. The console accepts at most 6 MB of frames and 48 frames. */
export const MOTION = {
  sharp: { width: 960, height: 540, frames: 24, wait: 0.1 },
  smooth: { width: 640, height: 360, frames: 48, wait: 0.075 },
} as const
/** One loop of the particle animation, in milliseconds (still themes preview the sharp loop). */
export const loopMs = (motion: ThemeMotion) => {
  const preset = MOTION[motion === "smooth" ? "smooth" : "sharp"]
  return preset.frames * preset.wait * 1000
}
export const animates = (theme: ThemeSpec) =>
  theme.home.particles.kind !== "none" && theme.home.particles.screens.includes("home") && theme.ps4.motion !== "still"

function makeCanvas(w: number, h: number) {
  const canvas = document.createElement("canvas")
  canvas.width = w
  canvas.height = h
  return canvas
}
const pngData = (canvas: HTMLCanvasElement) => canvas.toDataURL("image/png")
const argb = (hex: string, alpha = "FF") => `#${alpha}${hex.replace("#", "").toUpperCase()}`
const tick = () => new Promise<void>(resolve => window.setTimeout(resolve, 0))

/* ---------------------------------------------------------------- particles */
export type Particle = { x: number; y: number; r: number; phase: number; drift: number; k: number }
function random(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
/** The same particles every time for a kind and amount, so exports match the preview. */
export function particleField(kind: ParticleKind, density: number): Particle[] {
  const next = random(kind.length * 7919 + 17)
  const count = Math.round((kind === "bubbles" ? 42 : kind === "snow" ? 120 : kind === "stars" ? 190 : 0) * density)
  return Array.from({ length: count }, () => ({ x: next(), y: next(), r: next(), phase: next(), drift: next(), k: 1 + Math.floor(next() * 2) }))
}

/**
 * Draws the particle layer at loop phase `t` (0 to 1). Every movement is periodic in `t`, so the
 * last frame flows into the first and the console's animation loops without a jump. Sizes are in
 * 1080p units and scale with the canvas.
 */
export function drawParticles(ctx: CanvasRenderingContext2D, w: number, h: number, spec: ThemeSpec["home"]["particles"], field: Particle[], t: number) {
  const s = h / 1080
  const speed = 0.25 + spec.speed * 1.5
  const tau = Math.PI * 2
  ctx.save()
  ctx.fillStyle = spec.color
  ctx.strokeStyle = spec.color
  for (const p of field) {
    if (spec.kind === "stars") {
      const size = (1 + p.r * 2.6) * s
      const twinkle = 0.35 + 0.65 * (0.5 + 0.5 * Math.sin((t * p.k * Math.round(1 + spec.speed * 2) + p.phase) * tau))
      ctx.globalAlpha = twinkle * (0.4 + p.r * 0.6)
      ctx.beginPath()
      ctx.arc(p.x * w, p.y * h, size, 0, tau)
      ctx.fill()
      if (p.r > 0.93) {
        ctx.globalAlpha *= 0.5
        ctx.fillRect(p.x * w - size * 3, p.y * h - 0.8 * s, size * 6, 1.6 * s)
        ctx.fillRect(p.x * w - 0.8 * s, p.y * h - size * 3, 1.6 * s, size * 6)
      }
      continue
    }
    // Snow and bubbles live for one loop each, fading in and out at staggered phases.
    const life = (t + p.phase) % 1
    const fade = Math.min(1, life * 5, (1 - life) * 5)
    if (spec.kind === "snow") {
      const x = p.x * w + Math.sin((life + p.drift) * tau) * 14 * s
      const y = (p.y * 1.1 - 0.05 + 0.12 * speed * (0.5 + p.r) * life) * h
      ctx.globalAlpha = fade * (0.35 + p.r * 0.5)
      ctx.beginPath()
      ctx.arc(x, y, (1.6 + p.r * 4.4) * s, 0, tau)
      ctx.fill()
    } else if (spec.kind === "bubbles") {
      const radius = (7 + p.r * 22) * s
      const x = p.x * w + Math.sin((life + p.drift) * tau) * 10 * s
      const y = (p.y * 1.1 + 0.02 - 0.1 * speed * (0.5 + p.r) * life) * h
      ctx.beginPath()
      ctx.arc(x, y, radius, 0, tau)
      ctx.globalAlpha = fade * 0.08
      ctx.fill()
      ctx.lineWidth = Math.max(1, 1.8 * s)
      ctx.globalAlpha = fade * (0.45 + p.r * 0.35)
      ctx.stroke()
      ctx.globalAlpha = fade * 0.7
      ctx.beginPath()
      ctx.arc(x - radius * 0.38, y - radius * 0.38, radius * 0.2, 0, tau)
      ctx.fill()
    }
  }
  ctx.restore()
}

/* ---------------------------------------------------------------- backgrounds */
function ellipseGlow(ctx: CanvasRenderingContext2D, w: number, h: number, cx: number, cy: number, rx: number, ry: number, color: string, stop: number) {
  ctx.save()
  ctx.translate(cx * w, cy * h)
  ctx.scale(rx * w, ry * h)
  const glow = ctx.createRadialGradient(0, 0, 0, 0, 0, 1)
  glow.addColorStop(0, color)
  glow.addColorStop(stop, "rgba(0,0,0,0)")
  ctx.fillStyle = glow
  ctx.fillRect(-cx / rx - 1, -cy / ry - 1, 1 / rx + 2, 1 / ry + 2)
  ctx.restore()
}
/** The stock PS4 blue (the same gradient and glows as the studio's preview). */
function drawStockBlue(ctx: CanvasRenderingContext2D, w: number, h: number) {
  const angle = (168 * Math.PI) / 180
  const dx = Math.sin(angle), dy = -Math.cos(angle)
  const half = (Math.abs(w * dx) + Math.abs(h * dy)) / 2
  const line = ctx.createLinearGradient(w / 2 - dx * half, h / 2 - dy * half, w / 2 + dx * half, h / 2 + dy * half)
  line.addColorStop(0, "#0f55d6")
  line.addColorStop(0.46, "#0b47c3")
  line.addColorStop(1, "#0838a9")
  ctx.fillStyle = line
  ctx.fillRect(0, 0, w, h)
  ellipseGlow(ctx, w, h, 0.18, 1.12, 0.9, 0.6, "rgba(80,150,255,.28)", 0.62)
  ellipseGlow(ctx, w, h, 0.95, 1.18, 1.6, 0.48, "rgba(120,175,255,.22)", 0.55)
  ellipseGlow(ctx, w, h, 0.3, 1.25, 1.3, 0.4, "rgba(255,255,255,.08)", 0.6)
}
/** Home background: the wallpaper (cover, as in the preview) or the stock blue, then blur and dim. */
export function drawWallpaper(ctx: CanvasRenderingContext2D, w: number, h: number, theme: ThemeSpec, image: HTMLImageElement | null, extra = { blur: 0, dim: 0 }) {
  ctx.save()
  if (image) {
    const blur = (theme.home.blur * 2.4 + extra.blur) * (w / 1920)
    ctx.filter = blur > 0 ? `blur(${blur}px)` : "none"
    const scale = Math.max(w / image.naturalWidth, h / image.naturalHeight) * 1.04
    const iw = image.naturalWidth * scale, ih = image.naturalHeight * scale
    ctx.drawImage(image, (w - iw) / 2, (h - ih) / 2, iw, ih)
    ctx.filter = "none"
  } else drawStockBlue(ctx, w, h)
  const dim = Math.min(0.95, theme.home.dim * (image ? 1 : 0.6) + extra.dim)
  if (dim > 0) {
    ctx.fillStyle = `rgba(0,0,0,${dim})`
    ctx.fillRect(0, 0, w, h)
  }
  ctx.restore()
}

/* ---------------------------------------------------------------- system icons */
type Shape = ThemeSpec["tile"]["shape"]
function tilePath(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, shape: Shape, radius: number) {
  ctx.beginPath()
  if (shape === "circle") { ctx.arc(x + w / 2, y + w / 2, w / 2, 0, Math.PI * 2); return }
  if (shape === "squircle") {
    const cx = x + w / 2, cy = y + w / 2, a = w / 2
    for (let i = 0; i <= 96; i++) {
      const t = (i / 96) * Math.PI * 2, c = Math.cos(t), s = Math.sin(t)
      const px = cx + a * Math.sign(c) * Math.abs(c) ** 0.5, py = cy + a * Math.sign(s) * Math.abs(s) ** 0.5
      if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py)
    }
    ctx.closePath()
    return
  }
  ctx.roundRect(x, y, w, w, shape === "rounded" ? Math.min(0.5, Math.max(0, radius)) * w : w * 0.06)
}
const luminance = (hex: string) => {
  const n = parseInt(hex.slice(1), 16)
  const channel = (v: number) => { const c = v / 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4 }
  return 0.2126 * channel((n >> 16) & 255) + 0.7152 * channel((n >> 8) & 255) + 0.0722 * channel(n & 255)
}

/** Line-art glyphs for the system apps, drawn around (cx, cy) in 512-unit space scaled by `u`. */
function drawGlyph(ctx: CanvasRenderingContext2D, slot: SystemIcon, cx: number, cy: number, u: number, color: string) {
  ctx.save()
  ctx.translate(cx, cy)
  ctx.scale(u, u)
  ctx.strokeStyle = color
  ctx.fillStyle = color
  ctx.lineWidth = 22
  ctx.lineCap = "round"
  ctx.lineJoin = "round"
  const path = (draw: () => void, fill = false) => { ctx.beginPath(); draw(); if (fill) ctx.fill(); else ctx.stroke() }
  switch (slot) {
    case "browser":
      path(() => ctx.arc(0, 0, 116, 0, Math.PI * 2))
      path(() => ctx.ellipse(0, 0, 50, 116, 0, 0, Math.PI * 2))
      ctx.lineWidth = 18
      path(() => { ctx.moveTo(-116, 0); ctx.lineTo(116, 0); ctx.moveTo(-96, -62); ctx.lineTo(96, -62); ctx.moveTo(-96, 62); ctx.lineTo(96, 62) })
      break
    case "library":
      for (const [x, y] of [[-112, -112], [12, -112], [-112, 12], [12, 12]]) path(() => ctx.roundRect(x, y, 100, 100, 20), true)
      break
    case "tvvideo":
      path(() => ctx.roundRect(-128, -92, 256, 172, 22))
      path(() => { ctx.moveTo(-26, -40); ctx.lineTo(42, -4); ctx.lineTo(-26, 32); ctx.closePath() }, true)
      path(() => { ctx.moveTo(-56, 118); ctx.lineTo(56, 118) })
      break
    case "gallery":
      path(() => ctx.roundRect(-128, -100, 256, 200, 22))
      path(() => { ctx.moveTo(-110, 84); ctx.lineTo(-34, 4); ctx.lineTo(16, 52); ctx.lineTo(56, 16); ctx.lineTo(110, 84); ctx.closePath() }, true)
      path(() => ctx.arc(52, -44, 24, 0, Math.PI * 2), true)
      break
    case "disc":
      path(() => ctx.arc(0, 0, 126, 0, Math.PI * 2))
      path(() => ctx.arc(0, 0, 34, 0, Math.PI * 2), true)
      ctx.lineWidth = 14
      path(() => ctx.arc(0, 0, 82, -2.4, -1.3))
      break
    case "folder":
      path(() => { ctx.moveTo(-132, -84); ctx.lineTo(-40, -84); ctx.lineTo(-12, -54); ctx.lineTo(132, -54); ctx.lineTo(132, 104); ctx.lineTo(-132, 104); ctx.closePath() })
      path(() => { ctx.moveTo(-132, -20); ctx.lineTo(132, -20) })
      break
    case "livefromps":
      path(() => ctx.arc(0, 0, 28, 0, Math.PI * 2), true)
      for (const r of [76, 126]) {
        path(() => ctx.arc(0, 0, r, -0.7, 0.7))
        path(() => ctx.arc(0, 0, r, Math.PI - 0.7, Math.PI + 0.7))
      }
      break
    case "shareplay":
      path(() => ctx.arc(-50, 0, 82, 0, Math.PI * 2))
      path(() => ctx.arc(50, 0, 82, 0, Math.PI * 2))
      break
    case "usbmusic":
      path(() => { ctx.moveTo(-46, 70); ctx.lineTo(-46, -96); ctx.lineTo(98, -124); ctx.lineTo(98, 44) })
      path(() => ctx.ellipse(-80, 72, 40, 30, -0.35, 0, Math.PI * 2), true)
      path(() => ctx.ellipse(64, 46, 40, 30, -0.35, 0, Math.PI * 2), true)
      break
  }
  ctx.restore()
}

/**
 * One system app icon (512 px, transparent outside the shape): the chosen image, or a glyph on
 * frosted glass or on the accent colour. Shape and border follow the game tiles.
 */
export function drawSystemIcon(slot: SystemIcon, theme: ThemeSpec, custom: HTMLImageElement | null, size = 512) {
  const canvas = makeCanvas(size, size)
  const ctx = canvas.getContext("2d")!
  const u = size / 512
  const inset = Math.max(0.03, Math.min(0.2, theme.tile.inset)) * size
  const w = size - inset * 2
  const { shape, radius } = theme.tile
  ctx.save()
  tilePath(ctx, inset, inset, w, shape, radius)
  ctx.clip()
  if (custom) {
    const scale = Math.max(w / custom.naturalWidth, w / custom.naturalHeight)
    ctx.imageSmoothingQuality = "high"
    ctx.drawImage(custom, inset + (w - custom.naturalWidth * scale) / 2, inset + (w - custom.naturalHeight * scale) / 2, custom.naturalWidth * scale, custom.naturalHeight * scale)
  } else if (theme.ps4.icons === "solid") {
    ctx.fillStyle = theme.home.accent
    ctx.fillRect(0, 0, size, size)
    const shade = ctx.createLinearGradient(0, inset, 0, inset + w)
    shade.addColorStop(0, "rgba(255,255,255,.18)")
    shade.addColorStop(1, "rgba(0,0,0,.18)")
    ctx.fillStyle = shade
    ctx.fillRect(0, 0, size, size)
  } else {
    const glass = ctx.createLinearGradient(0, inset, 0, inset + w)
    glass.addColorStop(0, "rgba(255,255,255,.24)")
    glass.addColorStop(1, "rgba(255,255,255,.10)")
    ctx.fillStyle = glass
    ctx.fillRect(0, 0, size, size)
  }
  ctx.restore()
  if (!custom) {
    const ink = theme.ps4.icons === "solid" && luminance(theme.home.accent) > 0.5 ? "#0b0b0c" : "#ffffff"
    // Glyphs are drawn for a 464-unit tile (512 less the default inset).
    drawGlyph(ctx, slot, size / 2, size / 2, (w / 464) * 0.92, ink)
  }
  const border = theme.tile.border > 0 ? Math.min(24, theme.tile.border) * u : theme.ps4.icons === "glass" && !custom ? 5 * u : 0
  if (border) {
    ctx.save()
    tilePath(ctx, inset + border / 2, inset + border / 2, w - border, shape, radius)
    ctx.lineWidth = border
    ctx.strokeStyle = theme.tile.border > 0 ? theme.tile.borderColor : "rgba(255,255,255,.42)"
    ctx.stroke()
    ctx.restore()
  }
  return canvas
}

/** System icons the package replaces: every slot for a restyle, otherwise only slots with custom art. */
export async function systemIconArt(theme: ThemeSpec, size = 512) {
  const out: Partial<Record<SystemIcon, HTMLCanvasElement>> = {}
  for (const [slot] of SYSTEM_ICONS) {
    const url = theme.ps4.customIcons[slot]
    if (theme.ps4.icons === "stock" && !url) continue
    const custom = url ? await loadImage(url).catch(() => null) : null
    if (theme.ps4.icons === "stock" && !custom) continue
    out[slot] = drawSystemIcon(slot, theme, custom, size)
  }
  return out
}

/* ---------------------------------------------------------------- the package request */
export type BuildStep = { text: string; share: number }

/** Draws every image of the theme and returns the request the app packs into a PKG. */
export async function buildThemeRequest(theme: ThemeSpec, onStep: (step: BuildStep) => void): Promise<ThemePackageRequest> {
  onStep({ text: "Drawing the backgrounds", share: 0.02 })
  const image = theme.home.wallpaper ? await loadImage(theme.home.wallpaper) : null
  const particles = theme.home.particles
  const field = particleField(particles.kind, particles.density)
  const onHome = particles.kind !== "none" && particles.screens.includes("home")
  const onSettings = particles.kind !== "none" && particles.screens.includes("settings")

  const home = makeCanvas(1920, 1080)
  const homeCtx = home.getContext("2d")!
  drawWallpaper(homeCtx, 1920, 1080, theme, image)
  if (onHome) drawParticles(homeCtx, 1920, 1080, particles, field, 0)

  // The function screen (Settings and the top menu) is a softer, darker copy of the home.
  const settings = makeCanvas(1920, 1080)
  const settingsCtx = settings.getContext("2d")!
  drawWallpaper(settingsCtx, 1920, 1080, theme, image, { blur: 30, dim: 0.12 })
  if (onSettings) drawParticles(settingsCtx, 1920, 1080, particles, field, 0.37)

  const preview = makeCanvas(740, 416)
  preview.getContext("2d")!.drawImage(home, 0, 0, 740, 416)
  const icon0 = makeCanvas(512, 512)
  icon0.getContext("2d")!.drawImage(home, 420, 0, 1080, 1080, 0, 0, 512, 512)
  await tick()

  onStep({ text: "Drawing the system icons", share: 0.08 })
  const contentIcons: Record<string, string> = {}
  for (const [slot, canvas] of Object.entries(await systemIconArt(theme))) contentIcons[slot] = pngData(canvas!)

  let animation: ThemePackageRequest["animation"] = null
  if (animates(theme)) {
    const preset = MOTION[theme.ps4.motion === "smooth" ? "smooth" : "sharp"]
    const base = makeCanvas(preset.width, preset.height)
    drawWallpaper(base.getContext("2d")!, preset.width, preset.height, theme, image)
    const frame = makeCanvas(preset.width, preset.height)
    const ctx = frame.getContext("2d")!
    const frames: string[] = []
    for (let n = 0; n < preset.frames; n++) {
      ctx.drawImage(base, 0, 0)
      drawParticles(ctx, preset.width, preset.height, particles, field, n / preset.frames)
      frames.push(pngData(frame))
      onStep({ text: `Drawing frame ${n + 1} of ${preset.frames}`, share: 0.1 + 0.6 * ((n + 1) / preset.frames) })
      if (n % 4 === 3) await tick()
    }
    animation = { width: preset.width, height: preset.height, wait: preset.wait, frames }
  }

  return {
    title: theme.name.trim() || "SSPI theme",
    label: "",
    home: pngData(home),
    function: pngData(settings),
    preview: pngData(preview),
    icon0: pngData(icon0),
    contentIcons,
    functionIcons: {},
    colors: {
      themeColor: 0,
      font: argb(theme.ps4.text),
      fontShadow: "#FF000000",
      focus: argb(theme.home.accent),
      homeDimmer: "#00FFFFFF",
      functionDimmer: "#00FFFFFF",
      titleDimmer: "#00FFFFFF",
    },
    animation,
  }
}
