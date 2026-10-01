/* Export as .txt or .csv: a small menu that saves through the app (or downloads in the preview). */
import { invoke } from "@tauri-apps/api/core"
import { save } from "@tauri-apps/plugin-dialog"
import { useEffect, useRef, useState } from "react"
import { Icon } from "../../Icon"
import { toast } from "../../toasts"
import type { ExportFormat } from "@/lib/diagnostics"
import { errorText } from "@/lib/format"

const LABEL: Record<ExportFormat, string> = { txt: "Text file (.txt)", csv: "Spreadsheet (.csv)" }

async function saveExport(name: string, format: ExportFormat, contents: string, demo: boolean) {
  const inApp = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window
  if (demo || !inApp) {
    // The offline preview has no file system access; the browser downloads it instead.
    const blob = new Blob([format === "csv" ? "﻿" + contents : contents], { type: format === "csv" ? "text/csv" : "text/plain" })
    const link = Object.assign(document.createElement("a"), { href: URL.createObjectURL(blob), download: name })
    link.click(); setTimeout(() => URL.revokeObjectURL(link.href), 4000)
    return name
  }
  const chosen = await save({ defaultPath: name, filters: [{ name: LABEL[format], extensions: [format] }] })
  if (!chosen) return null
  const path = /\.(txt|csv)$/i.test(chosen) ? chosen : `${chosen}.${format}`
  await invoke<number>("export_text_file", { path, contents })
  return path
}

export function ExportMenu({ name, build, demo, disabled, label = "Export" }: {
  /** File name without the extension. */
  name: string; build: (format: ExportFormat) => string; demo: boolean; disabled?: boolean; label?: string
}) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open) return
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (event instanceof KeyboardEvent ? event.key === "Escape" : !root.current?.contains(event.target as Node)) setOpen(false)
    }
    window.addEventListener("pointerdown", close); window.addEventListener("keydown", close)
    return () => { window.removeEventListener("pointerdown", close); window.removeEventListener("keydown", close) }
  }, [open])
  const run = async (format: ExportFormat) => {
    setOpen(false); setBusy(true)
    try {
      const saved = await saveExport(`${name}.${format}`, format, build(format), demo)
      if (saved) toast({ tone: "success", title: "Exported", text: saved })
    } catch (error) { toast({ tone: "error", title: "The export wasn't saved", text: errorText(error) }) }
    finally { setBusy(false) }
  }
  return (
    <div className="export-menu" ref={root}>
      <button type="button" className="btn sm" aria-haspopup="menu" aria-expanded={open} disabled={disabled || busy} onClick={() => setOpen(value => !value)}>
        {busy ? <span className="spinner" /> : <Icon name="save" />}{label}
      </button>
      {open && (
        <div className="export-pop" role="menu">
          {(["txt", "csv"] as ExportFormat[]).map(format => (
            <button key={format} type="button" role="menuitem" onClick={() => void run(format)}><Icon name="files" />{LABEL[format]}</button>
          ))}
        </div>
      )}
    </div>
  )
}
