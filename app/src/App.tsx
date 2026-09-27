import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react"

import { ProviderContext } from "@/components/ProviderContext"
import { Dock, Header, TitleBar, pressKey, pulseDownloadsTab, type ConsoleState, type Hint, type Tab } from "@/components/Shell"
import { SearchPage } from "@/components/SearchPage"
import { DetailsPage } from "@/components/DetailsPage"
import { DownloadsPage } from "@/components/DownloadsPage"
import { OptionsOverlay, type OptionsTab } from "@/components/OptionsOverlay"
import { ReceiverDialog } from "@/components/Dialogs"
import { toast } from "@/components/toasts"
import { demoGames, demoJobs, demoMetadata, demoPackages } from "@/data/demo"
import { applyAccentVars, loadAppearance, saveAppearance, type Appearance } from "@/lib/appearance"
import { notificationCase } from "@/lib/covers"
import { sendBlockReason } from "@/lib/consoles"
import { activeTransfer, kindOf, titleIdOf } from "@/lib/downloads"
import { compareVersions, errorText, fmtBytes, plural } from "@/lib/format"
import { isTyping, runPageKeys } from "@/lib/keys"
import { cachedPs4Snapshot, installedVersion, ps4EndpointKey, ps4SnapshotEntries, rememberCover, savePs4Snapshot, syncLibrary, type CachedPs4Snapshot, type LibraryEntry } from "@/lib/library"
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
  type Ps4LibrarySnapshot,
  type Ps4Probe,
  type Settings,
} from "@/types"

const invokeCmd = <T,>(name: string, args?: Record<string, unknown>) => invoke<T>(name, args)
const inTauri = () => "__TAURI_INTERNALS__" in window

// Resolve/search snappiness: soft invoke timeout, capped in-memory caches.
const RESOLVE_TIMEOUT_MS = 45_000
const SEARCH_CACHE_MS = 10 * 60 * 1_000
const PACKAGE_CACHE_MS = 30 * 60 * 1_000
const CACHE_CAP = 50
const RESOLVE_TIMEOUT_SENTINEL = "resolve-timeout"

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
  return mins === 1 ? "1 min ago" : `${mins} min ago`
}

export type Page = "search" | "downloads" | "details"
export type UpdateInfo = { state: "checking" | "done" | "error"; packages?: PackageCandidate[]; checkedAt: number }
type DockState = { context: ReactNode; hints: Hint[] }

const APP_VERSION = typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : ""
const displayVersion = (value: string) => value.replace(/\.0$/, "")
type LibrarySyncResult = { endpoint: string; state: "syncing" | "ready" | "partial" | "offline"; syncedAt?: number; messages: string[] }

const gameFromEntry = (entry: LibraryEntry): Game => ({ titleId: entry.titleId, name: entry.name, region: "", icon: entry.cover })

