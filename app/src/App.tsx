import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"
import { getCurrentWindow } from "@tauri-apps/api/window"
import { open } from "@tauri-apps/plugin-dialog"
import {
  ArrowLeft,
  ArrowRight,
  Check,
  CircleAlert,
  CircleCheck,
  Download,
  FolderOpen,
  Gamepad2,
  HardDriveDownload,
  Home,
  Info,
  Library,
  LoaderCircle,
  Minus,
  MonitorUp,
  PackageOpen,
  Play,
  RefreshCw,
  Search,
  Settings as SettingsIcon,
  ShieldCheck,
  Sparkles,
  Square,
  Wifi,
  WifiOff,
  X,
} from "lucide-react"
import { FormEvent, useCallback, useEffect, useRef, useState } from "react"

import { CoverImage, CaseCover, useCoverSource } from "@/lib/covers"
import { DownloadGroups } from "@/components/DownloadGroups"
import sspiLogo from "@/assets/sspi-logo.svg"
import texLines from "@/assets/tex-lines.jpg"
import texDots from "@/assets/tex-dots.jpg"
import { cn } from "@/lib/utils"
import { demoGames, demoJobs, demoMetadata, demoPackages } from "@/data/demo"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Progress } from "@/components/ui/progress"
import { Skeleton } from "@/components/ui/skeleton"
import { Switch } from "@/components/ui/switch"
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip"
import {
  blankSettings,
  isActiveJob,
  type DeliveryJob,
  type CatalogSection,
  type Game,
  type GameCatalog,
  type LoadState,
  type LocalPackage,
  type ManualCandidate,
  type ManualItem,
  type MetadataResponse,
  type PackageCandidate,
  type PackageSource,
  type Page,
  type Settings,
} from "@/types"

const invokeCmd = <T,>(name: string, args?: Record<string, unknown>) => invoke<T>(name, args)

const errorText = (error: unknown) => {
  if (error instanceof Error) return error.message
  if (typeof error === "string") return error
  try { return JSON.stringify(error) } catch { return "The operation failed" }
}

const pageCopy: Record<Page, { title: string; subtitle: string }> = {
  Discover: { title: "Catalog", subtitle: "Browse titles from your package sources." },
  Search: { title: "Search", subtitle: "Search PS4 and PS5 titles." },
  Downloads: { title: "Downloads", subtitle: "Transfers, local packages, and install status." },
  Settings: { title: "Settings", subtitle: "Package Sources, receiver, provider, and metadata configuration." },
}

// Resolve/search snappiness: soft invoke timeout, capped in-memory caches.
const RESOLVE_TIMEOUT_MS = 45_000
const SEARCH_CACHE_MS = 10 * 60 * 1_000
const PACKAGE_CACHE_MS = 30 * 60 * 1_000
const CACHE_CAP = 50
const RESOLVE_TIMEOUT_SENTINEL = "resolve-timeout"

