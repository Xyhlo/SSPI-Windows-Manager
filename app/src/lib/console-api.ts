import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"
import type { ConsoleKind } from "../types"
import type {
  ConsoleLibrarySnapshot,
  ConsoleProbe,
  ConsoleSystemInfo,
  DiscoveredConsole,
  IconWriteResult,
  PayloadEntry,
  PayloadSendResult,
  PayloadTarget,
  ShellRefreshResult,
  DebugService,
  KernelLog,
  KlogStreamEvent,
  LogFileList,
  LogTail,
  ProcessAction,
  ProcessControl,
  ProcessList,
  ConsoleThemes,
  ThemeBuildResult,
  ThemePackageRequest,
} from "./console-types"
import { demoCapabilities, demoIcon, demoKernelLog, demoKlogLines, demoLibrary, demoLogFiles, demoLogTail, demoPayloads, demoProbes, demoProcesses, demoServices, demoSystemInfo } from "./console-demo"

const OFFLINE_PREVIEW_MESSAGE = "Offline preview: nothing was sent to the console."
const delay = (ms = 180) => new Promise<void>(resolve => setTimeout(resolve, ms))
const clonedPayload = (entry: PayloadEntry): PayloadEntry => ({ ...entry })
let previewPayloads = demoPayloads.map(clonedPayload)
let previewId = 1
// Receivers "loaded" in the offline preview report as running afterwards.
const previewLoaded = new Set<ConsoleKind>()
// Processes "stopped" in the offline preview stay gone until the preview reloads.
const previewStopped = new Set<string>()

function requireTauri(demo: boolean) {
  if (!demo && (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window))) {
    throw new Error("This needs the SSPI app.")
  }
}

async function demoResult<T>(value: T): Promise<T> {
  await delay()
  return value
}

export async function probeConsoles({ demo }: { demo: boolean }) {
  requireTauri(demo)
  if (!demo) return invoke<ConsoleProbe[]>("probe_consoles")
  return demoResult(demoProbes.map(probe => previewLoaded.has(probe.target)
    ? { ...probe, receiver: { ...probe.receiver, state: "online" as const, version: probe.receiver.expectedVersion, capabilities: [...demoCapabilities] } }
    : { ...probe, receiver: { ...probe.receiver, capabilities: [...probe.receiver.capabilities] } }))
}

export async function discoverConsoles({ demo }: { demo: boolean }) {
  requireTauri(demo)
  const preview: DiscoveredConsole[] = [
    { host: "192.0.2.15", platform: "ps5", receiver: { port: 9114, version: "1.0.8", platform: "ps5" }, label: "PS5 receiver 1.0.8" },
    { host: "192.0.2.16", platform: "ps4", receiver: { port: 9114, version: "1.0.6", platform: "ps4" }, label: "PS4 receiver 1.0.6" },
  ]
  return demo ? demoResult(preview) : invoke<DiscoveredConsole[]>("discover_consoles")
}

export async function listConsoleLibrary({ target, host, port, demo }: { target: ConsoleKind; host: string; port: number; demo: boolean }) {
  requireTauri(demo)
  return demo ? demoResult(demoLibrary(target)) : invoke<ConsoleLibrarySnapshot>("list_console_library", { target, host, port })
}

export async function getTitleIcon({ target, host, port, titleId, original, demo }: { target: ConsoleKind; host: string; port: number; titleId: string; original: boolean; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    const entry = demoLibrary(target).entries.find(title => title.titleId === titleId)
    return demoResult(entry?.icon || demoIcon("Title icon", "#161923", "#30394b", "#91a9ff"))
  }
  return invoke<string>("get_title_icon", { target, host, port, titleId, original })
}

export async function setTitleIcon({ target, host, port, titleId, png, demo }: { target: ConsoleKind; host: string; port: number; titleId: string; png: string; demo: boolean }) {
  requireTauri(demo)
  const result: IconWriteResult = { titleId, written: 1, backedUp: true, refresh: "live", message: OFFLINE_PREVIEW_MESSAGE }
  return demo ? demoResult(result) : invoke<IconWriteResult>("set_title_icon", { target, host, port, titleId, png })
}

export async function restoreTitleIcon({ target, host, port, titleId, demo }: { target: ConsoleKind; host: string; port: number; titleId: string; demo: boolean }) {
  requireTauri(demo)
  const result: IconWriteResult = { titleId, written: 1, backedUp: false, refresh: "restart-required", message: OFFLINE_PREVIEW_MESSAGE }
  return demo ? demoResult(result) : invoke<IconWriteResult>("restore_title_icon", { target, host, port, titleId })
}

export async function refreshConsoleShell({ target, host, port, demo }: { target: ConsoleKind; host: string; port: number; demo: boolean }) {
  requireTauri(demo)
  const result: ShellRefreshResult = OFFLINE_PREVIEW_MESSAGE
  return demo ? demoResult(result) : invoke<ShellRefreshResult>("refresh_console_shell", { target, host, port })
}

