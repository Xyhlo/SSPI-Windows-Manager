import type { ConsoleKind, DeliveryJob, Game, Ps4Transport, Settings } from "../types"

/* Homebrew title IDs (SSHB00001, PPSA99008, ...) don't say which console
   they were built for, so the source's platform tag is remembered here and
   kept between launches for the download list. */
const HOMEBREW_KEY = "sspi.homebrewPlatforms"
const homebrewPlatforms = (() => {
  try { return new Map<string, ConsoleKind>(Object.entries(JSON.parse(localStorage.getItem(HOMEBREW_KEY) || "{}"))) } catch { return new Map<string, ConsoleKind>() }
})()

export function rememberHomebrew(games: Pick<Game, "titleId" | "homebrew">[]) {
  let changed = false
  for (const game of games) {
    const platform = game.homebrew?.platform
    const id = game.titleId.toUpperCase()
    if ((platform === "ps4" || platform === "ps5") && homebrewPlatforms.get(id) !== platform) { homebrewPlatforms.set(id, platform); changed = true }
  }
  if (!changed) return
  try { localStorage.setItem(HOMEBREW_KEY, JSON.stringify(Object.fromEntries([...homebrewPlatforms].slice(-500)))) } catch { /* the tags are rebuilt by the next search */ }
}

export function isHomebrew(titleId?: string): boolean {
  return homebrewPlatforms.has((titleId || "").toUpperCase())
}

export function platformOf(titleId?: string): ConsoleKind | undefined {
  const tagged = homebrewPlatforms.get((titleId || "").toUpperCase())
  if (tagged) return tagged
  if (/^(?:CUSA|SLUS|SLES|SCUS|SCES|SLPS|SLPM|SCPS|SCAJ|SLAJ|SLKA|SLKS|SCKA)\d{5}$/i.test(titleId || "")) return "ps4"
  if (/^PPSA/i.test(titleId || "")) return "ps5"
  return undefined
}

export function packageTitle(settings: Pick<Settings, "packageDumps">, titleId: string): boolean {
  return settings.packageDumps && !isHomebrew(titleId) && platformOf(titleId) === "ps5"
}

export function jobTarget(job: Pick<DeliveryJob, "target">): ConsoleKind {
  return job.target === "ps4" ? "ps4" : "ps5"
}

export function consoleLabel(target: ConsoleKind): "PS4" | "PS5" {
  return target === "ps4" ? "PS4" : "PS5"
}

export function consoleAddress(settings: Settings, target: ConsoleKind): string {
  const host = target === "ps4" ? settings.ps4Host.trim() : settings.ps5Host.trim()
  if (!host) return ""
  const port = target === "ps4"
    ? settings.ps4Transport === "receiver" ? settings.ps4ReceiverPort : settings.ps4FtpPort
    : settings.ps5Port
  return `${host}:${port}`
}

export function ps4TransportLabel(transport: Ps4Transport): "Receiver payload" | "SSPI inbox" {
  return transport === "receiver" ? "Receiver payload" : "SSPI inbox"
}

export function sendBlockReason(
  target: ConsoleKind,
  item: { titleId?: string; content?: "pkg" | "archive" | "dump"; backport?: boolean; homebrew?: { runsOn?: ConsoleKind[]; format?: string } },
): string | undefined {
  const runsOn = item.homebrew?.runsOn || []
  if (runsOn.length && !runsOn.includes(target)) return `This homebrew app runs on ${runsOn.map(consoleLabel).join(" and ")} only.`
  if (item.homebrew?.format === "folder" && target === "ps4") return "Homebrew folders install on a PS5 only."
  if (item.homebrew) return undefined
  if (target === "ps5") return undefined
  if (/^PPSA/i.test(item.titleId || "")) return "PS5 games can't be installed on a PS4."
  if (item.backport) return "Backports apply to PS5 games only."
  if (item.content === "dump") return "PS4 delivery takes PKG files. Game folders can only be sent to a PS5."
  return undefined
}
