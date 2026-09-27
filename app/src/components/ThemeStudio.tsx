/* =====================================================================
   Theme studio: edit a console theme and watch a PS5 or PS4 home screen
   and Settings screen change as you go. Tile styles are drawn into each
   game's icon, so Apply writes real icons through the receiver (the
   originals are kept for Restore). Wallpaper, particles and accent stay
   in the preview, and the studio says so where they're edited.
   ===================================================================== */
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog"
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import type { LibraryView } from "@/App"
import { getTitleIcon, loadThemeFile, restoreTitleIcon, saveThemeFile, setTitleIcon } from "@/lib/console-api"
import { demoLibrary } from "@/lib/console-demo"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe } from "@/lib/console-types"
import { errorText, plural } from "@/lib/format"
import { fileToDataUrl, loadImage, pngBase64, thumbnail } from "@/lib/image"
import { isTyping, setPanelKeys } from "@/lib/keys"
import { installedVersion, type LibraryEntry } from "@/lib/library"
import { motionOK, onFrame } from "@/lib/motion"
import { DEFAULT_THEME, PRESETS, changesIcons, loadThemes, newTheme, parseTheme, renderThemedIcon, saveThemes, serializeTheme, type LabelField, type ParticleKind, type PreviewScreen, type ThemeSpec, type TileShape } from "@/lib/theme"
import type { ConsoleKind, Settings } from "@/types"
import { Row, Seg, Switch } from "./Controls"
import { ConsoleScreen, MockFrame } from "./ConsoleMock"
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
const ACCENTS = ["#E4E4E1", "#7EB6FF", "#A78BFA", "#FF4D4D", "#F5A623", "#2EE6C5", "#F58BBF"]
const inTauri = () => "__TAURI_INTERNALS__" in window

