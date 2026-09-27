/* =====================================================================
   Case artwork for the stage: the PS4/PS5 case shell composited over a
   title's cover, a blurred wash for download cards, the PS4 backdrop
   flower mask, and CPU previews of the backdrop patterns.
   ===================================================================== */
import ps4Shell from "@/assets/PS4.png"
import ps5Shell from "@/assets/PS5.png"
import { cropBoxedCover, fallbackArt, isRenderedCase, resolveCover } from "@/lib/covers"
import { hexToRgb } from "@/lib/format"
import { PATTERN_STRENGTH, patternIndex } from "@/lib/appearance"

/** Case front height / width, matching the 3D geometry and the DOM anchors (1080 × 1379). */
export const CASE_ASPECT = 1379 / 1080

export type Platform = "ps4" | "ps5"
export const platformFor = (titleId?: string): Platform => (/^CUSA/i.test(titleId || "") ? "ps4" : "ps5")

export function makeCanvas(width: number, height: number) {
  const canvas = document.createElement("canvas")
  canvas.width = width
  canvas.height = height
  return canvas
}

const loadImage = (url: string) => new Promise<HTMLImageElement>((resolve, reject) => {
  const image = new Image()
  image.decoding = "async"
  image.onload = () => resolve(image)
  image.onerror = () => reject(new Error("Image failed to load"))
  image.src = url
})

/* ---------------------------------------------------------------- shells */
type Shell = { img: HTMLImageElement; w: number; h: number; bbox: [number, number, number, number] }
const shells: Partial<Record<Platform, Promise<Shell>>> = {}

/** Loads a shell once and measures its opaque bounds, so the case front maps exactly onto the geometry. */
function shell(platform: Platform): Promise<Shell> {
  shells[platform] ??= loadImage(platform === "ps4" ? ps4Shell : ps5Shell).then(img => {
    const w = img.naturalWidth, h = img.naturalHeight
    const probe = makeCanvas(w, h), ctx = probe.getContext("2d", { willReadFrequently: true })
    let bbox: Shell["bbox"] = [0, 0, 1, 1]
    if (ctx) {
      ctx.drawImage(img, 0, 0)
      const data = ctx.getImageData(0, 0, w, h).data
      let x0 = w, y0 = h, x1 = 0, y1 = 0
      for (let y = 0; y < h; y += 2) {
        for (let x = 0; x < w; x += 2) {
          if (data[(y * w + x) * 4 + 3] > 8) {
            if (x < x0) x0 = x
            if (x > x1) x1 = x
            if (y < y0) y0 = y
            if (y > y1) y1 = y
          }
        }
      }
      if (x1 > x0 && y1 > y0) bbox = [x0 / w, y0 / h, Math.min(1, (x1 + 2) / w), Math.min(1, (y1 + 2) / h)]
    }
    return { img, w, h, bbox }
  })
  return shells[platform]!
}

/** Where the cover sits inside the cropped shell (fractions of the case front). */
function windowRect(platform: Platform, bbox: Shell["bbox"]) {
  const [x0, y0, x1, y1] = bbox
  const inset = platform === "ps4" ? { t: 0.145, r: 0.043, b: 0.037, l: 0.0215 } : { t: 0.1485, r: 0.043, b: 0.037, l: 0.0215 }
  const bw = x1 - x0, bh = y1 - y0
  return { x: (inset.l - x0) / bw, y: (inset.t - y0) / bh, w: (1 - inset.r - inset.l) / bw, h: (1 - inset.b - inset.t) / bh }
}

/* ---------------------------------------------------------------- cover art */
export type CoverSpec = { cover?: string; titleId?: string; title: string }

type Art = { image: HTMLImageElement; rendered: boolean }
const artCache = new Map<string, Promise<Art>>()

