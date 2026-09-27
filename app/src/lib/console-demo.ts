import type { ConsoleKind } from "../types"
import type { ConsoleLibrarySnapshot, ConsoleProbe, ConsoleSystemInfo, PayloadEntry } from "./console-types"

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

// Matches what receiver 1.0.6 / 1.0.4 advertise; neither offers a live home-screen refresh.
export const demoCapabilities = ["installed-library-v1", "title-icons-v1", "system-info-v1", "theme-install-v1", "themes-v1"]
const capabilities = demoCapabilities

export const demoProbes: ConsoleProbe[] = [
  {
    target: "ps5", host: "192.168.0.215",
    receiver: { state: "online", port: 9114, version: "1.0.6", expectedVersion: "1.0.6", capabilities: [...capabilities], latencyMs: 8 },
    checkedAt: 1_782_516_000_000,
  },
  {
    target: "ps4", host: "192.168.0.216",
    receiver: { state: "outdated", port: 9114, version: "1.0.3", expectedVersion: "1.0.4", capabilities: [], latencyMs: 12 },
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
      icon: demoIcon(title.name, title.from, title.to, title.accent),
      cover: demoBoxArt(title.name.toUpperCase(), title.from, title.to, title.accent),
    })),
    complete: true, truncated: false, errors: [], metadataWarnings: [],
  }
}

export function demoSystemInfo(target: ConsoleKind): ConsoleSystemInfo {
  // Mirrors what the receivers read: firmware and storage on both, uptime and host name on the PS5.
  return target === "ps5" ? {
    target, receiverVersion: "1.0.6", firmware: "11.00", sdkVersion: "0x11008001", model: null, consoleName: "ps5-living-room", uptimeSeconds: 197_520,
    cpuTempC: null, socTempC: null,
    storage: [
      { label: "Console storage", path: "/user", totalBytes: 825_439_027_200, freeBytes: 412_316_860_416 },
      { label: "M.2 storage", path: "/mnt/ext1", totalBytes: 1_000_204_886_016, freeBytes: 621_234_626_560 },
    ],
    memory: null, network: null, runningTitleId: null,
    capabilities: [...capabilities], extras: {},
  } : {
    target, receiverVersion: "1.0.4", firmware: "9.00", sdkVersion: "0x09000001", model: null, consoleName: null, uptimeSeconds: null,
    cpuTempC: null, socTempC: null,
    storage: [{ label: "Console storage", path: "/user", totalBytes: 858_993_459_200, freeBytes: 298_634_444_800 }],
    memory: null, network: null, runningTitleId: null,
    capabilities: [...capabilities], extras: {},
  }
}

export const demoPayloads: PayloadEntry[] = [
  { id: "builtin:ps5-receiver", name: "PS5 receiver", fileName: "receiver-ps5.elf", path: "", size: 862_208, sha256: "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5", target: "ps5", builtin: true, addedAt: 1_782_000_000_000, lastSentAt: 1_782_516_100_000, lastResult: "Receiver verified" },
  { id: "builtin:ps4-receiver", name: "PS4 receiver", fileName: "receiver-ps4.bin", path: "", size: 684_032, sha256: "b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6", target: "ps4", builtin: true, addedAt: 1_782_000_000_000 },
  { id: "demo-payload-ftp", name: "FTP server", fileName: "ftp-server.elf", path: "preview/ftp-server.elf", size: 327_680, sha256: "c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7", target: "any", builtin: false, addedAt: 1_782_200_000_000, notes: "Example payload for offline preview" },
  { id: "demo-payload-kernel", name: "Kernel patches", fileName: "kernel-patches.bin", path: "preview/kernel-patches.bin", size: 524_288, sha256: "d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8d8", target: "ps5", builtin: false, addedAt: 1_782_300_000_000, lastSentAt: 1_782_516_200_000, lastResult: "Sent 524 KB" },
  { id: "demo-payload-homebrew", name: "Homebrew launcher", fileName: "homebrew-launcher.elf", path: "preview/homebrew-launcher.elf", size: 1_048_576, sha256: "e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9e9", target: "ps4", builtin: false, addedAt: 1_782_400_000_000, notes: "" },
]
