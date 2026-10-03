import type { KernelLog, LogFileList, LogTail } from "./console-types"
import { severityOf, toCsv, type ExportFormat } from "./diagnostics"

export type TimelineCapture = {
  capturedAt: number
  kernel: KernelLog | null
  files: LogFileList | null
  logs: LogTail[]
  unavailable: Array<{ source: string; reason: string }>
  skippedLogs: number
}
export type TimelineEvent = {
  id: string
  source: string
  line: number | null
  kind: "panic" | "crash" | "error" | "warning" | "restart" | "activity" | "report"
  summary: string
  excerpt: string
  detail: string
  timestamp: number | null
  timeLabel: string
  timeBasis: "log" | "file" | "local" | "uptime" | "unknown"
  interpretation: string
  firstObservedAt?: number
  lastObservedAt?: number
}
export type CrashTimeline = {
  dated: TimelineEvent[]
  undated: Array<{ source: string; events: TimelineEvent[] }>
  coverage: string[]
  eventCount: number
  issueCount: number
  reportCount: number
}
export type TimelineHistory = { capturedAt: number; events: TimelineEvent[]; coverage: string[]; limited: boolean }
const HISTORY_KEY = "sspi.crashTimeline.v1"
const HISTORY_EVENTS = 200
const HISTORY_CHARACTERS = 280_000
type HistoryStore = Pick<Storage, "getItem" | "setItem">

const FILE_LIMIT = 12
const SOURCE_EVENT_LIMIT = 160
const MAX_TIME = Date.UTC(2100, 0, 1)
const MIN_TIME = Date.UTC(2000, 0, 1)
const fileName = (path: string) => path.slice(path.lastIndexOf("/") + 1)
const validTime = (value: number) => Number.isFinite(value) && value >= MIN_TIME && value < MAX_TIME

/** Only explicit wall-clock timestamps are compared between logs. No boot time is guessed. */
export function timelineTime(line: string): Pick<TimelineEvent, "timestamp" | "timeLabel" | "timeBasis"> {
  const prefix = line.replace(/^\s*(?:<\d+>)?\s*/, "")
  const date = prefix.match(/^\[?(\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?)(Z|[+-]\d{2}:?\d{2}|\s+UTC)?(?=\]|\s|$)/)
  if (date) {
    const label = `${date[1]}${date[2] || ""}`
    const wallTime = date[1].replace(" ", "T")
    // Date.parse accepts impossible dates such as February 30; check the calendar first.
    const calendar = new Date(`${wallTime}Z`)
    const validDate = Number.isFinite(calendar.getTime()) && calendar.toISOString().slice(0, 10) === wallTime.slice(0, 10)
    if (validDate && date[2]) {
      const timestamp = Date.parse(wallTime + date[2].replace(/\s+UTC/, "Z"))
      if (validTime(timestamp)) return { timestamp, timeLabel: label, timeBasis: "log" }
    }
    if (validDate && !date[2]) return { timestamp: null, timeLabel: `${label} · time zone unknown`, timeBasis: "local" }
  }
  const clock = prefix.match(/^\[?(\d{2}:\d{2}:\d{2}(?:\.\d{1,3})?)(?=\]|\s|$)/)
  if (clock && Number(clock[1].slice(0, 2)) < 24 && Number(clock[1].slice(3, 5)) < 60 && Number(clock[1].slice(6, 8)) < 60) {
    return { timestamp: null, timeLabel: `${clock[1]} · date unknown`, timeBasis: "local" }
  }
  const uptime = prefix.match(/^\[\s*(\d+\.\d+)\]\s/)
  if (uptime) return { timestamp: null, timeLabel: `+${uptime[1]} s · boot unknown`, timeBasis: "uptime" }
  return { timestamp: null, timeLabel: "Time not recorded", timeBasis: "unknown" }
}

