import { invoke } from "@tauri-apps/api/core"
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
  ThemeFileContents,
} from "./console-types"
import { demoCapabilities, demoIcon, demoLibrary, demoPayloads, demoProbes, demoSystemInfo } from "./console-demo"

const OFFLINE_PREVIEW_MESSAGE = "Offline preview: nothing was sent to the console."
const delay = (ms = 180) => new Promise<void>(resolve => setTimeout(resolve, ms))
const clonedPayload = (entry: PayloadEntry): PayloadEntry => ({ ...entry })
let previewPayloads = demoPayloads.map(clonedPayload)
let previewId = 1
const previewThemes = new Map<string, string>()
// Receivers "loaded" in the offline preview report as running afterwards.
const previewLoaded = new Set<ConsoleKind>()

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
    { host: "192.168.0.215", platform: "ps5", receiver: { port: 9114, version: "1.0.6", platform: "ps5" }, label: "PS5 receiver 1.0.6" },
    { host: "192.168.0.216", platform: "ps4", receiver: { port: 9114, version: "1.0.3", platform: "ps4" }, label: "PS4 receiver 1.0.3" },
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
    return demoResult({ message: OFFLINE_PREVIEW_MESSAGE, bytes: entry?.size || 0, port, verified: entry?.builtin || false } satisfies PayloadSendResult)
  }
  return invoke<PayloadSendResult>("send_payload", { id, target, host, port })
}

export async function saveThemeFile({ path, contents, demo }: { path: string; contents: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) { previewThemes.set(path, contents); return demoResult(contents) }
  return invoke<ThemeFileContents>("save_theme_file", { path, contents })
}

export async function loadThemeFile({ path, demo }: { path: string; demo: boolean }) {
  requireTauri(demo)
  if (demo) return demoResult(previewThemes.get(path) || "")
  return invoke<ThemeFileContents>("load_theme_file", { path })
}
