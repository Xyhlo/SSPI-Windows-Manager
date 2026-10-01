/* FPKG packaging display. Every figure comes from the engine's telemetry (builder milestones and
   the engine process's own I/O counters); nothing here estimates progress it cannot measure. */
import type { PackEngine, PackStage, PackagingInfo } from "../types"

// Same output as format.ts; kept local so this module has no runtime imports (node --test).
function fmtBytes(bytes: number | null | undefined) {
  if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return "0 B"
  const units = ["B", "KB", "MB", "GB", "TB"], unit = Math.min(4, Math.floor(Math.log(bytes) / Math.log(1024))), value = bytes / 1024 ** unit
  return `${value.toFixed(unit === 0 ? 0 : value >= 100 ? 0 : 1)} ${units[unit]}`
}
const fmtSpeed = (bps: number) => `${fmtBytes(bps)}/s`
export { fmtBytes as packBytes, fmtSpeed as packSpeed }

/** `\\?\D:\x` -> `D:\x`, `\\?\UNC\s\x` -> `\\s\x`. Older builds stored verbatim paths. */
export function plainPath(path: string) {
  if (path.startsWith("\\\\?\\UNC\\")) return `\\\\${path.slice(8)}`
  return path.startsWith("\\\\?\\") ? path.slice(4) : path
}

/** 0.4 s, 11.2 s, 42.6 s, 3 min 12 s, 1 h 04 min. */
export function fmtDuration(seconds: number | null | undefined) {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "–"
  if (seconds < 59.95) return `${seconds.toFixed(1)} s`
  const total = Math.round(seconds), h = Math.floor(total / 3600), m = Math.floor((total % 3600) / 60), s = total % 60
  return h ? `${h} h ${String(m).padStart(2, "0")} min` : `${m} min ${String(s).padStart(2, "0")} s`
}

/** Clock-style elapsed time for live counters: 0:07, 3:12, 1:04:09. */
export function fmtClock(seconds: number | null | undefined) {
  const total = Math.max(0, Math.floor(seconds || 0)), h = Math.floor(total / 3600), m = Math.floor((total % 3600) / 60), s = total % 60
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`
}

export const packEngine = (info?: PackagingInfo | null): PackEngine | null => info?.engine && Array.isArray(info.engine.stages) ? info.engine : null

export const activeStage = (engine: PackEngine): PackStage | undefined => engine.stages.find(stage => stage.state === "active")

/** The byte stream that is the work right now: reads while compressing or checking, writes while building the outer PFS or an image. */
export function engineRate(engine: PackEngine): { bps: number | null; direction: "read" | "write" | null } {
  if (engine.stage === "compress" || engine.stage === "lizard" || engine.stage === "verify") return { bps: engine.io.readBps ?? null, direction: "read" }
  if (engine.stage === "outer" || engine.stage === "write") return { bps: engine.io.writeBps ?? null, direction: "write" }
  return { bps: null, direction: null }
}

/** Measured progress of the active stage (0..1), or null where the engine cannot measure it. */
export function stageProgress(info?: PackagingInfo | null) {
  const engine = packEngine(info)
  if (engine) return engine.stageProgress ?? null
  return info?.phaseProgress ?? null
}

/** One header line while packaging: stage, measured work, rate and the stage's own ETA. */
export function packHeadline(info: PackagingInfo) {
  const engine = packEngine(info)
  if (!engine) return info.activity || (info.format === "exfat" ? "Building the exFAT image" : "Packaging FPKG")
  const stage = activeStage(engine)
  const parts = [`${engine.stageIndex}/${engine.stageCount} ${stage?.label || engine.stage}`]
  const { bps } = engineRate(engine)
  if (engine.stage === "compress" && engine.inputBytes) {
    parts.push(`${fmtBytes(Math.min(engine.inputBytes, (stage?.progress ?? 0) * engine.inputBytes))} of ${fmtBytes(engine.inputBytes)} read`)
  } else if (stage?.detail) parts.push(stage.detail)
  if (bps) parts.push(fmtSpeed(bps))
  if (engine.etaSeconds != null && engine.etaSeconds >= 1) parts.push(`${fmtDuration(engine.etaSeconds)} left in stage`)
  return parts.join(" · ")
}

/** Completion line: input -> package, ratio, total time. */
export function packSummary(info: PackagingInfo) {
  const engine = packEngine(info)
  const input = info.format === "exfat" ? info.inputBytes || engine?.inputBytes || 0 : engine?.inputBytes || info.inputBytes
  const ratio = input && info.outputBytes ? ` (${(100 * info.outputBytes / input).toFixed(1)}%)` : ""
  const time = info.totalSeconds ?? info.elapsedSeconds
  return `${info.format === "exfat" ? "Imaged" : "Packaged"} ${fmtBytes(input)} into ${fmtBytes(info.outputBytes)}${ratio} in ${fmtDuration(time)}`
}
