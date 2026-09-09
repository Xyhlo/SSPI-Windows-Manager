import { useEffect, useMemo, useRef, useState } from "react"
import { Check, ChevronRight, FileStack, Pause, Play, X } from "lucide-react"
import { CaseCover, useCoverSource } from "@/lib/covers"
import { activeTransfer, errorContext, groupDownloads, honestPercent, kindLabel, kindOf, phaseDetail, phaseState, stageLabel, stagingLocations, systemDriveWarning, transferPhases, transferSize, UploadTracker, type DownloadGroup, type UploadState } from "@/lib/downloads"
import { cn } from "@/lib/utils"
import type { DeliveryJob } from "@/types"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Progress } from "@/components/ui/progress"
import "./downloads.css"

type Props = { jobs: DeliveryJob[]; demo: boolean; onCancel: (jobId: string) => Promise<unknown>; onPause: (jobId: string, paused: boolean) => Promise<unknown>; downloadDir?: string; systemDrive?: string | null; onOpenSettings?: () => void }

function PackageTags({ group }: { group: DownloadGroup }) {
  return <span className="download-package-tags">
    {group.kinds.includes("base") && <span>Base package</span>}
    {group.kinds.includes("update") && <span className="download-tag update">+UP</span>}
    {group.kinds.includes("backport") && <span className="download-tag backport">+BP</span>}
    {group.kinds.includes("dlc") && <span className="download-tag dlc">+DLC+</span>}
    {!group.kinds.some(k => ["base", "update", "backport", "dlc"].includes(k)) && <span>{group.jobs.length} package{group.jobs.length === 1 ? "" : "s"}</span>}
  </span>
}

function TransferStats({ job, upload }: { job: DeliveryJob; upload?: UploadState | null }) {
  const detail = phaseDetail(job)
  const tracked = job.stage === "uploading" ? upload : null
  const done = tracked ? tracked.bytesDone : job.bytesDone || 0
  const total = tracked ? tracked.bytesTotal : job.bytesTotal
  const speed = tracked ? tracked.speedBps : 0
  const trackedEta = tracked && speed > 0 ? Math.ceil((tracked.bytesTotal - tracked.bytesDone) / speed) : null
  const eta = tracked ? trackedEta : job.etaSeconds
  const left = total
    ? `${transferSize(done)} / ${transferSize(total)}`
    : job.stage === "extracting"
      ? detail.doneBytes != null ? `${transferSize(detail.doneBytes)} so far · ${detail.label}` : detail.label
      : stageLabel(job)
  return <div className="download-stats">
    <span>{left}</span>
    <span>{tracked && !job.paused && speed > 0 && <strong>{transferSize(speed)}/s</strong>}{!tracked && !job.paused && activeTransfer(job) && job.speedBps > 0 && <strong>{transferSize(job.speedBps)}/s</strong>}{!job.paused && activeTransfer(job) && eta != null && eta > 0 && <span> · {eta < 60 ? `${eta}s left` : `${Math.ceil(eta / 60)} min left`}</span>}</span>
  </div>
}

/** Monotonic per-job upload trackers: uploading bytes/speed come from a bounded
 * window over aggregate bytes (no spikes), never from raw backend rates. */
