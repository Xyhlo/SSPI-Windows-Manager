/* =====================================================================
   Game details — the SSPI PS4 details layout: case, install actions and
   the selected-package panel on the left; package tabs, region, provider
   and the package list on the right, with the selection tray underneath.
   The page fits the window; the package list is the part that scrolls.
   ===================================================================== */
import { invoke } from "@tauri-apps/api/core"
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { useProviders } from "./ProviderContext"
import { CaseAnchor } from "./CaseAnchor"
import { Collapse } from "./Collapse"
import { Icon } from "./Icon"
import type { OptionsTab } from "./OptionsOverlay"
import { toast } from "./toasts"
import { consoleAddress, packageTitle, sendBlockReason, platformOf } from "@/lib/consoles"
import { volumeOf } from "@/lib/downloads"
import { compareVersions, errorText, fmtBytes, plural } from "@/lib/format"
import { setPageKeys } from "@/lib/keys"
import { installedVersion, type LibraryEntry } from "@/lib/library"
import { clamp, glide, motionOK } from "@/lib/motion"
import { archiveLabel, groupHosts, groupParts, isSevenZip, partsSize, structurePackages, type PackageGroup } from "@/lib/packages"
import { archivePartsFor, packageKind, packageProblem, packageReleaseKey, packageVersion, planPackages } from "@/lib/package-selection"
import { compatibleProviders, directCandidate, providerName, variantKey } from "@/lib/providers"
import { displayText } from "@/lib/display"
import { coverTint } from "@/stage/art"
import { getStage } from "@/stage/stage"
import type { DeliveryRequest, Game, LoadState, MetadataResponse, PackageCandidate, Settings, SpacePlan } from "@/types"

type TabId = "all" | "base" | "update" | "dlc" | "backport" | "about"
const TABS: Array<[TabId, string]> = [["all", "All"], ["base", "Base"], ["update", "Updates"], ["dlc", "DLC"], ["backport", "Backport"], ["about", "About"]]

type Props = {
  game: Game
  packages: PackageCandidate[]
  packageState: LoadState
  packageError: string
  packageNote: string | null
  metadata: MetadataResponse | null
  metadataState: LoadState
  demo: boolean
  settings: Settings
  autoBackports: boolean
  setAutoBackports: (value: boolean) => void
  deliveryBusy: boolean
  libraryEntry?: LibraryEntry
  pendingUpdate?: string
  backLabel: string
  onBack: () => void
  onRetry: () => void
  onVariant: (game: Game) => void
  onInstall: (candidates: PackageCandidate[], available: PackageCandidate[], from: HTMLElement | null, provider?: string) => Promise<string[]>
  onOptions: (tab: OptionsTab) => void
  tintOn: boolean
}

type RowState = {
  group: PackageGroup
  parts: PackageCandidate[]
  candidate?: PackageCandidate
  problem: string
  tone: "good" | "fail"
  status: string
  size: number | null
  auto: boolean
  checked: boolean
}