/** Resolves the cover to an image: a composited case, a cropped plain cover, or the lettered fallback. */
function coverImage(spec: CoverSpec): Promise<Art> {
  const key = `${spec.cover || ""}|${spec.title}`
  let request = artCache.get(key)
  if (!request) {
    request = (async () => {
      try {
        const resolved = await resolveCover(spec.cover)
        if (isRenderedCase(resolved)) return { image: await loadImage(resolved), rendered: true }
        return { image: await loadImage(await cropBoxedCover(resolved)), rendered: false }
      } catch {
        return { image: await loadImage(fallbackArt(spec.title)), rendered: false }
      }
    })()
    artCache.set(key, request)
  }
  return request
}

function drawCover(ctx: CanvasRenderingContext2D, image: HTMLImageElement, x: number, y: number, w: number, h: number) {
  const iw = image.naturalWidth || image.width, ih = image.naturalHeight || image.height
  if (!iw || !ih) return
  const scale = Math.max(w / iw, h / ih)
  const sw = w / scale, sh = h / scale
  ctx.drawImage(image, (iw - sw) / 2, (ih - sh) / 2, sw, sh, x, y, w, h)
}

const frontCache = new Map<string, Promise<HTMLCanvasElement>>()

/** The case front texture: shell over cover, cropped to the shell's bounds. */
export function caseFront(spec: CoverSpec, width = 512): Promise<HTMLCanvasElement> {
  const key = `${spec.cover || ""}|${spec.title}|${spec.titleId || ""}|${width}`
  let request = frontCache.get(key)
  if (!request) {
    request = (async () => {
      const platform = platformFor(spec.titleId)
      const [art, sh] = await Promise.all([coverImage(spec), shell(platform)])
      const height = Math.round(width * CASE_ASPECT)
      const canvas = makeCanvas(width, height), ctx = canvas.getContext("2d")!
      ctx.imageSmoothingQuality = "high"
      const [x0, y0, x1, y1] = sh.bbox
      if (art.rendered) {
        // Already a finished case (saved with older transfers): crop it like the shell.
        const iw = art.image.naturalWidth, ih = art.image.naturalHeight
        ctx.drawImage(art.image, x0 * iw, y0 * ih, (x1 - x0) * iw, (y1 - y0) * ih, 0, 0, width, height)
        return canvas
      }
      ctx.fillStyle = "#101012"
      ctx.fillRect(0, 0, width, height)
      const r = windowRect(platform, sh.bbox)
      drawCover(ctx, art.image, r.x * width, r.y * height, r.w * width, r.h * height)
      ctx.drawImage(sh.img, x0 * sh.w, y0 * sh.h, (x1 - x0) * sh.w, (y1 - y0) * sh.h, 0, 0, width, height)
      return canvas
    })()
    frontCache.set(key, request)
    request.catch(() => frontCache.delete(key))
  }
  return request
}

const placeholders = new Map<Platform, { canvas: HTMLCanvasElement; ready: Promise<void> }>()
/** A placeholder front shown while the cover loads: the shell over a dark window. `ready` resolves once the shell is drawn. */
export function placeholderFront(titleId: string | undefined) {
  const platform = platformFor(titleId)
  let entry = placeholders.get(platform)
  if (!entry) {
    const width = 256, height = Math.round(width * CASE_ASPECT)
    const canvas = makeCanvas(width, height), ctx = canvas.getContext("2d")!
    ctx.fillStyle = "#141416"
    ctx.fillRect(0, 0, width, height)
    const ready = shell(platform).then(sh => {
      const [x0, y0, x1, y1] = sh.bbox
      ctx.drawImage(sh.img, x0 * sh.w, y0 * sh.h, (x1 - x0) * sh.w, (y1 - y0) * sh.h, 0, 0, width, height)
    }).catch(() => undefined)
    entry = { canvas, ready }
    placeholders.set(platform, entry)
  }
  return entry
}

/** Starts loading both shells so the first cases appear with their frames. */
export function preloadShells() {
  placeholderFront("CUSA00000")
  placeholderFront("PPSA00000")
}

