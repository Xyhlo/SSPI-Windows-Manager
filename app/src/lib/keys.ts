/* The page on screen owns the keyboard below the global shortcuts (Q/E, O, Escape in overlays). */
type Handler = (event: KeyboardEvent) => void

let pageHandler: Handler | null = null

export function setPageKeys(handler: Handler) {
  pageHandler = handler
  return () => { if (pageHandler === handler) pageHandler = null }
}

export const runPageKeys = (event: KeyboardEvent) => pageHandler?.(event)

/* A panel inside a page (a Tools panel) gets the keys first; it returns true when it used one. */
type PanelHandler = (event: KeyboardEvent) => boolean
let panelHandler: PanelHandler | null = null

export function setPanelKeys(handler: PanelHandler) {
  panelHandler = handler
  return () => { if (panelHandler === handler) panelHandler = null }
}

export const runPanelKeys = (event: KeyboardEvent) => !!panelHandler?.(event)

export const isTyping = () => {
  const el = document.activeElement as HTMLElement | null
  return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable)
}
