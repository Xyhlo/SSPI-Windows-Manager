/* =====================================================================
   Stock console screens for the theme studio, laid out on a 1920 × 1080
   canvas (measured from real PS4 captures) and scaled to fit. Game tiles
   show the theme's icons; everything else is the stock look.
   ===================================================================== */
import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import type { ConsoleKind } from "@/types"

export type MockTile = { key: string; name: string; art: string | null }
export type MockScreen = "home" | "settings"

/* ---------------------------------------------------------------- glyphs (white line art, as on the console) */
const G = {
  info: <><circle cx="24" cy="24" r="17" fill="#fff" /><path d="M24 19v14" stroke="#0a47c2" strokeWidth="5" strokeLinecap="round" /><circle cx="24" cy="13.5" r="3" fill="#0a47c2" /></>,
  friends: <><rect x="6" y="10" width="22" height="20" rx="5" fill="#fff" /><rect x="20" y="18" width="22" height="20" rx="5" fill="#fff" stroke="#0a47c2" strokeWidth="3" /><circle cx="13" cy="19" r="2" fill="#0a47c2" /><circle cx="21" cy="19" r="2" fill="#0a47c2" /><circle cx="27" cy="27" r="2" fill="#0a47c2" /><circle cx="35" cy="27" r="2" fill="#0a47c2" /></>,
  face: <><rect x="7" y="7" width="34" height="34" rx="9" fill="#fff" /><circle cx="18" cy="21" r="3" fill="#0a47c2" /><circle cx="30" cy="21" r="3" fill="#0a47c2" /><path d="M16 29c4 5 12 5 16 0" stroke="#0a47c2" strokeWidth="3.4" fill="none" strokeLinecap="round" /></>,
  trophy: <><path d="M14 8h20v8c0 7-4 12-10 12s-10-5-10-12z" fill="#fff" /><path d="M14 12H7c0 6 3 9 8 9M34 12h7c0 6-3 9-8 9" stroke="#fff" strokeWidth="3.4" fill="none" /><rect x="21" y="27" width="6" height="7" fill="#fff" /><rect x="14" y="34" width="20" height="6" rx="1.5" fill="#fff" /></>,
  help: <><circle cx="20" cy="26" r="13" fill="#fff" /><circle cx="20" cy="26" r="6" fill="#e04b2c" /><path d="M9 26h22M20 15v22" stroke="#e04b2c" strokeWidth="4" /><circle cx="36" cy="12" r="8" fill="#fff" /><path d="M31 23h10M31 27h10" stroke="#fff" strokeWidth="2.4" /></>,
  data: <><rect x="8" y="9" width="32" height="30" rx="5" fill="#fff" /><path d="M24 13v10M19 18h10" stroke="#0a47c2" strokeWidth="3.4" /><circle cx="16" cy="32" r="3" fill="#0a47c2" /><circle cx="32" cy="32" r="3" fill="#0a47c2" /><path d="M8 26h32" stroke="#0a47c2" strokeWidth="2.4" /></>,
  access: <><circle cx="24" cy="24" r="16" fill="none" stroke="#fff" strokeWidth="3.4" /><circle cx="24" cy="14.5" r="3.2" fill="#fff" /><path d="M13 20h22M24 20v9l-6 10M24 29l6 10" stroke="#fff" strokeWidth="3.4" fill="none" strokeLinecap="round" strokeLinejoin="round" /></>,
  shield: <><path d="M24 5l16 6v10c0 11-7 19-16 22C15 40 8 32 8 21V11z" fill="#fff" /><rect x="15" y="16" width="18" height="15" rx="4" fill="#0a47c2" /><circle cx="20.5" cy="22" r="1.8" fill="#fff" /><circle cx="27.5" cy="22" r="1.8" fill="#fff" /></>,
  login: <><path d="M17 40V14a7 7 0 0 1 14 0v26" stroke="#fff" strokeWidth="4.5" fill="none" /><path d="M9 34l8 6M9 40l8-6" stroke="#fff" strokeWidth="3.4" strokeLinecap="round" /></>,
  globe: <><circle cx="24" cy="24" r="16" fill="none" stroke="#fff" strokeWidth="3.4" /><path d="M8 24h32M24 8c5 5 7 10 7 16s-2 11-7 16c-5-5-7-10-7-16s2-11 7-16zM11 15h26M11 33h26" stroke="#fff" strokeWidth="2.8" fill="none" /></>,
  sound: <><path d="M9 19h7l9-8v26l-9-8H9z" fill="#fff" /><path d="M31 17c3 4 3 10 0 14M35 13c5 6 5 16 0 22" stroke="#fff" strokeWidth="3" fill="none" strokeLinecap="round" /></>,
  storage: <><rect x="8" y="12" width="32" height="10" rx="3" fill="#fff" /><rect x="8" y="26" width="32" height="10" rx="3" fill="#fff" /><circle cx="33" cy="17" r="2" fill="#0a47c2" /><circle cx="33" cy="31" r="2" fill="#0a47c2" /></>,
  system: <><rect x="7" y="11" width="34" height="22" rx="3" fill="none" stroke="#fff" strokeWidth="3.4" /><path d="M17 39h14M24 33v6" stroke="#fff" strokeWidth="3.4" /></>,
  themes: <><rect x="8" y="8" width="32" height="32" rx="4" fill="none" stroke="#fff" strokeWidth="3.4" /><path d="M8 30l10-9 8 7 5-4 9 8" stroke="#fff" strokeWidth="3" fill="none" /><circle cx="31" cy="16" r="3.4" fill="#fff" /></>,
  shapes: <><rect x="10" y="10" width="36" height="36" fill="none" stroke="#fff" strokeWidth="5" /><path d="M78 8l20 36H58z" fill="none" stroke="#fff" strokeWidth="5" strokeLinejoin="round" /><path d="M12 62l32 32M44 62L12 94" stroke="#fff" strokeWidth="6" /><circle cx="78" cy="78" r="17" fill="none" stroke="#fff" strokeWidth="5.5" /></>,
}
const Glyph = ({ name, size = 48, box = 48 }: { name: keyof typeof G; size?: number; box?: number }) => (
  <svg width={size} height={size} viewBox={`0 0 ${box} ${box}`} aria-hidden="true">{G[name]}</svg>
)

