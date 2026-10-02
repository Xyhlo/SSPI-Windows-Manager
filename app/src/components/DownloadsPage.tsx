/* =====================================================================
   Downloads — the original list: one card per game, newest first. The
   selected card opens in place like the SSPI PS4 drawer (Packages, Files,
   Errors); "Stats for nerds" adds the speed graph and the parts or
   packaging panel. The list is the part of this page that scrolls.
   ===================================================================== */
import { invoke } from "@tauri-apps/api/core"
import { open as openDialog } from "@tauri-apps/plugin-dialog"
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { CaseAnchor } from "./CaseAnchor"
import { ActionBubbleMenu } from "./ActionBubbleMenu"
import { Collapse } from "./Collapse"
import { DoctorReportView } from "./DoctorReportView"
import { ImportDialog, type ManualRow } from "./Dialogs"
import { CloudFilesDialog, PasteLinksDialog } from "./LinkDialogs"
import { Icon } from "./Icon"
import { useJobActions } from "./JobActions"
import type { OptionsTab } from "./OptionsOverlay"
import { SpeedChart } from "./SpeedChart"
import { toast } from "./toasts"
import type { CardSize, CardStyle } from "@/lib/appearance"
import { consoleAddress, jobTarget, sendBlockReason } from "@/lib/consoles"
import {
  activeTransfer, errorContext, groupDownloads, honestPercent, kindLabel, kindOf, phaseDetail, phaseState, stageLabel,
  stagingLocations, systemDriveWarning, transferPhases, transferStats, UploadTracker, type DownloadGroup, type UploadState,
} from "@/lib/downloads"
import { displayText } from "@/lib/display"
import { errorText, fmtBytes, fmtEta, fmtSpeed, plural } from "@/lib/format"
import { activeStage, engineRate, fmtClock, fmtDuration, packEngine, packHeadline, packSummary, plainPath, stageProgress } from "@/lib/packaging"
import { setPageKeys } from "@/lib/keys"
import { clamp, glide } from "@/lib/motion"
import { speedNow, speedSamples } from "@/lib/speed"
import { coverTint } from "@/stage/art"
import { getStage } from "@/stage/stage"
import type { ConsoleKind, DeliveryJob, LocalPackage, ManualCandidate, ManualItem, PackEngine, Settings } from "@/types"

type DrawerTab = "packages" | "files" | "errors"
type Props = {
  jobs: DeliveryJob[]
  demo: boolean
  settings: Settings
  systemDrive: string | null
  filter: string
  setFilter: (value: string) => void
  open: string | null | undefined
  setOpen: (value: string | null) => void
  drawer: DrawerTab
  setDrawer: (value: DrawerTab) => void
  statsForNerds: boolean
  setStatsForNerds: (value: boolean) => void
  ensureConsole: (target: ConsoleKind) => Promise<boolean>
  onOptions: (tab: OptionsTab) => void
  onSearch: () => void
  onOpenGame: (group: DownloadGroup) => void
  tintOn: boolean
  cardStyle: CardStyle
  cardSize: CardSize
}

const FILTERS = ["All", "Active", "Attention"] as const
const attention = (job: DeliveryJob) => ["failed", "monitoring-ended"].includes(job.stage)
const consoleOwned = (job: DeliveryJob) => ["submitting", "installing", "mounting", "handoff"].includes(job.stage)

/** Monotonic per-job upload trackers: sending bytes and speed come from a bounded window over aggregate bytes, never raw spikes. */
function useUploadTrackers() {
  const trackers = useRef(new Map<string, UploadTracker>())
  const stages = useRef(new Map<string, string>())
  return (job: DeliveryJob): UploadState | null => {
    if (job.stage !== "uploading") {
      if (!activeTransfer(job)) { trackers.current.delete(job.jobId); stages.current.delete(job.jobId) }
      else stages.current.set(job.jobId, job.stage)
      return null
    }
    let tracker = trackers.current.get(job.jobId)
    if (!tracker || stages.current.get(job.jobId) !== "uploading") {
      tracker = new UploadTracker()
      tracker.reset(job.bytesDone || 0, job.bytesTotal || 0)
      trackers.current.set(job.jobId, tracker)
    }
    stages.current.set(job.jobId, "uploading")
    return tracker.update({ bytesDone: job.bytesDone || 0, bytesTotal: job.bytesTotal || 0 })
  }
}

function barPercent(job: DeliveryJob, upload?: UploadState | null) {
  // One number while packaging: the active stage's measured progress (the segmented bar shows the rest).
  if (job.stage === "packaging") return Math.round((stageProgress(job.packaging) ?? 0) * 100)
  if (job.stage === "extracting") {
    const detail = phaseDetail(job)
    if (detail.doneBytes != null && detail.totalBytes) return Math.round(Math.min(0.99, detail.doneBytes / detail.totalBytes) * 100)
  }
  return honestPercent(job, upload)
}

function barIndeterminate(job: DeliveryJob) {
  const active = activeTransfer(job)
  if (job.stage === "queued" || /^waiting for another extraction/i.test(job.message || "")) return false
  if (job.stage === "packaging") return !job.paused && active && stageProgress(job.packaging) == null
  if (phaseDetail(job).indeterminate) return !job.paused && active
  return !job.paused && active && !job.bytesTotal && job.progress === 0
}

function progressClass(job: DeliveryJob) {
  if (["complete", "delivered"].includes(job.stage)) return "done"
  if (["failed", "cancelled", "monitoring-ended"].includes(job.stage)) return "fail"
  if (job.paused) return "paused"
  if (job.stage === "queued") return "idle"
  return barIndeterminate(job) ? "indeterminate" : ""
}

function statusOf(job: DeliveryJob): { cls: "done" | "attn" | "wait" | "act"; dot: string } {
  if (["complete", "delivered"].includes(job.stage)) return { cls: "done", dot: "good" }
  if (["failed", "cancelled", "monitoring-ended"].includes(job.stage)) return { cls: "attn", dot: "fail" }
  if (job.paused || job.stage === "queued" || /^waiting/i.test(job.message || "")) return { cls: "wait", dot: "" }
  return { cls: "act", dot: "act" }
}

function failureText(job: DeliveryJob) {
  const failure = errorContext(job)
  const issue = (failure?.detail || "").toLowerCase()
  if (job.stage === "cancelled") return "Cancelled. Retained files are kept for a retry."
  if (/not enough|disk full/.test(issue)) return "Not enough free space to continue."
  if (/could not write|could not create/.test(issue)) return "The drive could not write the extracted files."
  if (/checksum|crc|damaged/.test(issue)) return "The archive failed its integrity check."
  if (/volume is missing|could not read|could not open/.test(issue)) return "An archive part is missing or unreadable."
  if (/password/.test(issue)) return "The archive password was not accepted."
  if (job.stage === "monitoring-ended") return jobTarget(job) === "ps4" ? "Check the install status on your PS4." : "Check the installation on your PS5."
  return `${failure?.phase && failure.phase !== "unknown" ? failure.phase[0].toUpperCase() + failure.phase.slice(1) : "The transfer"} stopped. Open Errors for the details.`
}

