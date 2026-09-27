/* =====================================================================
   Tools — console utilities beyond installing games: Payloads (the
   binloader manager), System (what the console reports) and Themes (the
   tile and home-screen studio). The switch picks the console for all
   three; 1, 2 and 3 jump between them.
   ===================================================================== */
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import type { LibraryView } from "@/App"
import type { ConsoleProbe } from "@/lib/console-types"
import { isTyping, runPanelKeys, setPageKeys } from "@/lib/keys"
import { glide } from "@/lib/motion"
import type { ConsoleKind, Settings } from "@/types"
import { ConsoleSwitch } from "./Controls"
import type { OptionsTab } from "./OptionsOverlay"
import type { Hint } from "./Shell"
import { ThemeStudio } from "./ThemeStudio"
import { PayloadsPanel } from "./tools/PayloadsPanel"
import { SystemPanel } from "./tools/SystemPanel"

export type ToolsTab = "payloads" | "system" | "themes"
const TOOLS: Array<{ id: ToolsTab; label: string; detail: string }> = [
  { id: "payloads", label: "Payloads", detail: "Send ELF and BIN payloads to your console's loader." },
  { id: "system", label: "System", detail: "What the receiver reads about your console." },
  { id: "themes", label: "Themes", detail: "Restyle every game tile, and preview a home screen theme." },
]

export function ToolsPage({ tab, setTab, target, settings, demo, probes, libraries, onConsole, onLoadReceiver, onReceiverLoaded, onIconChanged, onRefreshLibrary, onOptions, onDock }: {
  tab: ToolsTab
  setTab: (tab: ToolsTab) => void
  target: ConsoleKind
  settings: Settings
  demo: boolean
  probes: Partial<Record<ConsoleKind, ConsoleProbe>>
  libraries: Record<ConsoleKind, LibraryView>
  onConsole: (target: ConsoleKind) => void
  onLoadReceiver: (target: ConsoleKind) => void
  onReceiverLoaded: (target: ConsoleKind) => void
  onIconChanged: (target: ConsoleKind, titleId: string, icon: string | null, customIcon: boolean) => void
  onRefreshLibrary: (target: ConsoleKind) => void
  onOptions: (tab: OptionsTab) => void
  onDock: (dock: { context: ReactNode; hints: Hint[] }) => void
}) {
  const tabsRef = useRef<HTMLDivElement>(null)
  const inkRef = useRef<HTMLSpanElement>(null)
  const [panelHints, setPanelHints] = useState<Hint[]>([])
  const current = TOOLS.find(item => item.id === tab) || TOOLS[0]
  const other: ConsoleKind = target === "ps5" ? "ps4" : "ps5"
  // The offline preview behaves as if both consoles were set up.
  const toolSettings = demo ? { ...settings, ps5Host: settings.ps5Host || "192.168.0.20", ps4Host: settings.ps4Host || "192.168.0.21" } : settings

  useLayoutEffect(() => {
    glide(inkRef.current, tabsRef.current?.querySelector<HTMLElement>(`[data-tool="${tab}"]`) || null, tabsRef.current, { liquid: true })
  }, [tab])
  useEffect(() => { setPanelHints([]) }, [tab])

  const live = useRef({ tab, setTab, target, onConsole })
  live.current = { tab, setTab, target, onConsole }
  useEffect(() => setPageKeys(event => {
    if (runPanelKeys(event)) return
    if (isTyping() || event.ctrlKey || event.altKey || event.metaKey) return
    const { setTab, target, onConsole } = live.current
    const k = event.key
    if (k === "1" || k === "2" || k === "3") { event.preventDefault(); setTab(TOOLS[Number(k) - 1].id); return }
    if (k === "c" || k === "C") { event.preventDefault(); onConsole(target === "ps5" ? "ps4" : "ps5") }
  }), [])

  useEffect(() => {
    onDock({
      context: <>Tools<small>{current.label}</small></>,
      hints: [
        ...panelHints,
        { key: "1-3", glyph: "1–3", face: "neutral", label: "Switch tool" },
        { key: "C", glyph: "C", face: "neutral", label: `Use your ${other.toUpperCase()}`, run: () => onConsole(other) },
      ],
    })
  }, [tab, target, panelHints])

  return (
    <div className="page tools-page enter">
      <div className="tl-head">
        <div className="ftabs" ref={tabsRef} role="tablist" aria-label="Tools">
          {TOOLS.map(item => (
            <button key={item.id} type="button" role="tab" data-tool={item.id} className="ftab" aria-selected={tab === item.id} onClick={() => setTab(item.id)}>{item.label}</button>
          ))}
          <span className="ftab-ink" ref={inkRef} />
        </div>
        <p className="tl-detail">{current.detail}</p>
        <ConsoleSwitch value={target} onChange={onConsole} probes={probes} demo={demo} label="Console for tools" />
      </div>
      <div className="tl-body swap-fade" key={tab}>
        {tab === "payloads" && <PayloadsPanel target={target} settings={toolSettings} demo={demo} onReceiverLoaded={onReceiverLoaded} onHints={setPanelHints} />}
        {tab === "system" && <SystemPanel target={target} settings={toolSettings} demo={demo} probe={probes[target]} onLoadReceiver={() => onLoadReceiver(target)} onHints={setPanelHints} />}
        {tab === "themes" && (
          <ThemeStudio
            target={target} settings={toolSettings} demo={demo} probe={probes[target]} library={libraries[target]}
            onIconChanged={onIconChanged} onRefreshLibrary={onRefreshLibrary} onLoadReceiver={onLoadReceiver} onOptions={onOptions} onHints={setPanelHints}
          />
        )}
      </div>
    </div>
  )
}
