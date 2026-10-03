import { invoke as nativeInvoke } from "@tauri-apps/api/core"
import type { ConsoleKind } from "@/types"
import type { PayloadEntry } from "./console-types"

export const launcherAvailable = (demo: boolean) => demo || (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window)
function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!launcherAvailable(false)) return Promise.reject(new Error("Available in the SSPI app."))
  return nativeInvoke<T>(command, args)
}

export type AutostartStepState = "pending" | "waiting" | "sending" | "sent" | "verified" | "failed" | "skipped"
export type AutostartStep = { payloadId: string; delayMs: number; state: AutostartStepState; message: string }
export type AutostartPhase = "off" | "ready" | "waiting" | "unavailable" | "sending" | "done" | "failed" | "stopped"
export type AutostartStatus = {
  target: ConsoleKind; enabled: boolean; steps: AutostartStep[]
  phase: AutostartPhase; message: string; current: number | null
  attempts: number; lastAttemptAt: number | null; nextAttemptAt: number | null
}
/** The saved order: each payload waits `delayMs` after the previous one (or after the start). */
export type AutostartOrder = Array<{ payloadId: string; delayMs: number }>
export const AUTOSTART_MAX_STEPS = 16
export const AUTOSTART_MAX_DELAY_MS = 120_000
export const autostartRunning = (status?: AutostartStatus) => !!status && ["waiting", "unavailable", "sending"].includes(status.phase)

export type WebLauncherStatus = {
  running: boolean; phase: "idle" | "ready" | "dns" | "connected" | "loading" | "error"
  version: string; availableVersion: string | null; address: string; url: string
  message: string; requests: number; lastClient: string | null; lastRequestAt: number | null; logs: string[]
  dnsRequests: number; lastDnsClient: string | null; lastDnsAt: number | null; lastDnsName: string | null
  consoleClient: string | null; consoleLastSeenAt: number | null
  managerReady: boolean; managerSession: string | null; managerCheckedAt: number | null
}
export type CatalogPayload = {
  name: string; filename: string; url: string; source?: string; description?: string
  version?: string; category?: string; checksum?: string; installed: boolean; last_update?: string
}
export type PayloadCatalog = { entries: CatalogPayload[]; checkedAt: number; stale: boolean; warning: string | null }
export type CatalogSort = "category" | "name" | "update_time"
export function filterCatalog(entries: CatalogPayload[], search: string, category: string, showInstalled: boolean, sort: CatalogSort): CatalogPayload[] {
  const normalize = (value: string) => value.toLowerCase().replace(/[_-]+/g, " ")
  const terms = normalize(search).trim().split(/\s+/).filter(Boolean)
  const nameOrder = (a: CatalogPayload, b: CatalogPayload) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" })
  return entries.filter(entry => {
    if (!showInstalled && entry.installed) return false
    if (category && (entry.category?.trim() || "Uncategorized") !== category) return false
    const text = normalize([entry.name, entry.filename, entry.description, entry.category, entry.version].filter(Boolean).join(" "))
    return terms.every(term => text.includes(term))
  }).sort((a, b) => {
    if (sort === "category") return (a.category?.trim() || "Uncategorized").localeCompare(b.category?.trim() || "Uncategorized") || nameOrder(a, b)
    if (sort === "update_time") return (Date.parse(b.last_update || "") || 0) - (Date.parse(a.last_update || "") || 0) || nameOrder(a, b)
    return nameOrder(a, b)
  })
}
const clone = <T,>(value: T): T => JSON.parse(JSON.stringify(value))

/* ---------------------------------------------------------------- autostart */
const off = (target: ConsoleKind): AutostartStatus => ({ target, enabled: false, steps: [], phase: "off", message: "Autostart is off.", current: null, attempts: 0, lastAttemptAt: null, nextAttemptAt: null })
let previewAuto: AutostartStatus[] = [off("ps5"), off("ps4")]
const previewTimers: Partial<Record<ConsoleKind, number[]>> = {}

