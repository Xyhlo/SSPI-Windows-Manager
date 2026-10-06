import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"

import { ProviderContext } from "@/components/ProviderContext"
import { Header, TABS, pressKey, pulseDownloadsTab, type ConsoleState, type Tab } from "@/components/Shell"
import { LibraryPage } from "@/components/LibraryPage"
import { SearchPage } from "@/components/SearchPage"
import { DetailsPage } from "@/components/DetailsPage"
import { DownloadsPage } from "@/components/DownloadsPage"
import { ToolsPage, type ToolsTab } from "@/components/ToolsPage"
import { Updater } from "@/components/Updater"
import { OptionsOverlay, OptionsTabs, type OptionsTab } from "@/components/OptionsOverlay"
import { ReceiverDialog } from "@/components/Dialogs"
import { toast } from "@/components/toasts"
import { demoGames, demoJobs, demoMetadata, demoPackages } from "@/data/demo"
import { applyAccentVars, loadAppearance, saveAppearance, type Appearance } from "@/lib/appearance"
import { notificationCase } from "@/lib/covers"
import { consoleLabel, packageTitle, rememberHomebrew, sendBlockReason } from "@/lib/consoles"
import { addHomebrewPayload } from "@/lib/launcher-api"
import { listConsoleLibrary, probeConsoles, sendPayload } from "@/lib/console-api"
import { hasCapability, loaderEndpoint, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe } from "@/lib/console-types"
import { activeTransfer, kindOf, titleIdOf } from "@/lib/downloads"
import { compareVersions, errorText, fmtBytes, plural } from "@/lib/format"
import { isTyping, runPageKeys } from "@/lib/keys"
import { cachedSnapshot, installedVersion, patchSnapshotIcon, rememberCover, saveSnapshot, snapshotEntries, snapshotKey, syncLibrary, type CachedSnapshot, type LibraryEntry } from "@/lib/library"
import { motionOK, setReduceMotion, sleep, SPRINGS } from "@/lib/motion"
import { archivePartsFor, packageKind, packageReleaseKey, packageVersion, planPackages } from "@/lib/package-selection"
import { variantKey } from "@/lib/providers"
import { trackSpeeds } from "@/lib/speed"
import { preloadShells } from "@/stage/art"
import { getStage } from "@/stage/stage"
import {
  blankSettings,
  isActiveJob,
  type ConsoleKind,
  type DeliveryJob,
  type DeliveryRequest,
  type Game,
  type LoadState,
  type MetadataResponse,
  type PackageCandidate,
  type PackageSource,
  type Ps4Probe,
  type Settings,
} from "@/types"

const invokeCmd = <T,>(name: string, args?: Record<string, unknown>) => invoke<T>(name, args)
const inTauri = () => "__TAURI_INTERNALS__" in window
const CONSOLES: ConsoleKind[] = ["ps5", "ps4"]

// Resolve/search snappiness: soft invoke timeout, capped in-memory caches.
const RESOLVE_TIMEOUT_MS = 45_000
const SEARCH_CACHE_MS = 10 * 60 * 1_000
const PACKAGE_CACHE_MS = 30 * 60 * 1_000
const LIBRARY_STALE_MS = 5 * 60 * 1_000
const PROBE_EVERY_MS = 60_000
const CACHE_CAP = 50
const RESOLVE_TIMEOUT_SENTINEL = "resolve-timeout"
const RECENT_KEY = "sspi.recent.v1"

const withTimeout = <T,>(promise: Promise<T>, ms: number, sentinel: string): Promise<T> => new Promise((resolve, reject) => {
  const timer = window.setTimeout(() => reject(new Error(sentinel)), ms)
  promise.then(
    value => { window.clearTimeout(timer); resolve(value) },
    error => { window.clearTimeout(timer); reject(error) },
  )
})

const putCapped = <T extends { at: number }>(map: Map<string, T>, key: string, entry: Omit<T, "at">) => {
  map.delete(key)
  map.set(key, { ...entry, at: Date.now() } as T)
  while (map.size > CACHE_CAP) {
    const oldest = map.keys().next().value
    if (oldest === undefined) break
    map.delete(oldest)
  }
}

const ageText = (at: number) => {
  const mins = Math.max(0, Math.round((Date.now() - at) / 60_000))
  if (mins < 1) return "just now"
  if (mins < 60) return mins === 1 ? "1 min ago" : `${mins} min ago`
  const hours = Math.round(mins / 60)
  return hours === 1 ? "1 hour ago" : hours < 48 ? `${hours} hours ago` : `${Math.round(hours / 24)} days ago`
}

const loadRecent = (): Game[] => {
  try {
    const value = JSON.parse(window.localStorage.getItem(RECENT_KEY) || "[]") as Game[]
    return Array.isArray(value) ? value.filter(game => game && typeof game.titleId === "string" && typeof game.name === "string").slice(0, 10) : []
  } catch { return [] }
}

export type Page = Tab | "details"
export type UpdateInfo = { state: "checking" | "done" | "error"; packages?: PackageCandidate[]; checkedAt: number }
export type LibrarySync = { endpoint: string; state: "syncing" | "ready" | "partial" | "offline" | "unsupported"; syncedAt?: number; messages: string[] }
export type LibraryView = { target: ConsoleKind; entries: LibraryEntry[]; authoritative: boolean; sync?: LibrarySync; syncedAt?: number }

const APP_VERSION = typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : ""
const displayVersion = (value: string) => value.replace(/\.0$/, "")

const gameFromEntry = (entry: LibraryEntry): Game => ({ titleId: entry.titleId, name: entry.name, region: "", icon: entry.cover || entry.icon })

/** Header state for a console from its receiver probe; the PS4 inbox transport doesn't depend on the receiver. */
function probeState(probe: ConsoleProbe | undefined, previous: ConsoleState, settings: Settings): ConsoleState {
  if (!probe) return previous
  const receiverOptional = probe.target === "ps4" && settings.ps4Transport !== "receiver"
  switch (probe.receiver.state) {
    case "online": return "ok"
    case "outdated": return receiverOptional ? previous : "warn"
    case "unconfigured": return "unknown"
    default: return receiverOptional ? previous : "fail"
  }
}

