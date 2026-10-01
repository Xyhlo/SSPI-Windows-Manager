/* =====================================================================
   A PS4 screen mock on the console's own 1920 × 1080 layout (home row,
   top menu, Settings), scaled to fit. It shows either the stock look or
   the theme, and can mark which parts the theme changes. Clicking a
   tile or icon focuses it like the console does, and picks it for
   editing.
   ===================================================================== */
import { useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode } from "react"
import type { ContentSlot, FunctionSlot, SlotId } from "@/lib/ps4-icons"
import { slotInfo } from "@/lib/ps4-icons"

export type PreviewScreen = "home" | "menu" | "settings"
/** `system` holds the console's own white glyphs for screens themes don't change (the Settings list). */
export type PreviewArt = { content: Record<ContentSlot, string>; fn: Record<FunctionSlot, string>; home: string; menu: string; system: Partial<Record<SlotId, string>> }
export type PreviewGame = { titleId: string; name: string; src: string }
type Item = { key: string; label: string; src?: string; slot?: ContentSlot; game?: boolean }

const SYSTEM_ROW: ContentSlot[] = ["tvvideo", "livefromps", "gallery", "library", "browser", "usbmusic", "shareplay", "folder", "disc"]
const MENU_ROW: Array<FunctionSlot | "profile"> = ["notification", "friend", "event", "message", "party", "profile", "trophy", "setting", "power"]
const SETTINGS = ["Themes", "Network", "Notifications", "Sound and Screen", "Accessibility", "Account Management", "Parental Controls", "System"]
export const SETTING_ART: SlotId[] = ["gallery", "browser", "notification", "tvvideo", "community", "friend", "event", "setting"]

/* Small stock pieces drawn as SVG: they belong to the console, not to themes. */
const Face = ({ size }: { size: number }) => (
  <svg width={size} height={size} viewBox="0 0 40 40" aria-hidden="true"><rect x="3" y="3" width="34" height="34" rx="10" fill="#fff" /><circle cx="14" cy="17" r="3" fill="#1b3f9c" /><circle cx="26" cy="17" r="3" fill="#1b3f9c" /><path d="M12 25q8 7 16 0" stroke="#1b3f9c" strokeWidth="3" fill="none" strokeLinecap="round" /></svg>
)
const Cross = () => <svg width="34" height="34" viewBox="0 0 34 34" aria-hidden="true"><circle cx="17" cy="17" r="14" fill="none" stroke="#fff" strokeWidth="2.5" /><path d="M11 11l12 12M23 11L11 23" stroke="#fff" strokeWidth="2.5" /></svg>
const Ring = () => <svg width="34" height="34" viewBox="0 0 34 34" aria-hidden="true"><circle cx="17" cy="17" r="14" fill="none" stroke="#fff" strokeWidth="2.5" /><circle cx="17" cy="17" r="8" fill="none" stroke="#ff5a5a" strokeWidth="3" /></svg>
const Key = ({ children }: { children: ReactNode }) => <span className="ps4-key">{children}</span>
const WhatsNew = () => (
  <svg viewBox="0 0 200 200" width="100%" height="100%" aria-hidden="true">
    <defs><linearGradient id="ps4-wn" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stopColor="#2862d6" /><stop offset="1" stopColor="#1442a8" /></linearGradient></defs>
    <rect width="200" height="200" fill="url(#ps4-wn)" />
    <g fill="none" stroke="#fff" strokeWidth="9" strokeLinejoin="round"><rect x="44" y="46" width="44" height="44" /><path d="M134 44l26 46h-52z" /><path d="M48 112l40 40M88 112l-40 40" strokeLinecap="round" /><circle cx="134" cy="132" r="22" /></g>
  </svg>
)

function Hints({ items }: { items: ReactNode[] }) {
  return (
    <div className="ps4-hints">
      <div className="ps4-hint-row">{items.map((item, i) => <span key={i} className="ps4-hint">{item}</span>)}</div>
      <span className="ps4-user"><Face size={38} />Player</span>
    </div>
  )
}

