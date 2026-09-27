import type { DeliveryJob } from "../types"

const terminal = new Set(["complete", "delivered", "failed", "cancelled", "monitoring-ended"])
export const activeTransfer = (job: DeliveryJob) => !terminal.has(job.stage)
export const transferPhases = ["Download", "Extract", "Package", "Upload", "Install"] as const

export function phaseOf(stage: string) {
  if (["unlocking", "downloading"].includes(stage)) return 0
  if (stage === "extracting") return 1
  if (stage === "packaging") return 2
  if (stage === "uploading") return 3
  if (["submitting", "installing", "mounting", "handoff", "monitoring-ended"].includes(stage)) return 4
  return -1
}

export function kindOf(job: DeliveryJob) {
  if (job.components?.some(c => c.kind === "base") && job.components.some(c => c.kind === "backport")) return "combined"
  if (job.packageKind === "base" && /\+\s*backport\b/i.test(job.packageLabel || "")) return "combined"
  const explicit = job.packageKind?.toLowerCase()
  if (["update", "patch"].includes(explicit || "") && /\bback[ -]?port(?:ed)?\b/i.test(job.packageLabel || "")) return "backport"
  if (explicit) return explicit === "patch" ? "update" : explicit
  const label = job.packageLabel || job.message
  if (/\bbackport\b/i.test(label)) return "backport"
  if (/\b(dlc|add-on)\b/i.test(label)) return "dlc"
  if (/\b(update|patch)\b/i.test(label)) return "update"
  if (/\bbase\b/i.test(label)) return "base"
  return "file"
}

export function titleIdOf(job: DeliveryJob) {
  return `${job.titleId || ""} ${job.packageLabel || ""} ${job.title || ""} ${job.message}`.match(/\b(?:CUSA|PPSA)\d{5}\b/i)?.[0].toUpperCase()
}

export const kindLabel = (kind: string) => ({base: "Base package", combined: "Base + backport", update: "Game update", dlc: "DLC", backport: "Backport", batch: "Manual batch"}[kind] || "Package")

export function jobControls(job: Pick<DeliveryJob, "stage" | "retryable">) {
  const terminal = ["complete", "delivered", "failed", "cancelled", "monitoring-ended"].includes(job.stage)
  const consoleOwned = ["submitting", "installing", "mounting", "handoff"].includes(job.stage)
  return {
    retry: !!job.retryable && ["failed", "cancelled", "monitoring-ended"].includes(job.stage),
    pause: !terminal && !consoleOwned,
    cancel: !terminal && !consoleOwned,
    remove: terminal,
  }
}

export function stageLabel(job: DeliveryJob) {
  if (job.paused) return "Paused"
  if (job.target === "ps4") {
    return ({
      uploading: "Sending to PS4",
      installing: "Installing on PS4",
      complete: "Installed",
      handoff: "Waiting for PS4",
      delivered: "Delivered to PS4",
      "monitoring-ended": "Check PS4",
    }[job.stage] || ({queued: "Queued", unlocking: "Preparing link", downloading: "Downloading", extracting: "Extracting", packaging: "Packaging FPKG", submitting: "Submitting to console", mounting: "Mounting", failed: "Needs attention", cancelled: "Cancelled"}[job.stage] || job.stage))
  }
  return ({queued: "Queued", unlocking: "Preparing link", downloading: "Downloading", extracting: "Extracting", packaging: "Packaging FPKG", uploading: "Uploading", submitting: "Submitting to console", installing: "Installing", mounting: "Mounting", complete: "Complete", handoff: "Waiting for console", delivered: "Delivered", failed: "Needs attention", cancelled: "Cancelled", "monitoring-ended": "Check console"}[job.stage] || job.stage)
}

export function transferPercent(job: DeliveryJob) {
  if (["complete", "delivered"].includes(job.stage)) return 100
  const progress = Number.isFinite(job.progress) ? Math.max(0, job.progress) * 100 : 0
  const bytes = job.bytesTotal && job.bytesTotal > 0 ? Math.max(0, job.bytesDone || 0) / job.bytesTotal * 100 : null
  return Math.round(Math.max(0, Math.min(99, bytes == null ? progress : progress > 0 ? Math.min(bytes, progress) : bytes)))
}

export type DownloadGroup = { key: string; titleId?: string; title: string; icon?: string; jobs: DeliveryJob[]; primary: DeliveryJob; kinds: string[]; createdAt: number }