const Cross = () => <svg width="34" height="34" viewBox="0 0 34 34" aria-hidden="true"><circle cx="17" cy="17" r="15" fill="none" stroke="#dfe7ff" strokeWidth="2" /><path d="M11 11l12 12M23 11L11 23" stroke="#b9c7ff" strokeWidth="2.6" strokeLinecap="round" /></svg>
const Circle = () => <svg width="34" height="34" viewBox="0 0 34 34" aria-hidden="true"><circle cx="17" cy="17" r="15" fill="none" stroke="#dfe7ff" strokeWidth="2" /><circle cx="17" cy="17" r="7.5" fill="none" stroke="#ff6b6b" strokeWidth="2.8" /></svg>
const Key = ({ children }: { children: ReactNode }) => <span className="cm-key">{children}</span>

/* ---------------------------------------------------------------- scaling frame */
export function MockFrame({ children, overlay, className = "" }: { children: ReactNode; overlay?: ReactNode; className?: string }) {
  const boxRef = useRef<HTMLDivElement>(null)
  const [k, setK] = useState(0.4)
  useLayoutEffect(() => {
    const box = boxRef.current
    if (!box) return
    const fit = () => setK(box.clientWidth / 1920)
    fit()
    const observer = new ResizeObserver(fit)
    observer.observe(box)
    return () => observer.disconnect()
  }, [])
  return (
    <div ref={boxRef} className={`cm-box ${className}`}>
      <div className="cm" style={{ transform: `scale(${k})` }}>{children}</div>
      {overlay}
    </div>
  )
}

/* ---------------------------------------------------------------- PS4 */
export function Ps4Home({ tiles, systemTiles = [], focusName }: { tiles: MockTile[]; systemTiles?: MockTile[]; focusName?: string }) {
  const [focus, ...rest] = tiles
  // Library, TV & Video and Internet Browser sit at the end of the row, after the games.
  const row = [...rest.slice(0, 7 - Math.min(3, systemTiles.length)), ...systemTiles.slice(0, 3)]
  const clock = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })
  return (
    <>
      <div className="p4-fn">
        <span className="p4-fn-note"><span className="p4-badge">3</span><Glyph name="info" /><span className="p4-fn-text">: Version 1.04 Update File</span></span>
        <span className="p4-fn-item" style={{ left: 724 }}><Glyph name="friends" /></span>
        <span className="p4-fn-item p4-user" style={{ left: 870 }}><Glyph name="face" /><i>User</i></span>
        <span className="p4-fn-item" style={{ left: 1304 }}><Glyph name="trophy" /><span className="p4-fn-dash">-</span></span>
        <span className="p4-clock">{clock}</span>
      </div>
      <span className="p4-tile p4-sys p4-partial" />
      <span className="p4-tile p4-sys" style={{ left: 73 }}><svg width="120" height="120" viewBox="0 0 104 104" aria-hidden="true">{G.shapes}</svg></span>
      <span className="p4-focus">
        <span className="p4-focus-art">{focus?.art ? <img src={focus.art} alt="" draggable={false} /> : null}</span>
        <span className="p4-start">Start</span>
      </span>
      {row.map((tile, n) => (
        <span key={tile.key} className={`p4-tile ${tile.key.startsWith("sys:") ? "p4-app" : ""}`} style={{ left: 632 + n * 204 }}>{tile.art ? <img src={tile.art} alt="" draggable={false} /> : null}</span>
      ))}
      <span className="p4-title">{focusName || focus?.name || ""}</span>
      <Ps4Hints items={[[<Cross key="x" />, "Enter"], [<Key key="l1">L1</Key>, "To the Left End"], [<Key key="r1">R1</Key>, "To the Right End"], [<Key key="op">OPTIONS</Key>, "Options Menu"]]} />
    </>
  )
}