function jobLine(job: DeliveryJob, group: DownloadGroup, downloadDir: string): { text: string; cls: string } {
  const target = jobTarget(job).toUpperCase()
  if (job.paused) return { text: "Paused. Retained files stay where they are.", cls: "muted" }
  if (["failed", "cancelled", "monitoring-ended"].includes(job.stage)) return { text: `${failureText(job)} ${errorContext(job)?.retryHint || ""}`.trim(), cls: "attn" }
  if (job.stage === "complete") return { text: job.packageOnly ? job.packaging?.outputBytes ? `${packSummary(job.packaging)}.` : `The package is saved in ${plainPath(job.packaging?.outputPath || downloadDir)}.` : job.packaging?.format === "exfat" ? `The ${group.title} image is on your ${target}. ShadowMount Plus mounts it on its next scan.` : `${group.title} is installed on your ${target}.`, cls: "done" }
  if (job.stage === "delivered") return { text: `Delivered to SSPI on the ${target}. The PC can't confirm this install.`, cls: "done" }
  if (consoleOwned(job)) return { text: jobTarget(job) === "ps4" ? "Installation is managed on the PS4. Follow it in the PS4's download queue." : "Installation is managed by the PS5. Follow it on the console.", cls: "" }
  if (job.stage === "queued") return { text: job.message ? displayText(job.message) : "Queued. Starts when a download slot is free.", cls: "muted" }
  if (job.stage === "packaging") return { text: job.packaging ? packHeadline(job.packaging) : phaseDetail(job).label, cls: "" }
  return { text: displayText(job.message || phaseDetail(job).label), cls: /^waiting/i.test(job.message || "") ? "muted" : "" }
}

