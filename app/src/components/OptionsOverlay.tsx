/* =====================================================================
   Options — the SSPI PS4 "Options" overlay. Its sections take the place
   of the app's tabs in the header (Q / E switch them); each page is one
   column of grouped settings. "Save and close" persists; Appearance
   applies live.
   ===================================================================== */
import { invoke } from "@tauri-apps/api/core"
import { open as openDialog } from "@tauri-apps/plugin-dialog"
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type FormEvent, type ReactNode } from "react"
import { DoctorReportView } from "./DoctorReportView"
import { UpdatePanel } from "./Updater"
import { CheckDraw, ConsoleGlyph, Icon } from "./Icon"
import { Field, Row, Seg, Switch } from "./Controls"
import { toast } from "./toasts"
import { ACCENTS, DEFAULT_APPEARANCE, PATTERNS, accentColor, type Appearance } from "@/lib/appearance"
import { discoverConsoles } from "@/lib/console-api"
import { installCommunitySource, listCommunitySources, type CommunitySource, type CommunitySources } from "@/lib/community-sources"
import { displayText } from "@/lib/display"
import { errorText, fmtBytes } from "@/lib/format"
import { FPKG_LEVELS, FPKG_PRESETS, levelName, presetOf, type FpkgPreset } from "@/lib/fpkg-presets"
import { glide } from "@/lib/motion"
import { patternPreview } from "@/stage/art"
import type { DiscoveredConsole } from "@/lib/console-types"
import type { ConsoleKind, DoctorReport, FolderAction, PackageFormat, PackageSource, Ps4Probe, Settings } from "@/types"

export type OptionsTab = "consoles" | "sources" | "debrid" | "downloads" | "packaging" | "appearance" | "updates"
const TABS: Array<[OptionsTab, string]> = [["consoles", "Consoles"], ["sources", "Sources"], ["debrid", "Debrid"], ["downloads", "Downloads"], ["packaging", "Packaging"], ["appearance", "Appearance"], ["updates", "Updates"]]

/** The section before or after `tab`; the matching shoulder key flashes as if it was pressed. */
export function stepOptionsTab(tab: OptionsTab, direction: -1 | 1): OptionsTab {
  const index = TABS.findIndex(([id]) => id === tab)
  const key = document.getElementById(direction < 0 ? "optQ" : "optE")
  if (key) { key.classList.add("pressed"); window.setTimeout(() => key.classList.remove("pressed"), 140) }
  return TABS[(index + direction + TABS.length) % TABS.length][0]
}

/** The Options sections, drawn in the header in place of the app's tabs while Options is open. */
export function OptionsTabs({ tab, onTab }: { tab: OptionsTab; onTab: (tab: OptionsTab) => void }) {
  const ref = useRef<HTMLElement>(null)
  const ink = useRef<HTMLSpanElement>(null)
  const placed = useRef(false)
  useLayoutEffect(() => {
    glide(ink.current, ref.current?.querySelector<HTMLElement>(`.tab[data-tab="${tab}"]`) || null, ref.current, placed.current ? { inset: 14, liquid: true } : { inset: 14, instant: true })
    placed.current = true
  }, [tab])
  useEffect(() => {
    const onResize = () => glide(ink.current, ref.current?.querySelector<HTMLElement>(".tab[aria-selected=true]") || null, ref.current, { inset: 14, instant: true })
    window.addEventListener("resize", onResize)
    document.fonts?.ready.then(onResize).catch(() => undefined)
    return () => window.removeEventListener("resize", onResize)
  }, [])
  return (
    <nav className="tabs opt-nav" ref={ref} role="tablist" aria-label="Options sections">
      <span className="shoulder l" id="optQ" role="button" tabIndex={-1} aria-label="Previous section (Q)" onClick={() => onTab(stepOptionsTab(tab, -1))}>Q</span>
      {TABS.map(([id, label]) => <button key={id} type="button" className="tab" role="tab" data-tab={id} aria-selected={tab === id} onClick={() => onTab(id)}>{label}</button>)}
      <span className="shoulder r" id="optE" role="button" tabIndex={-1} aria-label="Next section (E)" onClick={() => onTab(stepOptionsTab(tab, 1))}>E</span>
      <span className="tab-ink" ref={ink} />
    </nav>
  )
}

type Props = {
  tab: OptionsTab
  setTab: (tab: OptionsTab) => void
  onClose: () => void
  settings: Settings
  setSettings: (settings: Settings) => void
  sources: PackageSource[]
  setSources: (sources: PackageSource[]) => void
  appearance: Appearance
  setAppearance: (update: Appearance | ((prev: Appearance) => Appearance), origin?: { x: number; y: number }) => void
  demo: boolean
  onLeaveDemo: () => void
  downloadReceiver: () => Promise<void>
  payloadBusy: boolean
  onConsoleTested: (target: ConsoleKind, ok: boolean) => void
  build: string
  /** Changes when the header's Options button asks to close (unsaved changes still ask first). */
  closeRequest: number
}

const inTauri = () => "__TAURI_INTERNALS__" in window

function TestButton({ busy, ok, onClick, label, okLabel = "Connected", disabled, icon = "signal" }: { busy: boolean; ok: boolean; onClick: () => void; label: string; okLabel?: string; disabled?: boolean; icon?: Parameters<typeof Icon>[0]["name"] }) {
  return (
    <button type="button" className={`btn sm ${ok ? "ok" : ""}`} disabled={disabled || busy} onClick={onClick}>
      {busy ? <span className="signal"><i /><i /><i /></span> : ok ? <CheckDraw on /> : <Icon name={icon} />}
      {busy ? "Testing" : ok ? okLabel : label}
    </button>
  )
}

