/* =====================================================================
   Themes (PS4) — a live PS4 screen beside three tabs:
   Look (wallpaper, colours, icon lines, tile shape, border, glass),
   Icons (every replaceable icon, one by one or as a pack) and Install
   (the system theme, plus game icons masked to the same shape).
   ===================================================================== */
import { invoke } from "@tauri-apps/api/core"
import { save } from "@tauri-apps/plugin-dialog"
import { useDeferredValue, useEffect, useMemo, useRef, useState } from "react"
import { Icon } from "../Icon"
import { Row, Seg, Switch } from "../Controls"
import { toast } from "../toasts"
import { IconsTab } from "./themes/IconsTab"
import { ShapeGlyph } from "./themes/ShapeGlyph"
import { Ps4Preview, SETTING_ART, type PreviewArt, type PreviewScreen } from "./themes/Ps4Preview"
import { applyConsoleTheme, buildPs4Theme, installLocalPackage, listConsoleLibrary, listConsoleThemes, removeConsoleTheme } from "@/lib/console-api"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe, ConsoleThemes, LibraryTitle } from "@/lib/console-types"
import { errorText, plural } from "@/lib/format"
import { fileToDataUrl, loadImage } from "@/lib/image"
import { ART, CONTENT_SLOTS, FUNCTION_SLOTS, iconPrompt, slotForFile, type ContentSlot, type FunctionSlot, type SlotId } from "@/lib/ps4-icons"
import {
  PRESETS, SHAPES, borderOf, buildThemeRequest, drawStockWallpaper, drawWallpaper, makeCanvas, normalizeImage, paintArt, renderContent, renderFunction,
  stockContent, stockFunction, type Fit, type ThemeImages, type ThemeSpec,
} from "@/lib/ps4-theme"
import { loadImages, loadSpec, putImage, saveSpec } from "@/lib/theme-store"
import { unzip, zipStore, type ZipEntry } from "@/lib/zip"
import type { Settings } from "@/types"

type Tab = "look" | "icons" | "install"
/** The last theme package built here, kept so it can be sent again after a PS4 restart. */
type SavedPackage = { contentId: string; path: string; title: string }
const PACKAGE_KEY = "sspi.ps4-theme.package"
const readPackage = (): SavedPackage | null => { try { return JSON.parse(window.localStorage.getItem(PACKAGE_KEY) || "null") } catch { return null } }
const writePackage = (value: SavedPackage) => { try { window.localStorage.setItem(PACKAGE_KEY, JSON.stringify(value)) } catch { /* the session keeps it */ } }
const TABS: Array<[Tab, string]> = [["look", "Look"], ["icons", "Icons"], ["install", "Install"]]
const SCREENS: Array<[PreviewScreen, string]> = [["home", "Home"], ["menu", "Top menu"], ["settings", "Settings"]]

const wallpaperUrl = (draw: (ctx: CanvasRenderingContext2D) => void) => { const c = makeCanvas(960, 540); draw(c.getContext("2d")!); return c.toDataURL("image/jpeg", 0.86) }
const glyphUrl = (slot: SlotId) => { const c = makeCanvas(96); paintArt(c.getContext("2d")!, ART[slot], "stock", ["#fff", "#fff"], 0, 0, 96); return c.toDataURL() }
const bytesOf = (dataUrl: string) => Uint8Array.from(atob(dataUrl.slice(dataUrl.indexOf(",") + 1)), c => c.charCodeAt(0))
function base64Of(bytes: Uint8Array) {
  let text = ""
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(text)
}
const blobUrl = (blob: Blob) => new Promise<string>((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.onerror = () => reject(reader.error); reader.readAsDataURL(blob) })
const MIME: Record<string, string> = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp" }