export async function consoleSystemInfo({ target, host, port, demo }: { target: ConsoleKind; host: string; port: number; demo: boolean }) {
  requireTauri(demo)
  return demo ? demoResult(demoSystemInfo(target)) : invoke<ConsoleSystemInfo>("console_system_info", { target, host, port })
}

type Endpoint = { target: ConsoleKind; host: string; port: number; demo: boolean }

export async function consoleKernelLog({ target, host, port, demo }: Endpoint) {
  requireTauri(demo)
  return demo ? demoResult(demoKernelLog(target)) : invoke<KernelLog>("console_kernel_log", { target, host, port })
}

export async function consoleProcesses({ target, host, port, demo }: Endpoint) {
  requireTauri(demo)
  if (!demo) return invoke<ProcessList>("console_processes", { target, host, port })
  const list = demoProcesses(target)
  return demoResult({ ...list, processes: list.processes.filter(p => !previewStopped.has(`${target}:${p.pid}`)) })
}

/** Stops an app or a payload; the receiver refuses system processes, the loader and itself. */
export async function consoleProcessControl({ target, host, port, demo, pid, name, action }: Endpoint & { pid: number; name: string; action: ProcessAction }) {
  requireTauri(demo)
  if (!demo) return invoke<ProcessControl>("console_process_control", { target, host, port, pid, name, action })
  const process = demoProcesses(target).processes.find(p => p.pid === pid && p.name === name)
  if (!process?.control) throw new Error("System processes can't be stopped from here. Only apps, games and payloads loaded through elfldr can.")
  const waitedMs = action === "end" ? 420 : 1300
  await delay(waitedMs)
  previewStopped.add(`${target}:${pid}`)
  const method = action === "end" ? "sigkill" : process.control === "app" ? "close-app" : "sigterm"
  return { pid, name, kind: process.control, method, exited: true, waitedMs } satisfies ProcessControl
}

export async function consoleLogFiles({ target, host, port, demo }: Endpoint) {
  requireTauri(demo)
  return demo ? demoResult(demoLogFiles(target)) : invoke<LogFileList>("console_log_files", { target, host, port })
}

export async function consoleReadLog({ target, host, port, demo, path, maxBytes }: Endpoint & { path: string; maxBytes?: number }) {
  requireTauri(demo)
  return demo ? demoResult(demoLogTail(path)) : invoke<LogTail>("console_read_log", { target, host, port, path, maxBytes: maxBytes ?? null })
}

export async function probeDebugServices({ target, host, demo }: { target: ConsoleKind; host: string; demo: boolean }) {
  requireTauri(demo)
  return demo ? demoResult(demoServices(target)) : invoke<DebugService[]>("probe_debug_services", { target, host })
}

/* The preview replays sample kernel log lines through the same listener interface. */
const previewKlogListeners = new Set<(event: KlogStreamEvent) => void>()
const previewKlogTimers = new Map<number, number>()
let previewKlogId = 1

export async function startKlogStream({ target, host, port, demo }: Endpoint) {
  requireTauri(demo)
  if (!demo) return invoke<number>("start_klog_stream", { target, host, port })
  const id = previewKlogId++
  let n = 0
  const timer = window.setInterval(() => {
    const text = demoKlogLines[n++ % demoKlogLines.length] + "\n"
    previewKlogListeners.forEach(listener => listener({ id, text, closed: false }))
  }, 900)
  previewKlogTimers.set(id, timer)
  return id
}

export async function stopKlogStream({ id, demo }: { id: number; demo: boolean }) {
  if (demo) { window.clearInterval(previewKlogTimers.get(id)); previewKlogTimers.delete(id); return true }
  return invoke<boolean>("stop_klog_stream", { id })
}

/** Subscribes to live kernel log chunks; returns the unsubscribe function. */
export async function onKlogStream(demo: boolean, handler: (event: KlogStreamEvent) => void): Promise<() => void> {
  if (demo) { previewKlogListeners.add(handler); return () => { previewKlogListeners.delete(handler) } }
  return listen<KlogStreamEvent>("klog-stream", event => handler(event.payload))
}

export async function listPayloads({ demo }: { demo: boolean }) {
  requireTauri(demo)
  return demo ? demoResult(previewPayloads.map(clonedPayload)) : invoke<PayloadEntry[]>("list_payloads")
}

export async function addPayloads({ paths, target, demo }: { paths: string[]; target: PayloadTarget; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    previewPayloads = [
      ...previewPayloads,
      ...paths.map(path => {
        const fileName = path.split(/[\\/]/).pop() || "payload.bin"
        const id = `demo-added-${previewId++}`
        const sha = (previewId.toString(16).padStart(2, "0").slice(-2)).repeat(32)
        return { id, name: fileName.replace(/\.(elf|bin)$/i, ""), fileName, path: `preview/${fileName}`, size: 524_288, sha256: sha, target, builtin: false, addedAt: 1_782_516_000_000 + previewId }
      }),
    ]
    return demoResult(previewPayloads.map(clonedPayload))
  }
  return invoke<PayloadEntry[]>("add_payloads", { paths, target })
}

