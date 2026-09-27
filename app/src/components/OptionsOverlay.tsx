/* =====================================================================
   Options — the SSPI PS4 "Options" overlay: sections switch with Q / E,
   rows take focus, "Save and close" persists. Appearance applies live.
   ===================================================================== */
import { invoke } from "@tauri-apps/api/core"
import { open as openDialog } from "@tauri-apps/plugin-dialog"
import { useEffect, useLayoutEffect, useRef, useState, type FormEvent, type ReactNode } from "react"
import { DoctorReportView } from "./DoctorReportView"
import { CheckDraw, Icon } from "./Icon"
import { Field, Row, Seg, Switch } from "./Controls"
import { Dock, type Hint } from "./Shell"
import { toast } from "./toasts"
import { ACCENTS, DEFAULT_APPEARANCE, PATTERNS, type Appearance } from "@/lib/appearance"
import { discoverConsoles } from "@/lib/console-api"
import { displayText } from "@/lib/display"
import { errorText } from "@/lib/format"
import { glide } from "@/lib/motion"
import { patternPreview } from "@/stage/art"
import type { DiscoveredConsole } from "@/lib/console-types"
import type { ConsoleKind, DoctorReport, PackageSource, Ps4Probe, Settings } from "@/types"

export type OptionsTab = "consoles" | "sources" | "debrid" | "downloads" | "packaging" | "appearance"
const TABS: Array<[OptionsTab, string]> = [["consoles", "Consoles"], ["sources", "Sources"], ["debrid", "Debrid"], ["downloads", "Downloads"], ["packaging", "Packaging"], ["appearance", "Appearance"]]

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
  const { tab, setTab, onClose, settings, setSettings, sources, setSources, appearance, setAppearance, demo, onLeaveDemo, downloadReceiver, payloadBusy, onConsoleTested, build } = props
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
  const tabsRef = useRef<HTMLDivElement>(null)
  const thumbRef = useRef<HTMLSpanElement>(null)
  const bodyRef = useRef<HTMLDivElement>(null)

  const dirty = !demo && (JSON.stringify(draft) !== JSON.stringify(settings) || !!rdToken || !!torboxToken || !!alldebridToken || !!ps4FtpPassword)
  const patch = (update: Partial<Settings>) => setDraft(current => ({ ...current, ...update }))

  useLayoutEffect(() => { glide(thumbRef.current, tabsRef.current?.querySelector<HTMLElement>(`.opt-tab[data-tab="${tab}"]`) || null, tabsRef.current) }, [tab])
  useEffect(() => { if (bodyRef.current) bodyRef.current.scrollTop = 0 }, [tab])
  useEffect(() => { if (flash) { const t = window.setTimeout(() => setFlash(""), 1600); return () => window.clearTimeout(t) } }, [flash])

  const close = (force = false) => { if (dirty && !force) { setSheet(true); return } onClose() }
  const stepTab = (direction: number) => {
    const index = TABS.findIndex(([id]) => id === tab)
    const next = TABS[(index + direction + TABS.length) % TABS.length][0]
    const el = document.getElementById(direction < 0 ? "optQ" : "optE")
    if (el) { el.classList.add("pressed"); window.setTimeout(() => el.classList.remove("pressed"), 140) }
    setTab(next)
  }

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
  const level = draft.fpkgCompressionLevel ?? ({ fast: 2, standard: 4, smallest: 7 } as Record<string, number>)[draft.fpkgPreset] ?? 2
  const sections: Record<OptionsTab, ReactNode> = {
    consoles: <>
      <h2>Consoles</h2><p className="lede">SSPI installs on the console you manage. Browsing and package links never need a connection.</p>
      <div className="orows one">
        <Row wide label="Find consoles on this network" detail="Scan this PC's network for consoles and receiver services.">
          <button type="button" className="btn sm" disabled={scanningNetwork || (!demo && !inTauri())} onClick={() => void findConsoles()}>
            {scanningNetwork ? <><span className="spinner" aria-hidden="true" />Scanning</> : <><Icon name="search" />Find consoles</>}
          </button>
        </Row>
      </div>
      {discoveryError && <p className="opt-msg" role="alert">{discoveryError}</p>}
      {discoveredConsoles?.length === 0 && <p className="opt-msg" role="status">No consoles answered. Check that the console is on and on the same network, then load the receiver.</p>}
      {!!discoveredConsoles?.length && <div className="orows one" aria-label="Discovered consoles">
        {discoveredConsoles.map(console => <Row key={`${console.host}-${console.label}`} label={console.host} detail={console.label}>
          {(console.platform === "ps5" || console.platform === "unknown") && <button type="button" className="btn sm ghost" onClick={() => useDiscoveredConsole(console, "ps5")}>Use for PS5</button>}
          {(console.platform === "ps4" || console.platform === "unknown") && <button type="button" className="btn sm ghost" onClick={() => useDiscoveredConsole(console, "ps4")}>Use for PS4</button>}
        </Row>)}
      </div>}
      <div className="opt-section">PS5 receiver</div>
      <div className="orows">
        <Row label="Address" detail="IPv4 address or hostname of your PS5"><Field label="PS5 address" value={draft.ps5Host} onChange={value => patch({ ps5Host: value })} placeholder="IP address or hostname" icon="monitor" /></Row>
        <Row label="Receiver port" detail="Must match the receiver on the PS5"><Field label="PS5 receiver port" type="number" width={110} value={draft.ps5Port} onChange={value => patch({ ps5Port: Number(value) })} /></Row>
        <Row label="ELF loader port" detail="Payloads and the receiver are sent here. The etaHEN and elfldr loaders listen on 9021."><Field label="PS5 ELF loader port" type="number" width={110} value={draft.ps5LoaderPort ?? 9021} onChange={value => { const port = Number(value); if (Number.isFinite(port)) patch({ ps5LoaderPort: Math.min(65535, Math.max(1, Math.trunc(port))) }) }} /></Row>
        <Row wide label="Receiver" detail={ps5Message || "The SSPI receiver shows artwork and progress on the PS5. Reload it after updating the app or changing the port."} tone={ps5Message ? "good" : undefined}>
          <TestButton busy={busy === "ps5"} ok={flash === "ps5"} onClick={() => void testPs5()} label="Test receiver" disabled={disabled} />
          <button type="button" className="btn ghost sm" disabled={payloadBusy || disabled} onClick={() => void downloadReceiver()}>{payloadBusy ? <span className="spinner" /> : <Icon name="download" />}Download receiver ELF</button>
        </Row>
      </div>
      <div className="opt-section">PS4</div>
      <div className="orows">
        <Row label="Address" detail="IPv4 address or hostname of your PS4"><Field label="PS4 address" value={draft.ps4Host} onChange={value => patch({ ps4Host: value })} placeholder="IP address or hostname" icon="monitor" /></Row>
        <Row label="Delivery method" detail={draft.ps4Transport === "receiver" ? "The SSPI receiver payload, loaded through GoldHEN" : "SSPI's pkg-rars inbox over FTP"}>
          <Seg label="PS4 delivery method" value={draft.ps4Transport} options={[["receiver", "Receiver payload"], ["inbox", "SSPI inbox"]]} onChange={value => patch({ ps4Transport: value })} />
        </Row>
        {draft.ps4Transport === "receiver" ? <>
          <Row label="Receiver port" detail="9114 unless you changed it on the PS4"><Field label="PS4 receiver port" type="number" width={110} value={draft.ps4ReceiverPort} onChange={value => patch({ ps4ReceiverPort: Number(value) })} /></Row>
          <Row label="GoldHEN BinLoader port" detail="9090 by default"><Field label="BinLoader port" type="number" width={110} value={draft.ps4LoaderPort} onChange={value => patch({ ps4LoaderPort: Number(value) })} /></Row>
          <Row label="PC download port" detail="The PS4 downloads base games and updates from this PC. Allow SSPI through Windows Firewall when asked."><Field label="PC download port" type="number" width={110} value={draft.ps4ServePort} onChange={value => patch({ ps4ServePort: Number(value) })} /></Row>
          <Row wide label="Receiver" detail="Load it again after each PS4 restart. GoldHEN's “Payload received” message alone doesn't confirm that it started.">
            <button type="button" className="btn sm" disabled={!!busy || disabled} onClick={() => void loadPs4Receiver()}>{busy === "ps4-load" ? <span className="spinner" /> : <Icon name="upload" />}Load receiver on PS4</button>
            <TestButton busy={busy === "ps4-test"} ok={flash === "ps4-test"} onClick={() => void testPs4Receiver()} label="Test receiver" disabled={disabled} />
            <button type="button" className="btn ghost sm" disabled={!!busy || disabled} onClick={() => void exportPs4()}><Icon name="download" />Download PS4 payload</button>
          </Row>
          <p className="opt-note">Needs GoldHEN 2.4b18.5 or newer with BinLoader enabled. If sending fails while BinLoader is on, turn it off and on again. SSPI on the PS4 isn't required for this method.</p>
        </> : <>
          <Row label="FTP port" detail="GoldHEN's FTP server, 2121 by default"><Field label="FTP port" type="number" width={110} value={draft.ps4FtpPort} onChange={value => patch({ ps4FtpPort: Number(value) })} /></Row>
          <Row label="FTP user" detail="Optional"><Field label="FTP user" value={draft.ps4FtpUser} onChange={value => patch({ ps4FtpUser: value })} placeholder="anonymous" width={180} /></Row>
          <Row label="FTP password" detail={draft.ps4FtpPasswordConfigured ? "A password is saved. Leave blank to keep it." : "Optional. Leave blank for anonymous."}><Field label="FTP password" type="password" value={ps4FtpPassword} onChange={setPs4FtpPassword} width={180} icon="lock" /></Row>
          <Row label="Archives for PS4" detail="Where RAR sets are unpacked">
            <select className="select" value={draft.ps4ArchiveMode} onChange={event => patch({ ps4ArchiveMode: event.target.value as Settings["ps4ArchiveMode"] })}>
              <option value="pc">Extract on this PC (recommended)</option>
              <option value="ps4">Send RAR sets to the PS4</option>
            </select>
          </Row>
          <Row label="Remove uploads after a confirmed install" detail="Deletes the copy in pkg-rars only after SSPI confirms the installation"><Switch label="Remove uploads after install" checked={draft.ps4RemoveAfterInstall} onChange={value => patch({ ps4RemoveAfterInstall: value })} /></Row>
          <Row label="Connection" detail="SSPI's background worker installs uploads even when the SSPI app is closed">
            <TestButton busy={busy === "ps4"} ok={flash === "ps4"} onClick={() => void testPs4Ftp()} label="Test PS4" disabled={disabled} />
          </Row>
          <p className="opt-note">Enable GoldHEN's FTP server. PS5 games and game folders can't be sent to a PS4.</p>
        </>}
        {ps4Message && <p className="opt-msg" role="status">{ps4Message}</p>}
      </div>
    </>,
    sources: <>
      <h2>Sources</h2><p className="lede">Package sources list the titles and packages you can search. Install one from a URL or a .gssource file.</p>
      <div className="orows">
        <Row label="Install from a URL" detail="A .gssource link">
          <Field label="Source URL" value={sourceUrl} onChange={setSourceUrl} placeholder="https://…/source.gssource" icon="link" width={280} />
          <button type="button" className="btn sm" disabled={!sourceUrl.trim() || !!busy || disabled} onClick={() => void installSource()}>{busy === "source-install" ? <span className="spinner" /> : <Icon name="download" />}Install</button>
        </Row>
        <Row label="Install from a file" detail="A .gssource or .zip on this PC">
          <button type="button" className="btn sm" disabled={!!busy || disabled} onClick={() => void browseSource()}>{busy === "source-browse" ? <span className="spinner" /> : <Icon name="folder" />}Browse</button>
        </Row>
      </div>
      <div className="opt-section">Installed sources</div>
      <div className="orows one">
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
        {!sources.length && <p className="opt-note">No package sources are installed. Install one from a URL or a file to start searching.</p>}
      </div>
    </>,
    debrid: <>
      <h2>Debrid</h2><p className="lede">Download services resolve links from supported hosts. Turn on more than one to fall back when a host isn't supported.</p>
      {([
        { id: "real-debrid", name: "Real-Debrid", enabled: draft.realDebridEnabled, configured: draft.realDebridConfigured, token: rdToken, setToken: setRdToken, setEnabled: (value: boolean) => patch({ realDebridEnabled: value }) },
        { id: "torbox", name: "TorBox", enabled: draft.torboxEnabled, configured: draft.torboxConfigured, token: torboxToken, setToken: setTorboxToken, setEnabled: (value: boolean) => patch({ torboxEnabled: value }) },
        { id: "alldebrid", name: "AllDebrid", enabled: draft.alldebridEnabled, configured: draft.alldebridConfigured, token: alldebridToken, setToken: setAlldebridToken, setEnabled: (value: boolean) => patch({ alldebridEnabled: value }) },
      ]).map(service => (
        <div key={service.id}>
          <div className="opt-section"><span className={`dot ${service.enabled && service.configured ? "good" : ""}`} />{service.name}</div>
          <div className="orows">
            <Row label={`Use ${service.name}`} detail={service.configured ? "An API key is saved" : "No API key saved"} tone={service.configured ? "good" : undefined}><Switch label={`Use ${service.name}`} checked={service.enabled} onChange={service.setEnabled} /></Row>
            <Row label="API key" detail={service.configured ? "Leave blank to keep the saved key" : "Paste the key from your account page"}>
              <Field label={`${service.name} API key`} type="password" value={service.token} onChange={service.setToken} placeholder={service.configured ? "Saved" : "API key"} icon="key" width={220} />
              <TestButton busy={busy === service.id} ok={flash === service.id} onClick={() => void verify(service.id, service.token)} label="Verify" okLabel="Verified" icon="shield" disabled={disabled} />
            </Row>
          </div>
        </div>
      ))}
      <p className="opt-note" style={{ marginTop: 14 }}>Keys stay in Windows Credential Manager. Save to apply changes and refresh host support.</p>
    </>,
    downloads: <>
      <h2>Downloads</h2><p className="lede">Where files land on this PC, what is kept afterwards, and how fast packages go to the PS5.</p>
      <div className="orows">
        <Row wide label="Download folder" detail="Archives, extracted games and packages are staged here. Choose a drive other than the Windows drive for large games.">
          <Field label="Download folder" value={draft.downloadDir} onChange={value => patch({ downloadDir: value })} icon="folder" width={340} />
          <button type="button" className="btn sm" disabled={disabled} onClick={() => void browseFolder()}>Browse</button>
        </Row>
        <Row label="Keep downloaded archives" detail="RAR, ZIP and multipart downloads after extraction"><Switch label="Keep downloaded archives" checked={draft.keepArchives} onChange={value => patch({ keepArchives: value })} /></Row>
        <Row label="Keep extracted game files" detail="Base and backport folders after packaging"><Switch label="Keep extracted game files" checked={draft.keepExtractions} onChange={value => patch({ keepExtractions: value })} /></Row>
        <Row label="Keep packaged PKGs" detail="After a confirmed install. Failed installs and package-only jobs always keep theirs."><Switch label="Keep packaged PKGs" checked={draft.keepPackages} onChange={value => patch({ keepPackages: value })} /></Row>
        <p className="opt-note">These apply when a transfer starts or is retried. Keeping files uses more disk space. Imported originals are never removed.</p>
      </div>
      <div className="opt-section">Upload to PS5</div>
      <div className="orows">
        <Row label="Max bandwidth" detail="Balanced uses 4 lanes; Max uses up to 12 on your local network"><Switch label="Max bandwidth" checked={(draft.transferMode || "balanced") === "max"} onChange={value => patch({ transferMode: value ? "max" : "balanced" })} /></Row>
        {(draft.transferMode || "balanced") === "max" && (
          <Row label="Upload lanes" detail="1 to 12"><Field label="Upload lanes" type="number" width={90} value={draft.uploadLanes || 4} onChange={value => patch({ uploadLanes: Math.min(12, Math.max(1, Number(value) || 4)) })} /></Row>
        )}
        <p className="opt-note">Max bandwidth needs the current receiver ELF. If installs start failing, go back to Balanced. Downloads stay one stream per volume.</p>
      </div>
    </>,
    packaging: <>
      <h2>Packaging</h2><p className="lede">Package extracted game dumps into a single FPKG before they go to the console.</p>
      <div className="orows">
        <Row label="Package extracted dumps" detail="Download and extract dumps, then build a finalized PS5 package"><Switch label="Package extracted dumps" checked={draft.packageDumps} onChange={value => patch({ packageDumps: value })} /></Row>
        <Row label="Download and package only" detail="Save packages on this PC without installing. No console needed; finished packages are kept."><Switch label="Download and package only" checked={draft.downloadPackageOnly} onChange={value => patch({ downloadPackageOnly: value, ...(value ? { packageDumps: true } : {}) })} /></Row>
        {draft.packageDumps && <>
          <Row label="Kraken compression" detail="Higher levels spend more CPU time for smaller packages. It doesn't change launch compatibility.">
            <select className="select" value={level} onChange={event => patch({ fpkgCompressionLevel: Number(event.target.value) })}>
              {[1, 2, 3, 4, 5, 6, 7, 8, 9].map(value => <option key={value} value={value}>Level {value}{({ 1: ", fastest", 2: ", fast", 4: ", balanced", 7: ", stronger", 9: ", most effort" } as Record<number, string>)[value] || ""}</option>)}
            </select>
          </Row>
          <Row label="Dump doctor" detail="Checks modules and metadata, and repairs invalid or missing modules from validated backups in the workspace"><Switch label="Dump doctor" checked={!!draft.fpkgDoctor} onChange={value => patch({ fpkgDoctor: value })} /></Row>
          <Row label="Target console firmware" detail="Optional. Used to pick backports and gate PFS v3."><Field label="Target firmware" value={draft.targetFw} onChange={value => patch({ targetFw: value })} placeholder="For example 4.03" width={150} /></Row>
          <Row label="PFS v3" detail="Needs firmware 7.00 or newer. Unknown or older firmware uses PFS v2."><Switch label="PFS v3" checked={draft.fpkgPfsVersion === 3} onChange={value => patch({ fpkgPfsVersion: value ? 3 : 2 })} /></Row>
          <Row wide label="Packaging engine" detail="Found automatically when left blank"><Field label="Packaging engine path" value={draft.fpkgEnginePath} onChange={value => patch({ fpkgEnginePath: value })} placeholder="fpkg-cli.exe" icon="cpu" width={320} /></Row>
          <p className="opt-note">Packaging uses a private workspace with hard links for bulk files; originals and backport overlays are preserved. Backport libraries are included inside the package. Launching them needs compatible kstuff and PPR patches and a loader with installed-PKG backport support, such as ShadowMount Plus 1.7.</p>
        </>}
        <Row wide label="Inspect a dump" detail="Runs the dump doctor on a folder without packaging it">
          <button type="button" className={`btn sm ${flash === "doctor" ? "ok" : ""}`} disabled={!!busy || disabled} onClick={() => void inspectDump()}>{busy === "doctor" ? <span className="spinner" /> : <Icon name="shield" />}Inspect a dump</button>
        </Row>
        {doctor && <div className="opt-msg"><DoctorReportView report={doctor} /></div>}
      </div>
    </>,
    appearance: <AppearanceSection appearance={appearance} setAppearance={setAppearance} reduceMotion={draft.reduceMotion} setReduceMotion={value => patch({ reduceMotion: value })} />,
  }

  const hints: Hint[] = [
    { key: "Enter", label: "Select" },
    { key: "QE", glyph: "Q E", face: "neutral", label: "Sections", run: () => stepTab(1) },
    { key: "Save", glyph: "Ctrl S", face: "neutral", label: "Save and close", run: () => void save() },
    { key: "Escape", label: "Close", run: () => close() },
  ]
  return (
    <>
      <div className="options" role="dialog" aria-modal="true" aria-label="Options">
        <div className="opt-scrim" onClick={() => close()} />
        <form className="opt-panel" onSubmit={save}>
          <div className="opt-head">
            <span className="shoulder l" id="optQ" role="button" tabIndex={-1} aria-label="Previous section (Q)" onClick={() => stepTab(-1)}>Q</span>
            <div className="opt-tabs" role="tablist" ref={tabsRef}>
              <span className="opt-thumb" ref={thumbRef} />
              {TABS.map(([id, label]) => <button key={id} type="button" role="tab" className="opt-tab" data-tab={id} aria-selected={tab === id} onClick={() => setTab(id)}>{label}</button>)}
            </div>
            <span className="shoulder r" id="optE" role="button" tabIndex={-1} aria-label="Next section (E)" onClick={() => stepTab(1)}>E</span>
          </div>
          <div className="opt-body" ref={bodyRef}>
            {demo && (
              <div className="orows one" style={{ marginBottom: 14 }}>
                <Row label="Offline preview is on" detail="Titles, packages and transfers are simulated. Leave the preview to use your consoles and sources." tone="warn">
                  <button type="button" className="btn primary sm" onClick={onLeaveDemo}>Leave the preview</button>
                </Row>
              </div>
            )}
            <div key={tab} className="swap-fade">{sections[tab]}</div>
          </div>
          <div className="opt-foot">
            <span className="dirty">{demo ? "Settings can't be saved in the offline preview." : dirty ? "You have unsaved changes." : "Appearance changes apply right away."}</span>
            <div className="row">
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
      <div className="options-dock"><Dock context={<>Options<small>{TABS.find(([id]) => id === tab)?.[1]}</small></>} hints={hints} build={build} /></div>
    </>
  )
}

