/* Stroke icons drawn for the SSPI Windows interface (24 × 24, 1.7 stroke). */
export const ICONS = {
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>',
  download: '<path d="M12 4v11"/><path d="m7 10.5 5 5 5-5"/><path d="M5 20h14"/>',
  upload: '<path d="M12 20V9"/><path d="m7 13.5 5-5 5 5"/><path d="M5 4h14"/>',
  left: '<path d="M19 12H5"/><path d="m11 18-6-6 6-6"/>',
  right: '<path d="M5 12h14"/><path d="m13 6 6 6-6 6"/>',
  chevL: '<path d="m15 6-6 6 6 6"/>',
  chevR: '<path d="m9 6 6 6-6 6"/>',
  chevD: '<path d="m6 9 6 6 6-6"/>',
  check: '<path d="M5 12.5 10 17.5 19.5 7"/>',
  x: '<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>',
  minus: '<path d="M5 12h14"/>',
  square: '<rect x="5.5" y="5.5" width="13" height="13" rx="1"/>',
  pause: '<path d="M9 6.5v11M15 6.5v11"/>',
  play: '<path d="M8 5.8v12.4a.6.6 0 0 0 .9.5l10-6.2a.6.6 0 0 0 0-1l-10-6.2a.6.6 0 0 0-.9.5z"/>',
  retry: '<path d="M4.5 12a7.5 7.5 0 1 0 2.2-5.3"/><path d="M4.5 4.5v4h4"/>',
  trash: '<path d="M4 7h16"/><path d="M10 11v6M14 11v6"/><path d="M6 7l.9 12a1.6 1.6 0 0 0 1.6 1.5h7a1.6 1.6 0 0 0 1.6-1.5L18 7"/><path d="M9 7V4.8h6V7"/>',
  folder: '<path d="M3.5 7.5A1.5 1.5 0 0 1 5 6h4l2 2.5h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19.5H5A1.5 1.5 0 0 1 3.5 18z"/>',
  box: '<path d="M20.5 7.8 12 3.3 3.5 7.8v8.4l8.5 4.5 8.5-4.5z"/><path d="m3.5 7.8 8.5 4.5 8.5-4.5"/><path d="M12 12.3v8.4"/>',
  library: '<path d="M5 4.5v15M9.5 4.5v15"/><path d="m14 5 4.5 14"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5"/><path d="M12 7.8h.01"/>',
  alert: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5v5.2"/><path d="M12 16.1h.01"/>',
  warn: '<path d="M12 4.5 20.5 19h-17z"/><path d="M12 10v4"/><path d="M12 16.6h.01"/>',
  checkCircle: '<circle cx="12" cy="12" r="8.5"/><path d="m8.3 12.3 2.6 2.6 5-5.3"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  monitor: '<rect x="3.5" y="4.5" width="17" height="11.5" rx="1.8"/><path d="M9 20h6M12 16v4"/>',
  shield: '<path d="M12 3.5 5 6.2v5.3c0 4.3 2.9 7.6 7 9 4.1-1.4 7-4.7 7-9V6.2z"/><path d="m9 12 2.1 2.1L15.3 10"/>',
  archive: '<rect x="4" y="4" width="16" height="16" rx="2"/><path d="M12 4v2M12 8v2M12 12v2"/><rect x="10.5" y="14" width="3" height="3.5" rx=".8"/>',
  layers: '<path d="m12 4 8.5 4.5L12 13 3.5 8.5z"/><path d="m3.5 12.5 8.5 4.5 8.5-4.5"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l2.8-2.8a4 4 0 0 0-5.7-5.7L11.5 6.8"/><path d="M14 10a4 4 0 0 0-5.7 0l-2.8 2.8a4 4 0 0 0 5.7 5.7l1.3-1.3"/>',
  key: '<circle cx="8" cy="15" r="3.5"/><path d="m10.5 12.5 8-8M16 7l2 2M14 9l1.5 1.5"/>',
  lock: '<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8 10.5V8a4 4 0 0 1 8 0v2.5"/>',
  globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17"/><path d="M12 3.5c2.4 2.4 3.4 5.2 3.4 8.5s-1 6.1-3.4 8.5c-2.4-2.4-3.4-5.2-3.4-8.5s1-6.1 3.4-8.5z"/>',
  cpu: '<rect x="6.5" y="6.5" width="11" height="11" rx="1.5"/><path d="M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3"/>',
  refresh: '<path d="M19.5 12a7.5 7.5 0 0 1-13.1 5"/><path d="M4.5 12a7.5 7.5 0 0 1 13.1-5"/><path d="M17.8 3.5v3.7h-3.7M6.2 20.5v-3.7h3.7"/>',
  signal: '<path d="M3 9.2a13.5 13.5 0 0 1 18 0"/><path d="M6 12.7a9 9 0 0 1 12 0"/><path d="M9.2 16.1a4.4 4.4 0 0 1 5.6 0"/><path d="M12 19.5h.01"/>',
  drive: '<rect x="3.5" y="13" width="17" height="6.5" rx="1.8"/><path d="M5.5 13 8 5.5h8L18.5 13"/><path d="M7.5 16.3h.01M10.5 16.3h.01"/>',
  files: '<path d="m12 3.5 8.5 4.5-8.5 4.5L3.5 8z"/><path d="m3.5 12.2 8.5 4.5 8.5-4.5"/><path d="m3.5 16.2 8.5 4.5 8.5-4.5"/>',
  sliders: '<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>',
  star: '<path d="m12 4.2 2.4 4.9 5.4.8-3.9 3.8.9 5.4L12 16.6l-4.8 2.5.9-5.4-3.9-3.8 5.4-.8z"/>',
  unplug: '<path d="m19 5-3.5 3.5M5 19l3.5-3.5"/><path d="m9.5 9.5 5 5"/><path d="M13 7.5 16.5 11l1.3-1.3a2.5 2.5 0 0 0-3.5-3.5z"/><path d="M11 16.5 7.5 13l-1.3 1.3a2.5 2.5 0 0 0 3.5 3.5z"/>',
  gamepad: '<path d="M7 8.5h10a4.5 4.5 0 0 1 4.3 5.8l-.9 3a2.3 2.3 0 0 1-3.8 1l-2.2-2.3H9.6l-2.2 2.3a2.3 2.3 0 0 1-3.8-1l-.9-3A4.5 4.5 0 0 1 7 8.5z"/><path d="M8.5 11v3M7 12.5h3"/><path d="M15.5 12h.01M17.5 13.5h.01"/>',
  image: '<rect x="4" y="4.5" width="16" height="15" rx="2"/><circle cx="9" cy="9.5" r="1.6"/><path d="m4.5 17 4.8-4.6a1.3 1.3 0 0 1 1.8 0l6.4 6.1"/><path d="m14 15 1.6-1.5a1.3 1.3 0 0 1 1.8 0l2.1 2"/>',
  pencil: '<path d="M15.3 5.2a2 2 0 0 1 2.9 0l.6.6a2 2 0 0 1 0 2.9L9 18.5l-4.2 1 1-4.2z"/><path d="m13.8 6.7 3.5 3.5"/>',
  palette: '<path d="M12 3.8a8.2 8.2 0 1 0 0 16.4c1.2 0 1.9-.8 1.9-1.8 0-.5-.2-.9-.5-1.3-.3-.3-.5-.7-.5-1.2 0-1 .8-1.8 1.8-1.8h2.1a3.5 3.5 0 0 0 3.5-3.5c0-3.8-3.7-6.8-8.3-6.8z"/><circle cx="7.8" cy="11.5" r="1"/><circle cx="10.3" cy="7.9" r="1"/><circle cx="14.6" cy="7.9" r="1"/>',
  send: '<path d="M20 4 4.5 10.6l6.3 2.6 2.6 6.3z"/><path d="m10.8 13.2 4.4-4.4"/>',
  grid: '<rect x="4" y="4" width="6.5" height="6.5" rx="1.4"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1.4"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1.4"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.4"/>',
  rows: '<rect x="3.5" y="7" width="7" height="10" rx="1.4"/><rect x="12.5" y="8.5" width="4" height="7" rx="1"/><rect x="18.5" y="8.5" width="2" height="7" rx=".8"/>',
  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
  thermo: '<path d="M10 14.2V5.5a2 2 0 0 1 4 0v8.7a4 4 0 1 1-4 0z"/><path d="M12 10v6.2"/>',
  wrench: '<path d="M14.8 4.2a4.5 4.5 0 0 0-4.9 6.1L4.3 16a1.8 1.8 0 0 0 2.5 2.6l5.7-5.6a4.5 4.5 0 0 0 6.1-4.9l-2.7 2.7-2.4-.4-.4-2.4z"/>',
  undo: '<path d="M8.5 8.5H15a4.5 4.5 0 0 1 0 9H9"/><path d="m11.5 5.5-3 3 3 3"/>',
  save: '<path d="M5.5 4h10.2l3.3 3.3v11.2a1.5 1.5 0 0 1-1.5 1.5h-12A1.5 1.5 0 0 1 4 18.5v-13A1.5 1.5 0 0 1 5.5 4z"/><path d="M8 4v4.5h7V4"/><rect x="7.5" y="13" width="9" height="7" rx=".8"/>',
  home: '<path d="M4.5 10.5 12 4.5l7.5 6"/><path d="M6.5 9v10.5h11V9"/><path d="M10 19.5v-5h4v5"/>',
  sparkle: '<path d="M12 4.5c.4 3.6 2.9 6.1 6.5 6.5-3.6.4-6.1 2.9-6.5 6.5-.4-3.6-2.9-6.1-6.5-6.5 3.6-.4 6.1-2.9 6.5-6.5z"/><path d="M18.5 16.5c.1 1 .9 1.8 2 2-1.1.2-1.9 1-2 2-.2-1-1-1.8-2-2 1-.2 1.8-1 2-2z"/>',
  eye: '<path d="M3 12s3.3-6 9-6 9 6 9 6-3.3 6-9 6-9-6-9-6z"/><circle cx="12" cy="12" r="2.6"/>',
} as const

export type IconName = keyof typeof ICONS

export const iconSvg = (name: IconName, cls = "") => `<svg class="i ${cls}" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`

export function Icon({ name, className = "" }: { name: IconName; className?: string }) {
  return <svg className={`i ${className}`} viewBox="0 0 24 24" aria-hidden="true" dangerouslySetInnerHTML={{ __html: ICONS[name] }} />
}

/** A check that draws itself when it appears. */
export function CheckDraw({ on = false }: { on?: boolean }) {
  return <svg className={`i check-draw ${on ? "on" : ""}`} viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.5 10 17.5 19.5 7" /></svg>
}
