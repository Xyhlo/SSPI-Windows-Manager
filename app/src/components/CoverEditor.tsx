/* =====================================================================
   Change cover: frame an image as a console home-screen icon (512 px
   square PNG), preview it among the console's tiles, and save it through
   the receiver, which keeps the original for Restore.
   ===================================================================== */
import * as RD from "@radix-ui/react-dialog"
import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react"
import { cropBoxedCover, resolveCover } from "@/lib/covers"
import { getTitleIcon, refreshConsoleShell, restoreTitleIcon, setTitleIcon } from "@/lib/console-api"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe } from "@/lib/console-types"
import { errorText } from "@/lib/format"
import { DEFAULT_FRAMING, ICON_SIZE, drawFramed, fileToDataUrl, loadImage, makeSquare, pngBase64, renderFramed, thumbnail, type Framing } from "@/lib/image"
import type { LibraryEntry } from "@/lib/library"
import { clamp } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { Seg } from "./Controls"
import { Icon } from "./Icon"
import { toast } from "./toasts"

type Source = { id: "file" | "catalog" | "current" | "original"; label: string; url: string }

export function CoverEditor({ open, entry, neighbours, target, settings, probe, demo, onClose, onChanged }: {
  open: boolean
  entry: LibraryEntry | null
  neighbours: LibraryEntry[]
  target: ConsoleKind
  settings: Settings
  probe?: ConsoleProbe
  demo: boolean
  onClose: () => void
  onChanged: (titleId: string, icon: string | null, customIcon: boolean) => void
}) {
  const name = target.toUpperCase()
  const canWrite = demo || hasCapability(probe, "title-icons-v1")
  const canRefresh = !demo && hasCapability(probe, "shell-refresh-v1")
  const [sources, setSources] = useState<Source[]>([])
  const [source, setSource] = useState<Source | null>(null)
  const [image, setImage] = useState<HTMLImageElement | null>(null)
  const [framing, setFraming] = useState<Framing>(DEFAULT_FRAMING)
  const [busy, setBusy] = useState<"" | "load" | "save" | "restore">("")
  const [error, setError] = useState("")
  const [refreshAfter, setRefreshAfter] = useState(true)
  const fileRef = useRef<HTMLInputElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const drag = useRef<{ x: number; y: number; fx: number; fy: number } | null>(null)

  // Collect sources when the dialog opens: catalog art first, then the console's current and original icons.
  useEffect(() => {
    if (!open || !entry) return
    let live = true
    setError("")
    setFraming(DEFAULT_FRAMING)
    setImage(null)
    setSource(null)
    setSources([])
    const { host, port } = receiverEndpoint(settings, target)
    const found: Source[] = []
    const add = (item: Source) => {
      if (!live || found.some(existing => existing.id === item.id)) return
      found.push(item)
      found.sort((a, b) => ["file", "catalog", "current", "original"].indexOf(a.id) - ["file", "catalog", "current", "original"].indexOf(b.id))
      setSources([...found])
      setSource(current => current || item)
    }
    const catalog = entry.cover && entry.cover !== entry.icon ? entry.cover : null
    const tasks: Array<Promise<unknown>> = []
    if (catalog) tasks.push(resolveCover(catalog).then(cropBoxedCover).then(url => add({ id: "catalog", label: "Catalog artwork", url })).catch(() => undefined))
    if (canWrite) {
      tasks.push(getTitleIcon({ target, host, port, titleId: entry.titleId, original: false, demo }).then(url => add({ id: "current", label: entry.customIcon ? "Current cover" : "Console icon", url })).catch(() => undefined))
      if (entry.customIcon) tasks.push(getTitleIcon({ target, host, port, titleId: entry.titleId, original: true, demo }).then(url => add({ id: "original", label: "Original icon", url })).catch(() => undefined))
    } else if (entry.icon) add({ id: "current", label: "Console icon", url: entry.icon })
    setBusy("load")
    void Promise.allSettled(tasks).then(() => { if (live) setBusy(current => current === "load" ? "" : current) })
    return () => { live = false }
  }, [open, entry?.key])

  useEffect(() => {
    if (!source) return
    let live = true
    setFraming(old => ({ ...DEFAULT_FRAMING, background: old.background, color: old.color }))
    loadImage(source.url).then(img => { if (live) setImage(img) }).catch(() => { if (live) setError("This image couldn't be opened. Choose another one.") })
    return () => { live = false }
  }, [source?.url])

  // Live preview.
  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas || !image) return
    const size = canvas.width
    drawFramed(canvas.getContext("2d")!, image, size, framing)
  }, [image, framing])

  const preview = useMemo(() => image ? renderFramed(image, framing, 192).toDataURL("image/png") : null, [image, framing])

  const pick = async (file: File | undefined) => {
    if (!file) return
    if (!/^image\/(png|jpe?g|webp|gif|bmp)$/i.test(file.type)) { setError("Choose a PNG, JPEG, WebP, GIF or BMP image."); return }
    if (file.size > 40 * 1024 * 1024) { setError("That image is larger than 40 MB."); return }
    try {
      const url = await fileToDataUrl(file)
      const item: Source = { id: "file", label: file.name, url }
      setSources(old => [item, ...old.filter(existing => existing.id !== "file")])
      setSource(item)
      setError("")
    } catch (err) { setError(errorText(err)) }
  }

  const onPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { x: event.clientX, y: event.clientY, fx: framing.x, fy: framing.y }
  }
  const onPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const start = drag.current
    if (!start || !image) return
    const rect = event.currentTarget.getBoundingClientRect()
    const size = rect.width
    const iw = image.naturalWidth, ih = image.naturalHeight
    const base = framing.fit === "fill" ? Math.max(size / iw, size / ih) : Math.min(size / iw, size / ih)
    const w = iw * base * framing.zoom, h = ih * base * framing.zoom
    const ox = Math.abs(w - size) / 2 || 1, oy = Math.abs(h - size) / 2 || 1
    setFraming(old => ({ ...old, x: clamp(start.fx - (event.clientX - start.x) / ox, -1, 1), y: clamp(start.fy - (event.clientY - start.y) / oy, -1, 1) }))
  }
  const onWheel = (event: React.WheelEvent) => {
    const min = framing.fit === "fill" ? 1 : 0.4
    setFraming(old => ({ ...old, zoom: clamp(old.zoom * (event.deltaY < 0 ? 1.06 : 1 / 1.06), min, 4) }))
  }

  const save = async () => {
    if (!entry || !image) return
    const { host, port } = receiverEndpoint(settings, target)
    setBusy("save")
    setError("")
    try {
      const canvas = renderFramed(image, framing, ICON_SIZE)
      // Console tiles aren't known to honour transparency, so the icon is flattened.
      const result = await setTitleIcon({ target, host, port, titleId: entry.titleId, png: pngBase64(canvas, "#000000"), demo })
      onChanged(entry.titleId, thumbnail(canvas), true)
      let refreshed = false
      if (canRefresh && refreshAfter) {
        try { await refreshConsoleShell({ target, host, port, demo }); refreshed = true }
        catch (err) { toast({ tone: "warning", title: "The home screen didn't refresh", text: errorText(err) }) }
      }
      toast({
        tone: "success", title: `Cover saved on your ${name}`,
        text: demo ? result.message : refreshed ? `${entry.name} shows its new cover now.` : `${result.message} Restart your ${name} to see it on the home screen.`,
      })
      onClose()
    } catch (err) {
      setError(errorText(err))
    } finally { setBusy("") }
  }

  const restore = async () => {
    if (!entry) return
    const { host, port } = receiverEndpoint(settings, target)
    setBusy("restore")
    setError("")
    try {
      const result = await restoreTitleIcon({ target, host, port, titleId: entry.titleId, demo })
      let icon: string | null = null
      try {
        const url = await getTitleIcon({ target, host, port, titleId: entry.titleId, original: false, demo })
        const img = await loadImage(url)
        const square = makeSquare(256)
        square.getContext("2d")!.drawImage(img, 0, 0, 256, 256)
        icon = thumbnail(square)
      } catch { /* the tile keeps its last thumbnail until the next sync */ }
      onChanged(entry.titleId, icon, false)
      toast({ tone: "success", title: "Original cover restored", text: demo ? result.message : `${result.message} Restart your ${name} to see it on the home screen.` })
      onClose()
    } catch (err) { setError(errorText(err)) }
    finally { setBusy("") }
  }

  const tiles = neighbours.filter(item => item.key !== entry?.key).slice(0, 4)
  return (
    <RD.Root open={open} onOpenChange={next => { if (!next && !busy.startsWith("s") && busy !== "restore") onClose() }}>
      <RD.Portal>
        <RD.Overlay className="scrim" />
        <RD.Content className="dialog cover-editor" aria-describedby={undefined}>
          <RD.Title asChild><h2>Change the cover</h2></RD.Title>
          <div className="lede">{entry ? <>The icon your {name} shows for <strong>{entry.name}</strong>. SSPI keeps the original so you can put it back.</> : null}</div>
          <div className="ce-body">
            <div className="ce-editor">
              <div className={`ce-canvas ${target}`} onWheel={onWheel}>
                <canvas ref={canvasRef} width={680} height={680} onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={() => { drag.current = null }} onPointerCancel={() => { drag.current = null }} aria-label="Drag to move the image, scroll to zoom" />
                {!image && <div className="ce-wait">{busy === "load" ? <span className="spinner" /> : <Icon name="image" />}<span>{busy === "load" ? "Loading artwork" : "Choose an image"}</span></div>}
              </div>
              <div className="ce-controls">
                <label className="ce-zoom">
                  <span>Zoom</span>
                  <input type="range" min={framing.fit === "fill" ? 1 : 0.4} max={4} step={0.01} value={framing.zoom} disabled={!image} onChange={event => setFraming(old => ({ ...old, zoom: Number(event.target.value) }))} style={{ ["--fill" as string]: `${((framing.zoom - (framing.fit === "fill" ? 1 : 0.4)) / (4 - (framing.fit === "fill" ? 1 : 0.4))) * 100}%` }} />
                </label>
                <Seg label="Framing" value={framing.fit} options={[["fill", "Fill"], ["fit", "Fit"]]} onChange={fit => setFraming(old => ({ ...old, fit, zoom: 1, x: 0, y: 0 }))} />
                {framing.fit === "fit" && (
                  <>
                    <Seg label="Background" value={framing.background} options={[["blur", "Blurred"], ["color", "Colour"]]} onChange={background => setFraming(old => ({ ...old, background }))} />
                    {framing.background === "color" && <input className="ce-color" type="color" value={framing.color} aria-label="Background colour" onChange={event => setFraming(old => ({ ...old, color: event.target.value }))} />}
                  </>
                )}
              </div>
            </div>
            <div className="ce-side">
              <p className="ce-label">Image</p>
              <div className="ce-sources">
                {sources.map(item => (
                  <button key={item.id} type="button" className="ce-source" aria-pressed={source?.id === item.id} onClick={() => setSource(item)}>
                    <img src={item.url} alt="" draggable={false} />
                    <span>{item.label}</span>
                  </button>
                ))}
                <button type="button" className="ce-source add" onClick={() => fileRef.current?.click()}>
                  <span className="ce-add"><Icon name="plus" /></span>
                  <span>From this PC</span>
                </button>
                <input ref={fileRef} type="file" accept="image/png,image/jpeg,image/webp,image/gif,image/bmp" hidden onChange={event => { void pick(event.target.files?.[0]); event.target.value = "" }} />
              </div>
              <p className="ce-label">On your {name}</p>
              <div className={`ce-home ${target}`} aria-hidden="true">
                {preview && <img className="ce-home-bg" src={preview} alt="" />}
                <div className="ce-row">
                  <span className="ce-tile focus">{preview ? <img src={preview} alt="" /> : <i />}</span>
                  {tiles.map(item => <span key={item.key} className="ce-tile">{item.icon || item.cover ? <img src={item.icon || item.cover} alt="" /> : <i />}</span>)}
                </div>
                <span className="ce-home-name">{entry?.name}</span>
              </div>
              {canRefresh
                ? <label className="ce-check"><input type="checkbox" checked={refreshAfter} onChange={event => setRefreshAfter(event.target.checked)} />Refresh the home screen after saving</label>
                : !demo && <p className="ce-note"><Icon name="info" />Your {name} shows new covers after a restart. The home screen can't be refreshed safely while it's running.</p>}
              {!canWrite && <p className="ce-note warn"><Icon name="alert" />Load {probe?.receiver.expectedVersion ? `receiver ${probe.receiver.expectedVersion}` : "the latest receiver"} on your {name} to change covers.</p>}
            </div>
          </div>
          {error && <div className="inline-error" role="alert"><Icon name="alert" /><span>{error}</span></div>}
          <div className="dialog-actions">
            {entry?.customIcon && <button type="button" className="btn ghost" disabled={!!busy || !canWrite} onClick={() => void restore()}>{busy === "restore" ? <span className="spinner" /> : <Icon name="undo" />}Restore original</button>}
            <span style={{ flex: 1 }} />
            <button type="button" className="btn ghost" disabled={busy === "save" || busy === "restore"} onClick={onClose}>Cancel</button>
            <button type="button" className="btn primary" disabled={!image || !!busy && busy !== "load" || !canWrite} onClick={() => void save()}>{busy === "save" ? <span className="spinner" /> : <Icon name="check" />}Save to your {name}</button>
          </div>
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  )
}
