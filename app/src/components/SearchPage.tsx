/* =====================================================================
   Search — the SSPI PS4 search screen with the library built in: the
   search field, "Your library" as a row of cases with installed and
   update versions, and the selected title underneath. Typing swaps the
   library for results, which is the one list on this page that scrolls.
   ===================================================================== */
import { useEffect, useLayoutEffect, useMemo, useRef, type MutableRefObject, type ReactNode } from "react"
import type { UpdateInfo } from "@/App"
import { compareVersions, plural } from "@/lib/format"
import { setPageKeys, isTyping } from "@/lib/keys"
import { installedVersion, type LibraryEntry } from "@/lib/library"
import { Spring, SPRINGS, clamp, motionOK, onFrame } from "@/lib/motion"
import { packageKind, packageVersion } from "@/lib/package-selection"
import { compatibleProviders, directCandidate, groupedGames, type ProviderHosts } from "@/lib/providers"
import { useProviders } from "./ProviderContext"
import { displayText } from "@/lib/display"
import { CASE_ASPECT, coverTint } from "@/stage/art"
import { getStage } from "@/stage/stage"
import type { ConsoleKind, Game, LoadState, PackageCandidate, PackageSource } from "@/types"
import { CaseAnchor, CaseThumb } from "./CaseAnchor"
import { Icon } from "./Icon"
import type { Hint } from "./Shell"
import type { OptionsTab } from "./OptionsOverlay"

type Props = {
  query: string
  setQuery: (value: string) => void
  onSearch: (term: string) => void
  searchState: LoadState
  searchError: string
  searchedFor: string
  results: Game[]
  region: string
  setRegion: (value: string) => void
  resultsScroll: MutableRefObject<number>
  library: LibraryEntry[]
  libIndex: number
  setLibIndex: (value: number) => void
  libExpanded: boolean
  setLibExpanded: (value: boolean) => void
  updates: Record<string, UpdateInfo>
  pendingUpdates: Record<string, string>
  onCheckUpdate: (entry: LibraryEntry) => void
  onQueueUpdate: (entry: LibraryEntry, candidate: PackageCandidate, available: PackageCandidate[], anchor: HTMLElement | null) => void
  onOpen: (game: Game, from?: { el: HTMLElement | null; kind: "case" | "thumb" }) => void
  openEntry: (entry: LibraryEntry, el: HTMLElement | null) => void
  sources: PackageSource[]
  demo: boolean
  activeConsole: ConsoleKind
  deliveryBusy: boolean
  librarySync?: { state: "syncing" | "ready" | "partial" | "offline"; message: string; messages: string[] }
  onRefreshLibrary?: () => void
  libraryIsAuthoritative?: boolean
  onOptions: (tab: OptionsTab) => void
  onDemo: () => void
  onDock: (dock: { context: ReactNode; hints: Hint[] }) => void
  tintOn: boolean
}

function LibrarySyncNotice({ sync }: { sync?: Props["librarySync"] }) {
  if (!sync) return null
  return (
    <div className={`lib-sync ${sync.state}`} role="status" aria-live="polite">
      <span>{sync.message}</span>
      {sync.messages.length > 0 && <ul>{sync.messages.slice(0, 4).map((message, index) => <li key={`${index}:${message}`}>{message}</li>)}</ul>}
    </div>
  )
}

type UpdateView = { tone: "up" | "ok" | ""; card: string; meta: string; available?: string; pending?: boolean; candidate?: PackageCandidate; packages?: PackageCandidate[] }
type Eligible = (packages: PackageCandidate[]) => PackageCandidate[]

/** Packages the user's services can actually download: direct links, or hosts an enabled provider supports. */
const eligibility = (inventories: ProviderHosts[], demo: boolean): Eligible => packages => packages.filter(candidate => demo || directCandidate(candidate) || compatibleProviders(candidate, inventories).length > 0)

