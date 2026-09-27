import type { ConsoleKind, Settings } from "../types"
import type { ConsoleProbe } from "./console-types"

export function receiverEndpoint(settings: Settings, target: ConsoleKind) {
  return target === "ps5"
    ? { host: settings.ps5Host.trim(), port: settings.ps5Port }
    : { host: settings.ps4Host.trim(), port: settings.ps4ReceiverPort }
}

export function loaderEndpoint(settings: Settings, target: ConsoleKind) {
  return target === "ps5"
    ? { host: settings.ps5Host.trim(), port: (settings as Settings & { ps5LoaderPort?: number }).ps5LoaderPort ?? 9021 }
    : { host: settings.ps4Host.trim(), port: settings.ps4LoaderPort || 9090 }
}

export function hasCapability(probe: ConsoleProbe | undefined, name: string) {
  return probe?.receiver.capabilities.includes(name) ?? false
}

export function probeFor(probes: ConsoleProbe[] | undefined, target: ConsoleKind) {
  return probes?.find(probe => probe.target === target)
}

export function receiverSentence(probe: ConsoleProbe | undefined) {
  if (!probe) return "Add the console's address in Options, Consoles"
  const { state, version, port, message } = probe.receiver
  switch (state) {
    case "online": return `Receiver${version ? ` ${version}` : ""} is running`
    case "outdated": return `Receiver${version ? ` ${version}` : ""} is loaded; reload it to use this`
    case "offline": return probe.host ? `Nothing answered at ${probe.host}:${port}` : "Add the console's address in Options, Consoles"
    case "unconfigured": return "Add the console's address in Options, Consoles"
    case "error": return message?.trim() || (probe.host ? `Couldn't check the receiver at ${probe.host}:${port}` : "The receiver check failed")
  }
}

export function formatUptime(seconds: number | null | undefined) {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "Not reported"
  const totalMinutes = Math.floor(seconds / 60)
  const minutes = totalMinutes % 60
  const hours = Math.floor(totalMinutes / 60) % 24
  const days = Math.floor(totalMinutes / 1440)
  if (days) return `${days} d${hours ? ` ${hours} h` : ""}`
  if (hours) return `${hours} h${minutes ? ` ${minutes} min` : ""}`
  return `${totalMinutes} min`
}

export function formatTemp(c: number | null | undefined) {
  if (c == null || !Number.isFinite(c)) return "Not reported"
  const value = Math.round(c * 10) / 10
  return `${Number.isInteger(value) ? value.toFixed(0) : value} °C`
}
