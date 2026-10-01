/* FPKG compression presets. The packager stays simple: pick one (or keep the default) and pack.
   Levels mirror fpkg.rs PackagePreset; tune both together. */

export type FpkgPreset = "fastest" | "balanced" | "smallest"

export const FPKG_PRESETS: Record<FpkgPreset, { level: number; label: string; note: string }> = {
  fastest: { level: 1, label: "Fastest", note: "Quicker; packages come out slightly larger" },
  balanced: { level: 3, label: "Balanced", note: "Recommended for most games" },
  smallest: { level: 7, label: "Smallest", note: "Much slower for a package a few percent smaller" },
}

/** Older settings used fast / standard / smallest. */
export function presetOf(value: string | null | undefined): FpkgPreset {
  const v = (value || "").trim().toLowerCase()
  if (v === "fastest" || v === "fast") return "fastest"
  if (v === "smallest" || v === "small") return "smallest"
  return "balanced"
}

/** Kraken level names as the builder uses them: −4..−1 HyperFast4..1, 1..9 SuperFast..Optimal5. */
export function levelName(level: number) {
  if (level < 0) return `HyperFast${-level}`
  return ["", "SuperFast", "VeryFast", "Fast", "Normal", "Optimal1", "Optimal2", "Optimal3", "Optimal4", "Optimal5"][level] || `Level ${level}`
}

export const FPKG_LEVELS = [-4, -3, -2, -1, 1, 2, 3, 4, 5, 6, 7, 8, 9]

/** The level a build will use: an exact level set under Advanced wins over the preset. */
export const effectiveLevel = (preset: string, level?: number | null) => level ?? FPKG_PRESETS[presetOf(preset)].level
