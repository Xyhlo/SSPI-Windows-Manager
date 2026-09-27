import type { ConsoleKind, DeliveryJob, Ps4Transport, Settings } from "../types"

export function platformOf(titleId?: string): ConsoleKind | undefined {
  if (/^CUSA/i.test(titleId || "")) return "ps4"
  if (/^PPSA/i.test(titleId || "")) return "ps5"
  return undefined
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
  item: { titleId?: string; content?: "pkg" | "archive" | "dump"; backport?: boolean },
): string | undefined {
  if (target === "ps5") return undefined
  if (/^PPSA/i.test(item.titleId || "")) return "PS5 games can't be installed on a PS4."
  if (item.backport) return "Backports apply to PS5 games only."
  if (item.content === "dump") return "PS4 delivery takes PKG files. Game folders can only be sent to a PS5."
  return undefined
}