const withTimeout = <T,>(promise: Promise<T>, ms: number, sentinel: string): Promise<T> => new Promise((resolve, reject) => {
  const timer = window.setTimeout(() => reject(new Error(sentinel)), ms)
  promise.then(
    (value) => { window.clearTimeout(timer); resolve(value) },
    (error) => { window.clearTimeout(timer); reject(error) },
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

export default function App() {
  const [settings, setSettings] = useState<Settings>(blankSettings)
  const [systemDrive, setSystemDrive] = useState<string | null>(null)
  const [booting, setBooting] = useState(true)
  const [demo, setDemo] = useState(false)
  const [page, setPage] = useState<Page>("Discover")
  const [query, setQuery] = useState("")
  const [games, setGames] = useState<Game[]>([])
  const [catalogSections, setCatalogSections] = useState<CatalogSection[]>([])
  const [catalogState, setCatalogState] = useState<LoadState>("idle")
  const [catalogError, setCatalogError] = useState("")
  const [searchState, setSearchState] = useState<LoadState>("idle")
  const [searchError, setSearchError] = useState("")
  const [selected, setSelected] = useState<Game | null>(null)
  const [packages, setPackages] = useState<PackageCandidate[]>([])
  const [packageState, setPackageState] = useState<LoadState>("idle")
  const [packageError, setPackageError] = useState("")
  const [details, setDetails] = useState<MetadataResponse | null>(null)
  const [metadataState, setMetadataState] = useState<LoadState>("idle")
  const [jobs, setJobs] = useState<DeliveryJob[]>([])
  const [notice, setNotice] = useState<{ tone: "info" | "error" | "success"; text: string } | null>(null)
  const [receiverPrompt, setReceiverPrompt] = useState("")
  const [payloadBusy, setPayloadBusy] = useState(false)
  const [packageNote, setPackageNote] = useState<string | null>(null)
  // Resolve/search guards + caches (instant re-opens, no stuck loaders)
  const resolveGen = useRef(0)
  const searchCache = useRef(new Map<string, { at: number; games: Game[] }>())
  const packageCache = useRef(new Map<string, { at: number; packages: PackageCandidate[] }>())

  useEffect(() => {
    Promise.allSettled([
      invokeCmd<Settings>("get_settings"),
      invokeCmd<DeliveryJob[]>("list_jobs"),
      invokeCmd<string | null>("system_drive_prefix"),
    ]).then(([saved, currentJobs, system]) => {
      if (saved.status === "fulfilled") setSettings(saved.value)
      if (system.status === "fulfilled") setSystemDrive(system.value)
      if (currentJobs.status === "fulfilled") setJobs(currentJobs.value)
      if (saved.status === "rejected") setNotice({ tone: "error", text: `Settings could not be read: ${errorText(saved.reason)}` })
      setBooting(false)
    })
    let unlisten: (() => void) | undefined
    if ("__TAURI_INTERNALS__" in window) {
      listen<DeliveryJob>("delivery-progress", (event) => {
        setJobs((old) => [event.payload, ...old.filter((job) => job.jobId !== event.payload.jobId)])
      }).then((stop) => { unlisten = stop })
    }
    return () => unlisten?.()
  }, [])

  useEffect(() => {
    document.documentElement.classList.toggle("reduce-motion", settings.reduceMotion)
  }, [settings.reduceMotion])

  useEffect(() => {
    if (!selected) return
    const frame = window.requestAnimationFrame(() => {
      document.querySelector(".inline-detail")?.scrollIntoView({ behavior: settings.reduceMotion ? "auto" : "smooth", block: "start" })
    })
    return () => window.cancelAnimationFrame(frame)
  }, [selected, settings.reduceMotion])

  const refreshCatalog = useCallback(async (refresh = false) => {
    if (demo) return
    setCatalogState("loading")
    setCatalogError("")
    try {
      const catalog = await invokeCmd<GameCatalog>("load_catalog", { refresh })
      setCatalogSections(catalog.sections)
      setCatalogState("success")
    } catch (error) {
      setCatalogSections([])
      setCatalogState("error")
      setCatalogError(errorText(error))
    }
  }, [demo, settings.resolverBaseUrl])

  useEffect(() => {
    void refreshCatalog(false)
  }, [refreshCatalog])

  const visibleGames = demo ? (games.length || searchState === "success" ? games : demoGames) : games

  const enterDemo = () => {
    setDemo(true)
    setGames(demoGames)
    setJobs(demoJobs)
    setCatalogSections([])
    setPage("Discover")
    setSearchState("success")
    setNotice({ tone: "info", text: "Offline preview is active. Network calls and installs are disabled." })
  }

  const leaveDemo = () => {
    setDemo(false)
    setGames([])
    setJobs([])
    setCatalogSections([])
    setSelected(null)
    setSearchState("idle")
    setNotice(null)
  }

  const runSearch = async (event?: FormEvent) => {
    event?.preventDefault()
    const term = query.trim()
    if (!term) {
      setPage("Search")
      setSearchState("error")
      setSearchError("Enter a game title, CUSA ID, or PPSA ID.")
      return
    }
    setPage("Search")
    setSelected(null)
    if (!demo) {
      const hit = searchCache.current.get(term.toLowerCase())
      if (hit && Date.now() - hit.at < SEARCH_CACHE_MS) {
        setGames(hit.games)
        setSearchState("success")
        setSearchError("")
        return
      }
    }
    setSearchState("loading")
    setSearchError("")
    try {
      if (demo) {
        const needle = term.toLowerCase()
        await new Promise((resolve) => window.setTimeout(resolve, 300))
        setGames(demoGames.filter((game) => `${game.name} ${game.titleId}`.toLowerCase().includes(needle)))
      } else {
        const games = await invokeCmd<Game[]>("search_games", { query: term })
        putCapped(searchCache.current, term.toLowerCase(), { games })
        setGames(games)
      }
      setSearchState("success")
    } catch (error) {
      setGames([])
      setSearchState("error")
      setSearchError(errorText(error))
    }
  }

  const selectGame = async (game: Game) => {
    const gen = ++resolveGen.current
    const readyPackages = game.packages || []
    setReceiverPrompt("")
    setSelected(game)
    setPackageError("")
    setPackageNote(null)
    setDetails(null)
    const cached = packageCache.current.get(game.titleId)
    const fresh = !demo && readyPackages.length === 0 && cached !== undefined && Date.now() - cached.at < PACKAGE_CACHE_MS
    if (readyPackages.length > 0) {
      setPackages(readyPackages)
      setPackageState("success")
    } else if (fresh) {
      setPackages(cached.packages)
      setPackageState("success")
      setPackageNote(`Checked ${ageText(cached.at)} · refreshing…`)
    } else {
      setPackages([])
      setPackageState("loading")
    }
    setMetadataState("loading")
    if (demo) {
      await new Promise((resolve) => window.setTimeout(resolve, 220))
      if (gen !== resolveGen.current) return
      setPackages(demoPackages)
      setDetails(demoMetadata(game))
      setPackageState("success")
      setMetadataState("success")
      return
    }
    const [resolved, metadata] = await Promise.allSettled([
      readyPackages.length > 0
        ? Promise.resolve(readyPackages)
        : withTimeout(
            invokeCmd<PackageCandidate[]>("resolve_packages", { gameTitleId: game.titleId, gameName: game.name, gameRegion: game.region }),
            RESOLVE_TIMEOUT_MS,
            RESOLVE_TIMEOUT_SENTINEL,
          ),
      invokeCmd<MetadataResponse>("get_game_details", { name: game.name, titleId: game.titleId }),
    ])
    if (gen !== resolveGen.current) return
    if (resolved.status === "fulfilled") {
      if (readyPackages.length === 0) putCapped(packageCache.current, game.titleId, { packages: resolved.value })
      setPackages(resolved.value)
      setPackageState("success")
      setPackageNote(null)
    } else {
      const timedOut = resolved.reason instanceof Error && resolved.reason.message === RESOLVE_TIMEOUT_SENTINEL
      if (fresh) {
        setPackageNote(`Checked ${ageText(cached.at)} · refresh failed`)
      } else {
        setPackageState("error")
        setPackageError(timedOut ? "Resolving is taking too long. Retry." : errorText(resolved.reason))
      }
    }
    if (gen !== resolveGen.current) return
    if (metadata.status === "fulfilled") {
      setDetails(metadata.value)
      setMetadataState("success")
    } else {
      setMetadataState("error")
    }
  }

  const ensureReceiver = useCallback(async () => {
    if (!settings.ps5Host.trim()) {
      setReceiverPrompt("Add your PS5 address, start the SSPI receiver ELF on the console, and test the connection before installing.")
      return false
    }
    try {
      await invokeCmd<string>("test_ps5", { host: settings.ps5Host, port: settings.ps5Port })
      return true
    } catch (error) {
      setReceiverPrompt(`The PS5 receiver at ${settings.ps5Host}:${settings.ps5Port} did not answer. ${errorText(error)}`)
      return false
    }
  }, [settings.ps5Host, settings.ps5Port])

  const deliver = async (candidate: PackageCandidate) => {
    if (demo) {
      setNotice({ tone: "info", text: `${candidate.label} is a preview. Exit offline mode to install.` })
      return
    }
    if (!selected) return
    if (!await ensureReceiver()) return
    try {
      const archiveParts = candidate.archiveSetId ? packages.filter((item) => item.archiveSetId === candidate.archiveSetId) : []
      if (!candidate.archiveSetId && (candidate.archivePartNumber || (candidate.archiveFileName || "").toLowerCase().includes(".part"))) {
        setNotice({ tone: "error", text: `${candidate.label} is one volume of a split RAR but the source did not group the set. Try another host.` })
        return
      }
      await invokeCmd<string>("start_delivery", { request: { package: candidate, titleId: selected.titleId, titleName: selected.name, icon: selected.icon, archiveParts } })
      setSelected(null)
      setPage("Downloads")
      setNotice({ tone: "success", text: `${candidate.label} was added to Downloads.` })
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    }
  }

  const downloadReceiver = async () => {
    setPayloadBusy(true)
    try {
      const path = await invokeCmd<string>("export_receiver_payload")
      setNotice({ tone: "success", text: `Receiver ELF saved to ${path}` })
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    } finally {
      setPayloadBusy(false)
    }
  }

  if (booting) return <BootScreen />

  return (
    <TooltipProvider delayDuration={200}>
      <div className="app-shell">
        <TitleBar />
        <SideRail page={page} setPage={setPage} activeJobs={jobs.filter((job) => isActiveJob(job.stage)).length} />
        <div className="app-main">
          {!selected && page !== "Downloads" && (
          <TopBar
            page={page}
            query={query}
            setQuery={setQuery}
            runSearch={runSearch}
            searching={searchState === "loading"}
            demo={demo}
            settings={settings}
            showSearch={page === "Discover" || page === "Search"}
          />
          )}
          {notice && <Notice tone={notice.tone} onClose={() => setNotice(null)}>{notice.text}</Notice>}
          <div className="page-wrap">
            {page === "Discover" && !selected && (
              <DiscoverPage
                sections={demo ? [{ id: "preview", title: "Featured games", games: demoGames }] : catalogSections}
                state={demo ? "success" : catalogState}
                error={catalogError}
                demo={demo}
                onBrowse={() => setPage("Search")}
                onSelect={selectGame}
                onRefresh={() => refreshCatalog(true)}
                onSettings={() => setPage("Settings")}
                onDemo={enterDemo}
              />
            )}
            {page === "Search" && !selected && (
              <SearchPage
                games={visibleGames}
                state={searchState}
                error={searchError}
                query={query}
                onRetry={() => runSearch()}
                onSelect={selectGame}
              />
            )}
            {page === "Downloads" && (
              <DownloadsPage jobs={demo ? demoJobs : jobs} demo={demo} setNotice={setNotice} ensureReceiver={ensureReceiver} downloadDir={settings.downloadDir} systemDrive={systemDrive} onOpenSettings={() => setPage("Settings")} />
            )}
            {page === "Settings" && (
              demo
                ? <DemoSettings leave={leaveDemo} />
                : <SettingsPage settings={settings} setSettings={setSettings} setNotice={setNotice} downloadReceiver={downloadReceiver} payloadBusy={payloadBusy} />
            )}
            {(page === "Discover" || page === "Search") && selected && (
              <GameDetail
                game={selected}
                packages={packages}
                packageState={packageState}
                packageError={packageError}
                packageNote={packageNote}
                metadata={details}
                metadataState={metadataState}
                demo={demo}
                receiverMessage={receiverPrompt}
                receiverHost={settings.ps5Host}
                receiverPort={settings.ps5Port}
                receiverBusy={payloadBusy}
                onClose={() => { setReceiverPrompt(""); setSelected(null) }}
                onRetry={() => selectGame(selected)}
                onSettings={() => { setReceiverPrompt(""); setSelected(null); setPage("Settings") }}
                onInstall={deliver}
                onReceiverBack={() => setReceiverPrompt("")}
                onReceiverDownload={downloadReceiver}
              />
            )}
          </div>
        </div>
        <ReceiverPrompt
          open={Boolean(receiverPrompt) && !selected}
          message={receiverPrompt}
          host={settings.ps5Host}
          port={settings.ps5Port}
          busy={payloadBusy}
          onClose={() => setReceiverPrompt("")}
          onSettings={() => { setReceiverPrompt(""); setSelected(null); setPage("Settings") }}
          onDownload={downloadReceiver}
        />
      </div>
    </TooltipProvider>
  )
}

function TitleBar() {
  const run = (action: "minimize" | "maximize" | "close") => {
    if (!("__TAURI_INTERNALS__" in window)) return
    const current = getCurrentWindow()
    if (action === "minimize") void current.minimize()
    if (action === "maximize") void current.toggleMaximize()
    if (action === "close") void current.close()
  }
  return (
    <div className="titlebar" data-tauri-drag-region>
      <span data-tauri-drag-region>SSPI</span>
      <div className="titlebar-controls">
        <button type="button" onClick={() => run("minimize")} aria-label="Minimize"><Minus /></button>
        <button type="button" onClick={() => run("maximize")} aria-label="Maximize"><Square /></button>
        <button type="button" className="close" onClick={() => run("close")} aria-label="Close"><X /></button>
      </div>
    </div>
  )
}

function BootScreen() {
  return (
    <div className="boot-screen grid min-h-screen place-items-center bg-background text-foreground">
      <img src={texLines} alt="" aria-hidden="true" className="boot-texture" draggable={false} />
      <div className="flex flex-col items-center gap-4 boot-foreground">
        <div className="grid size-14 place-items-center overflow-hidden rounded-2xl bg-black shadow-[0_0_50px_rgba(255,255,255,.12)]"><img src={sspiLogo} alt="SSPI logo" className="size-full object-contain" draggable={false} /></div>
        <LoaderCircle className="size-5 animate-spin text-muted-foreground" />
      </div>
    </div>
  )
}

function SideRail({ page, setPage, activeJobs }: { page: Page; setPage: (page: Page) => void; activeJobs: number }) {
  const items: Array<{ page: Page; icon: typeof Home }> = [
    { page: "Discover", icon: Home },
    { page: "Search", icon: Search },
    { page: "Downloads", icon: Download },
    { page: "Settings", icon: SettingsIcon },
  ]
  return (
    <aside className="side-rail">
      <button className="brand-mark brand-logo" onClick={() => setPage("Discover")} aria-label="SSPI home"><img src={sspiLogo} alt="SSPI logo" draggable={false} /></button>
      <nav className="rail-nav">
        {items.map((item) => {
          const Icon = item.icon
          return (
            <Tooltip key={item.page}>
              <TooltipTrigger asChild>
                <button className={cn("rail-button", page === item.page && "active")} onClick={() => setPage(item.page)} aria-label={item.page}>
                  <Icon />
                  {item.page === "Downloads" && activeJobs > 0 && <span className="job-dot">{activeJobs}</span>}
                </button>
              </TooltipTrigger>
              <TooltipContent side="right">{item.page}</TooltipContent>
            </Tooltip>
          )
        })}
      </nav>
      <div className="rail-wordmark">SSPI</div>
    </aside>
  )
}

function TopBar({ page, query, setQuery, runSearch, searching, demo, settings, showSearch }: {
  page: Page
  query: string
  setQuery: (value: string) => void
  runSearch: (event?: FormEvent) => void
  searching: boolean
  demo: boolean
  settings: Settings
  showSearch: boolean
}) {
  return (
    <header className={cn("top-bar", !showSearch && "top-bar-plain")}>
      <div className="page-heading">
        <p>{pageCopy[page].title}</p>
        <span>{pageCopy[page].subtitle}</span>
      </div>
      {showSearch && (
      <form className="search-box" onSubmit={runSearch}>
        <Search className="size-4 text-muted-foreground" />
        <Input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search title, PPSA, or CUSA..." aria-label="Search title or title ID" />
        <Button type="submit" size="sm" disabled={searching}>
          {searching ? <LoaderCircle className="animate-spin" /> : <ArrowRight />}
          Search
        </Button>
      </form>
      )}
      <span className="connection-plain">{demo ? "Offline preview" : settings.ps5Host ? `${settings.ps5Host}:${settings.ps5Port}` : "Receiver not set"}</span>
    </header>
  )
}

function Notice({ tone, children, onClose }: { tone: "info" | "error" | "success"; children: string; onClose: () => void }) {
  const Icon = tone === "error" ? CircleAlert : tone === "success" ? CircleCheck : Info
  return (
    <div className={cn("notice-bar", tone)} role="status">
      <Icon />
      <span>{children}</span>
      <button onClick={onClose} aria-label="Dismiss"><X /></button>
    </div>
  )
}

function DiscoverPage({ sections, state, error, demo, onBrowse, onSelect, onRefresh, onSettings, onDemo }: {
  sections: CatalogSection[]
  state: LoadState
  error: string
  demo: boolean
  onBrowse: () => void
  onSelect: (game: Game) => void
  onRefresh: () => void
  onSettings: () => void
  onDemo: () => void
}) {
  const [platform, setPlatform] = useState<"both" | "ps4" | "ps5">("both")
  const featured = sections[0]?.games[0] || demoGames[0]
  const heroSource = useCoverSource(featured.icon, featured.name)
  const hasCatalog = sections.some((section) => section.games.length > 0)
  const featuredLinks = featured.packages?.length || 0
  const library = sections.flatMap((section) => section.games).filter((game) => {
    if (platform === "ps5") return game.titleId.startsWith("PPSA")
    if (platform === "ps4") return game.titleId.startsWith("CUSA")
    return true
  })
  return (
    <div className="discover-page">
      <section className="hero-banner">
        <img src={heroSource} alt="" className="hero-art" />
        <img src={texDots} alt="" aria-hidden="true" className="hero-texture" draggable={false} />
        <div className="hero-vignette" />
        <div className="hero-copy">
          {demo && <Badge variant="outline" className="border-white/20 bg-black/25 text-white/75"><Sparkles /> Interface preview</Badge>}
          <h1>{demo || hasCatalog ? featured.name : "Loading Package Source titles."}</h1>
          <p>{demo ? "Open the full title view, inspect packages, and preview the install flow without a console connection." : featuredLinks ? `${featured.titleId} already has ${featuredLinks} resolver link${featuredLinks === 1 ? "" : "s"} ready. A PS5 connection is only needed when you install.` : hasCatalog ? `${featured.titleId} · ${featured.region} is listed by your Package Source.` : "Install a Package Source in Settings to populate the home catalog."}</p>
          <div className="flex flex-wrap gap-3">
            <Button size="lg" onClick={hasCatalog || demo ? () => onSelect(featured) : onRefresh} disabled={state === "loading"}>{state === "loading" ? <LoaderCircle className="animate-spin" /> : hasCatalog || demo ? <Play /> : <RefreshCw />} {hasCatalog || demo ? "View title" : "Load catalog"}</Button>
            <Button size="lg" variant="glass" onClick={onBrowse}><Search /> Search titles</Button>
          </div>
        </div>
      </section>
      {state === "loading" && <section className="shelf-section catalog-loading"><SectionHeading title="Loading titles and links" subtitle="Requesting the resolver catalog, supported packages, and Orbis artwork." /><PosterSkeletons /><div className="mt-10"><PosterSkeletons /></div></section>}
      {state === "error" && <section className="shelf-section"><StateCard icon={CircleAlert} title="Catalog could not be loaded" copy={error} action="Retry catalog" onAction={onRefresh} /><div className="mt-4 flex justify-center gap-2"><Button variant="ghost" onClick={onSettings}><SettingsIcon /> Resolver settings</Button><Button variant="ghost" onClick={onDemo}><Play /> Offline preview</Button></div></section>}
      {state === "idle" && !demo && <section className="shelf-section"><FeatureStrip /></section>}
      {state === "success" && (
        <section className="shelf-section catalog-section">
          <div className="catalog-section-head">
            <div className="platform-filter">
              {(["both", "ps4", "ps5"] as const).map((value) => (
                <button key={value} type="button" className={cn("platform-chip", platform === value && "active")} onClick={() => setPlatform(value)}>
                  {value === "both" ? "Both" : value === "ps4" ? "PS4" : "PS5"}
                </button>
              ))}
            </div>
            <Button size="sm" variant="ghost" onClick={onRefresh} disabled={demo}><RefreshCw /> Refresh</Button>
          </div>
          <GameShelf games={library} onSelect={onSelect} />
        </section>
      )}
    </div>
  )
}

function FeatureStrip() {
  const items = [
    { icon: Search, title: "Live title search", copy: "Orbis title records and cover art." },
    { icon: ShieldCheck, title: "Verified delivery", copy: "Resolver, provider, and receiver states stay visible." },
    { icon: MonitorUp, title: "Direct to PS5", copy: "The proven SSPI receiver flow remains intact." },
  ]
  return <div className="feature-grid">{items.map(({ icon: Icon, title, copy }) => <Card key={title} className="feature-card"><CardHeader><div className="feature-icon"><Icon /></div><CardTitle>{title}</CardTitle><CardDescription>{copy}</CardDescription></CardHeader></Card>)}</div>
}

function SearchPage({ games, state, error, query, onRetry, onSelect }: { games: Game[]; state: LoadState; error: string; query: string; onRetry: () => void; onSelect: (game: Game) => void }) {
  return (
    <section className="content-section">
      <SectionHeading title={query ? `Results for "${query}"` : "Find a title"} subtitle={state === "success" ? `${games.length} matching title${games.length === 1 ? "" : "s"}.` : "Search by game name or exact CUSA ID."} />
      {state === "loading" && <PirateSeasLoading label={query ? `Searching resolvers for "${query}"` : "Searching resolvers"} />}
      {state === "error" && <StateCard icon={CircleAlert} title="Search did not complete" copy={error} action="Try again" onAction={onRetry} />}
      {state === "idle" && <StateCard icon={Search} title="Search your catalog" copy="Use the search box above. Results and cover art will appear here." />}
      {state === "success" && games.length === 0 && <StateCard icon={Library} title="No matching titles" copy="Try a broader title name or an exact CUSA ID." />}
      {state === "success" && games.length > 0 && <GameShelf games={games} onSelect={onSelect} />}
    </section>
  )
}

function SectionHeading({ title, subtitle }: { title: string; subtitle: string }) {
  return <div className="section-heading"><div><h2>{title}</h2><p>{subtitle}</p></div></div>
}

function GameShelf({ games, onSelect }: { games: Game[]; onSelect: (game: Game) => void }) {
  return (
    <div className="game-shelf">
      {games.map((game) => {
        const kinds = [...new Set((game.packages || []).map((item) => item.kind).filter(Boolean))]
        const firmware = game.packages?.find((item) => item.firmware)?.firmware
        return (
        <button
          className="game-card"
          key={`${game.titleId}-${game.region}-${game.sourceId || ""}`}
          onClick={() => onSelect(game)}
          onMouseMove={(event) => {
            const rect = event.currentTarget.getBoundingClientRect()
            event.currentTarget.style.setProperty("--mx", `${(((event.clientX - rect.left) / rect.width) * 100).toFixed(1)}%`)
            event.currentTarget.style.setProperty("--my", `${(((event.clientY - rect.top) / rect.height) * 100).toFixed(1)}%`)
          }}
        >
          <div className="game-case">
          <div className="poster-frame has-case">
            <CaseCover source={game.icon} title={game.name} titleId={game.titleId} />
            <span className="case-glare" aria-hidden="true" />
            <div className="poster-shade" />
            <div className="poster-badges">
              <span>{game.region || "—"}</span>
              {kinds.slice(0, 2).map((kind) => <span key={kind}>{kind}</span>)}
            </div>
            <span className="poster-action"><Play /> View details</span>
          </div>
          </div>
          <span className="game-name">{game.name}</span>
          <span className="game-meta">{game.titleId} · {game.region}{firmware ? ` · FW ${firmware}` : ""}{game.packages?.length ? ` · ${game.packages.length} pkgs` : ""}</span>
          {game.sourceName && <span className="game-link-preview">{game.sourceName}{game.sourceVersion ? ` ${game.sourceVersion}` : ""}</span>}
          {!game.sourceName && game.packages && game.packages.length > 0 && <span className="game-link-preview">{game.packages.slice(0, 2).map((item) => item.label).join("  ·  ")}{game.packages.length > 2 ? `  +${game.packages.length - 2}` : ""}</span>}
        </button>
        )
      })}
    </div>
  )
}

function PosterSkeletons() {
  return <div className="game-shelf">{Array.from({ length: 7 }, (_, index) => <div key={index}><Skeleton className="aspect-[2/3] w-full rounded-xl" /><Skeleton className="mt-3 h-4 w-4/5" /><Skeleton className="mt-2 h-3 w-2/5" /></div>)}</div>
}

const PIRATE_BOAT = [
  "      |\\",
  "      | \\",
  "  ____|__\\____",
  "  \\___________/",
].join("\n")
const WAVE_ROW = "~  ~~~  ~~~~  ~~~  ".repeat(24)

function PirateSeasLoading({ label }: { label?: string }) {
  return (
    <div className="pirate-seas" role="status" aria-live="polite">
      <div className="pirate-track" aria-hidden="true">
        <img src={texLines} alt="" className="pirate-texture" draggable={false} />
        <pre className="pirate-boat">{PIRATE_BOAT}</pre>
        <pre className="pirate-waves w1">{WAVE_ROW + WAVE_ROW}</pre>
        <pre className="pirate-waves w2">{WAVE_ROW + WAVE_ROW}</pre>
      </div>
      <p className="pirate-line">Being a pirate at all ways easy, the seas take time to travel</p>
      {label && <p className="pirate-sub">{label}</p>}
    </div>
  )
}

function StateCard({ icon: Icon, title, copy, action, onAction }: { icon: typeof Search; title: string; copy: string; action?: string; onAction?: () => void }) {
  return (
    <Card className="state-card">
      <CardContent className="flex flex-col items-center py-14 text-center">
        <div className="state-icon"><Icon /></div>
        <h3>{title}</h3>
        <p>{copy}</p>
        {action && <Button variant="outline" onClick={onAction}><RefreshCw /> {action}</Button>}
      </CardContent>
    </Card>
  )
}

function GameDetail({ game, packages, packageState, packageError, packageNote, metadata, metadataState, demo, receiverMessage, receiverHost, receiverPort, receiverBusy, onClose, onRetry, onSettings, onInstall, onReceiverBack, onReceiverDownload }: {
  game: Game | null
  packages: PackageCandidate[]
  packageState: LoadState
  packageError: string
  packageNote: string | null
  metadata: MetadataResponse | null
  metadataState: LoadState
  demo: boolean
  receiverMessage: string
  receiverHost: string
  receiverPort: number
  receiverBusy: boolean
  onClose: () => void
  onRetry: () => void
  onSettings: () => void
  onInstall: (candidate: PackageCandidate) => void
  onReceiverBack: () => void
  onReceiverDownload: () => Promise<void>
}) {
  const raw = metadata?.game
  const backdrop = useCoverSource(raw?.background_image || game?.icon, game?.name || "Game Search")
  const genres = raw?.genres?.map((genre) => genre.name).filter(Boolean) || []
  if (!game) return null
  const baseCandidates = packages.filter((candidate) => candidate.kind === "base")
  const basePackage = (baseCandidates.length > 0 ? baseCandidates : packages)
    .slice()
    .sort((left, right) => (left.archivePartNumber || 1) - (right.archivePartNumber || 1))[0]
  const firmware = basePackage?.firmware || raw?.firmware || "Not provided"
  const packageVersion = basePackage?.version || raw?.version || "Not provided"
  const heroParts = basePackage?.archiveSetId ? packages.filter((item) => item.archiveSetId === basePackage.archiveSetId) : (basePackage ? [basePackage] : [])
  const heroBytes = heroParts.reduce((sum, item) => sum + (item.expectedSize || 0), 0)
  const packageSize = heroBytes ? formatPackageSize(heroBytes) : "Not provided"
  const hasBackport = packages.some((candidate) => candidate.kind === "backport")
  const scrollToUpdates = () => document.querySelector(".package-table")?.scrollIntoView({ behavior: "smooth", block: "start" })
  return (
    <section className="inline-detail" aria-label={`${game.name} details`}>
      <button type="button" className="detail-back" onClick={onClose}><ArrowLeft /> Back</button>
        {receiverMessage ? (
          <section className="detail-connection">
            <div className="settings-card-icon"><MonitorUp /></div>
            <h2>PS5 connection required</h2>
            <p>Browsing and package links work without a console. Installation needs the SSPI receiver running on your PS5.</p>
            <div className="receiver-status"><span>Configured target</span><strong>{receiverHost ? `${receiverHost}:${receiverPort}` : "Not configured"}</strong></div>
            <div className="inline-error"><CircleAlert /><span>{receiverMessage}</span></div>
            <div className="detail-connection-actions">
              <Button variant="ghost" onClick={onReceiverBack}>Back to packages</Button>
              <Button variant="outline" onClick={onReceiverDownload} disabled={receiverBusy}>{receiverBusy ? <LoaderCircle className="animate-spin" /> : <Download />} Download receiver ELF</Button>
              <Button onClick={onSettings}><SettingsIcon /> Open connection settings</Button>
            </div>
          </section>
        ) : <>
          <section className="detail-hero">
            <img src={backdrop} alt="" className="detail-backdrop" />
            <div className="detail-gradient" />
            <div className="detail-cover has-case"><CaseCover source={game.icon} title={game.name} titleId={game.titleId} /></div>
            <div className="detail-title">
              <h2>{game.name}</h2>
              <div className="detail-badges"><Badge>{game.titleId}</Badge><Badge variant="outline">{game.region}</Badge>{demo && <Badge variant="warning">Preview</Badge>}</div>
              <p className="detail-identity">{game.titleId} <span>·</span> {game.region}{genres.length > 0 ? ` · ${genres.join(" · ")}` : ""}</p>
              <p className="detail-package-summary"><strong>Base Version</strong> {packageVersion}<i /><strong>Required Firmware</strong> {firmware}<i /><strong>Size</strong> {packageSize}{game.sourceName ? <><i /><strong>Source</strong> {game.sourceName}</> : null}</p>
              <div className="detail-hero-actions">
                <Button size="lg" disabled={!basePackage} onClick={() => basePackage && onInstall(basePackage)}><Download /> {demo ? "Preview Base Game" : "Install Base Game"}</Button>
                <Button size="lg" variant="outline" onClick={scrollToUpdates}>View Updates</Button>
              </div>
            </div>
          </section>
          <div className="detail-body">
            <div className="package-panel">
              <div className="package-heading"><div><h3>Available packages</h3><p>Resolved for {game.titleId}{packageNote ? ` · ${packageNote}` : ""}</p></div>{packageState === "loading" && <LoaderCircle className="animate-spin text-primary" />}</div>
              {packageState === "loading" && <PirateSeasLoading label={`Resolving packages for ${game.titleId}`} />}
              {packageState === "error" && (
                <div className="resolve-error">
                  <CircleAlert />
                  <div><strong>Package resolution failed</strong><p>{packageError}</p></div>
                  <div className="flex gap-2"><Button size="sm" variant="outline" onClick={onRetry}><RefreshCw /> Retry</Button><Button size="sm" variant="ghost" onClick={onSettings}><SettingsIcon /> Settings</Button></div>
                </div>
              )}
              {packageState === "success" && packages.length === 0 && <div className="resolve-empty"><PackageOpen /><strong>No packages returned</strong><p>The resolver answered successfully but has no links for this title.</p></div>}
              {packageState === "success" && packages.length > 0 && (
                <PackageGroups packages={packages} demo={demo} onInstall={onInstall} />
              )}
            </div>
            <div className="detail-lower-grid">
              <section className="detail-about">
                <h3>About this game</h3>
                <p className="detail-description">{raw?.description_raw || raw?.description || (metadataState === "error" ? "Metadata is unavailable, but package resolution can still continue." : "Metadata is optional. Package resolution is running independently.")}</p>
                {genres.length > 0 && <div className="detail-genres">{genres.map((genre) => <Badge variant="secondary" key={genre}>{genre}</Badge>)}</div>}
                <p className="attribution">{metadata?.attribution || (game.titleId.startsWith("PPSA") ? "Title and cover from Prospero Patches" : "Title and cover from Orbis Patches")}</p>
              </section>
              <section className="technical-information">
                <h3>Technical information</h3>
                <div className="technical-columns">
                  <div><MetadataLine label="Title ID" value={game.titleId} /><MetadataLine label="Content ID" value={basePackage?.expectedContentId || "Not provided"} /><MetadataLine label="Base version" value={packageVersion} /><MetadataLine label="SDK / Firmware" value={firmware} /></div>
                  <div><MetadataLine label="Backport" value={hasBackport ? "Available" : "Not listed"} /><MetadataLine label="Install status" value="Not checked" accent /><MetadataLine label="Storage required" value={raw?.size || packageSize} /><MetadataLine label="Metacritic" value={raw?.metacritic ? String(raw.metacritic) : "Not provided"} /></div>
                </div>
              </section>
            </div>
          </div>
        </>}
    </section>
  )
}

function MetadataLine({ label, value, accent = false }: { label: string; value: string; accent?: boolean }) {
  return <div className="metadata-line"><span>{label}</span><strong className={accent ? "accent" : ""}>{value}</strong></div>
}

function compareVersions(left = "", right = "") {
  const a = left.split(".").map((part) => Number(part) || 0)
  const b = right.split(".").map((part) => Number(part) || 0)
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    if ((a[index] || 0) !== (b[index] || 0)) return (a[index] || 0) - (b[index] || 0)
  }
  return 0
}

function formatPackageSize(bytes: number) {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(2)} GB`
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`
  return `${bytes.toLocaleString()} B`
}

function structurePackages(packages: PackageCandidate[]) {
  const names: Record<string, string> = { base: "Base Game", update: "Game Update", dlc: "DLC Pack", backport: "Backport", exfat: "exFAT" }
  const groups = new Map<string, { key: string; kind: string; title: string; version: string; firmware: string; hosts: Map<string, PackageCandidate[]> }>()
  for (const candidate of packages) {
    const key = `${candidate.kind}|${candidate.version || ""}|${candidate.groupId || candidate.label || candidate.url}`
    if (!groups.has(key)) {
      groups.set(key, {
        key,
        kind: candidate.kind,
        title: names[candidate.kind] || candidate.label || "Package",
        version: candidate.version || "—",
        firmware: candidate.firmware || "—",
        hosts: new Map(),
      })
    }
    const host = candidate.hoster || "Host"
    const group = groups.get(key)!
    group.hosts.set(host, [...(group.hosts.get(host) || []), candidate])
  }
  const order = ["base", "update", "dlc", "backport", "exfat"]
  return [...groups.values()].sort((left, right) => order.indexOf(left.kind) - order.indexOf(right.kind) || left.title.localeCompare(right.title))
}

function archiveLabel(parts: PackageCandidate[]) {
  const format = (parts.find((item) => item.archiveFormatHint && item.archiveFormatHint !== "unknown")?.archiveFormatHint || (parts.some((item) => item.url.toLowerCase().includes(".rar") || (item.archiveFileName || "").toLowerCase().includes(".rar")) ? "rar" : "")).toUpperCase()
  const count = parts[0]?.archivePartCount || parts.length
  const incomplete = parts.some((item) => item.diagnostics?.some((note) => note.includes("incomplete") || note.includes("missing Part.")))
  if (parts.length > 1 || parts[0]?.archivePartNumber) return `${format || "RAR"} · ${parts.length}${count ? `/${count}` : ""} parts${incomplete ? " · incomplete" : ""}`
  if (format) return format
  const access = (parts[0]?.accessType || "").toLowerCase()
  if (access === "direct") return "Direct"
  return "RAR"
}

function PackageGroups({ packages, demo, onInstall }: { packages: PackageCandidate[]; demo: boolean; onInstall: (candidate: PackageCandidate) => void }) {
  const groups = structurePackages(packages)
  const [hostByKey, setHostByKey] = useState<Record<string, string>>({})
  return (
    <div className="package-table">
      <div className="package-table-head"><span>Package</span><span>Version</span><span>Host</span><span>Archive</span><span>FW</span><span>Action</span></div>
      {groups.map((group) => {
        const hosts = [...group.hosts.keys()]
        const selectedHost = hostByKey[group.key] || hosts[0]
        const parts = [...(group.hosts.get(selectedHost) || [])].sort((left, right) => (left.archivePartNumber || 0) - (right.archivePartNumber || 0))
        const candidate = parts[0]
        const incomplete = parts.some((item) => item.diagnostics?.some((note) => note.includes("incomplete") || note.includes("missing Part.")))
        const sevenZ = archiveLabel(parts).startsWith("7Z")
        return (
          <div className="package-table-row" key={group.key}>
            <div className="package-name"><PackageOpen /><span><strong>{group.title}</strong><small>{candidate?.label && candidate.label !== group.title ? candidate.label : group.kind}</small></span></div>
            <span>{group.version}</span>
            <span>
              {hosts.length > 1 ? (
                <select className="host-select" value={selectedHost} onChange={(event) => setHostByKey((current) => ({ ...current, [group.key]: event.target.value }))}>
                  {hosts.map((host) => <option key={host} value={host}>{host}</option>)}
                </select>
              ) : selectedHost}
            </span>
            <span>{archiveLabel(parts)}</span>
            <span>{group.firmware}</span>
            <Button onClick={() => candidate && onInstall(candidate)} size="sm" disabled={!candidate || incomplete || sevenZ} title={sevenZ ? "7z splits are not extracted on PC in this build — pick the RAR mirror" : undefined}>{demo ? <Play /> : <HardDriveDownload />}{demo ? "Preview" : sevenZ ? "7z" : "Install"}</Button>
          </div>
        )
      })}
    </div>
  )
}

function DownloadsPage({ jobs, demo, setNotice, ensureReceiver, downloadDir, systemDrive, onOpenSettings }: { jobs: DeliveryJob[]; demo: boolean; setNotice: (notice: { tone: "info" | "error" | "success"; text: string }) => void; ensureReceiver: () => Promise<boolean>; downloadDir: string; systemDrive: string | null; onOpenSettings: () => void }) {
  const [local, setLocal] = useState<LocalPackage[]>([])
  const [picked, setPicked] = useState<string[]>([])
  const [manual, setManual] = useState<Array<ManualCandidate & { kind: string }>>([])
  const [manualBusy, setManualBusy] = useState(false)
  const choose = async (directory: boolean) => {
    try {
      const value = await open({ multiple: false, directory, title: directory ? "Choose package folder" : "Choose PKG, ZIP, or RAR" })
      if (!value || Array.isArray(value)) return
      setLocal(await invokeCmd<LocalPackage[]>("scan_local_packages", { path: value }))
      setPicked([])
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    }
  }
  const install = async () => {
    if (!await ensureReceiver()) return
    for (const path of picked) {
      try { await invokeCmd<string>("start_local_install", { path }) }
      catch (error) { setNotice({ tone: "error", text: errorText(error) }); return }
    }
    setNotice({ tone: "success", text: `${picked.length} local package${picked.length === 1 ? "" : "s"} queued.` })
    setPicked([])
  }
  const addManualFolders = async () => {
    try {
      const value = await open({ multiple: true, directory: true, title: "Choose game folder(s)" })
      const dirs = (Array.isArray(value) ? value : value ? [value] : []).filter((v): v is string => typeof v === "string")
      if (dirs.length === 0) return
      setManualBusy(true)
      const found: Array<ManualCandidate & { kind: string }> = []
      for (const dir of dirs) {
        try {
          const scanned = await invokeCmd<ManualCandidate[]>("scan_manual_folder", { path: dir })
          for (const item of scanned) found.push({ ...item, kind: item.detectedKind })
        } catch (error) {
          setNotice({ tone: "error", text: errorText(error) })
        }
      }
      setManual((old) => {
        const seen = new Set(old.map((item) => item.path.toLowerCase()))
        return [...old, ...found.filter((item) => !seen.has(item.path.toLowerCase()))]
      })
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    } finally {
      setManualBusy(false)
    }
  }
  const installManual = async () => {
    if (!await ensureReceiver()) return
    for (const item of manual) {
      if (!item.titleId?.trim() && item.content === "dump") {
        setNotice({ tone: "error", text: `${item.name} needs a CUSA/PPSA title ID.` })
        return
      }
    }
    const items: ManualItem[] = manual.map((item) => ({
      path: item.path,
      kind: item.kind,
      titleId: item.titleId?.trim() ? item.titleId.trim().toUpperCase() : undefined,
    }))
    try {
      await invokeCmd<string>("start_manual_install", { items })
      setNotice({ tone: "success", text: `${items.length} manual item${items.length === 1 ? "" : "s"} queued.` })
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    }
  }
  return (
    <section className="content-section">
      <div className="downloads-heading">
        <SectionHeading title="Downloads" subtitle="" />
        {!demo && <div className="flex gap-2"><Button variant="outline" onClick={() => choose(false)}><PackageOpen /> Import file</Button><Button variant="outline" onClick={() => choose(true)}><Library /> Scan folder</Button><Button variant="outline" onClick={addManualFolders} disabled={manualBusy}><FolderOpen /> Manual folder</Button></div>}
      </div>
      {local.length > 0 && <Card className="mb-6"><CardHeader><CardTitle>Local packages</CardTitle><CardDescription>Select packages found in the imported location.</CardDescription></CardHeader><CardContent className="space-y-2">{local.map((item) => <label className="local-row" key={item.path}><input type="checkbox" checked={picked.includes(item.path)} onChange={(event) => setPicked(event.target.checked ? [...picked, item.path] : picked.filter((path) => path !== item.path))} /><div><strong>{item.name}</strong><span>{item.titleId || item.kind} / {formatPackageSize(item.size)}</span></div></label>)}{picked.length > 0 && <Button onClick={install}><HardDriveDownload /> Install selected ({picked.length})</Button>}</CardContent></Card>}
      {manual.length > 0 && (
        <Card className="mb-6"><CardHeader><CardTitle>Manual install</CardTitle><CardDescription>Kind is auto-detected — fix it when wrong. Dumps install to /data/homebrew/&lt;ID&gt; (backports under backports/).</CardDescription></CardHeader>
        <CardContent className="space-y-2">
          {manual.map((item) => (
            <div className="local-row manual-row" key={item.path}>
              <div className="min-w-0 flex-1"><strong>{item.name}</strong><span>{item.content} · {item.titleId || "no title ID"} / {formatPackageSize(item.size)}</span></div>
              <select
                className="host-select manual-kind"
                aria-label={`Kind for ${item.name}`}
                value={item.kind}
                onChange={(event) => setManual((old) => old.map((row) => row.path === item.path ? { ...row, kind: event.target.value } : row))}
              >
                {(["base", "update", "dlc", "backport"] as const).map((kind) => <option key={kind} value={kind}>{kind}</option>)}
              </select>
              <Input
                className="manual-title"
                aria-label={`Title ID for ${item.name}`}
                placeholder={item.needsTitle ? "CUSA/PPSA…" : "Title ID"}
                value={item.titleId || ""}
                onChange={(event) => setManual((old) => old.map((row) => row.path === item.path ? { ...row, titleId: event.target.value } : row))}
              />
              <Button type="button" size="sm" variant="ghost" onClick={() => setManual((old) => old.filter((row) => row.path !== item.path))}>Remove</Button>
            </div>
          ))}
          <div className="flex gap-2">
            <Button onClick={installManual}><HardDriveDownload /> Install manual ({manual.length})</Button>
            <Button variant="ghost" onClick={() => setManual([])}>Clear</Button>
          </div>
        </CardContent></Card>
      )}
      <DownloadGroups jobs={jobs} demo={demo} downloadDir={downloadDir} systemDrive={systemDrive} onOpenSettings={onOpenSettings} onPause={async (jobId, paused) => { try { await invokeCmd("pause_job", { jobId, paused }) } catch (error) { setNotice({ tone: "error", text: String(error) }) } }} onCancel={async (jobId) => {
        try {
          const accepted = await invokeCmd<boolean>("cancel_job", { jobId })
          if (!accepted) setNotice({ tone: "info", text: "This transfer has already stopped." })
        } catch (error) { setNotice({ tone: "error", text: errorText(error) }) }
      }} />
    </section>
  )
}


function DemoSettings({ leave }: { leave: () => void }) {
  return (
    <section className="content-section settings-section">
      <SectionHeading title="Offline preview" subtitle="The interface is running on safe, simulated data." />
      <Card className="settings-card"><CardHeader><div className="settings-card-icon"><WifiOff /></div><CardTitle>No console or provider is in use</CardTitle><CardDescription>Exit preview to return to the live catalog. Resolver and PS5 details can be added here in Settings at any time.</CardDescription></CardHeader><CardContent><Button onClick={leave}><Wifi /> Return to live catalog</Button></CardContent></Card>
    </section>
  )
}

function SettingsPage({ settings, setSettings, setNotice, downloadReceiver, payloadBusy }: { settings: Settings; setSettings: (settings: Settings) => void; setNotice: (notice: { tone: "info" | "error" | "success"; text: string }) => void; downloadReceiver: () => Promise<void>; payloadBusy: boolean }) {
  const [draft, setDraft] = useState(settings)
  const [rdToken, setRdToken] = useState("")
  const [sourceUrl, setSourceUrl] = useState("")
  const [sources, setSources] = useState<PackageSource[]>([])
  const [busy, setBusy] = useState("")
  const [tab, setTab] = useState<"receiver" | "sources" | "debrid" | "app">("receiver")
  const [flash, setFlash] = useState("")
  useEffect(() => setDraft(settings), [settings])
  useEffect(() => {
    invokeCmd<PackageSource[]>("list_package_sources")
      .then(setSources)
      .catch((error) => setNotice({ tone: "error", text: `Package Sources could not be read: ${errorText(error)}` }))
  }, [setNotice])
  const test = async (key: string, command: string, args: Record<string, unknown>) => {
    setBusy(key)
    try {
      await invokeCmd<string>(command, args)
      setFlash(key)
      window.setTimeout(() => setFlash(""), 1400)
    } catch (error) {
      setNotice({ tone: "error", text: errorText(error) })
    } finally {
      setBusy("")
    }
  }
  const save = async (event: FormEvent) => {
    event.preventDefault()
    setBusy("save")
    try {
      const saved = await invokeCmd<Settings>("save_settings", { input: { ...draft, onboardingComplete: true, realDebridToken: rdToken || undefined } })
      setSettings(saved)
      setDraft(saved)
      setRdToken("")
      setNotice({ tone: "success", text: "Settings saved. Secrets remain in Windows Credential Manager." })
    } catch (error) { setNotice({ tone: "error", text: errorText(error) }) }
    finally { setBusy("") }
  }
  const installSource = async () => {
    if (!sourceUrl.trim()) return
    setBusy("source-install")
    try {
      setSources(await invokeCmd<PackageSource[]>("install_package_source", { url: sourceUrl.trim() }))
      setNotice({ tone: "success", text: "Package Source installed and enabled. Search now runs through all enabled sources." })
    } catch (error) { setNotice({ tone: "error", text: errorText(error) }) }
    finally { setBusy("") }
  }
  const browseSource = async () => {
    try {
      const value = await open({
        multiple: false,
        directory: false,
        title: "Choose a .gssource file",
        filters: [{ name: "Package Source", extensions: ["gssource", "zip"] }],
      })
      if (!value || Array.isArray(value)) return
      setBusy("source-browse")
      setSources(await invokeCmd<PackageSource[]>("install_package_source_from_path", { path: value }))
      setNotice({ tone: "success", text: "Package Source installed from file and enabled." })
    } catch (error) { setNotice({ tone: "error", text: errorText(error) }) }
    finally { setBusy("") }
  }
  const toggleSource = async (id: string, enabled: boolean) => {
    setBusy(`source-${id}`)
    try { setSources(await invokeCmd<PackageSource[]>("set_package_source_enabled", { id, enabled })) }
    catch (error) { setNotice({ tone: "error", text: errorText(error) }) }
    finally { setBusy("") }
  }
  const removeSource = async (id: string) => {
    setBusy(`source-${id}`)
    try {
      setSources(await invokeCmd<PackageSource[]>("remove_package_source", { id }))
      setNotice({ tone: "success", text: "Package Source removed." })
    } catch (error) { setNotice({ tone: "error", text: errorText(error) }) }
    finally { setBusy("") }
  }
  return (
    <form className="content-section settings-section" onSubmit={save}>
      <div className="settings-tabs">
        {([["receiver", "Receiver"], ["sources", "Sources"], ["debrid", "Debrid"], ["app", "App"]] as const).map(([id, label]) => (
          <button key={id} type="button" className={cn("settings-tab", tab === id && "active")} onClick={() => setTab(id)}>{label}</button>
        ))}
      </div>
      {tab === "receiver" && (
        <SettingsCard icon={MonitorUp} title="PS5 receiver" description="SSPI payload on the console.">
          <Field label="PS5 IPv4 or hostname"><Input value={draft.ps5Host} onChange={(event) => setDraft({ ...draft, ps5Host: event.target.value })} placeholder="Console IP address or hostname" /></Field>
          <Field label="Payload port"><Input type="number" value={draft.ps5Port} onChange={(event) => setDraft({ ...draft, ps5Port: Number(event.target.value) })} /></Field>
          <p className="text-sm text-muted-foreground">App expects receiver v1.0.3. Download the ELF and reload it on the PS5 after updating the app or changing the port.</p>
          <div className="flex flex-wrap gap-2">
            <Button type="button" className={cn(flash === "ps5" && "btn-ok")} variant="outline" onClick={() => test("ps5", "test_ps5", { host: draft.ps5Host, port: draft.ps5Port })} disabled={Boolean(busy)}>{busy === "ps5" ? <LoaderCircle className="animate-spin" /> : flash === "ps5" ? <Check /> : <Wifi />} {flash === "ps5" ? "Connected" : "Test receiver"}</Button>
            <Button type="button" variant="ghost" onClick={downloadReceiver} disabled={payloadBusy}>{payloadBusy ? <LoaderCircle className="animate-spin" /> : <Download />} Download receiver ELF</Button>
          </div>
        </SettingsCard>
      )}
      {tab === "sources" && (
        <SettingsCard icon={Library} title="Package Sources" description="Install a package source from a file or URL. Enable the sources you want to search.">
          <Field label="Install from URL"><div className="source-install"><Input value={sourceUrl} onChange={(event) => setSourceUrl(event.target.value)} placeholder=".gssource URL (optional)" /><Button type="button" onClick={installSource} disabled={Boolean(busy)}>{busy === "source-install" ? <LoaderCircle className="animate-spin" /> : <Download />} Install</Button></div></Field>
          <Button type="button" variant="outline" onClick={browseSource} disabled={Boolean(busy)}>{busy === "source-browse" ? <LoaderCircle className="animate-spin" /> : <FolderOpen />} Browse .gssource</Button>
          <div className="source-list">
            {sources.map((source) => <div className="source-row" key={source.id}><div><strong>{source.name}</strong><span>{source.version} / {source.engineType} / {source.trust}</span><p>{source.description}</p></div><Switch checked={source.enabled} onCheckedChange={(enabled) => toggleSource(source.id, enabled)} disabled={Boolean(busy)} /><Button type="button" size="sm" variant="ghost" onClick={() => removeSource(source.id)} disabled={Boolean(busy)}>Remove</Button></div>)}
            {sources.length === 0 && <div className="source-empty">No package sources installed. Use Browse or install a .gssource URL to get started.</div>}
          </div>
        </SettingsCard>
      )}
      {tab === "debrid" && (
        <SettingsCard icon={ShieldCheck} title="Real-Debrid" description="Unlock hoster links before download.">
          <ToggleField label="Enable Real-Debrid" detail={draft.realDebridConfigured ? "A token is stored" : "No token stored"} checked={draft.realDebridEnabled} onCheckedChange={(checked) => setDraft({ ...draft, realDebridEnabled: checked })} />
          <Field label="API token"><Input type="password" value={rdToken} onChange={(event) => setRdToken(event.target.value)} placeholder={draft.realDebridConfigured ? "Leave blank to keep saved token" : "Paste token"} /></Field>
          <Button type="button" className={cn(flash === "rd" && "btn-ok")} variant="outline" onClick={() => test("rd", "verify_real_debrid", { token: rdToken || undefined })} disabled={Boolean(busy)}>{busy === "rd" ? <LoaderCircle className="animate-spin" /> : flash === "rd" ? <Check /> : <ShieldCheck />} {flash === "rd" ? "Token valid" : "Verify token"}</Button>
        </SettingsCard>
      )}
      {tab === "app" && (
        <>
        <SettingsCard icon={FolderOpen} title="Downloads" description="Where archives land on this PC.">
          <Field label="Download folder"><Input value={draft.downloadDir} onChange={(event) => setDraft({ ...draft, downloadDir: event.target.value })} /></Field>
          <ToggleField label="Reduce motion" detail="Cuts animations" checked={draft.reduceMotion} onCheckedChange={(checked) => setDraft({ ...draft, reduceMotion: checked })} />
        </SettingsCard>
        <SettingsCard icon={HardDriveDownload} title="Max bandwidth" description="LAN upload to the PS5 only. Downloads stay one stream per volume.">
          <ToggleField label="Max bandwidth" detail="Balanced 4 lanes / Max up to 12 (LAN upload only)" checked={(draft.transferMode || "balanced") === "max"} onCheckedChange={(checked) => setDraft({ ...draft, transferMode: checked ? "max" : "balanced" })} />
          {(draft.transferMode || "balanced") === "max" && (
            <Field label="Upload lanes 1–12">
              <Input type="number" min={1} max={12} value={draft.uploadLanes || 4} onChange={(event) => setDraft({ ...draft, uploadLanes: Math.min(12, Math.max(1, Number(event.target.value) || 4)) })} />
            </Field>
          )}
          <p className="text-sm text-muted-foreground">Needs the reloaded ELF (listen 64 + NODELAY). If mounts start failing, back to Balanced. After changing the receiver port, reload the ELF.</p>
        </SettingsCard>
        </>
      )}
      <div className="settings-save"><div><strong>Apply configuration</strong><p>Browsing never requires a PS5 connection.</p></div><Button type="submit" size="lg" disabled={Boolean(busy)}>{busy === "save" ? <LoaderCircle className="animate-spin" /> : <Check />} Save settings</Button></div>
    </form>
  )
}

function SettingsCard({ icon: Icon, title, description, children }: { icon: typeof Wifi; title: string; description: string; children: React.ReactNode }) {
  return <Card className="settings-card"><CardHeader><div className="settings-card-icon"><Icon /></div><CardTitle>{title}</CardTitle><CardDescription>{description}</CardDescription></CardHeader><CardContent className="space-y-4">{children}</CardContent></Card>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label className="field"><span>{label}</span>{children}</label>
}

function ToggleField({ label, detail, checked, onCheckedChange }: { label: string; detail: string; checked: boolean; onCheckedChange: (checked: boolean) => void }) {
  return <div className="toggle-field"><div><strong>{label}</strong><span>{detail}</span></div><Switch checked={checked} onCheckedChange={onCheckedChange} /></div>
}

function ReceiverPrompt({ open, message, host, port, busy, onClose, onSettings, onDownload }: {
  open: boolean
  message: string
  host: string
  port: number
  busy: boolean
  onClose: () => void
  onSettings: () => void
  onDownload: () => Promise<void>
}) {
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="receiver-dialog">
        <DialogHeader>
          <div className="settings-card-icon"><MonitorUp /></div>
          <DialogTitle>PS5 connection required</DialogTitle>
          <DialogDescription>Browsing and package links work without a console. Installation needs the SSPI receiver running on your PS5.</DialogDescription>
        </DialogHeader>
        <div className="receiver-status"><span>Configured target</span><strong>{host ? `${host}:${port}` : "Not configured"}</strong></div>
        <div className="inline-error"><CircleAlert /><span>{message}</span></div>
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>Not now</Button>
          <Button variant="outline" onClick={onDownload} disabled={busy}>{busy ? <LoaderCircle className="animate-spin" /> : <Download />} Download receiver ELF</Button>
          <Button onClick={onSettings}><SettingsIcon /> Open connection settings</Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