export function Ps4Preview({ screen, themed, marks, art, games, focus, onPick }: {
  screen: PreviewScreen; themed: boolean; marks: boolean; art: PreviewArt; games: PreviewGame[]; focus: string
  onPick?: (slot: SlotId) => void
}) {
  const wrap = useRef<HTMLDivElement>(null)
  const [scale, setScale] = useState(0.4)
  const [rowFocus, setRowFocus] = useState(1)
  const [menuFocus, setMenuFocus] = useState(0)
  useLayoutEffect(() => {
    const el = wrap.current
    if (!el) return
    const measure = () => setScale(el.clientWidth / 1920)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  const items: Item[] = [
    { key: "whatsnew", label: "What's New" },
    ...games.slice(0, 4).map(game => ({ key: game.titleId, label: game.name, src: game.src, game: true })),
    ...SYSTEM_ROW.map(slot => ({ key: slot, label: slotInfo(slot).label, src: art.content[slot], slot })),
  ]
  const f = Math.min(rowFocus, items.length - 1)
  const mark = (kind: "theme" | "game" | "fixed") => marks ? ` mark-${kind}` : ""
  const pick = (slot?: SlotId) => { if (slot && themed) onPick?.(slot) }
  const latest = games[0]?.name || "Super Simple Package Installer"
  const style = { "--focus": themed ? focus : "#ffffff" } as CSSProperties

  return (
    <div className={`ps4 ${themed ? "is-themed" : "is-stock"}`} ref={wrap}>
      <div className="ps4-stage" style={{ ...style, transform: `scale(${scale})` }}>
        {screen === "home" && (
          <div className={`ps4-screen${mark("theme")}`} style={{ backgroundImage: `url(${art.home})` }}>
            <div className="ps4-top">
              <span className="ps4-count" style={{ left: 208 }}>1</span>
              <button type="button" className={`ps4-top-icon${mark("theme")}`} style={{ left: 192 }} onClick={() => pick("notification")} aria-label="Notifications"><img src={art.fn.notification} alt="" /></button>
              <span className="ps4-top-text ps4-latest" style={{ left: 258 }}>Ready to use.&nbsp;&nbsp;{latest}</span>
              <button type="button" className={`ps4-top-icon${mark("theme")}`} style={{ left: 718 }} onClick={() => pick("friend")} aria-label="Friends"><img src={art.fn.friend} alt="" /></button>
              <span className={`ps4-top-face${mark("fixed")}`} style={{ left: 862 }}><Face size={44} /><i>Player</i></span>
              <button type="button" className={`ps4-top-icon${mark("theme")}`} style={{ left: 1296 }} onClick={() => pick("trophy")} aria-label="Trophies"><img src={art.fn.trophy} alt="" /></button>
              <span className="ps4-top-text" style={{ left: 1356 }}>★ -</span>
              <span className="ps4-clock">22:58</span>
            </div>
            <div className="ps4-row">
              {items.map((item, i) => {
                const left = i < f ? 73 - (f - 1 - i) * 204 : i === f ? 283 : 632 + (i - f - 1) * 204
                const kind = item.slot ? "theme" : "fixed"
                return (
                  <button key={item.key} type="button" className={`ps4-tile${i === f ? " is-focus" : ""}${mark(kind)}`} style={{ left }}
                    onClick={() => { setRowFocus(i); pick(item.slot) }} aria-label={item.label}>
                    <span className="ps4-art">{item.src ? <img src={item.src} alt="" /> : <WhatsNew />}</span>
                    {i === f && <span className="ps4-start">Start</span>}
                  </button>
                )
              })}
            </div>
            <p className="ps4-title">{items[f]?.label}</p>
            <Hints items={[<><Cross />Enter</>, <><Key>L1</Key>To the Left End</>, <><Key>R1</Key>To the Right End</>, <><Key>OPTIONS</Key>Options Menu</>]} />
          </div>
        )}
        {screen === "menu" && (
          <div className={`ps4-screen${mark("theme")}`} style={{ backgroundImage: `url(${art.menu})` }}>
            <p className="ps4-menu-head">New Notifications</p>
            <div className="ps4-note">
              <span className="ps4-note-icon"><svg viewBox="0 0 128 128" width="128" height="128" aria-hidden="true"><path d="M64 34v4M64 50v4M64 66v4M44 78h40L64 100z" stroke="#fff" strokeWidth="7" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg></span>
              <b>Ready to use.</b><span>{latest}</span><em>Yesterday, 22:53</em>
            </div>
            <div className="ps4-menu-row">
              {MENU_ROW.map((slot, i) => {
                const focused = i === menuFocus, label = slot === "profile" ? "Profile" : slotInfo(slot).label
                return (
                  <button key={slot} type="button" className={`ps4-menu-icon${focused ? " is-focus" : ""}${mark(slot === "profile" ? "fixed" : "theme")}`} style={{ left: 330 + i * 168 }}
                    onClick={() => { setMenuFocus(i); if (slot !== "profile") pick(slot) }} aria-label={label}>
                    {slot === "notification" && <span className="ps4-count">1</span>}
                    {slot === "profile" ? <Face size={focused ? 104 : 60} /> : <img src={art.fn[slot]} alt="" />}
                    {focused && <span className="ps4-menu-label">{label}</span>}
                  </button>
                )
              })}
            </div>
            <div className="ps4-peek">{SYSTEM_ROW.slice(0, 7).map(slot => <img key={slot} src={art.content[slot]} alt="" />)}</div>
            <Hints items={[<><Cross />Enter</>, <><Ring />Back</>]} />
          </div>
        )}
        {screen === "settings" && (
          <div className={`ps4-screen${mark("theme")}`} style={{ backgroundImage: `url(${art.menu})` }}>
            <p className="ps4-page-head">Settings</p>
            <div className="ps4-list">
              {SETTINGS.map((label, i) => (
                <div key={label} className={`ps4-item${i === 0 ? " is-focus" : ""}`}>
                  <span className={`ps4-item-icon${mark("fixed")}`}><img src={art.system[SETTING_ART[i]]} alt="" /></span>
                  {label}
                </div>
              ))}
            </div>
            <span className="ps4-scroll"><i /></span>
            <Hints items={[<><Cross />Enter</>, <><Ring />Back</>]} />
          </div>
        )}
        {marks && (
          <div className="ps4-legend">
            <span><i className="theme" />Changed by the theme</span>
            <span><i className="fixed" />Stays as it is</span>
          </div>
        )}
      </div>
    </div>
  )
}
