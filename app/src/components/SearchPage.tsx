/* =====================================================================
   Search — the SSPI PS4 search screen: the search field over the results
   list, which is the one list on this page that scrolls. Before typing,
   the titles opened recently stand in a row of cases.
   ===================================================================== */
import { useEffect, useLayoutEffect, useMemo, useRef, type MutableRefObject } from "react"
import { plural } from "@/lib/format"
import { setPageKeys, isTyping } from "@/lib/keys"
import { installedVersion, type LibraryEntry } from "@/lib/library"
import { clamp } from "@/lib/motion"
import { groupedGames } from "@/lib/providers"
import { displayText } from "@/lib/display"
import type { ConsoleKind, Game, LoadState, PackageSource } from "@/types"
import { CaseAnchor, CaseThumb } from "./CaseAnchor"
import { Icon } from "./Icon"
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
  recent: Game[]
  onClearRecent: () => void
  onOpen: (game: Game, from?: { el: HTMLElement | null; kind: "case" | "thumb" }) => void
  sources: PackageSource[]
  demo: boolean
  activeConsole: ConsoleKind
  onOptions: (tab: OptionsTab) => void
  onDemo: () => void
}

const highlight = (text: string, q: string) => {
  const needle = q.trim().toLowerCase()
  const index = needle ? text.toLowerCase().indexOf(needle) : -1
  if (index < 0) return text
  return <>{text.slice(0, index)}<span className="mark">{text.slice(index, index + needle.length)}</span>{text.slice(index + needle.length)}</>
}

export function SearchPage(props: Props) {
  const { query, setQuery, onSearch, searchState } = props
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
  useEffect(() => { if (idle && !props.recent.length) inputRef.current?.focus() }, [])

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
              ;(document.querySelector<HTMLElement>(".rrow") || document.querySelector<HTMLElement>(".si-case"))?.focus()
            }
            if (event.key === "Escape") {
              event.preventDefault()
              event.stopPropagation()
              if (query) clear(); else inputRef.current?.blur()
            }
          }}
        />
        {query && <button type="button" className="search-clear" aria-label="Clear search" title="Clear search (Esc)" onClick={clear}><Icon name="x" /></button>}
        <button type="submit" className="search-go" disabled={searchState === "loading"}><span className="face cross">⏎</span>Search</button>
      </form>
      {idle
        ? <Idle {...props} />
        : <Results {...props} focusFirst={focusFirst} />}
      <SearchKeys idle={idle} inputRef={inputRef} type={type} onClear={clear} props={props} />
    </div>
  )
}

/* ---------------------------------------------------------------- before searching */
function Idle(props: Props) {
  const { recent, onOpen, sources, demo, onOptions, onDemo, onClearRecent, activeConsole } = props
  const enabled = sources.filter(source => source.enabled)
  const noSources = !demo && enabled.length === 0
  const owned = new Set(props.library.map(entry => entry.titleId.toUpperCase()))
  return (
    <section className="search-idle" aria-label="Start a search">
      {recent.length > 0 && (
        <div className="si-recent">
          <div className="si-head">
            <h2>Recently viewed</h2>
            <button type="button" className="link" onClick={onClearRecent}>Clear</button>
          </div>
          <div className="si-row" data-case-clip="">
            {recent.map((game, n) => (
              <button
                key={`${game.titleId}-${n}`} type="button" className="si-case" data-hover-case
                aria-label={`${game.name}, ${game.titleId}`}
                onClick={event => onOpen(game, { el: event.currentTarget.querySelector<HTMLElement>("[data-case-anchor]"), kind: "case" })}
              >
                <CaseAnchor spec={{ key: game.titleId.toUpperCase(), cover: game.icon, title: game.name, titleId: game.titleId, kind: "library" }} delay={Math.min(n, 10) * 0.02} />
                <span className="si-name">{game.name}</span>
                <span className="si-line">{game.titleId}{owned.has(game.titleId.toUpperCase()) ? `, on your ${activeConsole.toUpperCase()}` : ""}</span>
              </button>
            ))}
          </div>
        </div>
      )}
      <div className={`si-hint ${recent.length ? "" : "alone"}`}>
        <Icon name="search" />
        <h3>Search your package sources</h3>
        <p>Type a game's name or its CUSA or PPSA title ID. Results show every region and source, and the packages each one offers.</p>
        <p className="si-sources">{demo ? "Offline preview catalog" : enabled.length ? `Searching ${enabled.map(source => `${displayText(source.name)} ${source.version}`).join(", ")}` : "No package source is enabled yet."}</p>
        <div className="row">
          {noSources && <button type="button" className="btn primary sm" onClick={() => onOptions("sources")}><Icon name="plus" />Add a package source</button>}
          {!demo && <button type="button" className="btn ghost sm" onClick={onDemo}>Try the offline preview</button>}
        </div>
      </div>
    </section>
  )
}

/* ---------------------------------------------------------------- results */
function Results(props: Props & { focusFirst: MutableRefObject<boolean> }) {
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

/* ---------------------------------------------------------------- keyboard */
function SearchKeys({ idle, inputRef, type, onClear, props }: {
  idle: boolean; inputRef: React.RefObject<HTMLInputElement>; type: (v: string) => void; onClear: () => void; props: Props
}) {
  const state = useRef({ idle, props })
  state.current = { idle, props }

  useEffect(() => setPageKeys(event => {
    const { idle, props } = state.current
    const k = event.key
    if (isTyping()) return
    const selector = idle ? ".si-case" : ".rrow"
    const rows = [...document.querySelectorAll<HTMLElement>(selector)]
    const current = (document.activeElement as HTMLElement | null)?.closest?.(selector) as HTMLElement | null
    const i = current ? rows.indexOf(current) : -1
    const forward = idle ? "ArrowRight" : "ArrowDown", back = idle ? "ArrowLeft" : "ArrowUp"
    if (k === forward && rows.length) { event.preventDefault(); rows[clamp(i + 1, 0, rows.length - 1)].focus(); return }
    if (k === back) { event.preventDefault(); if (i <= 0) inputRef.current?.focus(); else rows[i - 1].focus(); return }
    if (idle && k === "ArrowUp") { event.preventDefault(); inputRef.current?.focus(); return }
    if (k === "Escape" && !idle) { event.preventDefault(); onClear(); return }
    if (k.length === 1 && /\S/.test(k) && !event.ctrlKey && !event.metaKey && !event.altKey) { event.preventDefault(); inputRef.current?.focus(); type(`${props.query}${k}`) }
  }), [])

  return null
}
