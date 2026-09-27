/* =====================================================================
   "Your library": titles SSPI has installed, per console. The consoles
   don't report their installed titles to this PC, so the library is
   built from confirmed installs (stage "complete") and kept locally, so
   clearing finished downloads doesn't empty it.
   ===================================================================== */
import type { ConsoleKind, DeliveryJob, Ps4LibrarySnapshot } from "@/types"
import { compareVersions } from "./format"
import { jobTarget } from "./consoles"
import { kindOf, titleIdOf } from "./downloads"

export type LibraryEntry = {
  key: string
  titleId: string
  name: string
  cover?: string
  target: ConsoleKind
  baseVersion?: string
  updateVersion?: string
  installedAt: number
}

export type CachedPs4Snapshot = { entries: Ps4LibrarySnapshot["entries"]; syncedAt: number }
type Store = {
  entries: LibraryEntry[]
  covers: Record<string, { name: string; cover?: string }>
  ps4Snapshots: Record<string, CachedPs4Snapshot>
}

const KEY = "sspi.library.v1"

function read(): Store {
  try {
    const value = JSON.parse(window.localStorage.getItem(KEY) || "null") as Store | null
    if (value && Array.isArray(value.entries) && value.covers && typeof value.covers === "object") {
      return { ...value, ps4Snapshots: value.ps4Snapshots && typeof value.ps4Snapshots === "object" ? value.ps4Snapshots : {} }
    }
  } catch { /* fall through */ }
  return { entries: [], covers: {}, ps4Snapshots: {} }
}

function write(store: Store) {
  try { window.localStorage.setItem(KEY, JSON.stringify(store)); return true } catch { return false }
}

const cleanVersion = (value?: string) => (value || "").trim().replace(/^v(?:ersion)?\s*/i, "")
const higher = (a?: string, b?: string) => {
  const x = cleanVersion(a), y = cleanVersion(b)
  if (!x) return y || undefined
  if (!y) return x
  return compareVersions(x, y) >= 0 ? x : y
}

/** Remembers the catalog cover and name for a title when it is queued, so the library can show real artwork. */
export function rememberCover(titleId: string, name: string, cover?: string) {
  if (!titleId || !cover || cover.startsWith("data:image/png;sspi-case")) return
  const store = read()
  const id = titleId.toUpperCase()
  if (store.covers[id]?.cover === cover && store.covers[id]?.name === name) return
  store.covers[id] = { name, cover }
  write(store)
}

export const ps4EndpointKey = (host: string, port: number) => `${host.trim().toLowerCase()}:${port}`

export function cachedPs4Snapshot(host: string, port: number): CachedPs4Snapshot | null {
  const snapshot = read().ps4Snapshots[ps4EndpointKey(host, port)]
  return snapshot && Array.isArray(snapshot.entries) && typeof snapshot.syncedAt === "number" ? snapshot : null
}

/** Stores only a complete receiver snapshot so a partial scan can never erase a known library. */
export function savePs4Snapshot(host: string, port: number, snapshot: Ps4LibrarySnapshot): { snapshot: CachedPs4Snapshot; persisted: boolean } {
  const cached = { entries: snapshot.entries, syncedAt: Date.now() }
  const store = read()
  store.ps4Snapshots[ps4EndpointKey(host, port)] = cached
  return { snapshot: cached, persisted: write(store) }
}

/** Maps a receiver snapshot over history while retaining useful names/art if metadata was unavailable. */
export function ps4SnapshotEntries(snapshot: CachedPs4Snapshot): LibraryEntry[] {
  const store = read()
  const history = new Map(store.entries.filter(entry => entry.target === "ps4").map(entry => [entry.titleId.toUpperCase(), entry]))
  return snapshot.entries.map(item => {
    const titleId = item.titleId.toUpperCase()
    const previous = history.get(titleId)
    const remembered = store.covers[titleId]
    const reportedName = item.name.trim()
    const name = reportedName && reportedName.toUpperCase() !== titleId
      ? reportedName
      : previous?.name || remembered?.name || titleId
    return {
      key: `ps4:${titleId}`,
      titleId,
      name,
      cover: remembered?.cover || item.icon || previous?.cover,
      target: "ps4",
      baseVersion: item.baseVersion || undefined,
      updateVersion: item.updateVersion || undefined,
      installedAt: previous?.installedAt || snapshot.syncedAt,
    }
  })
}

function merge(entries: LibraryEntry[], covers: Store["covers"], jobs: DeliveryJob[]) {
  const map = new Map(entries.map(entry => [entry.key, { ...entry }]))
  let changed = false
  for (const job of jobs) {
    if (job.stage !== "complete") continue
    const titleId = titleIdOf(job)
    if (!titleId) continue
    const target = jobTarget(job)
    const key = `${target}:${titleId}`
    const kind = kindOf(job)
    const known = covers[titleId]
    const current = map.get(key)
    const next: LibraryEntry = current ? { ...current } : {
      key, titleId, target, name: job.title || known?.name || titleId, cover: known?.cover || job.icon, installedAt: job.createdAt || Date.now(),
    }
    if (job.localPkg && job.icon) next.cover = job.icon
    if (!next.cover && (known?.cover || job.icon)) next.cover = known?.cover || job.icon
    if (known?.cover && next.cover !== known.cover && next.cover?.startsWith("data:image/png")) next.cover = known.cover
    if (job.title && (next.name === titleId || job.localPkg)) next.name = job.title
    if (kind === "base" || kind === "combined") next.baseVersion = higher(next.baseVersion, job.packageVersion)
    if (kind === "update") next.updateVersion = higher(next.updateVersion, job.packageVersion)
    next.installedAt = Math.max(next.installedAt || 0, job.createdAt || 0)
    if (!current || JSON.stringify(current) !== JSON.stringify(next)) { map.set(key, next); changed = true }
  }
  return { entries: [...map.values()], changed }
}

/** Folds newly completed installs into the saved library and returns every entry, newest first. */
export function syncLibrary(jobs: DeliveryJob[], persist = true): LibraryEntry[] {
  const store = persist ? read() : { entries: [], covers: {}, ps4Snapshots: {} }
  const { entries, changed } = merge(store.entries, store.covers, jobs)
  if (persist && changed) write({ ...store, entries })
  return entries.sort((a, b) => b.installedAt - a.installedAt || a.name.localeCompare(b.name))
}

export function forgetLibraryEntry(key: string) {
  const store = read()
  store.entries = store.entries.filter(entry => entry.key !== key)
  write(store)
}

export const installedVersion = (entry: LibraryEntry) => higher(entry.updateVersion, entry.baseVersion) || ""
