/* =====================================================================
   Tools — console utilities beyond installing games: Payloads (the
   binloader manager and autostart), Web launcher (the PS5 console
   interface hosted from this PC), System (what the console reports, its
   kernel log, processes, logs and crash timeline) and Customize (game
   icon shapes, and PS4 system themes). The switch picks the console for
   payloads, system and game icons; 1 to 4 jump between the tools.
   ===================================================================== */
import { useEffect, useLayoutEffect, useRef, useState } from "react"
import type { ConsoleProbe } from "@/lib/console-types"
import { isTyping, runPanelKeys, setPageKeys } from "@/lib/keys"
import { glide } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { ConsoleSwitch, Seg } from "./Controls"
import { PayloadsPanel } from "./tools/PayloadsPanel"
import { WebLauncherPanel } from "./tools/WebLauncherPanel"
import { SystemPanel } from "./tools/SystemPanel"
import { IconMaskPanel } from "./tools/IconMaskPanel"

export type ToolsTab = "payloads" | "web" | "system" | "customize"
type CustomizeView = "icons" | "themes"
const TOOLS: Array<{ id: ToolsTab; label: string; detail: string }> = [
  { id: "payloads", label: "Payloads", detail: "Send payloads to your console's loader, and manage what starts after your console wakes." },
  { id: "web", label: "Web launcher", detail: "Host the SSPI console launcher for your PS5 from this PC." },
  { id: "system", label: "System", detail: "What the receiver reads about your console, its logs and crashes." },
  { id: "customize", label: "Customize", detail: "Game icon shapes, and PS4 system themes." },
]
const VIEW_KEY = "sspi.tools.customize"
const savedView = (): CustomizeView => { try { return localStorage.getItem(VIEW_KEY) === "themes" ? "themes" : "icons" } catch { return "icons" } }

export function ToolsPage({ tab, setTab, target, settings, demo, probes, onConsole, onLoadReceiver, onReceiverLoaded }: {
  tab: ToolsTab
  setTab: (tab: ToolsTab) => void
  target: ConsoleKind
  settings: Settings
  demo: boolean
  probes: Partial<Record<ConsoleKind, ConsoleProbe>>
  onConsole: (target: ConsoleKind) => void
  onLoadReceiver: (target: ConsoleKind) => void
  onReceiverLoaded: (target: ConsoleKind) => void
}) {
  const tabsRef = useRef<HTMLDivElement>(null)
  const inkRef = useRef<HTMLSpanElement>(null)
  const [view, setViewState] = useState<CustomizeView>(savedView)
  const setView = (next: CustomizeView) => { setViewState(next); try { localStorage.setItem(VIEW_KEY, next) } catch { /* per-PC preference only */ } }
  // The offline preview behaves as if both consoles were set up.
  const toolSettings = demo ? { ...settings, ps5Host: settings.ps5Host || "192.168.0.20", ps4Host: settings.ps4Host || "192.168.0.21" } : settings
  const singleConsole = tab === "web" ? "PS5 only" : tab === "customize" && view === "themes" ? "PS4 only" : ""

  useLayoutEffect(() => {
    glide(inkRef.current, tabsRef.current?.querySelector<HTMLElement>(`[data-tool="${tab}"]`) || null, tabsRef.current, { liquid: true })
  }, [tab])

  const live = useRef({ tab, setTab, target, onConsole })
  live.current = { tab, setTab, target, onConsole }
  useEffect(() => setPageKeys(event => {
    if (runPanelKeys(event)) return
    if (isTyping() || event.ctrlKey || event.altKey || event.metaKey) return
    const { setTab, target, onConsole } = live.current
    const k = event.key
    if (/^[1-4]$/.test(k)) { event.preventDefault(); setTab(TOOLS[Number(k) - 1].id); return }
    if (k === "c" || k === "C") { event.preventDefault(); onConsole(target === "ps5" ? "ps4" : "ps5") }
  }), [])

  return (
    <div className="page tools-page enter">
      <div className="tl-head">
        <div className="ftabs" ref={tabsRef} role="tablist" aria-label="Tools">
          {TOOLS.map(item => (
            <button key={item.id} type="button" role="tab" data-tool={item.id} className="ftab" title={item.detail} aria-selected={tab === item.id} onClick={() => setTab(item.id)}>{item.label}</button>
          ))}
          <span className="ftab-ink" ref={inkRef} />
        </div>
        {singleConsole ? <span className="tl-only swap-fade" key={singleConsole}>{singleConsole}</span> : <ConsoleSwitch value={target} onChange={onConsole} probes={probes} demo={demo} label="Console for tools" />}
      </div>
      <div className="tl-body swap-fade" key={tab}>
        {tab === "payloads" && <PayloadsPanel target={target} settings={toolSettings} demo={demo} onReceiverLoaded={onReceiverLoaded} />}
        {tab === "web" && <WebLauncherPanel demo={demo} />}
        {tab === "system" && <SystemPanel target={target} settings={toolSettings} demo={demo} probe={probes[target]} onLoadReceiver={() => onLoadReceiver(target)} />}
        {tab === "customize" && (
          <div className="cz">
            <div className="sysx-bar">
              <Seg<CustomizeView> label="Customize" value={view} options={[["icons", "Game icons"], ["themes", "PS4 themes"]]} onChange={setView} />
              <span className="cz-note">{view === "icons" ? "Shape, border and glow for every game's icon on your console." : "PS4 themes are coming soon."}</span>
            </div>
            <div className="cz-body swap-fade" key={view}>
              {view === "icons" && <IconMaskPanel target={target} settings={toolSettings} demo={demo} probe={probes[target]} />}
              {view === "themes" && <section className="themes-coming-soon" aria-label="PS4 themes">
                <div className="themes-coming-soon-preview" aria-hidden="true">
                  <div className="themes-coming-soon-wallpaper" />
                  <div className="themes-coming-soon-options">{["Wallpaper", "Colours", "System icons"].map(label => <div key={label}><span>{label}</span><i /></div>)}</div>
                </div>
                <div className="themes-coming-soon-message"><strong>Coming soon</strong><p>PS4 themes are being worked on.</p></div>
              </section>}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