export default function App() {
  /* ---------------------------------------------------------------- data */
  const [settings, setSettings] = useState<Settings>(blankSettings)
  const [jobs, setJobs] = useState<DeliveryJob[]>([])
  const [systemDrive, setSystemDrive] = useState<string | null>(null)
  const [sources, setSources] = useState<PackageSource[]>([])
  const [demo, setDemo] = useState(false)
  const [appearance, setAppearanceState] = useState<Appearance>(loadAppearance)
  const [consoleState, setConsoleState] = useState<Record<ConsoleKind, ConsoleState>>({ ps5: "unknown", ps4: "unknown" })
  const settingsRef = useRef(settings)
  settingsRef.current = settings
  const ps4LibraryGeneration = useRef(0)
  const completedPs4Jobs = useRef(new Set<string>())
  const libraryRefreshTimer = useRef<number | null>(null)
  const refreshLibraryRef = useRef<() => void>(() => undefined)
  const [ps4SnapshotState, setPs4SnapshotState] = useState<{ endpoint: string; snapshot: CachedPs4Snapshot } | null>(null)
  const [librarySyncResult, setLibrarySyncResult] = useState<LibrarySyncResult | null>(null)
  const [version, setVersion] = useState(APP_VERSION)
  const [revealed, setRevealed] = useState(false)
  const brandRef = useRef<HTMLImageElement>(null)

  /* ---------------------------------------------------------------- navigation */
  const [page, setPage] = useState<Page>("search")
  const [origin, setOrigin] = useState<Tab>("search")
  const [options, setOptions] = useState<OptionsTab | null>(null)
  const [dock, setDockState] = useState<DockState>({ context: "Search", hints: [] })
  const setDock = useCallback((next: DockState) => setDockState(next), [])

  /* ---------------------------------------------------------------- search and library */
  const [query, setQuery] = useState("")
  const [searchState, setSearchState] = useState<LoadState>("idle")
  const [searchError, setSearchError] = useState("")
  const [searchedFor, setSearchedFor] = useState("")
  const [results, setResults] = useState<Game[]>([])
  const [region, setRegion] = useState("All regions")
  const [libIndex, setLibIndex] = useState(0)
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
        for (const job of currentJobs.value) if (job.target === "ps4" && job.stage === "complete") completedPs4Jobs.current.add(job.jobId)
      }
      if (installed.status === "fulfilled") setSources(installed.value)
      if (saved.status === "rejected") window.setTimeout(() => toast({ tone: "error", title: "Settings could not be read", text: errorText(saved.reason) }), 900)
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
        if (job.target === "ps4" && job.stage === "complete" && !completedPs4Jobs.current.has(job.jobId)) {
          completedPs4Jobs.current.add(job.jobId)
          if (libraryRefreshTimer.current !== null) window.clearTimeout(libraryRefreshTimer.current)
          libraryRefreshTimer.current = window.setTimeout(() => {
            libraryRefreshTimer.current = null
            void refreshLibraryRef.current()
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
    const parts: Array<[string, number, number]> = [[".titlebar", 0, -8], [".header", 60, -14], [".dock", 120, 14]]
    for (const [selector, delay, y] of parts) {
      document.querySelector(selector)?.animate([{ opacity: 0, transform: `translateY(${y}px)` }, { opacity: 1, transform: "none" }], { duration: SPRINGS.soft.ms, delay, easing: SPRINGS.soft.easing, fill: "backwards" })
    }
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

  /* ---------------------------------------------------------------- library */
  const ps4Endpoint = ps4EndpointKey(settings.ps4Host, settings.ps4ReceiverPort)
  const ps4LibraryContext = `${settings.activeConsole}|${settings.ps4Transport}|${ps4Endpoint}`
  const ps4LibraryEnabled = !demo && settings.activeConsole === "ps4" && settings.ps4Transport === "receiver"

  useEffect(() => {
    ps4LibraryGeneration.current += 1
    const cached = settings.ps4Host.trim() ? cachedPs4Snapshot(settings.ps4Host, settings.ps4ReceiverPort) : null
    setPs4SnapshotState(cached ? { endpoint: ps4Endpoint, snapshot: cached } : null)
  }, [ps4LibraryContext])

  const syncPs4Library = useCallback(async () => {
    const current = settingsRef.current
    const host = current.ps4Host.trim()
    const port = current.ps4ReceiverPort
    const endpoint = ps4EndpointKey(host, port)
    if (current.activeConsole !== "ps4" || current.ps4Transport !== "receiver") return
    if (!host) {
      setLibrarySyncResult({ endpoint, state: "offline", messages: ["Set the PS4 receiver address in Options to sync installed titles."] })
      return
    }
    if (!inTauri()) return
    const generation = ++ps4LibraryGeneration.current
    const cached = cachedPs4Snapshot(host, port)
    setLibrarySyncResult({ endpoint, state: "syncing", syncedAt: cached?.syncedAt, messages: [] })
    const stillCurrent = () => {
      const latest = settingsRef.current
      return generation === ps4LibraryGeneration.current && latest.activeConsole === "ps4" && latest.ps4Transport === "receiver" && ps4EndpointKey(latest.ps4Host, latest.ps4ReceiverPort) === endpoint
    }
    try {
      const snapshot = await invokeCmd<Ps4LibrarySnapshot>("list_ps4_library", { host, port })
      if (!stillCurrent()) return
      if (!snapshot.complete || snapshot.truncated || snapshot.errors.length > 0) {
        setLibrarySyncResult({ endpoint, state: "partial", syncedAt: cached?.syncedAt, messages: [...snapshot.errors, ...(snapshot.truncated ? ["The receiver stopped at its title limit; the saved library was kept."] : []), ...snapshot.metadataWarnings] })
        return
      }
      const saved = savePs4Snapshot(host, port, snapshot)
      setPs4SnapshotState({ endpoint, snapshot: saved.snapshot })
      const messages = [...snapshot.metadataWarnings]
      if (!saved.persisted) messages.push("The new snapshot is visible now but could not be saved in local storage.")
      setLibrarySyncResult({ endpoint, state: "ready", syncedAt: saved.snapshot.syncedAt, messages })
    } catch (error) {
      if (!stillCurrent()) return
      setLibrarySyncResult({ endpoint, state: "offline", syncedAt: cached?.syncedAt, messages: [errorText(error)] })
    }
  }, [])
  refreshLibraryRef.current = syncPs4Library

  useEffect(() => {
    if (ps4LibraryEnabled) void syncPs4Library()
  }, [ps4LibraryContext, ps4LibraryEnabled, syncPs4Library])

  const cachedSnapshot = ps4SnapshotState?.endpoint === ps4Endpoint ? ps4SnapshotState.snapshot : null
  const librarySyncView = ps4LibraryEnabled ? (() => {
    const result = librarySyncResult?.endpoint === ps4Endpoint ? librarySyncResult : null
    const syncedAt = cachedSnapshot?.syncedAt || result?.syncedAt
    const lastSync = syncedAt ? ` Last complete sync ${ageText(syncedAt)}.` : ""
    const state = result?.state || "syncing"
    const message = state === "syncing" ? "Checking installed titles on your PS4…"
      : state === "ready" ? `PS4 library synced.${lastSync}`
        : state === "partial" ? `The scan was incomplete; showing the last complete PS4 library.${lastSync}`
          : `${result?.messages[0] || "The PS4 receiver is unavailable."}${lastSync}`
    return { state, message, messages: result?.messages || [] }
  })() : undefined

  const library = useMemo(() => {
    if (ps4LibraryEnabled && cachedSnapshot) return ps4SnapshotEntries(cachedSnapshot)
    return syncLibrary(shownJobs, !demo).filter(entry => entry.target === settings.activeConsole)
  }, [shownJobs, demo, settings.activeConsole, ps4LibraryEnabled, cachedSnapshot])
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
        if (!inTauri()) throw new Error("Search needs the SSPI app. Open the offline preview to explore the interface.")
        games = await invokeCmd<Game[]>("search_games", { query: term })
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

  const go = useCallback((next: Tab) => {
    if (page === "details" && selected) getStage()?.cases.retain(selected.titleId.toUpperCase())
    setPage(next)
  }, [page, selected])

  const openGame = useCallback((game: Game, from?: { el: HTMLElement | null; kind: "case" | "thumb" }) => {
    const stage = getStage()
    const key = game.titleId.toUpperCase()
    if (stage && from?.el) {
      if (from.kind === "case") stage.cases.retain(key)
      else stage.cases.expectFrom(key, from.el.getBoundingClientRect())
    }
    setOrigin(page === "details" ? origin : page)
    setPage("details")
    void selectGame(game)
  }, [page, origin, selectGame])

  const goBack = useCallback(() => {
    getStage()?.cases.retain(selected?.titleId.toUpperCase() || "")
    setReceiverPrompt("")
    setPage(origin)
  }, [origin, selected])

  const step = useCallback((direction: -1 | 1) => {
    pressKey(direction < 0 ? "Q" : "E")
    const order: Tab[] = ["search", "downloads"]
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
          toast({ tone: "error", title: "The PS4 receiver isn't ready", text: `Nothing answered at ${host}:${settings.ps4ReceiverPort}. ${errorText(error)}`, actions: [{ label: "Open Options", run: () => setOptions("consoles") }] })
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
  }, [settings.ps4Host, settings.ps4Transport, settings.ps4ReceiverPort, settings.ps4FtpPort, settings.ps4FtpUser, settings.ps5Host, settings.ps5Port, markConsole])

  const downloadReceiver = useCallback(async () => {
    setPayloadBusy(true)
    try { toast({ tone: "success", title: "Receiver ELF saved", text: await invokeCmd<string>("export_receiver_payload") }) }
    catch (error) { toast({ tone: "error", title: "The receiver could not be saved", text: errorText(error) }) }
    finally { setPayloadBusy(false) }
  }, [])

  /* ---------------------------------------------------------------- delivery */
  const deliver = useCallback(async (game: Game, candidates: PackageCandidate[], available: PackageCandidate[], options: { provider?: string; from?: HTMLElement | null } = {}): Promise<string[]> => {
    if (deliveryPending.current) return []
    const target = settings.activeConsole
    const packageOnly = settings.packageDumps && settings.downloadPackageOnly
    const plan = planPackages(candidates, available, game.titleId, { packageDumps: settings.packageDumps, autoBackports: autoBackports && target === "ps5", targetFw: settings.targetFw, catalog: available })
    if (plan.problems.length) { toast({ tone: "warning", title: "Check your selection", text: plan.problems.join(" ") }); return [] }
    if (!packageOnly && target === "ps4") {
      const reason = plan.items.map(item => sendBlockReason(target, { titleId: game.titleId, backport: !!item.backport || packageKind(item.base) === "backport" })).find(Boolean)
      if (reason) { toast({ tone: "warning", title: "This can't go to a PS4", text: reason }); return [] }
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
      return successful
    } finally {
      deliveryPending.current = false
      setDeliveryBusy(false)
    }
  }, [settings.activeConsole, settings.packageDumps, settings.downloadPackageOnly, settings.targetFw, autoBackports, demo, ensureConsole])

  const queueUpdate = useCallback((entry: LibraryEntry, candidate: PackageCandidate, available: PackageCandidate[], anchor: HTMLElement | null) => {
    if (pendingUpdates[entry.titleId] !== undefined) return
    if (compareVersions(packageVersion(candidate), installedVersion(entry)) <= 0) return
    void deliver(gameFromEntry(entry), [candidate], available, { from: anchor })
  }, [pendingUpdates, deliver])

  /* ---------------------------------------------------------------- offline preview */
  const enterDemo = useCallback(() => {
    setDemo(true)
    setResults([])
    setSearchState("idle")
    setQuery("")
    setPage("search")
    toast({ tone: "info", title: "Offline preview", text: "Titles, packages and transfers are simulated. Network calls and installs are off until you leave the preview in Options." })
  }, [])
  const leaveDemo = useCallback(() => {
    setDemo(false)
    setResults([])
    setSearchState("idle")
    setQuery("")
    setSelected(null)
    setPage("search")
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
  return (
    <ProviderContext settings={settings}>
      <div className="app">
        <TitleBar />
        <Header
          tab={tab} activeJobs={activeJobs} settings={settings} consoleState={consoleState} demo={demo} brandRef={brandRef} menuDisabled={Boolean(options || receiverPrompt)}
          onTab={next => go(next)} onStep={step} onConsole={target => void changeConsole(target)}
          onOptions={() => setOptions("consoles")} onManageConsoles={() => setOptions("consoles")}
        />
        <main className="viewport" id="viewport">
          {page === "search" && (
            <SearchPage
              key="search"
              query={query} setQuery={setQuery} onSearch={runSearch} searchState={searchState} searchError={searchError} searchedFor={searchedFor}
              results={results} region={region} setRegion={setRegion} resultsScroll={resultsScroll}
              library={library} libIndex={libIndex} setLibIndex={setLibIndex} libExpanded={libExpanded} setLibExpanded={setLibExpanded}
              updates={updates} pendingUpdates={pendingUpdates} onCheckUpdate={checkUpdate} onQueueUpdate={queueUpdate}
              onOpen={openGame} openEntry={(entry, el) => openGame(gameFromEntry(entry), { el, kind: "case" })}
              sources={sources} demo={demo} activeConsole={settings.activeConsole} deliveryBusy={deliveryBusy}
              librarySync={librarySyncView} onRefreshLibrary={() => void syncPs4Library()} libraryIsAuthoritative={Boolean(ps4LibraryEnabled && cachedSnapshot)}
              onOptions={setOptions} onDemo={enterDemo} onDock={setDock} tintOn={appearance.gameTint}
            />
          )}
          {page === "details" && selected && (
            <DetailsPage
              key={`details-${variantKey(selected)}`}
              game={selected} packages={packages} packageState={packageState} packageError={packageError} packageNote={packageNote}
              metadata={metadata} metadataState={metadataState} demo={demo} settings={settings}
              autoBackports={autoBackports} setAutoBackports={setAutoBackports} deliveryBusy={deliveryBusy}
              libraryEntry={entryFor} pendingUpdate={pendingUpdates[selected.titleId.toUpperCase()]}
              backLabel={origin === "downloads" ? "Downloads" : searchState === "idle" ? "Your library" : "Search results"}
              onBack={goBack} onRetry={() => void selectGame(selected)} onVariant={game => void selectGame(game)}
              onInstall={(candidates, available, from, provider) => deliver(selected, candidates, available, { from, provider })}
              onOptions={setOptions} onDock={setDock} tintOn={appearance.gameTint}
            />
          )}
          {page === "downloads" && (
            <DownloadsPage
              key="downloads"
              jobs={shownJobs} demo={demo} settings={settings} systemDrive={systemDrive}
              filter={dlFilter} setFilter={setDlFilter} open={dlOpen} setOpen={setDlOpen} drawer={dlDrawer} setDrawer={setDlDrawer}
              statsForNerds={appearance.statsForNerds} setStatsForNerds={value => setAppearance(prev => ({ ...prev, statsForNerds: value }))}
              ensureConsole={ensureConsole} onOptions={setOptions} onDock={setDock} onSearch={() => go("search")}
              onOpenGame={group => openGame({ titleId: group.titleId || group.key, name: group.title, region: "", icon: group.icon })}
              tintOn={appearance.gameTint}
            />
          )}
        </main>
        <Dock context={dock.context} hints={dock.hints} build={build} />
      </div>
      {options && (
        <OptionsOverlay
          tab={options} setTab={setOptions} onClose={() => setOptions(null)}
          settings={settings} setSettings={setSettings} sources={sources} setSources={setSources}
          appearance={appearance} setAppearance={setAppearance} demo={demo} onLeaveDemo={leaveDemo}
          downloadReceiver={downloadReceiver} payloadBusy={payloadBusy} onConsoleTested={markConsole} build={build}
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