export function DownloadsPage(props: Props) {
  const { jobs, demo, settings, systemDrive, filter, setFilter, open, setOpen, drawer, setDrawer, statsForNerds, setStatsForNerds, ensureConsole, onOptions, onSearch, tintOn, cardStyle, cardSize } = props
  const groups = useMemo(() => groupDownloads(jobs), [jobs])
  const trackUpload = useUploadTrackers()
  const [current, setCurrent] = useState<Record<string, string>>({})
  const [clearing, setClearing] = useState(false)
  const [clearBusy, setClearBusy] = useState(false)
  const [clearError, setClearError] = useState("")
  const [local, setLocal] = useState<LocalPackage[]>([])
  const [picked, setPicked] = useState<string[]>([])
  const [manual, setManual] = useState<ManualRow[]>([])
  const [manualBusy, setManualBusy] = useState(false)
  const manualBusyRef = useRef(false)
  const [localBusy, setLocalBusy] = useState(false)
  const localBusyRef = useRef(false)
  const [importOpen, setImportOpen] = useState(false)
  const [linksOpen, setLinksOpen] = useState(false)
  const [cloudOpen, setCloudOpen] = useState(false)
  const ftabsRef = useRef<HTMLDivElement>(null)
  const inkRef = useRef<HTMLSpanElement>(null)
  const listRef = useRef<HTMLDivElement>(null)

  const counts = {
    All: groups.length,
    Active: groups.filter(g => g.jobs.some(activeTransfer)).length,
    Attention: groups.filter(g => g.jobs.some(attention)).length,
  }
  const shown = groups.filter(g => filter === "All" || (filter === "Active" ? g.jobs.some(activeTransfer) : g.jobs.some(attention)))
  const fallbackOpen = shown.find(g => g.jobs.some(job => activeTransfer(job) && job.stage !== "queued")) || shown.find(g => g.jobs.some(attention)) || shown[0]
  const openKey = open === undefined ? fallbackOpen?.key || null : open
  const openGroup = shown.find(g => g.key === openKey) || null
  const inactive = jobs.filter(job => !activeTransfer(job))
  const locations = settings.downloadDir ? stagingLocations(settings.downloadDir) : null
  const storageWarning = locations ? systemDriveWarning({ download: locations.downloadDir, extraction: locations.extractionDir, staging: locations.stagingDir }, systemDrive) : null
  const currentJob = (group: DownloadGroup) => group.jobs.find(job => job.jobId === current[group.key]) || group.primary

  useLayoutEffect(() => {
    glide(inkRef.current, ftabsRef.current?.querySelector<HTMLElement>(`.ftab[data-filter="${filter}"]`) || null, ftabsRef.current, { liquid: true })
  }, [filter])
  useEffect(() => {
    if (!openGroup || demo) return
    for (const job of openGroup.jobs.filter(job => job.components?.some(component => component.bytesTotal == null))) {
      void invoke("refresh_job_details", { jobId: job.jobId }).catch(() => undefined)
    }
  }, [openGroup?.key, demo])
  useEffect(() => {
    const stage = getStage()
    if (!stage) return
    if (!tintOn || !openGroup) { stage.look.setTint(null); return }
    let live = true
    void coverTint({ cover: openGroup.icon, title: openGroup.title, titleId: openGroup.titleId }).then(hex => { if (live) stage.look.setTint(hex) })
    return () => { live = false }
  }, [tintOn, openGroup?.key])
  useEffect(() => () => getStage()?.look.setTint(null), [])

  const toggle = (key: string) => {
    const next = openKey === key ? null : key
    setOpen(next)
    if (next) window.setTimeout(() => listRef.current?.querySelector<HTMLElement>(`.dcard[data-group="${CSS.escape(next)}"]`)?.scrollIntoView({ behavior: "smooth", block: "nearest" }), 80)
  }

  /* ---------------------------------------------------------------- job controls */
  const onPause = async (jobId: string, paused: boolean) => { await invoke("pause_job", { jobId, paused }) }
  const onCancel = async (jobId: string) => {
    const accepted = await invoke<boolean>("cancel_job", { jobId })
    if (!accepted) toast({ tone: "info", title: "Already stopped", text: "This transfer had already stopped." })
  }
  const onRetry = async (jobId: string) => {
    await invoke<string>("retry_job", { jobId })
    toast({ tone: "info", title: "Retry started", text: "Retained files are reused where they were already downloaded or extracted.", duration: 3600 })
  }

  /* ---------------------------------------------------------------- tools */
  const mergeManual = (found: ManualRow[]) => setManual(old => {
    const seen = new Set(old.map(item => item.path.toLowerCase()))
    const next = [...old]
    for (const item of found) {
      const key = item.path.toLowerCase()
      if (seen.has(key)) continue
      seen.add(key)
      next.push(item)
    }
    return next
  })
  const choose = async (directory: boolean) => {
    try {
      const value = await openDialog({ multiple: true, directory, title: directory ? "Choose package or game folders" : "Choose PKG, ZIP or RAR files" })
      const paths = (Array.isArray(value) ? value : value ? [value] : []).filter((path): path is string => typeof path === "string")
      if (!paths.length) return
      const results = await Promise.all(paths.map(async path => {
        try { return { path, items: await invoke<LocalPackage[]>("scan_local_packages", { path }) } }
        catch (error) { return { path, error } }
      }))
      // A game dump holds no PKG files, so folder scans also look for dumps.
      // Folders without one are expected here, so their scan errors are ignored.
      const dumps: ManualRow[] = directory ? (await Promise.all(paths.map(path => invoke<ManualCandidate[]>("scan_manual_folder", { path }).catch(() => []))))
        .flat().filter(item => item.content === "dump").map(item => ({ ...item, kind: item.detectedKind })) : []
      const found = results.flatMap(result => result.items ?? [])
      for (const result of results) if ("error" in result) {
        toast({ tone: "error", title: directory ? "A folder couldn't be read" : "A file couldn't be scanned", text: `${result.path}: ${errorText(result.error)}` })
      }
      if (found.length) {
        setLocal(old => {
          const merged = new Map<string, LocalPackage>()
          for (const item of old) merged.set(item.path.toLowerCase(), item)
          for (const item of found) if (!merged.has(item.path.toLowerCase())) merged.set(item.path.toLowerCase(), item)
          return [...merged.values()].map((item, index) => ({ ...item, number: index + 1 }))
        })
        setPicked(old => [...new Set([...old, ...found.map(item => item.path)])])
      }
      if (dumps.length) mergeManual(dumps)
      if (dumps.length && !found.length && await runFolderAction(dumps)) return
      if (found.length || dumps.length) setImportOpen(true)
      else if (!results.some(result => "error" in result)) {
        toast({ tone: "info", title: "Nothing found", text: `No PKG files or game folders were found in ${paths.length === 1 ? paths[0] : `${paths.length} selected paths`}.` })
      }
    } catch (error) { toast({ tone: "error", title: "The scan didn't finish", text: errorText(error) }) }
  }
  const addFolders = async () => {
    if (manualBusyRef.current) return
    manualBusyRef.current = true
    setManualBusy(true)
    try {
      const value = await openDialog({ multiple: true, directory: true, title: "Choose game folders" })
      const dirs = (Array.isArray(value) ? value : value ? [value] : []).filter((v): v is string => typeof v === "string")
      if (!dirs.length) return
      const found: ManualRow[] = []
      for (const dir of dirs) {
        try {
          const scanned = await invoke<ManualCandidate[]>("scan_manual_folder", { path: dir })
          for (const item of scanned) found.push({ ...item, kind: item.detectedKind })
        } catch (error) { toast({ tone: "error", title: "A folder couldn't be read", text: errorText(error) }) }
      }
      mergeManual(found)
      manualBusyRef.current = false
      if (found.length && await runFolderAction(found)) return
      if (found.length) setImportOpen(true)
    } catch (error) { toast({ tone: "error", title: "The folders couldn't be added", text: errorText(error) }) }
    finally { manualBusyRef.current = false; setManualBusy(false) }
  }
  /** Options → Packaging → "When game folders are added" runs one batch action without asking. */
  const runFolderAction = async (rows: ManualRow[]) => {
    const action = settings.folderAction || "ask"
    if (action === "ask" || !rows.every(row => row.content === "dump" && row.kind !== "backport" && row.titleId?.trim())) return false
    await installManual(action === "package", action === "package-send" ? true : action === "send" ? false : undefined, rows)
    return true
  }
  const target = settings.activeConsole
  const localBlock = picked.map(path => local.find(item => item.path === path)).filter((item): item is LocalPackage => !!item)
    .map(item => sendBlockReason(target, { titleId: item.titleId, backport: item.packageKind === "backport" })).find(Boolean)
  // Game folders, PPSA titles and backports only install on a PS5, so they go there whichever console is active.
  const manualTarget = (row: ManualRow): ConsoleKind => row.content === "dump" || /^PPSA/i.test(row.titleId?.trim() || "") || row.kind === "backport" ? "ps5" : target
  const manualBlock = manual.map(item => sendBlockReason(manualTarget(item), { titleId: item.titleId, content: item.content === "dump" ? "dump" : item.content === "archive" ? "archive" : "pkg", backport: item.kind === "backport" })).find(Boolean)
  const manualTargets = [...new Set(manual.map(manualTarget))]
  const manualNote = target !== "ps5" && manualTargets.includes("ps5") ? "Game folders and PS5 titles go to your PS5." : undefined
  const manualInstallLabel = manual.length && manual.every(item => item.content === "dump" && item.kind !== "backport")
    ? settings.packageDumps ? settings.packageFormat === "exfat" ? "Build image and send" : "Package and install" : "Send to PS5"
    : "Install selected"
  const installLocal = async () => {
    if (localBusyRef.current) return
    localBusyRef.current = true
    setLocalBusy(true)
    try {
    if (localBlock) { toast({ tone: "warning", title: "This can't go to a PS4", text: localBlock }); return }
    if (!await ensureConsole(target)) return
    const byTitle = new Map<string, Array<{ item: LocalPackage; index: number }>>()
    picked.forEach((path, index) => {
      const item = local.find(candidate => candidate.path === path)
      if (!item) return
      const key = item.titleId?.toUpperCase() || `path:${index}`
      byTitle.set(key, [...(byTitle.get(key) || []), { item, index }])
    })
    const kindOrder: Record<string, number> = { base: 0, update: 1, dlc: 2, backport: 3 }
    const ordered = [...byTitle.values()].flatMap(items => items.sort((a, b) =>
      (kindOrder[a.item.packageKind || ""] ?? 4) - (kindOrder[b.item.packageKind || ""] ?? 4) || a.index - b.index,
    ))
    const queued = new Set<string>()
    const failed: string[] = []
    for (const { item } of ordered) {
      try {
        await invoke<string>("start_local_install", { path: item.path, target })
        queued.add(item.path)
      } catch (error) {
        failed.push(item.path)
        toast({ tone: "error", title: `${item.name} didn't start`, text: errorText(error) })
      }
    }
    if (queued.size) toast({ tone: "success", title: `${plural(queued.size, "local package")} queued`, text: `Sending to your ${target.toUpperCase()}. Imported originals are unchanged.` })
    setLocal(old => old.filter(item => !queued.has(item.path)).map((item, index) => ({ ...item, number: index + 1 })))
    setPicked(failed)
    if (!failed.length && !manual.length) setImportOpen(false)
    } finally { localBusyRef.current = false; setLocalBusy(false) }
  }
  const installManual = async (packageOnly: boolean, pack?: boolean, rows: ManualRow[] = manual) => {
    if (manualBusyRef.current) return
    manualBusyRef.current = true
    setManualBusy(true)
    const manual = rows
    try {
    if (!packageOnly && manualBlock) { toast({ tone: "warning", title: "This can't go to a PS4", text: manualBlock }); return }
    if (!packageOnly) for (const kind of manualTargets) if (!await ensureConsole(kind)) return
    const missing = manual.find(item => !item.titleId?.trim() && item.content === "dump")
    if (missing) { toast({ tone: "warning", title: "A title ID is needed", text: `${missing.name} needs a CUSA or PPSA title ID.` }); return }
    const kindOrder: Record<string, number> = { base: 0, update: 1, dlc: 2, backport: 3 }
    const ordered = manual.map((row, index) => ({ row, index })).sort((a, b) => {
      const titleOrder = (a.row.titleId || a.row.path).localeCompare(b.row.titleId || b.row.path)
      return titleOrder || (kindOrder[a.row.kind] ?? 4) - (kindOrder[b.row.kind] ?? 4) || a.index - b.index
    })
    const succeeded = new Set<string>()
    const failed: ManualRow[] = []
    for (const { row } of ordered) {
      const item: ManualItem = { path: row.path, kind: row.kind, titleId: row.titleId?.trim() ? row.titleId.trim().toUpperCase() : undefined }
      try {
        await invoke<string>("start_manual_install", { items: [item], packageOnly, ...(!packageOnly ? { target: manualTarget(row) } : {}), ...(pack != null ? { package: pack } : {}) })
        succeeded.add(row.path)
      } catch (error) {
        failed.push(row)
        toast({ tone: "error", title: `${row.name} didn't start`, text: errorText(error) })
      }
    }
    if (succeeded.size) toast({ tone: "success", title: packageOnly ? `Packaging ${plural(succeeded.size, "folder")}` : `${plural(succeeded.size, "item")} queued`, text: packageOnly ? "Title, version and artwork are read from each dump. Packages stay on this PC." : pack === false ? "Each folder is uploaded as it is." : `Sending to your ${[...new Set(manual.map(manualTarget))].map(kind => kind.toUpperCase()).join(" and ")}.` })
    setManual(old => old.filter(row => !succeeded.has(row.path)))
    if (!failed.length && !local.length) setImportOpen(false)
    } finally { manualBusyRef.current = false; setManualBusy(false) }
  }
  const clearInactive = async (deleteFiles: boolean) => {
    setClearBusy(true); setClearError("")
    try {
      for (const job of inactive) await invoke("remove_job", { jobId: job.jobId, deleteFiles })
      setClearing(false)
      toast({ tone: "success", title: `${plural(inactive.length, "transfer")} removed`, text: deleteFiles ? "Leftover files were deleted. Imported originals were kept." : "Files were kept. Imported originals are never removed." })
    } catch (error) { setClearError(errorText(error)) }
    finally { setClearBusy(false) }
  }

  /* ---------------------------------------------------------------- keyboard and dock */
  const primary = openGroup ? currentJob(openGroup) : null
  const actions = useJobActions({ job: primary || ({ jobId: "", stage: "complete", progress: 0, speedBps: 0, message: "" } as DeliveryJob), demo, onCancel, onPause, onRetry })
  const [cancelArmed, setCancelArmed] = useState(false)
  useEffect(() => { setCancelArmed(false) }, [primary?.jobId, primary?.stage])
  useEffect(() => { if (cancelArmed) { const t = window.setTimeout(() => setCancelArmed(false), 3000); return () => window.clearTimeout(t) } }, [cancelArmed])
  const cancel = () => { if (!primary) return; if (!cancelArmed) { setCancelArmed(true); return } setCancelArmed(false); void actions.run("cancel") }

  const live = useRef({ shown, openKey, primary, actions, cancel, toggle, setOpen, onSearch, setDrawer, setStatsForNerds, statsForNerds, demo })
  live.current = { shown, openKey, primary, actions, cancel, toggle, setOpen, onSearch, setDrawer, setStatsForNerds, statsForNerds, demo }
  useEffect(() => setPageKeys(event => {
    const { shown, openKey, primary, actions, cancel, toggle, setOpen, onSearch, setDrawer, setStatsForNerds, statsForNerds, demo } = live.current
    const k = event.key
    const heads = [...(listRef.current?.querySelectorAll<HTMLElement>(".dhead") || [])]
    const cur = (document.activeElement as HTMLElement | null)?.closest?.(".dhead") as HTMLElement | null
    const i = cur ? heads.indexOf(cur) : -1
    if (k === "ArrowDown" || k === "ArrowUp") { event.preventDefault(); (heads[clamp(i + (k === "ArrowDown" ? 1 : -1), 0, heads.length - 1)] || heads[0])?.focus(); return }
    if (k === "Enter" && !cur && !(document.activeElement as HTMLElement | null)?.closest?.("button")) { event.preventDefault(); if (shown[0]) toggle(openKey && shown.some(g => g.key === openKey) ? openKey : shown[0].key); return }
    if (k === "Escape") { event.preventDefault(); if (openKey) setOpen(null); else onSearch(); return }
    if (k === "n" || k === "N") { event.preventDefault(); setStatsForNerds(!statsForNerds); return }
    if (!primary || demo) return
    if (k === "p" || k === "P") { if (actions.controls.pause) { event.preventDefault(); void actions.run("pause") } return }
    if (k === "r" || k === "R") { if (actions.controls.retry) { event.preventDefault(); void actions.run("retry") } return }
    if (k === "Delete") { event.preventDefault(); if (actions.controls.cancel) cancel(); else if (actions.controls.remove) actions.setRemoving(true); return }
    if (k === "f" || k === "F") { event.preventDefault(); setDrawer("files") }
  }), [])

  /* ---------------------------------------------------------------- render */
  return (
    <div className="page downloads-page enter">
      <div className="dl-head">
        <div className="ftabs" ref={ftabsRef} role="tablist" aria-label="Filter downloads">
          {FILTERS.map(value => (
            <button key={value} type="button" role="tab" data-filter={value} className={`ftab ${value === "Attention" && counts.Attention ? "attn" : ""}`} aria-selected={filter === value} onClick={() => setFilter(value)}>
              {value}<b>{counts[value]}</b>
            </button>
          ))}
          <span className="ftab-ink" ref={inkRef} />
        </div>
        {!demo && (
          <div className="dl-tools">
            <ActionBubbleMenu label="Add to downloads" actions={[
              { id: "file", label: "Import file", icon: "box", description: "Import a PKG, ZIP or RAR", onSelect: () => choose(false) },
              { id: "scan", label: "Scan folder", icon: "library", description: "Scan a folder for packages", onSelect: () => choose(true) },
              { id: "folders", label: "Add game folders", icon: "folder", description: "Package game folders", busy: manualBusy, onSelect: addFolders },
              { id: "links", label: "Paste links", icon: "link", description: "Paste hoster or direct download links", onSelect: () => setLinksOpen(true) },
              { id: "cloud", label: "Debrid files", icon: "globe", description: "Browse files stored in your debrid accounts", onSelect: () => setCloudOpen(true) },
            ]} />
            {(local.length > 0 || manual.length > 0) && !importOpen && <button type="button" className="btn sm" onClick={() => setImportOpen(true)}><Icon name="files" />{plural(local.length + manual.length, "item")} ready</button>}
            {inactive.length > 0 && <button type="button" className="btn ghost sm" onClick={() => setClearing(value => !value)}><Icon name="trash" />Clear finished ({inactive.length})</button>}
          </div>
        )}
      </div>
      {clearing && (
        <div className="dl-confirm" role="group" aria-label="Clear finished transfers">
          <span>Remove {plural(inactive.length, "finished, failed or cancelled transfer")}. Active transfers and imported originals are kept.</span>
          <button type="button" className="btn sm" disabled={clearBusy} onClick={() => void clearInactive(false)}>Keep files</button>
          <button type="button" className="btn sm danger" disabled={clearBusy} onClick={() => void clearInactive(true)}>{clearBusy ? "Cleaning…" : "Delete leftover files"}</button>
          <button type="button" className="btn ghost sm" disabled={clearBusy} onClick={() => setClearing(false)}>Back</button>
          {clearError && <p className="error" role="alert">{clearError}</p>}
        </div>
      )}
      {storageWarning && (
        <div className="dl-banner" role="alert">
          <span>{storageWarning}</span>
          <button type="button" className="btn sm" onClick={() => onOptions("downloads")}>Download folder</button>
        </div>
      )}
      <div className="dlist scroll" ref={listRef} data-case-scroll="">
        {shown.map((group, n) => (
          <Card
            key={group.key} group={group} index={n} open={group.key === openKey} onToggle={() => toggle(group.key)}
            job={currentJob(group)} setCurrent={jobId => setCurrent(old => ({ ...old, [group.key]: jobId }))}
            drawer={drawer} setDrawer={setDrawer} statsForNerds={statsForNerds} setStatsForNerds={setStatsForNerds}
            trackUpload={trackUpload} settings={settings} demo={demo} cardStyle={cardStyle} cardSize={cardSize}
            actions={group.key === openKey ? actions : null} cancelArmed={cancelArmed} onCancel={cancel}
            onRetryJob={async jobId => { try { await onRetry(jobId) } catch (error) { toast({ tone: "error", title: "Retry didn't start", text: errorText(error) }) } }}
          />
        ))}
        {!shown.length && (
          <div className="empty-state">
            <Icon name="download" />
            <h3>{jobs.length ? "Nothing in this view" : "No downloads yet"}</h3>
            <p>{filter === "Attention" ? "Failed or stopped transfers appear here." : filter === "Active" ? "No transfers are running. Pick a title in Search to install it." : "Your downloads appear here. Search for a title, or import a PKG or game folder from this PC."}</p>
            {!jobs.length && <div className="row"><button type="button" className="btn sm" onClick={onSearch}><Icon name="search" />Search titles</button></div>}
          </div>
        )}
      </div>
      <PasteLinksDialog open={linksOpen} onClose={() => setLinksOpen(false)} />
      <CloudFilesDialog open={cloudOpen} onClose={() => setCloudOpen(false)} settings={settings} />
      <ImportDialog
        open={importOpen} onClose={() => setImportOpen(false)} target={target}
        local={local} picked={picked} setPicked={setPicked} onInstallLocal={() => void installLocal()} localBusy={localBusy} localBlock={localBlock}
        manual={manual} setManual={setManual} onInstallManual={(packageOnly, pack) => void installManual(packageOnly, pack)} manualBusy={manualBusy} manualBlock={manualBlock}
        manualNote={manualNote} manualInstallLabel={manualInstallLabel}
        onClear={() => { setLocal([]); setPicked([]); setManual([]); setImportOpen(false) }}
      />
    </div>
  )
}

