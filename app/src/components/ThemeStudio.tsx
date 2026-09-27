/* =====================================================================
   Theme studio: edit a console theme and watch a stock PS4 or PS5 home
   and Settings screen change as you go.
   - Game tiles (shape, border, debug label) are drawn into each game's
     icon and written through the receiver; originals are kept.
   - On a PS4, the home screen (wallpaper, looping particles, focus and
     text colours) and the system app icons install as a real system
     theme: SSPI draws the images, the app packs a theme PKG and the
     receiver installs, lists, applies and removes it.
   - The PS5 has no theme packages, so those settings stay in the preview.
   ===================================================================== */
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog"
import { useEffect, useMemo, useRef, useState } from "react"
import type { LibraryView } from "@/App"
import { applyConsoleTheme, buildPs4Theme, getTitleIcon, installLocalPackage, listConsoleThemes, loadThemeFile, removeConsoleTheme, restoreTitleIcon, saveThemeFile, setTitleIcon } from "@/lib/console-api"
import { demoLibrary } from "@/lib/console-demo"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe, ConsoleThemes } from "@/lib/console-types"
import { errorText, plural } from "@/lib/format"
import { fileToDataUrl, loadImage, pngBase64, thumbnail } from "@/lib/image"
import { isTyping, setPanelKeys } from "@/lib/keys"
import { installedVersion, type LibraryEntry } from "@/lib/library"
import { motionOK, onFrame } from "@/lib/motion"
import { DEFAULT_THEME, PRESETS, SYSTEM_ICONS, changesIcons, loadThemes, newTheme, parseTheme, renderThemedIcon, saveThemes, serializeTheme, type LabelField, type ParticleKind, type PreviewScreen, type SystemIcon, type SystemIconStyle, type ThemeMotion, type ThemeSpec, type TileShape } from "@/lib/theme"
import { MOTION, animates, buildThemeRequest, drawParticles, drawSystemIcon, loopMs, particleField, type BuildStep } from "@/lib/theme-package"
import type { ConsoleKind, Settings } from "@/types"
import { Row, Seg, Switch } from "./Controls"
import { ConsoleScreen, MockFrame, type MockTile } from "./ConsoleMock"
import { Icon } from "./Icon"
import type { OptionsTab } from "./OptionsOverlay"
import type { Hint } from "./Shell"
import { toast } from "./toasts"

type Props = {
  target: ConsoleKind
  settings: Settings
  demo: boolean
  probe?: ConsoleProbe
  library: LibraryView
  onIconChanged: (target: ConsoleKind, titleId: string, icon: string | null, customIcon: boolean) => void
  onRefreshLibrary: (target: ConsoleKind) => void
  onLoadReceiver: (target: ConsoleKind) => void
  onOptions: (tab: OptionsTab) => void
  onHints: (hints: Hint[]) => void
}

type Run = { mode: "apply" | "restore"; done: number; total: number; failed: string[] }
type Pending = { contentId: string; title: string; previous: string; started: number }
const ACCENTS = ["#E4E4E1", "#7EB6FF", "#A78BFA", "#FF4D4D", "#F5A623", "#2EE6C5", "#F58BBF"]
const TEXT_COLOURS = ["#FFFFFF", "#E8EEFF", "#FFE9B8", "#0B0B0C"]
const INSTALL_WAIT_MS = 10 * 60 * 1000
const inTauri = () => "__TAURI_INTERNALS__" in window
const labelOf = (contentId: string) => contentId.slice(20)

