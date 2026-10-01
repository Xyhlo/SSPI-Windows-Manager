import type { ConsoleKind } from "../types"
import type { ConsoleLibrarySnapshot, ConsoleProbe, ConsoleProcess, ConsoleSystemInfo, DebugService, KernelLog, LogFile, LogFileList, LogTail, PayloadEntry, ProcessList } from "./console-types"

const escapeXml = (value: string) => value.replace(/[&<>"']/g, char => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[char]!)

/** Portrait box art for the preview's cases (kept here so the file has no runtime imports). */
function demoBoxArt(title: string, from: string, to: string, accent: string) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="720" height="1000" viewBox="0 0 720 1000"><defs><linearGradient id="bg" x2="1" y2="1"><stop stop-color="${from}"/><stop offset="1" stop-color="${to}"/></linearGradient><radialGradient id="glow"><stop stop-color="${accent}" stop-opacity=".72"/><stop offset="1" stop-color="${accent}" stop-opacity="0"/></radialGradient></defs><rect width="720" height="1000" fill="url(#bg)"/><circle cx="560" cy="245" r="340" fill="url(#glow)"/><path d="M-80 675L640 85M20 820L760 210" fill="none" stroke="${accent}" stroke-opacity=".45" stroke-width="3"/><circle cx="540" cy="280" r="180" fill="none" stroke="white" stroke-opacity=".28" stroke-width="2"/><circle cx="540" cy="280" r="120" fill="none" stroke="white" stroke-opacity=".15" stroke-width="18"/><path d="M95 690h530" stroke="white" stroke-opacity=".34"/><text x="62" y="775" fill="white" font-family="Arial,sans-serif" font-weight="800" font-size="58" letter-spacing="2">${escapeXml(title)}</text></svg>`
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}

export function demoIcon(title: string, from: string, to: string, accent: string) {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512"><defs><linearGradient id="bg" x2="1" y2="1"><stop stop-color="${from}"/><stop offset="1" stop-color="${to}"/></linearGradient><radialGradient id="glow"><stop stop-color="${accent}" stop-opacity=".7"/><stop offset="1" stop-color="${accent}" stop-opacity="0"/></radialGradient></defs><rect width="512" height="512" fill="url(#bg)"/><circle cx="354" cy="174" r="230" fill="url(#glow)"/><path d="M-30 402 440 32M46 482 512 102" fill="none" stroke="${accent}" stroke-opacity=".5" stroke-width="3"/><circle cx="348" cy="178" r="118" fill="none" stroke="white" stroke-opacity=".27" stroke-width="2"/><path d="M55 350h402" stroke="white" stroke-opacity=".34"/><text x="44" y="468" fill="white" fill-opacity=".92" font-family="Arial,sans-serif" font-weight="800" font-size="30" letter-spacing="1">${escapeXml(title.toUpperCase())}</text></svg>`
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}

// Matches what receivers 1.0.8 / 1.0.6 advertise; neither offers a live home-screen refresh.
export const demoCapabilities = ["installed-library-v1", "title-icons-v1", "system-info-v1", "theme-install-v1", "themes-v1", "diagnostics-v1", "process-control-v1"]
const capabilities = demoCapabilities

export const demoProbes: ConsoleProbe[] = [
  {
    target: "ps5", host: "192.0.2.15",
    receiver: { state: "online", port: 9114, version: "1.0.8", expectedVersion: "1.0.8", capabilities: [...capabilities], latencyMs: 8 },
    checkedAt: 1_782_516_000_000,
  },
  {
    target: "ps4", host: "192.0.2.16",
    receiver: { state: "outdated", port: 9114, version: "1.0.5", expectedVersion: "1.0.6", capabilities: [], latencyMs: 12 },
    checkedAt: 1_782_516_000_000,
  },
]

type DemoTitle = { titleId: string; name: string; version: string; baseVersion: string; updateVersion: string; requiredFirmware: string; customIcon?: boolean; platform?: ConsoleKind; accent: string; from: string; to: string }

const ps5Titles: DemoTitle[] = [
  { titleId: "PPSA00001", name: "Signal Drift", version: "1.08", baseVersion: "1.00", updateVersion: "1.08", requiredFirmware: "8.00", accent: "#65d7c3", from: "#071e2a", to: "#16464d" },
  { titleId: "PPSA00002", name: "Ashfall Protocol", version: "1.04", baseVersion: "1.00", updateVersion: "1.04", requiredFirmware: "7.60", accent: "#ffad4f", from: "#21100b", to: "#783b20", customIcon: true },
  { titleId: "PPSA00003", name: "Glass Horizon", version: "1.12", baseVersion: "1.00", updateVersion: "1.12", requiredFirmware: "9.00", accent: "#6acfff", from: "#071925", to: "#195279" },
  { titleId: "PPSA00004", name: "Quiet Orbit", version: "1.02", baseVersion: "1.00", updateVersion: "1.02", requiredFirmware: "8.20", accent: "#b89cff", from: "#141126", to: "#423568" },
  { titleId: "PPSA00005", name: "Copper Sky", version: "1.06", baseVersion: "1.00", updateVersion: "1.06", requiredFirmware: "7.00", accent: "#f08359", from: "#22100d", to: "#693526" },
  { titleId: "PPSA00006", name: "Tidal Memory", version: "1.01", baseVersion: "1.00", updateVersion: "1.01", requiredFirmware: "9.00", accent: "#54cbd2", from: "#061d2a", to: "#176274" },
  { titleId: "PPSA00007", name: "Night Signal", version: "1.03", baseVersion: "1.00", updateVersion: "1.03", requiredFirmware: "8.00", accent: "#df7bb8", from: "#1e0c1d", to: "#63234e" },
  { titleId: "PPSA00008", name: "Bright Divide", version: "1.00", baseVersion: "1.00", updateVersion: "1.00", requiredFirmware: "8.50", accent: "#e4d05b", from: "#1c1a07", to: "#68601a" },
  { titleId: "CUSA00009", name: "Midnight Circuit", version: "1.14", baseVersion: "1.00", updateVersion: "1.14", requiredFirmware: "5.05", accent: "#7e9fff", from: "#090d24", to: "#1d2d69", platform: "ps4" },
  { titleId: "CUSA00010", name: "Solar Run", version: "1.07", baseVersion: "1.00", updateVersion: "1.07", requiredFirmware: "6.72", accent: "#ffd15a", from: "#211407", to: "#76501d", platform: "ps4", customIcon: true },
]

const ps4Titles: DemoTitle[] = [
  { titleId: "CUSA00011", name: "Vector Run", version: "1.05", baseVersion: "1.00", updateVersion: "1.05", requiredFirmware: "5.05", accent: "#5ce0b2", from: "#071d19", to: "#155a49" },
  { titleId: "CUSA00012", name: "Arctic Signal", version: "1.03", baseVersion: "1.00", updateVersion: "1.03", requiredFirmware: "6.72", accent: "#74d9ff", from: "#071b2a", to: "#195080" },
  { titleId: "CUSA00013", name: "Paper Lanterns", version: "1.00", baseVersion: "1.00", updateVersion: "1.00", requiredFirmware: "5.05", accent: "#f9a86e", from: "#26130d", to: "#75432c" },
  { titleId: "CUSA00014", name: "Stone & Sky", version: "1.08", baseVersion: "1.00", updateVersion: "1.08", requiredFirmware: "7.02", accent: "#bdadff", from: "#17152a", to: "#45406f" },
  { titleId: "CUSA00015", name: "Last Meridian", version: "1.02", baseVersion: "1.00", updateVersion: "1.02", requiredFirmware: "6.00", accent: "#e68192", from: "#260f17", to: "#752d3a" },
  { titleId: "CUSA00016", name: "Deep Current", version: "1.12", baseVersion: "1.00", updateVersion: "1.12", requiredFirmware: "7.00", accent: "#48cad4", from: "#071b26", to: "#145c6a" },
  { titleId: "CUSA00017", name: "Amber Sky", version: "1.01", baseVersion: "1.00", updateVersion: "1.01", requiredFirmware: "5.50", accent: "#ffce63", from: "#271906", to: "#79531b" },
  { titleId: "CUSA00018", name: "Soft Static", version: "1.04", baseVersion: "1.00", updateVersion: "1.04", requiredFirmware: "6.72", accent: "#9dafed", from: "#11182c", to: "#364a72" },
  { titleId: "CUSA00019", name: "Afterimage", version: "1.06", baseVersion: "1.00", updateVersion: "1.06", requiredFirmware: "7.55", accent: "#e78aba", from: "#27101f", to: "#743a60" },
  { titleId: "CUSA00020", name: "Northbound", version: "1.00", baseVersion: "1.00", updateVersion: "1.00", requiredFirmware: "5.05", accent: "#83d6de", from: "#071e24", to: "#23555d" },
  { titleId: "CUSA00021", name: "Blue Hour", version: "1.09", baseVersion: "1.00", updateVersion: "1.09", requiredFirmware: "6.72", accent: "#89a7ff", from: "#08152d", to: "#263c7b" },
  { titleId: "CUSA00022", name: "Field Notes", version: "1.02", baseVersion: "1.00", updateVersion: "1.02", requiredFirmware: "5.50", accent: "#c2c878", from: "#181d0c", to: "#505b26" },
]

export function demoLibrary(target: ConsoleKind): ConsoleLibrarySnapshot {
  const source = target === "ps5" ? ps5Titles : ps4Titles
  return {
    target,
    entries: source.map(title => ({
      titleId: title.titleId, name: title.name, version: title.version, baseVersion: title.baseVersion,
      updateVersion: title.updateVersion, requiredFirmware: title.requiredFirmware, customIcon: title.customIcon || false,
      platform: title.platform || target,
      sources: target === "ps5" ? ["appdb", "appmeta"] : [],
      icon: demoIcon(title.name, title.from, title.to, title.accent),
      cover: demoBoxArt(title.name.toUpperCase(), title.from, title.to, title.accent),
    })),
    complete: true, truncated: false, errors: [], metadataWarnings: [],
    diagnostics: target === "ps5" ? { appdb: "ok", counts: { appdb: source.length, app: 0, appmeta: source.length, shadowmount: 0 }, skipped: [] } : null,
  }
}

export function demoSystemInfo(target: ConsoleKind): ConsoleSystemInfo {
  // Mirrors what receivers 1.0.8 (PS5) and 1.0.6 (PS4) read; the PS4 has no model call.
  return target === "ps5" ? {
    target, receiverVersion: "1.0.8", firmware: "4.03", sdkVersion: null, model: "CFI-1015A", consoleName: "ps5-living-room", uptimeSeconds: 197_520,
    cpuTempC: 54, socTempC: 61,
    storage: [
      { label: "Internal", path: "/user", totalBytes: 825_439_027_200, freeBytes: 412_316_860_416 },
      { label: "Extended 1", path: "/mnt/ext1", totalBytes: 1_000_204_886_016, freeBytes: 621_234_626_560 },
      { label: "USB 0", path: "/mnt/usb0", totalBytes: 2_000_398_934_016, freeBytes: 1_210_122_072_064 },
    ],
    memory: { totalBytes: 17_179_869_184, freeBytes: 2_791_728_742 },
    network: { ip: "192.0.2.15", mac: "a8:47:4a:00:00:15" }, runningTitleId: "PPSA00001",
    mounts: [
      { from: "/dev/ssd0.user", on: "/user", type: "ufs", readOnly: false, totalBytes: 825_439_027_200, freeBytes: 412_316_860_416 },
      { from: "/dev/ssd0.system", on: "/system", type: "exfatfs", readOnly: true, totalBytes: 2_147_483_648, freeBytes: 402_653_184 },
      { from: "/dev/nvme0.ext", on: "/mnt/ext1", type: "ufs", readOnly: false, totalBytes: 1_000_204_886_016, freeBytes: 621_234_626_560 },
      { from: "/dev/da0s1", on: "/mnt/usb0", type: "exfatfs", readOnly: false, totalBytes: 2_000_398_934_016, freeBytes: 1_210_122_072_064 },
      { from: "/data/homebrew/PPSA00001", on: "/system_ex/app/PPSA00001", type: "nullfs", readOnly: true, totalBytes: 825_439_027_200, freeBytes: 412_316_860_416 },
      { from: "devfs", on: "/dev", type: "devfs", readOnly: false, totalBytes: 0, freeBytes: 0 },
    ],
    capabilities: [...capabilities], extras: { processCount: "143", cpuFrequencyMhz: "3500", loadAverage: "1.84 1.62 1.40", networkInterface: "eth0", firmwareRaw: "0x04030000" },
  } : {
    target, receiverVersion: "1.0.6", firmware: "9.00", sdkVersion: null, model: null, consoleName: null, uptimeSeconds: 7_260,
    cpuTempC: 58, socTempC: null,
    storage: [{ label: "Internal", path: "/user", totalBytes: 858_993_459_200, freeBytes: 298_634_444_800 }],
    memory: { totalBytes: 8_589_934_592, freeBytes: 1_020_054_732 }, network: { ip: "192.0.2.16", mac: null }, runningTitleId: null,
    mounts: [{ from: "/dev/da0x13.crypt", on: "/user", type: "ufs", readOnly: false, totalBytes: 858_993_459_200, freeBytes: 298_634_444_800 }],
    capabilities: [...capabilities], extras: { processCount: "96", cpuFrequencyMhz: "1600" },
  }
}

export const demoPayloads: PayloadEntry[] = [
  { id: "builtin:ps5-receiver", name: "PS5 receiver", fileName: "receiver-ps5.elf", path: "", size: 1_086_888, elf: { class: "ELF64", endian: "LE", kind: "DYN", machine: "x86-64", entry: "0x5a1c", segments: 4, loadable: 3, loadableBytes: 948_578 }, sha256: "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5", target: "ps5", builtin: true, addedAt: 1_782_000_000_000, lastSentAt: 1_782_516_100_000, lastResult: "Receiver verified" },
  { id: "builtin:ps4-receiver", name: "PS4 receiver", fileName: "receiver-ps4.bin", path: "", size: 119_936, elf: { class: "ELF64", endian: "LE", kind: "DYN", machine: "x86-64", entry: "0x100", segments: 3, loadable: 2, loadableBytes: 945_492 }, sha256: "b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6", target: "ps4", builtin: true, addedAt: 1_782_000_000_000 },
  { id: "demo-payload-ftp", name: "FTP server", fileName: "ftp-server.elf", path: "preview/ftp-server.elf", size: 327_680, elf: { class: "ELF64", endian: "LE", kind: "EXEC", machine: "x86-64", entry: "0x401000", segments: 5, loadable: 2, loadableBytes: 344_064 }, sha256: "c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7", target: "any", builtin: false, addedAt: 1_782_200_000_000, notes: "Example payload for offline preview" },
  { id: "demo-payload-kernel", name: "Kernel patches", fileName: "kernel-patches.bin", path: "preview/kernel-patches.bin", size: 524_288, sha256: "d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8", target: "ps5", builtin: false, addedAt: 1_782_300_000_000, lastSentAt: 1_782_516_200_000, lastResult: "Sent 524 KB" },
  { id: "demo-payload-homebrew", name: "Homebrew launcher", fileName: "homebrew-launcher.elf", path: "preview/homebrew-launcher.elf", size: 1_048_576, elf: { class: "ELF64", endian: "LE", kind: "DYN", machine: "x86-64", entry: "0x2f40", segments: 6, loadable: 3, loadableBytes: 1_101_824 }, sha256: "e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9", target: "ps4", builtin: false, addedAt: 1_782_400_000_000, notes: "" },
]

/* ---------------------------------------------------------------- diagnostics preview (receivers 1.0.8 / 1.0.6) */

// Relative to now, so file ages and process run times read naturally in the preview.
const PREVIEW_NOW = Math.floor(Date.now() / 1000)

const ps5Log = [
  "---<<BOOT>>---",
  "Copyright (c) 1992-2019 The FreeBSD Project.",
  "[SceShellCore] boot: initialization done",
  "[elfldr] listening on port 9021",
  "[kstuff] kernel patches applied",
  "[etaHEN] etaHEN 2.x loaded, klog server on 9081",
  "[ShadowMount] mounted 4 games from /data/homebrew",
  "Fatal trap 12: page fault while in kernel mode",
  "cpuid = 5; apic id = 05",
  "fault virtual address\t= 0x28",
  "fault code\t\t= supervisor read data, page not present",
  "instruction pointer\t= 0x20:0xffffffff82b4c1a0",
  "current process\t\t= 97 (SceShellUI)",
  "trap number\t\t= 12",
  "panic: page fault",
  "Uptime: 3h41m12s",
  "---<<BOOT>>---",
  "[SceShellCore] boot: initialization done",
  "[elfldr] listening on port 9021",
  "[sspi.elf] SSPI receiver 1.0.8 ready on 9114",
  "[SceShellUI] warning: notification queue timed out",
  "[AppInstUtil] sceAppInstUtilInstallByPackage failed: 0x80b2116f",
  "pid 412 (eboot.bin), jid 0, uid 0: exited on signal 11 (core dumped)",
  "[sspi.elf] transfer PPSA00001 verified, 24.56 GB",
]
const ps4Log = [
  "[GoldHEN] v2.4 loaded",
  "[GoldHEN] klog server listening on 3232",
  "[GoldHEN] BinLoader listening on 9090",
  "[sspi-receiver] SSPI PS4 receiver 1.0.6 ready on 9114",
  "[BGFT] download task 12 started",
  "[BGFT] warning: retrying after network timeout",
]

export const demoKlogLines = [
  "[SceShellUI] library refresh",
  "[sspi.elf] system info requested",
  "[etaHEN] toolbox: 3 plugins active",
  "[SceNet] eth0 link up 1000 Mbps",
  "[SceShellUI] warning: thumbnail cache miss",
]

export function demoKernelLog(target: ConsoleKind): KernelLog {
  const text = (target === "ps5" ? ps5Log : ps4Log).join("\n") + "\n"
  return target === "ps5"
    ? { source: "msgbuf", busy: false, dropped: false, bytes: text.length, text, capturedAt: PREVIEW_NOW * 1000 }
    : { source: "klog", busy: true, dropped: false, bytes: text.length, text, capturedAt: PREVIEW_NOW * 1000 }
}

const proc = (pid: number, name: string, extra: Partial<ConsoleProcess>): ConsoleProcess => ({
  pid, ppid: 1, name, state: "sleeping", uid: 0, titleId: null, appType: null, authId: null,
  rssBytes: 12_000_000, vmBytes: 90_000_000, threads: 4, startedAt: PREVIEW_NOW - 13_000, cpuMs: 2_400, ...extra,
})

export function demoProcesses(target: ConsoleKind): ProcessList {
  const processes = target === "ps5" ? [
    proc(1, "init", { ppid: 0, threads: 1, rssBytes: 1_200_000 }),
    proc(41, "SceSysCore", { titleId: "NPXS40000", authId: "3800000000000007", threads: 38, rssBytes: 96_000_000 }),
    proc(57, "SceShellCore", { titleId: "NPXS40001", authId: "3800000000000010", threads: 112, rssBytes: 410_000_000, cpuMs: 188_000 }),
    proc(97, "SceShellUI", { titleId: "NPXS40087", authId: "3800000000000011", threads: 164, rssBytes: 1_120_000_000, cpuMs: 912_000 }),
    proc(203, "elfldr.elf", { authId: "4800000000000007", threads: 3, rssBytes: 6_400_000 }),
    proc(211, "kstuff.elf", { ppid: 203, authId: "4800000000000007", threads: 2, rssBytes: 3_100_000, control: "payload" }),
    proc(224, "etaHEN", { ppid: 203, authId: "4801000000000013", threads: 21, rssBytes: 58_000_000, cpuMs: 41_000, control: "payload" }),
    proc(236, "shadowmount.elf", { ppid: 203, authId: "4800000000000007", threads: 5, rssBytes: 9_800_000, control: "payload" }),
    proc(240, "ftpsrv.elf", { ppid: 203, authId: "4801000000000013", threads: 3, rssBytes: 4_200_000, control: "payload" }),
    proc(248, "sspi.elf", { ppid: 203, authId: "4801000000000013", threads: 9, rssBytes: 22_000_000, cpuMs: 12_300 }),
    proc(412, "eboot.bin", { ppid: 41, titleId: "PPSA00001", appType: 1, authId: "5000000000000001", state: "running", threads: 71, rssBytes: 5_600_000_000, vmBytes: 11_200_000_000, cpuMs: 2_910_000, control: "app" }),
  ] : [
    proc(1, "init", { ppid: 0, threads: 1 }),
    proc(33, "SceShellCore", { titleId: "NPXS20001", threads: 88, rssBytes: 180_000_000, cpuMs: 99_000 }),
    proc(54, "SceShellUI", { titleId: "NPXS20000", threads: 120, rssBytes: 640_000_000, cpuMs: 402_000 }),
    proc(77, "GoldHEN", { threads: 6, rssBytes: 8_000_000 }),
    proc(140, "eboot.bin", { ppid: 33, titleId: "CUSA00001", state: "running", threads: 44, rssBytes: 2_100_000_000, cpuMs: 820_000, control: "app" }),
  ]
  return { processes, truncated: false, capturedAt: PREVIEW_NOW * 1000 }
}

const logFile = (path: string, size: number, age: number, kind: LogFile["kind"] = "log"): LogFile => ({ path, size, modified: PREVIEW_NOW - age, kind })

export function demoLogFiles(target: ConsoleKind): LogFileList {
  const files = target === "ps5" ? [
    logFile("/data/etaHEN/etaHEN.log", 48_211, 40), logFile("/data/etaHEN/config.ini", 612, 86_400, "config"),
    logFile("/data/shadowmount/shadowmount.log", 9_840, 600), logFile("/data/shadowmount/manual.lst", 212, 3_600, "config"),
    logFile("/data/SSPI/trace.log", 3_112, 20), logFile("/data/elfldr/elfldr.log", 1_004, 13_000),
    logFile("/user/data/sce_coredumps/eboot.bin-412.prosperodmp", 184_549_376, 900, "crash"),
  ] : [
    logFile("/data/GoldHEN/goldhen.log", 22_410, 120), logFile("/data/GoldHEN/plugins.ini", 380, 604_800, "config"),
    logFile("/user/data/sspi-receiver/receiver.log", 5_620, 30),
    logFile("/user/data/orbiscore-1782500000-0x0000004c-eboot.bin.orbisdmp", 96_468_992, 7_200, "crash"),
  ]
  return { files, truncated: false, incomplete: false, capturedAt: PREVIEW_NOW * 1000 }
}

export function demoLogTail(path: string): LogTail {
  const lines = path.endsWith(".ini")
    ? ["[Settings]", "Klog=1", "FTP=1", "Toolbox=1", "DisableUpdates=1"]
    : Array.from({ length: 40 }, (_, i) => `[${String(i).padStart(2, "0")}] ${i === 31 ? "error: mount of /data/homebrew/PPSA00009 failed (22)" : i % 9 === 4 ? "warning: slow response from shell" : "status ok"}`)
  const text = lines.join("\n") + "\n"
  return { path, size: text.length, offset: 0, bytes: text.length, modified: PREVIEW_NOW - 40, text }
}

export function demoServices(target: ConsoleKind): DebugService[] {
  return target === "ps5" ? [
    { port: 3232, name: "klogsrv kernel log", kind: "klog", open: false },
    { port: 9081, name: "etaHEN kernel log", kind: "klog", open: true, latencyMs: 5 },
    { port: 1337, name: "etaHEN FTP", kind: "ftp", open: true, latencyMs: 6 },
    { port: 2121, name: "ftpsrv FTP", kind: "ftp", open: false },
  ] : [
    { port: 3232, name: "GoldHEN kernel log", kind: "klog", open: true, latencyMs: 6 },
    { port: 2121, name: "GoldHEN FTP", kind: "ftp", open: true, latencyMs: 8 },
    { port: 744, name: "ps4debug", kind: "debugger", open: false },
    { port: 12800, name: "Remote Package Installer", kind: "installer", open: false },
  ]
}
