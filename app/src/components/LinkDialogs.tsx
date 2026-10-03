/* Paste download links, and browse files already stored in debrid accounts (as in SSPI PS4).
   Both queue ordinary downloads; hoster and account links are unlocked when each download starts. */
import * as RD from "@radix-ui/react-dialog"
import { invoke } from "@tauri-apps/api/core"
import { useEffect, useMemo, useState, type ReactNode } from "react"
import { errorText, fmtBytes, plural } from "@/lib/format"
import type { Settings } from "@/types"
import { Icon } from "./Icon"
import { toast } from "./toasts"

export type LinkItem = {
  url: string; name: string; titleId: string; title: string; kind: string
  provider: string; size?: number | null; direct: boolean; set: string; part?: number | null
}
type CloudEntry = { name: string; folder: string; link?: LinkItem | null; size?: number | null; detail: string; ready: boolean }

const PROVIDER_NAMES: Record<string, string> = { "real-debrid": "Real-Debrid", torbox: "TorBox", alldebrid: "AllDebrid" }
const KINDS = ["base", "update", "dlc", "backport"]

function Shell({ open, onClose, title, description, children }: { open: boolean; onClose: () => void; title: string; description: ReactNode; children: ReactNode }) {
  return (
    <RD.Root open={open} onOpenChange={next => { if (!next) onClose() }}>
      <RD.Portal>
        <RD.Overlay className="scrim" />
        <RD.Content className="dialog wide" aria-describedby={undefined}>
          <RD.Title asChild><h2>{title}</h2></RD.Title>
          <div className="lede">{description}</div>
          {children}
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  )
}

/** Links whose names say they are volumes of one archive are downloaded together by default. */
const oneSet = (items: LinkItem[]) => items.length > 1 && items.every(item => item.set && item.set === items[0].set)

function hostOf(url: string) { try { return new URL(url).host } catch { return url } }

/** Selected links as rows with an editable title ID and kind. */
function LinkRows({ items, picked, setPicked, setItems, busy }: {
  items: LinkItem[]; picked: Set<string>; setPicked: (next: Set<string>) => void
  setItems: (update: (rows: LinkItem[]) => LinkItem[]) => void; busy: boolean
}) {
  return (
    <div className="local-list">
      {items.map(item => {
        const on = picked.has(item.url)
        const tags = [item.direct ? "Direct" : PROVIDER_NAMES[item.provider] || hostOf(item.url), item.part ? `part ${item.part}` : "", item.size ? fmtBytes(item.size) : ""].filter(Boolean)
        return (
          <div key={item.url} className="local-row link-row">
            <button type="button" className="check" role="checkbox" aria-checked={on} aria-label={`Select ${item.name || item.url}`} disabled={busy}
              onClick={() => { const next = new Set(picked); if (on) next.delete(item.url); else next.add(item.url); setPicked(next) }}><Icon name="check" /></button>
            <span><strong title={item.url}>{item.title || item.name || hostOf(item.url)}</strong><span className="sub">{[item.name && item.name !== item.title ? item.name : "", ...tags].filter(Boolean).join(" · ")}</span></span>
            <select className="select" aria-label={`Kind for ${item.name || item.url}`} disabled={busy} value={item.kind} onChange={event => setItems(rows => rows.map(row => row.url === item.url ? { ...row, kind: event.target.value } : row))}>
              {KINDS.map(kind => <option key={kind} value={kind}>{kind}</option>)}
            </select>
            <label className="field"><input aria-label={`Title ID for ${item.name || item.url}`} disabled={busy} placeholder="CUSA or PPSA ID" value={item.titleId}
              onChange={event => setItems(rows => rows.map(row => row.url === item.url ? { ...row, titleId: event.target.value.trim().toUpperCase() } : row))} /></label>
          </div>
        )
      })}
    </div>
  )
}

/** Download button with the "one archive" switch; shared by both dialogs. */
function QueueBar({ chosen, asSet, setAsSet, busy, onQueue }: { chosen: LinkItem[]; asSet: boolean; setAsSet: (value: boolean) => void; busy: boolean; onQueue: () => void }) {
  return (
    <div className="dialog-actions link-actions">
      {chosen.length > 1 && (
        <label className="link-set"><input type="checkbox" checked={asSet} disabled={busy} onChange={event => setAsSet(event.target.checked)} />Parts of one archive, in this order</label>
      )}
      <button type="button" className="btn primary" disabled={!chosen.length || busy} onClick={onQueue}>
        {busy ? <span className="spinner" /> : <Icon name="download" />}{chosen.length ? `Download ${asSet && chosen.length > 1 ? `${chosen.length} parts` : plural(chosen.length, "link")}` : "Download"}
      </button>
    </div>
  )
}