/* ---------------------------------------------------------------- one game card */
function Card({ group, index, open, onToggle, job, setCurrent, drawer, setDrawer, statsForNerds, setStatsForNerds, trackUpload, settings, demo, cardStyle, cardSize, actions, cancelArmed, onCancel, onRetryJob }: {
  group: DownloadGroup; index: number; open: boolean; onToggle: () => void; job: DeliveryJob; setCurrent: (jobId: string) => void
  drawer: DrawerTab; setDrawer: (tab: DrawerTab) => void; statsForNerds: boolean; setStatsForNerds: (value: boolean) => void
  trackUpload: (job: DeliveryJob) => UploadState | null; settings: Settings; demo: boolean; cardStyle: CardStyle; cardSize: CardSize
  actions: ReturnType<typeof useJobActions> | null; cancelArmed: boolean; onCancel: () => void; onRetryJob: (jobId: string) => Promise<void>
}) {
  const cardRef = useRef<HTMLElement>(null)
  const primary = group.primary
  const headActions = useJobActions({ job: primary, demo, onPause: async (id, paused) => { await invoke("pause_job", { jobId: id, paused }) }, onCancel: async id => { await invoke("cancel_job", { jobId: id }) }, onRetry: async id => { await invoke("retry_job", { jobId: id }) } })
  const upload = trackUpload(job)
  const stats = transferStats(job, upload)
  const status = statusOf(job)
  const line = jobLine(job, group, settings.downloadDir)
  const finished = ["complete", "delivered"].includes(job.stage)
  const pct = finished ? 100 : barPercent(job, upload)
  const cover = { cover: group.icon, title: group.title, titleId: group.titleId }
  const hasAttention = group.jobs.some(attention)
  const target = jobTarget(job)
  const moving = activeTransfer(job) && !job.paused
  // One status line: measured bytes, speed and time left while bytes move; otherwise what's happening.
  const summary = moving && stats.speedBps != null && job.stage !== "packaging"
    ? [stats.label.replace(" / ", " of "), fmtSpeed(stats.speedBps), stats.etaSeconds ? fmtEta(stats.etaSeconds) : ""].filter(Boolean).join(", ")
    : line.text
  const showBar = !finished && !["failed", "cancelled", "monitoring-ended"].includes(job.stage)
  const showPct = showBar && !barIndeterminate(job) && job.stage !== "queued"
  const engine = packEngine(job.packaging)

  useLayoutEffect(() => {
    const stage = getStage(), el = cardRef.current
    if (!stage || !el) return
    stage.surfaces.register(el, cover)
    return () => stage.surfaces.unregister(el)
  }, [group.icon, group.title])

  return (
    <article
      ref={cardRef} className={`dcard ${open ? "is-open" : ""} ${hasAttention ? "attn" : ""} ${cardStyle} ${cardSize} enter`}
      style={{ animationDelay: `${Math.min(index, 10) * 30}ms`, ["--art" as string]: cardStyle === "art" && group.icon?.startsWith("data:image/") && !group.icon.startsWith("data:image/svg") ? `url("${group.icon}")` : undefined }}
      data-group={group.key} data-art-level={open ? 1 : 0} data-banner={cardStyle === "art" ? 1 : 0}
    >
      <button type="button" className="dhead" aria-expanded={open} aria-label={`${group.title}, ${stageLabel(job)}${showPct ? `, ${pct}%` : ""}`} data-hover-case onClick={onToggle} onContextMenu={event => void headActions.contextMenu(event)} onKeyDown={headActions.onKeyDown}>
        <CaseAnchor spec={{ key: (group.titleId || group.key).toUpperCase(), cover: group.icon, title: group.title, titleId: group.titleId, kind: "download" }} className="dcase" />
        <span className="dsum">
          <strong className="dtitle">{group.title}</strong>
          <span className="dmeta">
            <span className={`dstatus ${status.cls}`}><span className={`dot ${status.dot}`} />{stageLabel(job)}</span>
            <span className="console-badge">{target.toUpperCase()}</span>
            {group.jobs.length > 1 && <span className="dcount">{plural(group.jobs.length, "package")}</span>}
          </span>
          <span className={`dline ${moving ? "" : line.cls}`}>{summary}</span>
        </span>
        <span className="dside">
          {showPct && <span className="dpct">{pct}<small>%</small>{engine && job.stage === "packaging" && <em className="dstage">stage {engine.stageIndex}/{engine.stageCount}</em>}</span>}
          <Icon name="chevD" className="dchev" />
        </span>
        {showBar && (engine && job.stage === "packaging"
          ? <StageBar engine={engine} paused={!!job.paused} />
          : <span className={`dbar progress ${progressClass(job)}`} aria-hidden="true"><span className="fill" style={{ width: `${pct}%` }} /></span>)}
      </button>
      <Collapse open={open} className="ddrawer">
        <Drawer group={group} job={job} setCurrent={setCurrent} drawer={drawer} setDrawer={setDrawer} statsForNerds={statsForNerds} setStatsForNerds={setStatsForNerds} trackUpload={trackUpload} settings={settings} demo={demo} actions={actions || headActions} cancelArmed={!!actions && cancelArmed} onCancel={actions ? onCancel : () => void headActions.run("cancel")} onRetryJob={onRetryJob} />
      </Collapse>
    </article>
  )
}

