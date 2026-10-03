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
export type UpdateCommand = "check_for_updates" | "download_update" | "install_update"
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

/* ---------------------------------------------------------------- offline preview */
// The offline preview plays the whole flow with a made-up release; nothing is downloaded or installed.
let preview: UpdateStatus = { ...initial, stage: "current", currentVersion: "2.24.0", currentBuild: 20261002180000, message: "SSPI is up to date.", lastChecked: Date.now() - 4 * 60_000 }
const previewSubscribers = new Set<() => void>()
let previewBusy = false
function setPreview(next: UpdateStatus) { preview = next; previewSubscribers.forEach(callback => callback()) }
const previewRelease: UpdateManifest = {
  schema: 1, product: "windows", channel: "development", version: "2.24.1", build: 20261004120000, publishedAt: new Date().toISOString(),
  notes: "Faster archive extraction and smaller fixes across Downloads and Tools. This release only exists in the offline preview.",
  restart: "SSPI closes and reopens to finish.", package: { url: "", size: 128_400_000, sha256: "", format: "zip" }, files: [],
}
const wait = (ms: number) => new Promise(resolve => window.setTimeout(resolve, ms))
async function runPreview(command: UpdateCommand) {
  if (previewBusy) return
  previewBusy = true
  try {
    if (command === "check_for_updates") {
      setPreview({ ...preview, stage: "checking", message: "Checking for updates…" })
      await wait(1100)
      setPreview({ ...preview, stage: "available", available: previewRelease, total: previewRelease.package.size, downloaded: 0, lastChecked: Date.now(), message: `SSPI ${previewRelease.version} is available.` })
    } else if (command === "download_update") {
      const total = previewRelease.package.size
      for (let done = 0; done < total; done = Math.min(total, done + total / 36)) {
        setPreview({ ...preview, stage: "downloading", downloaded: done, total, message: "Downloading the update…" })
        await wait(110)
      }
      setPreview({ ...preview, stage: "verifying", downloaded: total, total, message: "Checking every file…" })
      await wait(1200)
      setPreview({ ...preview, stage: "ready", message: "The update is ready to install." })
    } else {
      setPreview({ ...preview, stage: "installing", message: "Restarting SSPI…" })
      await wait(1600)
      setPreview({ ...preview, stage: "current", available: null, downloaded: 0, total: 0, lastChecked: Date.now(), message: "Offline preview: nothing was installed." })
    }
  } finally { previewBusy = false }
}

export function useUpdater(demo = false) {
  return useSyncExternalStore(
    callback => { const set = demo ? previewSubscribers : subscribers; set.add(callback); return () => { set.delete(callback) } },
    () => demo ? preview : status, () => initial,
  )
}
export function runUpdateAction(command: UpdateCommand, background = false, demo = false) {
  if (demo) return runPreview(command)
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
