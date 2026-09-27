/* The page on screen owns the keyboard below the global shortcuts (Q/E, O, Escape in overlays). */
type Handler = (event: KeyboardEvent) => void

let pageHandler: Handler | null = null

export function setPageKeys(handler: Handler) {
  pageHandler = handler
  return () => { if (pageHandler === handler) pageHandler = null }
}

export const runPageKeys = (event: KeyboardEvent) => pageHandler?.(event)

export const isTyping = () => {
  const el = document.activeElement as HTMLElement | null
  return !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable)
}
