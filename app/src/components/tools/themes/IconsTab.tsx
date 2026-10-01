/* =====================================================================
   Icons — every icon the theme replaces, each with its exact size and
   format, so anyone can swap in their own PNG (an AI image included):
   replace or reset one, copy a ready prompt for an image generator, or
   move the whole set in and out as a zip named by slot.
   ===================================================================== */
import { useRef, useState } from "react"
import { Icon } from "../../Icon"
import { Seg } from "../../Controls"
import { toast } from "../../toasts"
import { CONTENT_SLOTS, FUNCTION_SLOTS, iconPrompt, slotInfo, type SlotId, type SlotInfo } from "@/lib/ps4-icons"
import type { Fit, ThemeSpec } from "@/lib/ps4-theme"

const FITS: Record<SlotInfo["area"], Array<[Fit, string]>> = {
  content: [["fill", "Fill the tile"], ["glyph", "On a tile"], ["asis", "As is"]],
  function: [["asis", "As is"], ["glyph", "On a tile"]],
}

export function IconsTab({ spec, setFit, thumbs, custom, selected, onSelect, onReplace, onReset, onImport, onExport, busy }: {
  spec: ThemeSpec
  setFit: (slot: SlotId, fit: Fit) => void
  /** The current look of every slot, as shown in the preview. */
  thumbs: Record<SlotId, string>
  custom: Partial<Record<SlotId | "wallpaper", string>>
  selected: SlotId
  onSelect: (slot: SlotId) => void
  onReplace: (slot: SlotId, file: File) => void
  onReset: (slot: SlotId) => void
  onImport: (files: File[]) => void
  onExport: () => void
  busy: boolean
}) {
  const one = useRef<HTMLInputElement>(null)
  const many = useRef<HTMLInputElement>(null)
  const [showPrompt, setShowPrompt] = useState(false)
  const slot = slotInfo(selected)
  const prompt = iconPrompt(slot, { primary: spec.primary, secondary: spec.secondary }, spec.glyph)
  const isCustom = !!custom[selected]
  const fit = spec.fit[selected] ?? (slot.area === "content" ? "fill" : "asis")

  const copy = async () => {
    try { await navigator.clipboard.writeText(prompt); toast({ tone: "success", title: "Prompt copied", text: `Paste it into your image generator, then replace ${slot.label} with the PNG it makes.` }) }
    catch { setShowPrompt(true); toast({ tone: "warning", title: "Copy the prompt from the box", text: "The clipboard isn't available here." }) }
  }

  const grid = (slots: SlotInfo[]) => (
    <div className="thi-grid">
      {slots.map(item => (
        <button key={item.id} type="button" className="thi-card" aria-pressed={item.id === selected} onClick={() => { onSelect(item.id); setShowPrompt(false) }} title={item.label}>
          <span className={`thi-thumb ${item.area}`}><img src={thumbs[item.id]} alt="" /></span>
          <span className="thi-name">{item.label}</span>
          {custom[item.id] && <i className="thi-own">Yours</i>}
        </button>
      ))}
    </div>
  )

  return (
    <div className="thi">
      <section className="thi-detail" aria-label={`${slot.label} icon`}>
        <span className={`thi-big ${slot.area}`}><img src={thumbs[selected]} alt="" /></span>
        <div className="thi-about">
          <b>{slot.label}</b>
          <span className="thi-spec">{slot.area === "content" ? "PNG or JPG, 512 × 512. Transparency is kept." : "PNG, 128 × 128, transparent background."} Other sizes are fitted automatically.</span>
          {slot.note && <span className="thi-note">{slot.note}</span>}
          <div className="thi-actions">
            <button type="button" className="btn sm" onClick={() => one.current?.click()}><Icon name="upload" />Replace</button>
            {isCustom && <button type="button" className="btn sm" onClick={() => onReset(selected)}><Icon name="undo" />Use SSPI's</button>}
            <button type="button" className="btn sm" onClick={() => void copy()}><Icon name="sparkle" />Copy AI prompt</button>
          </div>
          {isCustom && slot.id !== "discoverlay" && <Seg value={fit} options={FITS[slot.area]} onChange={value => setFit(selected, value)} label={`How your ${slot.label} image sits`} />}
          {showPrompt && <textarea className="thi-prompt" readOnly value={prompt} aria-label="AI prompt" onFocus={event => event.currentTarget.select()} />}
        </div>
        <input ref={one} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={event => { const file = event.target.files?.[0]; if (file) onReplace(selected, file); event.target.value = "" }} />
      </section>

      <div className="thi-pack">
        <button type="button" className="btn sm" disabled={busy} onClick={onExport}><Icon name="download" />Export icon pack</button>
        <button type="button" className="btn sm" disabled={busy} onClick={() => many.current?.click()}><Icon name="files" />Import images</button>
        <span className="thi-hint">Files are matched by name: <code>library.png</code>, <code>trophy.png</code>, <code>wallpaper.jpg</code>, or a .zip of them.</span>
        <input ref={many} type="file" multiple accept="image/png,image/jpeg,image/webp,.zip,application/zip" hidden onChange={event => { const files = [...(event.target.files || [])]; if (files.length) onImport(files); event.target.value = "" }} />
      </div>

      <p className="sys-label">Home screen tiles</p>
      {grid(CONTENT_SLOTS)}
      <p className="sys-label">Top menu icons</p>
      {grid(FUNCTION_SLOTS)}
    </div>
  )
}
