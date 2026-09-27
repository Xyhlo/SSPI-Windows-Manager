import { displayText } from "./display"

export const KB = 1024
export const MB = KB * 1024
export const GB = MB * 1024

export function fmtBytes(bytes: number | null | undefined, digits?: number) {
  if (bytes == null || !Number.isFinite(bytes) || bytes <= 0) return "0 B"
  const units = ["B", "KB", "MB", "GB", "TB"]
  const unit = Math.min(4, Math.floor(Math.log(bytes) / Math.log(1024)))
  const value = bytes / 1024 ** unit
  const d = digits ?? (unit === 0 ? 0 : value >= 100 ? 0 : 1)
  return `${value.toFixed(d)} ${units[unit]}`
}

export const fmtSpeed = (bps: number) => `${fmtBytes(bps)}/s`

export function fmtEta(seconds: number | null | undefined) {
  if (seconds == null || !Number.isFinite(seconds) || seconds <= 0) return ""
  if (seconds < 60) return `${Math.ceil(seconds)} s left`
  const minutes = Math.ceil(seconds / 60)
  if (minutes < 60) return `${minutes} min left`
  const hours = Math.floor(minutes / 60)
  return `${hours} h ${minutes % 60} min left`
}

export const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`

export const hexToRgb = (hex: string): [number, number, number] => {
  const n = parseInt(hex.replace("#", ""), 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255]
}

export function compareVersions(left = "", right = "") {
  const a = left.split(".").map(part => Number(part) || 0)
  const b = right.split(".").map(part => Number(part) || 0)
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    if ((a[index] || 0) !== (b[index] || 0)) return (a[index] || 0) - (b[index] || 0)
  }
  return 0
}

export const errorText = (error: unknown) => {
  if (error instanceof Error) return displayText(error.message)
  if (typeof error === "string") return displayText(error)
  try { return JSON.stringify(error) } catch { return "The operation failed" }
}
