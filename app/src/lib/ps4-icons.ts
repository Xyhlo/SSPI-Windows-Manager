/* =====================================================================
   The icons a PS4 system theme can replace, and SSPI's own line art
   for each one: a retro spin-off of the console's motifs, drawn as
   vector paths in a 512-unit box so every size stays crisp. Tone 0 and
   1 are the theme's two colours (magenta and cyan in Retro neon).
   ===================================================================== */

export type ContentSlot = "library" | "tvvideo" | "browser" | "gallery" | "livefromps" | "usbmusic" | "shareplay" | "folder" | "disc" | "discoverlay"
export type FunctionSlot = "notification" | "friend" | "event" | "message" | "party" | "community" | "trophy" | "setting" | "power"
export type SlotId = ContentSlot | FunctionSlot
export type SlotInfo = { id: SlotId; area: "content" | "function"; label: string; subject: string; size: number; note?: string }

/** Content-area tiles: 512 × 512 PNG, transparent pixels allowed. */
export const CONTENT_SLOTS: SlotInfo[] = [
  { id: "library", area: "content", label: "Library", subject: "a bookshelf with a row of books, one leaning", size: 512 },
  { id: "tvvideo", area: "content", label: "TV & Video", subject: "a retro television with a play button and two antennas", size: 512 },
  { id: "browser", area: "content", label: "Internet Browser", subject: "the letters WWW inside a ring of dots", size: 512 },
  { id: "gallery", area: "content", label: "Capture Gallery", subject: "a framed landscape photo with a film strip behind it", size: 512 },
  { id: "livefromps", area: "content", label: "Live from PlayStation", subject: "a grid of video windows with a camera badge", size: 512 },
  { id: "usbmusic", area: "content", label: "USB Music Player", subject: "a pair of music notes and a USB symbol", size: 512 },
  { id: "shareplay", area: "content", label: "Share Play", subject: "two game controllers side by side", size: 512 },
  { id: "folder", area: "content", label: "Folders", subject: "a folder with a small sparkle", size: 512, note: "Shown for folders you make on the home screen." },
  { id: "disc", area: "content", label: "Disc", subject: "an optical disc", size: 512, note: "Shown while a disc is inserted." },
  { id: "discoverlay", area: "content", label: "Disc badge", subject: "a small disc badge in the lower-right corner, everything else transparent", size: 512, note: "Laid over disc games' tiles, so keep it small and keep the rest transparent." },
]
/** Function-area icons along the top of the home screen: 128 × 128 PNG with a transparent background. */
export const FUNCTION_SLOTS: SlotInfo[] = [
  { id: "notification", area: "function", label: "Notifications", subject: "the letter i inside a speech bubble", size: 128 },
  { id: "friend", area: "function", label: "Friends", subject: "two smiling faces, one behind the other", size: 128 },
  { id: "event", area: "function", label: "Events", subject: "a calendar page showing 31", size: 128 },
  { id: "message", area: "function", label: "Messages", subject: "two overlapping speech bubbles", size: 128 },
  { id: "party", area: "function", label: "Party", subject: "a gaming headset with a microphone", size: 128 },
  { id: "community", area: "function", label: "Communities", subject: "a group of three people", size: 128, note: "Only firmware that still has Communities shows it." },
  { id: "trophy", area: "function", label: "Trophies", subject: "a trophy cup with a star", size: 128 },
  { id: "setting", area: "function", label: "Settings", subject: "a toolbox", size: 128 },
  { id: "power", area: "function", label: "Power", subject: "a power symbol", size: 128 },
]
export const SLOTS: SlotInfo[] = [...CONTENT_SLOTS, ...FUNCTION_SLOTS]
export const slotInfo = (id: SlotId) => SLOTS.find(slot => slot.id === id)!

/* ---------------------------------------------------------------- import names */

const ALIASES: Record<string, SlotId | "wallpaper"> = {
  tvandvideo: "tvvideo", tv: "tvvideo", internetbrowser: "browser", www: "browser", capturegallery: "gallery",
  livefromplaystation: "livefromps", usbmusicplayer: "usbmusic", music: "usbmusic", folders: "folder", discbadge: "discoverlay",
  notifications: "notification", friends: "friend", events: "event", messages: "message", communities: "community",
  trophies: "trophy", settings: "setting", background: "wallpaper", home: "wallpaper",
}
/** Which slot a file belongs to, from its name: `library.png`, `TV & Video.png`, `top-trophies.png`, `wallpaper.jpg`... */
export function slotForFile(fileName: string): SlotId | "wallpaper" | null {
  const base = (fileName.split(/[\\/]/).pop() || "").replace(/\.[a-z0-9]+$/i, "").toLowerCase()
  const key = base.replace(/^(top|content|function|tile|icon)[-_ ]+/, "").replace(/[^a-z0-9]/g, "")
  if (key === "wallpaper") return "wallpaper"
  if (SLOTS.some(slot => slot.id === key)) return key as SlotId
  return ALIASES[key] ?? null
}