export function ThemeStudio({ target, settings, demo, probe, library, onIconChanged, onRefreshLibrary, onLoadReceiver, onHints }: Props) {
  const name = target.toUpperCase()
  const [themes, setThemes] = useState<ThemeSpec[]>(loadThemes)
  const [theme, setTheme] = useState<ThemeSpec>(() => loadThemes()[0] || newTheme(PRESETS[0], "My theme"))
  const [dirty, setDirty] = useState(false)
  const [screen, setScreen] = useState<PreviewScreen>("home")
  const [run, setRun] = useState<Run | null>(null)
  const [confirm, setConfirm] = useState<"apply" | "restore" | null>(null)
  const cancel = useRef(false)
  const wallpaperRef = useRef<HTMLInputElement>(null)

  const edit = (change: (current: ThemeSpec) => ThemeSpec) => { setTheme(current => ({ ...change(current), updatedAt: Date.now() })); setDirty(true) }
  const tile = (patch: Partial<ThemeSpec["tile"]>) => edit(current => ({ ...current, tile: { ...current.tile, ...patch } }))
  const label = (patch: Partial<ThemeSpec["label"]>) => edit(current => ({ ...current, label: { ...current.label, ...patch } }))
  const home = (patch: Partial<ThemeSpec["home"]>) => edit(current => ({ ...current, home: { ...current.home, ...patch } }))
  const particles = (patch: Partial<ThemeSpec["home"]["particles"]>) => edit(current => ({ ...current, home: { ...current.home, particles: { ...current.home.particles, ...patch } } }))

  /* ---------------------------------------------------------------- saved themes */
  const persist = (next: ThemeSpec[]) => {
    setThemes(next)
    if (!saveThemes(next)) toast({ tone: "warning", title: "The theme is too large to keep", text: "Local storage refused it, usually because of a large wallpaper. Export it to a file instead." })
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
      const imported = { ...parsed, id: newTheme(parsed).id }
      persist([imported, ...themes])
      choose(imported)
      toast({ tone: "success", title: "Theme imported", text: imported.name })
    } catch (error) { toast({ tone: "error", title: "The theme wasn't imported", text: errorText(error) }) }
  }
  const pickWallpaper = async (file?: File) => {
    if (!file) return
    if (!/^image\/(png|jpeg|webp)$/.test(file.type)) { toast({ tone: "warning", title: "Choose a PNG, JPEG or WebP image" }); return }
    if (file.size > 4 * 1024 * 1024) { toast({ tone: "warning", title: "That image is larger than 4 MB", text: "Themes carry their wallpaper, so it has to stay small." }); return }
    home({ wallpaper: await fileToDataUrl(file) })
  }

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
    const art = image ? renderThemedIcon(image, theme, { titleId: entry.titleId, version: installedVersion(entry), firmware: entry.requiredFirmware }, 256).toDataURL("image/jpeg", 0.9) : null
    return { key: entry.key, name: entry.name, art }
  }), [previewEntries, images, theme.tile, theme.label])

  /* ---------------------------------------------------------------- apply and restore */
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
      title: stopped ? "Stopped" : failed.length ? `${count} of ${list.length} titles changed` : mode === "apply" ? `${theme.name} applied` : "Original covers restored",
      text: `${failed.length ? `${failed.slice(0, 2).join(" ")} ` : ""}${demo ? "Offline preview: nothing was sent to the console." : `Your ${name} shows the new tiles after a restart.`}`,
    })
    if (mode === "restore" && !demo) onRefreshLibrary(target)
    setRun(null)
  }

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
  const applyBlocked = !canWrite
    ? `Load ${probe?.receiver.expectedVersion ? `receiver ${probe.receiver.expectedVersion}` : "the latest receiver"} on your ${name} to apply themes.`
    : !eligible.length ? `Refresh your library so SSPI knows which titles are installed on your ${name}.`
      : !iconChanges ? "This theme leaves tiles as they are. Pick a shape, border or label to apply it." : ""
  return (
    <div className="ts">
      <div className="ts-stage">
        <div className="ts-stage-head">
          <Seg label="Preview screen" value={screen} options={[["home", "Home"], ["settings", "Settings"]]} onChange={setScreen} />
          <span className="ts-stage-note">{previewEntries[0]?.key.startsWith("demo:") ? "Sample titles" : `Titles on your ${name}`}{dirty ? ", not saved" : ""}</span>
        </div>
        <ConsolePreview target={target} screen={screen} theme={theme} tiles={tiles} />
        <div className="tray ts-tray">
          {run ? (
            <>
              <span className="tray-count">{run.mode === "apply" ? "Applying" : "Restoring"} <b>{run.done}</b> of {run.total}</span>
              <span className="ts-progress"><i style={{ width: `${(run.done / Math.max(1, run.total)) * 100}%` }} /></span>
              <span className="tray-meta">{run.failed.length ? `${run.failed.length} failed` : ""}</span>
              <button type="button" className="btn sm ghost" onClick={() => { cancel.current = true }}>Stop</button>
            </>
          ) : confirm ? (
            <>
              <span className="tray-count">{confirm === "apply" ? <>Change <b>{eligible.length}</b> icons on your {name}?</> : <>Restore <b>{customCount}</b> original {customCount === 1 ? "cover" : "covers"}?</>}</span>
              <span className="tray-meta">{confirm === "apply" ? "Each title's original icon is kept, so Restore can undo this." : "Titles go back to the icons they had before SSPI changed them."}</span>
              <button type="button" className="btn sm ghost" onClick={() => setConfirm(null)}>Back</button>
              <button type="button" className="btn sm primary" onClick={() => void start(confirm)}>{confirm === "apply" ? "Apply" : "Restore"}</button>
            </>
          ) : (
            <>
              <span className="tray-count">{theme.name}</span>
              <span className={`tray-meta ${applyBlocked ? "" : ""}`}>{applyBlocked || `Draws the tile style into ${plural(eligible.length, "game icon")}. Your ${name} shows them after a restart.`}</span>
              {!canWrite && !demo && probe?.host && <button type="button" className="btn sm" onClick={() => onLoadReceiver(target)}><Icon name="upload" />Load receiver</button>}
              {customCount > 0 && <button type="button" className="btn sm ghost" disabled={!canWrite} onClick={() => setConfirm("restore")}>Restore originals</button>}
              <button type="button" className="btn sm primary" disabled={!!applyBlocked} onClick={() => setConfirm("apply")}>Apply to your {name}</button>
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

        <p className="ts-section">Game tiles<span>Changes your {name}</span></p>
        <div className="orows one">
          <Row label="Shape" className="col"><Seg label="Tile shape" value={theme.tile.shape} options={[["square", "Square"], ["rounded", "Rounded"], ["squircle", "Squircle"], ["circle", "Round"]] as Array<[TileShape, string]>} onChange={shape => tile({ shape })} /></Row>
          {theme.tile.shape === "rounded" && <Slider label="Corner radius" value={theme.tile.radius} min={0.02} max={0.5} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={radius => tile({ radius })} />}
          <Slider label="Space around the art" value={theme.tile.inset} min={0} max={0.2} step={0.005} format={v => `${Math.round(v * 100)}%`} onChange={inset => tile({ inset })} />
          <Slider label="Border" value={theme.tile.border} min={0} max={24} step={1} format={v => v ? `${v} px` : "None"} onChange={border => tile({ border })} />
          <Row label="Colours" detail="Border, and the area outside the shape">
            <ColorInput label="Border colour" value={theme.tile.borderColor} disabled={!theme.tile.border} onChange={borderColor => tile({ borderColor })} />
            <ColorInput label="Colour behind the tile" value={theme.tile.fill} onChange={fill => tile({ fill })} />
          </Row>
        </div>

        <p className="ts-section">Debug label<span>Changes your {name}</span></p>
        <div className="orows one">
          <Row label="Show on every tile" detail="Drawn with each title's version when you apply">
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

        <p className="ts-section">Home screen<span>Preview only</span></p>
        <div className="orows one">
          <Row label="Wallpaper" detail={theme.home.wallpaper ? "Shown behind the preview" : "A PNG, JPEG or WebP up to 4 MB"}>
            {theme.home.wallpaper && <button type="button" className="btn sm ghost" onClick={() => home({ wallpaper: null })}>Remove</button>}
            <button type="button" className="btn sm" onClick={() => wallpaperRef.current?.click()}><Icon name="image" />{theme.home.wallpaper ? "Change" : "Choose"}</button>
            <input ref={wallpaperRef} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={event => { void pickWallpaper(event.target.files?.[0]); event.target.value = "" }} />
          </Row>
          {theme.home.wallpaper && <Slider label="Blur" value={theme.home.blur} min={0} max={24} step={1} format={v => `${v} px`} onChange={blur => home({ blur })} />}
          <Slider label="Dim" value={theme.home.dim} min={0} max={0.9} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={dim => home({ dim })} />
          <Row label="Accent" className="col">
            <div className="ts-swatches" role="radiogroup" aria-label="Accent">
              {ACCENTS.map(hex => <button key={hex} type="button" role="radio" aria-checked={theme.home.accent.toUpperCase() === hex} aria-label={hex} className="ts-swatch" style={{ background: hex }} onClick={() => home({ accent: hex })} />)}
            </div>
          </Row>
          <Row label="Particles" className="col"><Seg label="Particles" value={theme.home.particles.kind} options={[["none", "None"], ["stars", "Stars"], ["snow", "Snow"], ["bubbles", "Bubbles"]] as Array<[ParticleKind, string]>} onChange={kind => particles({ kind })} /></Row>
          {theme.home.particles.kind !== "none" && (
            <>
              <Row label="Show on" className="col">
                <div className="chips" role="group" aria-label="Screens with particles">
                  {([["home", "Home"], ["settings", "Settings"]] as Array<[PreviewScreen, string]>).map(([id, text]) => (
                    <button key={id} type="button" className="chip" aria-pressed={theme.home.particles.screens.includes(id)} onClick={() => particles({ screens: theme.home.particles.screens.includes(id) ? theme.home.particles.screens.filter(s => s !== id) : [...theme.home.particles.screens, id] })}>{text}</button>
                  ))}
                </div>
              </Row>
              <Slider label="Amount" value={theme.home.particles.density} min={0.05} max={1} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={density => particles({ density })} />
              <Slider label="Speed" value={theme.home.particles.speed} min={0} max={1} step={0.01} format={v => `${Math.round(v * 100)}%`} onChange={speed => particles({ speed })} />
              <Row label="Particle colour"><ColorInput label="Particle colour" value={theme.home.particles.color} onChange={color => particles({ color })} /></Row>
            </>
          )}
        </div>
        <p className="ts-note">Your {name} doesn't accept wallpapers, particles or accents from the receiver. They stay in the preview and in exported theme files.</p>
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
function ConsolePreview({ target, screen, theme, tiles }: { target: ConsoleKind; screen: PreviewScreen; theme: ThemeSpec; tiles: Array<{ key: string; name: string; art: string | null }> }) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const showParticles = theme.home.particles.kind !== "none" && theme.home.particles.screens.includes(screen)
  useParticles(canvasRef, theme.home.particles, showParticles)
  const hero = target === "ps5" && screen === "home" ? tiles[0]?.art : null
  return (
    <div className="tsp-frame">
      <MockFrame className={`${target} ${screen}`}>
        <div className={`cm-bg ${target}`}>
          {theme.home.wallpaper
            ? <img src={theme.home.wallpaper} alt="" style={{ filter: theme.home.blur ? `blur(${theme.home.blur * 2.4}px)` : undefined }} />
            : hero ? <span className="cm-hero" style={{ backgroundImage: `url(${hero})` }} /> : null}
        </div>
        <span className="cm-dim" style={{ opacity: theme.home.dim * (theme.home.wallpaper ? 1 : 0.6) }} />
        <canvas ref={canvasRef} className="cm-particles" style={{ opacity: showParticles ? 1 : 0 }} />
        <ConsoleScreen target={target} screen={screen} tiles={tiles} />
      </MockFrame>
    </div>
  )
}

type Particle = { x: number; y: number; r: number; phase: number; v: number }
function useParticles(ref: React.RefObject<HTMLCanvasElement>, spec: ThemeSpec["home"]["particles"], active: boolean) {
  const specRef = useRef(spec)
  specRef.current = spec
  useEffect(() => {
    const canvas = ref.current
    if (!canvas || !active) return
    const ctx = canvas.getContext("2d")!
    let particles: Particle[] = []
    let kind: ParticleKind = "none", density = -1, width = 0, height = 0
    const seed = () => {
      const count = Math.round((kind === "bubbles" ? 40 : kind === "snow" ? 110 : 180) * specRef.current.density)
      particles = Array.from({ length: count }, () => ({ x: Math.random(), y: Math.random(), r: Math.random(), phase: Math.random() * Math.PI * 2, v: 0.4 + Math.random() * 0.6 }))
    }
    const draw = (dt: number, now: number) => {
      const s = specRef.current
      const dpr = Math.min(2, window.devicePixelRatio || 1)
      const w = Math.round(canvas.clientWidth * dpr), h = Math.round(canvas.clientHeight * dpr)
      if (w !== width || h !== height) { width = canvas.width = w; height = canvas.height = h }
      if (s.kind !== kind || s.density !== density) { kind = s.kind; density = s.density; seed() }
      ctx.clearRect(0, 0, w, h)
      ctx.fillStyle = s.color
      ctx.strokeStyle = s.color
      const speed = 0.15 + s.speed * 1.4
      const moving = motionOK()
      for (const p of particles) {
        if (kind === "stars") {
          if (moving) p.x = (p.x + dt * 0.004 * speed * p.v) % 1
          const twinkle = moving ? 0.35 + 0.65 * (0.5 + 0.5 * Math.sin(now * 0.0018 * speed * p.v + p.phase)) : 0.7
          ctx.globalAlpha = twinkle * (0.4 + p.r * 0.6)
          const size = (0.5 + p.r * 1.3) * dpr
          ctx.beginPath(); ctx.arc(p.x * w, p.y * h, size, 0, Math.PI * 2); ctx.fill()
          if (p.r > 0.93) { ctx.globalAlpha *= 0.5; ctx.fillRect(p.x * w - size * 3, p.y * h - 0.4 * dpr, size * 6, 0.8 * dpr); ctx.fillRect(p.x * w - 0.4 * dpr, p.y * h - size * 3, 0.8 * dpr, size * 6) }
        } else if (kind === "snow") {
          if (moving) { p.y += dt * 0.05 * speed * (0.4 + p.r); p.x += Math.sin(now * 0.001 + p.phase) * dt * 0.006; if (p.y > 1.02) { p.y = -0.02; p.x = Math.random() } }
          ctx.globalAlpha = 0.35 + p.r * 0.5
          ctx.beginPath(); ctx.arc(p.x * w, p.y * h, (0.8 + p.r * 2.2) * dpr, 0, Math.PI * 2); ctx.fill()
        } else if (kind === "bubbles") {
          if (moving) { p.y -= dt * 0.035 * speed * (0.5 + p.r); p.x += Math.sin(now * 0.0012 + p.phase) * dt * 0.004; if (p.y < -0.05) { p.y = 1.05; p.x = Math.random() } }
          ctx.globalAlpha = 0.25 + p.r * 0.3
          ctx.lineWidth = 1 * dpr
          ctx.beginPath(); ctx.arc(p.x * w, p.y * h, (3 + p.r * 9) * dpr, 0, Math.PI * 2); ctx.stroke()
        }
      }
      ctx.globalAlpha = 1
    }
    if (!motionOK()) { draw(0, 0); const t = window.setInterval(() => draw(0, 0), 500); return () => window.clearInterval(t) }
    return onFrame((dt, now) => draw(dt, now))
  }, [active])
}