function classify(line: string): Pick<TimelineEvent, "kind" | "summary" | "interpretation"> | null {
  const severity = severityOf(line)
  if (severity === "panic") return {
    kind: "panic", summary: "Kernel panic / trap reported",
    interpretation: "The log reports a panic or trap. Its root cause and whether it belongs to this boot are not established by this message alone.",
  }
  if (severity === "error" && /exited on signal|segmentation|\bsig(?:segv|bus|ill|abrt|fpe)\b|core ?dump|\bcrashed\b/i.test(line)) return {
    kind: "crash", summary: "Process crash reported",
    interpretation: "The message reports a process fault or crash. It does not by itself establish that the console crashed.",
  }
  if (severity) return {
    kind: severity, summary: severity === "error" ? "Error reported" : "Warning reported",
    interpretation: "This is a message from the recorded log. Being near another event does not establish that it caused a crash.",
  }
  if (/\b(?:rebooting|rebooted|restarting|restarted|booting)\b|\b(?:receiver|kernel)\b.*\b(?:startup|starting|started)\b/i.test(line)) return {
    kind: "restart", summary: "Startup / restart reported",
    interpretation: "The source records a startup or restart message. The reason for it is not given by chronology alone.",
  }
  if (/\b(?:loaded|loading|launched|launching|listening|receiver ready|install complete|installation complete)\b/i.test(line)) return {
    kind: "activity", summary: "Activity reported",
    interpretation: "Recorded activity provides context. It is not evidence that this activity caused a later failure.",
  }
  return null
}

const TRAP_DETAIL = /^\s*(?:cpuid\s*=|apic id|fault (?:virtual|code)|instruction pointer|stack pointer|frame pointer|code segment|processor eflags|current process|trap number|panic:|KDB:|db_trace|#\d+\s|0x[\da-f]+|Uptime:|backtrace|stack trace)/i

function logEvents(source: string, text: string, tail = false): { events: TimelineEvent[]; omitted: number } {
  const lines = text.replace(/\r\n?/g, "\n").split("\n")
  const events: TimelineEvent[] = []
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]
    const classification = classify(line)
    if (!classification) continue
    const time = timelineTime(line)
    let end = index
    if (classification.kind === "panic") {
      // Fold only recognisable, untimed trap-frame lines; a second timestamped panic is a new event.
      while (end + 1 < lines.length && end - index < 20 &&
        timelineTime(lines[end + 1]).timeBasis === "unknown" && TRAP_DETAIL.test(lines[end + 1])) end++
    }
    const excerpt = line.trim()
    events.push({
      id: `${source}:${index + 1}`, source, line: index + 1, ...classification, ...time,
      excerpt: excerpt.length > 180 ? `${excerpt.slice(0, 177)}…` : excerpt,
      detail: `${tail ? "Line numbers refer to the captured tail, not the whole file.\n\n" : ""}${lines.slice(index, end + 1).join("\n")}`,
    })
    index = end
  }
  return { events: events.slice(-SOURCE_EVENT_LIMIT), omitted: Math.max(0, events.length - SOURCE_EVENT_LIMIT) }
}

export function selectTimelineLogs(files: LogFileList["files"]) {
  // Read text logs only. Config files may contain credentials, and binary dumps aren't text evidence.
  return files.filter(file => file.kind === "log" && file.size > 0)
    .sort((a, b) => b.modified - a.modified || a.path.localeCompare(b.path)).slice(0, FILE_LIMIT)
}