export function groupDownloads(jobs: DeliveryJob[]): DownloadGroup[] {
  const grouped = new Map<string, DeliveryJob[]>()
  for (const job of jobs) {
    const key = titleIdOf(job) || `job:${job.jobId}`
    grouped.set(key, [...(grouped.get(key) || []), job])
  }
  const order: Record<string, number> = {base: 0, update: 1, backport: 2, dlc: 3}
  return [...grouped].map(([key, rows]) => {
    const newest = [...rows].sort((a, b) => (b.createdAt || 0) - (a.createdAt || 0) || a.jobId.localeCompare(b.jobId))
    const primary = newest.find(j => activeTransfer(j) && j.stage !== "queued") || newest.find(activeTransfer) || newest.find(j => ["failed", "monitoring-ended"].includes(j.stage)) || newest[0]
    const identity = rows.find(j => j.icon && j.title) || rows.find(j => j.title) || primary
    return {
      key, titleId: titleIdOf(primary), title: identity.title || titleIdOf(primary) || "Local package",
      icon: rows.find(j => j.icon)?.icon, primary,
      jobs: [...rows].sort((a, b) => (order[kindOf(a)] ?? 4) - (order[kindOf(b)] ?? 4) || (a.createdAt || 0) - (b.createdAt || 0) || a.jobId.localeCompare(b.jobId)),
      kinds: [...new Set(rows.flatMap(j => kindOf(j) === "combined" ? ["base", "backport"] : [kindOf(j)]))], createdAt: Math.max(...rows.map(j => j.createdAt || 0)),
    }
  }).sort((a, b) => b.createdAt - a.createdAt || a.key.localeCompare(b.key))
}

export function phaseState(job: DeliveryJob, index: number): "done" | "current" | "failed" | "skipped" | "pending" {
  if (job.localPkg && job.target === "ps4" && job.stage === "complete" && index === 4) return "done"
  const seen: number[] = (job.stageHistory || []).map(phaseOf).filter(i => i >= 0)
  const current = phaseOf(job.stage)
  const last = current >= 0 ? current : seen[seen.length - 1] ?? -1
  const succeeded = ["complete", "delivered"].includes(job.stage)
  if (["failed", "cancelled"].includes(job.stage) && last === index) return "failed"
  if (!succeeded && last === index) return "current"
  if (seen.includes(index) && (succeeded || last > index)) return "done"
  if (succeeded || last > index) return "skipped"
  return "pending"
}

export function transferSize(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B"
  const unit = Math.min(3, Math.floor(Math.log(bytes) / Math.log(1024)))
  return `${(bytes / 1024 ** unit).toFixed(unit > 0 ? 1 : 0)} ${["B", "KB", "MB", "GB"][unit]}`
}

export function transferStats(job: DeliveryJob, upload?: UploadState | null) {
  const detail = phaseDetail(job)
  const tracked = job.stage === "uploading" ? upload : null
  const done = tracked ? tracked.bytesDone : job.bytesDone || 0
  const total = tracked ? tracked.bytesTotal : job.bytesTotal
  const transferred = !!total && done >= total
  let label = total ? `${transferSize(done)} / ${transferSize(total)}`
    : job.stage === "extracting" ? detail.doneBytes != null ? `${transferSize(detail.doneBytes)} so far · ${detail.label}` : detail.label
    : stageLabel(job)
  if (job.stage === "extracting" && detail.phase === "finalization") label = detail.label
  if (job.target === "ps4" && job.stage === "installing") label = transferred ? "Transfer complete · installing on PS4" : "Installing on PS4"
  if (job.target === "ps4" && job.stage === "monitoring-ended") label = transferred ? "Transfer complete · installation unconfirmed" : "Installation confirmation unavailable"
  const byteWork = !job.paused && activeTransfer(job) && !transferred &&
    (job.stage === "downloading" || job.stage === "uploading" || (job.stage === "extracting" && detail.phase === "extraction"))
  const rate = tracked ? tracked.speedBps : job.speedBps
  const speedBps = byteWork && Number.isFinite(rate) && (rate > 0 || (job.stage === "extracting" && done > 0)) ? Math.max(0, rate) : null
  const eta = tracked && rate > 0 ? Math.ceil((tracked.bytesTotal - tracked.bytesDone) / rate) : tracked ? null : job.etaSeconds
  return { label, speedBps, etaSeconds: byteWork && eta != null && Number.isFinite(eta) && eta > 0 ? eta : null }
}

