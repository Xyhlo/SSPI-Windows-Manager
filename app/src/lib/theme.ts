/* =====================================================================
   Console themes. Tile styles (shape, border, debug label) are drawn into
   each game's home-screen icon through the receiver. On a PS4 the rest
   (wallpaper, animated particles, focus colour, text colour and the
   system app icons) is built into a real system-theme package that the
   console installs under Settings > Themes. The PS5 has no theme
   packages, so there those settings stay in the preview.
   ===================================================================== */

// Kept free of runtime imports so the model can be tested under Node.
function makeSquare(size: number) {
  const canvas = document.createElement("canvas")
  canvas.width = canvas.height = size
  return canvas
}

export type TileShape = "square" | "rounded" | "squircle" | "circle"
export type LabelField = "titleId" | "version" | "firmware"
export type ParticleKind = "none" | "stars" | "snow" | "bubbles"
export type PreviewScreen = "home" | "settings"
/** Animated background quality: the console caps the scene at 6 MB and 48 frames. */
export type ThemeMotion = "still" | "sharp" | "smooth"
export type SystemIconStyle = "stock" | "glass" | "solid"
/** Slots a PS4 theme can replace under texture/content_icon (the disc overlay is left alone). */
export const SYSTEM_ICONS = [
  ["browser", "Internet Browser"], ["library", "Library"], ["tvvideo", "TV & Video"], ["gallery", "Capture Gallery"], ["disc", "Disc"],
  ["folder", "Folders"], ["livefromps", "Live from PlayStation"], ["shareplay", "Share Play"], ["usbmusic", "USB Music Player"],
] as const
export type SystemIcon = typeof SYSTEM_ICONS[number][0]

export type ThemeSpec = {
  version: 1
  id: string
  name: string
  tile: {
    shape: TileShape
    /** Corner radius for "rounded", as a share of the tile (0–0.5). */
    radius: number
    /** Space around the art, as a share of the tile (0–0.2). */
    inset: number
    /** Border width in icon pixels at 512 px (0–24). */
    border: number
    borderColor: string
    /** What fills the tile outside the shape. */
    fill: string
    /** Leave the area outside the shape transparent instead of filling it. */
    clear: boolean
  }
  label: { enabled: boolean; fields: LabelField[]; position: "top" | "bottom"; style: "bar" | "pill" }
  home: {
    wallpaper: string | null
    blur: number
    dim: number
    accent: string
    particles: { kind: ParticleKind; density: number; speed: number; color: string; screens: PreviewScreen[] }
  }
  ps4: {
    /** 16-character label of the last installed package; a new install replaces it. */
    label: string
    motion: ThemeMotion
    text: string
    icons: SystemIconStyle
    /** Custom art per system icon, as data URLs. */
    customIcons: Partial<Record<SystemIcon, string>>
  }
  updatedAt: number
}

export type TitleFacts = { titleId: string; version?: string; firmware?: string }

const uid = () => `t${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`

export const DEFAULT_THEME: ThemeSpec = {
  version: 1,
  id: "default",
  name: "Untitled theme",
  tile: { shape: "square", radius: 0.14, inset: 0, border: 0, borderColor: "#ffffff", fill: "#0b0b0c", clear: false },
  label: { enabled: false, fields: ["titleId", "version"], position: "bottom", style: "bar" },
  home: { wallpaper: null, blur: 0, dim: 0.25, accent: "#E4E4E1", particles: { kind: "none", density: 0.45, speed: 0.4, color: "#ffffff", screens: ["home", "settings"] } },
  ps4: { label: "", motion: "sharp", text: "#ffffff", icons: "stock", customIcons: {} },
  updatedAt: 0,
}