export function buildCrashTimeline(capture: TimelineCapture): CrashTimeline {
  const events: TimelineEvent[] = []
  const coverage: string[] = []
  const addLog = (source: string, text: string, tail = false) => {
    const parsed = logEvents(source, text, tail)
    events.push(...parsed.events)
    if (parsed.omitted) coverage.push(`${source}: ${parsed.omitted} older matching messages omitted; the latest ${SOURCE_EVENT_LIMIT} are shown.`)
  }
  if (capture.kernel) {
    addLog("Kernel log", capture.kernel.text)
    if (capture.kernel.busy) coverage.push("Another reader holds /dev/klog. The kernel snapshot may contain only earlier buffered messages; use Kernel log → Live to receive new output.")
    if (capture.kernel.dropped) coverage.push("The kernel buffer dropped its oldest messages.")
    if (!capture.kernel.text.trim()) coverage.push("The kernel snapshot is empty. It does not establish that no crash occurred.")
  } else if (!capture.unavailable.some(item => item.source === "Kernel log")) coverage.push("The kernel log was not captured.")
  if (capture.files?.incomplete) coverage.push("Some payload folders could not be read.")
  if (capture.files?.truncated) coverage.push("The receiver's file list reached its limit; other logs or crash reports may be missing.")
  if (capture.skippedLogs) coverage.push(`${capture.skippedLogs} older text log${capture.skippedLogs === 1 ? " was" : "s were"} not read. Open Logs & crashes to inspect them.`)
  if (!capture.files && !capture.unavailable.some(item => item.source === "Log files")) coverage.push("The payload log and crash-report list was not captured.")
  for (const log of capture.logs) {
    addLog(log.path, log.text, log.offset > 0)
    if (log.offset > 0) coverage.push(`${log.path}: only the last ${log.bytes.toLocaleString()} of ${log.size.toLocaleString()} bytes were read.`)
  }
  for (const file of capture.files?.files || []) {
    if (file.kind !== "crash") continue
    const timestamp = file.modified * 1000
    events.push({
      id: `file:${file.path}`, source: file.path, line: null, kind: "report", summary: "Crash report file found",
      excerpt: fileName(file.path), detail: `Path: ${file.path}\nSize: ${file.size.toLocaleString()} bytes\nFile modification time: ${validTime(timestamp) ? new Date(timestamp).toISOString() : "not available"}\n\nThe binary report has not been decoded.`,
      timestamp: validTime(timestamp) ? timestamp : null,
      timeLabel: validTime(timestamp) ? new Date(timestamp).toISOString() : "Time not recorded", timeBasis: validTime(timestamp) ? "file" : "unknown",
      interpretation: "The receiver found a crash-report file. Its modification time is not a confirmed crash time; the filename alone does not establish the crashed process or cause.",
    })
  }
  for (const item of capture.unavailable) coverage.push(`${item.source}: ${item.reason}`)
  const dated = events.filter(event => event.timestamp != null).sort((a, b) => a.timestamp! - b.timestamp! || a.id.localeCompare(b.id))
  const undated = new Map<string, TimelineEvent[]>()
  for (const event of events.filter(event => event.timestamp == null)) {
    const group = undated.get(event.source) || []
    group.push(event); undated.set(event.source, group)
  }
  if (undated.size) coverage.push("Some messages lack a full timestamp and time zone. They stay in source order below; they cannot be placed between dated events.")
  return {
    dated, undated: [...undated].map(([source, entries]) => ({ source, events: entries })), coverage,
    eventCount: events.length, issueCount: events.filter(event => ["panic", "crash", "error"].includes(event.kind)).length,
    reportCount: events.filter(event => event.kind === "report").length,
  }
}

export function crashTimelineExport(timeline: CrashTimeline, capture: Pick<TimelineCapture, "capturedAt">, target: string, format: ExportFormat) {
  const events = [...timeline.dated, ...timeline.undated.flatMap(group => group.events)]
  if (format === "csv") return toCsv(
    ["Record", "Time", "Time basis", "Source", "Line", "Summary", "Recorded detail", "Interpretation", "First observed", "Last observed"],
    [...events.map(event => ["Recorded", event.timeLabel, event.timeBasis, event.source, event.line ?? "", event.summary, event.detail, event.interpretation, event.firstObservedAt ? new Date(event.firstObservedAt).toISOString() : "", event.lastObservedAt ? new Date(event.lastObservedAt).toISOString() : ""]),
      ...timeline.coverage.map(note => ["Coverage", "", "", "", "", note, "", "", "", ""])],
  )
  return [
    `${target.toUpperCase()} crash timeline · captured ${new Date(capture.capturedAt).toISOString()}`,
    "Dated events are oldest first. Undated messages follow grouped by source and first capture. File modification times are not confirmed crash times.", "",
    ...events.flatMap(event => [`${event.timeLabel} [${event.timeBasis}] · ${event.summary}`, `Recorded · ${event.source}${event.line ? ` · line ${event.line}` : ""}`, event.detail, `Interpretation: ${event.interpretation}`, ...(event.firstObservedAt ? [`First observed by SSPI: ${new Date(event.firstObservedAt).toISOString()}`, `Last observed by SSPI: ${new Date(event.lastObservedAt!).toISOString()}`] : []), ""]),
    "Coverage", ...timeline.coverage.map(note => `- ${note}`), "",
  ].join("\r\n")
}

/* ---------------------------------------------------------------- presentation */
/** One line in the timeline: a record, or a run of near-identical records (numbers aside) from one source. */
export type TimelineRow = { key: string; event: TimelineEvent; count: number; lines: [number, number] | null }
export type TimelineSection = { key: string; title: string; subtitle: string; rows: TimelineRow[] }
export type TimelineCounts = { panics: number; crashes: number; errors: number; warnings: number; reports: number; restarts: number }

