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
  label: string // e.g. "PS5 receiver 1.0.7", "PS4, GoldHEN FTP"
}

/* ---------------------------------------------------------------- library */

export type LibrarySource = "appdb" | "app" | "appmeta" | "shadowmount"
export type LibraryDiagnostics = {
  appdb: "ok" | `unavailable:${string}`
  counts: Record<LibrarySource, number>
  skipped: Array<{ id: string; reason: string }>
  skippedTruncated?: boolean
  budgetExceeded?: boolean
  elapsedMs?: number | null
}

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
  sources?: LibrarySource[] // absent on older receivers
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
  diagnostics?: LibraryDiagnostics | null // absent on older receivers
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
  /** Every mounted filesystem (receivers with `diagnostics-v1`). */
  mounts?: ConsoleMount[] | null
  capabilities: string[]
  /** processCount, cpuFrequencyMhz, loadAverage, networkInterface, firmwareRaw; all strings. */
  extras: Record<string, string>
}
export type ConsoleMount = { from: string; on: string; type: string; readOnly: boolean; totalBytes: number; freeBytes: number }

/* ---------------------------------------------------------------- diagnostics (receiver `diagnostics-v1`) */

/**
 * `console_kernel_log({ target, host, port })`. `msgbuf` is a non-destructive snapshot of the kernel
 * message buffer. `klog` is what the receiver drained from /dev/klog; `busy` means another klog
 * server (GoldHEN, etaHEN, klogsrv) holds the device, so use its live stream instead.
 */
export type KernelLog = { source: "msgbuf" | "klog"; busy: boolean; dropped: boolean; bytes: number; text: string; capturedAt: number }

/** `console_processes({ target, host, port })`. The PS4 cannot read auth IDs, so they are null there. */
export type ConsoleProcess = {
  pid: number; ppid: number; name: string
  state: "unknown" | "starting" | "running" | "sleeping" | "stopped" | "zombie" | "waiting" | "locked"
  uid: number; titleId?: string | null; appType?: number | null; authId?: string | null
  rssBytes: number; vmBytes: number; threads: number; startedAt?: number | null; cpuMs: number
  /** What the receiver lets the app stop here (`process-control-v1`): apps and elfldr payloads. */
  control?: "app" | "payload" | null
}
export type ProcessList = { processes: ConsoleProcess[]; truncated: boolean; capturedAt: number }
/** `console_process_control(...)`: `stop` closes an app (or SIGTERM), `end` forces it. */
export type ProcessAction = "stop" | "end"
export type ProcessControl = { pid: number; name: string; kind: "app" | "payload"; method: "close-app" | "sigterm" | "sigkill"; exited: boolean; waitedMs: number }

/** `console_log_files({ target, host, port })`: logs, settings and crash reports under /data and /user/data. */
export type LogFile = { path: string; size: number; modified: number; kind: "log" | "config" | "crash" }
export type LogFileList = { files: LogFile[]; truncated: boolean; incomplete: boolean; capturedAt: number }
/** `console_read_log({ target, host, port, path, maxBytes })`: the last `maxBytes` of a log or settings file. */
export type LogTail = { path: string; size: number; offset: number; bytes: number; modified: number; text: string }

/** `probe_debug_services({ target, host })`: which well-known homebrew services answer a TCP connect. Loader ports are never probed. */
export type DebugServiceKind = "klog" | "ftp" | "debugger" | "installer"
export type DebugService = { port: number; name: string; kind: DebugServiceKind; open: boolean; latencyMs?: number | null }

/**
 * `start_klog_stream({ target, host, port })` → stream id; the app then emits `klog-stream` events.
 * Only known kernel log ports are accepted. `stop_klog_stream({ id })` ends it.
 */
export type KlogStreamEvent = { id: number; text?: string | null; closed: boolean; error?: string | null }

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
  /** Parsed from the stored bytes; absent for raw BIN payloads. */
  elf?: PayloadElf | null
}

export type PayloadElf = { class: string; endian: string; kind: string; machine: string; entry: string; segments: number; loadable: number; loadableBytes: number }

/**
 * `list_payloads()` → PayloadEntry[]
 * `add_payloads({ paths, target })` → full list (files are copied into the app data folder)
 * `update_payload({ id, name, target, notes })` → full list
 * `remove_payload({ id })` → full list (built-ins can't be removed)
 * `send_payload({ id, target, host, port })` → PayloadSendResult; `port` is the loader port
 *   (PS4 GoldHEN BinLoader 9090, PS5 ELF loader 9021). Built-in receivers are verified after sending.
 */
export type PayloadSendStep = { label: string; detail: string; ms: number; ok: boolean }
/** Every send is traced: each step's duration, the socket write time and throughput, and the payload hash. */
export type PayloadSendResult = {
  message: string; bytes: number; port: number; verified: boolean
  steps?: PayloadSendStep[]; totalMs?: number; sendMs?: number | null; bytesPerSecond?: number | null; host?: string; sha256?: string
}

/* ---------------------------------------------------------------- PS4 system themes */

/**
 * `build_ps4_theme({ request })` → ThemeBuildResult. The studio draws every image; the app checks
 * sizes, compresses the animated background into the console's scene format and packs a
 * system-theme PKG (images are base64 PNG, a data: prefix is accepted).
 */
export type ThemePackageColors = { themeColor: number; font: string; fontShadow: string; focus: string; homeDimmer: string; functionDimmer: string; titleDimmer: string }
export type ThemePackageRequest = {
  title: string
  /** 16 characters, A–Z and 0–9; anything else gets a fresh label. */
  label: string
  home: string
  function?: string | null
  preview?: string | null
  icon0?: string | null
  contentIcons: Record<string, string>
  functionIcons: Record<string, { icon: string; glow: string }>
  colors: ThemePackageColors
  animation?: { width: number; height: number; wait: number; frames: string[] } | null
}
export type ThemeBuildResult = { path: string; size: number; contentId: string; title: string; frames: number; sceneBytes: number; files: number }

/**
 * PS4 receiver `themes-v1`: `list_console_themes({ host, port })` → ConsoleThemes,
 * `apply_console_theme` / `remove_console_theme({ host, port, contentId })` → message.
 */
export type InstalledTheme = { contentId: string; title: string }
export type ConsoleThemes = { themes: InstalledTheme[]; activeContentId: string | null; truncated: boolean }