const thumbCache = new Map<string, Promise<string>>()
/** A small case image for toasts and result rows. */
export function caseThumb(spec: CoverSpec): Promise<string> {
  const key = `${spec.cover || ""}|${spec.title}|${spec.titleId || ""}`
  let request = thumbCache.get(key)
  if (!request) {
    request = caseFront(spec, 192).then(canvas => canvas.toDataURL("image/webp", 0.9))
    thumbCache.set(key, request)
    request.catch(() => thumbCache.delete(key))
  }
  return request
}

const blurCache = new Map<string, Promise<HTMLCanvasElement>>()
/** A soft, wide crop of the cover for the open download card's surface. */
export function blurredArt(spec: CoverSpec): Promise<HTMLCanvasElement> {
  const key = `${spec.cover || ""}|${spec.title}`
  let request = blurCache.get(key)
  if (!request) {
    request = coverImage(spec).then(({ image }) => {
      const canvas = makeCanvas(640, 360), ctx = canvas.getContext("2d")!
      ctx.filter = "blur(18px) saturate(1.2)"
      const iw = image.naturalWidth, ih = image.naturalHeight
      const sh = iw * 360 / 640
      ctx.drawImage(image, 0, Math.max(0, (ih - sh) * 0.3), iw, Math.min(ih, sh), -40, -40, 720, 440)
      return canvas
    })
    blurCache.set(key, request)
  }
  return request
}

const tintCache = new Map<string, Promise<string>>()
/** The cover's average colour, used when "Tint with game artwork" is on. */
export function coverTint(spec: CoverSpec): Promise<string> {
  const key = `${spec.cover || ""}|${spec.title}`
  let request = tintCache.get(key)
  if (!request) {
    request = coverImage(spec).then(({ image }) => {
      const canvas = makeCanvas(24, 24), ctx = canvas.getContext("2d", { willReadFrequently: true })!
      ctx.drawImage(image, 0, 0, 24, 24)
      const data = ctx.getImageData(0, 0, 24, 24).data
      let r = 0, g = 0, b = 0, n = 0
      for (let i = 0; i < data.length; i += 4) {
        const w = Math.max(data[i], data[i + 1], data[i + 2]) - Math.min(data[i], data[i + 1], data[i + 2]) + 8
        r += data[i] * w; g += data[i + 1] * w; b += data[i + 2] * w; n += w
      }
      const hex = (v: number) => Math.round(Math.min(255, v / n * 1.25)).toString(16).padStart(2, "0")
      return `#${hex(r)}${hex(g)}${hex(b)}`
    })
    tintCache.set(key, request)
  }
  return request
}

/* ---------------------------------------------------------------- PS4 BackdropPattern.cs: flower mask and previews */
const FLOWER = ["    .-.    ", " .-(   )-. ", "(   .@.   )", " '-(   )-' ", "    '-'    ", "     |     ", "  \\  |  /  ", "   \\ | /   ", "    \\|/    ", "     |     "]
const GLYPHS: Record<string, string> = {
  ".": "     " + "     " + "     " + "     " + "     " + " ##  " + " ##  ",
  "-": "     " + "     " + "     " + " ### " + "     " + "     " + "     ",
  "(": "   # " + "  #  " + " #   " + " #   " + " #   " + "  #  " + "   # ",
  ")": " #   " + "  #  " + "   # " + "   # " + "   # " + "  #  " + " #   ",
  "@": " ### " + "#   #" + "# ###" + "# # #" + "# ###" + "#    " + " ### ",
  "|": "  #  " + "  #  " + "  #  " + "  #  " + "  #  " + "  #  " + "  #  ",
  "/": "    #" + "   # " + "   # " + "  #  " + " #   " + " #   " + "#    ",
  "\\": "#    " + " #   " + " #   " + "  #  " + "   # " + "   # " + "    #",
  "'": "  #  " + "  #  " + " #   " + "     " + "     " + "     " + "     ",
}
let flowerMaskCache: Float32Array | null = null
export function flowerMask() {
  if (flowerMaskCache) return flowerMaskCache
  const W = 640, H = 360, mask = new Float32Array(W * H)
  const blooms = [[12, 32], [16, 210], [150, 272], [366, 261], [482, 224], [565, 104], [520, 12]]
  for (const [bx, by] of blooms) {
    FLOWER.forEach((row, ri) => [...row].forEach((ch, ci) => {
      const glyph = GLYPHS[ch]
      if (!glyph) return
      for (let gy = 0; gy < 7; gy++) for (let gx = 0; gx < 5; gx++) {
        if (glyph[gy * 5 + gx] !== "#") continue
        const x = bx + ci * 6 + gx, y = by + ri * 9 + gy
        if (x < W && y < H) mask[y * W + x] = x < W / 2 ? 0.105 : 0.19
      }
    }))
  }
  flowerMaskCache = mask
  return mask
}

