import { invoke } from "@tauri-apps/api/core"
import { listen, type UnlistenFn } from "@tauri-apps/api/event"
import { useSyncExternalStore } from "react"

export type UpdateManifest = {
  schema: number; product: string; channel: string; version: string; build: number
  publishedAt: string; notes: string; restart: string
  package: { url: string; size: number; sha256: string; format: string }
  files: Array<{ path: string; size: number; sha256: string }>
}
export type UpdateStatus = {
  stage: "idle" | "checking" | "available" | "current" | "downloading" | "verifying" | "ready" | "installing" | "error"
  currentVersion: string; currentBuild: number; channel: string; available: UpdateManifest | null
  downloaded: number; total: number; message: string; lastChecked: number
}
export const CHECK_INTERVAL = 10 * 60 * 1000
const initial: UpdateStatus = { stage: "idle", currentVersion: "", currentBuild: 0, channel: "development", available: null, downloaded: 0, total: 0, message: "", lastChecked: 0 }
let status = initial
let subscribers = new Set<() => void>()
let startCount = 0
let timer: ReturnType<typeof setInterval> | undefined
let unlisten: UnlistenFn | undefined
let generation = 0
let action: Promise<void> | null = null
let userError = false
export const updaterHasUserError = () => userError
export const updaterSupported = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window
export const updateBusy = (s: UpdateStatus) => ["checking", "downloading", "verifying", "installing"].includes(s.stage)
function setStatus(next: UpdateStatus) { status = next; subscribers.forEach(callback => callback()) }
export function useUpdater() {
  return useSyncExternalStore(callback => { subscribers.add(callback); return () => subscribers.delete(callback) }, () => status, () => initial)
}
export function runUpdateAction(command: "check_for_updates" | "download_update" | "install_update", background = false) {
  if (!updaterSupported() || action) return action ?? Promise.resolve()
  action = (async () => {
    userError = false
    try {
      const next = await invoke<UpdateStatus | null>(command)
      if (next) setStatus(next)
    } catch (error) {
      const message = typeof error === "string" ? error : error instanceof Error ? error.message : String(error)
      const backend = await invoke<UpdateStatus>("get_update_status").catch(() => status)
      userError = !background
      setStatus({ ...backend, stage: backend.stage === "ready" ? "ready" : "error", message })
    } finally { action = null }
  })()
  return action
}
/** Reference counted so React StrictMode and an open Options panel cannot create duplicate timers. */
export function startUpdater() {
  if (!updaterSupported()) return () => {}
  startCount++
  if (startCount === 1) {
    const current = ++generation
    void (async () => {
      const stop = await listen<UpdateStatus>("sspi-update", event => setStatus(event.payload)).catch(() => undefined)
      if (current !== generation) { stop?.(); return }
      unlisten = stop
      try { setStatus(await invoke<UpdateStatus>("get_update_status")) } catch (error) { setStatus({ ...status, stage: "error", message: String(error) }) }
      if (current !== generation) return
      void runUpdateAction("check_for_updates", true)
      timer = setInterval(() => { if (!updateBusy(status)) void runUpdateAction("check_for_updates", true) }, CHECK_INTERVAL)
    })()
  }
  return () => {
    if (--startCount === 0) { generation++; if (timer) clearInterval(timer); timer = undefined; unlisten?.(); unlisten = undefined }
  }
}