const ISSUES = new Set<TimelineEvent["kind"]>(["panic", "crash", "error", "report"])
export const isTimelineIssue = (event: TimelineEvent) => ISSUES.has(event.kind)
/** Counters, ids and addresses differ between repeats of the same message; the text around them doesn't. */
const shape = (event: TimelineEvent) => `${event.source}\u0000${event.kind}\u0000${event.excerpt.replace(/0x[\da-f]+|\d+/gi, "#")}`

function mergeRows(events: TimelineEvent[]): TimelineRow[] {
  const rows = new Map<string, TimelineRow & { latest: number }>()
  events.forEach((event, index) => {
    const key = shape(event)
    const row = rows.get(key)
    const line = event.line
    if (!row) { rows.set(key, { key: event.id, event, count: 1, lines: line ? [line, line] : null, latest: index }); return }
    row.count++
    row.event = event
    row.latest = index
    if (line) row.lines = row.lines ? [Math.min(row.lines[0], line), Math.max(row.lines[1], line)] : [line, line]
  })
  // Newest first: each merged row sits where its latest record is.
  return [...rows.values()].sort((a, b) => b.latest - a.latest).map(({ latest: _latest, ...row }) => row)
}

const dayTitle = (timestamp: number, now: number) => {
  const day = new Date(timestamp), today = new Date(now)
  const yesterday = new Date(now - 86_400_000)
  if (day.toDateString() === today.toDateString()) return "Today"
  if (day.toDateString() === yesterday.toDateString()) return "Yesterday"
  return day.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric", year: day.getFullYear() === today.getFullYear() ? undefined : "numeric" })
}

/** Dated records by local day, newest first; then each source without timestamps, in its own order. */
export function timelineSections(timeline: CrashTimeline, matches: (event: TimelineEvent) => boolean, now = Date.now()): TimelineSection[] {
  const sections: TimelineSection[] = []
  const days = new Map<string, TimelineEvent[]>()
  for (const event of timeline.dated.filter(matches)) {
    const title = dayTitle(event.timestamp!, now)
    days.set(title, [...(days.get(title) || []), event])
  }
  for (const [title, events] of [...days].reverse()) sections.push({ key: `day:${title}`, title, subtitle: "Your local time", rows: mergeRows(events) })
  for (const group of timeline.undated) {
    const events = group.events.filter(matches)
    if (events.length) sections.push({ key: `source:${group.source}`, title: fileName(group.source) || group.source, subtitle: group.source === "Kernel log" ? "No timestamps · newest first" : `${group.source} · no timestamps`, rows: mergeRows(events) })
  }
  return sections
}

export function timelineCounts(events: TimelineEvent[]): TimelineCounts {
  const count = (kind: TimelineEvent["kind"]) => events.filter(event => event.kind === kind).length
  return { panics: count("panic"), crashes: count("crash"), errors: count("error"), warnings: count("warning"), reports: count("report"), restarts: count("restart") }
}

export function timelineEndpoint(target: string, host: string, port: number, demo: boolean) {
  return `${demo ? "preview" : "console"}:${target}:${host.trim().toLowerCase()}:${port}`
}

const eventIdentity = (event: TimelineEvent) => JSON.stringify([event.source, event.kind, event.timestamp, event.timeLabel, event.detail])