/* ---------------------------------------------------------------- geometry helpers (SVG path data) */

const f = (n: number) => Number(n.toFixed(2))
export const rr = (x: number, y: number, w: number, h: number, r: number) =>
  `M${x + r} ${y}H${x + w - r}A${r} ${r} 0 0 1 ${x + w} ${y + r}V${y + h - r}A${r} ${r} 0 0 1 ${x + w - r} ${y + h}H${x + r}A${r} ${r} 0 0 1 ${x} ${y + h - r}V${y + r}A${r} ${r} 0 0 1 ${x + r} ${y}Z`
export const circle = (cx: number, cy: number, r: number) => `M${cx - r} ${cy}A${r} ${r} 0 1 0 ${cx + r} ${cy}A${r} ${r} 0 1 0 ${cx - r} ${cy}Z`
const ellipse = (cx: number, cy: number, rx: number, ry: number, deg: number) => {
  const t = (deg * Math.PI) / 180, dx = rx * Math.cos(t), dy = rx * Math.sin(t)
  return `M${f(cx - dx)} ${f(cy - dy)}A${rx} ${ry} ${deg} 1 0 ${f(cx + dx)} ${f(cy + dy)}A${rx} ${ry} ${deg} 1 0 ${f(cx - dx)} ${f(cy - dy)}Z`
}
/** An open arc from `a0` to `a1` degrees, clockwise on screen. */
const arc = (cx: number, cy: number, r: number, a0: number, a1: number) => {
  const p = (a: number) => `${f(cx + r * Math.cos((a * Math.PI) / 180))} ${f(cy + r * Math.sin((a * Math.PI) / 180))}`
  return `M${p(a0)}A${r} ${r} 0 ${a1 - a0 > 180 ? 1 : 0} 1 ${p(a1)}`
}
const star = (cx: number, cy: number, outer: number, inner: number, points = 5) =>
  Array.from({ length: points * 2 }, (_, i) => {
    const r = i % 2 ? inner : outer, a = (i * Math.PI) / points - Math.PI / 2
    return `${i ? "L" : "M"}${f(cx + r * Math.cos(a))} ${f(cy + r * Math.sin(a))}`
  }).join("") + "Z"
const dots = (points: Array<[number, number]>, r: number) => points.map(([x, y]) => circle(x, y, r)).join("")
/** A game controller outline with its d-pad and face buttons, 224 × 120 units from (x, y). */
const controller = (x: number, y: number) => [
  `M${x + 42} ${y}H${x + 182}Q${x + 208} ${y} ${x + 212} ${y + 26}L${x + 224} ${y + 92}Q${x + 228} ${y + 120} ${x + 202} ${y + 120}Q${x + 186} ${y + 120} ${x + 174} ${y + 102}L${x + 162} ${y + 84}H${x + 62}L${x + 50} ${y + 102}Q${x + 38} ${y + 120} ${x + 22} ${y + 120}Q${x - 4} ${y + 120} ${x} ${y + 92}L${x + 12} ${y + 26}Q${x + 16} ${y} ${x + 42} ${y}Z`,
  `M${x + 58} ${y + 30}V${y + 62}M${x + 42} ${y + 46}H${x + 74}`,
]

/* ---------------------------------------------------------------- the art */

/** One stroke of an icon: SVG path data, which theme colour it takes, filled or outlined, and its width. */
export type Stroke = { d: string; tone: 0 | 1; fill?: boolean; w?: number }
const W = (cx: number) => `M${cx - 36} 222L${cx - 18} 290L${cx} 238L${cx + 18} 290L${cx + 36} 222`
const ring = [-110, -70, -140, -40, 40, 140, 70, 110].map(a => [256 + 150 * Math.cos((a * Math.PI) / 180), 256 + 150 * Math.sin((a * Math.PI) / 180)] as [number, number])

