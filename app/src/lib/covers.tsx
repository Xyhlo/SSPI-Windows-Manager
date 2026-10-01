import { invoke } from "@tauri-apps/api/core"
import { useEffect, useState } from "react"

import { PLAIN_ART_PREFIX } from "@/lib/image"
import { cn } from "@/lib/utils"
import ps4Case from "@/assets/PS4.png"
import ps5Case from "@/assets/PS5.png"

const coverCache = new Map<string, Promise<string>>()
const plainCache = new Map<string, Promise<string>>()
const renderedCasePrefix = "data:image/png;sspi-case=1;base64,"

export function isRenderedCase(source: string) {
  if (source.startsWith(renderedCasePrefix)) return true
  // Older saved transfers contain unmarked 240 x 304 notification cases.
  const prefix = "data:image/png;base64,"
  if (!source.startsWith(prefix)) return false
  try {
    const header = atob(source.slice(prefix.length, prefix.length + 44))
    const u32 = (offset: number) => ((header.charCodeAt(offset) << 24) | (header.charCodeAt(offset + 1) << 16) | (header.charCodeAt(offset + 2) << 8) | header.charCodeAt(offset + 3)) >>> 0
    return header.startsWith("\x89PNG\r\n\x1a\n") && u32(16) === 240 && u32(20) === 304
  } catch { return false }
}

const isCaseBlue = (r: number, g: number, b: number) => b > 90 && b > r + 25 && b > g + 15 && r < 140
const isCaseWhite = (r: number, g: number, b: number) => {
  const l = (r + g + b) / 3
  return l > 175 && Math.abs(r - g) < 28 && Math.abs(g - b) < 28
}
const isCaseDark = (r: number, g: number, b: number) => (r + g + b) / 3 < 38

function chromeRatio(data: Uint8ClampedArray, w: number, h: number, axis: "row" | "col", index: number, edge: boolean) {
  let chrome = 0
  let n = 0
  const step = axis === "row" ? Math.max(1, Math.floor(w / 48)) : Math.max(1, Math.floor(h / 48))
  const limit = axis === "row" ? w : h
  for (let p = 0; p < limit; p += step) {
    const x = axis === "row" ? p : index
    const y = axis === "row" ? index : p
    const i = (y * w + x) * 4
    const r = data[i]
    const g = data[i + 1]
    const b = data[i + 2]
    n += 1
    if (isCaseBlue(r, g, b) || isCaseWhite(r, g, b) || (edge && isCaseDark(r, g, b))) chrome += 1
  }
  return n ? chrome / n : 0
}

export function cropBoxedCover(dataUrl: string) {
  if (dataUrl.startsWith(PLAIN_ART_PREFIX)) return Promise.resolve(dataUrl)
  const existing = plainCache.get(dataUrl)
  if (existing) return existing
  const request = new Promise<string>((resolve) => {
    const image = new Image()
    image.onload = () => {
      const width = image.naturalWidth
      const height = image.naturalHeight
      if (width < 48 || height < 48) {
        resolve(dataUrl)
        return
      }
      const canvas = document.createElement("canvas")
      canvas.width = width
      canvas.height = height
      const ctx = canvas.getContext("2d", { willReadFrequently: true })
      if (!ctx) {
        resolve(dataUrl)
        return
      }
      ctx.drawImage(image, 0, 0)
      const { data } = ctx.getImageData(0, 0, width, height)
      const threshold = 0.52
      let top = 0
      let bottom = height - 1
      let left = 0
      let right = width - 1
      const maxTop = Math.floor(height * 0.28)
      const maxSide = Math.floor(width * 0.14)
      const maxBottom = Math.floor(height * 0.14)
      while (top < maxTop && chromeRatio(data, width, height, "row", top, false) > threshold) top += 1
      while (bottom > height - maxBottom && chromeRatio(data, width, height, "row", bottom, true) > threshold) bottom -= 1
      while (left < maxSide && chromeRatio(data, width, height, "col", left, true) > threshold) left += 1
      while (right > width - maxSide && chromeRatio(data, width, height, "col", right, true) > threshold) right -= 1
      const croppedW = right - left + 1
      const croppedH = bottom - top + 1
      const boxed = top > height * 0.06 && croppedW > 32 && croppedH > 32 && croppedW * croppedH < width * height * 0.9
      if (!boxed) {
        resolve(dataUrl)
        return
      }
      const out = document.createElement("canvas")
      out.width = croppedW
      out.height = croppedH
      const draw = out.getContext("2d")
      if (!draw) {
        resolve(dataUrl)
        return
      }
      draw.drawImage(canvas, left, top, croppedW, croppedH, 0, 0, croppedW, croppedH)
      resolve(out.toDataURL("image/jpeg", 0.92))
    }
    image.onerror = () => resolve(dataUrl)
    image.src = dataUrl
  })
  plainCache.set(dataUrl, request)
  return request
}