function updateView(entry: LibraryEntry, updates: Record<string, UpdateInfo>, pending: Record<string, string>, eligible: Eligible): UpdateView {
  const id = entry.titleId.toUpperCase()
  if (pending[id] !== undefined) {
    const v = pending[id]
    return { tone: "up", card: v ? `Update ${v} on its way` : "Update on its way", meta: v ? `Update ${v} is on its way` : "An update is on its way", pending: true }
  }
  const info = updates[id]
  if (!info) return { tone: "", card: "", meta: "" }
  if (info.state === "checking") return { tone: "", card: "Checking for updates", meta: "Checking your sources for updates" }
  if (info.state === "error") return { tone: "", card: "", meta: "The update check didn't finish" }
  const usable = eligible(info.packages || [])
  const candidate = usable.filter(item => packageKind(item) === "update" && packageVersion(item)).sort((a, b) => compareVersions(packageVersion(b), packageVersion(a)))[0]
  const latest = candidate ? packageVersion(candidate) : ""
  const shown = candidate?.version?.trim().replace(/^v(?:ersion)?\s*/i, "") || latest
  if (candidate && compareVersions(latest, installedVersion(entry)) > 0) return { tone: "up", card: `Update ${shown}`, meta: `Update ${shown} available`, available: shown, candidate, packages: usable }
  return { tone: "ok", card: "Up to date", meta: "No newer update is listed" }
}

const highlight = (text: string, q: string) => {
  const needle = q.trim().toLowerCase()
  const index = needle ? text.toLowerCase().indexOf(needle) : -1
  if (index < 0) return text
  return <>{text.slice(0, index)}<span className="mark">{text.slice(index, index + needle.length)}</span>{text.slice(index + needle.length)}</>
}

export function SearchPage(props: Props) {
  const { query, setQuery, onSearch, searchState, library, libIndex, setLibIndex, libExpanded, setLibExpanded, demo, onDock } = props
  const inputRef = useRef<HTMLInputElement>(null)
  const timer = useRef<number | null>(null)
  const focusFirst = useRef(false)
  const idle = searchState === "idle"

  const type = (value: string) => {
    setQuery(value)
    if (timer.current) window.clearTimeout(timer.current)
    const term = value.trim()
    if (!term) { onSearch(""); return }
    if (term.length < 2) return
    timer.current = window.setTimeout(() => onSearch(term), 420)
  }
  useEffect(() => () => { if (timer.current) window.clearTimeout(timer.current) }, [])

  const submit = (event: React.FormEvent) => {
    event.preventDefault()
    if (timer.current) window.clearTimeout(timer.current)
    if (!query.trim()) { inputRef.current?.focus(); return }
    focusFirst.current = true
    onSearch(query)
  }
  const clear = () => {
    if (timer.current) window.clearTimeout(timer.current)
    setQuery("")
    onSearch("")
    inputRef.current?.focus()
  }

  return (
    <div className="page search-page enter">
      <form className="search-box" role="search" onSubmit={submit}>
        <Icon name="search" />
        <input
          ref={inputRef} type="search" autoComplete="off" spellCheck={false} value={query}
          placeholder="Search games or a CUSA or PPSA ID" aria-label="Search titles"
          onChange={event => type(event.target.value)}
          onKeyDown={event => {
            if (event.key === "ArrowDown") {
              event.preventDefault()
              ;(document.querySelector<HTMLElement>(".rrow") || document.querySelector<HTMLElement>(".lib-card.is-selected"))?.focus()
            }
            if (event.key === "Escape") {
              event.preventDefault()
              event.stopPropagation()
              if (query) clear(); else inputRef.current?.blur()
            }
          }}
        />
        <button type="submit" className="search-go" disabled={searchState === "loading"}><span className="face cross">⏎</span>Search</button>
      </form>
      {idle
        ? <Library {...props} inputRef={inputRef} />
        : <Results {...props} inputRef={inputRef} focusFirst={focusFirst} onClear={clear} />}
      <KeysAndDock idle={idle} library={library} libIndex={libIndex} setLibIndex={setLibIndex} libExpanded={libExpanded} setLibExpanded={setLibExpanded}
        inputRef={inputRef} setQuery={setQuery} type={type} onClear={clear} props={props} onDock={onDock} demo={demo} />
    </div>
  )
}