const PS4_SETTINGS: Array<[keyof typeof G, string]> = [
  ["help", "User's Guide/Helpful Info"], ["data", "Data Collection/Health & Safety"], ["access", "Accessibility"], ["face", "Account Management"],
  ["shield", "Parental Controls/Family Management"], ["login", "Login Settings"], ["globe", "Network"], ["info", "Notifications"], ["sound", "Sound and Screen"],
]

export function Ps4Settings() {
  return (
    <>
      <h1 className="p4-page-title">Settings</h1>
      <span className="p4-page-rule" />
      <div className="p4-list">
        {PS4_SETTINGS.map(([glyph, text], n) => (
          <div key={text} className={`p4-item ${n === 0 ? "focus" : ""}`}>
            <span className="p4-item-icon"><Glyph name={glyph} /></span>
            <span className="p4-item-text">{text}</span>
          </div>
        ))}
      </div>
      <span className="p4-scroll"><i /></span>
      <Ps4Hints items={[[<Cross key="x" />, "Enter"], [<Circle key="o" />, "Back"]]} />
    </>
  )
}

function Ps4Hints({ items }: { items: Array<[ReactNode, string]> }) {
  return (
    <div className="p4-hints">
      <span className="p4-hints-rule" />
      <div className="p4-hints-row">
        {items.map(([glyph, text]) => <span key={text} className="p4-hint">{glyph}<span>{text}</span></span>)}
      </div>
      <span className="p4-hint p4-hint-user"><Glyph name="face" size={40} /><i>User</i></span>
    </div>
  )
}

/* ---------------------------------------------------------------- PS5 */
export function Ps5Home({ tiles, focusName }: { tiles: MockTile[]; focusName?: string }) {
  const [focus, ...rest] = tiles
  const clock = new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
  return (
    <>
      <div className="p5-top">
        <span className="p5-tabs"><b>Games</b><span>Media</span></span>
        <span className="p5-status"><i className="p5-ico" /><i className="p5-ico" /><i className="p5-avatar" /><span>{clock}</span></span>
      </div>
      <div className="p5-row">
        <span className="p5-tile p5-sys" />
        <span className="p5-tile p5-focus">{focus?.art ? <img src={focus.art} alt="" draggable={false} /> : null}</span>
        {rest.slice(0, 8).map(tile => <span key={tile.key} className="p5-tile">{tile.art ? <img src={tile.art} alt="" draggable={false} /> : null}</span>)}
      </div>
      <div className="p5-hero">
        <strong>{focusName || focus?.name || ""}</strong>
        <span className="p5-play">Play</span>
      </div>
    </>
  )
}

const PS5_SETTINGS = ["Accessibility", "Network", "Users and Accounts", "Family and Parental Controls", "System", "Storage", "Sound", "Screen and Video", "Saved Data and Game/App Settings"]
export function Ps5Settings() {
  return (
    <>
      <h1 className="p5-page-title">Settings</h1>
      <div className="p5-list">
        {PS5_SETTINGS.map((text, n) => <div key={text} className={`p5-item ${n === 0 ? "focus" : ""}`}><i />{text}</div>)}
      </div>
    </>
  )
}

export function ConsoleScreen({ target, screen, tiles, systemTiles }: { target: ConsoleKind; screen: MockScreen; tiles: MockTile[]; systemTiles?: MockTile[] }) {
  if (target === "ps4") return screen === "home" ? <Ps4Home tiles={tiles} systemTiles={systemTiles} /> : <Ps4Settings />
  return screen === "home" ? <Ps5Home tiles={tiles} /> : <Ps5Settings />
}
