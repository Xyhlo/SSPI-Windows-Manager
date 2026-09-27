/* =====================================================================
   Library — the SSPI PS4 "Your library" as its own section: a row of
   cases (L expands it to a grid) with installed and update versions,
   and the selected title underneath. The PS5 | PS4 switch picks the
   console. Clicking a case changes its cover on the console.
   ===================================================================== */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react"
import type { LibraryView, UpdateInfo } from "@/App"
import { restoreTitleIcon } from "@/lib/console-api"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe } from "@/lib/console-types"
import { compareVersions, errorText, plural } from "@/lib/format"
import { portraitFromIcon } from "@/lib/image"
import { isTyping, setPageKeys } from "@/lib/keys"
import { installedVersion, type LibraryEntry } from "@/lib/library"
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
import type { Hint } from "./Shell"
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
  onDock: (dock: { context: ReactNode; hints: Hint[] }) => void
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
  const { target, libraries, probes, settings, demo, expanded, setExpanded, updates, pendingUpdates, onCheckUpdate, onQueueUpdate, onDock, tintOn } = props
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
  const select = (n: number) => { const next = library[n]; if (next) props.onSelect(next.key) }

  /* ---------------------------------------------------------------- strip layout (from the PS4 library) */
  const libRef = useRef<HTMLElement>(null)
  const stripRef = useRef<HTMLDivElement>(null)
  const trackRef = useRef<HTMLDivElement>(null)
  const layout = useRef({ cols: 7, caseW: 150, x: new Spring(0, 170, 24) })
  const live = useRef({ index, expanded, count: library.length })
  live.current = { index, expanded, count: library.length }

  const place = (instant = false) => {
    const strip = stripRef.current, lib = libRef.current
    if (!strip || !lib) return
    const { index, expanded, count } = live.current
    const gap = expanded ? 22 : 28
    const width = strip.clientWidth - 24
    const caseW = expanded
      ? clamp((width + gap) / Math.max(2, Math.round((width + gap) / (148 + gap))) - gap, 112, 168)
      : clamp((strip.clientHeight - 24 - 70) / CASE_ASPECT, 104, 208)
    const cols = clamp(Math.floor((width + gap) / (caseW + gap)), 2, 14)
    layout.current.cols = cols
    layout.current.caseW = caseW
    lib.style.setProperty("--case-w", `${caseW.toFixed(1)}px`)
    lib.style.setProperty("--lib-cols", String(cols))
    lib.style.setProperty("--lib-gap", `${gap}px`)
    if (expanded) { layout.current.x.snap(0); return }
    const first = Math.max(0, Math.min(index - (cols - 2), count - cols))
    const targetX = -first * (caseW + gap)
    layout.current.x.set(targetX)
    if (instant || !motionOK()) layout.current.x.snap(targetX)
  }
  useLayoutEffect(() => { place(true) }, [expanded, library.length])
  useEffect(() => { place() }, [index])
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
  }, [expanded])

  // The expanded grid scrolls; keep the selected case in view there.
  useEffect(() => {
    if (!expanded || !entry) return
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

  /* ---------------------------------------------------------------- keys and dock */
  const view2 = entry ? updateView(entry, updates, pendingUpdates, eligible) : null
  const anchorOf = (item?: LibraryEntry) => item ? document.querySelector<HTMLElement>(`.lib-card[data-key="${CSS.escape(item.key)}"] [data-case-anchor]`) : null
  const keys = useRef({ library, index, expanded, entry, view: view2, editing, canWrite, props, toggle, select })
  keys.current = { library, index, expanded, entry, view: view2, editing, canWrite, props, toggle, select }
  useEffect(() => setPageKeys(event => {
    const { library, index, expanded, entry, view, editing, canWrite, props, toggle, select } = keys.current
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
    if (k === "Enter" && entry && canWrite && !(document.activeElement as HTMLElement | null)?.closest?.("button:not(.lib-card), input")) { event.preventDefault(); setEditing(entry); return }
    if (k === " " && entry && view?.candidate && !view.pending) { event.preventDefault(); props.onQueueUpdate(entry, view.candidate, view.packages || [view.candidate], anchorOf(entry)); return }
    if ((k === "i" || k === "I") && entry) { event.preventDefault(); props.onOpenGame(entry, anchorOf(entry)); return }
    if (k === "l" || k === "L") { event.preventDefault(); toggle(); return }
    if (k === "r" || k === "R") { event.preventDefault(); props.onRefresh(props.target) }
  }), [])

  useEffect(() => {
    const hints: Hint[] = [
      { key: "Enter", label: "Change cover", disabled: !entry || !canWrite, run: () => entry && setEditing(entry) },
      { key: "Space", label: view2?.pending ? "Update queued" : view2?.available ? `Queue update ${view2.available}` : "Queue update", disabled: !view2?.candidate || !!view2.pending || props.deliveryBusy, run: () => { if (entry && view2?.candidate) onQueueUpdate(entry, view2.candidate, view2.packages || [view2.candidate], anchorOf(entry)) } },
      { key: "I", glyph: "I", face: "triangle", label: "Game page", disabled: !entry, run: () => entry && props.onOpenGame(entry, anchorOf(entry)) },
      { key: "L", glyph: "L", face: "neutral", label: expanded ? "Collapse library" : "Expand library", disabled: !library.length, run: toggle },
    ]
    onDock({ context: entry ? <>Library<small>{entry.name}</small></> : "Library", hints })
  }, [entry?.key, view2?.card, view2?.pending, props.deliveryBusy, canWrite, expanded, library.length, target])

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
            <div ref={stripRef} className="lib-strip" data-case-clip={expanded ? undefined : ""} data-case-scroll={expanded ? "" : undefined}>
              <div ref={trackRef} className="lib-track">
                {library.map((item, n) => {
                  const v = updateView(item, updates, pendingUpdates, eligible)
                  return (
                    <button
                      key={item.key} type="button" data-key={item.key} data-hover-case
                      className={`lib-card ${n === index ? "is-selected" : ""}`}
                      aria-label={`${item.name}, installed ${installedVersion(item) || ""} on your ${name}${canWrite ? ". Change cover" : ""}`}
                      onPointerEnter={() => select(n)}
                      onFocus={() => select(n)}
                      onClick={() => { select(n); if (canWrite) setEditing(item); else props.onOpenGame(item, anchorOf(item)) }}
                    >
                      <CaseAnchor spec={{ key: item.titleId.toUpperCase(), cover: artFor(item), title: item.name, titleId: item.titleId, kind: "library" }} delay={Math.min(n, 14) * 0.035}>
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
            {entry && view2 && (
              <div className="lib-info">
                <div key={entry.key} className="lib-info-main swap">
                  <strong>{entry.name}</strong>
                  <div className="lib-meta">
                    <span>{entry.titleId}</span>
                    <span>{installedVersion(entry) ? `Installed ${installedVersion(entry)} on your ${name}` : `Installed on your ${name}`}</span>
                    {entry.requiredFirmware && <span>Needs firmware {entry.requiredFirmware}</span>}
                    {elsewhere.has(entry.titleId.toUpperCase()) && <span>Also on your {other.toUpperCase()}</span>}
                    {entry.customIcon && <span>Custom cover</span>}
                    {view2.meta && <span className={view2.tone === "up" ? "up" : ""}>{view2.meta}</span>}
                  </div>
                </div>
                <div className="lib-actions">
                  {view2.candidate && !view2.pending && <button type="button" className="btn sm" disabled={props.deliveryBusy} onClick={event => onQueueUpdate(entry, view2.candidate!, view2.packages || [view2.candidate!], event.currentTarget)}><Icon name="download" />Queue update</button>}
                  {canWrite && <button type="button" className="btn sm" onClick={() => setEditing(entry)}><Icon name="image" />Change cover</button>}
                  {entry.customIcon && canWrite && <button type="button" className="btn ghost sm" disabled={restoring} onClick={() => void restore(entry)}>{restoring ? <span className="spinner" /> : <Icon name="undo" />}Restore</button>}
                  <button type="button" className="btn ghost sm" onClick={() => props.onOpenGame(entry, anchorOf(entry))}>Game page</button>
                </div>
                <span className="lib-pos">{index + 1} / {library.length}</span>
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