export const ART: Record<SlotId, Stroke[]> = {
  library: [
    { d: rr(128, 146, 256, 220, 18), tone: 0 },
    { d: "M128 256H384", tone: 0 },
    { d: "M164 172V234M190 172V234M216 172V234M242 172V234M322 172V234M348 172V234M164 282V344M190 282V344M268 282V344M294 282V344M320 282V344M348 282V344", tone: 1, w: 16 },
    { d: "M268 234L292 176M214 344L238 286", tone: 1, w: 16 },
  ],
  tvvideo: [
    { d: rr(124, 160, 264, 176, 24), tone: 0 },
    { d: "M256 336V368M196 372H316", tone: 0 },
    { d: "M232 210L300 248L232 286Z", tone: 1, fill: true },
    { d: "M214 160L182 116M298 160L330 116", tone: 1, w: 16 },
  ],
  browser: [
    { d: W(160) + W(256) + W(352), tone: 0, w: 16 },
    { d: dots(ring, 11), tone: 1, fill: true, w: 8 },
  ],
  gallery: [
    { d: "M206 188V148A16 16 0 0 1 222 132H368A16 16 0 0 1 384 148V244A16 16 0 0 1 368 260H336", tone: 1 },
    { d: [146, 174, 202, 230].map(y => rr(350, y, 14, 14, 3)).join(""), tone: 1, fill: true, w: 8 },
    { d: rr(122, 188, 214, 160, 18), tone: 0 },
    { d: "M142 326L194 266L228 300L262 256L316 326", tone: 0, w: 18 },
    { d: circle(176, 234, 16), tone: 1, fill: true, w: 10 },
  ],
  livefromps: [
    { d: rr(124, 142, 124, 56, 10) + rr(260, 142, 128, 56, 10) + rr(124, 210, 72, 56, 10) + rr(208, 210, 72, 56, 10) + rr(292, 210, 96, 56, 10) + rr(124, 278, 72, 56, 10) + rr(208, 278, 56, 56, 10), tone: 0, w: 16 },
    { d: circle(334, 334, 54), tone: 1, w: 18 },
    { d: rr(304, 318, 42, 32, 8) + "M348 326L370 314V354L348 342Z", tone: 1, fill: true, w: 8 },
  ],
  usbmusic: [
    { d: "M208 330V178L338 150V302M208 206L338 178", tone: 0 },
    { d: ellipse(178, 332, 32, 24, -20) + ellipse(308, 304, 32, 24, -20), tone: 0, fill: true, w: 12 },
    { d: "M384 384V292M384 352L362 338V320M384 334L404 322V306", tone: 1, w: 12 },
    { d: "M372 300L384 278L396 300Z" + circle(384, 392, 10) + circle(362, 312, 8) + rr(397, 292, 14, 14, 2), tone: 1, fill: true, w: 6 },
  ],
  shareplay: [
    { d: controller(98, 150).join(""), tone: 0, w: 18 },
    { d: dots([[260, 194], [276, 208], [260, 222], [244, 208]], 6), tone: 0, fill: true, w: 6 },
    { d: controller(190, 262).join(""), tone: 1, w: 18 },
    { d: dots([[352, 306], [368, 320], [352, 334], [336, 320]], 6), tone: 1, fill: true, w: 6 },
  ],
  folder: [
    { d: "M128 168H210L238 196H384V352H128Z", tone: 0 },
    { d: "M128 228H384", tone: 1 },
    { d: star(316, 292, 30, 9, 4), tone: 1, fill: true, w: 8 },
  ],
  disc: [
    { d: circle(256, 256, 122), tone: 0 },
    { d: circle(256, 256, 30), tone: 1 },
    { d: arc(256, 256, 80, -160, -110) + arc(256, 256, 80, 20, 70), tone: 1, w: 14 },
  ],
  discoverlay: [
    { d: circle(420, 420, 56), tone: 1, w: 14 },
    { d: circle(420, 420, 14), tone: 0, fill: true, w: 8 },
  ],
  notification: [
    { d: "M256 88C360 88 428 158 428 246C428 304 398 350 352 378L388 432L302 400C287 403 272 404 256 404C152 404 84 334 84 246C84 158 152 88 256 88Z", tone: 0, w: 26 },
    { d: circle(256, 176, 22), tone: 1, fill: true, w: 8 },
    { d: "M256 228V330", tone: 1, w: 32 },
  ],
  friend: [
    { d: "M196 290H136A52 52 0 0 1 84 238V164A52 52 0 0 1 136 112H232A52 52 0 0 1 284 164V190", tone: 1, w: 22 },
    { d: dots([[140, 180], [204, 180]], 12), tone: 1, fill: true, w: 6 },
    { d: rr(196, 190, 228, 208, 58), tone: 0, w: 24 },
    { d: dots([[268, 270], [352, 270]], 15), tone: 0, fill: true, w: 6 },
    { d: arc(310, 300, 52, 25, 155), tone: 0, w: 20 },
  ],
  event: [
    { d: rr(104, 126, 304, 284, 44), tone: 0, w: 24 },
    { d: "M180 94V156M332 94V156", tone: 1, w: 24 },
    { d: "M192 230Q206 214 230 214Q262 214 262 242Q262 264 232 266Q264 268 264 296Q264 326 230 326Q204 326 190 310M300 234L330 214V326", tone: 1, w: 22 },
  ],
  message: [
    { d: "M172 264H136A52 52 0 0 1 84 212V152A52 52 0 0 1 136 100H268A52 52 0 0 1 320 152V190M128 264L108 312L164 264", tone: 1, w: 22 },
    { d: rr(172, 190, 256, 176, 56) + "M372 366L398 420L320 366", tone: 0, w: 24 },
    { d: dots([[236, 278], [300, 278], [364, 278]], 14), tone: 0, fill: true, w: 6 },
  ],
  party: [
    { d: "M120 300V256A136 136 0 0 1 392 256V300", tone: 0, w: 26 },
    { d: rr(96, 276, 64, 112, 26) + rr(352, 276, 64, 112, 26), tone: 0, fill: true, w: 18 },
    { d: "M384 388Q380 432 312 432", tone: 1, w: 18 },
    { d: circle(300, 432, 15), tone: 1, fill: true, w: 6 },
  ],
  community: [
    { d: circle(256, 170, 50) + "M164 360Q164 262 256 262Q348 262 348 360", tone: 0, w: 24 },
    { d: circle(132, 214, 36) + circle(380, 214, 36) + "M70 360Q70 296 132 290M442 360Q442 296 380 290", tone: 1, w: 20 },
  ],
  trophy: [
    { d: "M164 104H348V196C348 268 306 306 256 310C206 306 164 268 164 196Z", tone: 0, w: 24 },
    { d: "M164 136H112C108 200 132 240 178 246M348 136H400C404 200 380 240 334 246", tone: 1, w: 20 },
    { d: "M256 310V364M196 408H316L300 364H212Z", tone: 0, w: 22 },
    { d: star(256, 196, 40, 17), tone: 1, fill: true, w: 8 },
  ],
  setting: [
    { d: rr(88, 196, 336, 196, 28), tone: 0, w: 24 },
    { d: "M196 196V156A24 24 0 0 1 220 132H292A24 24 0 0 1 316 156V196", tone: 1, w: 22 },
    { d: "M88 268H222M290 268H424", tone: 0, w: 18 },
    { d: rr(228, 246, 56, 44, 10), tone: 1, fill: true, w: 10 },
  ],
  power: [
    { d: arc(256, 268, 136, -55, 235), tone: 0, w: 30 },
    { d: "M256 96V258", tone: 1, w: 30 },
  ],
}