export function OptionsOverlay(props: Props) {
  const { tab, setTab, onClose, settings, setSettings, sources, setSources, appearance, setAppearance, demo, onLeaveDemo, downloadReceiver, payloadBusy, onConsoleTested, build, closeRequest } = props
  const [draft, setDraft] = useState<Settings>(settings)
  const [rdToken, setRdToken] = useState("")
  const [torboxToken, setTorboxToken] = useState("")
  const [alldebridToken, setAlldebridToken] = useState("")
  const [ps4FtpPassword, setPs4FtpPassword] = useState("")
  const [ps4Message, setPs4Message] = useState("")
  const [ps5Message, setPs5Message] = useState("")
  const [discoveredConsoles, setDiscoveredConsoles] = useState<DiscoveredConsole[] | null>(null)
  const [discoveryError, setDiscoveryError] = useState("")
  const [scanningNetwork, setScanningNetwork] = useState(false)
  const [busy, setBusy] = useState("")
  const [flash, setFlash] = useState("")
  const [doctor, setDoctor] = useState<DoctorReport | null>(null)
  const [sourceUrl, setSourceUrl] = useState("")
  const [removing, setRemoving] = useState("")
  const [sheet, setSheet] = useState(false)
  const bodyRef = useRef<HTMLDivElement>(null)

  const dirty = !demo && (JSON.stringify(draft) !== JSON.stringify(settings) || !!rdToken || !!torboxToken || !!alldebridToken || !!ps4FtpPassword)
  const patch = (update: Partial<Settings>) => setDraft(current => ({ ...current, ...update }))

  useEffect(() => { if (bodyRef.current) bodyRef.current.scrollTop = 0 }, [tab])
  useEffect(() => { if (flash) { const t = window.setTimeout(() => setFlash(""), 1600); return () => window.clearTimeout(t) } }, [flash])

  const close = (force = false) => { if (dirty && !force) { setSheet(true); return } onClose() }
  const stepTab = (direction: -1 | 1) => setTab(stepOptionsTab(tab, direction))

  const save = async (event?: FormEvent) => {
    event?.preventDefault()
    if (demo) { onClose(); return }
    if (!dirty) { onClose(); return }
    setBusy("save")
    try {
      const saved = await invoke<Settings>("save_settings", { input: { ...draft, onboardingComplete: true, ...(ps4FtpPassword ? { ps4FtpPassword } : {}), realDebridToken: rdToken || undefined, torboxToken: torboxToken || undefined, alldebridToken: alldebridToken || undefined } })
      setSettings(saved)
      setDraft(saved)
      setRdToken(""); setTorboxToken(""); setAlldebridToken(""); setPs4FtpPassword("")
      toast({ tone: "success", title: "Settings saved", text: "API keys and passwords stay in Windows Credential Manager.", duration: 3600 })
      onClose()
    } catch (error) { toast({ tone: "error", title: "Settings couldn't be saved", text: errorText(error) }) }
    finally { setBusy("") }
  }

  // Options owns the keyboard while it is open.
  const live = useRef({ close, stepTab, save, sheet })
  live.current = { close, stepTab, save, sheet }
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const typing = (() => { const el = document.activeElement as HTMLElement | null; return !!el && ["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName) })()
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") { event.preventDefault(); void live.current.save(); return }
      if (event.key === "Escape") {
        event.preventDefault()
        if (live.current.sheet) { setSheet(false); return }
        if (typing) { (document.activeElement as HTMLElement).blur(); return }
        live.current.close()
        return
      }
      if (typing || event.ctrlKey || event.altKey || event.metaKey) return
      if (event.key === "q" || event.key === "Q") { event.preventDefault(); live.current.stepTab(-1) }
      if (event.key === "e" || event.key === "E") { event.preventDefault(); live.current.stepTab(1) }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])
  const firstCloseRequest = useRef(closeRequest)
  useEffect(() => { if (closeRequest !== firstCloseRequest.current) live.current.close() }, [closeRequest])

  /* ---------------------------------------------------------------- console checks */
  const run = async (key: string, work: () => Promise<void>, failTitle: string) => {
    setBusy(key)
    try { await work(); setFlash(key) }
    catch (error) { toast({ tone: "error", title: failTitle, text: errorText(error) }) }
    finally { setBusy("") }
  }
  const testPs5 = () => run("ps5", async () => {
    setPs5Message("")
    try { setPs5Message(await invoke<string>("test_ps5", { host: draft.ps5Host, port: draft.ps5Port })); onConsoleTested("ps5", true) }
    catch (error) { onConsoleTested("ps5", false); throw error }
  }, "The PS5 receiver didn't answer")
  const testPs4Receiver = () => run("ps4-test", async () => {
    setPs4Message("")
    try { setPs4Message(await invoke<string>("test_ps4_receiver", { host: draft.ps4Host, port: draft.ps4ReceiverPort })); onConsoleTested("ps4", true) }
    catch (error) { onConsoleTested("ps4", false); throw error }
  }, "The PS4 receiver didn't answer")
  const loadPs4Receiver = () => run("ps4-load", async () => {
    setPs4Message("Sending the payload and waiting for the PS4 receiver to start…")
    try { setPs4Message(await invoke<string>("load_ps4_receiver", { host: draft.ps4Host, loaderPort: draft.ps4LoaderPort, receiverPort: draft.ps4ReceiverPort })); onConsoleTested("ps4", true) }
    catch (error) { setPs4Message(""); onConsoleTested("ps4", false); throw error }
  }, "The receiver couldn't be loaded")
  const testPs4Ftp = () => run("ps4", async () => {
    setPs4Message("")
    try {
      const probe = await invoke<Ps4Probe>("test_ps4", { host: draft.ps4Host, port: draft.ps4FtpPort, user: draft.ps4FtpUser || undefined, ...(ps4FtpPassword ? { password: ps4FtpPassword } : {}) })
      setPs4Message(probe.message)
      onConsoleTested("ps4", true)
    } catch (error) { onConsoleTested("ps4", false); throw error }
  }, "The PS4 didn't answer")
  const exportPs4 = () => run("ps4-export", async () => {
    toast({ tone: "success", title: "PS4 receiver payload saved", text: await invoke<string>("export_ps4_receiver_payload") })
  }, "The payload couldn't be saved")
  const findConsoles = async () => {
    setScanningNetwork(true)
    setDiscoveryError("")
    setDiscoveredConsoles(null)
    try { setDiscoveredConsoles(await discoverConsoles({ demo })) }
    catch (error) { setDiscoveryError(errorText(error)) }
    finally { setScanningNetwork(false) }
  }
  const useDiscoveredConsole = (console: DiscoveredConsole, target: ConsoleKind) => {
    if (target === "ps5") patch({ ps5Host: console.host, ...(console.receiver ? { ps5Port: console.receiver.port } : {}) })
    else patch({ ps4Host: console.host, ...(console.receiver ? { ps4ReceiverPort: console.receiver.port } : {}) })
  }
  const verify = (id: string, token: string) => run(id, async () => {
    toast({ tone: "success", title: "Account verified", text: await invoke<string>("verify_provider", { provider: id, token: token || undefined }) })
  }, "The account couldn't be verified")

  /* ---------------------------------------------------------------- sources */
  const installSource = () => run("source-install", async () => {
    if (!sourceUrl.trim()) return
    setSources(await invoke<PackageSource[]>("install_package_source", { url: sourceUrl.trim() }))
    setSourceUrl("")
    toast({ tone: "success", title: "Package source installed", text: "It's enabled, and search now includes it." })
  }, "The source couldn't be installed")
  const browseSource = () => run("source-browse", async () => {
    const value = await openDialog({ multiple: false, directory: false, title: "Choose a .gssource file", filters: [{ name: "Package Source", extensions: ["gssource", "zip"] }] })
    if (!value || Array.isArray(value)) return
    setSources(await invoke<PackageSource[]>("install_package_source_from_path", { path: value }))
    toast({ tone: "success", title: "Package source installed", text: "Installed from the file and enabled." })
  }, "The source couldn't be installed")
  const toggleSource = (id: string, enabled: boolean) => run(`source-${id}`, async () => {
    setSources(await invoke<PackageSource[]>("set_package_source_enabled", { id, enabled }))
  }, "The source couldn't be changed")
  const removeSource = (id: string) => run(`source-${id}`, async () => {
    setSources(await invoke<PackageSource[]>("remove_package_source", { id }))
    setRemoving("")
    toast({ tone: "success", title: "Package source removed", text: "Its titles no longer appear in search." })
  }, "The source couldn't be removed")

  /* ---------------------------------------------------------------- downloads and packaging */
  const browseFolder = async () => {
    try {
      const value = await openDialog({ multiple: false, directory: true, title: "Choose the download folder", defaultPath: draft.downloadDir || undefined })
      if (value && !Array.isArray(value)) patch({ downloadDir: value })
    } catch (error) { toast({ tone: "error", title: "The folder picker didn't open", text: errorText(error) }) }
  }
  const inspectDump = async () => {
    try {
      const path = await openDialog({ directory: true, multiple: false, title: "Inspect an extracted PS5 dump" })
      if (!path || Array.isArray(path)) return
      setBusy("doctor")
      setDoctor(null)
      setDoctor(await invoke<DoctorReport>("inspect_package_dump", { path }))
      setFlash("doctor")
    } catch (error) { toast({ tone: "error", title: "The dump couldn't be inspected", text: errorText(error) }) }
    finally { setBusy("") }
  }

  /* ---------------------------------------------------------------- sections */
  const disabled = demo || !inTauri()
  const preset = presetOf(draft.fpkgPreset)
  const exactLevel = draft.fpkgCompressionLevel ?? null
  const image = draft.packageFormat === "exfat"
  const sections: Record<OptionsTab, ReactNode> = {
    consoles: <>
      <div className="orows">
        <Row label="Find consoles on this network">
          <button type="button" className="btn sm" disabled={scanningNetwork || (!demo && !inTauri())} onClick={() => void findConsoles()}>
            {scanningNetwork ? <><span className="spinner" aria-hidden="true" />Scanning</> : <><Icon name="search" />Find consoles</>}
          </button>
        </Row>
      </div>
      {discoveryError && <p className="opt-msg" role="alert">{discoveryError}</p>}
      {discoveredConsoles?.length === 0 && <p className="opt-msg" role="status">No consoles answered. Check that the console is on and on the same network, then load the receiver.</p>}
      {!!discoveredConsoles?.length && <div className="orows" aria-label="Discovered consoles">
        {discoveredConsoles.map(console => <Row key={`${console.host}-${console.label}`} label={console.host} detail={console.label}>
          {(console.platform === "ps5" || console.platform === "unknown") && <button type="button" className="btn sm ghost" onClick={() => useDiscoveredConsole(console, "ps5")}>Use for PS5</button>}
          {(console.platform === "ps4" || console.platform === "unknown") && <button type="button" className="btn sm ghost" onClick={() => useDiscoveredConsole(console, "ps4")}>Use for PS4</button>}
        </Row>)}
      </div>}
      <div className="opt-section"><ConsoleGlyph kind="ps5" />PS5</div>
      <div className="orows">
        <Row label="Address"><Field label="PS5 address" value={draft.ps5Host} onChange={value => patch({ ps5Host: value })} placeholder="IP address or hostname" icon="monitor" /></Row>
        <Row label="Receiver port"><Field label="PS5 receiver port" type="number" width={110} value={draft.ps5Port} onChange={value => patch({ ps5Port: Number(value) })} /></Row>
        <Row label="ELF loader port" detail="etaHEN and elfldr use 9021"><Field label="PS5 ELF loader port" type="number" width={110} value={draft.ps5LoaderPort ?? 9021} onChange={value => { const port = Number(value); if (Number.isFinite(port)) patch({ ps5LoaderPort: Math.min(65535, Math.max(1, Math.trunc(port))) }) }} /></Row>
        <Row label="Receiver" detail={ps5Message || "Reload it after updating SSPI"} tone={ps5Message ? "good" : undefined}>
          <TestButton busy={busy === "ps5"} ok={flash === "ps5"} onClick={() => void testPs5()} label="Test receiver" disabled={disabled} />
          <button type="button" className="btn ghost sm" disabled={payloadBusy || disabled} onClick={() => void downloadReceiver()}>{payloadBusy ? <span className="spinner" /> : <Icon name="download" />}Download receiver ELF</button>
        </Row>
      </div>
      <div className="opt-section"><ConsoleGlyph kind="ps4" />PS4</div>
      <div className="orows">
        <Row label="Address"><Field label="PS4 address" value={draft.ps4Host} onChange={value => patch({ ps4Host: value })} placeholder="IP address or hostname" icon="monitor" /></Row>
        <Row label="Delivery method">
          <Seg label="PS4 delivery method" value={draft.ps4Transport} options={[["receiver", "Receiver payload"], ["inbox", "SSPI inbox"]]} onChange={value => patch({ ps4Transport: value })} />
        </Row>
        {draft.ps4Transport === "receiver" ? <>
          <Row label="Receiver port"><Field label="PS4 receiver port" type="number" width={110} value={draft.ps4ReceiverPort} onChange={value => patch({ ps4ReceiverPort: Number(value) })} /></Row>
          <Row label="GoldHEN BinLoader port"><Field label="BinLoader port" type="number" width={110} value={draft.ps4LoaderPort} onChange={value => patch({ ps4LoaderPort: Number(value) })} /></Row>
          <Row label="PC download port" detail="Allow SSPI through Windows Firewall when asked"><Field label="PC download port" type="number" width={110} value={draft.ps4ServePort} onChange={value => patch({ ps4ServePort: Number(value) })} /></Row>
          <Row label="Receiver" detail="Load it again after each PS4 restart">
            <button type="button" className="btn sm" disabled={!!busy || disabled} onClick={() => void loadPs4Receiver()}>{busy === "ps4-load" ? <span className="spinner" /> : <Icon name="upload" />}Load receiver on PS4</button>
            <TestButton busy={busy === "ps4-test"} ok={flash === "ps4-test"} onClick={() => void testPs4Receiver()} label="Test receiver" disabled={disabled} />
            <button type="button" className="btn ghost sm" disabled={!!busy || disabled} onClick={() => void exportPs4()}><Icon name="download" />Download PS4 payload</button>
          </Row>
        </> : <>
          <Row label="FTP port"><Field label="FTP port" type="number" width={110} value={draft.ps4FtpPort} onChange={value => patch({ ps4FtpPort: Number(value) })} /></Row>
          <Row label="FTP user"><Field label="FTP user" value={draft.ps4FtpUser} onChange={value => patch({ ps4FtpUser: value })} placeholder="anonymous" width={180} /></Row>
          <Row label="FTP password" detail={draft.ps4FtpPasswordConfigured ? "Saved. Leave blank to keep it." : undefined}><Field label="FTP password" type="password" value={ps4FtpPassword} onChange={setPs4FtpPassword} width={180} icon="lock" /></Row>
          <Row label="Archives for PS4">
            <select className="select" value={draft.ps4ArchiveMode} onChange={event => patch({ ps4ArchiveMode: event.target.value as Settings["ps4ArchiveMode"] })}>
              <option value="pc">Extract on this PC (recommended)</option>
              <option value="ps4">Send RAR sets to the PS4</option>
            </select>
          </Row>
          <Row label="Remove uploads after a confirmed install"><Switch label="Remove uploads after install" checked={draft.ps4RemoveAfterInstall} onChange={value => patch({ ps4RemoveAfterInstall: value })} /></Row>
          <Row label="Connection">
            <TestButton busy={busy === "ps4"} ok={flash === "ps4"} onClick={() => void testPs4Ftp()} label="Test PS4" disabled={disabled} />
          </Row>
        </>}
      </div>
      {ps4Message && <p className="opt-msg" role="status">{ps4Message}</p>}
      <p className="opt-note">{draft.ps4Transport === "receiver" ? "Needs GoldHEN 2.4b18.5 or newer with BinLoader on." : "Needs GoldHEN's FTP server."}</p>
    </>,
    sources: <>
      <CommunitySourceList sources={sources} setSources={setSources} demo={demo} disabled={disabled} busy={busy} setBusy={setBusy} />
      <div className="opt-section">Add a source</div>
      <div className="orows">
        <Row label="Install from a URL">
          <Field label="Source URL" value={sourceUrl} onChange={setSourceUrl} placeholder="https://…/source.gssource" icon="link" width={280} />
          <button type="button" className="btn sm" disabled={!sourceUrl.trim() || !!busy || disabled} onClick={() => void installSource()}>{busy === "source-install" ? <span className="spinner" /> : <Icon name="download" />}Install</button>
        </Row>
        <Row label="Install from a file">
          <button type="button" className="btn sm" disabled={!!busy || disabled} onClick={() => void browseSource()}>{busy === "source-browse" ? <span className="spinner" /> : <Icon name="folder" />}Browse</button>
        </Row>
      </div>
      <div className="opt-section">Installed sources</div>
      {!!sources.length && <div className="orows">
        {sources.map(source => (
          <div key={source.id} className="orow">
            <div className="source-row">
              <span className="src-ico"><Icon name="library" /></span>
              <div className="ol"><strong>{displayText(source.name)} <span style={{ display: "inline", color: "var(--faint)" }}>{source.version}</span></strong><span>{source.description}</span></div>
            </div>
            <div className="or">
              <Switch label={`Enable ${displayText(source.name)}`} checked={source.enabled} disabled={!!busy || disabled} onChange={value => void toggleSource(source.id, value)} />
              {removing === source.id
                ? <><button type="button" className="btn sm danger" disabled={!!busy} onClick={() => void removeSource(source.id)}>Remove source</button><button type="button" className="btn ghost sm" onClick={() => setRemoving("")}>Keep</button></>
                : <button type="button" className="btn ghost sm" disabled={!!busy || disabled} onClick={() => setRemoving(source.id)}>Remove</button>}
            </div>
          </div>
        ))}
      </div>}
      {!sources.length && <p className="opt-note">No sources installed yet.</p>}
    </>,
    debrid: <>
      {([
        { id: "real-debrid", name: "Real-Debrid", enabled: draft.realDebridEnabled, configured: draft.realDebridConfigured, token: rdToken, setToken: setRdToken, setEnabled: (value: boolean) => patch({ realDebridEnabled: value }) },
        { id: "torbox", name: "TorBox", enabled: draft.torboxEnabled, configured: draft.torboxConfigured, token: torboxToken, setToken: setTorboxToken, setEnabled: (value: boolean) => patch({ torboxEnabled: value }) },
        { id: "alldebrid", name: "AllDebrid", enabled: draft.alldebridEnabled, configured: draft.alldebridConfigured, token: alldebridToken, setToken: setAlldebridToken, setEnabled: (value: boolean) => patch({ alldebridEnabled: value }) },
      ]).map(service => (
        <div key={service.id}>
          <div className="opt-section"><span className={`dot ${service.enabled && service.configured ? "good" : ""}`} />{service.name}</div>
          <div className="orows">
            <Row label={`Use ${service.name}`}><Switch label={`Use ${service.name}`} checked={service.enabled} onChange={service.setEnabled} /></Row>
            <Row label="API key" detail={service.configured ? "Saved. Leave blank to keep it." : undefined} tone={service.configured ? "good" : undefined}>
              <Field label={`${service.name} API key`} type="password" value={service.token} onChange={service.setToken} placeholder={service.configured ? "Saved" : "API key"} icon="key" width={220} />
              <TestButton busy={busy === service.id} ok={flash === service.id} onClick={() => void verify(service.id, service.token)} label="Verify" okLabel="Verified" icon="shield" disabled={disabled} />
            </Row>
          </div>
        </div>
      ))}
      <p className="opt-note">Keys are stored in Windows Credential Manager.</p>
    </>,
    downloads: <>
      <div className="orows">
        <Row label="Download folder">
          <Field label="Download folder" value={draft.downloadDir} onChange={value => patch({ downloadDir: value })} icon="folder" width={340} />
          <button type="button" className="btn sm" disabled={disabled} onClick={() => void browseFolder()}>Browse</button>
        </Row>
        <Row label="Keep downloaded archives"><Switch label="Keep downloaded archives" checked={draft.keepArchives} onChange={value => patch({ keepArchives: value })} /></Row>
        <Row label="Keep extracted game files"><Switch label="Keep extracted game files" checked={draft.keepExtractions} onChange={value => patch({ keepExtractions: value })} /></Row>
        <Row label="Keep packaged PKGs" detail="After a confirmed install"><Switch label="Keep packaged PKGs" checked={draft.keepPackages} onChange={value => patch({ keepPackages: value })} /></Row>
      </div>
      <p className="opt-note">Imported originals are never removed.</p>
      <div className="opt-section">At the same time</div>
      <div className="orows">
        <Row label="Games downloading" detail="1 to 8. A prioritized game always starts">
          <Field label="Games downloading" type="number" width={90} value={draft.downloadSlots ?? 4} onChange={value => patch({ downloadSlots: Math.min(8, Math.max(1, Math.round(Number(value)) || 4)) })} />
        </Row>
        <Row label="Games extracting" detail="1 to 4">
          <Field label="Games extracting" type="number" width={90} value={draft.extractionSlots ?? 2} onChange={value => patch({ extractionSlots: Math.min(4, Math.max(1, Math.round(Number(value)) || 2)) })} />
        </Row>
        <Row label="Download connections" detail="1 to 32, shared so downloads finish together">
          <Field label="Download connections" type="number" width={90} value={draft.downloadConnections ?? 16} onChange={value => patch({ downloadConnections: Math.min(32, Math.max(1, Math.round(Number(value)) || 16)) })} />
        </Row>
      </div>
      <p className="opt-note">Prioritize a game on its download card (or press T) to give it about 90% of the download connections and the first extraction, packaging and console slot. Other games' extraction and packaging run at low priority while it works.</p>
      <div className="opt-section">Upload to PS5</div>
      <div className="orows">
        <Row label="Max bandwidth" detail="Up to 12 upload lanes instead of 4"><Switch label="Max bandwidth" checked={(draft.transferMode || "balanced") === "max"} onChange={value => patch({ transferMode: value ? "max" : "balanced" })} /></Row>
        {(draft.transferMode || "balanced") === "max" && (
          <Row label="Upload lanes" detail="1 to 12"><Field label="Upload lanes" type="number" width={90} value={draft.uploadLanes || 4} onChange={value => patch({ uploadLanes: Math.min(12, Math.max(1, Number(value) || 4)) })} /></Row>
        )}
      </div>
    </>,
    packaging: <>
      <div className="orows">
        <Row label="Package extracted dumps"><Switch label="Package extracted dumps" checked={draft.packageDumps} onChange={value => patch({ packageDumps: value })} /></Row>
        {draft.packageDumps && (
          <Row label="Output" detail={image ? "Mounted by ShadowMount Plus; nothing is installed" : "An installable .pkg"}>
            <Seg<PackageFormat> label="Output" value={image ? "exfat" : "fpkg"} options={[["fpkg", "FPKG package"], ["exfat", "ShadowMount image"]]} onChange={value => patch({ packageFormat: value })} />
          </Row>
        )}
        {draft.packageDumps && image && (
          <Row label="Lizard asset packing" detail="Experimental. Only for backports that use ampr_emu">
            <Switch label="Lizard asset packing" checked={!!draft.lizardPacking} onChange={value => patch({ lizardPacking: value })} />
          </Row>
        )}
        <Row label="When game folders are added">
          <select className="select" aria-label="When game folders are added" value={draft.folderAction || "ask"} onChange={event => patch({ folderAction: event.target.value as FolderAction })}>
            <option value="ask">Ask</option>
            <option value="package">Package all</option>
            <option value="package-send">Package and send all</option>
            <option value="send">Send all as folders</option>
          </select>
        </Row>
        <Row label="Download and package only" detail="Keeps the output on this PC; nothing is sent"><Switch label="Download and package only" checked={draft.downloadPackageOnly} onChange={value => patch({ downloadPackageOnly: value, ...(value ? { packageDumps: true } : {}) })} /></Row>
        <Row label="Keep finished packages when removing"><Switch label="Keep finished packages when removing" checked={draft.keepPackagesOnRemove !== false} onChange={value => patch({ keepPackagesOnRemove: value })} /></Row>
        {draft.packageDumps && !image && (
          <Row label="Compression" detail={exactLevel != null ? `Kraken ${exactLevel} is set in Advanced` : FPKG_PRESETS[preset].note}>
            <Seg<FpkgPreset> label="Compression" value={preset} options={(Object.keys(FPKG_PRESETS) as FpkgPreset[]).map(id => [id, FPKG_PRESETS[id].label])} onChange={value => patch({ fpkgPreset: value, fpkgCompressionLevel: null })} />
          </Row>
        )}
      </div>
      {draft.packageDumps && (
        <details className="opt-advanced">
          <summary>Advanced packaging</summary>
          <div className="orows">
            {!image && <>
              <Row label="Exact Kraken level" detail={exactLevel != null ? "Overrides the compression choice" : undefined}>
                <select className="select" aria-label="Exact Kraken level" value={exactLevel ?? "preset"} onChange={event => patch({ fpkgCompressionLevel: event.target.value === "preset" ? null : Number(event.target.value) })}>
                  <option value="preset">Use the compression choice</option>
                  {FPKG_LEVELS.map(value => <option key={value} value={value}>{value} · {levelName(value)}</option>)}
                </select>
              </Row>
              <Row label="PFS v3" detail="Firmware 7.00 or newer"><Switch label="PFS v3" checked={draft.fpkgPfsVersion === 3} onChange={value => patch({ fpkgPfsVersion: value ? 3 : 2 })} /></Row>
            </>}
            <Row label="Target console firmware" detail="Optional. Used to pick backports"><Field label="Target firmware" value={draft.targetFw} onChange={value => patch({ targetFw: value })} placeholder="For example 4.03" width={150} /></Row>
            <Row label="Dump doctor" detail="Checks and repairs modules and metadata"><Switch label="Dump doctor" checked={!!draft.fpkgDoctor} onChange={value => patch({ fpkgDoctor: value })} /></Row>
            {!image && <Row label="Packaging engine" detail="Found automatically if blank"><Field label="Packaging engine path" value={draft.fpkgEnginePath} onChange={value => patch({ fpkgEnginePath: value })} placeholder="fpkg-cli.exe" icon="cpu" width={320} /></Row>}
          </div>
        </details>
      )}
      {draft.packageDumps && !image && <p className="opt-note">Backports need kstuff, PPR patches and a loader with backport support, such as ShadowMount Plus 1.7.</p>}
      {draft.packageDumps && image && <p className="opt-note">Needs ShadowMount Plus on the PS5.</p>}
      <div className="opt-section">Check a dump</div>
      <div className="orows">
        <Row label="Inspect a dump" detail="Runs the dump doctor without packaging">
          <button type="button" className={`btn sm ${flash === "doctor" ? "ok" : ""}`} disabled={!!busy || disabled} onClick={() => void inspectDump()}>{busy === "doctor" ? <span className="spinner" /> : <Icon name="shield" />}Inspect a dump</button>
        </Row>
      </div>
      {doctor && <div className="opt-msg"><DoctorReportView report={doctor} /></div>}
    </>,
    appearance: <AppearanceSection appearance={appearance} setAppearance={setAppearance} reduceMotion={draft.reduceMotion} setReduceMotion={value => patch({ reduceMotion: value })} />,
    updates: <UpdatePanel demo={demo} />,
  }

  return (
    <div className="options" role="dialog" aria-modal="true" aria-label="Options">
      <div className="opt-scrim" onClick={() => close()} />
      <form className="opt-panel" onSubmit={save}>
        <div className="opt-body" ref={bodyRef}>
          {demo && (
            <div className="orows demo-note">
              <Row label="Offline preview is on" detail="Nothing is sent to a console" tone="warn">
                <button type="button" className="btn primary sm" onClick={onLeaveDemo}>Leave the preview</button>
              </Row>
            </div>
          )}
          <div key={tab} className="swap-fade">{sections[tab]}</div>
        </div>
        <div className="opt-foot">
          <span className="dirty">{demo ? "Settings can't be saved in the offline preview." : dirty ? "You have unsaved changes." : "Appearance changes apply right away."}</span>
          <div className="row">
            <span className="opt-build">{build}</span>
            <button type="button" className="btn ghost" onClick={() => close()}>Close</button>
            <button type="submit" className="btn primary" disabled={busy === "save"}>{busy === "save" ? <span className="spinner" /> : <Icon name="check" />}Save and close</button>
          </div>
        </div>
        {sheet && (
          <div className="sheet" role="alertdialog" aria-label="Unsaved changes">
            <h3>Save your changes?</h3>
            <p>Your console, source or download settings changed. Appearance changes are already applied.</p>
            <div className="dialog-actions">
              <button type="button" className="btn primary" onClick={() => void save()}>Save and close</button>
              <button type="button" className="btn" onClick={() => { setSheet(false); close(true) }}>Discard changes</button>
              <button type="button" className="btn ghost" onClick={() => setSheet(false)}>Keep editing</button>
            </div>
          </div>
        )}
      </form>
    </div>
  )
}

function CommunitySourceList({ sources, setSources, demo, disabled, busy, setBusy }: { sources: PackageSource[]; setSources: Props["setSources"]; demo: boolean; disabled: boolean; busy: string; setBusy: (value: string) => void }) {
  const [catalog, setCatalog] = useState<CommunitySources | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState("")
  const active = useRef(false)
  const request = useRef(0)
  const installing = useRef(false)
  const available = demo || !disabled

  const refresh = useCallback(async () => {
    if (!available) return
    const current = ++request.current
    setLoading(true)
    setError("")
    try {
      const result = await listCommunitySources(demo)
      if (active.current && current === request.current) setCatalog(result)
    } catch (reason) {
      if (active.current && current === request.current) setError(errorText(reason))
    } finally {
      if (active.current && current === request.current) setLoading(false)
    }
  }, [available, demo])

  useEffect(() => {
    active.current = true
    setCatalog(null)
    void refresh()
    return () => { active.current = false; request.current++ }
  }, [refresh])

  const install = async (entry: CommunitySource) => {
    if (disabled || busy || loading || installing.current) return
    installing.current = true
    setBusy(`community-${entry.id}`)
    setError("")
    const existing = sources.find(source => source.id === entry.source)
    try {
      const result = await installCommunitySource(entry.id)
      setSources(result)
      if (active.current) setCatalog(current => current && ({ ...current, entries: current.entries.map(item => item.source === entry.source ? { ...item, installed: item.id === entry.id } : item) }))
      const installed = result.find(source => source.id === entry.source)
      toast({ tone: "success", title: `${displayText(entry.name)} ${existing ? "updated" : "installed"}`, text: installed?.enabled ? "Search now includes this source." : "The source is installed and remains disabled." })
    } catch (reason) {
      const message = `${displayText(entry.name)}: ${errorText(reason)}`
      if (active.current) setError(message)
      toast({ tone: "error", title: "The source couldn't be installed", text: message })
    } finally {
      installing.current = false
      setBusy("")
    }
  }

  return <section aria-label="Community sources">
    <div className="opt-section"><Icon name="globe" />Community sources
      <button type="button" className="btn ghost sm" style={{ marginLeft: "auto" }} disabled={loading || !!busy || !available} onClick={() => void refresh()}>{loading ? <span className="spinner" /> : <Icon name="refresh" />}{loading ? "Loading…" : "Refresh"}</button>
    </div>
    {catalog?.warning && <p className="opt-msg" role="status">{catalog.warning}</p>}
    {error && <p className="opt-msg" role="alert">{error}</p>}
    {!available && <p className="opt-note">Open the installed SSPI app to browse community sources.</p>}
    {loading && !catalog && <p className="opt-note" role="status">Loading the community directory…</p>}
    {catalog && !catalog.entries.length && !loading && !error && <p className="opt-note">No community sources are available.</p>}
    {!!catalog?.entries.length && <div className="orows" aria-busy={loading}>
      {catalog.entries.map(entry => {
        const installed = sources.find(source => source.id === entry.source)
        const current = !!installed && entry.installed
        const pending = busy === `community-${entry.id}`
        const metadata = [...entry.tags, entry.revision ? `Revision ${entry.revision}` : "", entry.date, entry.size ? fmtBytes(entry.size) : ""].filter(Boolean).join(" · ")
        return <div key={entry.id} className="orow">
          <div className="source-row">
            <span className="src-ico"><Icon name="library" /></span>
            <div className="ol">
              <strong>{displayText(entry.name)}</strong>
              {entry.description && <span>{entry.description}</span>}
              {metadata && <span>{metadata}</span>}
              {installed && <span className={installed.enabled ? "good" : ""}>Installed{installed.version ? ` · ${installed.version}` : ""}{installed.enabled ? " · Enabled" : " · Disabled"}</span>}
            </div>
          </div>
          <div className="or"><button type="button" className="btn sm" disabled={disabled || !!busy || loading || current} onClick={() => void install(entry)} aria-label={`${current ? "Installed" : installed ? "Update" : "Install"} ${displayText(entry.name)}`}>
            {pending ? <span className="spinner" /> : <Icon name={current ? "check" : installed ? "refresh" : "download"} />}{pending ? installed ? "Updating…" : "Installing…" : current ? "Installed" : installed ? "Update" : "Install"}
          </button></div>
        </div>
      })}
    </div>}
  </section>
}

function AppearanceSection({ appearance, setAppearance, reduceMotion, setReduceMotion }: { appearance: Appearance; setAppearance: Props["setAppearance"]; reduceMotion: boolean; setReduceMotion: (value: boolean) => void }) {
  const tilesRef = useRef<HTMLDivElement>(null)
  const [customAccent, setCustomAccent] = useState(appearance.accent)
  useEffect(() => { setCustomAccent(appearance.accent) }, [appearance.accent])
  useEffect(() => {
    tilesRef.current?.querySelectorAll<HTMLCanvasElement>("canvas[data-pattern]").forEach(canvas => patternPreview(canvas.dataset.pattern || "solid", appearance.accent, canvas))
  }, [appearance.accent])
  return <>
    <div className="opt-section">Background pattern</div>
    <div className="patterns" role="radiogroup" aria-label="Background pattern" ref={tilesRef}>
      {PATTERNS.map(pattern => (
        <button key={pattern.id} type="button" className="pattern" role="radio" aria-checked={appearance.pattern === pattern.id} onClick={() => setAppearance(prev => ({ ...prev, pattern: pattern.id }))}>
          <canvas data-pattern={pattern.id} /><span>{pattern.name}</span>
        </button>
      ))}
    </div>
    <div className="opt-section">Accent</div>
    <div className="swatches" role="radiogroup" aria-label="Accent colour">
      {ACCENTS.map(accent => (
        <button key={accent.hex} type="button" className="swatch" role="radio" aria-checked={appearance.accent.toUpperCase() === accent.hex} style={{ ["--c" as string]: accent.hex }} onClick={event => setAppearance(prev => ({ ...prev, accent: accent.hex }), { x: event.clientX, y: event.clientY })}>
          <i />{accent.name}<Icon name="check" className="ck" />
        </button>
      ))}
    </div>
    <div className="accent-picker">
      <label className="accent-custom"><input type="color" aria-label="Choose custom accent" value={appearance.accent} onChange={event => setAppearance(prev => ({ ...prev, accent: event.target.value.toUpperCase() }))} /><span>Custom colour</span></label>
      <label className="accent-hex"><span>Hex</span><input aria-label="Accent hex colour" value={customAccent} maxLength={7} spellCheck={false} placeholder="#E4E4E1" aria-invalid={!accentColor(customAccent)} onChange={event => {
        setCustomAccent(event.target.value)
        const accent = accentColor(event.target.value)
        if (accent) setAppearance(prev => ({ ...prev, accent }))
      }} onBlur={() => setCustomAccent(appearance.accent)} onKeyDown={event => { if (event.key === "Enter") event.currentTarget.blur() }} /></label>
    </div>
    <div className="opt-section">Artwork and motion</div>
    <div className="orows">
      <Row label="Tint with game artwork"><Switch label="Tint with game artwork" checked={appearance.gameTint} onChange={value => setAppearance(prev => ({ ...prev, gameTint: value }))} /></Row>
      <Row label="Reduced motion"><Switch label="Reduced motion" checked={reduceMotion} onChange={setReduceMotion} /></Row>
      <Row label="Restore defaults">
        <button type="button" className="btn sm" onClick={event => setAppearance(prev => ({ ...prev, accent: DEFAULT_APPEARANCE.accent, pattern: DEFAULT_APPEARANCE.pattern, gameTint: DEFAULT_APPEARANCE.gameTint, cardStyle: DEFAULT_APPEARANCE.cardStyle, cardSize: DEFAULT_APPEARANCE.cardSize }), { x: event.clientX, y: event.clientY })}><Icon name="refresh" />Restore</button>
      </Row>
    </div>
    <div className="opt-section">Download cards</div>
    <div className="orows">
      <Row label="Card style">
        <Seg label="Download card style" value={appearance.cardStyle} options={[["art", "Artwork"], ["poster", "Poster"], ["plain", "Plain"]]} onChange={cardStyle => setAppearance(prev => ({ ...prev, cardStyle }))} />
      </Row>
      <Row label="Card size">
        <Seg label="Download card size" value={appearance.cardSize} options={[["large", "Large"], ["compact", "Compact"]]} onChange={cardSize => setAppearance(prev => ({ ...prev, cardSize }))} />
      </Row>
    </div>
  </>
}