/** Retain evidence, not a guessed crash count. Matching records are re-observations, not new events. */
export function retainTimeline(previous: TimelineHistory | null, timeline: CrashTimeline, capturedAt: number): TimelineHistory {
  const events = (previous?.events || []).map(event => ({ ...event }))
  const byRecord = new Map<string, number[]>()
  events.forEach((event, index) => { const key = eventIdentity(event); byRecord.set(key, [...(byRecord.get(key) || []), index]) })
  const occurrences = new Map<string, number>()
  for (const incoming of [...timeline.dated, ...timeline.undated.flatMap(group => group.events)]) {
    const key = eventIdentity(incoming)
    const occurrence = occurrences.get(key) || 0
    occurrences.set(key, occurrence + 1)
    const existing = byRecord.get(key)?.[occurrence]
    if (existing != null) events[existing] = { ...events[existing], lastObservedAt: capturedAt }
    else events.push({ ...incoming, id: `observed:${capturedAt}:${events.length}`, firstObservedAt: capturedAt, lastObservedAt: capturedAt })
  }
  // Keep the most recently observed records while preserving their original source ordering.
  const newest = events.map((event, index) => ({ event, index }))
    .sort((a, b) => (b.event.lastObservedAt || 0) - (a.event.lastObservedAt || 0) || (b.event.firstObservedAt || 0) - (a.event.firstObservedAt || 0) || b.index - a.index)
    .slice(0, HISTORY_EVENTS).map(item => item.event)
  const kept = new Set(newest)
  let bounded = events.filter(event => kept.has(event))
  let characters = JSON.stringify(bounded).length
  while (characters > HISTORY_CHARACTERS && newest.length) {
    const oldest = newest.pop()!
    kept.delete(oldest)
    bounded = bounded.filter(event => event !== oldest)
    characters = JSON.stringify(bounded).length
  }
  return { capturedAt, events: bounded, coverage: timeline.coverage, limited: !!previous?.limited || bounded.length < events.length }
}

export function timelineFromHistory(history: TimelineHistory): CrashTimeline {
  const undated = new Map<string, TimelineEvent[]>()
  for (const event of history.events.filter(event => event.timestamp == null)) undated.set(event.source, [...(undated.get(event.source) || []), event])
  return {
    dated: history.events.filter(event => event.timestamp != null).sort((a, b) => a.timestamp! - b.timestamp! || a.id.localeCompare(b.id)),
    undated: [...undated].map(([source, events]) => ({ source, events })),
    coverage: [...history.coverage, "History is grouped by console address. Identical captured records are combined across refreshes; this is not a count of separate crashes. Observation times say when SSPI read evidence, not when the event happened.", ...(history.limited ? ["Saved history reached its size limit. Older evidence was removed; up to 200 recently observed records are retained."] : [])],
    eventCount: history.events.length, issueCount: history.events.filter(event => ["panic", "crash", "error"].includes(event.kind)).length,
    reportCount: history.events.filter(event => event.kind === "report").length,
  }
}

function historyCache(storage: HistoryStore): Record<string, TimelineHistory> {
  const raw = storage.getItem(HISTORY_KEY)
  if (!raw) return {}
  const parsed: unknown = JSON.parse(raw)
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("Saved crash history could not be read.")
  const clean: Record<string, TimelineHistory> = {}
  for (const [key, value] of Object.entries(parsed).slice(0, 4)) {
    const history = value as TimelineHistory
    if (!key.startsWith("console:") && !key.startsWith("preview:")) continue
    if (!history || !validTime(history.capturedAt) || !Array.isArray(history.events) || history.events.length > HISTORY_EVENTS || !Array.isArray(history.coverage)) continue
    const events = history.events.filter(event => event &&
      ["id", "source", "summary", "excerpt", "detail", "timeLabel", "interpretation"].every(field => typeof event[field as keyof TimelineEvent] === "string") &&
      ["panic", "crash", "error", "warning", "restart", "activity", "report"].includes(event.kind) &&
      ["log", "file", "local", "uptime", "unknown"].includes(event.timeBasis) &&
      (event.timestamp === null || validTime(event.timestamp)) && validTime(event.firstObservedAt || 0) && validTime(event.lastObservedAt || 0) &&
      (event.line === null || Number.isInteger(event.line) && event.line > 0))
    if (JSON.stringify(events).length > HISTORY_CHARACTERS) continue
    clean[key] = { capturedAt: history.capturedAt, events, coverage: history.coverage.filter(note => typeof note === "string").slice(0, 40).map(note => note.slice(0, 2048)), limited: history.limited === true }
  }
  return clean
}

export function readTimelineHistory(storage: HistoryStore, endpoint: string): TimelineHistory | null {
  return historyCache(storage)[endpoint] || null
}

export function saveTimelineHistory(storage: HistoryStore, endpoint: string, history: TimelineHistory): void {
  let cache: Record<string, TimelineHistory>
  try { cache = historyCache(storage) } catch { cache = {} }
  cache[endpoint] = history
  const recent = Object.entries(cache).sort(([, a], [, b]) => b.capturedAt - a.capturedAt).slice(0, 4)
  storage.setItem(HISTORY_KEY, JSON.stringify(Object.fromEntries(recent)))
}
