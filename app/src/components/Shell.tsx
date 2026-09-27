import { getCurrentWindow } from "@tauri-apps/api/window"
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { createPortal } from "react-dom"
import sspiLogo from "@/assets/sspi-logo.svg"
import { consoleAddress } from "@/lib/consoles"
import { SPRINGS, glide, motionOK, settle } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { Icon } from "./Icon"

export type Tab = "search" | "downloads"
export type ConsoleState = "unknown" | "ok" | "fail"

const inTauri = () => "__TAURI_INTERNALS__" in window

export function TitleBar() {
  const run = (action: "minimize" | "maximize" | "close") => {
    if (!inTauri()) return
    const current = getCurrentWindow()
    if (action === "minimize") void current.minimize()
    if (action === "maximize") void current.toggleMaximize()
    if (action === "close") void current.close()
  }
  return (
    <div className="titlebar" data-tauri-drag-region>
      <div className="tb-left" data-tauri-drag-region><img src={sspiLogo} alt="" draggable={false} /><span>SSPI</span></div>
      <div className="tb-controls">
        <button type="button" onClick={() => run("minimize")} aria-label="Minimize"><Icon name="minus" /></button>
        <button type="button" onClick={() => run("maximize")} aria-label="Maximize"><Icon name="square" /></button>
        <button type="button" className="close" onClick={() => run("close")} aria-label="Close"><Icon name="x" /></button>
      </div>
    </div>
  )
}

/** Flashes a shoulder keycap as if it was pressed. */
export function pressKey(letter: "Q" | "E") {
  const el = document.getElementById(letter === "Q" ? "shoulderQ" : "shoulderE")
  if (!el) return
  el.classList.add("pressed")
  window.setTimeout(() => el.classList.remove("pressed"), 140)
}

/** A ring pulses around the Downloads tab when a thrown case lands. */
export function pulseDownloadsTab() {
  const tab = document.querySelector<HTMLElement>('.tab[data-page="downloads"]')
  if (!tab || !motionOK()) return
  const ring = document.createElement("span")
  ring.className = "tab-ring"
  tab.appendChild(ring)
  void settle(ring.animate([{ opacity: 0.9, transform: "scale(.9)" }, { opacity: 0, transform: "scale(1.18)" }], { duration: 700, easing: "cubic-bezier(.2,.8,.2,1)" })).then(() => ring.remove())
}

export function Header({ tab, activeJobs, settings, consoleState, demo, brandRef, menuDisabled = false, onTab, onStep, onConsole, onOptions, onManageConsoles }: {
  tab: Tab
  activeJobs: number
  settings: Settings
  consoleState: Record<ConsoleKind, ConsoleState>
  demo: boolean
  brandRef: React.RefObject<HTMLImageElement>
  menuDisabled?: boolean
  onTab: (tab: Tab) => void
  onStep: (direction: -1 | 1) => void
  onConsole: (target: ConsoleKind) => void
  onOptions: () => void
  onManageConsoles: () => void
}) {
  const tabsRef = useRef<HTMLElement>(null)
  const inkRef = useRef<HTMLSpanElement>(null)
  const badgeRef = useRef<HTMLSpanElement>(null)
  const chipRef = useRef<HTMLButtonElement>(null)
  const lastCount = useRef(activeJobs)
  const [menu, setMenu] = useState(false)

  useEffect(() => { if (menuDisabled) setMenu(false) }, [menuDisabled])

  useLayoutEffect(() => {
    const tabs = tabsRef.current
    glide(inkRef.current, tabs?.querySelector<HTMLElement>(`.tab[data-page="${tab}"]`) || null, tabs, { inset: 14, liquid: true })
  }, [tab])
  useEffect(() => {
    const onResize = () => {
      const tabs = tabsRef.current
      glide(inkRef.current, tabs?.querySelector<HTMLElement>(`.tab[aria-selected=true]`) || null, tabs, { inset: 14, instant: true })
    }
    window.addEventListener("resize", onResize)
    document.fonts?.ready.then(onResize).catch(() => undefined)
    return () => window.removeEventListener("resize", onResize)
  }, [])
  useEffect(() => {
    if (activeJobs > lastCount.current && badgeRef.current && motionOK()) {
      badgeRef.current.animate([{ transform: "scale(1)" }, { transform: "scale(1.55)" }, { transform: "scale(1)" }], { duration: 520, easing: SPRINGS.bouncy.easing })
    }
    lastCount.current = activeJobs
  }, [activeJobs])

  const active = settings.activeConsole
  const address = demo ? "Offline preview" : consoleAddress(settings, active) || (active === "ps5" ? "Receiver not set" : "PS4 not set")
  const dot = consoleState[active] === "ok" ? "good live" : consoleState[active] === "fail" ? "fail" : ""
  return (
    <header className="header">
      <button type="button" className="brand" aria-label="Search" onClick={() => onTab("search")}><img ref={brandRef} src={sspiLogo} alt="SSPI" draggable={false} /></button>
      <nav className="tabs" ref={tabsRef} role="tablist" aria-label="Sections">
        <span className="shoulder l" id="shoulderQ" role="button" tabIndex={-1} aria-label="Previous section (Q)" onClick={() => onStep(-1)}>Q</span>
        <button type="button" className="tab" role="tab" data-page="search" aria-selected={tab === "search"} onClick={() => onTab("search")}>Search</button>
        <button type="button" className="tab" role="tab" data-page="downloads" aria-selected={tab === "downloads"} onClick={() => onTab("downloads")}>
          Downloads{activeJobs > 0 && <span className="tab-badge" ref={badgeRef}>{activeJobs}</span>}
        </button>
        <span className="shoulder r" id="shoulderE" role="button" tabIndex={-1} aria-label="Next section (E)" onClick={() => onStep(1)}>E</span>
        <span className="tab-ink" ref={inkRef} />
      </nav>
      <div className="status">
        <button type="button" ref={chipRef} className="console-chip" disabled={menuDisabled} aria-haspopup="menu" aria-expanded={menu && !menuDisabled} onClick={() => setMenu(open => !open)} aria-label={`Managed console ${active.toUpperCase()}, ${address}`}>
          <span className={`dot ${dot}`} />
          <span className="cc-kind">{active.toUpperCase()}</span>
          <span className="cc-addr">{address}</span>
          <Icon name="chevD" />
        </button>
        <button type="button" className="options-btn" onClick={onOptions}><span className="options-glyph" aria-hidden="true"><i /><i /><i /></span>Options</button>
      </div>
      {menu && !menuDisabled && chipRef.current && createPortal(
        <ConsoleMenu anchor={chipRef.current} settings={settings} consoleState={consoleState} demo={demo} onClose={() => setMenu(false)} onPick={target => { setMenu(false); onConsole(target) }} onManage={() => { setMenu(false); onManageConsoles() }} />,
        document.body,
      )}
    </header>
  )
}