/* ---------------------------------------------------------------- the drawer inside the open card */
function Drawer({ group, job, setCurrent, drawer, setDrawer, statsForNerds, setStatsForNerds, trackUpload, settings, demo, actions, cancelArmed, onCancel, onRetryJob }: {
  group: DownloadGroup; job: DeliveryJob; setCurrent: (jobId: string) => void; drawer: DrawerTab; setDrawer: (tab: DrawerTab) => void
  statsForNerds: boolean; setStatsForNerds: (value: boolean) => void; trackUpload: (job: DeliveryJob) => UploadState | null
  settings: Settings; demo: boolean; actions: ReturnType<typeof useJobActions>; cancelArmed: boolean; onCancel: () => void; onRetryJob: (jobId: string) => Promise<void>
}) {
  const tabsRef = useRef<HTMLDivElement>(null)
  const inkRef = useRef<HTMLSpanElement>(null)
  const showPackaging = !!(settings.packageDumps || job.packaging || job.stageHistory?.includes("packaging"))
  const phases = transferPhases.map((name, index) => ({ name, index })).filter(({ name, index }) => !(name === "Package" && !showPackaging) && !(job.packageOnly && index > 2))
  const errors = group.jobs.filter(item => errorContext(item) || (item.space && !item.space.enough))
  const fileCount = (job.components || []).reduce((sum, component) => sum + component.parts.length, 0) + (job.packaging?.outputPath ? 1 : 0)
  const counts = { packages: group.jobs.length, files: fileCount, errors: errors.length }
  const detail = phaseDetail(job)
  const pct = barPercent(job, trackUpload(job))
  const engine = job.stage === "packaging" ? packEngine(job.packaging) : null
  const showBuild = !!job.packaging && (job.stage === "packaging" || !!packEngine(job.packaging) || job.packaging.outputBytes > 0)

  useLayoutEffect(() => {
    glide(inkRef.current, tabsRef.current?.querySelector<HTMLElement>(`.dtab[data-drawer="${drawer}"]`) || null, tabsRef.current, { liquid: true })
  }, [drawer])

  return (
    <div className="ddrawer-inner">
      <ol className="phases" style={{ gridTemplateColumns: `repeat(${phases.length}, minmax(0, 1fr))` }} aria-label="Stages">
        {phases.map(({ name, index }, n) => {
          const state = phaseState(job, index)
          return (
            <li key={name} className="phase" data-state={state} aria-current={state === "current" ? "step" : undefined}>
              <span className="link-fill" />
              <span className="node">
                {state === "done" ? <Icon name="check" /> : state === "failed" ? <Icon name="x" /> : state === "skipped" ? "–" : n + 1}
              </span>
              <span>{name}</span>
              <small>{state === "skipped" ? job.localPkg && index === 0 ? "Local file" : "Not needed" : state === "done" ? "Done" : state === "current" ? (job.paused ? "Paused" : name === "Package" && engine ? `Stage ${engine.stageIndex} of ${engine.stageCount}` : detail.indeterminate ? detail.label : "In progress") : state === "failed" ? "Stopped" : ""}</small>
            </li>
          )
        })}
      </ol>
      {showBuild && <PackagingPanel job={job} demo={demo} />}
      <div className="dtabs" role="tablist" ref={tabsRef}>
        {(["packages", "files", "errors"] as const).map(id => (
          <button key={id} type="button" role="tab" className="dtab" data-drawer={id} aria-selected={drawer === id} onClick={() => setDrawer(id)}>{id[0].toUpperCase() + id.slice(1)}<b>{counts[id]}</b></button>
        ))}
        <span className="dtab-ink" ref={inkRef} />
        <span className="nerds">Stats for nerds<button type="button" className="switch" role="switch" aria-checked={statsForNerds} aria-label="Stats for nerds" onClick={() => setStatsForNerds(!statsForNerds)} /></span>
      </div>
      <div className="dpanel swap-fade" key={drawer}>
        {drawer === "packages" && (
          <div className="drows">
            {group.jobs.map(item => {
              const s = transferStats(item, trackUpload(item))
              const st = statusOf(item)
              return (
                <button key={item.jobId} type="button" className={`drow ${item.jobId === job.jobId ? "is-current" : ""}`} onClick={() => setCurrent(item.jobId)} aria-pressed={item.jobId === job.jobId}>
                  <span><strong>{kindLabel(kindOf(item))}{item.packageVersion && <small>v{item.packageVersion}</small>}</strong><span className="sub">{item.message && activeTransfer(item) ? displayText(item.message) : s.label}</span></span>
                  <span className={`dstatus ${st.cls}`}><span className={`dot ${st.dot}`} />{stageLabel(item)}</span>
                </button>
              )
            })}
          </div>
        )}
        {drawer === "files" && <FilesPanel job={job} settings={settings} />}
        {drawer === "errors" && (
          errors.length ? errors.map(item => {
            const failure = errorContext(item)
            return (
              <div key={item.jobId} className="errbox">
                <Icon name="alert" />
                <div>
                  <strong>{kindLabel(kindOf(item))}{item.packageVersion ? ` ${item.packageVersion}` : ""}: {failure ? failureText(item) : "Not enough disk space"}</strong>
                  <p>{failure ? `${displayText(failure.detail)} ${failure.retryHint}` : item.space?.message}</p>
                </div>
                {item.retryable && ["failed", "cancelled", "monitoring-ended"].includes(item.stage) && !demo && <button type="button" className="btn sm" onClick={() => void onRetryJob(item.jobId)}><Icon name="retry" />Retry</button>}
              </div>
            )
          }) : <p className="noerr"><Icon name="checkCircle" />No errors reported for this game.</p>
        )}
      </div>
      {statsForNerds && <NerdPanels job={job} settings={settings} />}
      {!demo && (
        <div className="focus-actions">
          {actions.controls.pause && <button type="button" className="btn sm" disabled={!!actions.busy} onClick={() => void actions.run("pause")}><Icon name={job.paused ? "play" : "pause"} />{job.paused ? "Resume" : "Pause"}</button>}
          {actions.controls.cancel && <button type="button" className={`btn sm ${cancelArmed ? "confirm" : "ghost danger"}`} disabled={!!actions.busy} onClick={onCancel}><Icon name={cancelArmed ? "alert" : "x"} />{actions.busy === "cancel" ? "Cancelling…" : cancelArmed ? "Cancel transfer?" : "Cancel"}</button>}
          {actions.controls.retry && <button type="button" className="btn sm primary" disabled={!!actions.busy} onClick={() => void actions.run("retry")}><Icon name="retry" />{actions.busy === "retry" ? "Retrying…" : `Retry from ${pct}%`}</button>}
          {actions.controls.remove && !actions.removing && <button type="button" className="btn sm ghost" disabled={!!actions.busy} onClick={() => actions.setRemoving(true)}><Icon name="trash" />Remove from list</button>}
          {consoleOwned(job) && <span className="rail-note" style={{ margin: 0 }}>{jobTarget(job) === "ps4" ? "The PS4 owns this step, so it can't be paused from here." : "The PS5 owns this step, so it can't be paused from here."}</span>}
          {actions.removing && (
            <div className="remove-confirm">
              <span>Remove this transfer? Imported originals and files used by other transfers are kept.</span>
              <button type="button" className="btn sm" disabled={!!actions.busy} onClick={() => void actions.remove(false)}>Keep files</button>
              <button type="button" className="btn sm danger" disabled={!!actions.busy} onClick={() => void actions.remove(true)}>{actions.busy === "remove" ? "Cleaning…" : "Delete leftover files"}</button>
              <button type="button" className="btn sm ghost" disabled={!!actions.busy} onClick={() => actions.setRemoving(false)}>Back</button>
            </div>
          )}
          {actions.error && <p className="error" role="alert">{actions.error}</p>}
        </div>
      )}
    </div>
  )
}