function previewUpdate(target: ConsoleKind, change: (status: AutostartStatus) => AutostartStatus) {
  previewAuto = previewAuto.map(item => item.target === target ? change(clone(item)) : item)
}
/** The offline preview plays a run through: each payload waits its delay, then "sends". */
function previewRun(target: ConsoleKind) {
  for (const timer of previewTimers[target] || []) window.clearTimeout(timer)
  const timers: number[] = previewTimers[target] = []
  const status = previewAuto.find(item => item.target === target)
  if (!status?.steps.length) return
  const at = (ms: number, fn: () => void) => timers.push(window.setTimeout(fn, ms))
  previewUpdate(target, s => ({ ...s, phase: "waiting", message: "Starting soon.", current: 0, steps: s.steps.map((step, i) => ({ ...step, state: i ? "pending" : "waiting", message: "" })), nextAttemptAt: Date.now() + 1200 + s.steps[0].delayMs }))
  let t = 1200
  status.steps.forEach((step, index) => {
    t += step.delayMs
    at(t, () => previewUpdate(target, s => ({ ...s, phase: "sending", message: "Sending through the configured loader…", current: index, lastAttemptAt: Date.now(), nextAttemptAt: null, steps: s.steps.map((item, i) => i === index ? { ...item, state: "sending" } : item) })))
    t += 1100
    const last = index === status.steps.length - 1
    at(t, () => previewUpdate(target, s => ({
      ...s, phase: last ? "done" : "waiting", current: last ? null : index + 1,
      message: last ? `Sent ${s.steps.length} payload${s.steps.length === 1 ? "" : "s"}.` : "Waiting before the next payload.",
      nextAttemptAt: last ? null : Date.now() + (s.steps[index + 1]?.delayMs || 0),
      steps: s.steps.map((item, i) => i === index ? { ...item, state: item.payloadId.startsWith("builtin:") ? "verified" : "sent", message: "Offline preview: nothing was sent." } : i === index + 1 ? { ...item, state: "waiting" } : item),
    })))
  })
}

export async function getAutostart(demo: boolean): Promise<AutostartStatus[]> {
  return demo ? clone(previewAuto) : invoke("get_payload_autostart")
}
export async function setAutostart(target: ConsoleKind, enabled: boolean, order: AutostartOrder, demo: boolean): Promise<AutostartStatus[]> {
  if (demo) {
    const touched = previewAuto.find(item => item.target === target)?.steps.some(step => ["sending", "sent", "verified", "failed"].includes(step.state))
    previewUpdate(target, () => ({
      ...off(target), enabled,
      steps: order.map(step => ({ ...step, state: "pending" as const, message: "" })),
      phase: enabled && order.length ? "ready" : "off",
      message: enabled && order.length ? "Runs the next time SSPI starts." : "Autostart is off.",
    }))
    if (enabled && order.length && !touched) previewRun(target)
    return clone(previewAuto)
  }
  return invoke("set_payload_autostart", { target, enabled, steps: order })
}
export async function runAutostart(target: ConsoleKind, demo: boolean): Promise<AutostartStatus[]> {
  if (demo) { previewRun(target); return clone(previewAuto) }
  return invoke("run_payload_autostart", { target })
}
export async function stopAutostart(target: ConsoleKind, demo: boolean): Promise<AutostartStatus[]> {
  if (demo) {
    for (const timer of previewTimers[target] || []) window.clearTimeout(timer)
    previewUpdate(target, s => ({ ...s, phase: "stopped", message: "Stopped. The remaining payloads were not sent.", current: null, nextAttemptAt: null, steps: s.steps.map(step => ["waiting", "pending", "sending"].includes(step.state) ? { ...step, state: "skipped" } : step) }))
    return clone(previewAuto)
  }
  return invoke("stop_payload_autostart", { target })
}

/* ---------------------------------------------------------------- web launcher */
const PREVIEW_HOST = ["192", "168", "0", "10"].join(".")
const emptyWebContact = { dnsRequests: 0, lastDnsClient: null, lastDnsAt: null, lastDnsName: null, consoleClient: null, consoleLastSeenAt: null, managerReady: false, managerSession: null, managerCheckedAt: null }
let previewWeb: WebLauncherStatus = { ...emptyWebContact, running: false, phase: "idle", version: "0.5.2-sspi-2.24.2", availableVersion: null, address: PREVIEW_HOST, url: `http://${PREVIEW_HOST}/`, message: "Ready to host SSPI Web Launcher.", requests: 0, lastClient: null, lastRequestAt: null, logs: [] }
let previewStarted = 0

