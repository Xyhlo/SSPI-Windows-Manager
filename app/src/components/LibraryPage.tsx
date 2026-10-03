/* =====================================================================
   Library — the SSPI PS4 "Your library" as its own section: a row of
   cases (L expands it to a grid), each with its title and installed or
   update version, and a quiet line underneath with the selected title's
   details and actions. The PS5 | PS4 switch picks the console. Clicking
   a case opens its game page; right-clicking it changes its cover on the
   console. The pointer selects what it rests on but never scrolls the
   row; the keyboard and the mouse wheel do.
   ===================================================================== */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react"
import type { LibraryView, UpdateInfo } from "@/App"
import { restoreTitleIcon } from "@/lib/console-api"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe } from "@/lib/console-types"
import { compareVersions, errorText, plural } from "@/lib/format"
import { portraitFromIcon } from "@/lib/image"
import { isTyping, setPageKeys } from "@/lib/keys"
import { installedVersion, shelfLayout, type LibraryEntry } from "@/lib/library"
import { Spring, SPRINGS, clamp, motionOK, onFrame } from "@/lib/motion"
import { packageKind, packageVersion } from "@/lib/package-selection"
import { compatibleProviders, directCandidate, type ProviderHosts } from "@/lib/providers"
import { CASE_ASPECT, coverTint } from "@/stage/art"
import { getStage } from "@/stage/stage"
import type { ConsoleKind, PackageCandidate, Settings } from "@/types"
import { CaseAnchor } from "./CaseAnchor"
import { ConsoleSwitch } from "./Controls"
import { CoverEditor } from "./CoverEditor"
import { Icon } from "./Icon"
import type { OptionsTab } from "./OptionsOverlay"
import { useProviders } from "./ProviderContext"
import { toast } from "./toasts"

type Props = {
  target: ConsoleKind
  libraries: Record<ConsoleKind, LibraryView>
  probes: Partial<Record<ConsoleKind, ConsoleProbe>>
  settings: Settings
  demo: boolean
  selectedKey?: string
  onSelect: (key: string) => void
  expanded: boolean
  setExpanded: (value: boolean) => void
  updates: Record<string, UpdateInfo>
  pendingUpdates: Record<string, string>
  onCheckUpdate: (entry: LibraryEntry) => void
  onQueueUpdate: (entry: LibraryEntry, candidate: PackageCandidate, available: PackageCandidate[], anchor: HTMLElement | null) => void
  deliveryBusy: boolean
  onConsole: (target: ConsoleKind) => void
  onRefresh: (target: ConsoleKind) => void
  onOpenGame: (entry: LibraryEntry, el: HTMLElement | null) => void
  onIconChanged: (target: ConsoleKind, titleId: string, icon: string | null, customIcon: boolean) => void
  onLoadReceiver: (target: ConsoleKind) => void
  loadingReceiver: ConsoleKind | null
  onOptions: (tab: OptionsTab) => void
  onSearch: () => void
  onDemo: () => void
  tintOn: boolean
}

type UpdateView = { tone: "up" | "ok" | ""; card: string; meta: string; available?: string; pending?: boolean; candidate?: PackageCandidate; packages?: PackageCandidate[] }
type Eligible = (packages: PackageCandidate[]) => PackageCandidate[]

/** Packages the user's services can actually download: direct links, or hosts an enabled provider supports. */
const eligibility = (inventories: ProviderHosts[], demo: boolean): Eligible => packages => packages.filter(candidate => demo || directCandidate(candidate) || compatibleProviders(candidate, inventories).length > 0)