/** One segment per engine stage, each filled only by that stage's measured progress. */
function StageBar({ engine, paused }: { engine: PackEngine; paused: boolean }) {
  return (
    <span className={`dbar segbar ${paused ? "paused" : ""}`} aria-hidden="true">
      {engine.stages.map(stage => {
        const fill = stage.state === "done" ? 1 : stage.state === "active" ? stage.progress ?? null : 0
        return <i key={stage.id} data-state={stage.state} data-indet={stage.state === "active" && fill == null ? "" : undefined}><b style={{ width: `${(fill ?? 0) * 100}%` }} /></i>
      })}
    </span>
  )
}

const STAGE_NAMES: Record<string, string> = { prepare: "Prepare", compress: "Compress", outer: "Outer PFS", metadata: "Metadata", finalize: "Finalize", verify: "Verify", lizard: "Lizard", layout: "Layout", write: "Write" }
const baseName = (path: string) => path.split(/[\\/]/).pop() || path

/** The FPKG or image build, compact: one strip of engine stages, one line of measured numbers, the result. */
function PackagingPanel({ job, demo, ensureConsole }: { job: DeliveryJob; demo: boolean; ensureConsole?: (target: ConsoleKind) => Promise<boolean> }) {
  const info = job.packaging!
  const engine = packEngine(info)
  const packing = job.stage === "packaging" && activeTransfer(job)
  const built = !!info.outputBytes
  const [revealError, setRevealError] = useState("")
  const level = engine?.level ?? info.compressionLevel ?? 0
  const temp = info.tempPath ? plainPath(info.tempPath).slice(0, 2) : ""
  const image = info.format === "exfat"
  const lizard = info.lizard
  const spec = (image
    ? ["exFAT", `${engine?.blockKiB ?? 64} KiB clusters`, engine?.stages.some(stage => stage.id === "lizard") || lizard ? `Lizard LZ4, ${engine?.workers ?? info.threads} workers` : ""]
    : [`Kraken ${level}`, `${engine?.workers ?? info.threads} workers`, engine?.pfs ? `PFS ${engine.pfs}` : `PFS v${info.pfsVersion}`, temp && /^[A-Za-z]:$/.test(temp) ? `temp ${temp.toUpperCase()}` : ""]).filter(Boolean).join(" · ")
  const elapsed = (info.workspaceSeconds || 0) + (engine?.elapsedSeconds ?? info.elapsedSeconds)
  const total = info.totalSeconds ?? (built ? info.elapsedSeconds : null)
  const active = engine ? activeStage(engine) : undefined
  const counted = ["compress", "lizard", "write"].includes(active?.id || "")
  // An image compares with the dump as downloaded; its engine input is the (possibly Lizard-packed) payload.
  const input = image ? info.inputBytes || engine?.inputBytes || 0 : engine?.inputBytes || info.inputBytes
  const reveal = async () => {
    setRevealError("")
    try { await invoke("reveal_path", { path: info.outputPath }) } catch (error) { setRevealError(errorText(error)) }
  }
  const [sending, setSending] = useState(false)
  // A finished "package only" build can still go to the PS5 from here.
  const canSend = !demo && built && job.packageOnly && ["complete", "failed", "cancelled"].includes(job.stage)
  const send = async () => {
    setRevealError(""); setSending(true)
    try {
      if (ensureConsole && !await ensureConsole("ps5")) return
      await invoke<string>("send_packaged", { jobId: job.jobId })
      toast({ tone: "success", title: "Sending to your PS5", text: image ? "The image goes to /data/homebrew; ShadowMount Plus mounts it on its next scan." : "The package is uploaded and installed. Follow it on this card." })
    } catch (error) { setRevealError(errorText(error)) }
    finally { setSending(false) }
  }
  // One line of numbers: live rates while working, the result once built.
  const line = packing && engine ? [
    engine.io.readBps != null && active?.id !== "metadata" ? `read ${fmtSpeed(engine.io.readBps)}` : "",
    engine.io.writeBps != null && active?.id !== "metadata" ? `write ${fmtSpeed(engine.io.writeBps)}` : "",
    counted && engine.files ? `${engine.files.done} of ${engine.files.total} files` : active?.detail || "",
    engine.etaSeconds != null && engine.etaSeconds >= 1 ? `${fmtDuration(engine.etaSeconds)} left in stage` : "",
    counted && engine.currentFile ? `${baseName(engine.currentFile)}${engine.currentFileProgress != null ? ` ${Math.round(engine.currentFileProgress * 100)}%` : ""}` : "",
  ] : [
    built ? `${fmtBytes(input)} → ${fmtBytes(info.outputBytes)} (${(100 * info.outputBytes / Math.max(1, input)).toFixed(1)}%)` : fmtBytes(input),
    lizard ? `Lizard packed ${lizard.filesPacked} of ${lizard.filesTotal} files, saved ${fmtBytes(Math.max(0, lizard.packedBytes - lizard.storedBytes))}` : "",
    engine ? `read ${fmtBytes(engine.io.readBytes)}` : "",
    engine ? `written ${fmtBytes(engine.io.writeBytes)}` : "",
    info.workspaceSeconds != null ? `workspace ${fmtDuration(info.workspaceSeconds)}` : "",
  ]
  return (
    <section className="pack" aria-label={image ? "ShadowMount image build" : "FPKG build"}>
      <header className="pack-head">
        <span className="pack-title"><strong>{image ? "ShadowMount image" : "FPKG build"}</strong><span className="pack-spec">{spec}</span></span>
        <span className="pack-clock">
          {total != null && !packing ? <><small>{image ? "Built in" : "Packaged in"}</small><b>{fmtDuration(total)}</b></> : <><small>{job.paused ? "Paused at" : "Elapsed"}</small><b>{fmtClock(elapsed)}</b></>}
        </span>
      </header>
      {engine ? (
        <ol className="pack-strip" style={{ gridTemplateColumns: `repeat(${engine.stages.length}, minmax(0, 1fr))` }}>
          {engine.stages.map(stage => {
            const fill = stage.state === "done" ? 1 : stage.state === "active" ? stage.progress ?? null : 0
            const name = stage.id === "metadata" && /artwork/i.test(stage.label) ? "Artwork" : STAGE_NAMES[stage.id] || stage.label
            return (
              <li key={stage.id} data-state={stage.state} title={[stage.label, stage.detail].filter(Boolean).join(": ")}>
                <span className="n">{stage.state === "done" && <Icon name="check" />}{name}</span>
                <span className="t">{stage.state === "active" && fill != null ? `${Math.floor(fill * 100)}%` : stage.state === "pending" ? "–" : fmtDuration(stage.seconds)}</span>
                <i className={stage.state === "active" && fill == null && !job.paused ? "indet" : ""}><b style={{ width: `${(fill ?? 0) * 100}%` }} /></i>
              </li>
            )
          })}
        </ol>
      ) : (
        <p className="rail-note" style={{ margin: "0 0 8px" }}>{image ? "Preparing the image workspace." : "Stage timings need packaging engine 1.1 or newer. This package was built by an earlier engine."}</p>
      )}
      <p className="pack-line">{line.filter(Boolean).join(" · ")}</p>
      {built && info.outputPath && (
        <div className="pack-out">
          <Icon name="box" />
          <code title={plainPath(info.outputPath)}>{plainPath(info.outputPath)}</code>
          {canSend && <button type="button" className="btn sm" disabled={sending} onClick={() => void send()}>{sending ? <span className="spinner" /> : <Icon name="download" />}Send to PS5</button>}
          {!demo && <button type="button" className="btn sm ghost" onClick={() => void reveal()}><Icon name="folder" />Show in folder</button>}
        </div>
      )}
      {revealError && <p className="error" role="alert">{revealError}</p>}
    </section>
  )
}