export const fallbackArt = (title: string) => {
  const initials = title
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 3)
    .map((word) => word[0]?.toUpperCase())
    .join("") || "GS"
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="600" height="850"><defs><linearGradient id="g" x2="1" y2="1"><stop stop-color="#111"/><stop offset="1" stop-color="#333"/></linearGradient></defs><rect width="600" height="850" fill="url(#g)"/><circle cx="470" cy="190" r="220" fill="#fff" fill-opacity=".08"/><path d="M40 600L560 180M40 700L560 280" stroke="#fff" stroke-opacity=".1" stroke-width="6"/><text x="300" y="455" text-anchor="middle" fill="#fff" font-family="Arial" font-size="128" font-weight="700">${initials}</text><text x="300" y="760" text-anchor="middle" fill="#fff" fill-opacity=".65" font-family="Arial" font-size="20" letter-spacing="6">GAME SEARCH</text></svg>`)}`
}

export const resolveCover = (source?: string) => {
  if (!source) return Promise.reject(new Error("No cover"))
  if (source.startsWith("data:")) return Promise.resolve(source)
  const existing = coverCache.get(source)
  if (existing) return existing
  const request = invoke<string>("fetch_cover", { url: source }).catch((error) => {
    coverCache.delete(source)
    throw error
  })
  coverCache.set(source, request)
  return request
}

export function useCoverSource(source: string | undefined, title: string) {
  const fallback = fallbackArt(title)
  const [resolved, setResolved] = useState(source?.startsWith("data:") ? source : fallback)

  useEffect(() => {
    let live = true
    setResolved(source?.startsWith("data:") ? source : fallback)
    resolveCover(source)
      .then((value) => live && setResolved(value))
      .catch(() => live && setResolved(fallback))
    return () => { live = false }
  }, [source, fallback])

  return resolved
}

export function CoverImage({ source, title, className }: { source?: string; title: string; className?: string }) {
  const resolved = useCoverSource(source, title)
  return <img src={resolved} alt={`${title} cover`} className={cn("block size-full object-cover", className)} draggable={false} />
}

export function platformOf(titleId?: string) {
  if (titleId?.startsWith("CUSA")) return "ps4" as const
  return "ps5" as const
}

export async function notificationCase(source: string | undefined, titleId: string): Promise<string | undefined> {
  if (!source) return undefined
  try {
    const load = (url: string) => new Promise<HTMLImageElement>((resolve, reject) => {
      const img = new Image(); img.onload = () => resolve(img); img.onerror = reject; img.src = url
    })
    const resolved = await resolveCover(source)
    if (isRenderedCase(resolved)) return resolved
    const plain = await cropBoxedCover(resolved)
    const ps4 = platformOf(titleId) === "ps4"
    const [art, shell] = await Promise.all([load(plain), load(ps4 ? ps4Case : ps5Case)])
    const canvas = document.createElement("canvas"); canvas.width = 240; canvas.height = 304
    const ctx = canvas.getContext("2d"); if (!ctx) return source
    const x = 240 * .0215, y = 304 * (ps4 ? .145 : .1485), width = 240 * .9355, height = 304 - y - 304 * .037
    const scale = Math.max(width / art.naturalWidth, height / art.naturalHeight)
    ctx.drawImage(art, (art.naturalWidth - width / scale) / 2, (art.naturalHeight - height / scale) / 2, width / scale, height / scale, x, y, width, height)
    ctx.drawImage(shell, 0, 0, 240, 304)
    return canvas.toDataURL("image/png").replace("data:image/png;base64,", renderedCasePrefix)
  } catch { return source }
}

export function CaseCover({ source, title, titleId, className }: { source?: string; title: string; titleId?: string; className?: string }) {
  const fetched = useCoverSource(source, title)
  const [art, setArt] = useState(fetched)
  const rendered = isRenderedCase(fetched)
  useEffect(() => {
    let live = true
    setArt(fetched)
    if (rendered) return () => { live = false }
    void cropBoxedCover(fetched).then((value) => {
      if (live) setArt(value)
    })
    return () => {
      live = false
    }
  }, [fetched, rendered])
  const ps4 = platformOf(titleId) === "ps4"
  const shell = ps4 ? ps4Case : ps5Case
  if (rendered) return <img src={fetched} alt={`${title} cover`} className={cn("case-shell", className)} draggable={false} />
  return (
    <>
      <div className="case-window" style={{ inset: ps4 ? "14.5% 4.3% 3.7% 2.15%" : "14.85% 4.3% 3.7% 2.15%" }}>
        <img src={art} alt={`${title} cover`} className={cn("case-art", className)} draggable={false} />
      </div>
      <img src={shell} alt="" aria-hidden="true" className="case-shell" draggable={false} />
    </>
  )
}