// ---------------------------------------------------------------------------
// Windows storage, extraction phases and honest upload reporting.
//
// The backend keeps owning downloads, retries and the mount protocol. This
// layer only describes where work lands (never assuming C:), surfaces explicit
// phases from measured bytes/files, and sanitizes progress/speed display so
// impossible spikes and wrong bars cannot reach the UI.
// ---------------------------------------------------------------------------

export type StorageLocations = { downloadDir: string; extractionDir: string; stagingDir: string }

const trimTrailing = (path: string) => path.trim().replace(/[/\\]+$/, "")
const joinWin = (base: string, leaf: string) => (base ? `${base}\\${leaf}` : leaf)

/** Volume holding a Windows path: drive prefix ("D:") or UNC host+share. Null for relative paths. Never assumes C:. */
export function volumeOf(path: string): string | null {
  const raw = (path || "").trim().replace(/\//g, "\\")
  if (raw.startsWith("\\\\")) {
    const parts = raw.slice(2).split("\\").filter(Boolean)
    return parts.length >= 2 ? `\\\\${parts[0]}\\${parts[1]}` : null
  }
  const match = /^([A-Za-z]):/.exec(raw)
  return match ? `${match[1].toUpperCase()}:` : null
}

export function isSystemDrivePath(path: string | null | undefined, systemPrefix: string | null | undefined): boolean {
  if (!path || !systemPrefix) return false
  const volume = volumeOf(path)
  return volume != null && volume.toUpperCase() === systemPrefix.trim().toUpperCase()
}

/** Download/extraction/staging destinations derived from the chosen download folder. Temp extraction files always stay under it. */
export function stagingLocations(downloadDir: string): StorageLocations {
  const root = trimTrailing(downloadDir || "")
  return { downloadDir: root, extractionDir: root ? joinWin(root, "extracted") : "", stagingDir: root }
}

export function sanitizeSetId(setId: string): string {
  const cleaned = (setId || "").split("").map(c => /[A-Za-z0-9]/.test(c) ? c : "_").join("")
  return cleaned.replace(/_/g, "").length === 0 ? "set" : cleaned
}

/** Multipart staging ("archive_<set>") for a chosen download folder. */
export function stagingLocationsForSet(downloadDir: string, setId?: string | null): StorageLocations {
  const base = stagingLocations(downloadDir)
  return setId ? { ...base, stagingDir: joinWin(base.downloadDir, `archive_${sanitizeSetId(setId)}`) } : base
}

export type StagingSelection =
  { ok: true; warning?: string; locations: StorageLocations } |
  { ok: false; error: string; locations: StorageLocations }

/** Validates a folder selection: missing folders block, system-drive placement warns (never silently falls back). */
export function validateStagingSelection(downloadDir: string, systemPrefix?: string | null): StagingSelection {
  const trimmed = (downloadDir || "").trim()
  if (!trimmed) {
    return {
      ok: false,
      error: "Download folder is required. Choose a folder/drive in Settings before starting.",
      locations: stagingLocations(""),
    }
  }
  const locations = stagingLocations(trimmed)
  const warning = systemDriveWarning(
    { download: locations.downloadDir, extraction: locations.extractionDir, staging: locations.stagingDir },
    systemPrefix,
  )
  return warning ? { ok: true, warning, locations } : { ok: true, locations }
}

/** Clear warning naming every affected path on the Windows/system drive, with a settings shortcut hint. Null when fine or the system volume is unknown (never guess C:). */
export function systemDriveWarning(
  roles: Record<string, string | null | undefined>,
  systemPrefix: string | null | undefined,
): string | null {
  if (!systemPrefix) return null
  const hits = Object.entries(roles).filter(([, path]) => isSystemDrivePath(path, systemPrefix))
  if (hits.length === 0) return null
  return `Warning: ${hits.map(([role, path]) => `${role} (${path})`).join(", ")} ${hits.length === 1 ? "is" : "are"} on the Windows system drive (${systemPrefix.trim().toUpperCase()}). Downloads and extraction can fill the drive Windows runs on — choose a different folder/drive in Settings before starting.`
}

export type JobPaths = { downloadPaths?: string[]; extractionDir?: string | null; outputPaths?: string[] }

/** Retains an active job's recorded paths across events/retries so retry inputs are never lost. Later records only add; nothing is dropped. */
export function retainJobPaths(prev: JobPaths | undefined, next: JobPaths): JobPaths {
  const union = (...lists: Array<string[] | undefined>) =>
    [...new Set(lists.flatMap(list => list || []).filter(Boolean))]
  return {
    downloadPaths: union(prev?.downloadPaths, next.downloadPaths),
    extractionDir: next.extractionDir ?? prev?.extractionDir ?? null,
    outputPaths: union(prev?.outputPaths, next.outputPaths),
  }
}

export type WorkPhase = "inspection" | "extraction" | "finalization" | "packaging" | "cleanup" | "staging" | "upload" | "unknown"

type JobSnapshot = Pick<DeliveryJob, "stage" | "progress" | "bytesDone" | "bytesTotal" | "message" | "packageLabel"> & { stageHistory?: string[] }

/** Recovers the failing phase from a tagged backend error ("extraction/inspection: …", "cleanup: …"); "unknown" for untagged errors rather than guessing. */
export function errorWorkPhase(message: string): WorkPhase {
  const text = message || ""
  if (/^packaging(?:[/:]|\b)/i.test(text)) return "packaging"
  const tagged = /^extraction\/(inspection|extraction|finalization)/i.exec(text)?.[1].toLowerCase()
  if (tagged === "inspection" || tagged === "extraction" || tagged === "finalization") return tagged
  if (/^cleanup:/i.test(text)) return "cleanup"
  if (/^staging:/i.test(text)) return "staging"
  if (/^upload:/i.test(text)) return "upload"
  if (/not enough (free space|extraction space)/i.test(text)) return "extraction"
  if (/download (failed|stalled)|unlock/i.test(text)) return "staging"
  if (/START_UPLOAD|UPLOAD_CHUNK|END_UPLOAD|verif/i.test(text)) return "upload"
  return "unknown"
}

export type ErrorContext =
  { phase: WorkPhase; origin: string; affected: string; detail: string; retryHint: string }

const retryHintFor = (phase: WorkPhase): string => {
  if (phase === "inspection" || phase === "extraction" || phase === "finalization") return "Archive kept for retry."
  if (phase === "cleanup") return "Extracted outputs kept — retry cleanup."
  if (phase === "staging") return "Inputs kept — retry download."
  if (phase === "packaging") return "Extracted dump kept — retry packaging."
  if (phase === "upload") return "Extracted outputs kept — retry upload."
  return ""
}

/** Exact failing phase + affected file for failed/cancelled jobs. Null while work is still in flight. */
export function errorContext(job: JobSnapshot): ErrorContext | null {
  if (!["failed", "cancelled", "monitoring-ended"].includes(job.stage)) return null
  const history = job.stageHistory || []
  const origin = history.length > 0 ? history[history.length - 1] : ""
  let phase = errorWorkPhase(job.message || "")
  if (phase === "unknown") {
    if (["unlocking", "downloading", "queued"].includes(origin)) phase = "staging"
    else if (origin === "extracting") phase = "extraction"
    else if (origin === "packaging") phase = "packaging"
    else if (["uploading", "submitting", "installing", "mounting"].includes(origin)) phase = "upload"
  }
  return {
    phase,
    origin: origin || job.stage,
    affected: job.packageLabel || "",
    detail: job.message || "",
    retryHint: retryHintFor(phase),
  }
}

export type ByteProgress = { doneBytes: number; totalBytes: number | null }
const GB = 1024 ** 3
const MB = 1024 ** 2

/** Measured extraction bytes from backend messages ("Extracting 1.23 / 4.56 GB · 78.9 MB/s"); null when the message carries no measurement. Unknown totals stay null (indeterminate), never invented. */
export function parseExtractionProgress(message: string): ByteProgress | null {
  const match = /extracting\s+(?:(?:base game|backport)\s*:\s*)?([\d.]+)\s*(Gi?B|Mi?B)?\s*(?:\/\s*([\d.]+)\s*(Gi?B|Mi?B))?/i.exec(message || "")
  if (!match || (!match[2] && !match[4])) return null
  const scale = (unit: string) => (unit.toUpperCase().startsWith("G") ? GB : MB)
  const doneBytes = Math.round(parseFloat(match[1]) * scale(match[2] || match[4]))
  if (!Number.isFinite(doneBytes)) return null
  if (!match[3]) return { doneBytes, totalBytes: null }
  const totalBytes = Math.round(parseFloat(match[3]) * scale(match[4]))
  return Number.isFinite(totalBytes) ? { doneBytes, totalBytes } : { doneBytes, totalBytes: null }
}

export type PhaseDetail =
  { phase: WorkPhase; label: string; indeterminate: boolean; doneBytes?: number; totalBytes?: number | null; filesDone?: number; filesTotal?: number }

/** Explicit pipeline phase for a job snapshot: inspection → extraction → finalization → cleanup/staging/upload. Unknown totals report counts + indeterminate progress. */
export function phaseDetail(job: JobSnapshot): PhaseDetail {
  if (job.stage === "extracting") {
    if (/^waiting for another extraction/i.test(job.message || "")) {
      return { phase: "extraction", label: "Waiting for another extraction on this drive", indeterminate: false }
    }
    const extracted = /extracted\s+(\d+)\s+pkgs?/i.exec(job.message || "")
    if (extracted) {
      const files = Number(extracted[1])
      return { phase: "finalization", label: "Validating outputs", indeterminate: false, filesDone: files, filesTotal: files }
    }
    const measured = (job.bytesTotal || 0) > 0 || (job.bytesDone || 0) > 0
      ? { doneBytes: job.bytesDone || 0, totalBytes: job.bytesTotal || null }
      : parseExtractionProgress(job.message || "")
    const label = /extracting base game/i.test(job.message || "") ? "Extracting base game" : /extracting backport/i.test(job.message || "") ? "Extracting backport" : "Extracting"
    if (measured && measured.totalBytes && measured.doneBytes >= measured.totalBytes) {
      return { phase: "finalization", label: "Finishing extraction", indeterminate: true, doneBytes: measured.doneBytes, totalBytes: measured.totalBytes }
    }
    if (measured && measured.totalBytes) {
      return { phase: "extraction", label, indeterminate: false, doneBytes: measured.doneBytes, totalBytes: measured.totalBytes }
    }
    if (measured) {
      return { phase: "extraction", label, indeterminate: true, doneBytes: measured.doneBytes, totalBytes: null }
    }
    if (/cleanup:/i.test(job.message || "")) {
      return { phase: "cleanup", label: "Retrying cleanup", indeterminate: true }
    }
    return { phase: "inspection", label: "Inspecting archive", indeterminate: true }
  }
  if (job.stage === "packaging") {
    return { phase: "packaging", label: "Packaging FPKG", indeterminate: !(job.progress > 0) }
  }
  if (job.stage === "uploading") {
    if (/retrying/i.test(job.message || "")) return { phase: "upload", label: "Retrying upload", indeterminate: !(job.bytesTotal || 0) }
    return { phase: "upload", label: "Sending to console", indeterminate: !(job.bytesTotal || 0) }
  }
  if (job.stage === "queued" || job.stage === "unlocking" || job.stage === "downloading") {
    return { phase: "staging", label: job.stage === "queued" ? "Queued" : "Downloading", indeterminate: !(job.bytesTotal || 0) }
  }
  if (job.stage === "submitting" || job.stage === "installing" || job.stage === "mounting") {
    return { phase: "upload", label: "Confirming on console", indeterminate: false }
  }
  if (job.stage === "complete") return { phase: "upload", label: "Complete", indeterminate: false }
  const error = errorContext(job)
  if (error) {
    return {
      phase: error.phase,
      label: job.stage === "cancelled" ? "Cancelled" : job.stage === "monitoring-ended" ? "Check console" : "Failed",
      indeterminate: false,
    }
  }
  return { phase: "unknown", label: job.stage || "Working", indeterminate: true }
}

export type UploadEvent = { bytesDone: number; bytesTotal: number; attempt?: string | number; confirmed?: boolean }
export type UploadState =
  { bytesDone: number; bytesTotal: number; fraction: number; speedBps: number; stalled: boolean; confirmed: boolean }
export type UploadTrackerOptions =
  { windowMs?: number; minSampleBytes?: number; stallAfterMs?: number; now?: () => number }

const clampNumber = (value: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, value))

