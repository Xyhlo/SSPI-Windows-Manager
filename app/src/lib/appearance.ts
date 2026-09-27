import { hexToRgb } from "./format"

/* Accents and backdrop patterns from SSPI PS4 (ThemePalette.cs / BackdropPattern.cs). */
export const ACCENTS = [
  { name: "Charcoal", hex: "#E4E4E1" },
  { name: "Steel Ice", hex: "#7EB6FF" },
  { name: "Arctic", hex: "#E8EEF7" },
  { name: "Violet", hex: "#A78BFA" },
  { name: "Crimson", hex: "#FF4D4D" },
  { name: "Amber", hex: "#F5A623" },
  { name: "Teal", hex: "#2EE6C5" },
  { name: "Pink", hex: "#F58BBF" },
] as const

export const PATTERNS = [
  { id: "solid", name: "Plain charcoal" },
  { id: "ripple", name: "Ripple" },
  { id: "wave", name: "Wave" },
  { id: "grid", name: "Mosaic" },
  { id: "halo", name: "Halo" },
  { id: "graphite", name: "Graphite fade" },
  { id: "obsidian", name: "Obsidian" },
  { id: "slate", name: "Slate glow" },
  { id: "flowers", name: "ASCII Flowers" },
] as const

export const patternIndex = (id: string) => Math.max(0, PATTERNS.findIndex(pattern => pattern.id === id))

/** The backdrop pattern is drawn at reduced strength so it stays behind the interface. */
export const PATTERN_STRENGTH = 0.58

/** Look-and-feel preferences. They only affect this PC's window, so they live in the WebView's storage. */
export type Appearance = { accent: string; pattern: string; gameTint: boolean; statsForNerds: boolean }

export const DEFAULT_APPEARANCE: Appearance = { accent: "#E4E4E1", pattern: "ripple", gameTint: false, statsForNerds: true }

const KEY = "sspi.appearance.v1"

export function loadAppearance(): Appearance {
  try {
    const saved = JSON.parse(window.localStorage.getItem(KEY) || "null") as Partial<Appearance> | null
    if (!saved || typeof saved !== "object") return { ...DEFAULT_APPEARANCE }
    return {
      accent: ACCENTS.some(a => a.hex === saved.accent) ? saved.accent! : DEFAULT_APPEARANCE.accent,
      pattern: PATTERNS.some(p => p.id === saved.pattern) ? saved.pattern! : DEFAULT_APPEARANCE.pattern,
      gameTint: typeof saved.gameTint === "boolean" ? saved.gameTint : DEFAULT_APPEARANCE.gameTint,
      statsForNerds: typeof saved.statsForNerds === "boolean" ? saved.statsForNerds : DEFAULT_APPEARANCE.statsForNerds,
    }
  } catch {
    return { ...DEFAULT_APPEARANCE }
  }
}

export function saveAppearance(appearance: Appearance) {
  try { window.localStorage.setItem(KEY, JSON.stringify(appearance)) } catch { /* storage unavailable: keep the session value */ }
}

export function applyAccentVars(hex: string) {
  const [r, g, b] = hexToRgb(hex)
  document.documentElement.style.setProperty("--accent", hex)
  document.documentElement.style.setProperty("--accent-rgb", `${r}, ${g}, ${b}`)
}
