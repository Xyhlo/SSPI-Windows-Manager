/* Package rows for the details page: resolved candidates grouped by kind, version and release, with their hosts as mirrors. */
import type { PackageCandidate } from "@/types"
import { compareVersions } from "./format"
import { packageKind } from "./package-selection"

export type PackageGroup = {
  key: string
  kind: string
  title: string
  version: string
  firmware: string
  hosts: Map<string, PackageCandidate[]>
}

const NAMES: Record<string, string> = { base: "Base game", update: "Game update", dlc: "DLC", backport: "Backport", exfat: "exFAT image" }
const ORDER = ["base", "update", "dlc", "backport", "exfat"]

export function structurePackages(packages: PackageCandidate[]): PackageGroup[] {
  const groups = new Map<string, PackageGroup>()
  for (const candidate of packages) {
    const kind = packageKind(candidate)
    const key = `${kind}|${candidate.version || ""}|${candidate.groupId || candidate.label || candidate.url}`
    if (!groups.has(key)) {
      groups.set(key, { key, kind, title: NAMES[kind] || candidate.label || "Package", version: candidate.version || "", firmware: candidate.firmware || "", hosts: new Map() })
    }
    const host = candidate.hoster || "Host"
    const group = groups.get(key)!
    group.hosts.set(host, [...(group.hosts.get(host) || []), candidate])
  }
  return [...groups.values()].sort((left, right) => {
    const a = ORDER.indexOf(left.kind), b = ORDER.indexOf(right.kind)
    return (a < 0 ? 9 : a) - (b < 0 ? 9 : b) || compareVersions(right.version, left.version) || left.title.localeCompare(right.title)
  })
}

export const groupHosts = (group: PackageGroup) => [...group.hosts.keys()]

export function groupParts(group: PackageGroup, host?: string) {
  const hosts = groupHosts(group)
  const chosen = host && hosts.includes(host) ? host : hosts[0]
  return [...(group.hosts.get(chosen) || [])].sort((a, b) => (a.archivePartNumber || 0) - (b.archivePartNumber || 0))
}

export function formatPackageSize(bytes: number) {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(2)} GB`
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`
  return `${bytes.toLocaleString()} B`
}

export function partsSize(parts: PackageCandidate[]) {
  return parts.every(part => (part.expectedSize || 0) > 0) ? parts.reduce((total, part) => total + (part.expectedSize || 0), 0) : null
}

export function archiveLabel(parts: PackageCandidate[]) {
  const format = (parts.find(item => item.archiveFormatHint && item.archiveFormatHint !== "unknown")?.archiveFormatHint
    || (parts.some(item => item.url.toLowerCase().includes(".rar") || (item.archiveFileName || "").toLowerCase().includes(".rar")) ? "rar" : "")).toUpperCase()
  const count = parts[0]?.archivePartCount || parts.length
  const incomplete = parts.some(item => item.diagnostics?.some(note => note.includes("incomplete") || note.includes("missing Part.")))
  if (parts.length > 1 || parts[0]?.archivePartNumber) return `${format || "RAR"} archive, ${parts.length}${count && count !== parts.length ? ` of ${count}` : ""} ${parts.length === 1 ? "part" : "parts"}${incomplete ? ", incomplete" : ""}`
  if (format) return `${format} archive`
  const access = (parts[0]?.accessType || "").toLowerCase()
  if (access === "direct" || /\.pkg(?:$|[?#])/i.test(parts[0]?.url || "")) return "Single PKG"
  return "RAR archive"
}

export const isSevenZip = (parts: PackageCandidate[]) => archiveLabel(parts).startsWith("7Z")