async function queue(items: LinkItem[], asSet: boolean) {
  const started = await invoke<string[]>("start_link_downloads", { items, asSet: asSet && items.length > 1 })
  toast({ tone: "success", title: `${plural(started.length, "download")} queued`, text: "Hoster and account links are unlocked when each download starts." })
}

export function PasteLinksDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [text, setText] = useState("")
  const [items, setItems] = useState<LinkItem[]>([])
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [asSet, setAsSet] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const chosen = items.filter(item => picked.has(item.url))
  useEffect(() => { if (!open) { setText(""); setItems([]); setPicked(new Set()); setError("") } }, [open])
  const find = async (value = text) => {
    setError("")
    try {
      const found = await invoke<LinkItem[]>("parse_download_links", { text: value })
      setItems(found); setPicked(new Set(found.map(item => item.url))); setAsSet(oneSet(found))
      if (!found.length) setError("No download links were found in that text.")
    } catch (err) { setError(errorText(err)) }
  }
  const paste = async () => {
    try { const value = await navigator.clipboard.readText(); setText(value); await find(value) }
    catch { setError("The clipboard couldn't be read. Paste into the box instead (Ctrl+V).") }
  }
  const start = async () => {
    if (chosen.some(item => item.titleId && !/^(CUSA|PPSA|SLUS|SLES|SCUS|SCES|SLPS|SLPM|SCPS|SCAJ|SLAJ|SLKA|SLKS|SCKA)\d{5}$/.test(item.titleId))) { setError("Title IDs look like CUSA01234, PPSA01234, or SLUS01234."); return }
    setBusy(true); setError("")
    try { await queue(chosen, asSet); onClose() } catch (err) { setError(errorText(err)) } finally { setBusy(false) }
  }
  return (
    <Shell open={open} onClose={onClose} title="Paste download links" description="Paste links or a whole page of text. Hoster links download through your connected debrid service; direct links download as they are.">
      <div className="dialog-body">
        <textarea className="link-text" aria-label="Download links" rows={items.length ? 3 : 7} value={text} disabled={busy}
          placeholder="https://… one or more links, in any text" onChange={event => setText(event.target.value)} />
        <div className="row link-find">
          <button type="button" className="btn sm" disabled={busy} onClick={() => void paste()}><Icon name="copy" />Paste from clipboard</button>
          <button type="button" className="btn sm" disabled={!text.trim() || busy} onClick={() => void find()}><Icon name="link" />Find links</button>
          {items.length > 0 && <span className="sub">{plural(items.length, "link")} found</span>}
        </div>
        {items.length > 0 && <LinkRows items={items} picked={picked} setPicked={setPicked} setItems={setItems} busy={busy} />}
        {error && <p className="local-note" role="alert">{error}</p>}
      </div>
      <QueueBar chosen={chosen} asSet={asSet} setAsSet={setAsSet} busy={busy} onQueue={() => void start()} />
    </Shell>
  )
}

type Crumb = { label: string; folder: string }