function AppearanceSection({ appearance, setAppearance, reduceMotion, setReduceMotion }: { appearance: Appearance; setAppearance: Props["setAppearance"]; reduceMotion: boolean; setReduceMotion: (value: boolean) => void }) {
  const tilesRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    tilesRef.current?.querySelectorAll<HTMLCanvasElement>("canvas[data-pattern]").forEach(canvas => patternPreview(canvas.dataset.pattern || "solid", appearance.accent, canvas))
  }, [appearance.accent])
  return <>
    <h2>Appearance</h2><p className="lede">Background patterns and accents from SSPI on PS4. Changes apply right away.</p>
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
    <div className="orows" style={{ marginTop: 12 }}>
      <Row label="Tint with game artwork" detail="The background picks up the colour of the selected game"><Switch label="Tint with game artwork" checked={appearance.gameTint} onChange={value => setAppearance(prev => ({ ...prev, gameTint: value }))} /></Row>
      <Row label="Reduced motion" detail="Static focus and immediate transitions. Saved with your settings."><Switch label="Reduced motion" checked={reduceMotion} onChange={setReduceMotion} /></Row>
      <Row label="Restore defaults" detail="Resets the background, accent, artwork tint and download cards">
        <button type="button" className="btn sm" onClick={event => setAppearance(prev => ({ ...prev, accent: DEFAULT_APPEARANCE.accent, pattern: DEFAULT_APPEARANCE.pattern, gameTint: DEFAULT_APPEARANCE.gameTint, cardStyle: DEFAULT_APPEARANCE.cardStyle, cardSize: DEFAULT_APPEARANCE.cardSize }), { x: event.clientX, y: event.clientY })}><Icon name="refresh" />Restore</button>
      </Row>
    </div>
    <div className="opt-section">Download cards</div>
    <div className="orows">
      <Row label="Card style" detail="Artwork blurs each game's art behind its card.">
        <Seg label="Download card style" value={appearance.cardStyle} options={[["art", "Artwork"], ["plain", "Plain"]]} onChange={cardStyle => setAppearance(prev => ({ ...prev, cardStyle }))} />
      </Row>
      <Row label="Card size" detail="Compact fits more transfers on screen.">
        <Seg label="Download card size" value={appearance.cardSize} options={[["large", "Large"], ["compact", "Compact"]]} onChange={cardSize => setAppearance(prev => ({ ...prev, cardSize }))} />
      </Row>
    </div>
  </>
}