export async function updatePayload({ id, name, target, notes, demo }: { id: string; name: string; target: PayloadTarget; notes: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    previewPayloads = previewPayloads.map(entry => entry.id === id ? { ...entry, name, target, notes } : entry)
    return demoResult(previewPayloads.map(clonedPayload))
  }
  return invoke<PayloadEntry[]>("update_payload", { id, name, target, notes })
}

export async function removePayload({ id, demo }: { id: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    previewPayloads = previewPayloads.filter(entry => entry.id !== id || entry.builtin)
    return demoResult(previewPayloads.map(clonedPayload))
  }
  return invoke<PayloadEntry[]>("remove_payload", { id })
}

export async function sendPayload({ id, target, host, port, demo }: { id: string; target: ConsoleKind; host: string; port: number; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    const entry = previewPayloads.find(payload => payload.id === id)
    if (entry?.builtin) previewLoaded.add(target)
    previewPayloads = previewPayloads.map(payload => payload.id === id ? { ...payload, lastSentAt: 1_782_516_000_000 + previewId++, lastResult: OFFLINE_PREVIEW_MESSAGE } : payload)
    // A labeled simulation so the preview can show the trace layout; nothing is sent.
    const bytes = entry?.size || 0
    const steps = [
      ...(entry?.builtin ? [{ label: "Probe receiver", detail: "Preview: nothing answered", ms: 41, ok: true }] : [{ label: "Read stored copy", detail: `Preview: ${bytes.toLocaleString("en-US")} bytes`, ms: 2, ok: true }]),
      { label: "Connect to loader", detail: `Preview: ${host}:${port}`, ms: 12, ok: true },
      { label: "Send payload", detail: `Preview: ${bytes.toLocaleString("en-US")} bytes, connection closed`, ms: 96, ok: true },
      ...(entry?.builtin ? [{ label: "Verify receiver", detail: "Preview: v1.0.8 answered on :9114", ms: 1420, ok: true }] : []),
    ]
    return demoResult({ message: OFFLINE_PREVIEW_MESSAGE, bytes, port, verified: entry?.builtin || false, steps, totalMs: steps.reduce((sum, step) => sum + step.ms, 0), sendMs: 96, bytesPerSecond: bytes / 0.096, host, sha256: entry?.sha256 || "" } satisfies PayloadSendResult)
  }
  return invoke<PayloadSendResult>("send_payload", { id, target, host, port })
}

/* ---------------------------------------------------------------- PS4 theme packages */
let previewThemeId = 1
let previewInstalled: ConsoleThemes = { themes: [{ contentId: "UP9000-CUSA00000_00-SSPIDEMOTHEME001", title: "Preview theme" }], activeContentId: null, truncated: false }

export async function buildPs4Theme({ request, demo }: { request: ThemePackageRequest; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    const label = /^[A-Z0-9]{16}$/.test(request.label) ? request.label : `SSPIPREVIEW${String(previewThemeId++).padStart(5, "0")}`
    const frames = request.animation?.frames.length || 0
    return demoResult({ path: `preview/${label}.pkg`, size: 2_228_224 + frames * 259_328, contentId: `UP9000-CUSA00000_00-${label}`, title: request.title, frames, sceneBytes: frames * 259_328, files: 8 + frames } satisfies ThemeBuildResult)
  }
  return invoke<ThemeBuildResult>("build_ps4_theme", { request })
}

/** Queues a local PKG for the console; progress shows in Downloads. Returns the job ID. */
export async function installLocalPackage({ path, target, title, demo }: { path: string; target: ConsoleKind; title: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    const label = path.replace(/^preview\//, "").replace(/\.pkg$/, "")
    previewInstalled = { ...previewInstalled, themes: [...previewInstalled.themes.filter(item => !item.contentId.endsWith(label)), { contentId: `UP9000-CUSA00000_00-${label}`, title }] }
    return demoResult(`preview-${label}`)
  }
  return invoke<string>("start_local_install", { path, target })
}

export async function listConsoleThemes({ host, port, demo }: { host: string; port: number; demo: boolean }) {
  requireTauri(demo)
  return demo ? demoResult(structuredClone(previewInstalled)) : invoke<ConsoleThemes>("list_console_themes", { host, port })
}

export async function applyConsoleTheme({ host, port, contentId, demo }: { host: string; port: number; contentId: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) { previewInstalled = { ...previewInstalled, activeContentId: contentId }; return demoResult(OFFLINE_PREVIEW_MESSAGE) }
  return invoke<string>("apply_console_theme", { host, port, contentId })
}

export async function removeConsoleTheme({ host, port, contentId, demo }: { host: string; port: number; contentId: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) {
    previewInstalled = { ...previewInstalled, themes: previewInstalled.themes.filter(item => item.contentId !== contentId), activeContentId: previewInstalled.activeContentId === contentId ? null : previewInstalled.activeContentId }
    return demoResult(OFFLINE_PREVIEW_MESSAGE)
  }
  return invoke<string>("remove_console_theme", { host, port, contentId })
}
