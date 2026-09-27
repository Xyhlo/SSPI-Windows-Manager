/* =====================================================================
   The library: titles installed on each console. A receiver that can
   list installed titles is the source of truth; its last complete
   snapshot is kept per console address. Without one, the library falls
   back to titles SSPI confirmed as installed (stage "complete"), kept
   locally so clearing finished downloads doesn't empty it.
   ===================================================================== */
import type { ConsoleKind, DeliveryJob } from "../types"
import type { ConsoleLibrarySnapshot, LibraryTitle } from "./console-types"
import { compareVersions } from "./format"
import { jobTarget } from "./consoles"
import { kindOf, titleIdOf } from "./downloads"

export type LibraryEntry = {
  key: string
  titleId: string
  name: string
  /** Catalog artwork (portrait), when SSPI has seen the title in a package source. */
  cover?: string
  /** The console's own home-screen icon (square thumbnail), when the receiver reported it. */
  icon?: string
  customIcon?: boolean
  requiredFirmware?: string
  target: ConsoleKind
  baseVersion?: string
  updateVersion?: string
  installedAt: number
}

export type CachedSnapshot = { entries: LibraryTitle[]; syncedAt: number }
/** Kept for the 2.20.2 PS4 cache format. */
export type CachedPs4Snapshot = CachedSnapshot
type Store = {
  entries: LibraryEntry[]
  covers: Record<string, { name: string; cover?: string }>
  snapshots: Record<string, CachedSnapshot>
  ps4Snapshots?: Record<string, CachedSnapshot>
}

const KEY = "sspi.library.v1"

function read(): Store {
  try {
    const value = JSON.parse(window.localStorage.getItem(KEY) || "null") as Store | null
    if (value && Array.isArray(value.entries) && value.covers && typeof value.covers === "object") {
      const snapshots: Record<string, CachedSnapshot> = value.snapshots && typeof value.snapshots === "object" ? { ...value.snapshots } : {}
      // 2.20.2 kept PS4 snapshots keyed by host:port only.
      for (const [endpoint, snapshot] of Object.entries(value.ps4Snapshots && typeof value.ps4Snapshots === "object" ? value.ps4Snapshots : {})) {
        if (!snapshots[`ps4|${endpoint}`]) snapshots[`ps4|${endpoint}`] = snapshot
      }
      return { entries: value.entries, covers: value.covers, snapshots }
    }
  } catch { /* fall through */ }
  return { entries: [], covers: {}, snapshots: {} }
}

function write(store: Store) {
  try { window.localStorage.setItem(KEY, JSON.stringify({ entries: store.entries, covers: store.covers, snapshots: store.snapshots })); return true } catch { return false }
}

const cleanVersion = (value?: string | null) => (value || "").trim().replace(/^v(?:ersion)?\s*/i, "")
const higher = (a?: string | null, b?: string | null) => {
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

export const endpointKey = (host: string, port: number) => `${host.trim().toLowerCase()}:${port}`
export const snapshotKey = (target: ConsoleKind, host: string, port: number) => `${target}|${endpointKey(host, port)}`
/** Kept for callers of the 2.20.2 API. */
export const ps4EndpointKey = endpointKey

export function cachedSnapshot(target: ConsoleKind, host: string, port: number): CachedSnapshot | null {
  const snapshot = read().snapshots[snapshotKey(target, host, port)]
  return snapshot && Array.isArray(snapshot.entries) && typeof snapshot.syncedAt === "number" ? snapshot : null
}

/** Stores only a complete receiver snapshot so a partial scan can never erase a known library. */
export function saveSnapshot(target: ConsoleKind, host: string, port: number, snapshot: Pick<ConsoleLibrarySnapshot, "entries">): { snapshot: CachedSnapshot; persisted: boolean } {
  const cached = { entries: snapshot.entries, syncedAt: Date.now() }
  const store = read()
  store.snapshots[snapshotKey(target, host, port)] = cached
  return { snapshot: cached, persisted: write(store) }
}

/** Replaces one title's icon in a cached snapshot after SSPI changed it on the console. */
export function patchSnapshotIcon(target: ConsoleKind, host: string, port: number, titleId: string, icon: string | null, customIcon: boolean): CachedSnapshot | null {
  const store = read()
  const key = snapshotKey(target, host, port)
  const snapshot = store.snapshots[key]
  if (!snapshot) return null
  const id = titleId.toUpperCase()
  const next = { ...snapshot, entries: snapshot.entries.map(item => item.titleId.toUpperCase() === id ? { ...item, icon: icon ?? item.icon, customIcon } : item) }
  store.snapshots[key] = next
  write(store)
  return next
}

/** Maps a receiver snapshot over install history while keeping useful names and art if metadata was unavailable. */
export function snapshotEntries(target: ConsoleKind, snapshot: CachedSnapshot): LibraryEntry[] {
  const store = read()
  const history = new Map(store.entries.filter(entry => entry.target === target).map(entry => [entry.titleId.toUpperCase(), entry]))
  return snapshot.entries.map(item => {
    const titleId = item.titleId.toUpperCase()
    const previous = history.get(titleId)
    const remembered = store.covers[titleId]
    const reportedName = item.name.trim()
    const name = reportedName && reportedName.toUpperCase() !== titleId ? reportedName : previous?.name || remembered?.name || titleId
    return {
      key: `${target}:${titleId}`,
      titleId,
      name,
      cover: remembered?.cover || previous?.cover || item.cover || undefined,
      icon: item.icon || undefined,
      customIcon: !!item.customIcon,
      requiredFirmware: item.requiredFirmware || undefined,
      target,
      baseVersion: item.baseVersion || undefined,
      updateVersion: item.updateVersion || undefined,
      installedAt: previous?.installedAt || snapshot.syncedAt,
    }
  })
}
/** Kept for callers of the 2.20.2 API. */
export const ps4SnapshotEntries = (snapshot: CachedSnapshot) => snapshotEntries("ps4", snapshot)

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
  const store = persist ? read() : { entries: [], covers: {}, snapshots: {} }
  const { entries, changed } = merge(store.entries, store.covers, jobs)
  if (persist && changed) write({ ...store, entries })
  return entries.sort((a, b) => b.installedAt - a.installedAt || a.name.localeCompare(b.name))
}

export function forgetLibraryEntry(key: string) {
  const store = read()
  store.entries = store.entries.filter(entry => entry.key !== key)
  write(store)
}

export const installedVersion = (entry: Pick<LibraryEntry, "baseVersion" | "updateVersion">) => higher(entry.updateVersion, entry.baseVersion) || ""
