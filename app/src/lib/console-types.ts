/* =====================================================================
   Contracts between the interface and the console backend (Tauri
   commands) for Library, receiver detection and Tools. Tauri maps the
   camelCase argument names below to the Rust snake_case parameters.
   Command names, arguments and result shapes are fixed; the receiver
   wire protocol behind them is the backend's business.
   ===================================================================== */
import type { ConsoleKind } from "@/types"

/* ---------------------------------------------------------------- detection */

/** `probe_consoles()`: both configured receivers, checked concurrently at launch and on demand. Never touches loader ports. */
export type ReceiverState = "online" | "outdated" | "offline" | "unconfigured" | "error"
export type ConsoleProbe = {
  target: ConsoleKind
  host: string // "" when the console isn't configured
  receiver: {
    state: ReceiverState
    port: number
    version?: string | null
    expectedVersion: string
    capabilities: string[]
    latencyMs?: number | null
    message?: string | null // plain sentence for the interface when not online
  }
  checkedAt: number // unix ms
}

/** `discover_consoles()`: a short scan of this PC's private IPv4 /24 networks for receivers (9114) and FTP servers (2121). */
export type DiscoveredConsole = {
  host: string
  platform: ConsoleKind | "unknown"
  receiver?: { port: number; version?: string | null; platform?: ConsoleKind | null } | null
  ftp?: { port: number; banner: string } | null
  label: string // e.g. "PS5 receiver 1.0.6", "PS4, GoldHEN FTP"
}

/* ---------------------------------------------------------------- library */

export type LibraryTitle = {
  titleId: string // CUSA00000 or PPSA00000
  name: string
  version?: string | null
  baseVersion?: string | null
  updateVersion?: string | null
  icon?: string | null // small JPEG data URL (at most 256 px)
  customIcon?: boolean // SSPI replaced the icon and kept the original
  requiredFirmware?: string | null // "9.00", from SYSTEM_VER or requiredSystemSoftwareVersion
  contentId?: string | null
  platform?: ConsoleKind | null // a PS5 can hold PS4 titles
  cover?: string | null // box art; only the offline preview supplies it
}

/** `list_console_library({ target, host, port })`. Only a complete snapshot may replace a cached library. */
export type ConsoleLibrarySnapshot = {
  target: ConsoleKind
  entries: LibraryTitle[]
  complete: boolean
  truncated: boolean
  errors: string[]
  metadataWarnings: string[]
}

/* ---------------------------------------------------------------- icons and home screen */

/**
 * `get_title_icon({ target, host, port, titleId, original })` → `data:image/png;base64,…` at full size.
 * With `original: true` the receiver returns its saved original when SSPI changed the icon, otherwise the current one.
 * `set_title_icon({ target, host, port, titleId, png })`: `png` is base64 PNG bytes without a data: prefix, square, 256–1024 px.
 * `restore_title_icon({ target, host, port, titleId })` → IconWriteResult; puts the saved original back.
 */
export type IconWriteResult = {
  titleId: string
  written: number // metadata copies written
  backedUp: boolean // the original was saved by this write (first change only)
  refresh: "live" | "restart-required"
  message: string
}

/** `refresh_console_shell({ target, host, port })` → message. Restarts the home screen; refused while a game is running. */
export type ShellRefreshResult = string

/* ---------------------------------------------------------------- system information */

/** `console_system_info({ target, host, port })`. Fields the console can't report are null. */
export type ConsoleSystemInfo = {
  target: ConsoleKind
  receiverVersion: string
  firmware?: string | null // "11.00"
  sdkVersion?: string | null // "0x11008001"
  model?: string | null
  consoleName?: string | null
  uptimeSeconds?: number | null
  cpuTempC?: number | null
  socTempC?: number | null
  storage: Array<{ label: string; path: string; totalBytes: number; freeBytes: number }>
  memory?: { totalBytes: number; freeBytes: number } | null
  network?: { ip?: string | null; mac?: string | null } | null
  runningTitleId?: string | null
  capabilities: string[]
  extras: Record<string, string>
}

/* ---------------------------------------------------------------- payloads (binloader manager) */

export type PayloadTarget = ConsoleKind | "any"
export type PayloadEntry = {
  id: string // "builtin:ps5-receiver", "builtin:ps4-receiver", or a random id for added files
  name: string
  fileName: string
  path: string // stored copy under the app data folder; empty for built-ins
  size: number
  sha256: string
  target: PayloadTarget
  builtin: boolean
  addedAt: number
  lastSentAt?: number | null
  lastResult?: string | null
  notes?: string | null
}

/**
 * `list_payloads()` → PayloadEntry[]
 * `add_payloads({ paths, target })` → full list (files are copied into the app data folder)
 * `update_payload({ id, name, target, notes })` → full list
 * `remove_payload({ id })` → full list (built-ins can't be removed)
 * `send_payload({ id, target, host, port })` → PayloadSendResult; `port` is the loader port
 *   (PS4 GoldHEN BinLoader 9090, PS5 ELF loader 9021). Built-in receivers are verified after sending.
 */
export type PayloadSendResult = { message: string; bytes: number; port: number; verified: boolean }

/** `save_theme_file({ path, contents })` and `load_theme_file({ path })` → contents. `.sspitheme` only, at most 8 MiB. */
export type ThemeFileContents = string