function updateView(entry: LibraryEntry, updates: Record<string, UpdateInfo>, pending: Record<string, string>, eligible: Eligible): UpdateView {
  const id = entry.titleId.toUpperCase()
  if (pending[id] !== undefined) {
    const v = pending[id]
    return { tone: "up", card: v ? `Updating to ${v}` : "Update on its way", meta: v ? `Update ${v} is on its way` : "An update is on its way", pending: true }
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

/** The native tooltip names a card only when its title is cut off. */
const nameIfCut = (card: HTMLElement) => {
  const label = card.querySelector<HTMLElement>(".lib-name")
  card.title = label && label.scrollWidth > label.clientWidth ? label.textContent || "" : ""
}

const ageText = (at?: number) => {
  if (!at) return ""
  const mins = Math.max(0, Math.round((Date.now() - at) / 60_000))
  if (mins < 1) return "just now"
  if (mins < 60) return `${mins} min ago`
  const hours = Math.round(mins / 60)
  return hours < 48 ? `${hours} h ago` : `${Math.round(hours / 24)} days ago`
}

/** Case art: a changed cover shows the new icon; otherwise catalog box art, then the console's icon. */
function useCaseArt(entries: LibraryEntry[]) {
  const [portraits, setPortraits] = useState<Record<string, string>>({})
  const wanted = entries.map(entry => (entry.customIcon || !entry.cover) && entry.icon ? entry.icon : "").filter(Boolean)
  const key = wanted.join("|").length + ":" + wanted.length + ":" + (wanted[0] || "").slice(-24)
  useEffect(() => {
    let live = true
    for (const icon of wanted) {
      if (portraits[icon]) continue
      void portraitFromIcon(icon).then(url => { if (live) setPortraits(old => ({ ...old, [icon]: url })) }).catch(() => undefined)
    }
    return () => { live = false }
  }, [key])
  return (entry: LibraryEntry) => (entry.customIcon || !entry.cover) && entry.icon ? portraits[entry.icon] || (entry.customIcon ? undefined : entry.cover) : entry.cover
}

export function LibraryPage(props: Props) {
  const { target, libraries, probes, settings, demo, expanded, setExpanded, updates, pendingUpdates, onCheckUpdate, onQueueUpdate, tintOn } = props
  const view = libraries[target]
  const other: ConsoleKind = target === "ps5" ? "ps4" : "ps5"
  const name = target.toUpperCase()
  const probe = probes[target]
  const { inventories } = useProviders()
  const eligible = eligibility(inventories, demo)
  const [editing, setEditing] = useState<LibraryEntry | null>(null)
  const [restoring, setRestoring] = useState(false)
  const library = useMemo(() => [...view.entries].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" })), [view.entries])
  const elsewhere = useMemo(() => new Set(libraries[other].entries.map(entry => entry.titleId.toUpperCase())), [libraries, other])
  const artFor = useCaseArt(library)
  const selectedIndex = Math.max(0, library.findIndex(entry => entry.key === props.selectedKey))
  const index = clamp(selectedIndex, 0, Math.max(0, library.length - 1))
  const entry = library[index]
  const host = receiverEndpoint(settings, target).host
  const canWrite = demo || hasCapability(probe, "title-icons-v1")
  // How the selection last changed: the keyboard brings it into view, the pointer never moves the row under itself.
  const selectedBy = useRef<"key" | "pointer">("key")
  const select = (n: number, by: "key" | "pointer" = "key") => { const next = library[n]; if (!next) return; selectedBy.current = by; props.onSelect(next.key) }
  const hoverTimer = useRef(0)
  useEffect(() => () => window.clearTimeout(hoverTimer.current), [])
  const changeCover = (item: LibraryEntry) => {
    if (canWrite) { setEditing(item); return }
    const needs = probe?.receiver.expectedVersion ? `receiver ${probe.receiver.expectedVersion}` : "the latest receiver"
    toast({ tone: "info", title: "Covers can't be changed yet", text: `Load ${needs} on your ${name} to change covers.`, actions: [{ label: "Load receiver", run: () => props.onLoadReceiver(target) }] })
  }

  /* ---------------------------------------------------------------- strip layout (from the PS4 library) */
  const libRef = useRef<HTMLElement>(null)
  const stripRef = useRef<HTMLDivElement>(null)
  const trackRef = useRef<HTMLDivElement>(null)
  const layout = useRef({ cols: 7, caseW: 150, first: 0, x: new Spring(0, 170, 24) })
  const live = useRef({ index, expanded, count: library.length })
  live.current = { index, expanded, count: library.length }

  /** Lays out the strip. "follow" keeps the selection one case from the right edge (the PS4 row), "reveal" only
      scrolls when the selection is out of view, and "stay" keeps the row where the wheel left it. */
  const place = (instant = false, mode: "follow" | "reveal" | "stay" = "reveal") => {
    const strip = stripRef.current, lib = libRef.current
    if (!strip || !lib) return
    const { index, expanded, count } = live.current
    const width = Math.max(1, strip.clientWidth - 24)
    const shelf = shelfLayout(width, strip.clientHeight, count, CASE_ASPECT)
    const gap = expanded ? 22 : shelf.gap
    const caseW = expanded
      ? clamp((width + gap) / Math.max(2, Math.round((width + gap) / (148 + gap))) - gap, 112, 168)
      : shelf.caseW
    const cols = expanded ? Math.max(1, Math.min(count, Math.floor((width + gap) / (caseW + gap)))) : shelf.cols
    layout.current.cols = cols
    layout.current.caseW = caseW
    lib.style.setProperty("--case-w", `${caseW.toFixed(1)}px`)
    lib.style.setProperty("--lib-cols", String(cols))
    lib.style.setProperty("--lib-gap", `${gap}px`)
    if (expanded) { layout.current.x.snap(0); return }
    let first = layout.current.first
    if (mode === "follow") first = index - (cols - 2)
    else if (mode === "reveal" && index < first) first = index
    else if (mode === "reveal" && index > first + cols - 1) first = index - (cols - 1)
    first = clamp(first, 0, Math.max(0, count - cols))
    layout.current.first = first
    const targetX = shelf.offset - Math.min(shelf.overflow, first * (caseW + gap))
    layout.current.x.set(targetX)
    if (instant || !motionOK()) layout.current.x.snap(targetX)
  }
  useLayoutEffect(() => { place(true, "follow") }, [expanded, library.length])
  useEffect(() => { place(false, selectedBy.current === "key" ? "follow" : "reveal") }, [index])
  useEffect(() => {
    const observer = new ResizeObserver(() => place(true))
    if (stripRef.current) observer.observe(stripRef.current)
    const stop = onFrame(dt => {
      const track = trackRef.current
      if (!track) return
      if (live.current.expanded) { track.style.transform = "none"; return }
      track.style.transform = `translate3d(${layout.current.x.step(dt)}px, 0, 0)`
    })
    return () => { observer.disconnect(); stop() }
  }, [expanded, library.length])

  // The mouse wheel scrolls the row a case at a time without changing the selection.
  const wheel = useRef(0)
  const onWheel = (event: React.WheelEvent) => {
    if (expanded) return
    wheel.current += Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY
    const steps = Math.trunc(wheel.current / 60)
    if (!steps) return
    wheel.current -= steps * 60
    layout.current.first += steps
    place(false, "stay")
  }

  // The expanded grid scrolls; keep a case chosen with the keyboard in view there.
  useEffect(() => {
    if (!expanded || !entry || selectedBy.current !== "key") return
    stripRef.current?.querySelector<HTMLElement>(`.lib-card[data-key="${CSS.escape(entry.key)}"]`)?.scrollIntoView({ block: "nearest", behavior: motionOK() ? "smooth" : "auto" })
  }, [entry?.key, expanded])

  // Flip cases between strip and grid.
  const firstRects = useRef<Map<string, DOMRect> | null>(null)
  const toggle = () => {
    firstRects.current = new Map([...(trackRef.current?.querySelectorAll<HTMLElement>(".lib-card") || [])].map(card => [card.dataset.key || "", card.getBoundingClientRect()]))
    setExpanded(!expanded)
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
        const duration = 520 + Math.min(n, 24) * 14
        try { card.animate(frames, { duration, easing: SPRINGS.soft.easing }) }
        catch { card.animate(frames, { duration, easing: "cubic-bezier(.2,.8,.2,1)" }) }
      }
    })
  }, [expanded])

  // Update check after a short dwell, and the optional backdrop tint from the selected title.
  useEffect(() => {
    if (!entry) return
    const handle = window.setTimeout(() => onCheckUpdate(entry), 900)
    return () => window.clearTimeout(handle)
  }, [entry?.key])
  useEffect(() => {
    const stage = getStage()
    if (!stage) return
    if (!tintOn || !entry) { stage.look.setTint(null); return }
    let alive = true
    void coverTint({ cover: artFor(entry), title: entry.name, titleId: entry.titleId }).then(hex => { if (alive) stage.look.setTint(hex) })
    return () => { alive = false }
  }, [tintOn, entry?.key])
  useEffect(() => () => getStage()?.look.setTint(null), [])

  const restore = async (item: LibraryEntry) => {
    const { host, port } = receiverEndpoint(settings, target)
    setRestoring(true)
    try {
      const result = await restoreTitleIcon({ target, host, port, titleId: item.titleId, demo })
      props.onIconChanged(target, item.titleId, null, false)
      toast({ tone: "success", title: "Original cover restored", text: demo ? result.message : `${result.message} Your ${name} shows it after a restart.` })
      if (!demo) props.onRefresh(target)
    } catch (error) { toast({ tone: "error", title: "The cover wasn't restored", text: errorText(error) }) }
    finally { setRestoring(false) }
  }

  /* ---------------------------------------------------------------- keys */
  const view2 = entry ? updateView(entry, updates, pendingUpdates, eligible) : null
  const anchorOf = (item?: LibraryEntry) => item ? document.querySelector<HTMLElement>(`.lib-card[data-key="${CSS.escape(item.key)}"] [data-case-anchor]`) : null
  const keys = useRef({ library, index, expanded, entry, view: view2, editing, props, toggle, select, changeCover })
  keys.current = { library, index, expanded, entry, view: view2, editing, props, toggle, select, changeCover }
  useEffect(() => setPageKeys(event => {
    const { library, index, expanded, entry, view, editing, props, toggle, select, changeCover } = keys.current
    if (editing || isTyping()) return
    const k = event.key
    const cols = expanded ? layout.current.cols : 1
    const step = ({ ArrowLeft: -1, ArrowRight: 1, ArrowUp: expanded ? -cols : 0, ArrowDown: expanded ? cols : 0 } as Record<string, number>)[k]
    if (step !== undefined) {
      event.preventDefault()
      if (!library.length || !step) return
      const next = clamp(index + step, 0, library.length - 1)
      select(next)
      document.querySelector<HTMLElement>(`.lib-card[data-key="${CSS.escape(library[next].key)}"]`)?.focus({ preventScroll: true })
      return
    }
    if (k === "Enter" && entry && !(document.activeElement as HTMLElement | null)?.closest?.("button:not(.lib-card), input")) { event.preventDefault(); props.onOpenGame(entry, anchorOf(entry)); return }
    if ((k === "c" || k === "C") && entry) { event.preventDefault(); changeCover(entry); return }
    if (k === " " && entry && view?.candidate && !view.pending) { event.preventDefault(); props.onQueueUpdate(entry, view.candidate, view.packages || [view.candidate], anchorOf(entry)); return }
    if ((k === "i" || k === "I") && entry) { event.preventDefault(); props.onOpenGame(entry, anchorOf(entry)); return }
    if (k === "l" || k === "L") { event.preventDefault(); toggle(); return }
    if (k === "r" || k === "R") { event.preventDefault(); props.onRefresh(props.target) }
  }), [])

  /* ---------------------------------------------------------------- receiver notice */
  const sync = view.sync
  const syncing = sync?.state === "syncing"
  const subtitle = demo ? `${plural(library.length, "title")} in the offline preview`
    : syncing ? `Checking what's installed on your ${name}`
      : view.authoritative ? `${plural(library.length, "title")} on your ${name}, synced ${ageText(view.syncedAt)}`
        : library.length ? `${plural(library.length, "title")} installed through SSPI on your ${name}` : `Titles on your ${name}`
  const loadButton = (label = "Load receiver") => (
    <button type="button" className="btn sm" disabled={props.loadingReceiver !== null} onClick={() => props.onLoadReceiver(target)}>
      {props.loadingReceiver === target ? <span className="spinner" /> : <Icon name="upload" />}{props.loadingReceiver === target ? "Loading" : label}
    </button>
  )
  let notice: { tone: "" | "warn"; text: string; action?: ReactNode } | null = null
  if (host || demo) {
    if (probe?.receiver.state === "outdated") notice = { tone: "warn", text: `Your ${name} is running receiver ${probe.receiver.version || "(older)"}. Load ${probe.receiver.expectedVersion} to list everything installed and change covers.`, action: loadButton(`Load ${probe.receiver.expectedVersion}`) }
    else if (sync?.state === "unsupported") notice = { tone: "warn", text: sync.messages[0], action: loadButton() }
    else if (probe && probe.receiver.state !== "online" && probe.receiver.state !== "unconfigured") notice = { tone: "", text: `The ${name} receiver isn't answering${view.authoritative ? `, so this is the library from ${ageText(view.syncedAt)}` : ""}.`, action: loadButton() }
    else if (sync?.state === "partial") notice = { tone: "warn", text: `The last check didn't finish${view.authoritative ? "; this is the last complete library" : ""}. ${sync.messages[0] || ""}`.trim() }
    else if (sync?.state === "offline" && sync.messages[0]) notice = { tone: "", text: sync.messages[0] }
  }

  /* ---------------------------------------------------------------- render */
  const head = (
    <div className="lib-head">
      <div className="lib-title">
        <ConsoleSwitch value={target} onChange={props.onConsole} probes={probes} demo={demo} label="Library console" />
        <h2>Your library</h2>
        <span>{syncing && <span className="spinner" />}{subtitle}</span>
      </div>
      <div className="lib-head-actions">
        {(host || demo) && <button type="button" className="btn ghost sm" disabled={syncing} onClick={() => props.onRefresh(target)}>{syncing ? <span className="spinner" /> : <Icon name="refresh" />}Refresh</button>}
        {library.length > 0 && <button type="button" className="link lib-toggle" onClick={toggle}><span className="face triangle">L</span>{expanded ? "Collapse library" : "Expand library"}</button>}
      </div>
    </div>
  )

  if (!host && !demo) {
    return (
      <div className="page library-page enter">
        <section className="library" aria-label="Your library">
          {head}
          <div className="lib-empty">
            <Icon name="monitor" />
            <h3>Connect your {name}</h3>
            <p>Add your {name}'s address in Options, then load the SSPI receiver on it. Your library lists everything installed, and you can change each game's cover from here.</p>
            <div className="row">
              <button type="button" className="btn primary sm" onClick={() => props.onOptions("consoles")}><Icon name="monitor" />Console settings</button>
              <button type="button" className="btn ghost sm" onClick={props.onDemo}>Try the offline preview</button>
            </div>
          </div>
        </section>
      </div>
    )
  }

  return (
    <div className="page library-page enter">
      <section ref={libRef} className={`library ${expanded ? "is-expanded" : ""}`} aria-label="Your library">
        {head}
        {notice && <div className={`lib-sync ${notice.tone}`} role="status"><span>{notice.text}</span>{notice.action}</div>}
        {!library.length ? (
          <div className="lib-empty">
            <Icon name="library" />
            <h3>{syncing ? `Checking your ${name}` : view.authoritative ? `Nothing is installed on your ${name}` : "Nothing installed through SSPI yet"}</h3>
            <p>{syncing ? "This takes a few seconds for a large library." : view.authoritative ? "The receiver reports no installed games or apps." : `Titles you install through SSPI appear here. With ${probe?.receiver.expectedVersion ? `receiver ${probe.receiver.expectedVersion}` : "the latest receiver"} running, your library lists everything installed on your ${name}.`}</p>
            {!syncing && <div className="row"><button type="button" className="btn sm" onClick={props.onSearch}><Icon name="search" />Search titles</button></div>}
          </div>
        ) : (
          <>
            <div ref={stripRef} className="lib-strip" data-case-clip={expanded ? undefined : ""} data-case-scroll={expanded ? "" : undefined} onWheel={onWheel}>
              <div ref={trackRef} className="lib-track">
                {library.map((item, n) => {
                  const v = updateView(item, updates, pendingUpdates, eligible)
                  const installed = installedVersion(item)
                  return (
                    <button
                      key={item.key} type="button" data-key={item.key} data-hover-case="select" data-selected={n === index ? "" : undefined}
                      className={`lib-card ${n === index ? "is-selected" : ""}`}
                      aria-label={`${item.name}, installed ${installed || ""} on your ${name}${v.tone === "up" ? `, ${v.meta}` : ""}`}
                      onPointerEnter={event => {
                        nameIfCut(event.currentTarget)
                        window.clearTimeout(hoverTimer.current)
                        hoverTimer.current = window.setTimeout(() => select(n, "pointer"), 100)
                      }}
                      onPointerLeave={() => window.clearTimeout(hoverTimer.current)}
                      onFocus={event => select(n, event.currentTarget.matches(":focus-visible") ? "key" : "pointer")}
                      onClick={() => { select(n, "pointer"); props.onOpenGame(item, anchorOf(item)) }}
                      onContextMenu={event => { event.preventDefault(); window.clearTimeout(hoverTimer.current); select(n, "pointer"); changeCover(item) }}
                    >
                      <CaseAnchor spec={{ key: item.titleId.toUpperCase(), cover: artFor(item), title: item.name, titleId: item.titleId, kind: "library" }} delay={Math.min(n, 10) * 0.02}>
                        {v.available && <i className={`update-dot ${v.pending ? "" : "pulse"}`} />}
                        {v.pending && <i className="update-dot" />}
                      </CaseAnchor>
                      <span className="lib-name">{item.name}</span>
                      <span className="lib-line">
                        {v.tone === "up" ? <>{installed && !v.pending ? `${installed} · ` : ""}<em>{v.card}</em></> : installed ? `Installed ${installed}` : "Installed"}
                      </span>
                    </button>
                  )
                })}
              </div>
            </div>
            {entry && view2 && (
              <div className="lib-info">
                <p key={entry.key} className="lib-meta swap">
                  <span>{entry.titleId}</span>
                  {entry.requiredFirmware && <span>Needs firmware {entry.requiredFirmware}</span>}
                  {elsewhere.has(entry.titleId.toUpperCase()) && <span>Also on your {other.toUpperCase()}</span>}
                  {entry.customIcon && <span>Custom cover</span>}
                  {view2.meta && view2.tone !== "up" && <span>{view2.meta}</span>}
                </p>
                <div className="lib-actions">
                  {view2.candidate && !view2.pending && <button type="button" className="btn sm" disabled={props.deliveryBusy} title="Queue update (Space)" onClick={event => onQueueUpdate(entry, view2.candidate!, view2.packages || [view2.candidate!], event.currentTarget)}><Icon name="download" />Queue update {view2.available}</button>}
                  {canWrite && <button type="button" className="btn sm" title="Change cover (right-click a case, or C)" onClick={() => setEditing(entry)}><Icon name="image" />Change cover</button>}
                  {entry.customIcon && canWrite && <button type="button" className="btn ghost sm" disabled={restoring} onClick={() => void restore(entry)}>{restoring ? <span className="spinner" /> : <Icon name="undo" />}Restore</button>}
                  <button type="button" className="btn ghost sm" title="Game page (click a case, or Enter)" onClick={() => props.onOpenGame(entry, anchorOf(entry))}>Game page</button>
                </div>
              </div>
            )}
          </>
        )}
      </section>
      <CoverEditor
        open={!!editing} entry={editing} neighbours={library.slice(Math.max(0, index - 1), index + 5)} target={target} settings={settings} probe={probe} demo={demo}
        onClose={() => setEditing(null)} onChanged={(titleId, icon, custom) => props.onIconChanged(target, titleId, icon, custom)}
      />
    </div>
  )
}
