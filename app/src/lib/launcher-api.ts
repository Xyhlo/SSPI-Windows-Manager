import { invoke as nativeInvoke } from "@tauri-apps/api/core"
import type { ConsoleKind } from "@/types"
import type { PayloadEntry } from "./console-types"

export const launcherAvailable = (demo: boolean) => demo || (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window)
function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!launcherAvailable(false)) return Promise.reject(new Error("Available in the SSPI app."))
  return nativeInvoke<T>(command, args)
}

export type AutostartStatus = {
  target: ConsoleKind; payloadId: string | null
  phase: "off" | "waiting" | "unavailable" | "sending" | "sent" | "verified" | "failed"
  message: string; attempts: number; lastAttemptAt: number | null; nextAttemptAt: number | null
}
export type WebLauncherStatus = {
  running: boolean; phase: "idle" | "ready" | "connected" | "loading" | "error"
  version: string; availableVersion: string | null; address: string; url: string
  message: string; requests: number; lastClient: string | null; lastRequestAt: number | null; logs: string[]
}
export type CatalogPayload = {
  name: string; filename: string; url: string; source?: string; description?: string
  version?: string; category?: string; checksum?: string; installed: boolean
}
export type PayloadCatalog = { entries: CatalogPayload[]; checkedAt: number; stale: boolean; warning: string | null }
const off = (target: ConsoleKind): AutostartStatus => ({ target, payloadId: null, phase: "off", message: "Autostart is off.", attempts: 0, lastAttemptAt: null, nextAttemptAt: null })
let previewAuto = [off("ps5"), off("ps4")]
let previewWeb: WebLauncherStatus = { running: false, phase: "idle", version: "0.5.2-sspi-2.23.1", availableVersion: null, address: "192.168.0.10", url: "http://192.168.0.10/", message: "Ready to host SSPI Web Launcher.", requests: 0, lastClient: null, lastRequestAt: null, logs: [] }
const clone = <T,>(value: T): T => JSON.parse(JSON.stringify(value))

export async function getAutostart(demo: boolean): Promise<AutostartStatus[]> {
  return demo ? clone(previewAuto) : invoke("get_payload_autostart")
}
export async function setAutostart(target: ConsoleKind, payloadId: string | null, demo: boolean): Promise<AutostartStatus[]> {
  if (demo) {
    previewAuto = previewAuto.map(item => item.target === target ? { ...off(target), payloadId, phase: payloadId ? "waiting" : "off", message: payloadId ? "Offline preview: this selection would send once when SSPI starts." : "Autostart is off." } : item)
    return clone(previewAuto)
  }
  return invoke("set_payload_autostart", { target, payloadId })
}
export async function getWebLauncher(demo: boolean): Promise<WebLauncherStatus> {
  return demo ? clone(previewWeb) : invoke("get_web_launcher")
}
export async function startWebLauncher(address: string, demo: boolean): Promise<WebLauncherStatus> {
  if (demo) { previewWeb = { ...previewWeb, running: true, phase: "ready", address, url: `http://${address}/`, message: "Offline preview: no DNS or web server was started.", logs: ["Preview only. No network listeners are active."] }; return clone(previewWeb) }
  return invoke("start_web_launcher", { address: address || null })
}
export async function stopWebLauncher(demo: boolean): Promise<WebLauncherStatus> {
  if (demo) { previewWeb = { ...previewWeb, running: false, phase: "idle", message: "Offline preview: host stopped." }; return clone(previewWeb) }
  return invoke("stop_web_launcher")
}
export async function getPayloadCatalog(refresh: boolean, demo: boolean): Promise<PayloadCatalog> {
  return demo ? { entries: [{ name: "WebKit Autoloader Installer", filename: "webkit-autoloader-installer_v0.5.2.elf", url: "https://github.com/itsPLK/ps5-webkit-autoloader/releases", source: "https://github.com/itsPLK/ps5-webkit-autoloader", description: "Installs WebKit Autoloader on the PS5 home screen.", version: "v0.5.2", category: "Loaders", installed: false }], checkedAt: Date.now(), stale: false, warning: null } : invoke("get_payload_catalog", { refresh })
}
export async function downloadCatalogPayload(filename: string): Promise<PayloadEntry[]> { return invoke("download_catalog_payload", { filename }) }