function FilesPanel({ job, settings }: { job: DeliveryJob; settings: Settings }) {
  const components = job.components || []
  return (
    <>
      <div className="files">
        {components.map((component, index) => (
          <div key={index}>
            {components.length > 1 && <p className="file-group">{kindLabel(component.kind)}{component.version ? ` ${component.version}` : ""}{component.bytesTotal != null ? `, ${fmtBytes(component.bytesTotal)}` : ""}</p>}
            {component.parts.map(part => (
              <div key={part.number} className="file">
                {part.downloaded ? <Icon name="checkCircle" className="ok" /> : <Icon name="archive" />}
                <span>{displayText(part.name)}</span>
                <small>{part.bytes != null ? fmtBytes(part.bytes) : "Size unknown"}</small>
                <small>{part.downloaded ? "Downloaded" : "Waiting"}</small>
              </div>
            ))}
          </div>
        ))}
        {job.packaging?.outputPath && (
          <div className="file">
            {job.stage === "complete" || job.packaging.outputBytes > 0 ? <Icon name="checkCircle" className="ok" /> : <Icon name="box" />}
            <span>{plainPath(job.packaging.outputPath).split(/[\\/]/).pop()}</span>
            <small>{job.packaging.outputBytes > 0 ? fmtBytes(job.packaging.outputBytes) : "Not built yet"}</small>
            <small>{job.stage === "packaging" ? "Building" : job.packaging.outputBytes > 0 ? "Packaged" : "Pending"}</small>
          </div>
        )}
        {!components.length && !job.packaging?.outputPath && <p className="noerr"><Icon name="info" />File details appear once the transfer has started.</p>}
      </div>
      {settings.downloadDir && <p className="path-line"><Icon name="folder" /><code>{plainPath(job.packaging?.outputPath || settings.downloadDir)}</code></p>}
      <details className="tech">
        <summary>Technical details</summary>
        <div className="tech-body">
          {job.packageLabel && <p>{displayText(job.packageLabel)}</p>}
          {job.message && <p>{displayText(job.message)}</p>}
          {job.space && (
            <div className="space-note">
              <strong>{job.space.enough ? "Disk space checked" : "Not enough disk space"}{job.space.estimated ? ", estimated" : ""}</strong>
              {job.space.message}<br />Additional space required: {fmtBytes(job.space.requiredBytes)}. Available at the last check: {job.space.freeBytes == null ? "unknown" : fmtBytes(job.space.freeBytes)}.
            </div>
          )}
          {job.packaging && (
            <>
              {job.packaging.doctor && <DoctorReportView report={job.packaging.doctor} applied={job.packaging.doctorApplied} />}
              <p>Kraken {job.packaging.compressionLevel || ({ fast: 2, standard: 4, smallest: 7 } as Record<string, number>)[job.packaging.preset] || 2}, {job.packaging.threads} workers, PFS v{job.packaging.pfsVersion}</p>
              <p>{job.packaging.fileCount.toLocaleString()} files, {fmtBytes(job.packaging.inputBytes)} input, {Math.round(job.packaging.elapsedSeconds)} s elapsed{job.packaging.outputBytes > 0 ? `, ${fmtBytes(job.packaging.outputBytes)} package` : ""}</p>
              <details><summary>Packaging log</summary><pre role="log" aria-live="off" aria-label="Recent packaging activity">{job.packaging.log.slice(-100).join("\n") || "Waiting for the packaging engine…"}</pre></details>
            </>
          )}
        </div>
      </details>
    </>
  )
}