function useUploadTrackers() {
  const trackers = useRef(new Map<string, UploadTracker>())
  const stages = useRef(new Map<string, string>())
  return (job: DeliveryJob): UploadState | null => {
    if (job.stage !== "uploading") {
      if (!activeTransfer(job)) {
        trackers.current.delete(job.jobId)
        stages.current.delete(job.jobId)
      } else {
        stages.current.set(job.jobId, job.stage)
      }
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

/** Bar percent: measured extraction bytes when available, tracker-backed upload bars, legacy otherwise. */
function barPercent(job: DeliveryJob, upload?: UploadState | null): number {
  if (job.stage === "extracting") {
    const detail = phaseDetail(job)
    if (detail.doneBytes != null && detail.totalBytes) {
      return Math.round(Math.min(0.99, detail.doneBytes / detail.totalBytes) * 100)
    }
  }
  return honestPercent(job, upload)
}

function barIndeterminate(job: DeliveryJob, active: boolean): boolean {
  if (job.stage === "queued") return false
  if (phaseDetail(job).indeterminate) return !job.paused && active
  return !job.paused && active && !job.bytesTotal && job.progress === 0
}

function GameDownload({ group, expanded, onExpand, onOpen, upload }: { group: DownloadGroup; expanded: boolean; onExpand: (value: boolean) => void; onOpen: () => void; upload?: UploadState | null }) {
  const backdrop = useCoverSource(group.icon, group.title)
  const job = group.primary
  const active = activeTransfer(job)
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const clearHover = () => { if (hoverTimer.current) clearTimeout(hoverTimer.current) }
  useEffect(() => clearHover, [])
  const attention = ["failed", "monitoring-ended"].includes(job.stage)
  return <button type="button" className={cn("download-game", expanded && "is-expanded", attention && "has-error")} onClick={onOpen} onPointerMove={event => { if (event.movementX === 0 && event.movementY === 0) return; clearHover(); if (!expanded) hoverTimer.current = setTimeout(() => onExpand(true), 140) }} onPointerLeave={clearHover} onFocus={() => onExpand(true)} aria-label={`View files for ${group.title}`}>
    <img className="download-backdrop" src={backdrop} alt="" aria-hidden="true" />
    <span className="download-scrim" />
    <span className="download-case has-case"><CaseCover source={group.icon} title={group.title} titleId={group.titleId} /></span>
    <span className="download-summary">
      <span className="download-title-row"><strong>{group.title}</strong><span className={cn("download-status", active && "is-active", attention && "attention")}>{stageLabel(job)}</span></span>
      <span className="download-identity">{group.titleId && <span className="download-title-id">{group.titleId}</span>}<PackageTags group={group} /></span>
      <span className="download-expanded-info">
        <Progress value={barPercent(job, upload)} indeterminate={barIndeterminate(job, active)} aria-label={`${stageLabel(job)} progress`} />
        <TransferStats job={job} upload={upload} />
        <span className="download-card-foot"><span>{attention ? "Open files to view the error" : active ? kindLabel(kindOf(job)) + (job.packageVersion ? ` · v${job.packageVersion}` : "") : `${group.jobs.filter(j => j.stage === "complete").length} of ${group.jobs.length} complete`}</span><span className="download-files-link"><FileStack />Files {group.jobs.length}<ChevronRight /></span></span>
      </span>
    </span>
    <ChevronRight className="download-compact-arrow" />
  </button>
}

function FileTransfer({ job, demo, onCancel, onPause, upload }: {job: DeliveryJob; demo: boolean; onCancel: Props["onCancel"]; onPause: Props["onPause"]; upload?: UploadState | null}) {
  const [cancelling, setCancelling] = useState(false)
  const active = activeTransfer(job)
  const consoleOwned = ["submitting", "installing", "mounting"].includes(job.stage)
  const pausable = ["downloading", "uploading"].includes(job.stage)
  const [changing, setChanging] = useState(false)
  const togglePause = async () => { setChanging(true); try { await onPause(job.jobId, !job.paused) } finally { setChanging(false) } }
  const kind = kindOf(job)
  const label = kindLabel(kind) + (job.packageVersion ? ` · v${job.packageVersion}` : "")
  const detail = phaseDetail(job)
  const failure = errorContext(job)
  const cancel = async () => {
    setCancelling(true)
    try { await onCancel(job.jobId) } finally { setCancelling(false) }
  }
  return <article className={cn("download-file", job.stage === "failed" && "has-error")}>
    <div className="download-file-head"><span className={cn("download-file-kind", kind)}>{({base:"BASE",update:"UP",dlc:"DLC",backport:"BP"}[kind] || "FILE")}</span><h3>{label}</h3><span className="download-file-status">{stageLabel(job)}</span>{!demo && active && pausable && <button type="button" className="download-file-cancel" disabled={changing} onClick={togglePause}>{job.paused ? <Play /> : <Pause />}{job.paused ? "Resume" : "Pause"}</button>}{!demo && active && !consoleOwned && <button type="button" className="download-file-cancel" disabled={cancelling} onClick={cancel} aria-label={`Cancel ${label}`}><X />{cancelling ? "Cancelling…" : "Cancel"}</button>}</div>
    {job.packageLabel && job.packageLabel !== label && <p className="download-file-name">{job.packageLabel}</p>}
    <ol className="download-phases" aria-label={`Stages for ${label}`}>
      {transferPhases.map((phase, index) => {
        const state = phaseState(job, index)
        return <li key={phase} data-state={state} aria-current={state === "current" ? "step" : undefined}><span className="download-phase-marker">{state === "done" ? <Check /> : state === "failed" ? <X /> : index + 1}</span><span>{phase}<small>{({done:"Done",current:job.stage === "monitoring-ended" ? "Check console" : "In progress",failed:job.stage === "cancelled" ? "Cancelled" : "Failed",skipped:"Not needed",pending:"Waiting"})[state]}</small></span></li>
      })}
    </ol>
    <Progress value={barPercent(job, upload)} indeterminate={barIndeterminate(job, active)} aria-label={`${label} progress`} />
    <TransferStats job={job} upload={upload} />
    {detail.label !== stageLabel(job) && activeTransfer(job) && <p className="download-file-message">{detail.label}{detail.filesTotal ? ` · ${detail.filesDone} of ${detail.filesTotal} files` : ""}</p>}
    {consoleOwned && <p className="download-file-message">Installation is managed by the PS5. Manage it on the console.</p>}
    {failure
      ? <p className="download-file-message">Failed during {failure.phase}{failure.affected ? ` (${failure.affected})` : ""}: {failure.detail}{failure.retryHint ? ` ${failure.retryHint}` : ""}</p>
      : job.message && <p className="download-file-message">{job.message}</p>}
  </article>
}

export function DownloadGroups({ jobs, demo, onCancel, onPause, downloadDir, systemDrive, onOpenSettings }: Props) {
  const groups = useMemo(() => groupDownloads(jobs), [jobs])
  const trackUpload = useUploadTrackers()
  const locations = downloadDir ? stagingLocations(downloadDir) : null
  const storageWarning = locations
    ? systemDriveWarning({ download: locations.downloadDir, extraction: locations.extractionDir, staging: locations.stagingDir }, systemDrive ?? null)
    : null
  const [filter, setFilter] = useState("All")
  const [hovered, setHovered] = useState<string | null>(null)
  const [opened, setOpened] = useState<string | null>(null)
  const shown = groups.filter(g => filter === "All" || (filter === "Active" ? g.jobs.some(activeTransfer) : g.jobs.some(j => ["failed", "monitoring-ended"].includes(j.stage))))
  const selected = groups.find(g => g.key === opened)
  const expanded = hovered || shown.find(g => g.jobs.some(activeTransfer))?.key || shown[0]?.key
  return <>
    <div className="download-filters" aria-label="Filter downloads">{["All", "Active", "Attention"].map(value => <button type="button" key={value} aria-pressed={filter === value} onClick={() => setFilter(value)}>{value}</button>)}</div>
    {storageWarning && <div role="alert" style={{ border: "1px solid #e5b55266", background: "#dfaa3712", color: "#ffdb89", borderRadius: 8, padding: "10px 14px", fontSize: 12, lineHeight: 1.6, marginBottom: 14, display: "flex", gap: 12, alignItems: "center", justifyContent: "space-between", flexWrap: "wrap" }}>
      <span>{storageWarning}</span>
      {onOpenSettings && <button type="button" onClick={onOpenSettings} style={{ flex: "none", background: "none", border: "1px solid #e5b55266", borderRadius: 6, color: "#ffdb89", fontSize: 12, padding: "4px 10px" }}>Open folder settings</button>}
    </div>}
    <div className="download-games">{shown.map(group => <GameDownload key={group.key} group={group} expanded={expanded === group.key} onExpand={value => setHovered(old => value ? group.key : old === group.key ? null : old)} onOpen={() => setOpened(group.key)} upload={trackUpload(group.primary)} />)}{shown.length === 0 && <p className="download-empty">{jobs.length ? "No downloads in this view." : "Your downloads will appear here."}</p>}</div>
    <Dialog open={!!selected} onOpenChange={value => { if (!value) setOpened(null) }}>
      <DialogContent className="download-details">
        {selected && <>
          <DialogHeader className="download-details-heading"><div className="download-details-case has-case"><CaseCover source={selected.icon} title={selected.title} titleId={selected.titleId} /></div><div><DialogTitle>{selected.title}</DialogTitle><DialogDescription>{[selected.titleId, `${selected.jobs.length} file${selected.jobs.length === 1 ? "" : "s"}`].filter(Boolean).join(" · ")}</DialogDescription><PackageTags group={selected} /></div></DialogHeader>
          <div className="download-detail-files">{selected.jobs.map(job => <FileTransfer key={job.jobId} job={job} demo={demo} onCancel={onCancel} onPause={onPause} upload={trackUpload(job)} />)}</div>
        </>}
      </DialogContent>
    </Dialog>
  </>
}