/** Starting points offered in the studio. */
export const PRESETS: ThemeSpec[] = [
  { ...DEFAULT_THEME, id: "preset-rounded", name: "Rounded tiles", tile: { ...DEFAULT_THEME.tile, shape: "rounded", radius: 0.2, inset: 0.04 } },
  { ...DEFAULT_THEME, id: "preset-circle", name: "Round tiles", tile: { ...DEFAULT_THEME.tile, shape: "circle", inset: 0.03, border: 6, borderColor: "#e4e4e1", clear: true } },
  { ...DEFAULT_THEME, id: "preset-bubbles", name: "Bubbles", tile: { ...DEFAULT_THEME.tile, shape: "rounded", radius: 0.22, inset: 0.03, clear: true }, home: { ...DEFAULT_THEME.home, dim: 0.1, accent: "#7EB6FF", particles: { kind: "bubbles", density: 0.5, speed: 0.45, color: "#d6ecff", screens: ["home", "settings"] } }, ps4: { ...DEFAULT_THEME.ps4, icons: "glass" } },
  { ...DEFAULT_THEME, id: "preset-debug", name: "Debug labels", label: { enabled: true, fields: ["titleId", "version", "firmware"], position: "bottom", style: "bar" } },
  { ...DEFAULT_THEME, id: "preset-night", name: "Starry settings", tile: { ...DEFAULT_THEME.tile, shape: "squircle", inset: 0.03 }, home: { ...DEFAULT_THEME.home, dim: 0.35, particles: { kind: "stars", density: 0.6, speed: 0.35, color: "#ffffff", screens: ["settings", "home"] } } },
]

export const newTheme = (from: ThemeSpec = DEFAULT_THEME, name?: string): ThemeSpec => ({ ...structuredClone(from), id: uid(), name: name || (from.id.startsWith("preset-") ? from.name : `${from.name} copy`), ps4: { ...structuredClone(from.ps4), label: "" }, updatedAt: Date.now() })

/** True when applying the theme changes icons at all. */
export const changesIcons = (theme: ThemeSpec) =>
  theme.tile.shape !== "square" || theme.tile.inset > 0.001 || theme.tile.border > 0 || (theme.label.enabled && theme.label.fields.length > 0)

/* ---------------------------------------------------------------- shapes */

function shapePath(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, theme: ThemeSpec) {
  const { shape, radius } = theme.tile
  ctx.beginPath()
  if (shape === "circle") { ctx.arc(x + w / 2, y + w / 2, w / 2, 0, Math.PI * 2); return }
  if (shape === "squircle") {
    // Superellipse (n = 4) sampled as a polygon; smooth at icon sizes.
    const cx = x + w / 2, cy = y + w / 2, a = w / 2, n = 4, steps = 96
    for (let i = 0; i <= steps; i++) {
      const t = (i / steps) * Math.PI * 2
      const cos = Math.cos(t), sin = Math.sin(t)
      const px = cx + a * Math.sign(cos) * Math.abs(cos) ** (2 / n)
      const py = cy + a * Math.sign(sin) * Math.abs(sin) ** (2 / n)
      if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py)
    }
    ctx.closePath()
    return
  }
  const r = shape === "rounded" ? Math.min(0.5, Math.max(0, radius)) * w : 0
  ctx.roundRect(x, y, w, w, r)
}

function labelText(theme: ThemeSpec, facts: TitleFacts) {
  const parts: string[] = []
  for (const field of theme.label.fields) {
    if (field === "titleId" && facts.titleId) parts.push(facts.titleId)
    if (field === "version" && facts.version) parts.push(`v${facts.version.replace(/^v/i, "")}`)
    if (field === "firmware" && facts.firmware) parts.push(`FW ${facts.firmware}`)
  }
  return parts.join("  ")
}

/**
 * Draws one themed icon: the art clipped to the tile shape on the theme's fill (or on nothing when
 * `tile.clear` is set, so the corners stay transparent), a border, and the optional debug label.
 */