/** The offline preview walks through a console visit: DNS lookups, the launcher page, then the autoloader. */
function previewWebNow(): WebLauncherStatus {
  if (!previewWeb.running || !previewStarted) return previewWeb
  const elapsed = (Date.now() - previewStarted) / 1000
  const client = `${previewWeb.address.split(".").slice(0, 3).join(".")}.41`
  const logs = ["Offline preview: no DNS or web server was started.", `DNS listening on ${previewWeb.address}:53`, `HTTPS listening on ${previewWeb.address}:443`]
  if (elapsed < 3) return { ...previewWeb, phase: "ready", logs }
  const contact = { dnsRequests: 3, lastDnsClient: client, lastDnsAt: Date.now() - 500, lastDnsName: "manuals.playstation.net", consoleClient: client, consoleLastSeenAt: Date.now() - 300 }
  if (elapsed < 6) return { ...previewWeb, ...contact, phase: "dns", logs: [...logs, `${client} DNS manuals.playstation.net → SSPI host`] }
  const requests = Math.min(64, Math.floor((elapsed - 6) * 2.5) + 1)
  logs.push(`${client} resolved the User's Guide host`, `${client} GET / (SSPI launcher)`)
  if (elapsed < 13) return { ...previewWeb, ...contact, phase: "connected", requests, lastClient: client, lastRequestAt: Date.now() - 400, message: "The SSPI launcher was requested. Follow the console screen.", logs }
  logs.push(`${client} GET /autoloader/`, `${client} GET /payloads/elfldr.elf`)
  if (elapsed > 18) return { ...previewWeb, ...contact, phase: "loading", requests, lastClient: client, lastRequestAt: Date.now() - 300, managerReady: true, managerSession: "123-456-789", managerCheckedAt: Date.now() - 300, message: "Offline preview: Payload Manager session confirmed.", logs: [...logs, `${client} console: Opening Payload Manager`, `${client} Payload Manager ready · session 123-456-789`] }
  return { ...previewWeb, ...contact, phase: "loading", requests, lastClient: client, lastRequestAt: Date.now() - 300, message: "Serving SSPI launcher files. Follow progress on your PS5.", logs }
}
export async function getWebLauncher(demo: boolean): Promise<WebLauncherStatus> {
  return demo ? clone(previewWebNow()) : invoke("get_web_launcher")
}
export async function startWebLauncher(address: string, demo: boolean): Promise<WebLauncherStatus> {
  if (demo) {
    const host = address.trim() || previewWeb.address
    previewStarted = Date.now()
    previewWeb = { ...previewWeb, ...emptyWebContact, running: true, phase: "ready", address: host, url: `http://${host}/`, requests: 0, lastClient: null, lastRequestAt: null, message: "Offline preview: no DNS or web server was started.", logs: [] }
    return clone(previewWebNow())
  }
  return invoke("start_web_launcher", { address: address || null })
}
export async function stopWebLauncher(demo: boolean): Promise<WebLauncherStatus> {
  if (demo) { previewWeb = { ...previewWebNow(), managerReady: false, running: false, phase: "idle", message: "Host stopped. Restore your PS5's previous DNS settings when finished." }; previewStarted = 0; return clone(previewWeb) }
  return invoke("stop_web_launcher")
}

/* ---------------------------------------------------------------- payload catalog */
export async function getPayloadCatalog(refresh: boolean, demo: boolean): Promise<PayloadCatalog> {
  return demo ? {
    entries: [
      { name: "WebKit Autoloader Installer", filename: "webkit-autoloader-installer_v0.5.2.elf", url: "https://github.com/itsPLK/ps5-webkit-autoloader/releases", source: "https://github.com/itsPLK/ps5-webkit-autoloader", description: "Installs WebKit Autoloader on the PS5 home screen.", version: "v0.5.2", category: "Loaders", checksum: "preview", installed: false },
      { name: "FTP server", filename: "ftpsrv.elf", url: "https://github.com/ps5-payload-dev/ftpsrv/releases", source: "https://github.com/ps5-payload-dev/ftpsrv", description: "Browse and copy console files over FTP on port 2121.", version: "v0.11", category: "Tools", installed: true },
      { name: "klogsrv", filename: "klogsrv.elf", url: "https://github.com/ps5-payload-dev/klogsrv/releases", source: "https://github.com/ps5-payload-dev/klogsrv", description: "Streams the kernel log over the network for SSPI's live kernel log.", version: "v0.5", category: "Debug", installed: false },
    ], checkedAt: Date.now(), stale: false, warning: null,
  } : invoke("get_payload_catalog", { refresh })
}
export async function downloadCatalogPayload(filename: string): Promise<PayloadEntry[]> { return invoke("download_catalog_payload", { filename }) }