/** Bar fraction with valid bounds: 1 only after final confirmation, never from bytes alone. */
export function uploadFraction(bytesDone: number, bytesTotal: number, confirmed: boolean): number {
  if (!Number.isFinite(bytesDone) || !Number.isFinite(bytesTotal) || bytesTotal <= 0) return 0
  if (confirmed && bytesDone >= bytesTotal) return 1
  return clampNumber(bytesDone / bytesTotal, 0, 0.99)
}

/**
 * Monotonic aggregate upload progress across files. Stable totals (never
 * shrink), no duplicate counting (out-of-order/duplicate bytes are ignored),
 * resumed baselines kept separate from newly transferred bytes, stale attempts
 * ignored, and speed measured from transfer deltas over a bounded 1–5 s
 * monotonic-clock window: undersized samples hold the previous rate instead of
 * inventing spikes, and stalled rates expire to zero.
 */
export class UploadTracker {
  private readonly windowMs: number
  private readonly minSampleBytes: number
  private readonly stallAfterMs: number
  private readonly clock: () => number
  private samples: Array<{ at: number; bytes: number }> = []
  private baseline = 0
  private lastBytes = 0
  private total = 0
  private attempt: string | number | undefined = undefined
  private lastAdvanceAt = 0
  private rate = 0
  private confirmed = false
  private started = false