export function ThemeStudio({ target, settings, demo, probe, library, onIconChanged, onRefreshLibrary, onLoadReceiver, onHints }: Props) {
  const name = target.toUpperCase()
  const ps4 = target === "ps4"
  const [themes, setThemes] = useState<ThemeSpec[]>(loadThemes)
  const [theme, setTheme] = useState<ThemeSpec>(() => loadThemes()[0] || newTheme(PRESETS[0], "My theme"))
  const [dirty, setDirty] = useState(false)
  const [screen, setScreen] = useState<PreviewScreen>("home")
  const [run, setRun] = useState<Run | null>(null)
  const [confirm, setConfirm] = useState<"apply" | "restore" | null>(null)
  const [step, setStep] = useState<BuildStep | null>(null)
  const [pending, setPending] = useState<Pending | null>(null)
  const [installed, setInstalled] = useState<ConsoleThemes | null>(null)
  const [installedError, setInstalledError] = useState("")
  const [busyTheme, setBusyTheme] = useState("")
  const [removing, setRemoving] = useState("")
  const cancel = useRef(false)
  const wallpaperRef = useRef<HTMLInputElement>(null)
  const iconRef = useRef<HTMLInputElement>(null)
  const iconSlot = useRef<SystemIcon | null>(null)

  const edit = (change: (current: ThemeSpec) => ThemeSpec) => { setTheme(current => ({ ...change(current), updatedAt: Date.now() })); setDirty(true) }
  const tile = (patch: Partial<ThemeSpec["tile"]>) => edit(current => ({ ...current, tile: { ...current.tile, ...patch } }))
  const label = (patch: Partial<ThemeSpec["label"]>) => edit(current => ({ ...current, label: { ...current.label, ...patch } }))
  const home = (patch: Partial<ThemeSpec["home"]>) => edit(current => ({ ...current, home: { ...current.home, ...patch } }))
  const particles = (patch: Partial<ThemeSpec["home"]["particles"]>) => edit(current => ({ ...current, home: { ...current.home, particles: { ...current.home.particles, ...patch } } }))
  const pack = (patch: Partial<ThemeSpec["ps4"]>) => edit(current => ({ ...current, ps4: { ...current.ps4, ...patch } }))

  /* ---------------------------------------------------------------- saved themes */
  const persist = (next: ThemeSpec[]) => {
    setThemes(next)
    if (!saveThemes(next)) toast({ tone: "warning", title: "The theme is too large to keep", text: "Local storage refused it, usually because of a large wallpaper or icon. Export it to a file instead." })
  }
  const saveCurrent = () => {
    persist([theme, ...themes.filter(item => item.id !== theme.id)])
    setDirty(false)
    toast({ tone: "success", title: "Theme saved", text: `${theme.name} is kept on this PC.`, duration: 2600 })
  }
  const choose = (next: ThemeSpec) => { setTheme(next); setDirty(false) }
  const remove = () => {
    const next = themes.filter(item => item.id !== theme.id)
    persist(next)
    choose(next[0] || newTheme(PRESETS[0], "My theme"))
  }
  const exportTheme = async () => {
    try {
      if (demo || !inTauri()) { toast({ tone: "info", title: "Offline preview", text: "Exporting writes a .sspitheme file in the SSPI app." }); return }
      const path = await saveDialog({ title: "Export theme", defaultPath: `${theme.name.replace(/[^\w\- ]+/g, "").trim() || "theme"}.sspitheme`, filters: [{ name: "SSPI theme", extensions: ["sspitheme"] }] })
      if (!path) return
      await saveThemeFile({ path, contents: serializeTheme(theme), demo })
      toast({ tone: "success", title: "Theme exported", text: path })
    } catch (error) { toast({ tone: "error", title: "The theme wasn't exported", text: errorText(error) }) }
  }
  const importTheme = async () => {
    try {
      if (demo || !inTauri()) { toast({ tone: "info", title: "Offline preview", text: "Importing reads a .sspitheme file in the SSPI app." }); return }
      const path = await openDialog({ title: "Import theme", multiple: false, filters: [{ name: "SSPI theme", extensions: ["sspitheme"] }] })
      if (typeof path !== "string") return
      const parsed = parseTheme(JSON.parse(await loadThemeFile({ path, demo })))
      if (!parsed) throw new Error("That file isn't an SSPI theme.")
      const imported = { ...parsed, id: newTheme(parsed).id, ps4: { ...parsed.ps4, label: "" } }
      persist([imported, ...themes])
      choose(imported)
      toast({ tone: "success", title: "Theme imported", text: imported.name })
    } catch (error) { toast({ tone: "error", title: "The theme wasn't imported", text: errorText(error) }) }
  }
  const readImage = async (file: File | undefined, limit: number, what: string) => {
    if (!file) return null
    if (!/^image\/(png|jpeg|webp)$/.test(file.type)) { toast({ tone: "warning", title: "Choose a PNG, JPEG or WebP image" }); return null }
    if (file.size > limit) { toast({ tone: "warning", title: `That image is larger than ${Math.round(limit / 1048576)} MB`, text: `The theme carries its ${what}, so it has to stay small.` }); return null }
    return fileToDataUrl(file)
  }
  const pickWallpaper = async (file?: File) => { const url = await readImage(file, 4 * 1048576, "wallpaper"); if (url) home({ wallpaper: url }) }
  const pickIcon = async (file?: File) => {
    const slot = iconSlot.current
    const url = await readImage(file, 1048576, "icons")
    if (url && slot) pack({ customIcons: { ...theme.ps4.customIcons, [slot]: url } })
  }
  const clearIcon = (slot: SystemIcon) => { const next = { ...theme.ps4.customIcons }; delete next[slot]; pack({ customIcons: next }) }

  /* ---------------------------------------------------------------- preview tiles */
  const previewEntries: LibraryEntry[] = useMemo(() => {
    const real = library.entries.filter(entry => entry.icon)
    if (real.length >= 3) return real.slice(0, 9)
    return demoLibrary(target).entries.slice(0, 9).map(item => ({ key: `demo:${item.titleId}`, titleId: item.titleId, name: item.name, icon: item.icon || undefined, target, baseVersion: item.baseVersion || undefined, updateVersion: item.updateVersion || undefined, requiredFirmware: item.requiredFirmware || undefined, installedAt: 0 }))
  }, [library.entries, target])
  const [images, setImages] = useState<Record<string, HTMLImageElement>>({})
  useEffect(() => {
    let live = true
    for (const entry of previewEntries) {
      const url = entry.icon
      if (!url || images[url]) continue
      void loadImage(url).then(image => { if (live) setImages(old => ({ ...old, [url]: image })) }).catch(() => undefined)
    }
    return () => { live = false }
  }, [previewEntries])
  const tiles = useMemo(() => previewEntries.map(entry => {
    const image = images[entry.icon || ""]
    const art = image ? renderThemedIcon(image, theme, { titleId: entry.titleId, version: installedVersion(entry), firmware: entry.requiredFirmware }, 256).toDataURL("image/png") : null
    return { key: entry.key, name: entry.name, art }
  }), [previewEntries, images, theme.tile, theme.label])

  // System icon art at preview size, for the icon grid and the home row.
  const [systemArt, setSystemArt] = useState<Partial<Record<SystemIcon, string>>>({})
  useEffect(() => {
    if (!ps4) return
    let live = true
    void (async () => {
      const next: Partial<Record<SystemIcon, string>> = {}
      for (const [slot] of SYSTEM_ICONS) {
        const url = theme.ps4.customIcons[slot]
        if (theme.ps4.icons === "stock" && !url) continue
        const custom = url ? await loadImage(url).catch(() => null) : null
        next[slot] = drawSystemIcon(slot, theme, custom, 160).toDataURL("image/png")
      }
      if (live) setSystemArt(next)
    })()
    return () => { live = false }
  }, [ps4, theme.ps4.icons, theme.ps4.customIcons, theme.tile, theme.home.accent])
  const systemTiles: MockTile[] = ps4 ? (["library", "tvvideo", "browser"] as SystemIcon[]).filter(slot => systemArt[slot]).map(slot => ({ key: `sys:${slot}`, name: SYSTEM_ICONS.find(([id]) => id === slot)![1], art: systemArt[slot]! })) : []

  /* ---------------------------------------------------------------- game tiles: apply and restore */
  const canWrite = demo || hasCapability(probe, "title-icons-v1")
  const eligible = library.authoritative || demo ? library.entries : []
  const customCount = eligible.filter(entry => entry.customIcon).length
  const iconChanges = changesIcons(theme)
  const start = async (mode: "apply" | "restore") => {
    setConfirm(null)
    const list = mode === "apply" ? eligible : eligible.filter(entry => entry.customIcon)
    if (!list.length) return
    const { host, port } = receiverEndpoint(settings, target)
    cancel.current = false
    const failed: string[] = []
    setRun({ mode, done: 0, total: list.length, failed })
    for (const [n, entry] of list.entries()) {
      if (cancel.current) break
      try {
        if (mode === "apply") {
          const original = await getTitleIcon({ target, host, port, titleId: entry.titleId, original: true, demo })
          const art = await loadImage(original)
          const canvas = renderThemedIcon(art, theme, { titleId: entry.titleId, version: installedVersion(entry), firmware: entry.requiredFirmware }, 512)
          await setTitleIcon({ target, host, port, titleId: entry.titleId, png: pngBase64(canvas), demo })
          onIconChanged(target, entry.titleId, thumbnail(canvas), true)
        } else {
          await restoreTitleIcon({ target, host, port, titleId: entry.titleId, demo })
          onIconChanged(target, entry.titleId, null, false)
        }
      } catch (error) {
        failed.push(`${entry.name}: ${errorText(error)}`)
      }
      setRun({ mode, done: n + 1, total: list.length, failed: [...failed] })
    }
    const stopped = cancel.current
    const count = list.length - failed.length
    toast({
      tone: failed.length ? (count ? "warning" : "error") : "success",
      title: stopped ? "Stopped" : failed.length ? `${count} of ${list.length} titles changed` : mode === "apply" ? "Game tiles changed" : "Original covers restored",
      text: `${failed.length ? `${failed.slice(0, 2).join(" ")} ` : ""}${demo ? "Offline preview: nothing was sent to the console." : `Your ${name} shows the new tiles after a restart.`}`,
    })
    if (mode === "restore" && !demo) onRefreshLibrary(target)
    setRun(null)
  }

  /* ---------------------------------------------------------------- PS4 theme packages */
  const canPackage = ps4 && (demo || hasCapability(probe, "theme-install-v1"))
  const canManage = ps4 && (demo || hasCapability(probe, "themes-v1"))
  const endpoint = () => receiverEndpoint(settings, "ps4")
  const refreshInstalled = async (quiet = true) => {
    if (!canManage) return null
    try {
      const { host, port } = endpoint()
      const list = await listConsoleThemes({ host, port, demo })
      setInstalled(list)
      setInstalledError("")
      return list
    } catch (error) {
      setInstalledError(errorText(error))
      if (!quiet) toast({ tone: "error", title: "Couldn't read your PS4's themes", text: errorText(error) })
      return null
    }
  }
  useEffect(() => { void refreshInstalled() }, [canManage, settings.ps4Host, demo])

  const rememberLabel = (label: string) => {
    setTheme(current => ({ ...current, ps4: { ...current.ps4, label } }))
    setThemes(list => {
      const next = list.map(item => item.id === theme.id ? { ...item, ps4: { ...item.ps4, label } } : item)
      saveThemes(next)
      return next
    })
  }
  const installTheme = async () => {
    if (step || pending) return
    try {
      setStep({ text: "Drawing the backgrounds", share: 0.02 })
      const request = await buildThemeRequest(theme, setStep)
      setStep({ text: request.animation ? "Compressing the animation and packing the theme" : "Packing the theme", share: 0.74 })
      const result = await buildPs4Theme({ request, demo })
      setStep({ text: `Sending ${result.title} to your PS4`, share: 0.94 })
      await installLocalPackage({ path: result.path, target: "ps4", title: result.title, demo })
      setPending({ contentId: result.contentId, title: result.title, previous: theme.ps4.label, started: Date.now() })
      toast({ tone: "info", title: `Installing ${result.title}`, text: "Follow it in Downloads. SSPI asks to apply it once it's on your PS4.", duration: 5200 })
    } catch (error) {
      toast({ tone: "error", title: "The theme wasn't installed", text: errorText(error) })
    } finally { setStep(null) }
  }
  const applyTheme = async (contentId: string, title: string, previous = "") => {
    setBusyTheme(contentId)
    try {
      const { host, port } = endpoint()
      await applyConsoleTheme({ host, port, contentId, demo })
      // The build this one replaces goes once the new one is selected.
      if (previous && previous !== labelOf(contentId)) {
        const list = await listConsoleThemes({ host, port, demo }).catch(() => null)
        if (list?.themes.some(item => labelOf(item.contentId) === previous)) {
          await removeConsoleTheme({ host, port, contentId: `UP9000-CUSA00000_00-${previous}`, demo }).catch(() => undefined)
        }
      }
      toast({ tone: "success", title: `${title} selected`, text: demo ? "Offline preview: nothing was sent to the console." : "Go back to your PS4's home screen to see it. If it hasn't changed, choose it in Settings > Themes." })
    } catch (error) {
      toast({ tone: "error", title: "The theme wasn't selected", text: `${errorText(error)} You can still choose it in Settings > Themes.` })
    } finally {
      setBusyTheme("")
      void refreshInstalled()
    }
  }
  const removeTheme = async (contentId: string, title: string) => {
    setRemoving("")
    setBusyTheme(contentId)
    try {
      const { host, port } = endpoint()
      await removeConsoleTheme({ host, port, contentId, demo })
      toast({ tone: "success", title: `${title} removed`, text: demo ? "Offline preview: nothing was sent to the console." : "It's no longer under Settings > Themes on your PS4.", duration: 3200 })
    } catch (error) {
      toast({ tone: "error", title: "The theme wasn't removed", text: errorText(error) })
    } finally {
      setBusyTheme("")
      void refreshInstalled()
    }
  }
  // Watch for the install to land, then offer to apply it.
  useEffect(() => {
    if (!pending) return
    let live = true
    const check = async () => {
      if (!live) return
      const list = await refreshInstalled()
      if (!live) return
      if (list?.themes.some(item => item.contentId === pending.contentId)) {
        setPending(null)
        rememberLabel(labelOf(pending.contentId))
        const previousInstalled = pending.previous && list.themes.some(item => labelOf(item.contentId) === pending.previous)
        const previousActive = previousInstalled && list.activeContentId && labelOf(list.activeContentId) === pending.previous
        if (previousInstalled && !previousActive) {
          const { host, port } = endpoint()
          await removeConsoleTheme({ host, port, contentId: `UP9000-CUSA00000_00-${pending.previous}`, demo }).catch(() => undefined)
          void refreshInstalled()
        }
        toast({
          tone: "success", title: `${pending.title} is on your PS4`,
          text: "Apply it now, or choose it later in Settings > Themes.",
          actions: [{ label: "Apply now", run: () => void applyTheme(pending.contentId, pending.title, previousActive ? pending.previous : "") }],
          duration: 12000,
        })
        return
      }
      if (Date.now() - pending.started > INSTALL_WAIT_MS) {
        setPending(null)
        toast({ tone: "warning", title: `${pending.title} hasn't appeared on your PS4 yet`, text: "Check Downloads for the install status. Once it's installed it appears under Settings > Themes." })
        return
      }
      window.setTimeout(() => void check(), 3000)
    }
    const first = window.setTimeout(() => void check(), 2500)
    return () => { live = false; window.clearTimeout(first) }
  }, [pending])

  /* ---------------------------------------------------------------- keys and dock */
  const live = useRef({ saveCurrent, screen })
  live.current = { saveCurrent, screen }
  useEffect(() => setPanelKeys(event => {
    if (isTyping()) return false
    const { saveCurrent, screen } = live.current
    if ((event.ctrlKey || event.metaKey) && (event.key === "s" || event.key === "S")) { event.preventDefault(); saveCurrent(); return true }
    if (!event.ctrlKey && !event.metaKey && !event.altKey && (event.key === "p" || event.key === "P")) { event.preventDefault(); setScreen(screen === "home" ? "settings" : "home"); return true }
    return false
  }), [])
  useEffect(() => {
    onHints([
      { key: "P", glyph: "P", face: "neutral", label: screen === "home" ? "Preview Settings" : "Preview home", run: () => setScreen(screen === "home" ? "settings" : "home") },
      { key: "S", glyph: "Ctrl S", face: "neutral", label: dirty ? "Save theme" : "Saved", run: saveCurrent },
    ])
  }, [dirty, screen, theme.id])

  /* ---------------------------------------------------------------- render */
  const tilesBlocked = !canWrite
    ? `Load ${probe?.receiver.expectedVersion ? `receiver ${probe.receiver.expectedVersion}` : "the latest receiver"} on your ${name} to change game tiles.`
    : !eligible.length ? `Refresh your library so SSPI knows which titles are installed on your ${name}.`
      : !iconChanges ? "Pick a shape, border or label to change game tiles." : ""
  const moving = animates(theme)
  const preset = MOTION[theme.ps4.motion === "smooth" ? "smooth" : "sharp"]
  const installMeta = !canPackage
    ? `Load receiver ${probe?.receiver.expectedVersion || "1.0.5"} on your PS4 to install themes.`
    : `Installs under Settings > Themes${moving ? `, with ${preset.frames} animated frames` : ""}.`
  const where = ps4 ? "In the PS4 theme" : "Preview only"
  return (
    <div className="ts">
      <div className="ts-stage">
        <div className="ts-stage-head">
          <Seg label="Preview screen" value={screen} options={[["home", "Home"], ["settings", "Settings"]]} onChange={setScreen} />
          <span className="ts-stage-note">{previewEntries[0]?.key.startsWith("demo:") ? "Sample titles" : `Titles on your ${name}`}{dirty ? ", not saved" : ""}</span>
        </div>
        <ConsolePreview target={target} screen={screen} theme={theme} tiles={tiles} systemTiles={systemTiles} />
        <div className="tray ts-tray">
          {step ? (
            <>
              <span className="tray-count">Building <b>{theme.name}</b></span>
              <span className="ts-progress"><i style={{ width: `${step.share * 100}%` }} /></span>
              <span className="tray-meta">{step.text}</span>
            </>
          ) : run ? (
            <>
              <span className="tray-count">{run.mode === "apply" ? "Changing tiles" : "Restoring"} <b>{run.done}</b> of {run.total}</span>
              <span className="ts-progress"><i style={{ width: `${(run.done / Math.max(1, run.total)) * 100}%` }} /></span>
              <span className="tray-meta">{run.failed.length ? `${run.failed.length} failed` : ""}</span>
              <button type="button" className="btn sm ghost" onClick={() => { cancel.current = true }}>Stop</button>
            </>
          ) : confirm ? (
            <>
              <span className="tray-count">{confirm === "apply" ? <>Change <b>{eligible.length}</b> game tiles on your {name}?</> : <>Restore <b>{customCount}</b> original {customCount === 1 ? "cover" : "covers"}?</>}</span>
              <span className="tray-meta">{confirm === "apply" ? "Each title's original icon is kept, so Restore can undo this." : "Titles go back to the icons they had before SSPI changed them."}</span>
              <button type="button" className="btn sm ghost" onClick={() => setConfirm(null)}>Back</button>
              <button type="button" className="btn sm primary" onClick={() => void start(confirm)}>{confirm === "apply" ? "Change tiles" : "Restore"}</button>
            </>
          ) : pending ? (
            <>
              <span className="tray-count">Installing <b>{pending.title}</b></span>
              <span className="tray-meta">Your PS4 is downloading it from this PC. Progress is in Downloads.</span>
            </>
          ) : (
            <>
              <span className="tray-count">{theme.name}</span>
              <span className="tray-meta">{ps4 ? installMeta : tilesBlocked || `Draws the tile style into ${plural(eligible.length, "game icon")}. Your ${name} shows them after a restart.`}</span>
              {((ps4 && !canPackage) || !canWrite) && !demo && probe?.host && <button type="button" className="btn sm" onClick={() => onLoadReceiver(target)}><Icon name="upload" />Load receiver</button>}
              {customCount > 0 && <button type="button" className="btn sm ghost" disabled={!canWrite} onClick={() => setConfirm("restore")}>Restore covers</button>}
              <button type="button" className={`btn sm ${ps4 ? "" : "primary"}`} disabled={!!tilesBlocked} title={tilesBlocked || undefined} onClick={() => setConfirm("apply")}>{ps4 ? "Change game tiles" : `Apply to your ${name}`}</button>
              {ps4 && <button type="button" className="btn sm primary" disabled={!canPackage} onClick={() => void installTheme()}><Icon name="download" />Install theme</button>}
            </>
          )}
        </div>
      </div>

      <div className="ts-controls scroll">
        <div className="ts-theme">
          <label className="field">
            <Icon name="palette" />
            <input value={theme.name} aria-label="Theme name" maxLength={60} spellCheck={false} onChange={event => edit(current => ({ ...current, name: event.target.value }))} />
          </label>
          <button type="button" className="btn sm" disabled={!dirty && themes.some(item => item.id === theme.id)} onClick={saveCurrent}>Save</button>
        </div>
        <div className="chips ts-saved" role="group" aria-label="Saved themes">
          {themes.map(item => <button key={item.id} type="button" className="chip" aria-pressed={item.id === theme.id} onClick={() => choose(item)}>{item.name}</button>)}
          {PRESETS.map(preset => <button key={preset.id} type="button" className="chip ghost" onClick={() => choose(newTheme(preset))}>{preset.name}</button>)}
          <button type="button" className="chip ghost" onClick={() => choose(newTheme(DEFAULT_THEME, "My theme"))}>New</button>
        </div>
        <div className="ts-links">
          <button type="button" className="link" onClick={() => void importTheme()}>Import</button>
          <button type="button" className="link" onClick={() => void exportTheme()}>Export</button>
          {themes.some(item => item.id === theme.id) && <button type="button" className="link" onClick={remove}>Delete</button>}
        </div>

        <p className="ts-section">Home screen<span>{where}</span></p>
        <div className="orows one">
          <Row label="Wallpaper" detail={theme.home.wallpaper ? (ps4 ? "Also used, softened, behind Settings" : "Shown behind the preview") : ps4 ? "The stock PS4 blue until you choose one" : "A PNG, JPEG or WebP up to 4 MB"}>
            {theme.home.wallpaper && <button type="button" className="btn sm ghost" onClick={() => home({ wallpaper: null })}>Remove</button>}
            <button type="button" className="btn sm" onClick={() => wallpaperRef.current?.click()}><Icon name="image" />{theme.home.wallpaper ? "Change" : "Choose"}</button>
            <input ref={wallpaperRef} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={event => { void pickWallpaper(event.target.files?.[0]); event.target.value = "" }} />
          </Row>
          {theme.home.wallpaper && <Slider label="Blur" value={theme.home.blur} min={0} max={24} step={1} format={v => `${v} px`} onChange={blur => home({ blur })} />}
          <Slider label="Dim" value={theme.home.dim} min={0} max={0.9} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={dim => home({ dim })} />
          <Row label={ps4 ? "Focus colour" : "Accent"} className="col">
            <div className="ts-swatches" role="radiogroup" aria-label={ps4 ? "Focus colour" : "Accent"}>
              {ACCENTS.map(hex => <button key={hex} type="button" role="radio" aria-checked={theme.home.accent.toUpperCase() === hex} aria-label={hex} className="ts-swatch" style={{ background: hex }} onClick={() => home({ accent: hex })} />)}
              <ColorInput label="Custom colour" value={theme.home.accent} onChange={accent => home({ accent })} />
            </div>
          </Row>
          {ps4 && (
            <Row label="Text colour" className="col">
              <div className="ts-swatches" role="radiogroup" aria-label="Text colour">
                {TEXT_COLOURS.map(hex => <button key={hex} type="button" role="radio" aria-checked={theme.ps4.text.toUpperCase() === hex} aria-label={hex} className="ts-swatch" style={{ background: hex }} onClick={() => pack({ text: hex })} />)}
              </div>
            </Row>
          )}
        </div>

        <p className="ts-section">Animated background<span>{where}</span></p>
        <div className="orows one">
          <Row label="Particles" className="col"><Seg label="Particles" value={theme.home.particles.kind} options={[["none", "None"], ["stars", "Stars"], ["snow", "Snow"], ["bubbles", "Bubbles"]] as Array<[ParticleKind, string]>} onChange={kind => particles({ kind })} /></Row>
          {theme.home.particles.kind !== "none" && (
            <>
              <Row label="Show on" className="col" detail={ps4 ? "Settings gets a still copy; the console only animates the home screen" : undefined}>
                <div className="chips" role="group" aria-label="Screens with particles">
                  {([["home", "Home"], ["settings", "Settings"]] as Array<[PreviewScreen, string]>).map(([id, text]) => (
                    <button key={id} type="button" className="chip" aria-pressed={theme.home.particles.screens.includes(id)} onClick={() => particles({ screens: theme.home.particles.screens.includes(id) ? theme.home.particles.screens.filter(s => s !== id) : [...theme.home.particles.screens, id] })}>{text}</button>
                  ))}
                </div>
              </Row>
              <Slider label="Amount" value={theme.home.particles.density} min={0.05} max={1} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={density => particles({ density })} />
              <Slider label="Speed" value={theme.home.particles.speed} min={0} max={1} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={speed => particles({ speed })} />
              <Row label="Particle colour"><ColorInput label="Particle colour" value={theme.home.particles.color} onChange={color => particles({ color })} /></Row>
              {ps4 && (
                <Row label="Motion" className="col" detail={theme.ps4.motion === "still" ? "Particles are drawn into a still wallpaper" : `${MOTION[theme.ps4.motion].frames} frames at ${MOTION[theme.ps4.motion].width} × ${MOTION[theme.ps4.motion].height}, looping every ${(loopMs(theme.ps4.motion) / 1000).toFixed(1)} s`}>
                  <Seg label="Motion" value={theme.ps4.motion} options={[["still", "Still"], ["sharp", "Sharp"], ["smooth", "Smooth"]] as Array<[ThemeMotion, string]>} onChange={motion => pack({ motion })} />
                </Row>
              )}
            </>
          )}
        </div>

        {ps4 && (
          <>
            <p className="ts-section">System icons<span>In the PS4 theme</span></p>
            <div className="orows one">
              <Row label="Style" className="col" detail={theme.ps4.icons === "stock" ? "Library, TV & Video, Internet Browser and the rest keep their stock look" : "Drawn in your tile shape; choose any icon to use your own image"}>
                <Seg label="System icon style" value={theme.ps4.icons} options={[["stock", "Stock"], ["glass", "Glass"], ["solid", "Solid"]] as Array<[SystemIconStyle, string]>} onChange={icons => pack({ icons })} />
              </Row>
              <div className="orow col ts-icons-row">
                <div className="ts-icons" role="group" aria-label="System icons">
                  {SYSTEM_ICONS.map(([slot, title]) => (
                    <div key={slot} className="ts-icon">
                      <button type="button" className="ts-icon-art" title={`Choose an image for ${title}`} aria-label={`Choose an image for ${title}`} onClick={() => { iconSlot.current = slot; iconRef.current?.click() }}>
                        {systemArt[slot] ? <img src={systemArt[slot]} alt="" draggable={false} /> : <span>Stock</span>}
                      </button>
                      <span className="ts-icon-name">{title}</span>
                      {theme.ps4.customIcons[slot] && <button type="button" className="link ts-icon-reset" onClick={() => clearIcon(slot)}>Reset</button>}
                    </div>
                  ))}
                </div>
                <input ref={iconRef} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={event => { void pickIcon(event.target.files?.[0]); event.target.value = "" }} />
              </div>
            </div>
          </>
        )}

        <p className="ts-section">Game tiles<span>Written to each game</span></p>
        <div className="orows one">
          <Row label="Shape" className="col"><Seg label="Tile shape" value={theme.tile.shape} options={[["square", "Square"], ["rounded", "Rounded"], ["squircle", "Squircle"], ["circle", "Round"]] as Array<[TileShape, string]>} onChange={shape => tile({ shape })} /></Row>
          {theme.tile.shape === "rounded" && <Slider label="Corner radius" value={theme.tile.radius} min={0.02} max={0.5} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={radius => tile({ radius })} />}
          <Slider label="Space around the art" value={theme.tile.inset} min={0} max={0.2} step={0.005} format={v => `${Math.round(v * 100)}%`} onChange={inset => tile({ inset })} />
          <Slider label="Border" value={theme.tile.border} min={0} max={24} step={1} format={v => v ? `${v} px` : "None"} onChange={border => tile({ border })} />
          {theme.tile.shape !== "square" && (
            <Row label="Corners" detail={theme.tile.clear ? "Left transparent so the wallpaper shows through" : "Filled with the colour behind the tile"}>
              <Seg label="Corners" value={theme.tile.clear ? "clear" : "fill"} options={[["clear", "Transparent"], ["fill", "Filled"]]} onChange={value => tile({ clear: value === "clear" })} />
            </Row>
          )}
          <Row label="Colours" detail={theme.tile.clear && theme.tile.shape !== "square" ? "Border" : "Border, and the area outside the shape"}>
            <ColorInput label="Border colour" value={theme.tile.borderColor} disabled={!theme.tile.border} onChange={borderColor => tile({ borderColor })} />
            {!(theme.tile.clear && theme.tile.shape !== "square") && <ColorInput label="Colour behind the tile" value={theme.tile.fill} onChange={fill => tile({ fill })} />}
          </Row>
        </div>

        <p className="ts-section">Debug label<span>Written to each game</span></p>
        <div className="orows one">
          <Row label="Show on every tile" detail="Drawn with each title's version when you change the tiles">
            <Switch label="Show a debug label on every tile" checked={theme.label.enabled} onChange={enabled => label({ enabled })} />
          </Row>
          {theme.label.enabled && (
            <Row label="Label" className="col">
              <div className="chips" role="group" aria-label="Label fields">
                {([["titleId", "Title ID"], ["version", "Version"], ["firmware", "Firmware"]] as Array<[LabelField, string]>).map(([id, text]) => (
                  <button key={id} type="button" className="chip" aria-pressed={theme.label.fields.includes(id)} onClick={() => label({ fields: theme.label.fields.includes(id) ? theme.label.fields.filter(f => f !== id) : [...theme.label.fields, id] })}>{text}</button>
                ))}
              </div>
              <div className="ts-pair">
                <Seg label="Label position" value={theme.label.position} options={[["top", "Top"], ["bottom", "Bottom"]]} onChange={position => label({ position })} />
                <Seg label="Label style" value={theme.label.style} options={[["bar", "Bar"], ["pill", "Pill"]]} onChange={style => label({ style })} />
              </div>
            </Row>
          )}
        </div>

        {ps4 && (
          <>
            <p className="ts-section">Themes on your PS4<span>{canManage ? <button type="button" className="link" onClick={() => void refreshInstalled(false)}>Refresh</button> : "Needs receiver 1.0.5"}</span></p>
            <div className="ts-installed">
              {!canManage ? (
                <p className="ts-empty">Load receiver {probe?.receiver.expectedVersion || "1.0.5"} to see, apply and remove themes from here.</p>
              ) : installedError ? (
                <p className="ts-empty error">{installedError}</p>
              ) : !installed ? (
                <p className="ts-empty">Reading your PS4's themes…</p>
              ) : !installed.themes.length ? (
                <p className="ts-empty">No custom themes yet. Install one and it shows here.</p>
              ) : installed.themes.map(item => {
                const active = installed.activeContentId === item.contentId
                const busy = busyTheme === item.contentId
                return (
                  <div key={item.contentId} className={`tsi ${active ? "active" : ""}`}>
                    <span className="tsi-name"><strong>{item.title}</strong>{active && <em>In use</em>}{labelOf(item.contentId) === theme.ps4.label && !active && <em className="mine">This theme</em>}</span>
                    {removing === item.contentId ? (
                      <>
                        <button type="button" className="btn sm ghost" onClick={() => setRemoving("")}>Keep</button>
                        <button type="button" className="btn sm confirm" onClick={() => void removeTheme(item.contentId, item.title)}>Remove</button>
                      </>
                    ) : (
                      <>
                        {!active && <button type="button" className="btn sm" disabled={!!busyTheme} onClick={() => void applyTheme(item.contentId, item.title)}>{busy ? "Applying…" : "Apply"}</button>}
                        <button type="button" className="btn sm ghost icon" disabled={!!busyTheme} aria-label={`Remove ${item.title}`} title="Remove" onClick={() => setRemoving(item.contentId)}><Icon name="trash" /></button>
                      </>
                    )}
                  </div>
                )
              })}
            </div>
          </>
        )}
        <p className="ts-note">
          {ps4
            ? "Wallpaper, particles, colours and system icons install as a PS4 theme. Game tiles are written to each game's icon and show after a restart. A custom theme may fall back to the stock look after a restart until GoldHEN is running again."
            : `Your ${name} doesn't accept wallpapers, particles or accents. They stay in the preview and in exported theme files.`}
        </p>
      </div>
    </div>
  )
}