/* ---------------------------------------------------------------- prompts for image generators */

/** A ready-to-paste prompt for an AI image generator, with the exact size and format the console takes. */
export function iconPrompt(slot: SlotInfo, colors: { primary: string; secondary: string }, style: "neon" | "line" = "neon") {
  const look = style === "neon"
    ? `retro 1980s neon sign style: glowing ${colors.primary} and ${colors.secondary} tubes with a soft bloom`
    : `clean modern line-icon style in white with ${colors.primary} accents`
  if (slot.area === "function") {
    return `Create a ${slot.size} × ${slot.size} pixel PNG icon with a fully transparent background: ${slot.subject}, ${look}. ` +
      `It is the "${slot.label}" icon in the PlayStation 4 top menu, so keep it a bold, simple symbol that still reads at 32 pixels, centred with a little padding. No text, no logos, no background, no drop shadow box.`
  }
  const extra = slot.id === "discoverlay" ? " Only the badge is visible; every other pixel must be transparent." : " A dark background inside the tile is fine; transparent corners are fine too."
  return `Create a ${slot.size} × ${slot.size} pixel square PNG icon: ${slot.subject}, ${look}. ` +
    `It is the "${slot.label}" tile on the PlayStation 4 home screen, so keep the subject centred and large, readable at 200 pixels. No text, no logos, no watermark.${extra}`
}