export function ThemesPanel({ settings, demo, probe }: { settings: Settings; demo: boolean; probe?: ConsoleProbe }) {
  const [spec, setSpecState] = useState<ThemeSpec>(loadSpec)
  const [urls, setUrls] = useState<Record<string, string>>({})
  const [images, setImages] = useState<ThemeImages>({})
  const [tab, setTab] = useState<Tab>("look")
  const [selected, setSelected] = useState<SlotId>("library")
  const [screen, setScreen] = useState<PreviewScreen>("home")
  const [themed, setThemed] = useState(true)
  const [marks, setMarks] = useState(false)
  const [titles, setTitles] = useState<LibraryTitle[] | null>(null)
  const [gameArt, setGameArt] = useState<Record<string, HTMLImageElement>>({})
  const [installed, setInstalled] = useState<ConsoleThemes | null>(null)
  const [step, setStep] = useState("")
  const [packing, setPacking] = useState(false)
  const [saved, setSaved] = useState<SavedPackage | null>(readPackage)
  const wallInput = useRef<HTMLInputElement>(null)
  const { host, port } = receiverEndpoint(settings, "ps4")
  const ready = demo || (!!host && probe?.receiver.state === "online")
  const canTheme = demo || hasCapability(probe, "theme-install-v1")
  const setSpec = (patch: Partial<ThemeSpec>) => setSpecState(old => { const next = { ...old, ...patch }; saveSpec(next); return next })

  /* ------------------------------------------------ stored images */
  useEffect(() => { void loadImages().then(setUrls) }, [])
  useEffect(() => {
    let live = true
    void Promise.all(Object.entries(urls).map(async ([key, url]) => [key, await loadImage(url).catch(() => null)] as const)).then(pairs => {
      if (live) setImages(Object.fromEntries(pairs.filter(([, image]) => image)) as ThemeImages)
    })
    return () => { live = false }
  }, [urls])
  const store = async (key: SlotId | "wallpaper", url: string | null) => {
    await putImage(key, url)
    setUrls(old => { const next = { ...old }; if (url) next[key] = url; else delete next[key]; return next })
  }

  /* ------------------------------------------------ the console */
  const loadConsole = async () => {
    if (!ready) return
    const [library, themes] = await Promise.allSettled([
      listConsoleLibrary({ target: "ps4", host, port, demo }),
      canTheme ? listConsoleThemes({ host, port, demo }) : Promise.resolve(null),
    ])
    if (library.status === "fulfilled") {
      const list = library.value.entries.filter(entry => entry.platform !== "ps5")
      setTitles(list)
      const loaded = await Promise.all(list.slice(0, 4).map(async entry => [entry.titleId, entry.icon ? await loadImage(entry.icon).catch(() => null) : null] as const))
      setGameArt(Object.fromEntries(loaded.filter(([, image]) => image)) as Record<string, HTMLImageElement>)
    } else setTitles([])
    if (themes.status === "fulfilled") setInstalled(themes.value)
  }
  useEffect(() => { void loadConsole() /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [host, port, demo, ready])

  /* ------------------------------------------------ preview art (deferred, so sliders stay smooth) */
  const view = useDeferredValue(spec)
  const system = useMemo(() => Object.fromEntries(SETTING_ART.map(slot => [slot, glyphUrl(slot)])), [])
  const stock = useMemo<PreviewArt>(() => {
    const wall = wallpaperUrl(ctx => drawStockWallpaper(ctx, 960, 540))
    return {
      content: Object.fromEntries(CONTENT_SLOTS.map(slot => [slot.id, stockContent(slot.id as ContentSlot, 256).toDataURL()])) as PreviewArt["content"],
      fn: Object.fromEntries(FUNCTION_SLOTS.map(slot => [slot.id, stockFunction(slot.id as FunctionSlot, 96).toDataURL()])) as PreviewArt["fn"],
      home: wall, menu: wall, system,
    }
  }, [system])
  const art = useMemo<PreviewArt>(() => ({
    content: Object.fromEntries(CONTENT_SLOTS.map(slot => [slot.id, renderContent(slot.id as ContentSlot, view, images[slot.id] ?? null, 256).toDataURL()])) as PreviewArt["content"],
    fn: Object.fromEntries(FUNCTION_SLOTS.map(slot => [slot.id, renderFunction(slot.id as FunctionSlot, view, images[slot.id] ?? null, 96).toDataURL()])) as PreviewArt["fn"],
    home: wallpaperUrl(ctx => drawWallpaper(ctx, 960, 540, view, images.wallpaper)),
    menu: wallpaperUrl(ctx => drawWallpaper(ctx, 960, 540, view, images.wallpaper, { blur: 28, dim: 0.16 })),
    system,
  }), [view, images, system])
  // Themes don't change game icons (Tools > Game icons does), so games show as they are.
  const games = useMemo(() => Object.entries(gameArt).map(([titleId, image]) => ({
    titleId, name: titles?.find(title => title.titleId === titleId)?.name || titleId, src: image.src,
  })), [gameArt, titles])
  const thumbs = { ...art.content, ...art.fn } as Record<SlotId, string>

  /* ------------------------------------------------ images in */
  const applyImage = async (key: SlotId | "wallpaper", url: string) => {
    const { url: normal, clear } = await normalizeImage(url, key)
    await store(key, normal)
    if (key === "wallpaper") setSpec({ wallpaper: "image" })
    // A glyph on a clear background sits best on the theme's tile; a full picture fills it.
    else if (CONTENT_SLOTS.some(slot => slot.id === key)) setSpecState(old => { const next = { ...old, fit: { ...old.fit, [key]: clear > 0.25 ? "glyph" : "fill" } as ThemeSpec["fit"] }; saveSpec(next); return next })
  }
  const replace = async (key: SlotId | "wallpaper", file: File) => {
    try { await applyImage(key, await fileToDataUrl(file)); if (key !== "wallpaper") toast({ tone: "success", title: "Icon replaced", text: file.name }) }
    catch (error) { toast({ tone: "error", title: "That image couldn't be used", text: errorText(error) }) }
  }
  const importFiles = async (files: File[]) => {
    const used: string[] = [], unknown: string[] = []
    const take = async (name: string, blob: Blob) => {
      const extension = name.split(".").pop()?.toLowerCase() || ""
      if (!MIME[extension]) return
      const key = slotForFile(name)
      if (!key) { unknown.push(name.split(/[\\/]/).pop() || name); return }
      await applyImage(key, await blobUrl(new Blob([blob], { type: MIME[extension] })))
      used.push(key)
    }
    setPacking(true)
    try {
      for (const file of files) {
        if (/\.zip$/i.test(file.name)) for (const entry of await unzip(new Uint8Array(await file.arrayBuffer()))) await take(entry.name, new Blob([entry.data as BlobPart]))
        else await take(file.name, file)
      }
      if (used.length) toast({ tone: unknown.length ? "warning" : "success", title: `Imported ${plural(used.length, "image")}`, text: unknown.length ? `No icon is named ${unknown.slice(0, 4).join(", ")}${unknown.length > 4 ? "…" : ""}.` : "The preview shows them now." })
      else toast({ tone: "warning", title: "Nothing was imported", text: "Name the files after their icons, like library.png or trophy.png." })
    } catch (error) { toast({ tone: "error", title: "The images weren't imported", text: errorText(error) }) }
    finally { setPacking(false) }
  }

  /* ------------------------------------------------ the pack out */
  const exportPack = async () => {
    setPacking(true)
    try {
      const entries: ZipEntry[] = []
      const readme = ["SSPI PS4 theme icon pack", "", "Edit or regenerate any of these files, keep the names, then use Import images in SSPI.", "Home screen tiles are 512 x 512 PNG; top menu icons are 128 x 128 PNG with a transparent background.", "The wallpaper is 1920 x 1080.", ""]
      for (const slot of CONTENT_SLOTS) {
        entries.push({ name: `${slot.id}.png`, data: bytesOf(renderContent(slot.id as ContentSlot, spec, images[slot.id] ?? null, 512).toDataURL("image/png")) })
        readme.push(`${slot.id}.png  ${slot.label}`, `  Prompt: ${iconPrompt(slot, spec, spec.glyph)}`, "")
      }
      for (const slot of FUNCTION_SLOTS) {
        entries.push({ name: `${slot.id}.png`, data: bytesOf(renderFunction(slot.id as FunctionSlot, spec, images[slot.id] ?? null, 128).toDataURL("image/png")) })
        readme.push(`${slot.id}.png  ${slot.label} (top menu)`, `  Prompt: ${iconPrompt(slot, spec, spec.glyph)}`, "")
      }
      const wall = makeCanvas(1920, 1080)
      drawWallpaper(wall.getContext("2d")!, 1920, 1080, { ...spec, dim: 0 }, images.wallpaper)
      entries.push({ name: "wallpaper.jpg", data: bytesOf(wall.toDataURL("image/jpeg", 0.92)) })
      entries.push({ name: "README.txt", data: new TextEncoder().encode(readme.join("\r\n")) })
      const zip = zipStore(entries)
      const name = `${(spec.name.trim() || "SSPI theme").replace(/[^\w -]+/g, "").trim() || "SSPI theme"} icons.zip`
      if (demo || !("__TAURI_INTERNALS__" in window)) {
        const link = Object.assign(document.createElement("a"), { href: URL.createObjectURL(new Blob([zip as BlobPart], { type: "application/zip" })), download: name })
        link.click(); setTimeout(() => URL.revokeObjectURL(link.href), 4000)
        toast({ tone: "success", title: "Icon pack exported", text: name })
        return
      }
      const path = await save({ defaultPath: name, filters: [{ name: "Zip archive", extensions: ["zip"] }] })
      if (!path) return
      await invoke<number>("export_zip_file", { path: /\.zip$/i.test(path) ? path : `${path}.zip`, data: base64Of(zip) })
      toast({ tone: "success", title: "Icon pack exported", text: path })
    } catch (error) { toast({ tone: "error", title: "The pack wasn't exported", text: errorText(error) }) }
    finally { setPacking(false) }
  }

  /* ------------------------------------------------ install */
  /** Waits until the PS4 lists the theme (the install runs in Downloads), up to two minutes. */
  const waitForTheme = async (contentId: string) => {
    for (let i = 0; i < 60; i++) {
      const list = await listConsoleThemes({ host, port, demo }).catch(() => null)
      if (list) setInstalled(list)
      if (list?.themes.some(theme => theme.contentId === contentId)) return list
      await new Promise(resolve => setTimeout(resolve, demo ? 40 : 2000))
    }
    return null
  }
  /** Sends a built theme, replacing an older copy of the same theme, then selects it. */
  const sendAndUse = async (pkg: SavedPackage) => {
    const current = await listConsoleThemes({ host, port, demo }).catch(() => null)
    if (current?.themes.some(theme => theme.contentId === pkg.contentId)) {
      setStep("Removing the old copy")
      await removeConsoleTheme({ host, port, contentId: pkg.contentId, demo })
    }
    setStep("Sending to the PS4")
    await installLocalPackage({ path: pkg.path, target: "ps4", title: pkg.title, demo })
    setStep("Installing on the PS4")
    const listed = await waitForTheme(pkg.contentId)
    if (!listed) throw new Error("The PS4 didn't list the theme within two minutes. Downloads shows how the install went.")
    setStep("Selecting it")
    await applyConsoleTheme({ host, port, contentId: pkg.contentId, demo })
    setInstalled(await listConsoleThemes({ host, port, demo }).catch(() => listed))
  }
  const installTheme = async () => {
    setStep("Drawing")
    try {
      const request = await buildThemeRequest(spec, images, setStep)
      setStep("Packing the theme")
      const result = await buildPs4Theme({ request, demo })
      const pkg = { contentId: result.contentId, path: result.path, title: result.title }
      writePackage(pkg); setSaved(pkg)
      await sendAndUse(pkg)
      toast({ tone: "success", title: "Theme installed and selected", text: `${result.title} is in Settings > Themes. If the home screen hasn't changed, pick it there once.` })
    } catch (error) { toast({ tone: "error", title: "The theme wasn't installed", text: errorText(error) }) }
    finally { setStep("") }
  }
  const restoreTheme = async () => {
    if (!saved) return
    try { await sendAndUse(saved); toast({ tone: "success", title: "Theme restored", text: `${saved.title} is installed and selected again.` }) }
    catch (error) { toast({ tone: "error", title: "The theme wasn't restored", text: errorText(error) }) }
    finally { setStep("") }
  }
  const themeAction = async (contentId: string, action: "apply" | "remove") => {
    try {
      const message = action === "apply" ? await applyConsoleTheme({ host, port, contentId, demo }) : await removeConsoleTheme({ host, port, contentId, demo })
      toast({ tone: "success", title: action === "apply" ? "Theme selected" : "Theme removed", text: message })
      setInstalled(await listConsoleThemes({ host, port, demo }))
    } catch (error) { toast({ tone: "error", title: action === "apply" ? "The theme wasn't selected" : "The theme wasn't removed", text: errorText(error) }) }
  }
  const busy = !!step || packing
  const preset = (Object.keys(PRESETS) as Array<keyof typeof PRESETS>).find(key => Object.entries(PRESETS[key].spec).every(([k, v]) => spec[k as keyof ThemeSpec] === v))

  return (
    <div className="thx">
      <section className="thx-stage" aria-label="PS4 preview">
        <div className="thx-bar">
          <Seg value={screen} options={SCREENS} onChange={setScreen} label="Preview screen" />
          <Seg value={themed ? "theme" : "stock"} options={[["stock", "Stock"], ["theme", "Themed"]]} onChange={value => setThemed(value === "theme")} label="Stock or themed" />
          <button type="button" className="btn sm" aria-pressed={marks} onClick={() => setMarks(value => !value)}><Icon name="eye" />{marks ? "Hide changes" : "Show changes"}</button>
        </div>
        <Ps4Preview screen={screen} themed={themed} marks={marks} art={themed ? art : stock} games={games} focus={spec.focus}
          onPick={slot => { setSelected(slot); setTab("icons") }} />
      </section>

      <section className="thx-side">
        <div className="thx-tabs" role="tablist" aria-label="Theme">
          {TABS.map(([id, label]) => <button key={id} type="button" role="tab" aria-selected={tab === id} onClick={() => setTab(id)}>{label}</button>)}
        </div>
        <div className="thx-panel" key={tab}>
          {tab === "look" && (
            <>
              <Row label="Theme name" className="tight"><input className="thx-input" value={spec.name} maxLength={60} onChange={event => setSpec({ name: event.target.value })} aria-label="Theme name" /></Row>
              <div className="thx-presets">
                {(Object.keys(PRESETS) as Array<keyof typeof PRESETS>).map(key => (
                  <button key={key} type="button" className={`thx-preset ${key}`} aria-pressed={preset === key} onClick={() => setSpec(PRESETS[key].spec)}><i />{PRESETS[key].label}</button>
                ))}
              </div>
              <Row label="Wallpaper" detail="A still image, fitted to 1920 × 1080.">
                <div className="thx-inline">
                  <Seg value={spec.wallpaper} options={[["neon", "Retro sunset"], ["image", "Your image"]]} onChange={value => value === "image" && !urls.wallpaper ? wallInput.current?.click() : setSpec({ wallpaper: value })} label="Wallpaper" />
                  <button type="button" className="btn sm icon" title="Choose an image" aria-label="Choose a wallpaper image" onClick={() => wallInput.current?.click()}><Icon name="image" /></button>
                </div>
                <input ref={wallInput} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={event => { const file = event.target.files?.[0]; if (file) void replace("wallpaper", file); event.target.value = "" }} />
              </Row>
              <Row label="Darken wallpaper"><input type="range" min={0} max={0.6} step={0.05} value={spec.dim} onChange={event => setSpec({ dim: Number(event.target.value) })} aria-label="Darken wallpaper" /></Row>
              <Row label="Colours" detail="Icon lines, then the second tone, then the focus frame.">
                <div className="thx-inline">
                  <input type="color" value={spec.primary} onChange={event => setSpec({ primary: event.target.value })} aria-label="Main colour" />
                  <input type="color" value={spec.secondary} onChange={event => setSpec({ secondary: event.target.value })} aria-label="Second colour" />
                  <input type="color" value={spec.focus} onChange={event => setSpec({ focus: event.target.value })} aria-label="Focus frame colour" />
                </div>
              </Row>
              <Row label="Icon lines"><Seg value={spec.glyph} options={[["neon", "Neon tubes"], ["line", "Clean lines"]]} onChange={glyph => setSpec({ glyph })} label="Icon lines" /></Row>
              <Row label="Tile background">
                <div className="thx-inline">
                  <Seg value={spec.backplate} options={[["dark", "Dark"], ["glass", "Frosted"], ["none", "None"]]} onChange={backplate => setSpec({ backplate })} label="Tile background" />
                  {spec.backplate === "dark" && <input type="color" value={spec.tile} onChange={event => setSpec({ tile: event.target.value })} aria-label="Tile colour" />}
                </div>
              </Row>
              <p className="sys-label">Shape</p>
              <div className="thx-shapes" role="radiogroup" aria-label="Icon shape">
                {SHAPES.map(([shape, label]) => <button key={shape} type="button" role="radio" aria-checked={spec.shape === shape} className="thx-shape" onClick={() => setSpec({ shape })}><ShapeGlyph shape={shape} />{label}</button>)}
              </div>
              {spec.shape === "rounded" && <Row label="Corner radius"><input type="range" min={0.06} max={0.46} step={0.02} value={spec.radius} onChange={event => setSpec({ radius: Number(event.target.value) })} aria-label="Corner radius" /></Row>}
              <Row label="Border" detail="Traces the final shape.">
                <div className="thx-inline">
                  <input type="range" min={0} max={24} step={1} value={spec.border} onChange={event => setSpec({ border: Number(event.target.value) })} aria-label="Border width" />
                  <input type="color" value={borderOf(spec)} onChange={event => setSpec({ borderColor: event.target.value })} aria-label="Border colour" />
                  {spec.borderColor && <button type="button" className="btn sm" onClick={() => setSpec({ borderColor: "" })} title="Follow the main colour">Auto</button>}
                </div>
              </Row>
              <Row label="Glass" detail="A top sheen and a fine rim." className="tight"><Switch checked={spec.glass} onChange={glass => setSpec({ glass })} label="Glass" /></Row>
              <Row label="Top menu icons"><Seg value={spec.topRow} options={[["glyph", "Symbols"], ["tile", "On tiles"]]} onChange={topRow => setSpec({ topRow })} label="Top menu icons" /></Row>
            </>
          )}
          {tab === "icons" && (
            <IconsTab spec={spec} thumbs={thumbs} custom={urls as Partial<Record<SlotId, string>>} selected={selected} onSelect={setSelected} busy={packing}
              setFit={(slot, fit: Fit) => setSpec({ fit: { ...spec.fit, [slot]: fit } })}
              onReplace={(slot, file) => void replace(slot, file)}
              onReset={slot => { void store(slot, null); toast({ tone: "success", title: "Back to SSPI's icon", text: CONTENT_SLOTS.concat(FUNCTION_SLOTS).find(item => item.id === slot)?.label || slot }) }}
              onImport={files => void importFiles(files)} onExport={() => void exportPack()} />
          )}
          {tab === "install" && (!ready ? <p className="sys-note">Load the receiver on your PS4 (Tools &gt; Payloads) to install themes. Game icons have their own tab.</p> : (
            <>
              <p className="sys-label">System theme</p>
              {saved && (() => {
                const there = installed?.themes.some(theme => theme.contentId === saved.contentId)
                const active = installed?.activeContentId === saved.contentId
                return (
                  <div className={`thx-state ${active ? "good" : there ? "warn" : ""}`} role="status">
                    <i className="dot" /><b>{saved.title}</b>
                    <span>{active ? "In use on the PS4" : there ? "Installed, not in use" : installed ? "Not on the PS4" : "Checking…"}</span>
                  </div>
                )
              })()}
              <div className="thx-actions">
                <button type="button" className="btn primary" disabled={busy || !canTheme} onClick={() => void installTheme()}>
                  {step ? <span className="spinner" /> : <Icon name="download" />}{step ? `${step}…` : "Install and use on PS4"}
                </button>
                {saved && <button type="button" className="btn" disabled={busy || !canTheme} onClick={() => void restoreTheme()}><Icon name="retry" />Restore after a restart</button>}
              </div>
              <p className="sys-note">Custom themes can drop out at a restart. Load GoldHEN and the receiver, then press Restore after a restart.</p>
              {installed && installed.themes.length > 0 && (
                <div className="thx-installed">
                  {installed.themes.map(theme => (
                    <div key={theme.contentId} className="thx-theme">
                      <b>{theme.title}</b>{installed.activeContentId === theme.contentId && <i className="tag">In use</i>}
                      <span className="thx-inline">
                        <button type="button" className="btn sm" disabled={installed.activeContentId === theme.contentId} onClick={() => void themeAction(theme.contentId, "apply")}>Use</button>
                        <button type="button" className="btn sm" onClick={() => void themeAction(theme.contentId, "remove")}>Remove</button>
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </>
          ))}
        </div>
      </section>
    </div>
  )
}