/* ---------------------------------------------------------------- library */
function Library(props: Props & { inputRef: React.RefObject<HTMLInputElement> }) {
  const { library, libIndex, setLibIndex, libExpanded, setLibExpanded, updates, pendingUpdates, onCheckUpdate, openEntry, activeConsole, demo, onOptions, onDemo, sources, tintOn } = props
  const { inventories } = useProviders()
  const eligible = eligibility(inventories, demo)
  const libRef = useRef<HTMLElement>(null)
  const stripRef = useRef<HTMLDivElement>(null)
  const trackRef = useRef<HTMLDivElement>(null)
  const layout = useRef({ cols: 7, caseW: 150, x: new Spring(0, 170, 24) })
  const index = clamp(libIndex, 0, Math.max(0, library.length - 1))
  const entry = library[index]
  const live = useRef({ index, libExpanded, count: library.length })
  live.current = { index, libExpanded, count: library.length }

  const place = (instant = false) => {
    const strip = stripRef.current, track = trackRef.current, lib = libRef.current
    if (!strip || !track || !lib) return
    const { index, libExpanded, count } = live.current
    const gap = 24
    const available = strip.clientHeight - 20 - 64
    const caseW = clamp(available / CASE_ASPECT, 96, 200)
    const width = strip.clientWidth - 24
    const cols = clamp(Math.floor((width + gap) / (caseW + gap)), 2, 12)
    layout.current.cols = cols
    layout.current.caseW = caseW
    lib.style.setProperty("--case-w", `${caseW.toFixed(1)}px`)
    lib.style.setProperty("--lib-cols", String(cols))
    if (libExpanded) { layout.current.x.snap(0); return }
    const first = Math.max(0, Math.min(index - (cols - 2), count - cols))
    const target = -first * (caseW + gap)
    layout.current.x.set(target)
    if (instant || !motionOK()) layout.current.x.snap(target)
  }
  useLayoutEffect(() => { place(true) }, [libExpanded, library.length])
  useEffect(() => { place() }, [index])
  useEffect(() => {
    const observer = new ResizeObserver(() => place(true))
    if (stripRef.current) observer.observe(stripRef.current)
    const stop = onFrame(dt => {
      const track = trackRef.current
      if (!track) return
      if (libExpanded) { track.style.transform = "none"; return }
      track.style.transform = `translate3d(${layout.current.x.step(dt)}px, 0, 0)`
    })
    return () => { observer.disconnect(); stop() }
  }, [libExpanded])

  // Check the selected title for a newer update after a short dwell.
  useEffect(() => {
    if (!entry) return
    const handle = window.setTimeout(() => onCheckUpdate(entry), 900)
    return () => window.clearTimeout(handle)
  }, [entry?.key])

  // Optional backdrop tint from the selected title's artwork.
  useEffect(() => {
    const stage = getStage()
    if (!stage) return
    if (!tintOn || !entry) { stage.look.setTint(null); return }
    let live = true
    void coverTint({ cover: entry.cover, title: entry.name, titleId: entry.titleId }).then(hex => { if (live) stage.look.setTint(hex) })
    return () => { live = false }
  }, [tintOn, entry?.key])
  useEffect(() => () => getStage()?.look.setTint(null), [])

  // Flip cards between strip and grid.
  const firstRects = useRef<Map<string, DOMRect> | null>(null)
  const toggle = () => {
    firstRects.current = new Map([...(trackRef.current?.querySelectorAll<HTMLElement>(".lib-card") || [])].map(card => [card.dataset.key || "", card.getBoundingClientRect()]))
    setLibExpanded(!libExpanded)
  }
  useLayoutEffect(() => {
    const first = firstRects.current
    firstRects.current = null
    if (!first || !motionOK()) return
    trackRef.current?.querySelectorAll<HTMLElement>(".lib-card").forEach((card, n) => {
      const f = first.get(card.dataset.key || ""), l = card.getBoundingClientRect()
      if (!f) return
      const dx = f.left - l.left, dy = f.top - l.top
      if (dx || dy) {
        const frames = [{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "none" }]
        const duration = 520 + n * 18
        try { card.animate(frames, { duration, easing: SPRINGS.soft.easing }) }
        catch { card.animate(frames, { duration, easing: "cubic-bezier(.2,.8,.2,1)" }) }
      }
    })
  }, [libExpanded])

  if (!library.length) {
    const noSources = !demo && sources.filter(source => source.enabled).length === 0
    return (
      <section className="library" aria-label="Your library">
        <div className="lib-head">
          <div className="lib-title"><h2>Your library</h2><span>{props.libraryIsAuthoritative ? `Titles found on your ${activeConsole.toUpperCase()}` : `Titles installed through SSPI on your ${activeConsole.toUpperCase()}`}</span></div>
          {props.librarySync && <button type="button" className="btn ghost sm" disabled={props.librarySync.state === "syncing"} onClick={props.onRefreshLibrary}>{props.librarySync.state === "syncing" ? <span className="spinner" /> : <Icon name="refresh" />}Refresh library</button>}
        </div>
        <LibrarySyncNotice sync={props.librarySync} />
        <div className="lib-empty">
          <Icon name="library" />
          <h3>{props.libraryIsAuthoritative ? `No titles found on your ${activeConsole.toUpperCase()}` : "Nothing installed through SSPI yet"}</h3>
          <p>{props.libraryIsAuthoritative ? "The receiver reports no installed titles on this console." : noSources ? "Install a package source in Options, then search for a game. Titles you install on this console appear here with their versions." : `Search for a game to install it. Titles you install on your ${activeConsole.toUpperCase()} appear here, with update checks for the one you select.`}</p>
          <div className="row">
            {noSources && <button type="button" className="btn primary sm" onClick={() => onOptions("sources")}><Icon name="plus" />Add a package source</button>}
            <button type="button" className="btn sm" onClick={() => props.inputRef.current?.focus()}><Icon name="search" />Search titles</button>
            {!demo && <button type="button" className="btn ghost sm" onClick={onDemo}>Try the offline preview</button>}
          </div>
        </div>
      </section>
    )
  }

  const view = entry ? updateView(entry, updates, pendingUpdates, eligible) : null
  return (
    <section ref={libRef} className={`library ${libExpanded ? "is-expanded" : ""}`} aria-label="Your library">
      <div className="lib-head">
        <div className="lib-title"><h2>Your library</h2><span>{props.libraryIsAuthoritative ? `${plural(library.length, "title")} found on your ${activeConsole.toUpperCase()}` : `${plural(library.length, "title")} installed through SSPI on your ${activeConsole.toUpperCase()}`}</span></div>
        <div className="lib-head-actions">
          {props.librarySync && <button type="button" className="btn ghost sm" disabled={props.librarySync.state === "syncing"} onClick={props.onRefreshLibrary}>{props.librarySync.state === "syncing" ? <span className="spinner" /> : <Icon name="refresh" />}Refresh</button>}
          <button type="button" className="link lib-toggle" onClick={toggle}><span className="face triangle">L</span>{libExpanded ? "Collapse library" : "Expand library"}</button>
        </div>
      </div>
      <LibrarySyncNotice sync={props.librarySync} />
      <div ref={stripRef} className="lib-strip" data-case-clip={libExpanded ? undefined : ""} data-case-scroll={libExpanded ? "" : undefined}>
        <div ref={trackRef} className="lib-track">
          {library.map((item, n) => {
            const v = updateView(item, updates, pendingUpdates, eligible)
            return (
              <button
                key={item.key} type="button" data-key={item.key} data-hover-case
                className={`lib-card ${n === index ? "is-selected" : ""}`}
                aria-label={`${item.name}, installed ${installedVersion(item) || ""} on ${item.target.toUpperCase()}`}
                onPointerEnter={() => setLibIndex(n)}
                onFocus={() => setLibIndex(n)}
                onClick={event => { setLibIndex(n); openEntry(item, event.currentTarget.querySelector<HTMLElement>("[data-case-anchor]")) }}
              >
                <CaseAnchor spec={{ key: item.titleId.toUpperCase(), cover: item.cover, title: item.name, titleId: item.titleId, kind: "library" }} delay={Math.min(n, 14) * 0.035}>
                  {v.available && <i className={`update-dot ${v.pending ? "" : "pulse"}`} />}
                  {v.pending && <i className="update-dot" />}
                </CaseAnchor>
                <span className="lib-name">{item.name}</span>
                <span className="lib-line">{installedVersion(item) ? `Installed ${installedVersion(item)}` : "Installed"}</span>
                <span className={`lib-line ${v.tone}`}>{v.card || " "}</span>
              </button>
            )
          })}
        </div>
      </div>
      {entry && view && (
        <div className="lib-info">
          <div key={entry.key} className="lib-info-main swap">
            <strong>{entry.name}</strong>
            <div className="lib-meta">
              <span>{entry.titleId}</span>
              <span>{installedVersion(entry) ? `Installed ${installedVersion(entry)} on your ${entry.target.toUpperCase()}` : `Installed on your ${entry.target.toUpperCase()}`}</span>
              {view.meta && <span className={view.tone === "up" ? "up" : ""}>{view.meta}</span>}
            </div>
            {!props.libraryIsAuthoritative && entry.installedAt > 1e11 && <p>Installed through SSPI on {new Date(entry.installedAt).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" })}.</p>}
          </div>
          <span className="lib-pos">{index + 1} / {library.length}</span>
        </div>
      )}
    </section>
  )
}

/* ---------------------------------------------------------------- results */
function Results(props: Props & { inputRef: React.RefObject<HTMLInputElement>; focusFirst: MutableRefObject<boolean>; onClear: () => void }) {
  const { searchState, searchError, searchedFor, results, region, setRegion, resultsScroll, library, onOpen, sources, demo, onOptions, onDemo, onSearch, focusFirst } = props
  const listRef = useRef<HTMLDivElement>(null)
  const grouped = useMemo(() => groupedGames(results), [results])
  const regions = useMemo(() => ["All regions", ...[...new Set(grouped.flatMap(game => (game.variants || [game]).map(v => v.region).filter(Boolean)))].sort()], [grouped])
  const shown = grouped.filter(game => region === "All regions" || (game.variants || [game]).some(v => v.region === region))
  const owned = new Map(library.map(entry => [entry.titleId.toUpperCase(), entry]))
  const enabled = sources.filter(source => source.enabled)

  useLayoutEffect(() => {
    if (listRef.current) listRef.current.scrollTop = resultsScroll.current
  }, [])
  useEffect(() => {
    if (searchState === "success" && focusFirst.current) {
      focusFirst.current = false
      window.setTimeout(() => listRef.current?.querySelector<HTMLElement>(".rrow")?.focus(), 60)
    }
  }, [searchState])

  return (
    <>
      <div className="search-status">
        <span className="q">“{searchedFor}”</span>
        {searchState === "loading" && <span className="pending">Searching your package sources<span className="dots"><i /><i /><i /></span></span>}
        {searchState === "success" && <span>{plural(shown.length, "title")}</span>}
      </div>
      {searchState === "error" && (
        <div className="search-error" role="alert">
          <Icon name="alert" />
          <div>
            <strong>The search didn't finish</strong>
            <span>{searchError}</span>
            <div className="row">
              <button type="button" className="btn sm" onClick={() => onSearch(searchedFor)}><Icon name="retry" />Try again</button>
              {!demo && <button type="button" className="btn ghost sm" onClick={() => onOptions("sources")}>Package sources</button>}
              {!demo && <button type="button" className="btn ghost sm" onClick={onDemo}>Offline preview</button>}
            </div>
          </div>
        </div>
      )}
      {searchState !== "error" && (
        <div className="search-filters">
          <div className="chips" role="group" aria-label="Region">
            {regions.map(value => <button key={value} type="button" className="chip" aria-pressed={region === value} onClick={() => setRegion(value)}>{value}</button>)}
          </div>
          <span className="search-sources">{demo ? "Offline preview catalog" : enabled.map(source => `${displayText(source.name)} ${source.version}`).join(", ")}</span>
        </div>
      )}
      <div ref={listRef} className="results scroll" role="listbox" aria-label="Search results" onScroll={event => { resultsScroll.current = event.currentTarget.scrollTop }}>
        {searchState === "loading" && !results.length && Array.from({ length: 5 }, (_, n) => <div key={n} className="skeleton" style={{ height: 70, flex: "none" }} />)}
        {shown.map((game, n) => {
          const inLibrary = owned.get(game.titleId.toUpperCase())
          const variants = game.variants?.length || 1
          const platform = /^CUSA/i.test(game.titleId) ? "PS4" : "PS5"
          const info = inLibrary ? `In your library, ${installedVersion(inLibrary) || "installed"}` : variants > 1 ? `${variants} region or source versions` : game.packages?.length ? plural(game.packages.length, "package") : game.sourceName ? `${displayText(game.sourceName)}${game.sourceVersion ? ` ${game.sourceVersion}` : ""}` : ""
          const regionLabel = region !== "All regions" ? region : game.region || (game.variants || []).map(v => v.region).find(Boolean) || "—"
          const pick = region === "All regions" ? game : { ...((game.variants || [game]).find(v => v.region === region) || game), variants: game.variants }
          return (
            <button
              key={`${game.titleId}-${game.region}-${game.sourceId || ""}`} type="button" role="option" aria-selected="false"
              className="rrow enter" style={{ animationDelay: `${Math.min(n, 12) * 35}ms` }}
              onClick={event => onOpen(pick, { el: event.currentTarget.querySelector<HTMLElement>(".rthumb"), kind: "thumb" })}
            >
              <CaseThumb spec={{ cover: game.icon, title: game.name, titleId: game.titleId }} />
              <span className="rmain"><strong>{highlight(game.name, searchedFor)}</strong><span>{game.titleId}&ensp;{platform}{info ? <>&ensp;{info}</> : null}</span></span>
              <span className="rview">View packages</span>
              <span className="pill">{regionLabel}</span>
              <Icon name="chevR" className="chev" />
            </button>
          )
        })}
        {searchState === "success" && !shown.length && <p className="empty-note">No titles match “{searchedFor}”{region !== "All regions" ? ` in ${region}` : ""}. Try a shorter name or the exact CUSA or PPSA ID.</p>}
      </div>
    </>
  )
}

/* ---------------------------------------------------------------- keyboard and dock */
function KeysAndDock({ idle, library, libIndex, setLibIndex, libExpanded, setLibExpanded, inputRef, type, onClear, props, onDock, demo }: {
  idle: boolean; library: LibraryEntry[]; libIndex: number; setLibIndex: (n: number) => void; libExpanded: boolean; setLibExpanded: (v: boolean) => void
  inputRef: React.RefObject<HTMLInputElement>; setQuery: (v: string) => void; type: (v: string) => void; onClear: () => void; props: Props
  onDock: Props["onDock"]; demo: boolean
}) {
  const { inventories } = useProviders()
  const eligibleRef = useRef<Eligible>(eligibility(inventories, demo))
  eligibleRef.current = eligibility(inventories, demo)
  const state = useRef({ idle, library, libIndex, libExpanded, props })
  state.current = { idle, library, libIndex, libExpanded, props }
  const anchorOf = (n: number) => document.querySelector<HTMLElement>(`.lib-card[data-key="${CSS.escape(library[n]?.key || "")}"] [data-case-anchor]`)

  useEffect(() => setPageKeys(event => {
    const { idle, library, libIndex, libExpanded, props } = state.current
    const k = event.key
    if (isTyping()) return
    if (!idle) {
      const rows = [...document.querySelectorAll<HTMLElement>(".rrow")]
      const current = (document.activeElement as HTMLElement | null)?.closest?.(".rrow") as HTMLElement | null
      const i = current ? rows.indexOf(current) : -1
      if (k === "ArrowDown" && rows.length) { event.preventDefault(); rows[clamp(i + 1, 0, rows.length - 1)].focus() }
      if (k === "ArrowUp") { event.preventDefault(); if (i <= 0) inputRef.current?.focus(); else rows[i - 1].focus() }
      if (k === "Escape") { event.preventDefault(); onClear() }
      if (k.length === 1 && /\S/.test(k) && !event.ctrlKey && !event.metaKey && !event.altKey) { event.preventDefault(); inputRef.current?.focus(); type(`${props.query}${k}`) }
      return
    }
    const cols = libExpanded ? Number(getComputedStyle(document.querySelector(".library") || document.body).getPropertyValue("--lib-cols")) || 6 : 1
    const step = ({ ArrowLeft: -1, ArrowRight: 1, ArrowUp: libExpanded ? -cols : 0, ArrowDown: libExpanded ? cols : 0 } as Record<string, number>)[k]
    if (step !== undefined) {
      event.preventDefault()
      if (k === "ArrowUp" && !libExpanded) { inputRef.current?.focus(); return }
      if (!library.length) return
      const next = clamp(libIndex + step, 0, library.length - 1)
      setLibIndex(next)
      document.querySelector<HTMLElement>(`.lib-card[data-key="${CSS.escape(library[next].key)}"]`)?.focus({ preventScroll: true })
      return
    }
    const entry = library[clamp(libIndex, 0, library.length - 1)]
    if (k === "Enter" && entry && !(document.activeElement as HTMLElement | null)?.closest?.("button, input")) { event.preventDefault(); props.openEntry(entry, anchorOf(libIndex)); return }
    if (k === " " && entry) { event.preventDefault(); const v = updateView(entry, props.updates, props.pendingUpdates, eligibleRef.current); if (v.candidate && !v.pending) props.onQueueUpdate(entry, v.candidate, v.packages || [v.candidate], anchorOf(libIndex)); return }
    if (k === "l" || k === "L") { event.preventDefault(); setLibExpanded(!libExpanded); return }
    if (k.length === 1 && /\S/.test(k) && !event.ctrlKey && !event.metaKey && !event.altKey) { event.preventDefault(); inputRef.current?.focus(); type(`${props.query}${k}`) }
  }), [])

  const entry = library[clamp(libIndex, 0, Math.max(0, library.length - 1))]
  const view = entry ? updateView(entry, props.updates, props.pendingUpdates, eligibleRef.current) : null
  useEffect(() => {
    if (!idle) {
      onDock({
        context: props.searchState === "loading" ? "Searching" : "Search",
        hints: [
          { key: "Enter", label: "View packages", run: () => { const row = (document.activeElement as HTMLElement | null)?.closest(".rrow") as HTMLElement | null; (row || document.querySelector<HTMLElement>(".rrow"))?.click() } },
          { key: "Arrows", glyph: "↑↓", face: "neutral", label: "Move" },
          { key: "Escape", label: "Back to library", run: onClear },
        ],
      })
      return
    }
    onDock({
      context: entry ? <>Search<small>{entry.name}</small></> : "Search",
      hints: [
        { key: "Enter", label: "Open", disabled: !entry, run: () => entry && props.openEntry(entry, anchorOf(libIndex)) },
        { key: "Space", label: view?.pending ? "Update queued" : view?.available ? `Queue update ${view.available}` : "Queue update", disabled: !view?.candidate || view.pending || props.deliveryBusy, run: () => { if (entry && view?.candidate) props.onQueueUpdate(entry, view.candidate, view.packages || [view.candidate], anchorOf(libIndex)) } },
        { key: "L", glyph: "L", face: "triangle", label: libExpanded ? "Collapse library" : "Expand library", disabled: !library.length, run: () => setLibExpanded(!libExpanded) },
        { key: "Type", glyph: "A–Z", face: "neutral", label: "Type to search", run: () => inputRef.current?.focus() },
      ],
    })
  }, [idle, entry?.key, view?.card, view?.pending, libExpanded, library.length, props.searchState, props.deliveryBusy])
  return null
}
