import * as RD from "@radix-ui/react-dialog"
import type { ReactNode } from "react"
import { fmtBytes, plural } from "@/lib/format"
import type { ConsoleKind, LocalPackage, ManualCandidate } from "@/types"
import { Icon } from "./Icon"

export type ManualRow = ManualCandidate & { kind: string }

function Shell({ open, onClose, wide, title, description, children }: { open: boolean; onClose: () => void; wide?: boolean; title: string; description: ReactNode; children: ReactNode }) {
  return (
    <RD.Root open={open} onOpenChange={next => { if (!next) onClose() }}>
      <RD.Portal>
        <RD.Overlay className="scrim" />
        <RD.Content className={`dialog ${wide ? "wide" : ""}`} aria-describedby={undefined}>
          <RD.Title asChild><h2>{title}</h2></RD.Title>
          <div className="lede">{description}</div>
          {children}
        </RD.Content>
      </RD.Portal>
    </RD.Root>
  )
}

/** PS5 connection required: shown when an install can't reach the receiver. */
export function ReceiverDialog({ open, message, host, port, busy, onClose, onDownload, onSettings }: {
  open: boolean; message: string; host: string; port: number; busy: boolean
  onClose: () => void; onDownload: () => Promise<void>; onSettings: () => void
}) {
  return (
    <Shell open={open} onClose={onClose} title="Your PS5 needs the receiver" description="Browsing and package links work without a console. Installing needs the SSPI receiver running on your PS5.">
      <div className="target-line"><span>Configured receiver</span><strong>{host ? `${host}:${port}` : "Not set"}</strong></div>
      <div className="inline-error"><Icon name="alert" /><span>{message}</span></div>
      <div className="dialog-actions" style={{ marginTop: 18 }}>
        <button type="button" className="btn ghost" onClick={onClose}>Not now</button>
        <button type="button" className="btn" disabled={busy} onClick={() => void onDownload()}>{busy ? <span className="spinner" /> : <Icon name="download" />}Download receiver ELF</button>
        <button type="button" className="btn primary" onClick={onSettings}><Icon name="monitor" />Console settings</button>
      </div>
    </Shell>
  )
}

/** Packages found on this PC and game folders to package or install. */
export function ImportDialog({ open, onClose, target, local, picked, setPicked, onInstallLocal, localBusy, localBlock, manual, setManual, onInstallManual, manualBusy, manualBlock, onClear }: {
  open: boolean; onClose: () => void; target: ConsoleKind
  local: LocalPackage[]; picked: string[]; setPicked: (paths: string[]) => void; onInstallLocal: () => void; localBusy: boolean; localBlock?: string
  manual: ManualRow[]; setManual: (update: (rows: ManualRow[]) => ManualRow[]) => void; onInstallManual: (packageOnly: boolean) => void; manualBusy: boolean; manualBlock?: string
  onClear: () => void
}) {
  const packableOnly = manual.some(item => item.content !== "dump" || item.kind === "backport")
  return (
    <Shell open={open} onClose={onClose} wide title="Add from this PC" description={`Choose what to send to your ${target.toUpperCase()}. Imported originals are never changed or removed.`}>
      <div className="dialog-body">
        {local.length > 0 && (
          <>
            <div className="opt-section" style={{ marginTop: 0 }}>Packages found</div>
            <div className="local-list">
              {local.map(item => {
                const on = picked.includes(item.path)
                return (
                  <label key={item.path} className="local-row">
                    <button type="button" className="check" role="checkbox" aria-checked={on} aria-label={`Select ${item.name}`} disabled={localBusy} onClick={() => setPicked(on ? picked.filter(path => path !== item.path) : [...picked, item.path])}><Icon name="check" /></button>
                    {item.icon ? <img src={item.icon} alt="" /> : <span className="noimg" />}
                    <span><strong>{item.name}{item.version ? ` v${item.version}` : ""}</strong><span className="sub">{[item.fileName && item.fileName !== item.name ? item.fileName : "", item.titleId, item.packageKind || item.kind, fmtBytes(item.size)].filter(Boolean).join(" · ")}</span></span>
                    <span className="tag">{item.packageKind || item.kind}</span>
                  </label>
                )
              })}
            </div>
            {localBlock && <p className="local-note">{localBlock}</p>}
            <div className="dialog-actions" style={{ justifyContent: "flex-start", marginTop: 10 }}>
              <button type="button" className="btn primary sm" disabled={!picked.length || !!localBlock || localBusy} onClick={onInstallLocal}>{localBusy ? <span className="spinner" /> : <Icon name="download" />}Install {picked.length ? plural(picked.length, "package") : "selected"}</button>
            </div>
          </>
        )}
        {manual.length > 0 && (
          <>
            <div className="opt-section" style={{ marginTop: local.length ? 22 : 0 }}>Game folders</div>
            <p className="rail-note" style={{ marginTop: -4, marginBottom: 8 }}>Title, version and artwork are read from each dump. Package them on this PC without a console, or install them with your packaging setting.</p>
            <div className="local-list">
              {manual.map(item => (
                <div key={item.path} className="local-row manual">
                  {item.icon ? <img src={item.icon} alt="" /> : <span className="noimg" />}
                  <span><strong>{item.name}{item.version ? ` v${item.version}` : ""}</strong><span className="sub">{item.content}, {item.titleId || "no title ID"}, {fmtBytes(item.size)}</span></span>
                  <select className="select" aria-label={`Kind for ${item.name}`} disabled={manualBusy} value={item.kind} onChange={event => setManual(rows => rows.map(row => row.path === item.path ? { ...row, kind: event.target.value } : row))}>
                    {["base", "update", "dlc", "backport"].map(kind => <option key={kind} value={kind}>{kind}</option>)}
                  </select>
                  <label className="field"><input aria-label={`Title ID for ${item.name}`} disabled={manualBusy} placeholder={item.needsTitle ? "CUSA or PPSA ID" : "Title ID"} value={item.titleId || ""} onChange={event => setManual(rows => rows.map(row => row.path === item.path ? { ...row, titleId: event.target.value } : row))} /></label>
                  <button type="button" className="btn ghost sm" disabled={manualBusy} onClick={() => setManual(rows => rows.filter(row => row.path !== item.path))}>Remove</button>
                </div>
              ))}
            </div>
            {manualBlock && <p className="local-note">{manualBlock}</p>}
            <div className="dialog-actions" style={{ justifyContent: "flex-start", marginTop: 10 }}>
              <button type="button" className="btn primary sm" disabled={packableOnly || manualBusy} title={packableOnly ? "Only complete game folders (not backports or archives) can be packaged on their own." : undefined} onClick={() => onInstallManual(true)}>{manualBusy ? <span className="spinner" /> : <Icon name="box" />}Package {plural(manual.length, "folder")}</button>
              <button type="button" className="btn sm" disabled={!!manualBlock || manualBusy} onClick={() => onInstallManual(false)}>{manualBusy ? <span className="spinner" /> : <Icon name="download" />}Install selected</button>
            </div>
          </>
        )}
        {!local.length && !manual.length && <p className="empty-note">Nothing is waiting here. Use Import file, Scan folder or Add game folders.</p>}
      </div>
      <div className="dialog-actions">
        <button type="button" className="btn ghost" disabled={localBusy || manualBusy} onClick={onClear}>Clear list</button>
        <button type="button" className="btn" onClick={onClose}>Done</button>
      </div>
    </Shell>
  )
}