function ConsoleMenu({ anchor, settings, consoleState, demo, onClose, onPick, onManage }: { anchor: HTMLElement | null; settings: Settings; consoleState: Record<ConsoleKind, ConsoleState>; demo: boolean; onClose: () => void; onPick: (target: ConsoleKind) => void; onManage: () => void }) {
  const ref = useRef<HTMLDivElement>(null)
  const [position, setPosition] = useState<{ top: number; right: number } | null>(null)
  useLayoutEffect(() => {
    if (!anchor) return
    const update = () => {
      const rect = anchor.getBoundingClientRect()
      setPosition({ top: rect.bottom + 8, right: window.innerWidth - rect.right })
    }
    update()
    window.addEventListener("resize", update)
    window.addEventListener("scroll", update, true)
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(update)
    observer?.observe(anchor)
    return () => {
      window.removeEventListener("resize", update)
      window.removeEventListener("scroll", update, true)
      observer?.disconnect()
    }
  }, [anchor])
  useEffect(() => {
    const away = (event: PointerEvent) => {
      if (ref.current?.contains(event.target as Node) || anchor?.contains(event.target as Node)) return
      onClose()
    }
    const key = (event: KeyboardEvent) => { if (event.key === "Escape") { event.stopPropagation(); onClose() } }
    document.addEventListener("pointerdown", away, true)
    document.addEventListener("keydown", key, true)
    ref.current?.querySelector<HTMLElement>("[aria-checked=true]")?.focus()
    return () => { document.removeEventListener("pointerdown", away, true); document.removeEventListener("keydown", key, true) }
  }, [anchor, onClose])
  const items: Array<{ id: ConsoleKind; detail: string }> = [
    { id: "ps5", detail: demo ? "Offline preview" : consoleAddress(settings, "ps5") ? `Receiver at ${consoleAddress(settings, "ps5")}` : "Receiver not set" },
    { id: "ps4", detail: demo ? "Offline preview" : consoleAddress(settings, "ps4") ? `${settings.ps4Transport === "receiver" ? "Receiver" : "SSPI inbox"} at ${consoleAddress(settings, "ps4")}` : "PS4 not set" },
  ]
  return (
    <div ref={ref} className="popover" role="menu" style={{ top: position?.top ?? 0, right: position?.right ?? 0, visibility: position ? "visible" : "hidden" }}>
      {items.map(item => (
        <button key={item.id} type="button" className="pop-item" role="menuitemradio" aria-checked={settings.activeConsole === item.id} onClick={() => onPick(item.id)}>
          <span className="pop-ico">{item.id.toUpperCase()}<span className={`dot ${consoleState[item.id] === "ok" ? "good" : consoleState[item.id] === "fail" ? "fail" : ""}`} /></span>
          <span><strong>Manage {item.id.toUpperCase()}</strong><span>{item.detail}</span></span>
          <Icon name="check" className="ck" />
        </button>
      ))}
      <div className="pop-sep" />
      <div className="pop-foot"><button type="button" className="link" onClick={onManage}>Console settings in Options</button></div>
    </div>
  )
}

export type Hint = { key: string; label: string; glyph?: string; face?: "cross" | "circle" | "square" | "triangle" | "neutral"; disabled?: boolean; run?: () => void }

const FACE: Record<string, [NonNullable<Hint["face"]>, string]> = {
  Enter: ["cross", "⏎"],
  Escape: ["circle", "Esc"],
  Space: ["square", "Space"],
  Delete: ["neutral", "Del"],
}

export function Dock({ context, hints, build }: { context: ReactNode; hints: Hint[]; build: string }) {
  return (
    <footer className="dock">
      <div className="dock-context">{context}</div>
      <div className="dock-hints">
        {hints.map(hint => {
          const [face, glyph] = FACE[hint.key] || [hint.face || "neutral", hint.glyph || hint.key]
          return (
            <button key={`${hint.key}-${hint.label}`} type="button" className="hint" disabled={hint.disabled} tabIndex={hint.run ? 0 : -1} onClick={() => hint.run?.()}>
              <span className={`face ${hint.face || face}`}>{hint.glyph || glyph}</span>{hint.label}
            </button>
          )
        })}
      </div>
      <div className="dock-build">{build}</div>
    </footer>
  )
}