/* ---------------------------------------------------------------- small controls */
function Slider({ label, value, min, max, step, format, onChange }: { label: string; value: number; min: number; max: number; step: number; format: (value: number) => string; onChange: (value: number) => void }) {
  return (
    <label className="orow col ts-slider">
      <span className="ts-slider-top"><strong>{label}</strong><output>{format(value)}</output></span>
      <input type="range" min={min} max={max} step={step} value={value} onChange={event => onChange(Number(event.target.value))} style={{ ["--fill" as string]: `${((value - min) / (max - min)) * 100}%` }} />
    </label>
  )
}

function ColorInput({ label, value, disabled, onChange }: { label: string; value: string; disabled?: boolean; onChange: (value: string) => void }) {
  return <input className="ts-color" type="color" value={value} disabled={disabled} aria-label={label} title={label} onChange={event => onChange(event.target.value)} />
}

/* ---------------------------------------------------------------- the console preview (stock screens in ConsoleMock) */
function ConsolePreview({ target, screen, theme, tiles, systemTiles }: { target: ConsoleKind; screen: PreviewScreen; theme: ThemeSpec; tiles: MockTile[]; systemTiles: MockTile[] }) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const ps4 = target === "ps4"
  const showParticles = theme.home.particles.kind !== "none" && theme.home.particles.screens.includes(screen)
  // On a PS4 only the home screen animates; Settings shows the still copy the theme carries.
  const still = ps4 && (screen === "settings" || theme.ps4.motion === "still")
  useParticles(canvasRef, theme, showParticles, still ? (screen === "settings" ? 0.37 : 0) : null)
  const hero = target === "ps5" && screen === "home" ? tiles[0]?.art : null
  const softened = ps4 && screen === "settings"
  const vars = ps4 ? { ["--cm-focus" as string]: theme.home.accent, ["--cm-text" as string]: theme.ps4.text } : undefined
  return (
    <div className="tsp-frame" style={vars}>
      <MockFrame className={`${target} ${screen}`}>
        <div className={`cm-bg ${target}`}>
          {theme.home.wallpaper
            ? <img src={theme.home.wallpaper} alt="" style={{ filter: theme.home.blur || softened ? `blur(${theme.home.blur * 2.4 + (softened ? 30 : 0)}px)` : undefined }} />
            : hero ? <span className="cm-hero" style={{ backgroundImage: `url(${hero})` }} /> : null}
        </div>
        <span className="cm-dim" style={{ opacity: Math.min(0.95, theme.home.dim * (theme.home.wallpaper ? 1 : 0.6) + (softened ? 0.12 : 0)) }} />
        <canvas ref={canvasRef} className="cm-particles" style={{ opacity: showParticles ? 1 : 0 }} />
        <ConsoleScreen target={target} screen={screen} tiles={tiles} systemTiles={systemTiles} />
      </MockFrame>
    </div>
  )
}