function NerdPanels({ job, settings }: { job: DeliveryJob; settings: Settings }) {
  const active = activeTransfer(job) && !job.paused
  const [table, setTable] = useState(false)
  const samples = speedSamples(job.jobId)
  const now = speedNow()
  const recent = samples.filter(sample => now - sample.t < 60)
  const values = recent.map(sample => sample.v)
  const parts = (job.components || []).flatMap(component => component.parts)
  const nextPart = parts.find(part => !part.downloaded)
  const panels: ReactNode[] = []
  if (recent.length > 2) {
    panels.push(
      <div key="speed" className="nerd-panel">
        <div className="nerd-head"><strong>{job.stage === "packaging" ? "Engine I/O" : "Speed"}</strong><span>{job.stage === "packaging" ? "Reads while compressing, writes while building the outer PFS" : `${stageLabel(job)}, last 60 seconds`}</span></div>
        <SpeedChart jobId={job.jobId} active={active} stageLabel={stage => stageLabel({ ...job, stage, paused: false })} />
        <div className="tp-foot">
          <span>Now <b>{fmtSpeed(values[values.length - 1] || 0)}</b></span>
          <span>Peak <b>{fmtSpeed(Math.max(0, ...values))}</b></span>
          <span>Average <b>{fmtSpeed(values.reduce((a, b) => a + b, 0) / Math.max(1, values.length))}</b></span>
          <button type="button" className="link" onClick={() => setTable(value => !value)}>{table ? "Hide table" : "Show table"}</button>
        </div>
        {table && (
          <table className="tp-table">
            <thead><tr><th>Time</th><th>Speed</th><th>Stage</th></tr></thead>
            <tbody>{Array.from({ length: 12 }, (_, i) => i * 5).map(age => {
              const best = recent.reduce((pick, sample) => (Math.abs(now - age - sample.t) < Math.abs(now - age - pick.t) ? sample : pick), recent[0])
              return <tr key={age}><td>{age ? `${age} s ago` : "Now"}</td><td>{fmtSpeed(best.v)}</td><td>{stageLabel({ ...job, stage: best.stage, paused: false })}</td></tr>
            })}</tbody>
          </table>
        )}
      </div>,
    )
  } else if (active) {
    panels.push(<div key="speed" className="nerd-panel"><div className="nerd-head"><strong>Speed</strong><span>Collecting samples</span></div><p className="rail-note" style={{ margin: 0 }}>The graph appears after a few seconds of transfer.</p></div>)
  }
  if (job.stage === "packaging") {
    // Stages, bytes and rates live in the FPKG build panel above.
  } else if (job.stage === "uploading") {
    const target = jobTarget(job)
    const lanes = target === "ps5" ? ((settings.transferMode || "balanced") === "max" ? `Max, up to ${settings.uploadLanes || 4}` : "Balanced, 4") : "One stream"
    panels.push(
      <div key="upload" className="nerd-panel">
        <div className="nerd-head"><strong>Upload</strong><span>To {target.toUpperCase()} {consoleAddress(settings, target)}</span></div>
        <div className="nerd-table">
          <div><div className="k">Sent</div><div className="v">{fmtBytes(job.bytesDone || 0)}</div></div>
          <div><div className="k">Total</div><div className="v">{job.bytesTotal ? fmtBytes(job.bytesTotal) : "Unknown"}</div></div>
          <div><div className="k">Lanes</div><div className="v">{lanes}</div></div>
        </div>
      </div>,
    )
  } else if (parts.length) {
    panels.push(
      <div key="parts" className="nerd-panel">
        <div className="nerd-head"><strong>Archive parts</strong><span>{plural(parts.filter(part => part.downloaded).length, "part")} of {parts.length} downloaded</span></div>
        <div className="pieces">{parts.map(part => (
          <div key={`${part.number}-${part.name}`} className={`piece ${part.downloaded ? "done" : ""} ${job.stage === "downloading" && part === nextPart ? "cur" : ""}`} title={displayText(part.name)}>
            <i /><span>{part.number}</span>
          </div>
        ))}</div>
        <div className="nerd-legend"><span><i style={{ background: "var(--s1)" }} />Downloaded</span><span><i style={{ background: "transparent", boxShadow: "inset 0 0 0 1px rgba(255,255,255,.7)" }} />Downloading</span></div>
      </div>,
    )
  }
  if (!panels.length) return null
  return <div className="dnerd">{panels}</div>
}