export default function App() {
  /* ---------------------------------------------------------------- data */
  const [settings, setSettings] = useState<Settings>(blankSettings)
  const [jobs, setJobs] = useState<DeliveryJob[]>([])
  const [systemDrive, setSystemDrive] = useState<string | null>(null)
  const [sources, setSources] = useState<PackageSource[]>([])
  // Simulated consoles and catalogs exist only for the browser dev server; the app never offers them.
  const [demo, setDemo] = useState(() => import.meta.env.DEV && !inTauri())
  const [appearance, setAppearanceState] = useState<Appearance>(loadAppearance)
  const [consoleState, setConsoleState] = useState<Record<ConsoleKind, ConsoleState>>({ ps5: "unknown", ps4: "unknown" })
  const [probes, setProbes] = useState<Partial<Record<ConsoleKind, ConsoleProbe>>>({})
  const [loadingReceiver, setLoadingReceiver] = useState<ConsoleKind | null>(null)
  const settingsRef = useRef(settings)
  settingsRef.current = settings
  const demoRef = useRef(demo)
  demoRef.current = demo
  const probesRef = useRef(probes)
  probesRef.current = probes
  const [loaded, setLoaded] = useState(false)
  const [version, setVersion] = useState(APP_VERSION)
  const [revealed, setRevealed] = useState(false)
  const brandRef = useRef<HTMLImageElement>(null)

  /* ---------------------------------------------------------------- navigation */
  const [page, setPage] = useState<Page>("library")
  const [origin, setOrigin] = useState<Tab>("library")
  const [options, setOptions] = useState<OptionsTab | null>(null)
  const [closeOptions, setCloseOptions] = useState(0)
  const [toolsTab, setToolsTab] = useState<ToolsTab>("payloads")

  /* ---------------------------------------------------------------- search and library */
  const [query, setQuery] = useState("")
  const [searchState, setSearchState] = useState<LoadState>("idle")
  const [searchError, setSearchError] = useState("")
  const [searchedFor, setSearchedFor] = useState("")
  const [results, setResults] = useState<Game[]>([])
  const [region, setRegion] = useState("All regions")
  const [recent, setRecent] = useState<Game[]>(loadRecent)
  const [libSelected, setLibSelected] = useState<Partial<Record<ConsoleKind, string>>>({})
  const [libExpanded, setLibExpanded] = useState(false)
  const [updates, setUpdates] = useState<Record<string, UpdateInfo>>({})
  const searchGen = useRef(0)
  const searchCache = useRef(new Map<string, { at: number; games: Game[] }>())
  const resultsScroll = useRef(0)

  /* ---------------------------------------------------------------- details */
  const [selected, setSelected] = useState<Game | null>(null)
  const [packages, setPackages] = useState<PackageCandidate[]>([])
  const [packageState, setPackageState] = useState<LoadState>("idle")
  const [packageError, setPackageError] = useState("")
  const [packageNote, setPackageNote] = useState<string | null>(null)
  const [metadata, setMetadata] = useState<MetadataResponse | null>(null)
  const [metadataState, setMetadataState] = useState<LoadState>("idle")
  const [autoBackports, setAutoBackports] = useState(true)
  const [deliveryBusy, setDeliveryBusy] = useState(false)
  const deliveryPending = useRef(false)
  const resolveGen = useRef(0)
  const packageCache = useRef(new Map<string, { at: number; packages: PackageCandidate[] }>())
  const [receiverPrompt, setReceiverPrompt] = useState("")
  const [payloadBusy, setPayloadBusy] = useState(false)

  /* ---------------------------------------------------------------- downloads UI */
  const [dlFilter, setDlFilter] = useState("All")
  const [dlOpen, setDlOpen] = useState<string | null | undefined>(undefined)
  const [dlDrawer, setDlDrawer] = useState<"packages" | "files" | "errors">("packages")

  const shownJobs = demo ? demoJobs : jobs
  const activeJobs = shownJobs.filter(job => isActiveJob(job.stage)).length
  const tab: Tab = page === "details" ? origin : page

  /* ---------------------------------------------------------------- boot */
  const libraryRefreshTimer = useRef<number | null>(null)
  const refreshLibraryRef = useRef<(target: ConsoleKind) => void>(() => undefined)
  const completedJobs = useRef(new Set<string>())

  useEffect(() => {
    preloadShells()
    const stage = getStage()
    if (inTauri()) void import("@tauri-apps/api/app").then(api => api.getVersion()).then(setVersion).catch(() => undefined)
    const load = Promise.allSettled([
      invokeCmd<Settings>("get_settings"),
      invokeCmd<DeliveryJob[]>("list_jobs"),
      invokeCmd<string | null>("system_drive_prefix"),
      invokeCmd<PackageSource[]>("list_package_sources"),
    ]).then(([saved, currentJobs, system, installed]) => {
      if (saved.status === "fulfilled") setSettings(saved.value)
      if (system.status === "fulfilled") setSystemDrive(system.value)
      if (currentJobs.status === "fulfilled") {
        setJobs(currentJobs.value)
        for (const job of currentJobs.value) if (job.stage === "complete") completedJobs.current.add(job.jobId)
      }
      if (installed.status === "fulfilled") setSources(installed.value)
      if (saved.status === "rejected") window.setTimeout(() => toast({ tone: "error", title: "Settings could not be read", text: errorText(saved.reason) }), 900)
      setLoaded(true)
    })
    const fonts = document.fonts?.ready.catch(() => undefined) || Promise.resolve()
    const ready = Promise.all([load, fonts])
    const run = stage ? stage.boot.run(ready, brandRef.current) : ready
    void run.then(() => reveal())

    let unlisten: (() => void) | undefined
    let unlistenRemoved: (() => void) | undefined
    if (inTauri()) {
      listen<DeliveryJob>("delivery-progress", event => {
        const job = event.payload
        setJobs(old => [job, ...old.filter(existing => existing.jobId !== job.jobId)])
        if (job.stage === "complete" && !completedJobs.current.has(job.jobId)) {
          completedJobs.current.add(job.jobId)
          const target: ConsoleKind = job.target === "ps4" ? "ps4" : "ps5"
          if (libraryRefreshTimer.current !== null) window.clearTimeout(libraryRefreshTimer.current)
          libraryRefreshTimer.current = window.setTimeout(() => {
            libraryRefreshTimer.current = null
            void refreshLibraryRef.current(target)
          }, 900)
        }
      }).then(stop => { unlisten = stop })
      listen<string>("delivery-removed", event => setJobs(old => old.filter(job => job.jobId !== event.payload))).then(stop => { unlistenRemoved = stop })
    }
    return () => {
      unlisten?.(); unlistenRemoved?.()
      if (libraryRefreshTimer.current !== null) window.clearTimeout(libraryRefreshTimer.current)
    }
  }, [])

  function reveal() {
    document.body.classList.remove("booting")
    setRevealed(true)
    window.setTimeout(() => document.getElementById("bootCaption")?.remove(), 600)
    if (!motionOK()) return
    document.querySelector(".header")?.animate([{ opacity: 0, transform: "translateY(-14px)" }, { opacity: 1, transform: "none" }], { duration: SPRINGS.soft.ms, easing: SPRINGS.soft.easing, fill: "backwards" })
    document.querySelector(".viewport")?.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 500, delay: 140, easing: "ease-out", fill: "backwards" })
  }

  /* ---------------------------------------------------------------- settings and appearance */
  useEffect(() => {
    setReduceMotion(settings.reduceMotion)
    try { window.localStorage.setItem("sspi.reduceMotion", settings.reduceMotion ? "1" : "0") } catch { /* storage unavailable */ }
  }, [settings.reduceMotion])
  useEffect(() => {
    applyAccentVars(appearance.accent)
    const stage = getStage()
    stage?.look.setAccent(appearance.accent)
    stage?.look.setPattern(appearance.pattern, true)
  }, [])
  useEffect(() => {
    document.documentElement.dataset.cards = appearance.cardStyle
    document.documentElement.dataset.cardSize = appearance.cardSize
  }, [appearance.cardStyle, appearance.cardSize])
  const appearanceRef = useRef(appearance)
  const setAppearance = useCallback((update: Appearance | ((prev: Appearance) => Appearance), origin?: { x: number; y: number }) => {
    const prev = appearanceRef.current
    const next = typeof update === "function" ? update(prev) : update
    const stage = getStage()
    if (next.accent !== prev.accent) { applyAccentVars(next.accent); stage?.look.setAccent(next.accent, origin) }
    if (next.pattern !== prev.pattern) stage?.look.setPattern(next.pattern)
    appearanceRef.current = next
    saveAppearance(next)
    setAppearanceState(next)
  }, [])

  useEffect(() => { trackSpeeds(shownJobs) }, [shownJobs])

  /* ---------------------------------------------------------------- receivers: detect at launch, then every minute */
  const probeGen = useRef(0)
  const warnedOutdated = useRef(new Set<string>())
  const loadReceiverRef = useRef<(target: ConsoleKind) => void>(() => undefined)
  const probeAll = useCallback(async () => {
    const gen = ++probeGen.current
    try {
      const list = await probeConsoles({ demo: demoRef.current })
      if (gen !== probeGen.current) return null
      const next: Partial<Record<ConsoleKind, ConsoleProbe>> = {}
      for (const probe of list) next[probe.target] = probe
      setProbes(next)
      setConsoleState(old => ({ ps5: probeState(next.ps5, old.ps5, settingsRef.current), ps4: probeState(next.ps4, old.ps4, settingsRef.current) }))
      for (const probe of list) {
        const key = `${probe.target}|${probe.host}|${probe.receiver.version}`
        if (probe.receiver.state !== "outdated" || demoRef.current || warnedOutdated.current.has(key)) continue
        warnedOutdated.current.add(key)
        const name = probe.target.toUpperCase()
        toast({
          tone: "info", title: `Your ${name} has an older receiver`,
          text: `Receiver ${probe.receiver.version || ""} is running. Load ${probe.receiver.expectedVersion} to use the library, cover changes and system information.`,
          actions: [{ label: "Load receiver", run: () => loadReceiverRef.current(probe.target) }],
        })
      }
      return next
    } catch {
      return null // Older backends or no app: keep the last known states.
    }
  }, [])

  const endpointsKey = `${settings.ps5Host}|${settings.ps5Port}|${settings.ps4Host}|${settings.ps4ReceiverPort}|${settings.ps4Transport}`
  useEffect(() => {
    if (!loaded && !demo) return
    void probeAll()
  }, [loaded, demo, endpointsKey, probeAll])
  useEffect(() => {
    let last = Date.now()
    const tick = () => { if (document.visibilityState === "visible") { last = Date.now(); void probeAll() } }
    const timer = window.setInterval(tick, PROBE_EVERY_MS)
    const onFocus = () => { if (Date.now() - last > 15_000) tick() }
    window.addEventListener("focus", onFocus)
    return () => { window.clearInterval(timer); window.removeEventListener("focus", onFocus) }
  }, [probeAll])

  const loadReceiver = useCallback(async (target: ConsoleKind) => {
    const name = target.toUpperCase()
    const endpoint = loaderEndpoint(settingsRef.current, target)
    const { port } = endpoint
    const host = endpoint.host || (demoRef.current ? "preview" : "")
    if (!host) {
      toast({ tone: "error", title: `Add your ${name} first`, text: `Enter the ${name} address in Options, Consoles.`, actions: [{ label: "Open Options", run: () => setOptions("consoles") }] })
      return
    }
    setLoadingReceiver(target)
    try {
      const result = await sendPayload({ id: `builtin:${target}-receiver`, target, host, port, demo: demoRef.current })
      toast({ tone: result.verified ? "success" : "info", title: result.verified ? `${name} receiver is running` : `Receiver sent to your ${name}`, text: result.message })
      const next = await probeAll()
      if (next?.[target]?.receiver.state === "online") void refreshLibraryRef.current(target)
    } catch (error) {
      const loader = target === "ps4" ? `GoldHEN BinLoader on port ${port}` : `an ELF loader on port ${port}`
      toast({ tone: "error", title: "The receiver didn't load", text: `${errorText(error)} Check that ${loader} is running on your ${name}.` })
    } finally {
      setLoadingReceiver(null)
    }
  }, [probeAll])
  loadReceiverRef.current = loadReceiver

  /* ---------------------------------------------------------------- library, per console */
  const [snapshots, setSnapshots] = useState<Partial<Record<ConsoleKind, { key: string; snapshot: CachedSnapshot }>>>({})
  const [librarySync, setLibrarySync] = useState<Partial<Record<ConsoleKind, LibrarySync>>>({})
  const libraryGen = useRef<Record<ConsoleKind, number>>({ ps4: 0, ps5: 0 })
  const endpointFor = (target: ConsoleKind) => {
    const { host, port } = receiverEndpoint(settingsRef.current, target)
    return { host, port, key: snapshotKey(target, host, port) }
  }

  useEffect(() => {
    if (demo) return
    libraryGen.current = { ps4: libraryGen.current.ps4 + 1, ps5: libraryGen.current.ps5 + 1 }
    const next: typeof snapshots = {}
    for (const target of CONSOLES) {
      const { host, port, key } = endpointFor(target)
      const cached = host ? cachedSnapshot(target, host, port) : null
      if (cached) next[target] = { key, snapshot: cached }
    }
    setSnapshots(next)
    setLibrarySync({})
  }, [endpointsKey, demo])

  const syncConsoleLibrary = useCallback(async (target: ConsoleKind) => {
    const { host, port, key } = endpointFor(target)
    const name = target.toUpperCase()
    if (demoRef.current) {
      const snapshot = await listConsoleLibrary({ target, host: "preview", port: 0, demo: true })
      setSnapshots(old => ({ ...old, [target]: { key: `${target}|preview`, snapshot: { entries: snapshot.entries, syncedAt: Date.now() } } }))
      setLibrarySync(old => ({ ...old, [target]: { endpoint: `${target}|preview`, state: "ready", syncedAt: Date.now(), messages: [] } }))
      return
    }
    if (!host) {
      setLibrarySync(old => ({ ...old, [target]: { endpoint: key, state: "offline", messages: [`Add your ${name} address in Options to see what's installed.`] } }))
      return
    }
    if (!inTauri()) return
    const probe = probesRef.current[target]
    const cached = cachedSnapshot(target, host, port)
    if (probe?.receiver.state === "online" && !hasCapability(probe, "installed-library-v1")) {
      setLibrarySync(old => ({ ...old, [target]: { endpoint: key, state: "unsupported", syncedAt: cached?.syncedAt, messages: [`Load receiver ${probe.receiver.expectedVersion} to list everything installed on your ${name}.`] } }))
      return
    }
    const gen = ++libraryGen.current[target]
    setLibrarySync(old => ({ ...old, [target]: { endpoint: key, state: "syncing", syncedAt: cached?.syncedAt, messages: [] } }))
    const stillCurrent = () => gen === libraryGen.current[target] && endpointFor(target).key === key
    try {
      const snapshot = await listConsoleLibrary({ target, host, port, demo: false })
      if (!stillCurrent()) return
      if (!snapshot.complete || snapshot.truncated || snapshot.errors.length > 0) {
        const messages = [...snapshot.errors, ...(snapshot.truncated ? ["The receiver stopped at its title limit; the saved library was kept."] : []), ...snapshot.metadataWarnings]
        setLibrarySync(old => ({ ...old, [target]: { endpoint: key, state: "partial", syncedAt: cached?.syncedAt, messages } }))
        return
      }
      const saved = saveSnapshot(target, host, port, snapshot)
      setSnapshots(old => ({ ...old, [target]: { key, snapshot: saved.snapshot } }))
      const messages = [...snapshot.metadataWarnings]
      if (!saved.persisted) messages.push("The new library is shown but couldn't be saved on this PC.")
      setLibrarySync(old => ({ ...old, [target]: { endpoint: key, state: "ready", syncedAt: saved.snapshot.syncedAt, messages } }))
    } catch (error) {
      if (!stillCurrent()) return
      setLibrarySync(old => ({ ...old, [target]: { endpoint: key, state: "offline", syncedAt: cached?.syncedAt, messages: [errorText(error)] } }))
    }
  }, [])
  refreshLibraryRef.current = syncConsoleLibrary

  // Sync each console once its receiver answers, and again when its snapshot goes stale.
  const syncedOnce = useRef(new Set<string>())
  useEffect(() => {
    for (const target of CONSOLES) {
      const probe = probes[target]
      // The offline preview shows both consoles' sample libraries whatever their simulated receivers say.
      if (!probe || (probe.receiver.state !== "online" && !demo)) continue
      const key = demo ? `${target}|preview` : endpointFor(target).key
      if (syncedOnce.current.has(key)) continue
      syncedOnce.current.add(key)
      void syncConsoleLibrary(target)
    }
  }, [probes, demo, syncConsoleLibrary])
  useEffect(() => {
    if (page !== "library") return
    const target = settings.activeConsole
    const snap = snapshots[target]
    const sync = librarySync[target]
    if (sync?.state === "syncing") return
    if (!snap || Date.now() - snap.snapshot.syncedAt > LIBRARY_STALE_MS) {
      if (probes[target]?.receiver.state === "online") void syncConsoleLibrary(target)
    }
  }, [page, settings.activeConsole])

  const history = useMemo(() => syncLibrary(shownJobs, !demo), [shownJobs, demo])
  const libraries = useMemo(() => {
    const result = {} as Record<ConsoleKind, LibraryView>
    for (const target of CONSOLES) {
      const snap = snapshots[target]
      const sync = librarySync[target]
      result[target] = snap
        ? { target, entries: snapshotEntries(target, snap.snapshot), authoritative: true, sync, syncedAt: snap.snapshot.syncedAt }
        : { target, entries: history.filter(entry => entry.target === target), authoritative: false, sync }
    }
    return result
  }, [snapshots, librarySync, history])
  const library = libraries[settings.activeConsole].entries

  const onIconChanged = useCallback((target: ConsoleKind, titleId: string, icon: string | null, customIcon: boolean) => {
    const { host, port, key } = endpointFor(target)
    if (demoRef.current) {
      setSnapshots(old => {
        const snap = old[target]
        if (!snap) return old
        const id = titleId.toUpperCase()
        return { ...old, [target]: { ...snap, snapshot: { ...snap.snapshot, entries: snap.snapshot.entries.map(item => item.titleId.toUpperCase() === id ? { ...item, icon: icon ?? item.icon, customIcon } : item) } } }
      })
      return
    }
    const next = patchSnapshotIcon(target, host, port, titleId, icon, customIcon)
    if (next) setSnapshots(old => ({ ...old, [target]: { key, snapshot: next } }))
  }, [])

  const pendingUpdates = useMemo(() => {
    const map: Record<string, string> = {}
    for (const job of shownJobs) {
      if (!activeTransfer(job) || kindOf(job) !== "update") continue
      const id = titleIdOf(job)
      if (id) map[id] = job.packageVersion || map[id] || ""
    }
    return map
  }, [shownJobs])

  const checkUpdate = useCallback(async (entry: LibraryEntry) => {
    const id = entry.titleId
    const current = updates[id]
    if (current && (current.state === "checking" || Date.now() - current.checkedAt < (current.state === "error" ? 120_000 : PACKAGE_CACHE_MS))) return
    if (demo) {
      setUpdates(old => ({ ...old, [id]: { state: "done", packages: demoPackages, checkedAt: Date.now() } }))
      return
    }
    if (!inTauri()) return
    const cached = packageCache.current.get(id)
    if (cached && Date.now() - cached.at < PACKAGE_CACHE_MS) {
      setUpdates(old => ({ ...old, [id]: { state: "done", packages: cached.packages, checkedAt: cached.at } }))
      return
    }
    setUpdates(old => ({ ...old, [id]: { state: "checking", checkedAt: Date.now() } }))
    try {
      const resolved = await withTimeout(invokeCmd<PackageCandidate[]>("resolve_packages", { gameTitleId: id, gameName: entry.name, gameRegion: "" }), RESOLVE_TIMEOUT_MS, RESOLVE_TIMEOUT_SENTINEL)
      putCapped(packageCache.current, id, { packages: resolved })
      setUpdates(old => ({ ...old, [id]: { state: "done", packages: resolved, checkedAt: Date.now() } }))
    } catch {
      setUpdates(old => ({ ...old, [id]: { state: "error", checkedAt: Date.now() } }))
    }
  }, [updates, demo])

  /* ---------------------------------------------------------------- search */
  const runSearch = useCallback(async (raw: string) => {
    const term = raw.trim()
    const gen = ++searchGen.current
    if (!term) { setSearchState("idle"); setResults([]); setSearchError(""); setSearchedFor(""); return }
    setSearchedFor(term)
    setRegion("All regions")
    resultsScroll.current = 0
    if (!demo) {
      const hit = searchCache.current.get(term.toLowerCase())
      if (hit && Date.now() - hit.at < SEARCH_CACHE_MS) { setResults(hit.games); setSearchState("success"); setSearchError(""); return }
    }
    setSearchState("loading")
    setSearchError("")
    try {
      let games: Game[]
      if (demo) {
        await sleep(300)
        const needle = term.toLowerCase()
        games = demoGames.filter(game => `${game.name} ${game.titleId}`.toLowerCase().includes(needle))
      } else {
        if (!inTauri()) throw new Error("Search needs the SSPI app.")
        games = await invokeCmd<Game[]>("search_games", { query: term })
        rememberHomebrew(games)
        putCapped(searchCache.current, term.toLowerCase(), { games })
      }
      if (gen !== searchGen.current) return
      setResults(games)
      setSearchState("success")
    } catch (error) {
      if (gen !== searchGen.current) return
      setResults([])
      setSearchState("error")
      setSearchError(errorText(error))
    }
  }, [demo])

  /* ---------------------------------------------------------------- details */
  const selectGame = useCallback(async (game: Game) => {
    const gen = ++resolveGen.current
    const readyPackages = game.packages || []
    setReceiverPrompt("")
    setSelected(game)
    setPackageError("")
    setPackageNote(null)
    setMetadata(null)
    const cached = packageCache.current.get(game.titleId)
    const fresh = !demo && readyPackages.length === 0 && cached !== undefined && Date.now() - cached.at < PACKAGE_CACHE_MS
    if (readyPackages.length > 0) { setPackages(readyPackages); setPackageState("success") }
    else if (fresh) { setPackages(cached.packages); setPackageState("success"); setPackageNote(`Checked ${ageText(cached.at)}, refreshing`) }
    else { setPackages([]); setPackageState("loading") }
    setMetadataState("loading")
    if (demo) {
      await sleep(220)
      if (gen !== resolveGen.current) return
      setPackages(demoPackages)
      setMetadata(demoMetadata(game))
      setPackageState("success")
      setMetadataState("success")
      return
    }
    if (!inTauri()) {
      setPackageState("error")
      setPackageError("Package lookup needs the SSPI app.")
      setMetadataState("error")
      return
    }
    const [resolved, meta] = await Promise.allSettled([
      readyPackages.length > 0
        ? Promise.resolve(readyPackages)
        : withTimeout(invokeCmd<PackageCandidate[]>("resolve_packages", { gameTitleId: game.titleId, gameName: game.name, gameRegion: game.region }), RESOLVE_TIMEOUT_MS, RESOLVE_TIMEOUT_SENTINEL),
      invokeCmd<MetadataResponse>("get_game_details", { name: game.name, titleId: game.titleId }),
    ])
    if (gen !== resolveGen.current) return
    if (resolved.status === "fulfilled") {
      if (readyPackages.length === 0) putCapped(packageCache.current, game.titleId, { packages: resolved.value })
      setPackages(resolved.value)
      if (resolved.value.some(p => p.expectedSize == null)) {
        void invokeCmd<PackageCandidate[]>("inspect_package_sizes", { packages: resolved.value }).then(enriched => { if (gen === resolveGen.current) setPackages(enriched) }).catch(() => undefined)
      }
      setPackageState("success")
      setPackageNote(null)
    } else {
      const timedOut = resolved.reason instanceof Error && resolved.reason.message === RESOLVE_TIMEOUT_SENTINEL
      if (fresh) setPackageNote(`Checked ${ageText(cached!.at)}, refresh failed`)
      else { setPackageState("error"); setPackageError(timedOut ? "Resolving is taking too long. Try again." : errorText(resolved.reason)) }
    }
    if (gen !== resolveGen.current) return
    if (meta.status === "fulfilled") { setMetadata(meta.value); setMetadataState("success") }
    else setMetadataState("error")
  }, [demo])

  // Tabs swap pages outright; only Back carries the game's case to where it came from.
  const go = useCallback((next: Tab) => setPage(next), [])

  const rememberRecent = useCallback((game: Game, persist: boolean) => {
    setRecent(old => {
      const next = [{ titleId: game.titleId, name: game.name, region: game.region, icon: game.icon }, ...old.filter(item => item.titleId.toUpperCase() !== game.titleId.toUpperCase())].slice(0, 10)
      if (persist) try { window.localStorage.setItem(RECENT_KEY, JSON.stringify(next.filter(item => !item.icon?.startsWith("data:") || item.icon.length < 200_000))) } catch { /* storage full or unavailable */ }
      return next
    })
  }, [])

  const openGame = useCallback((game: Game, from?: { el: HTMLElement | null; kind: "case" | "thumb" }) => {
    const stage = getStage()
    const key = game.titleId.toUpperCase()
    if (stage && from?.el) {
      if (from.kind === "case") stage.cases.retain(key)
      else stage.cases.expectFrom(key, from.el.getBoundingClientRect())
    }
    setOrigin(page === "details" ? origin : page)
    setPage("details")
    rememberRecent(game, !demo)
    void selectGame(game)
  }, [page, origin, selectGame, demo, rememberRecent])

  const goBack = useCallback(() => {
    getStage()?.cases.retain(selected?.titleId.toUpperCase() || "")
    setReceiverPrompt("")
    setPage(origin)
  }, [origin, selected])

  const step = useCallback((direction: -1 | 1) => {
    pressKey(direction < 0 ? "Q" : "E")
    const order = TABS.map(([id]) => id)
    go(order[(order.indexOf(tab) + direction + order.length) % order.length])
  }, [tab, go])

  /* ---------------------------------------------------------------- consoles */
  const markConsole = useCallback((target: ConsoleKind, ok: boolean) => setConsoleState(old => ({ ...old, [target]: ok ? "ok" : "fail" })), [])

  const changeConsole = useCallback(async (target: ConsoleKind) => {
    if (demo || !inTauri()) { setSettings(current => ({ ...current, activeConsole: target })); return }
    try { setSettings(await invokeCmd<Settings>("set_active_console", { console: target })) }
    catch (error) { toast({ tone: "error", title: "The console could not be switched", text: errorText(error) }) }
  }, [demo])

  const ensureConsole = useCallback(async (target: ConsoleKind) => {
    if (target === "ps4") {
      const host = settings.ps4Host.trim()
      if (!host) {
        toast({ tone: "error", title: "Add your PS4 first", text: settings.ps4Transport === "receiver" ? "Enter the PS4 address in Options, Consoles, enable GoldHEN BinLoader, then load the receiver." : "Enter the PS4 address in Options, Consoles, and enable the FTP server in GoldHEN.", actions: [{ label: "Open Options", run: () => setOptions("consoles") }] })
        return false
      }
      if (settings.ps4Transport === "receiver") {
        try { await invokeCmd<string>("test_ps4_receiver", { host, port: settings.ps4ReceiverPort }); markConsole("ps4", true); return true }
        catch (error) {
          markConsole("ps4", false)
          toast({ tone: "error", title: "The PS4 receiver isn't ready", text: `Nothing answered at ${host}:${settings.ps4ReceiverPort}. ${errorText(error)}`, actions: [{ label: "Load receiver", run: () => void loadReceiver("ps4") }, { label: "Open Options", run: () => setOptions("consoles") }] })
          return false
        }
      }
      try {
        const probe = await invokeCmd<Ps4Probe>("test_ps4", { host, port: settings.ps4FtpPort, user: settings.ps4FtpUser || undefined })
        markConsole("ps4", true)
        if (probe.worker !== "ready") toast({ tone: "info", title: "PS4 connected", text: probe.message })
        return true
      } catch (error) {
        markConsole("ps4", false)
        toast({ tone: "error", title: "The PS4 didn't answer", text: `${host}:${settings.ps4FtpPort}. ${errorText(error)}` })
        return false
      }
    }
    if (!settings.ps5Host.trim()) {
      setReceiverPrompt("Add your PS5 address, start the SSPI receiver ELF on the console, and test the connection before installing.")
      return false
    }
    try { await invokeCmd<string>("test_ps5", { host: settings.ps5Host, port: settings.ps5Port }); markConsole("ps5", true); return true }
    catch (error) {
      markConsole("ps5", false)
      setReceiverPrompt(`The PS5 receiver at ${settings.ps5Host}:${settings.ps5Port} did not answer. ${errorText(error)}`)
      return false
    }
  }, [settings.ps4Host, settings.ps4Transport, settings.ps4ReceiverPort, settings.ps4FtpPort, settings.ps4FtpUser, settings.ps5Host, settings.ps5Port, markConsole, loadReceiver])

  const downloadReceiver = useCallback(async () => {
    setPayloadBusy(true)
    try { toast({ tone: "success", title: "Receiver ELF saved", text: await invokeCmd<string>("export_receiver_payload") }) }
    catch (error) { toast({ tone: "error", title: "The receiver could not be saved", text: errorText(error) }) }
    finally { setPayloadBusy(false) }
  }, [])

  /* ---------------------------------------------------------------- delivery */
  const deliver = useCallback(async (game: Game, candidates: PackageCandidate[], available: PackageCandidate[], options: { provider?: string; from?: HTMLElement | null; target?: ConsoleKind } = {}): Promise<string[]> => {
    if (deliveryPending.current) return []
    const target = options.target || settings.activeConsole
    // Homebrew payload rows are added to Payloads instead of being sent as a package.
    let payloadKeys: string[] = []
    const payloads = candidates.filter(candidate => candidate.homebrew?.format === "payload")
    if (payloads.length) {
      const added: string[] = []
      for (const row of payloads) {
        if (!row.homebrew) continue
        if (demo) { toast({ tone: "info", title: "Offline preview", text: `${row.label} would be added to Payloads.` }); continue }
        try {
          await addHomebrewPayload({ url: row.url, name: game.name, version: row.version, sha256: row.expectedSha256, homebrew: row.homebrew })
          added.push(packageReleaseKey(row))
          toast({ tone: "success", title: "Added to Payloads", text: `${game.name}${row.version ? ` ${row.version}` : ""} is in Tools > Payloads. Send it to the ${consoleLabel(row.homebrew.platform)} from there.` })
        } catch (error) {
          toast({ tone: "warning", title: "Payload not added", text: errorText(error) })
        }
      }
      candidates = candidates.filter(candidate => candidate.homebrew?.format !== "payload")
      if (!candidates.length) return added
      payloadKeys = added
    }
    const packageDumps = packageTitle(settings, game.titleId)
    const packageOnly = packageDumps && settings.downloadPackageOnly
    const plan = planPackages(candidates, available, game.titleId, { packageDumps, autoBackports: autoBackports && target === "ps5", targetFw: settings.targetFw, catalog: available })
    if (plan.problems.length) { toast({ tone: "warning", title: "Check your selection", text: plan.problems.join(" ") }); return [] }
    if (!packageOnly) {
      const reason = plan.items.map(item => sendBlockReason(target, { titleId: game.titleId, backport: !!item.backport || packageKind(item.base) === "backport", homebrew: item.base.homebrew })).find(Boolean)
      if (reason) { toast({ tone: "warning", title: `This can't go to a ${target === "ps4" ? "PS4" : "PS5"}`, text: reason }); return [] }
    }
    if (!plan.items.length) return []
    const labels = plan.items.map(item => item.backport ? `${item.base.label} with backport` : item.base.label)
    if (demo) {
      toast({ tone: "info", title: "Offline preview", text: `${plural(plan.items.length, "transfer")} would start: ${labels.join(", ")}. Downloads are disabled in the preview.` })
      return []
    }
    deliveryPending.current = true
    setDeliveryBusy(true)
    const successful: string[] = [], failures: string[] = []
    try {
      if (!packageOnly && !await ensureConsole(target)) return []
      rememberCover(game.titleId, game.name, game.icon)
      const notificationIcon = await Promise.race([notificationCase(game.icon, game.titleId), new Promise<string | undefined>(resolve => window.setTimeout(() => resolve(game.icon), 2000))])
      for (const { base, backport } of plan.items) {
        const request: DeliveryRequest = {
          target,
          packageDumps,
          package: { ...base, kind: packageKind(base) }, titleId: game.titleId,
          titleName: game.name, icon: notificationIcon, archiveParts: archivePartsFor(base, available), provider: options.provider,
          ...(backport ? { backport: { package: { ...backport, kind: "backport" }, parts: archivePartsFor(backport, available) } } : {}),
        }
        try {
          // The backend checks space after deciding whether an existing base can be reused.
          await invokeCmd<string>("start_delivery", { request })
          successful.push(packageReleaseKey(base))
          if (backport) successful.push(packageReleaseKey(backport))
        } catch (error) { failures.push(`${base.label}: ${errorText(error)}`) }
      }
      const cover = { cover: game.icon, title: game.name, titleId: game.titleId }
      if (successful.length) {
        const stage = getStage(), tabEl = document.querySelector<HTMLElement>('.tab[data-page="downloads"]')
        if (stage && options.from && tabEl && motionOK()) {
          const fromRect = stage.cases.worldRect(options.from)
          void stage.fx.flyCase(cover, fromRect, tabEl.getBoundingClientRect(), { endWidth: 24 }).then(pulseDownloadsTab)
        }
        const bytes = plan.items.reduce((sum, item) => sum + [...archivePartsFor(item.base, available), ...(item.backport ? archivePartsFor(item.backport, available) : [])].reduce((total, part) => total + (part.expectedSize || 0), 0), 0)
        toast({
          tone: "success", game: cover, title: packageOnly ? "Added to Downloads for packaging" : "Added to Downloads",
          text: `${game.name}: ${labels.join(", ")}.${bytes ? ` ${fmtBytes(bytes)}` : ""}${packageOnly ? " stays on this PC." : ` for your ${target.toUpperCase()}.`}`,
          actions: [{ label: "View downloads", run: () => { setDlOpen(game.titleId.toUpperCase()); setPage("downloads") } }],
        })
      }
      if (failures.length) toast({ tone: "error", title: successful.length ? "Some selections didn't start" : "The transfer didn't start", text: failures.slice(0, 3).join(" ") })
      return [...payloadKeys, ...successful]
    } finally {
      deliveryPending.current = false
      setDeliveryBusy(false)
    }
  }, [settings.activeConsole, settings.packageDumps, settings.downloadPackageOnly, settings.targetFw, autoBackports, demo, ensureConsole])

  const queueUpdate = useCallback((entry: LibraryEntry, candidate: PackageCandidate, available: PackageCandidate[], anchor: HTMLElement | null) => {
    if (pendingUpdates[entry.titleId] !== undefined) return
    if (compareVersions(packageVersion(candidate), installedVersion(entry)) <= 0) return
    void deliver(gameFromEntry(entry), [candidate], available, { from: anchor, target: entry.target })
  }, [pendingUpdates, deliver])

  /* ---------------------------------------------------------------- offline preview (dev server only) */
  const leaveDemo = useCallback(() => {
    setDemo(false)
    setResults([])
    setSearchState("idle")
    setQuery("")
    setSelected(null)
    setProbes({})
    syncedOnce.current.clear()
    setPage("library")
    setOptions(null)
  }, [])

  /* ---------------------------------------------------------------- keyboard */
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!revealed || event.defaultPrevented) return
      if (options || receiverPrompt || document.querySelector(".dialog")) return
      const k = event.key
      if (!isTyping() && !event.ctrlKey && !event.altKey && !event.metaKey) {
        if (k === "q" || k === "Q") { event.preventDefault(); step(-1); return }
        if (k === "e" || k === "E") { event.preventDefault(); step(1); return }
        if (k === "o" || k === "O") { event.preventDefault(); setOptions("consoles"); return }
      }
      runPageKeys(event)
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [revealed, options, receiverPrompt, step])

  /* ---------------------------------------------------------------- render */
  const entryFor = selected ? library.find(entry => entry.titleId.toUpperCase() === selected.titleId.toUpperCase()) : undefined
  const build = version ? `SSPI Windows ${displayVersion(version)}` : "SSPI Windows"
  const backLabel = origin === "downloads" ? "Downloads" : origin === "library" ? "Library" : origin === "tools" ? "Tools" : searchState === "idle" ? "Search" : "Search results"
  return (
    <ProviderContext settings={settings}>
      <Updater demo={demo} />
      <div className="app">
        <Header
          tab={tab} activeJobs={activeJobs} settings={settings} consoleState={consoleState} probes={probes} demo={demo} brandRef={brandRef} menuDisabled={Boolean(options || receiverPrompt)}
          optionsTabs={options ? <OptionsTabs tab={options} onTab={setOptions} /> : undefined}
          loadingReceiver={loadingReceiver} onLoadReceiver={target => void loadReceiver(target)}
          onTab={next => go(next)} onStep={step} onConsole={target => void changeConsole(target)}
          onOptions={() => options ? setCloseOptions(n => n + 1) : setOptions("consoles")} onManageConsoles={() => setOptions("consoles")}
        />
        <main className="viewport" id="viewport">
          {page === "library" && (
            <LibraryPage
              key="library"
              target={settings.activeConsole} libraries={libraries} probes={probes} settings={settings} demo={demo}
              selectedKey={libSelected[settings.activeConsole]} onSelect={key => setLibSelected(old => ({ ...old, [settings.activeConsole]: key }))}
              expanded={libExpanded} setExpanded={setLibExpanded}
              updates={updates} pendingUpdates={pendingUpdates} onCheckUpdate={checkUpdate} onQueueUpdate={queueUpdate} deliveryBusy={deliveryBusy}
              onConsole={target => void changeConsole(target)} onRefresh={target => void syncConsoleLibrary(target)}
              onOpenGame={(entry, el) => openGame(gameFromEntry(entry), { el, kind: "thumb" })}
              onIconChanged={onIconChanged} onLoadReceiver={target => void loadReceiver(target)} loadingReceiver={loadingReceiver}
              onOptions={setOptions} onSearch={() => go("search")} tintOn={appearance.gameTint}
            />
          )}
          {page === "search" && (
            <SearchPage
              key="search"
              query={query} setQuery={setQuery} onSearch={runSearch} searchState={searchState} searchError={searchError} searchedFor={searchedFor}
              results={results} region={region} setRegion={setRegion} resultsScroll={resultsScroll}
              library={library} recent={recent} onClearRecent={() => { setRecent([]); try { window.localStorage.removeItem(RECENT_KEY) } catch { /* ignore */ } }}
              onOpen={openGame} sources={sources} demo={demo} activeConsole={settings.activeConsole}
              onOptions={setOptions}
            />
          )}
          {page === "details" && selected && (
            <DetailsPage
              key={`details-${variantKey(selected)}`}
              game={selected} packages={packages} packageState={packageState} packageError={packageError} packageNote={packageNote}
              metadata={metadata} metadataState={metadataState} demo={demo} settings={settings}
              autoBackports={autoBackports} setAutoBackports={setAutoBackports} deliveryBusy={deliveryBusy}
              libraryEntry={entryFor} pendingUpdate={pendingUpdates[selected.titleId.toUpperCase()]}
              backLabel={backLabel}
              onBack={goBack} onRetry={() => void selectGame(selected)} onVariant={game => void selectGame(game)}
              onInstall={(candidates, available, from, provider) => deliver(selected, candidates, available, { from, provider })}
              onOptions={setOptions} tintOn={appearance.gameTint}
            />
          )}
          {page === "downloads" && (
            <DownloadsPage
              key="downloads"
              jobs={shownJobs} demo={demo} settings={settings} systemDrive={systemDrive}
              filter={dlFilter} setFilter={setDlFilter} open={dlOpen} setOpen={setDlOpen} drawer={dlDrawer} setDrawer={setDlDrawer}
              statsForNerds={appearance.statsForNerds} setStatsForNerds={value => setAppearance(prev => ({ ...prev, statsForNerds: value }))}
              ensureConsole={ensureConsole} onOptions={setOptions} onSearch={() => go("search")}
              onOpenGame={group => openGame({ titleId: group.titleId || group.key, name: group.title, region: "", icon: group.icon })}
              tintOn={appearance.gameTint} cardStyle={appearance.cardStyle} cardSize={appearance.cardSize}
            />
          )}
          {page === "tools" && (
            <ToolsPage
              key="tools"
              tab={toolsTab} setTab={setToolsTab} target={settings.activeConsole} settings={settings} demo={demo}
              probes={probes} onConsole={target => void changeConsole(target)}
              onLoadReceiver={target => void loadReceiver(target)} onReceiverLoaded={() => void probeAll()}
            />
          )}
        </main>
      </div>
      {options && (
        <OptionsOverlay
          tab={options} setTab={setOptions} onClose={() => setOptions(null)}
          settings={settings} setSettings={setSettings} sources={sources} setSources={setSources}
          appearance={appearance} setAppearance={setAppearance} demo={demo} onLeaveDemo={leaveDemo}
          downloadReceiver={downloadReceiver} payloadBusy={payloadBusy} onConsoleTested={markConsole} build={build} closeRequest={closeOptions}
        />
      )}
      <ReceiverDialog
        open={Boolean(receiverPrompt)} message={receiverPrompt} host={settings.ps5Host} port={settings.ps5Port} busy={payloadBusy}
        onClose={() => setReceiverPrompt("")} onDownload={downloadReceiver}
        onSettings={() => { setReceiverPrompt(""); setOptions("consoles") }}
      />
    </ProviderContext>
  )
}
