/* =====================================================================
   Game icon masks, the way Icon Mask works: each game's own icon is
   read from the console, cut to a shape with an optional border and
   glass, and written back. Nothing here depends on the system theme.
   ===================================================================== */
import { DEFAULT_SPEC, renderGameIcon, type IconShape, type ThemeSpec } from "./ps4-theme"

export type MaskSpec = {
  shape: IconShape
  /** Corner radius for "rounded", as a share of the icon width. */
  radius: number
  /** Border width at 512 px (0 = none); it traces the shape. */
  border: number
  borderColor: string
  /** A soft glow on the border, kept inside the shape. */
  glow: boolean
  glass: boolean
}
export const MASK_PRESETS: Array<{ id: string; label: string; mask: MaskSpec }> = [
  { id: "rounded", label: "Rounded", mask: { shape: "rounded", radius: 0.18, border: 0, borderColor: "#ffffff", glow: false, glass: false } },
  { id: "circle", label: "Circle", mask: { shape: "circle", radius: 0.18, border: 6, borderColor: "#ffffff", glow: false, glass: false } },
  { id: "squircle", label: "Squircle", mask: { shape: "squircle", radius: 0.18, border: 0, borderColor: "#ffffff", glow: false, glass: true } },
  { id: "neon", label: "Neon frame", mask: { shape: "rounded", radius: 0.2, border: 8, borderColor: "#ff3ec8", glow: true, glass: false } },
]
export const DEFAULT_MASK: MaskSpec = MASK_PRESETS[0].mask

const KEY = "sspi.icon-mask.v1"
export function loadMask(): MaskSpec {
  try { return { ...DEFAULT_MASK, ...(JSON.parse(window.localStorage.getItem(KEY) || "null") || {}) } } catch { return DEFAULT_MASK }
}
export function saveMask(mask: MaskSpec) {
  try { window.localStorage.setItem(KEY, JSON.stringify(mask)) } catch { /* the session keeps it */ }
}

/** A game icon cut to the mask, transparent outside the shape. */
export function maskIcon(mask: MaskSpec, art: CanvasImageSource & { width: number; height: number }, size = 512) {
  const spec: ThemeSpec = {
    ...DEFAULT_SPEC,
    shape: mask.shape, radius: mask.radius, border: mask.border, borderColor: mask.borderColor,
    primary: mask.borderColor, glass: mask.glass, glyph: mask.glow ? "neon" : "line",
  }
  return renderGameIcon(spec, art, size)
}