/** The PS4 pattern formula at 320 × 180, softened like the PS4 worker blur. */
export function patternPreview(id: string, accentHex: string, canvas: HTMLCanvasElement) {
  const W = 320, H = 180, [red, green, blue] = hexToRgb(accentHex), pattern = patternIndex(id)
  const tmp = makeCanvas(W, H), tctx = tmp.getContext("2d")!, img = tctx.createImageData(W, H), d = img.data
  const mask = pattern === 8 ? flowerMask() : null
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
    const u = x / W, v = y / H, p = (y * W + x) * 4
    let r = 11, g = 11, b = 12
    if (pattern >= 5 && pattern <= 7) {
      const light = pattern === 5 ? 8 + 17 * (1 - u) * (1 - v) : pattern === 6 ? 5 + 10 * Math.exp(-((u - 0.75) ** 2 + (v - 0.9) ** 2) * 4) : 9 + 18 * Math.exp(-((u - 0.2) ** 2 + (v - 0.15) ** 2) * 3)
      r = g = b = light
    } else if (pattern === 8 && mask) {
      const dx = u - 0.82, dy = v - 0.78, glow = 0.075 * Math.exp(-(dx * dx + dy * dy) * 5)
      const k = (glow + mask[Math.floor(v * 360) * 640 + Math.floor(u * 640)]) * PATTERN_STRENGTH
      r = 9 + red * k; g = 9 + green * k; b = 11 + blue * k
    } else if (pattern > 0) {
      const X = u * 640, Y = v * 360, dx = u - 0.82, dy = (v - 0.85) * 0.7, radius = Math.sqrt(dx * dx + dy * dy)
      const glow = Math.exp(-radius * radius * 5.5)
      let wave = 0.5 + 0.5 * Math.cos(radius * 38)
      if (pattern === 2) wave = 0.5 + 0.5 * Math.sin(u * 42 + Math.sin(v * 5) * 3)
      if (pattern === 3) wave = 0.25 + 0.75 * (0.5 + 0.5 * Math.sin(Math.floor(X / 7) * 0.61 + Math.floor(Y / 7) * 0.32))
      if (pattern === 4) wave = Math.exp(-(((radius - 0.26) * 9) ** 2))
      const mesh = (Math.floor(X) % 7 === 0 || Math.floor(Y) % 7 === 0) ? 0.78 : 1
      const intensity = (0.025 + 0.22 * glow) * (0.3 + 0.7 * wave) * mesh * PATTERN_STRENGTH
      r = 11 + red * intensity; g = 11 + green * intensity; b = 12 + blue * intensity
    }
    d[p] = r; d[p + 1] = g; d[p + 2] = b; d[p + 3] = 255
  }
  tctx.putImageData(img, 0, 0)
  canvas.width = W
  canvas.height = H
  const ctx = canvas.getContext("2d")!
  ctx.filter = pattern >= 1 && pattern <= 4 ? "blur(1.4px)" : "none"
  ctx.drawImage(tmp, 0, 0)
}