  constructor(options: UploadTrackerOptions = {}) {
    this.windowMs = clampNumber(options.windowMs ?? 3000, 1000, 5000)
    this.minSampleBytes = Math.max(0, options.minSampleBytes ?? 256 * 1024)
    this.stallAfterMs = Math.max(this.windowMs, options.stallAfterMs ?? 5000)
    this.clock = options.now ?? (() => Date.now())
  }

  /** New attempt or resume point: old bytes are never double-counted and the rate restarts honestly. */
  reset(baselineBytes: number, totalBytes: number, attempt?: string | number): UploadState {
    this.baseline = Math.max(0, Math.floor(baselineBytes) || 0)
    this.total = Math.max(0, Math.floor(totalBytes) || 0)
    this.lastBytes = this.baseline
    this.samples = []
    this.rate = 0
    this.confirmed = false
    this.attempt = attempt
    this.lastAdvanceAt = this.clock()
    this.started = true
    return this.snapshot()
  }

  update(event: UploadEvent): UploadState {
    const now = this.clock()
    const total = Math.floor(event.bytesTotal) || 0
    if (total > this.total) this.total = total
    if (event.attempt !== undefined && this.attempt !== undefined && event.attempt !== this.attempt) {
      if (typeof event.attempt === "number" && typeof this.attempt === "number") {
        if (event.attempt < this.attempt) return this.snapshot(now)
        return this.reset(
          Math.min(Math.floor(event.bytesDone) || 0, this.total || Math.floor(event.bytesDone) || 0),
          this.total || total,
          event.attempt,
        )
      }
      return this.snapshot(now)
    }
    if (event.attempt !== undefined) this.attempt = event.attempt
    const ceiling = this.total > 0 ? this.total : Math.floor(event.bytesDone) || 0
    const clamped = clampNumber(Math.floor(event.bytesDone) || 0, this.baseline, Math.max(ceiling, this.baseline))
    if (event.confirmed === true && this.total > 0 && clamped >= this.total) {
      this.lastBytes = this.total
      this.confirmed = true
      this.rate = 0
      return this.snapshot(now)
    }
    if (clamped > this.lastBytes) {
      this.lastBytes = clamped
      this.lastAdvanceAt = now
      this.samples.push({ at: now, bytes: clamped })
      const cutoff = now - this.windowMs
      while (this.samples.length > 1 && this.samples[0].at < cutoff) this.samples.shift()
      const first = this.samples[0]
      const dtMs = now - first.at
      const delta = clamped - first.bytes
      if (dtMs >= 1000 && delta >= this.minSampleBytes) {
        const measured = (delta / dtMs) * 1000
        this.rate = Number.isFinite(measured) && measured >= 0 ? measured : 0
      }
    }
    return this.snapshot(now)
  }

  current(): UploadState {
    return this.snapshot()
  }

  private snapshot(now?: number): UploadState {
    const at = now ?? this.clock()
    const stalled = this.started && !this.confirmed && this.rate > 0 && at - this.lastAdvanceAt > this.stallAfterMs
    return {
      bytesDone: this.lastBytes,
      bytesTotal: this.total,
      fraction: uploadFraction(this.lastBytes, this.total, this.confirmed),
      speedBps: stalled ? 0 : this.rate,
      stalled,
      confirmed: this.confirmed,
    }
  }
}

/** Display percent: tracker-backed while sending (bounded, 100 only when complete), legacy otherwise. */
export function honestPercent(job: DeliveryJob, tracker?: UploadState | null): number {
  if (["complete", "delivered"].includes(job.stage)) return 100
  if (tracker && job.bytesTotal && job.bytesTotal > 0) return Math.round(tracker.fraction * 100)
  return transferPercent(job)
}