export function DetailsPage(props: Props) {
  const { game, packages, packageState, packageError, packageNote, metadata, metadataState, demo, settings, autoBackports, setAutoBackports, deliveryBusy, libraryEntry, pendingUpdate, backLabel, onBack, onRetry, onVariant, onInstall, onOptions, tintOn } = props
  const { inventories, loading: providersLoading, refresh: refreshProviders } = useProviders()
  const [tab, setTab] = useState<TabId>("all")
  const [checked, setChecked] = useState<string[]>([])
  const [focusKey, setFocusKey] = useState<string | null>(null)
  const [hoverKey, setHoverKey] = useState<string | null>(null)
  const [mirrorOpen, setMirrorOpen] = useState<string | null>(null)
  const [hostByKey, setHostByKey] = useState<Record<string, string>>({})
  const [allVersions, setAllVersions] = useState(false)
  const tabsRef = useRef<HTMLDivElement>(null)
  const inkRef = useRef<HTMLSpanElement>(null)
  const listRef = useRef<HTMLDivElement>(null)

  const titleId = game.titleId
  const target = settings.activeConsole
  const packageDumps = packageTitle(settings, titleId)
  const packageOnly = packageDumps && settings.downloadPackageOnly
  const includeBackports = autoBackports && target === "ps5"
  const installBlock = packageOnly ? undefined : sendBlockReason(target, { titleId, homebrew: game.homebrew })
  const raw = metadata?.game

  /* ---------------------------------------------------------------- package rows */
  const visible = useMemo(() => packages.filter(candidate =>
    (!game.sourceId || candidate.sourceId === game.sourceId) &&
    (demo || directCandidate(candidate) || compatibleProviders(candidate, inventories).length > 0)), [packages, game.sourceId, demo, inventories])
  const groups = useMemo(() => structurePackages(visible), [visible])
  const selectedMirrors = groups.flatMap(group => groupParts(group, hostByKey[group.key]))
  const selectedCandidates = groups.filter(group => checked.includes(group.key)).map(group => groupParts(group, hostByKey[group.key])[0]).filter(Boolean)
  const selection = planPackages(selectedCandidates, selectedMirrors, titleId, { packageDumps, autoBackports: includeBackports, targetFw: settings.targetFw, catalog: packages })
  const included = new Set(selection.added.map(packageReleaseKey))
  const selectionBlock = packageOnly ? undefined : selectedCandidates.map(candidate => sendBlockReason(target, { titleId, backport: packageKind(candidate) === "backport", homebrew: candidate.homebrew })).find(Boolean)

  const rowState = (group: PackageGroup): RowState => {
    const parts = groupParts(group, hostByKey[group.key])
    const candidate = parts[0]
    const pairing = packageDumps && group.kind === "backport"
    const block = packageOnly ? undefined : sendBlockReason(target, { titleId, backport: group.kind === "backport", homebrew: candidate?.homebrew })
    let problem = candidate ? packageProblem(candidate, packages, pairing) : "No package is available."
    if (!problem && isSevenZip(parts)) problem = "7z extraction is unavailable in this build. Choose a RAR or ZIP mirror."
    if (!problem && block) problem = block
    const providers = candidate ? compatibleProviders(candidate, inventories) : []
    const status = problem || (candidate?.homebrew?.format === "payload" ? "Adds to Payloads" : demo ? "Preview package" : candidate && directCandidate(candidate) ? "Direct link" : providers.length ? `${providers.map(providerName).join(" or ")} supported` : "Host support unknown")
    const auto = !!candidate && included.has(packageReleaseKey(candidate))
    return { group, parts, candidate, problem, tone: problem ? "fail" : "good", status, size: partsSize(parts), auto, checked: checked.includes(group.key) || auto }
  }

  const latestUpdate = groups.filter(group => group.kind === "update").sort((a, b) => compareVersions(b.version, a.version))[0]
  const counts: Record<TabId, number> = { all: groups.length, base: 0, update: 0, dlc: 0, backport: 0, about: 0 }
  for (const group of groups) if (group.kind in counts) counts[group.kind as TabId] += 1
  const listed = groups.filter(group => {
    if (tab !== "all" && group.kind !== tab) return false
    if (!allVersions && group.kind === "update" && group !== latestUpdate && !checked.includes(group.key)) return false
    return true
  })

  const baseCandidates = visible.filter(candidate => packageKind(candidate) === "base").sort((a, b) => (a.archivePartNumber || 1) - (b.archivePartNumber || 1))
  const basePackage = baseCandidates[0]
  const heroPlan = basePackage ? planPackages([basePackage], visible, titleId, { packageDumps, autoBackports: includeBackports, targetFw: settings.targetFw, catalog: packages }) : null
  const heroBackport = heroPlan?.items[0]?.backport
  const heroParts = basePackage ? (basePackage.archiveSetId ? archivePartsFor(basePackage, visible) : [basePackage]) : []
  const heroBytes = [...heroParts, ...(heroBackport ? (heroBackport.archiveSetId ? archivePartsFor(heroBackport, visible) : [heroBackport]) : [])].reduce((sum, part) => sum + (part.expectedSize || 0), 0)
  const baseGroup = groups.find(group => group.kind === "base")
  const focusGroup = groups.find(group => group.key === (hoverKey || focusKey)) || baseGroup || groups[0]
  const focusRow = focusGroup ? rowState(focusGroup) : null

  const firmware = basePackage?.firmware || raw?.firmware || ""
  const platform = platformOf(titleId) === "ps4" ? "PS4" : "PS5"
  const variants = game.variants || [game]
  const variantIndex = Math.max(0, variants.findIndex(item => variantKey(item) === variantKey(game)))
  const enabledProviders = [
    settings.realDebridEnabled && settings.realDebridConfigured && "Real-Debrid",
    settings.torboxEnabled && settings.torboxConfigured && "TorBox",
    settings.alldebridEnabled && settings.alldebridConfigured && "AllDebrid",
  ].filter(Boolean) as string[]
  const installed = libraryEntry ? installedVersion(libraryEntry) : ""
  const newer = latestUpdate && installed && compareVersions(latestUpdate.version, installed) > 0 ? latestUpdate.version : ""
  /** How a listed base game or update compares with what the console has installed. The receiver doesn't
      report add-ons, so DLC rows carry no mark. */
  const installMark = (group: PackageGroup): { label: string; tone: "good" | "up" | "old" } | null => {
    if (!libraryEntry || (group.kind !== "base" && group.kind !== "update")) return null
    if (!group.version || !installed) return group.kind === "base" ? { label: "Installed", tone: "good" } : null
    const order = compareVersions(group.version, installed)
    if (order === 0) return { label: "Installed", tone: "good" }
    return order > 0 ? { label: "Newer than installed", tone: "up" } : { label: "Older than installed", tone: "old" }
  }

  /* ---------------------------------------------------------------- effects */
  useLayoutEffect(() => {
    const tabs = tabsRef.current
    glide(inkRef.current, tabs?.querySelector<HTMLElement>(`.ptab[data-tab="${tab}"]`) || null, tabs, { inset: 24, liquid: true })
  }, [tab, groups.length])
  useEffect(() => {
    const onResize = () => { const tabs = tabsRef.current; glide(inkRef.current, tabs?.querySelector<HTMLElement>(`.ptab[data-tab="${tab}"]`) || null, tabs, { inset: 24, instant: true }) }
    window.addEventListener("resize", onResize)
    return () => window.removeEventListener("resize", onResize)
  }, [tab])
  useEffect(() => {
    const stage = getStage()
    if (!stage) return
    if (!tintOn) { stage.look.setTint(null); return }
    let live = true
    void coverTint({ cover: game.icon, title: game.name, titleId }).then(hex => { if (live) stage.look.setTint(hex) })
    return () => { live = false; stage.look.setTint(null) }
  }, [tintOn, game.icon, game.name, titleId])

  /* ---------------------------------------------------------------- actions */
  const caseEl = () => document.querySelector<HTMLElement>(".d-case")
  const toggleCheck = (group: PackageGroup) => {
    const state = rowState(group)
    if (state.tone === "fail") { toast({ tone: "warning", title: `${group.title} can't be selected`, text: state.problem }); return }
    if (state.auto) return
    setChecked(current => current.includes(group.key) ? current.filter(key => key !== group.key) : [...current, group.key])
    if (!checked.includes(group.key) && motionOK()) {
      listRef.current?.querySelector<HTMLElement>(`.prow[data-key="${CSS.escape(group.key)}"]`)?.animate([{ backgroundColor: "rgba(255,255,255,.14)" }, { backgroundColor: "rgba(255,255,255,.055)" }], { duration: 500, easing: "ease-out" })
    }
  }
  const selectRecommended = () => {
    const picks = [baseGroup, latestUpdate].filter((group): group is PackageGroup => !!group && rowState(group).tone === "good")
    if (!picks.length) return
    setChecked(picks.map(group => group.key))
    setTab("all")
    toast({ tone: "info", title: "Recommended packages selected", text: `${picks.map(group => `${group.title}${group.version ? ` ${group.version}` : ""}`).join(" and ")}. The newest listed update is chosen for you.`, duration: 3600 })
  }
  const installSelected = async () => {
    if (!selectedCandidates.length) return
    const succeeded = await onInstall(selectedCandidates, selectedMirrors, caseEl())
    setChecked(current => current.filter(key => {
      const group = groups.find(item => item.key === key)
      return group && !succeeded.includes(packageReleaseKey(groupParts(group, hostByKey[group.key])[0]))
    }))
  }
  const installBase = () => { if (basePackage) void onInstall([basePackage], visible, caseEl()) }
  const moveVariant = (direction: number) => {
    if (variants.length < 2) return
    const next = variants[(variantIndex + direction + variants.length) % variants.length]
    onVariant({ ...next, variants })
  }

  /* ---------------------------------------------------------------- keyboard */
  const live = useRef({ listed, checked, focusKey, selectedCandidates, tab })
  live.current = { listed, checked, focusKey, selectedCandidates, tab }
  const handlers = useRef({ toggleCheck, selectRecommended, installSelected, installBase, onBack, setTab, setMirrorOpen, setFocusKey })
  handlers.current = { toggleCheck, selectRecommended, installSelected, installBase, onBack, setTab, setMirrorOpen, setFocusKey }
  useEffect(() => setPageKeys(event => {
    const { listed, focusKey, selectedCandidates, tab } = live.current
    const h = handlers.current
    const k = event.key
    const rows = [...(listRef.current?.querySelectorAll<HTMLElement>(".prow") || [])]
    const current = (document.activeElement as HTMLElement | null)?.closest?.(".prow") as HTMLElement | null
    const i = current ? rows.indexOf(current) : -1
    if (k === "ArrowDown" || k === "ArrowUp") { event.preventDefault(); (rows[clamp(i + (k === "ArrowDown" ? 1 : -1), 0, rows.length - 1)] || rows[0])?.focus(); return }
    if ((k === "ArrowLeft" || k === "ArrowRight") && (current || document.activeElement?.closest(".ptabs"))) {
      event.preventDefault()
      const enabled = TABS.map(([id]) => id).filter(id => id === "all" || id === "about" || counts[id] > 0)
      const j = enabled.indexOf(tab)
      h.setTab(enabled[clamp(j + (k === "ArrowRight" ? 1 : -1), 0, enabled.length - 1)])
      return
    }
    const focused = listed.find(group => group.key === (current?.dataset.key || focusKey))
    if (k === " " && focused && current) { event.preventDefault(); h.toggleCheck(focused); return }
    if ((k === "m" || k === "M") && focused) { event.preventDefault(); h.setMirrorOpen(open => open === focused.key ? null : focused.key); return }
    if (k === "r" || k === "R") { event.preventDefault(); h.selectRecommended(); return }
    if (k === "Enter" && !(document.activeElement as HTMLElement | null)?.closest?.("button, input, select")) { event.preventDefault(); if (selectedCandidates.length) void h.installSelected(); else h.installBase(); return }
    if (k === "Escape") { event.preventDefault(); h.onBack() }
  }), [counts.base, counts.update, counts.dlc, counts.backport])

  const total = selectedCandidates.length + selection.added.length

  /* ---------------------------------------------------------------- render */
  const trayOpen = total > 0
  const traySize = [...selectedCandidates, ...selection.added].reduce((sum, candidate) => sum + (candidate.archiveSetId ? archivePartsFor(candidate, selectedMirrors) : [candidate]).reduce((t, part) => t + (part.expectedSize || 0), 0), 0)
  const trayProblem = selection.problems[0] || selectionBlock || installBlock
  return (
    <article className="page details-page enter" aria-label={`${game.name} details`}>
      <nav className="crumbs"><button type="button" className="link" onClick={onBack}><Icon name="left" />{backLabel}</button></nav>
      <header className="d-head">
        <div className="d-title">
          <h1>{game.name}</h1>
          <div className="d-specs">
            <strong>{titleId}</strong>
            {game.region && <span>{game.region}</span>}
            <span>{platform}{firmware ? `, FW ${firmware}` : ""}</span>
            {game.sourceName && <span>{displayText(game.sourceName)}{game.sourceVersion ? ` ${game.sourceVersion}` : ""}</span>}
            {demo && <span>Offline preview</span>}
          </div>
        </div>
        {libraryEntry && (
          <div className="d-status lib">
            In your library, {installed || "installed"} on {libraryEntry.target.toUpperCase()}
            {pendingUpdate !== undefined ? <span className="up">{pendingUpdate ? `Update ${pendingUpdate} is on its way` : "An update is on its way"}</span> : newer ? <span className="up">Update {newer} available</span> : null}
          </div>
        )}
      </header>
      <div className="d-body">
        <aside className="d-left" data-case-scroll="">
          <CaseAnchor spec={{ key: titleId.toUpperCase(), cover: game.icon, title: game.name, titleId, kind: "details" }} className="d-case" role="img" label={`${game.name} case`} hoverable />
          <div className="d-actions">
            <button type="button" className="btn primary" disabled={!basePackage || deliveryBusy || !!installBlock} title={installBlock || undefined} onClick={installBase}>
              {deliveryBusy ? <span className="spinner" /> : <Icon name="download" />}{heroBackport ? "Install base + backport" : demo ? "Preview base game" : packageOnly ? "Package base game" : "Install base game"}{heroBytes ? <span className="muted-size">{fmtBytes(heroBytes)}</span> : null}
            </button>
            <button type="button" className="btn" disabled={!counts.update} onClick={() => setTab("update")}><Icon name="layers" />View updates</button>
            <button type="button" className="btn ghost" disabled={!baseGroup} title="Selects the base game and the newest update (R)" onClick={selectRecommended}><Icon name="check" />Select recommended</button>
            <p className={`d-target ${installBlock ? "block" : ""}`}>{installBlock || (packageOnly ? "Packages are saved on this PC" : demo ? "Installs are off in the offline preview" : consoleAddress(settings, target) ? `Installs on your ${target.toUpperCase()} at ${consoleAddress(settings, target)}` : `Installs on your ${target.toUpperCase()}`)}</p>
          </div>
          {focusRow && <SelectedPanel row={focusRow} hosts={groupHosts(focusRow.group).length} />}
        </aside>
        <section className="d-right" aria-label="Packages">
          <div className="ptabs" role="tablist" ref={tabsRef}>
            {TABS.map(([id, label]) => (
              <button key={id} type="button" className="ptab" role="tab" data-tab={id} aria-selected={tab === id} disabled={id !== "all" && id !== "about" && !counts[id]} onClick={() => setTab(id)}>
                {label}{id !== "about" && <b>{counts[id]}</b>}
              </button>
            ))}
            <span className="ptab-ink" ref={inkRef} />
          </div>
          {tab === "about" ? (
            <About game={game} raw={raw} metadataState={metadataState} attribution={metadata?.attribution} basePackage={basePackage} backport={heroBackport} firmware={firmware} hasBackport={counts.backport > 0} installed={libraryEntry ? `${installed || "Installed"} on ${libraryEntry.target.toUpperCase()}` : ""} demo={demo} settings={settings} visible={visible} />
          ) : (
            <>
              <div className="d-subbar">
                <div className="region-nav">
                  <button type="button" className="btn sm icon" aria-label="Previous region or source" disabled={variants.length < 2 || deliveryBusy} onClick={() => moveVariant(-1)}><Icon name="chevL" /></button>
                  <span>{game.region ? `Region ${game.region}` : "Region"}&ensp;<span style={{ color: "var(--faint)" }}>{variantIndex + 1} of {variants.length}</span></span>
                  <button type="button" className="btn sm icon" aria-label="Next region or source" disabled={variants.length < 2 || deliveryBusy} onClick={() => moveVariant(1)}><Icon name="chevR" /></button>
                </div>
                <div className="provider-pick"><Icon name="shield" />{demo ? "Offline preview" : enabledProviders.length ? enabledProviders.join(" and ") : "No provider enabled"}</div>
                <div className="versions-toggle"><span>All update versions</span><button type="button" className="switch" role="switch" aria-checked={allVersions} aria-label="Show all update versions" onClick={() => setAllVersions(value => !value)} /></div>
              </div>
              {packageNote && <p className="plist-note">{packageNote}</p>}
              <div className="plist scroll" ref={listRef} role="listbox" aria-multiselectable="true" aria-label="Available packages">
                {packageState === "loading" && Array.from({ length: 5 }, (_, n) => <div key={n} className="prow-skel skeleton" />)}
                {packageState === "error" && (
                  <div className="resolve-error" role="alert">
                    <Icon name="alert" />
                    <div><strong>Packages couldn't be listed</strong><p>{packageError}</p></div>
                    <div className="row"><button type="button" className="btn sm" onClick={onRetry}><Icon name="retry" />Retry</button><button type="button" className="btn ghost sm" onClick={() => onOptions("sources")}>Sources</button></div>
                  </div>
                )}
                {packageState === "success" && packages.length === 0 && <p className="plist-empty">Your package sources answered, but they don't list any packages for this title.</p>}
                {packageState === "success" && packages.length > 0 && !groups.length && !providersLoading && (
                  <p className="plist-empty">None of the listed mirrors work with your connected download services. <button type="button" className="link" onClick={() => onOptions("debrid")}>Check Debrid in Options</button> or <button type="button" className="link" onClick={refreshProviders}>try again</button>.</p>
                )}
                {packageState === "success" && packages.length > 0 && !groups.length && providersLoading && <p className="plist-empty">Checking your connected download services…</p>}
                {packageState === "success" && groups.length > 0 && !listed.length && <p className="plist-empty">No packages of this type are listed for this region.</p>}
                {packageState === "success" && listed.map((group, n) => {
                  const row = rowState(group)
                  const mark = installMark(group)
                  const hosts = groupHosts(group)
                  const picked = hostByKey[group.key] && hosts.includes(hostByKey[group.key]) ? hostByKey[group.key] : hosts[0]
                  return (
                    <div key={group.key} style={{ display: "contents" }}>
                      <div
                        className={`prow ${row.checked ? "is-checked" : ""} ${focusKey === group.key ? "is-focus" : ""} enter`}
                        style={{ animationDelay: `${Math.min(n, 10) * 28}ms` }}
                        role="option" tabIndex={0} data-key={group.key} aria-selected={row.checked}
                        onPointerEnter={() => setHoverKey(group.key)} onPointerLeave={() => setHoverKey(null)}
                        onFocus={() => setFocusKey(group.key)}
                        onClick={() => setFocusKey(group.key)}
                      >
                        <span className={`prow-bar ${row.tone}`} />
                        <button type="button" className={`check ${row.auto ? "auto" : ""}`} role="checkbox" aria-checked={row.checked && !row.auto} aria-label={`Select ${group.title} ${group.version}`} disabled={deliveryBusy} onClick={event => { event.stopPropagation(); toggleCheck(group) }}><Icon name="check" /></button>
                        <span className="prow-main">
                          <span className="prow-title">{group.kind === "dlc" && row.candidate?.label ? displayText(row.candidate.label) : group.title}{group.version && <span className="ver">{group.kind === "backport" ? group.version : `v${group.version}`}</span>}{mark && <span className={`inst ${mark.tone}`}>{mark.tone === "good" && <Icon name="check" />}{mark.label}</span>}</span>
                          <span className="prow-sub">
                            <span className={row.tone === "fail" ? "fail" : ""}>{row.status}</span>
                            <span>{archiveLabel(row.parts)}</span>
                            {row.auto && <span>Included automatically</span>}
                          </span>
                        </span>
                        <span className="prow-size">{row.size ? fmtBytes(row.size) : "Size unknown"}</span>
                        <span className="prow-fw">{group.firmware ? `FW ${group.firmware}` : ""}</span>
                        <button type="button" className="prow-mirrors" aria-expanded={mirrorOpen === group.key} aria-label={`Mirrors for ${group.title}`} onClick={event => { event.stopPropagation(); setFocusKey(group.key); setMirrorOpen(open => open === group.key ? null : group.key) }}>
                          <span className="face triangle">M</span>{plural(hosts.length, "mirror")}<Icon name="chevD" />
                        </button>
                      </div>
                      <Collapse open={mirrorOpen === group.key} className="mirrors">
                        <div className="mirrors-inner">
                          {hosts.map(host => {
                            const parts = groupParts(group, host)
                            const first = parts[0]
                            const providers = first ? compatibleProviders(first, inventories) : []
                            const direct = !!first && directCandidate(first)
                            const size = partsSize(parts)
                            return (
                              <button key={host} type="button" className="mirror" aria-pressed={host === picked} disabled={deliveryBusy} onClick={() => setHostByKey(current => ({ ...current, [group.key]: host }))}>
                                <span className="radio" />
                                <span>{displayText(host)} <small>{plural(parts.length, "part")}{size ? `, ${fmtBytes(size)}` : ""}</small></span>
                                <span className="prov">{direct ? <span className="ok">Direct</span> : providers.length ? providers.map(id => <span key={id} className="ok">{providerName(id)}</span>) : <span>Unknown host</span>}</span>
                                <small>{direct || providers.length || demo ? "Ready" : "Enable a matching provider"}</small>
                              </button>
                            )
                          })}
                        </div>
                      </Collapse>
                    </div>
                  )
                })}
              </div>
              {focusRow && packageState === "success" && <div className="plist-foot">{providerFoot(focusRow, demo)}</div>}
              {trayOpen && (
                <div className="tray" role="region" aria-label="Selected packages">
                  <div className="tray-count"><b key={total}>{total}</b> {total === 1 ? "package" : "packages"} selected</div>
                  <div className={`tray-meta ${trayProblem ? "error" : ""}`}>
                    {trayProblem || `${traySize ? `${fmtBytes(traySize)} in ` : ""}${plural(selection.items.length, "transfer")}${selection.added.length ? `. Added automatically: ${selection.added.map(candidate => candidate.label).join(", ")}` : ""}`}
                  </div>
                  {settings.packageDumps && target === "ps5" && titleId.startsWith("PPSA") && (
                    <label><button type="button" className="switch" role="switch" aria-checked={autoBackports} aria-label="Include matching backport" disabled={deliveryBusy} onClick={() => setAutoBackports(!autoBackports)} />Matching backport</label>
                  )}
                  <button type="button" className="btn ghost sm" disabled={deliveryBusy} onClick={() => setChecked([])}>Clear</button>
                  <button type="button" className="btn primary" disabled={deliveryBusy || !!trayProblem || !selectedCandidates.length} onClick={() => void installSelected()}>
                    {deliveryBusy ? <span className="spinner" /> : <Icon name="download" />}{demo ? "Preview selected" : packageOnly ? "Download and pack selected" : "Install selected"}
                  </button>
                </div>
              )}
            </>
          )}
        </section>
      </div>
    </article>
  )
}

function providerFoot(row: RowState, demo: boolean): ReactNode {
  if (row.tone === "fail") return <><span className="dot fail" /><span>{row.problem}</span></>
  if (demo) return <><span className="dot good" /><span><strong>Offline preview.</strong> Packages are simulated and nothing is downloaded.</span></>
  if (row.status === "Direct link") return <><span className="dot good" /><span><strong>Direct link.</strong> No provider needed; the file downloads straight from the source.</span></>
  if (row.status.endsWith("supported")) return <><span className="dot good" /><span><strong>{row.status}.</strong> Host support is confirmed; account and file limits still apply.</span></>
  return <><span className="dot" /><span>Host support is unknown for this mirror.</span></>
}

function SelectedPanel({ row, hosts }: { row: RowState; hosts: number }) {
  const { group, candidate } = row
  const [detailsOpen, setDetailsOpen] = useState(false)
  useEffect(() => setDetailsOpen(false), [group.key])
  const stats: Array<[string, string]> = [
    ["Version", group.version || "Not listed"],
    ["Size", row.size ? fmtBytes(row.size) : "Not listed"],
    ["Firmware", group.firmware || "Not listed"],
  ]
  return (
    <div className="d-selpanel">
      <button type="button" className="d-sel-toggle" aria-expanded={detailsOpen} onClick={() => setDetailsOpen(open => !open)}>
        <span className="d-sel-heading">
          <span className="d-sel-label">Selected package</span>
          <span key={`${group.key}-name`} className="d-sel-name swap-fade">{group.kind === "dlc" && candidate?.label ? displayText(candidate.label) : group.title}{group.version ? ` ${group.kind === "backport" ? group.version : `v${group.version}`}` : ""}</span>
        </span>
        <Icon name="chevD" />
      </button>
      <dl key={`${group.key}-stats`} className="d-sel-stats swap-fade">
        {stats.map(([label, value]) => <div key={label}><dt>{label}</dt><dd title={value}>{value}</dd></div>)}
      </dl>
      <Collapse open={detailsOpen} className="d-sel-details">
        <dl key={`${group.key}-details`} className="kv swap-fade">
        <dt>Host</dt><dd>{displayText(candidate?.hoster || "Host")}</dd>
        <dt>Mirrors</dt><dd>{hosts}</dd>
        {candidate?.sourceName && <><dt>Source</dt><dd>{displayText(candidate.sourceName)}{candidate.sourceVersion ? ` ${candidate.sourceVersion}` : ""}</dd></>}
        <dt>Files</dt><dd>{archiveLabel(row.parts)}</dd>
        <dt>Provider</dt><dd className={row.tone === "fail" ? "fail" : "good"}>{row.status}</dd>
        </dl>
      </Collapse>
    </div>
  )
}

function About({ game, raw, metadataState, attribution, basePackage, backport, firmware, hasBackport, installed, demo, settings, visible }: {
  game: Game; raw?: MetadataResponse["game"]; metadataState: LoadState; attribution?: string; basePackage?: PackageCandidate; backport?: PackageCandidate
  firmware: string; hasBackport: boolean; installed: string; demo: boolean; settings: Settings; visible: PackageCandidate[]
}) {
  const genres = raw?.genres?.map(genre => genre.name).filter(Boolean) as string[] || []
  const description = raw?.description_raw || raw?.description || (metadataState === "loading" ? "Loading the description…" : metadataState === "error" ? "No description is available for this title. Packages can still be installed." : "No description is listed for this title.")
  return (
    <div className="d-about scroll">
      <div>
        <h2>About this game</h2>
        <p className="d-desc">{description}</p>
        {(genres.length > 0 || raw?.metacritic) && <div className="genres">{genres.map(genre => <span key={genre}>{genre}</span>)}{raw?.metacritic ? <span><Icon name="star" />Metacritic {raw.metacritic}</span> : null}</div>}
        <p className="attribution">{attribution || (game.titleId.startsWith("PPSA") ? "Title and cover from Prospero Patches" : "Title and cover from Orbis Patches")}</p>
      </div>
      <div>
        <h2>Technical information</h2>
        <dl className="kv">
          <dt>Title ID</dt><dd>{game.titleId}</dd>
          <dt>Content ID</dt><dd>{basePackage?.expectedContentId || "Not listed"}</dd>
          <dt>Base version</dt><dd>{basePackage?.version || (basePackage && packageVersion(basePackage)) || raw?.version || "Not listed"}</dd>
          <dt>Firmware</dt><dd>{firmware || "Not listed"}</dd>
          <dt>Backport</dt><dd>{platformOf(game.titleId) === "ps4" ? "PS5 games only" : hasBackport ? "Available" : "Not listed"}</dd>
          <dt>Install status</dt><dd className={installed ? "good" : ""}>{installed || "Not installed through SSPI"}</dd>
          {raw?.released && <><dt>Released</dt><dd>{raw.released}</dd></>}
          {raw?.size && <><dt>Storage</dt><dd>{raw.size}</dd></>}
        </dl>
        {basePackage && !demo && <Storage game={game} basePackage={basePackage} backport={backport} settings={settings} visible={visible} />}
      </div>
    </div>
  )
}

function Storage({ game, basePackage, backport, settings, visible }: { game: Game; basePackage: PackageCandidate; backport?: PackageCandidate; settings: Settings; visible: PackageCandidate[] }) {
  const [plan, setPlan] = useState<SpacePlan | null>(null)
  const [error, setError] = useState("")
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return
    let live = true
    const request: DeliveryRequest = {
      target: settings.activeConsole, package: { ...basePackage, kind: packageKind(basePackage) }, titleId: game.titleId, titleName: game.name,
      archiveParts: archivePartsFor(basePackage, visible),
      ...(backport ? { backport: { package: { ...backport, kind: "backport" }, parts: archivePartsFor(backport, visible) } } : {}),
    }
    invoke<SpacePlan>("delivery_space", { request }).then(value => { if (live) setPlan(value) }).catch(err => { if (live) setError(errorText(err)) })
    return () => { live = false }
  }, [basePackage, backport, settings.activeConsole])
  if (error) return <p className="rail-note">The storage check didn't finish: {error}</p>
  if (!plan) return null
  const volume = volumeOf(plan.directory) || plan.directory
  const free = plan.freeBytes ?? null
  const need = plan.requiredBytes
  const tone = !plan.enough ? "fail" : free != null && need > free * 0.6 ? "warn" : ""
  const pct = free && free > 0 ? Math.min(100, (need / free) * 100) : 100
  return (
    <div className="meter-block">
      <h2>Storage on {volume}</h2>
      <div className="meter-top"><span>Needed while installing{plan.estimated ? ", estimated" : ""}</span><strong>{fmtBytes(need)}</strong></div>
      <div className="meter" role="img" aria-label={`${fmtBytes(need)} needed, ${free == null ? "free space unknown" : `${fmtBytes(free)} free`} on ${volume}`}><i className={`need ${tone}`} style={{ width: `${pct}%` }} /></div>
      <div className="meter-legend">
        <div><span><i style={{ background: "var(--ink)" }} />This install, temporarily</span><b>{fmtBytes(need)}</b></div>
        <div><span><i style={{ background: "rgba(255,255,255,.08)" }} />Free now</span><b>{free == null ? "Unknown" : fmtBytes(free)}</b></div>
      </div>
      <p className="rail-note">{plan.message || (settings.keepPackages ? "The finished package is kept after installation." : "Working files are removed after a confirmed install.")}</p>
    </div>
  )
}
