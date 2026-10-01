/* =====================================================================
   Game icons — an Icon Mask–style tool, separate from themes. It reads
   each game's icon from the console, cuts it to a shape with an
   optional border, glow and glass, and writes it back (on a PS4, also
   the copy the home screen draws). The console keeps the originals,
   and the new icons show after a restart.
   ===================================================================== */
import { useEffect, useMemo, useState } from "react"
import { Icon } from "../Icon"
import { Row, Switch } from "../Controls"
import { toast } from "../toasts"
import { ShapeGlyph } from "./themes/ShapeGlyph"
import { getTitleIcon, listConsoleLibrary, restoreTitleIcon, setTitleIcon } from "@/lib/console-api"
import { hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe, LibraryTitle } from "@/lib/console-types"
import { errorText, plural } from "@/lib/format"
import { loadImage, pngBase64 } from "@/lib/image"
import { MASK_PRESETS, loadMask, maskIcon, saveMask, type MaskSpec } from "@/lib/icon-mask"
import { SHAPES } from "@/lib/ps4-theme"
import type { ConsoleKind, Settings } from "@/types"

type Run = { verb: "Applying" | "Restoring"; done: number; total: number; failed: Array<{ name: string; error: string }>; finished: boolean; homeCopies: number }

export function IconMaskPanel({ target, settings, demo, probe }: { target: ConsoleKind; settings: Settings; demo: boolean; probe?: ConsoleProbe }) {
  const [mask, setMaskState] = useState<MaskSpec>(loadMask)
  const [titles, setTitles] = useState<LibraryTitle[] | null>(null)
  const [error, setError] = useState("")
  const [art, setArt] = useState<Record<string, HTMLImageElement>>({})
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [run, setRun] = useState<Run | null>(null)
  const { host, port } = receiverEndpoint(settings, target)
  const ready = demo || (!!host && probe?.receiver.state === "online")
  const canIcons = demo || hasCapability(probe, "title-icons-v1")
  const name = target.toUpperCase()
  const setMask = (patch: Partial<MaskSpec>) => setMaskState(old => { const next = { ...old, ...patch }; saveMask(next); return next })

  const load = async () => {
    if (!ready) return
    setError("")
    try {
      const library = await listConsoleLibrary({ target, host, port, demo })
      const list = library.entries.filter(entry => target === "ps5" || entry.platform !== "ps5")
      setTitles(list)
      setSelected(old => old.size ? new Set(list.filter(entry => old.has(entry.titleId)).map(entry => entry.titleId)) : new Set(list.map(entry => entry.titleId)))
      const loaded = await Promise.all(list.map(async entry => [entry.titleId, entry.icon ? await loadImage(entry.icon).catch(() => null) : null] as const))
      setArt(Object.fromEntries(loaded.filter(([, image]) => image)) as Record<string, HTMLImageElement>)
    } catch (reason) { setError(errorText(reason)); setTitles([]) }
  }
  useEffect(() => { setTitles(null); setRun(null); void load() /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [target, host, port, demo, ready])

  const previews = useMemo(() => Object.fromEntries(Object.entries(art).map(([id, image]) => [id, maskIcon(mask, image, 160).toDataURL()])), [art, mask])
  const chosen = (titles || []).filter(entry => selected.has(entry.titleId))
  const busy = !!run && !run.finished

  const apply = async (restore: boolean) => {
    if (!chosen.length) return
    const state: Run = { verb: restore ? "Restoring" : "Applying", done: 0, total: chosen.length, failed: [], finished: false, homeCopies: 0 }
    setRun({ ...state })
    for (const entry of chosen) {
      try {
        if (restore) await restoreTitleIcon({ target, host, port, titleId: entry.titleId, demo })
        else {
          // Always start from the console's saved original, so masks never stack.
          const original = await loadImage(await getTitleIcon({ target, host, port, titleId: entry.titleId, original: true, demo }))
          const result = await setTitleIcon({ target, host, port, titleId: entry.titleId, png: pngBase64(maskIcon(mask, original, 512)), demo })
          if (/home screen/i.test(result.message)) state.homeCopies++
        }
      } catch (reason) { state.failed.push({ name: entry.name || entry.titleId, error: errorText(reason) }) }
      state.done++
      setRun({ ...state })
    }
    state.finished = true
    setRun({ ...state })
    const ok = state.total - state.failed.length
    toast({
      tone: state.failed.length ? "warning" : "success",
      title: restore ? `Restored ${plural(ok, "icon")}` : `Masked ${plural(ok, "game icon")}`,
      text: state.failed.length ? `${state.failed[0].name}: ${state.failed[0].error}` : `Restart your ${name} to see them.`,
    })
    void load()
  }

  if (!ready) return (
    <div className="empty-state"><Icon name="image" /><h3>Load the receiver first</h3><p>Game icons are read from and written to your {name} by the SSPI receiver. Load it from Tools &gt; Payloads.</p></div>
  )
  if (!canIcons) return (
    <div className="empty-state"><Icon name="alert" /><h3>This receiver can't change icons</h3><p>Reload the {name} receiver from Tools &gt; Payloads.</p></div>
  )

  return (
    <div className="imk">
      <section className="imk-games" aria-label="Games">
        <div className="imk-bar">
          <strong>{titles ? plural(titles.length, "game") : "Games"}</strong>
          {titles && <span>{selected.size} selected</span>}
          <span className="imk-bar-tools">
            <button type="button" className="btn sm" disabled={!titles?.length || busy} onClick={() => setSelected(new Set((titles || []).map(entry => entry.titleId)))}>Select all</button>
            <button type="button" className="btn sm" disabled={!selected.size || busy} onClick={() => setSelected(new Set())}>Clear</button>
            <button type="button" className="btn sm icon" title="Refresh" aria-label="Refresh games" disabled={busy} onClick={() => void load()}><Icon name="refresh" /></button>
          </span>
        </div>
        {error ? <div className="empty-state"><Icon name="alert" /><h3>The games didn't load</h3><p>{error}</p></div>
          : !titles ? <div className="skeleton imk-skeleton" />
          : !titles.length ? <p className="kl-empty">No games found on this {name}.</p>
          : (
            <div className="imk-grid">
              {titles.map(entry => {
                const on = selected.has(entry.titleId)
                return (
                  <button key={entry.titleId} type="button" className="imk-card" aria-pressed={on} disabled={busy} title={entry.name}
                    onClick={() => setSelected(old => { const next = new Set(old); if (next.has(entry.titleId)) next.delete(entry.titleId); else next.add(entry.titleId); return next })}>
                    <span className="imk-art">{previews[entry.titleId] ? <img src={previews[entry.titleId]} alt="" /> : <Icon name="image" />}</span>
                    <span className="imk-name">{entry.name || entry.titleId}</span>
                    {entry.customIcon && <i className="imk-tag">Changed</i>}
                    <i className="imk-check"><Icon name="check" /></i>
                  </button>
                )
              })}
            </div>
          )}
      </section>

      <section className="imk-side" aria-label="Mask">
        <p className="sys-label">Mask</p>
        <div className="imk-presets">
          {MASK_PRESETS.map(preset => (
            <button key={preset.id} type="button" className="btn sm" onClick={() => setMask(preset.mask)}>{preset.label}</button>
          ))}
        </div>
        <div className="thx-shapes" role="radiogroup" aria-label="Icon shape">
          {SHAPES.map(([shape, label]) => <button key={shape} type="button" role="radio" aria-checked={mask.shape === shape} className="thx-shape" onClick={() => setMask({ shape })}><ShapeGlyph shape={shape} />{label}</button>)}
        </div>
        {mask.shape === "rounded" && <Row label="Corner radius"><input type="range" min={0.06} max={0.46} step={0.02} value={mask.radius} onChange={event => setMask({ radius: Number(event.target.value) })} aria-label="Corner radius" /></Row>}
        <Row label="Border" detail="Traces the shape.">
          <div className="thx-inline">
            <input type="range" min={0} max={24} step={1} value={mask.border} onChange={event => setMask({ border: Number(event.target.value) })} aria-label="Border width" />
            <input type="color" value={mask.borderColor} onChange={event => setMask({ borderColor: event.target.value })} aria-label="Border colour" />
          </div>
        </Row>
        <Row label="Glow" detail="A soft light along the border." className="tight"><Switch checked={mask.glow} onChange={glow => setMask({ glow })} label="Glow" /></Row>
        <Row label="Glass" detail="A top sheen and a fine rim." className="tight"><Switch checked={mask.glass} onChange={glass => setMask({ glass })} label="Glass" /></Row>

        <div className="thx-actions">
          <button type="button" className="btn primary" disabled={busy || !chosen.length} onClick={() => void apply(false)}><Icon name="layers" />{busy && run?.verb === "Applying" ? `Applying ${run.done} of ${run.total}…` : `Apply to ${plural(chosen.length, "game")}`}</button>
          <button type="button" className="btn" disabled={busy || !chosen.length} onClick={() => void apply(true)}><Icon name="undo" />Restore originals</button>
        </div>
        {run && (
          <div className={`imk-run${run.finished ? (run.failed.length ? " warn" : " good") : ""}`} role="status">
            <div className="imk-run-head">
              <b>{run.finished ? `${run.verb === "Applying" ? "Masked" : "Restored"} ${run.total - run.failed.length} of ${run.total}` : `${run.verb} ${run.done} of ${run.total}…`}</b>
              <i style={{ width: `${(run.done / run.total) * 100}%` }} />
            </div>
            {run.finished && run.total > run.failed.length && (
              <p className="imk-restart"><Icon name="refresh" />Restart your {name} (Power &gt; Restart{target === "ps4" ? " PS4" : ""}) to see the new icons. Rest mode doesn't reload them.</p>
            )}
            {run.failed.map(item => <p key={item.name} className="imk-fail"><b>{item.name}</b> {item.error}</p>)}
          </div>
        )}
        <p className="sys-note">The {name} keeps every original, so Restore puts it back exactly.</p>
      </section>
    </div>
  )
}
