/* =====================================================================
   Tools — console utilities beyond installing games: Payloads (the
   binloader manager), System (what the console reports, its kernel
   log, processes and payload logs) and Themes (PS4 system themes and
   game icon shapes). The switch picks the console for
   payloads, system and game icons; 1 to 5 jump between the tools.
   ===================================================================== */
import { useEffect, useLayoutEffect, useRef } from "react"
import type { ConsoleProbe } from "@/lib/console-types"
import { isTyping, runPanelKeys, setPageKeys } from "@/lib/keys"
import { glide } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { ConsoleSwitch } from "./Controls"
import { PayloadsPanel } from "./tools/PayloadsPanel"
import { WebLauncherPanel } from "./tools/WebLauncherPanel"
import { SystemPanel } from "./tools/SystemPanel"
import { ThemesPanel } from "./tools/ThemesPanel"
import { IconMaskPanel } from "./tools/IconMaskPanel"

export type ToolsTab = "payloads" | "system" | "themes" | "icons" | "web"
const TOOLS: Array<{ id: ToolsTab; label: string; detail: string }> = [
  { id: "payloads", label: "Payloads", detail: "Send ELF and BIN payloads to your console's loader." },
  { id: "web", label: "Web launcher", detail: "Host the PS5 WebKit Autoloader from this PC." },
  { id: "system", label: "System", detail: "What the receiver reads about your console." },
  { id: "themes", label: "Themes", detail: "PS4 system themes: wallpaper and system icons." },
  { id: "icons", label: "Game icons", detail: "Mask every game's icon, like Icon Mask." },
]

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
  // The offline preview behaves as if both consoles were set up.
  const toolSettings = demo ? { ...settings, ps5Host: settings.ps5Host || "192.168.0.20", ps4Host: settings.ps4Host || "192.168.0.21" } : settings

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
    if (/^[1-5]$/.test(k)) { event.preventDefault(); setTab(TOOLS[Number(k) - 1].id); return }
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
        {tab === "themes" || tab === "web" ? <span className="tl-only">{tab === "web" ? "PS5 only" : "PS4 only"}</span> : <ConsoleSwitch value={target} onChange={onConsole} probes={probes} demo={demo} label="Console for tools" />}
      </div>
      <div className="tl-body swap-fade" key={tab}>
        {tab === "payloads" && <PayloadsPanel target={target} settings={toolSettings} demo={demo} onReceiverLoaded={onReceiverLoaded} />}
        {tab === "web" && <WebLauncherPanel demo={demo} />}
        {tab === "themes" && <ThemesPanel settings={toolSettings} demo={demo} probe={probes.ps4} />}
        {tab === "icons" && <IconMaskPanel target={target} settings={toolSettings} demo={demo} probe={probes[target]} />}
        {tab === "system" && <SystemPanel target={target} settings={toolSettings} demo={demo} probe={probes[target]} onLoadReceiver={() => onLoadReceiver(target)} />}
      </div>
    </div>
  )
}