/** Draws the theme's particles with the exporter's own loop, so the preview is the animation the console gets. */
function useParticles(ref: React.RefObject<HTMLCanvasElement>, theme: ThemeSpec, active: boolean, stillAt: number | null) {
  const themeRef = useRef(theme)
  themeRef.current = theme
  useEffect(() => {
    const canvas = ref.current
    if (!canvas || !active) return
    const ctx = canvas.getContext("2d")!
    let key = "", field = particleField("none", 0), width = 0, height = 0
    const draw = (now: number) => {
      const spec = themeRef.current.home.particles
      const dpr = Math.min(2, window.devicePixelRatio || 1)
      const w = Math.round(canvas.clientWidth * dpr), h = Math.round(canvas.clientHeight * dpr)
      if (!w || !h) return
      if (w !== width || h !== height) { width = canvas.width = w; height = canvas.height = h }
      const next = `${spec.kind}:${spec.density}`
      if (next !== key) { key = next; field = particleField(spec.kind, spec.density) }
      ctx.clearRect(0, 0, w, h)
      const t = stillAt ?? (now / loopMs(themeRef.current.ps4.motion)) % 1
      drawParticles(ctx, w, h, spec, field, t)
    }
    if (stillAt !== null || !motionOK()) { draw(0); const timer = window.setInterval(() => draw(0), 400); return () => window.clearInterval(timer) }
    return onFrame((_dt, now) => draw(now))
  }, [active, stillAt])
}
