/* Payload catalog (PS5): payloads published in Payload Manager's repository, searchable,
   each added to the library with one click. Checksums are verified when published. */
import * as RD from "@radix-ui/react-dialog"
import { useEffect, useState } from "react"
import { Icon } from "../../Icon"
import { toast } from "../../toasts"
import type { PayloadEntry } from "@/lib/console-types"
import { errorText } from "@/lib/format"
import { downloadCatalogPayload, filterCatalog, getPayloadCatalog, launcherAvailable, type CatalogSort, type PayloadCatalog } from "@/lib/launcher-api"

export function CatalogDialog({ open, demo, onClose, onImported }: { open: boolean; demo: boolean; onClose: () => void; onImported: (entries: PayloadEntry[]) => void }) {
  const [catalog, setCatalog] = useState<PayloadCatalog | null>(null)
  const [search, setSearch] = useState("")
  const [category, setCategory] = useState("")
  const [sort, setSort] = useState<CatalogSort>("category")
  const [showInstalled, setShowInstalled] = useState(false)
  const [busy, setBusy] = useState("")
  const [error, setError] = useState("")
  const refresh = async (force: boolean) => {
    setBusy("refresh"); setError("")
    try { setCatalog(await getPayloadCatalog(force, demo)) }
    catch (reason) { setError(errorText(reason)) }
    finally { setBusy("") }
  }
  useEffect(() => { if (open && !catalog) void refresh(false) /* eslint-disable-next-line react-hooks/exhaustive-deps */ }, [open])
  const download = async (filename: string, name: string) => {
    if (busy) return
    if (demo) { toast({ tone: "info", title: "Offline preview", text: "No payload was downloaded." }); return }
    setBusy(filename); setError("")
    try {
      onImported(await downloadCatalogPayload(filename))
      setCatalog(await getPayloadCatalog(false, demo))
      toast({ tone: "success", title: `${name} added`, text: "It's in your payload library. Use Send when ready." })
    } catch (reason) { setError(errorText(reason)) }
    finally { setBusy("") }
  }
  const needle = search.trim().toLowerCase()
  const categories = [...new Set((catalog?.entries || []).map(item => item.category?.trim() || "Uncategorized"))].sort()
  const entries = filterCatalog(catalog?.entries || [], search, category, showInstalled, sort)

  return (
    <RD.Root open={open} onOpenChange={next => { if (!next) onClose() }}>
      <RD.Portal>
        <RD.Overlay className="scrim" />
        <RD.Content className="dialog wide pc-dialog" aria-describedby={undefined}>
          <RD.Title asChild><h2>Payload catalog</h2></RD.Title>
          <p className="lede">Payloads published in <a href="https://github.com/itsPLK/ps5-payloads-mirror" target="_blank" rel="noreferrer">Payload Manager's repository</a>. Adding one keeps a copy in your library; nothing is sent until you choose Send.</p>
          <div className="pc-bar">
            <label className="field"><Icon name="search" /><input value={search} autoFocus onChange={event => setSearch(event.target.value)} placeholder="Search the catalog" aria-label="Search the payload catalog" /></label>
            <button type="button" className="btn sm" disabled={!!busy || !launcherAvailable(demo)} onClick={() => void refresh(true)}>{busy === "refresh" ? <span className="spinner" /> : <Icon name="refresh" />}Refresh</button>
          </div>
          <div className="pc-filters">
            <label>Category<select aria-label="Payload category" value={category} onChange={event => setCategory(event.target.value)}><option value="">All categories</option>{categories.map(value => <option key={value} value={value}>{value}</option>)}</select></label>
            <label>Sort<select aria-label="Sort payloads" value={sort} onChange={event => setSort(event.target.value as CatalogSort)}><option value="category">Category</option><option value="name">Name</option><option value="update_time">Update time</option></select></label>
            <label className="pc-installed"><input type="checkbox" checked={showInstalled} onChange={event => setShowInstalled(event.target.checked)} />Show in library</label>
            <span className="pc-count" role="status">{entries.length} {entries.length === 1 ? "payload" : "payloads"}</span>
          </div>
          {(error || catalog?.warning) && <div className="inline-error"><Icon name="alert" /><span>{error || catalog?.warning}</span></div>}
          <ul className="pc-list scroll">
            {!catalog && busy === "refresh" && Array.from({ length: 4 }, (_, n) => <li key={n} className="skeleton pc-skel" />)}
            {entries.map((entry, n) => (
              <li key={entry.filename}>
                {sort === "category" && (n === 0 || entries[n - 1].category !== entry.category) && <h3 className="pc-category">{entry.category?.trim() || "Uncategorized"}</h3>}
                <div className="pc-row" style={{ animationDelay: `${Math.min(n, 12) * 24}ms` }}>
                <span className="pl-glyph md">{(entry.filename.split(".").pop() || "elf").toUpperCase()}</span>
                <span className="pc-main">
                  <strong>{entry.name}{entry.version && <em>{entry.version}</em>}{entry.category && <span className="tag">{entry.category}</span>}</strong>
                  {entry.description && <span className="pc-desc">{entry.description}</span>}
                  <span className="pc-filename">{entry.filename}{entry.last_update && <> · {entry.last_update}</>}</span>
                  <span className="pc-meta">{entry.checksum ? <><Icon name="shield" />SHA-256 checked</> : "No published checksum"}{entry.source && <> · <a href={entry.source} target="_blank" rel="noreferrer">Source</a></>}</span>
                </span>
                <button type="button" className={`btn sm ${entry.installed ? "ok" : ""}`} disabled={!!busy || entry.installed} onClick={() => void download(entry.filename, entry.name)}>
                  {busy === entry.filename ? <span className="spinner" /> : <Icon name={entry.installed ? "check" : "download"} />}{entry.installed ? "In library" : busy === entry.filename ? "Adding" : "Add"}
                </button>
                </div>
              </li>
            ))}
            {catalog && !entries.length && <li className="pc-empty">{needle || category ? "No payloads match these filters." : catalog.entries.length ? "Your library is up to date. Enable Show in library to see installed payloads." : "The catalog is empty."}</li>}
          </ul>
          <div className="dialog-actions"><button type="button" className="btn ghost" onClick={onClose}>Done</button></div>
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  )
}