export function renderThemedIcon(art: CanvasImageSource & { width: number; height: number }, theme: ThemeSpec, facts: TitleFacts, size = 512) {
  const canvas = makeSquare(size)
  const ctx = canvas.getContext("2d")!
  const k = size / 512
  if (!theme.tile.clear || theme.tile.shape === "square") {
    ctx.fillStyle = theme.tile.fill
    ctx.fillRect(0, 0, size, size)
  }
  const inset = Math.min(0.2, Math.max(0, theme.tile.inset)) * size
  const w = size - inset * 2
  ctx.save()
  shapePath(ctx, inset, inset, w, theme)
  ctx.clip()
  const iw = (art as HTMLImageElement).naturalWidth || art.width, ih = (art as HTMLImageElement).naturalHeight || art.height
  const scale = Math.max(w / iw, w / ih)
  ctx.imageSmoothingQuality = "high"
  ctx.drawImage(art, inset + (w - iw * scale) / 2, inset + (w - ih * scale) / 2, iw * scale, ih * scale)
  const text = theme.label.enabled ? labelText(theme, facts) : ""
  if (text) {
    const fontSize = Math.round(30 * k)
    ctx.font = `600 ${fontSize}px "Cascadia Mono", Consolas, "Geist Variable", monospace`
    ctx.textBaseline = "middle"
    const pad = 16 * k
    if (theme.label.style === "bar") {
      const barH = fontSize + pad * 1.4
      const y = theme.label.position === "top" ? inset : inset + w - barH
      ctx.fillStyle = "rgba(0,0,0,.72)"
      ctx.fillRect(inset, y, w, barH)
      ctx.fillStyle = "#ffffff"
      ctx.textAlign = "center"
      ctx.fillText(text, inset + w / 2, y + barH / 2, w - pad * 2)
    } else {
      const measure = Math.min(w - pad * 4, ctx.measureText(text).width)
      const pillW = measure + pad * 2, pillH = fontSize + pad
      const x = inset + (w - pillW) / 2
      const y = theme.label.position === "top" ? inset + pad * 1.6 : inset + w - pillH - pad * 1.6
      ctx.fillStyle = "rgba(0,0,0,.72)"
      ctx.beginPath()
      ctx.roundRect(x, y, pillW, pillH, pillH / 2)
      ctx.fill()
      ctx.fillStyle = "#ffffff"
      ctx.textAlign = "center"
      ctx.fillText(text, x + pillW / 2, y + pillH / 2, measure)
    }
  }
  ctx.restore()
  if (theme.tile.border > 0) {
    const bw = Math.min(24, theme.tile.border) * k
    ctx.save()
    shapePath(ctx, inset + bw / 2, inset + bw / 2, w - bw, theme)
    ctx.lineWidth = bw
    ctx.strokeStyle = theme.tile.borderColor
    ctx.stroke()
    ctx.restore()
  }
  return canvas
}

/* ---------------------------------------------------------------- storage */

const KEY = "sspi.themes.v1"
const MAX_WALLPAPER = 6 * 1024 * 1024
const MAX_ICON = 1536 * 1024

const hex = (value: unknown, fallback: string) => typeof value === "string" && /^#[0-9a-f]{6}$/i.test(value) ? value : fallback
const num = (value: unknown, min: number, max: number, fallback: number) => typeof value === "number" && Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : fallback
const pick = <T extends string>(value: unknown, allowed: readonly T[], fallback: T): T => allowed.includes(value as T) ? value as T : fallback

