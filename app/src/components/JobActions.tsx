import { useEffect, useRef, useState } from "react"
import { Menu } from "@tauri-apps/api/menu"
import { LogicalPosition } from "@tauri-apps/api/dpi"
import { invoke } from "@tauri-apps/api/core"
import { Pause, Play, RotateCcw, Trash2, X } from "lucide-react"
import type { DeliveryJob } from "@/types"
import { jobControls } from "@/lib/downloads"

export type JobActionProps = {
  job: DeliveryJob
  demo: boolean
  onPause: (id: string, paused: boolean) => Promise<unknown>
  onCancel: (id: string) => Promise<unknown>
  onRetry: (id: string) => Promise<unknown>
}

export function useJobActions({ job, demo, onPause, onCancel, onRetry }: JobActionProps) {
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState("")
  const [removing, setRemoving] = useState(false)
  const menu = useRef<Menu | null>(null)
  useEffect(() => () => { void menu.current?.close() }, [])
  const controls = jobControls(job)
  const remove = async (deleteFiles: boolean) => {
    if (busy || demo) return
    setBusy("remove"); setError("")
    try { await invoke("remove_job", { jobId: job.jobId, deleteFiles }); setRemoving(false) }
    catch (error) { setError(String(error)) }
    finally { setBusy(null) }
  }
  const run = async (action: "pause" | "cancel" | "retry" | "priority") => {
    if (busy || demo) return
    setBusy(action); setError("")
    try {
      if (action === "pause") await onPause(job.jobId, !job.paused)
      else if (action === "cancel") await onCancel(job.jobId)
      else if (action === "priority") await invoke("set_job_priority", { jobId: job.jobId, priority: !job.priority })
      else await onRetry(job.jobId)
    } catch (error) { setError(String(error)) }
    finally { setBusy(null) }
  }
  const contextMenu = async (event: React.MouseEvent | React.KeyboardEvent) => {
    if (demo) return
    event.preventDefault(); event.stopPropagation()
    const rect = event.currentTarget.getBoundingClientRect()
    const point = "clientX" in event ? new LogicalPosition(event.clientX, event.clientY) : new LogicalPosition(rect.left + 24, rect.top + 24)
    try {
      await menu.current?.close()
      menu.current = await Menu.new({ items: [
        { text: "Retry from retained files", enabled: controls.retry && !busy, action: () => { void run("retry") } },
        { text: job.paused ? "Resume" : "Pause", enabled: controls.pause && !busy, action: () => { void run("pause") } },
        { text: job.priority ? "Remove priority" : "Prioritize", enabled: controls.priority && !busy, action: () => { void run("priority") } },
        { text: "Cancel", enabled: controls.cancel && !busy, action: () => { void run("cancel") } },
        { item: "Separator" },
        { text: "Remove entry…", enabled: controls.remove && !busy, action: () => setRemoving(true) },
        { text: "Copy details", action: () => { void navigator.clipboard.writeText(`${job.title || job.titleId || "Package"}\n${job.message}`).catch(error => setError(String(error))) } },
      ] })
      await menu.current.popup(point)
    } catch (error) { setError(`Could not open the native menu: ${error}`) }
  }
  return { controls, busy, error, run, contextMenu, removing, setRemoving, remove,
    onKeyDown: (event: React.KeyboardEvent) => { if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) void contextMenu(event) } }
}

export function JobActions({ actions, paused, demo }: { actions: ReturnType<typeof useJobActions>; paused?: boolean; demo: boolean }) {
  if (demo) return null
  return <div className="download-actions" onClick={event => event.stopPropagation()}>
    {actions.controls.retry && <button type="button" disabled={!!actions.busy} onClick={() => void actions.run("retry")}><RotateCcw />{actions.busy === "retry" ? "Retrying…" : "Retry"}</button>}
    {actions.controls.pause && <button type="button" disabled={!!actions.busy} onClick={() => void actions.run("pause")}>{paused ? <Play /> : <Pause />}{paused ? "Resume" : "Pause"}</button>}
    {actions.controls.cancel && <button type="button" disabled={!!actions.busy} onClick={() => void actions.run("cancel")}><X />{actions.busy === "cancel" ? "Cancelling…" : "Cancel"}</button>}
    {actions.controls.remove && !actions.removing && <button type="button" disabled={!!actions.busy} onClick={() => actions.setRemoving(true)}><Trash2 />Remove…</button>}
    {actions.removing && <div className="download-remove-confirm"><span>Remove this transfer? Imported originals and files used by other transfers are kept.</span><button type="button" disabled={!!actions.busy} onClick={() => void actions.remove(false)}>Keep files</button><button type="button" className="delete-files" disabled={!!actions.busy} onClick={() => void actions.remove(true)}>{actions.busy === "remove" ? "Cleaning…" : "Delete leftover files"}</button><button type="button" disabled={!!actions.busy} onClick={() => actions.setRemoving(false)}>Back</button></div>}
    {actions.error && <p role="alert">{actions.error}</p>}
  </div>
}
