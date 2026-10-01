/* =====================================================================
   Reading console diagnostics: kernel panics and faults in a kernel log,
   which processes are homebrew payloads, and which payload a log belongs to.
   Pure functions, so the views and the tests share them.
   ===================================================================== */
import type { ConsoleProcess, LogFile } from "./console-types"

export type Severity = "panic" | "error" | "warning"
export type LogFinding = { severity: Severity; line: number; text: string; context: string[] }
export type LogAnalysis = { lines: string[]; findings: LogFinding[]; panics: number; errors: number; warnings: number }

// FreeBSD panic and trap messages, plus the fault lines Sony's kernel prints for crashed processes.
const PANIC = /\bpanic\s*[:!(]|kernel panic|\bfatal trap\b|double fault|\bkassert|trap number\s*=|stack overflow detected|machine check|\bstopped at\b.*\bdb_|\bkdb_enter\b/i
const ERROR = /\berror\b|\bfailed\b|\bfailure\b|\bfault\b|segmentation|\bsig(?:segv|bus|ill|abrt|fpe)\b|exited on signal|core ?dump|\bcrash(?:ed)?\b|\bpage fault\b|unhandled|abort(?:ed)?\b/i
const WARNING = /\bwarn(?:ing)?\b|timed? ?out\b|\bdenied\b|not permitted|\bretry(?:ing)?\b|\bunavailable\b/i
// Lines that name an error code without being one ("error 0", "no errors").
const BENIGN = /\b(?:no|0) errors?\b|\berror[:= ]+0x?0+\b|\berror\s*=\s*0\b/i

export function severityOf(line: string): Severity | null {
  if (PANIC.test(line)) return "panic"
  if (BENIGN.test(line)) return null
  if (ERROR.test(line)) return "error"
  if (WARNING.test(line)) return "warning"
  return null
}

/** Splits the log once and lists every panic, error and warning, newest last. Panics keep the lines that follow (the trap frame). */
export function analyzeKernelLog(text: string, contextLines = 12): LogAnalysis {
  const lines = text.replace(/\r\n?/g, "\n").split("\n")
  if (lines.length && lines[lines.length - 1] === "") lines.pop()
  const findings: LogFinding[] = []
  let panics = 0, errors = 0, warnings = 0, lastPanic = -Infinity
  for (let i = 0; i < lines.length; i++) {
    const severity = severityOf(lines[i])
    if (!severity) continue
    // A trap frame repeats fault and panic keywords; it belongs to the panic that opened it.
    if (i - lastPanic <= contextLines) continue
    if (severity === "panic") { panics++; lastPanic = i }
    else if (severity === "error") errors++
    else warnings++
    findings.push({ severity, line: i, text: lines[i].trim(), context: severity === "panic" ? lines.slice(i + 1, i + 1 + contextLines) : [] })
  }
  return { lines, findings, panics, errors, warnings }
}

// Process names ELF loaders give payloads, and well-known enablers. Games run as eboot.bin.
const PAYLOAD_NAMES = /\.elf$|etahen|kstuff|shadowmount|ftpsrv|klogsrv|elfldr|websrv|shsrv|gdbsrv|goldhen|itemzflow|sspi|byepervisor|ps[45]debug|ps[45]-?hen|mast1c0re|kernel.?patch|dumper|spoofer|launcher/i

export type ProcessKind = "payload" | "game" | "system" | "process"
export function processKind(process: ConsoleProcess): ProcessKind {
  if (PAYLOAD_NAMES.test(process.name)) return "payload"
  if (process.titleId && /^(?:CUSA|PPSA)\d{5}$/.test(process.titleId)) return "game"
  if (process.titleId?.startsWith("NPXS") || process.name.startsWith("Sce")) return "system"
  return "process"
}

/** The payload folder a file sits in: /data/etaHEN/etaHEN.log → "etaHEN", /user/data/orbiscore-… → "Crash reports". */
export function logOwner(file: LogFile) {
  if (file.kind === "crash") return "Crash reports"
  const parts = file.path.split("/").filter(Boolean)
  const base = parts[0] === "user" ? parts.slice(2) : parts.slice(1)
  return base.length > 1 ? base[0] : parts[0] === "user" ? "User data" : "Data"
}

/** Groups files by owner, newest file first within a group and newest group first. */
export function groupLogFiles(files: LogFile[]) {
  const groups = new Map<string, LogFile[]>()
  for (const file of [...files].sort((a, b) => b.modified - a.modified)) {
    const owner = logOwner(file)
    groups.set(owner, [...(groups.get(owner) || []), file])
  }
  return [...groups.entries()]
    .map(([owner, list]) => ({ owner, files: list, newest: list[0]?.modified ?? 0 }))
    .sort((a, b) => (a.owner === "Crash reports" ? -1 : b.owner === "Crash reports" ? 1 : b.newest - a.newest))
}

export function fmtDuration(ms: number) {
  if (!Number.isFinite(ms) || ms < 0) return "—"
  const seconds = Math.floor(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${minutes % 60}m`
}

/** "3 min ago" style age for unix-second timestamps. */
export function fmtAge(unixSeconds: number, now = Date.now()) {
  if (!unixSeconds || unixSeconds < 1_000_000_000) return "unknown time"
  const seconds = Math.max(0, Math.round(now / 1000 - unixSeconds))
  if (seconds < 60) return "just now"
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min ago`
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} h ago`
  const days = Math.floor(seconds / 86_400)
  return days < 30 ? `${days} d ago` : new Date(unixSeconds * 1000).toLocaleDateString()
}

/* ---------------------------------------------------------------- exports (.txt / .csv); saved through export_text_file */
export type ExportFormat = "txt" | "csv"

/** RFC 4180 field: quoted when it holds a comma, quote or line break. Formula-looking text is defused for spreadsheets. */
export function csvField(value: unknown): string {
  let text = value == null ? "" : String(value)
  if (/^[=+@\t\r]/.test(text)) text = `'${text}`
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text
}
export function toCsv(header: string[], rows: unknown[][]): string {
  return [header, ...rows].map(row => row.map(csvField).join(",")).join("\r\n") + "\r\n"
}

/** `ps5-processes-2026-10-01-1012.csv` */
export function exportName(target: string, what: string, format: ExportFormat, at = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0")
  const stamp = `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}-${pad(at.getHours())}${pad(at.getMinutes())}`
  return `${target}-${what}-${stamp}.${format}`
}

const iso = (seconds?: number | null) => seconds ? new Date(seconds * 1000).toISOString() : ""

export function processesExport(processes: ConsoleProcess[], format: ExportFormat, capturedAt: number, console: string): string {
  if (format === "csv") {
    return toCsv(
      ["PID", "Parent PID", "Name", "Kind", "Title ID", "State", "Memory bytes", "Virtual bytes", "Threads", "CPU ms", "Started", "Auth ID", "Can stop"],
      processes.map(p => [p.pid, p.ppid, p.name, processKind(p), p.titleId ?? "", p.state, p.rssBytes, p.vmBytes, p.threads, p.cpuMs, iso(p.startedAt), p.authId ?? "", p.control ?? ""]),
    )
  }
  const rows = processes.map(p => [String(p.pid), p.name, p.titleId ?? "-", p.state, `${(p.rssBytes / 1048576).toFixed(1)} MB`, String(p.threads), p.control ?? ""])
  const widths = ["PID", "Name", "Title", "State", "Memory", "Threads", "Can stop"].map((h, i) => Math.max(h.length, ...rows.map(r => r[i].length)))
  const line = (cells: string[]) => cells.map((cell, i) => cell.padEnd(widths[i])).join("  ").trimEnd()
  return [`${console} processes, captured ${new Date(capturedAt).toISOString()}`, "",
    line(["PID", "Name", "Title", "State", "Memory", "Threads", "Can stop"]), ...rows.map(line), ""].join("\r\n")
}

/** A kernel log or payload log: the text as is, or one row per line with its severity. */
export function textLogExport(text: string, format: ExportFormat): string {
  if (format === "txt") return text.replace(/\r?\n/g, "\r\n")
  const { lines } = analyzeKernelLog(text)
  return toCsv(["Line", "Severity", "Text"], lines.map((line, i) => [i + 1, severityOf(line) ?? "", line]))
}

export function logListExport(files: LogFile[], format: ExportFormat): string {
  if (format === "csv") return toCsv(["Path", "Kind", "Bytes", "Modified"], files.map(f => [f.path, f.kind, f.size, iso(f.modified)]))
  return files.map(f => `${f.path}\t${f.kind}\t${f.size} bytes\t${iso(f.modified)}`).join("\r\n") + "\r\n"
}