/** Validates anything that claims to be a theme (saved or imported) into a complete ThemeSpec. */
export function parseTheme(input: unknown): ThemeSpec | null {
  if (!input || typeof input !== "object") return null
  const raw = input as Partial<ThemeSpec> & Record<string, unknown>
  const tile = (raw.tile || {}) as Partial<ThemeSpec["tile"]>
  const label = (raw.label || {}) as Partial<ThemeSpec["label"]>
  const home = (raw.home || {}) as Partial<ThemeSpec["home"]>
  const particles = (home.particles || {}) as Partial<ThemeSpec["home"]["particles"]>
  const ps4 = (raw.ps4 || {}) as Partial<ThemeSpec["ps4"]>
  const d = DEFAULT_THEME
  const customIcons: Partial<Record<SystemIcon, string>> = {}
  const rawIcons = ps4.customIcons && typeof ps4.customIcons === "object" ? ps4.customIcons as Record<string, unknown> : {}
  for (const [slot] of SYSTEM_ICONS) {
    const value = rawIcons[slot]
    if (typeof value === "string" && /^data:image\/(png|jpeg|webp);base64,/.test(value) && value.length < MAX_ICON) customIcons[slot] = value
  }
  const wallpaper = typeof home.wallpaper === "string" && /^data:image\/(png|jpeg|webp);base64,/.test(home.wallpaper) && home.wallpaper.length < MAX_WALLPAPER ? home.wallpaper : null
  return {
    version: 1,
    id: typeof raw.id === "string" && raw.id.length < 64 ? raw.id : uid(),
    name: typeof raw.name === "string" && raw.name.trim() ? raw.name.trim().slice(0, 60) : d.name,
    tile: {
      shape: pick(tile.shape, ["square", "rounded", "squircle", "circle"] as const, d.tile.shape),
      radius: num(tile.radius, 0, 0.5, d.tile.radius),
      inset: num(tile.inset, 0, 0.2, d.tile.inset),
      border: num(tile.border, 0, 24, d.tile.border),
      borderColor: hex(tile.borderColor, d.tile.borderColor),
      fill: hex(tile.fill, d.tile.fill),
      clear: typeof tile.clear === "boolean" ? tile.clear : d.tile.clear,
    },
    label: {
      enabled: typeof label.enabled === "boolean" ? label.enabled : d.label.enabled,
      fields: Array.isArray(label.fields) ? [...new Set(label.fields.filter((f): f is LabelField => ["titleId", "version", "firmware"].includes(f as string)))] : d.label.fields,
      position: pick(label.position, ["top", "bottom"] as const, d.label.position),
      style: pick(label.style, ["bar", "pill"] as const, d.label.style),
    },
    home: {
      wallpaper,
      blur: num(home.blur, 0, 24, d.home.blur),
      dim: num(home.dim, 0, 0.9, d.home.dim),
      accent: hex(home.accent, d.home.accent),
      particles: {
        kind: pick(particles.kind, ["none", "stars", "snow", "bubbles"] as const, d.home.particles.kind),
        density: num(particles.density, 0, 1, d.home.particles.density),
        speed: num(particles.speed, 0, 1, d.home.particles.speed),
        color: hex(particles.color, d.home.particles.color),
        screens: Array.isArray(particles.screens) ? [...new Set(particles.screens.filter((s): s is PreviewScreen => s === "home" || s === "settings"))] : d.home.particles.screens,
      },
    },
    ps4: {
      label: typeof ps4.label === "string" && /^[A-Z0-9]{16}$/.test(ps4.label) ? ps4.label : "",
      motion: pick(ps4.motion, ["still", "sharp", "smooth"] as const, d.ps4.motion),
      text: hex(ps4.text, d.ps4.text),
      icons: pick(ps4.icons, ["stock", "glass", "solid"] as const, d.ps4.icons),
      customIcons,
    },
    updatedAt: typeof raw.updatedAt === "number" ? raw.updatedAt : Date.now(),
  }
}

export function loadThemes(): ThemeSpec[] {
  try {
    const value = JSON.parse(window.localStorage.getItem(KEY) || "[]")
    return Array.isArray(value) ? value.map(parseTheme).filter((theme): theme is ThemeSpec => !!theme) : []
  } catch { return [] }
}

/** Saves the theme list; returns false when local storage refused it (usually a large wallpaper). */
export function saveThemes(themes: ThemeSpec[]) {
  try { window.localStorage.setItem(KEY, JSON.stringify(themes)); return true } catch { return false }
}

export const serializeTheme = (theme: ThemeSpec) => JSON.stringify({ format: "sspi-theme", ...theme }, null, 2)
