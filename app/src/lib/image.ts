/* Square icon rendering for console home-screen icons (icon0.png): framing, PNG export and thumbnails. */

export const ICON_SIZE = 512

export type Framing = {
  /** Extra scale on top of the fill or fit size, 1 = exactly filled or fitted. */
  zoom: number
  /** Pan within the overflow, -1 (left/top edge) to 1 (right/bottom edge). */
  x: number
  y: number
  fit: "fill" | "fit"
  /** What shows around a fitted image. */
  background: "blur" | "color"
  color: string
}

export const DEFAULT_FRAMING: Framing = { zoom: 1, x: 0, y: 0, fit: "fill", background: "blur", color: "#101012" }

export function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image()
    image.decoding = "async"
    if (/^https?:/i.test(src)) image.crossOrigin = "anonymous"
    image.onload = () => resolve(image)
    image.onerror = () => reject(new Error("The image couldn't be read."))
    image.src = src
  })
}

export function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(String(reader.result || ""))
    reader.onerror = () => reject(new Error("The file couldn't be read."))
    reader.readAsDataURL(file)
  })
}

export function makeSquare(size: number) {
  const canvas = document.createElement("canvas")
  canvas.width = canvas.height = size
  return canvas
}

/** Where the image lands inside a square of `size` for a framing: its drawn size and top-left corner. */
export function placement(width: number, height: number, size: number, framing: Framing) {
  const base = framing.fit === "fill" ? Math.max(size / width, size / height) : Math.min(size / width, size / height)
  const scale = base * Math.max(0.2, framing.zoom)
  const w = width * scale, h = height * scale
  const overflowX = Math.abs(w - size) / 2, overflowY = Math.abs(h - size) / 2
  return { w, h, left: (size - w) / 2 - framing.x * overflowX, top: (size - h) / 2 - framing.y * overflowY }
}

/** Draws a framed image into a square context of `size` pixels. */
export function drawFramed(ctx: CanvasRenderingContext2D, image: CanvasImageSource & { width: number; height: number }, size: number, framing: Framing) {
  const iw = (image as HTMLImageElement).naturalWidth || image.width, ih = (image as HTMLImageElement).naturalHeight || image.height
  ctx.save()
  ctx.clearRect(0, 0, size, size)
  const place = placement(iw, ih, size, framing)
  const covers = place.left <= 0.5 && place.top <= 0.5 && place.left + place.w >= size - 0.5 && place.top + place.h >= size - 0.5
  if (!covers) {
    if (framing.background === "blur") {
      const cover = placement(iw, ih, size, { ...framing, fit: "fill", zoom: 1.08, x: 0, y: 0 })
      ctx.filter = `blur(${Math.round(size / 24)}px) saturate(1.1) brightness(.8)`
      ctx.drawImage(image, cover.left, cover.top, cover.w, cover.h)
      ctx.filter = "none"
    } else {
      ctx.fillStyle = framing.color
      ctx.fillRect(0, 0, size, size)
    }
  }
  ctx.imageSmoothingQuality = "high"
  ctx.drawImage(image, place.left, place.top, place.w, place.h)
  ctx.restore()
}

export function renderFramed(image: HTMLImageElement, framing: Framing, size = ICON_SIZE) {
  const canvas = makeSquare(size)
  drawFramed(canvas.getContext("2d")!, image, size, framing)
  return canvas
}

/** Base64 PNG bytes without the data: prefix, as the receiver commands expect. Flattened onto `matte` when given. */
export function pngBase64(canvas: HTMLCanvasElement, matte?: string) {
  let source = canvas
  if (matte) {
    source = makeSquare(canvas.width)
    const ctx = source.getContext("2d")!
    ctx.fillStyle = matte
    ctx.fillRect(0, 0, source.width, source.height)
    ctx.drawImage(canvas, 0, 0)
  }
  return source.toDataURL("image/png").replace(/^data:image\/png;base64,/, "")
}

const portraits = new Map<string, Promise<string>>()
/**
 * Case art from a square home-screen icon: the icon sits full width in a portrait front, over a
 * blurred, darkened copy of itself, so a case shows the whole icon instead of a cropped middle.
 */
export function portraitFromIcon(src: string): Promise<string> {
  let request = portraits.get(src)
  if (!request) {
    request = loadImage(src).then(image => {
      const w = 540, h = Math.round(540 * 1379 / 1080)
      const canvas = document.createElement("canvas")
      canvas.width = w; canvas.height = h
      const ctx = canvas.getContext("2d")!
      ctx.fillStyle = "#0b0b0c"
      ctx.fillRect(0, 0, w, h)
      ctx.filter = "blur(28px) saturate(1.15) brightness(.55)"
      ctx.drawImage(image, -h * 0.1, -h * 0.05, h * 1.2, h * 1.1)
      ctx.filter = "none"
      const top = Math.round((h - w) * 0.42)
      ctx.imageSmoothingQuality = "high"
      ctx.drawImage(image, 0, top, w, w)
      return canvas.toDataURL("image/jpeg", 0.9)
    })
    portraits.set(src, request)
    request.catch(() => portraits.delete(src))
  }
  return request
}

/** A small JPEG data URL, the size the library keeps for its tiles. */
export function thumbnail(canvas: HTMLCanvasElement, size = 256, matte = "#0b0b0c") {
  const small = makeSquare(size)
  const ctx = small.getContext("2d")!
  ctx.fillStyle = matte
  ctx.fillRect(0, 0, size, size)
  ctx.imageSmoothingQuality = "high"
  ctx.drawImage(canvas, 0, 0, size, size)
  return small.toDataURL("image/jpeg", 0.86)
}