export function CloudFilesDialog({ open, onClose, settings }: { open: boolean; onClose: () => void; settings: Settings }) {
  const roots = useMemo(() => [
    ...(settings.realDebridConfigured ? [{ label: "Real-Debrid downloads", folder: "rd-downloads" }, { label: "Real-Debrid torrents", folder: "rd-torrents" }] : []),
    ...(settings.torboxConfigured ? [{ label: "TorBox torrents", folder: "tb-torrents" }, { label: "TorBox web downloads", folder: "tb-webdl" }] : []),
    ...(settings.alldebridConfigured ? [{ label: "AllDebrid magnets", folder: "ad-magnets" }] : []),
  ], [settings.realDebridConfigured, settings.torboxConfigured, settings.alldebridConfigured])
  const [path, setPath] = useState<Crumb[]>([])
  const [entries, setEntries] = useState<CloudEntry[]>([])
  const [page, setPage] = useState(0)
  const [more, setMore] = useState(false)
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const [items, setItems] = useState<LinkItem[]>([])
  const [picked, setPicked] = useState<Set<string>>(new Set())
  const [asSet, setAsSet] = useState(false)
  const here = path[path.length - 1]
  const top = path.length === 1
  const chosen = items.filter(item => picked.has(item.url))

  const load = async (crumbs: Crumb[], nextPage: number) => {
    const folder = crumbs[crumbs.length - 1]
    setLoading(true); setError("")
    try {
      const found = await invoke<CloudEntry[]>("list_cloud_files", { folder: folder.folder, page: nextPage })
      setEntries(old => nextPage ? [...old, ...found] : found)
      setPage(nextPage); setMore(crumbs.length === 1 && found.length >= 50)
    } catch (err) { setError(errorText(err)); if (!nextPage) setEntries([]) }
    finally { setLoading(false) }
  }
  const go = (crumbs: Crumb[]) => { setPath(crumbs); setEntries([]); void load(crumbs, 0) }
  useEffect(() => { if (open && roots.length) go([roots[0]]); if (!open) { setPath([]); setEntries([]); setItems([]); setPicked(new Set()); setError("") } }, [open]) // eslint-disable-line react-hooks/exhaustive-deps

  const toggle = (link: LinkItem) => {
    const next = new Set(picked)
    if (next.has(link.url)) next.delete(link.url)
    else { next.add(link.url); setItems(rows => rows.some(row => row.url === link.url) ? rows : [...rows, link]) }
    setPicked(next)
    const selected = [...items, link].filter((item, i, all) => next.has(item.url) && all.findIndex(other => other.url === item.url) === i)
    setAsSet(oneSet(selected))
  }
  const start = async () => {
    setBusy(true); setError("")
    try { await queue(chosen, asSet); onClose() } catch (err) { setError(errorText(err)) } finally { setBusy(false) }
  }

  return (
    <Shell open={open} onClose={onClose} title="Debrid files" description="Files already stored in your connected debrid accounts. Choose files to download; they install like any other download.">
      {!roots.length ? (
        <div className="dialog-body"><p className="opt-note">Connect Real-Debrid, TorBox or AllDebrid in Options › Debrid to browse your stored files.</p></div>
      ) : (
        <div className="dialog-body">
          <div className="ftabs cloud-tabs" role="tablist" aria-label="Debrid folders">
            {roots.map(root => (
              <button key={root.folder} type="button" role="tab" className="ftab" aria-selected={path[0]?.folder === root.folder} onClick={() => go([root])}>{root.label}</button>
            ))}
          </div>
          {path.length > 1 && (
            <div className="row cloud-path">
              <button type="button" className="btn ghost sm" onClick={() => go(path.slice(0, -1))}><Icon name="left" />Back</button>
              <span className="sub">{path.map(crumb => crumb.label).join(" › ")}</span>
            </div>
          )}
          <div className="local-list cloud-list">
            {entries.map((entry, n) => entry.folder ? (
              <button key={`${entry.folder}-${n}`} type="button" className="local-row cloud-row" disabled={!entry.ready || loading} onClick={() => go([...path, { label: entry.name, folder: entry.folder }])}>
                <Icon name="folder" />
                <span><strong>{entry.name}</strong><span className="sub">{entry.detail}</span></span>
                <Icon name="chevR" />
              </button>
            ) : entry.link && (
              <div key={`${entry.link.url}-${n}`} className="local-row cloud-row file">
                <button type="button" className="check" role="checkbox" aria-checked={picked.has(entry.link.url)} aria-label={`Select ${entry.name}`} disabled={!entry.ready || busy} onClick={() => toggle(entry.link!)}><Icon name="check" /></button>
                <span><strong title={entry.name}>{entry.name}</strong><span className="sub">{entry.detail}</span></span>
                <span className="sub">{entry.size ? fmtBytes(entry.size) : ""}</span>
              </div>
            ))}
            {loading && <p className="sub cloud-note"><span className="spinner" /> Loading {here?.label.toLowerCase()}…</p>}
            {!loading && !entries.length && !error && <p className="sub cloud-note">Nothing here.</p>}
          </div>
          {top && more && !loading && <button type="button" className="btn ghost sm" onClick={() => void load(path, page + 1)}>Show more</button>}
          {chosen.length > 0 && <p className="sub cloud-note">{plural(chosen.length, "file")} selected{chosen.some(item => !item.titleId) ? ". Files without a title ID in the name are matched by the package after download." : "."}</p>}
          {error && <p className="local-note" role="alert">{error}</p>}
        </div>
      )}
      <QueueBar chosen={chosen} asSet={asSet} setAsSet={setAsSet} busy={busy} onQueue={() => void start()} />
    </Shell>
  )
}
