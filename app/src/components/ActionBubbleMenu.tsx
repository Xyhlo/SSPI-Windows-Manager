import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties } from "react"
import { createPortal } from "react-dom"
import { Icon, type IconName } from "./Icon"
import "./action-bubble-menu.css"

export type BubbleAction = {
  id: string
  label: string
  icon: IconName
  description?: string
  disabled?: boolean
  busy?: boolean
  checked?: boolean
  onSelect: () => void | Promise<void>
}

/** A compact secondary-action menu. Each action is its own bubble; the page stays interactive. */
export function ActionBubbleMenu({ label, actions, align = "end" }: {
  label: string; actions: BubbleAction[]; align?: "start" | "end"
}) {
  const [open, setOpen] = useState(false)
  const [position, setPosition] = useState<CSSProperties>({ visibility: "hidden" })
  const [upward, setUpward] = useState(false)
  const trigger = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const firstFocus = useRef<"first" | "last">("first")
  const id = useId()
  const enabled = () => Array.from(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") || [])
  const close = (restoreFocus = false) => {
    setOpen(false)
    if (restoreFocus) trigger.current?.focus()
  }

  useLayoutEffect(() => {
    if (!open) return
    const place = () => {
      if (!trigger.current || !menu.current) return
      const rect = trigger.current.getBoundingClientRect()
      const width = Math.min(menu.current.scrollWidth, window.innerWidth - 24)
      const height = menu.current.scrollHeight
      const below = window.innerHeight - rect.bottom - 18
      const above = rect.top - 18
      const up = height > below && above > below
      const left = Math.max(12, Math.min(align === "end" ? rect.right - width : rect.left, window.innerWidth - width - 12))
      setUpward(up)
      setPosition({ left, ...(up ? { bottom: window.innerHeight - rect.top + 10 } : { top: rect.bottom + 10 }), maxHeight: Math.max(80, up ? above : below) })
    }
    place()
    window.addEventListener("resize", place)
    window.addEventListener("scroll", place, true)
    return () => { window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true) }
  }, [open, align])

  useLayoutEffect(() => {
    if (!open || position.visibility === "hidden") return
    const buttons = enabled()
    ;(firstFocus.current === "last" ? buttons[buttons.length - 1] : buttons[0])?.focus({ preventScroll: true })
  }, [open, position.visibility])

  useEffect(() => {
    if (!open) return
    const outside = (event: PointerEvent | FocusEvent) => {
      const target = event.target as Node
      if (!menu.current?.contains(target) && !trigger.current?.contains(target)) close()
    }
    const keys = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault(); event.stopImmediatePropagation(); close(true); return
      }
      if (!menu.current?.contains(document.activeElement)) return
      if (event.key === "Tab") { close(true); return }
      const buttons = enabled()
      if (!buttons.length) return
      const index = buttons.indexOf(document.activeElement as HTMLButtonElement)
      let next = -1
      if (event.key === "ArrowDown") next = (index + 1) % buttons.length
      if (event.key === "ArrowUp") next = (index - 1 + buttons.length) % buttons.length
      if (event.key === "Home") next = 0
      if (event.key === "End") next = buttons.length - 1
      if (next >= 0) {
        event.preventDefault(); event.stopImmediatePropagation(); buttons[next].focus(); return
      }
      // Keep page shortcuts out of the menu. A letter moves to the next matching action.
      if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey && event.key !== " ") {
        const rotated = [...buttons.slice(index + 1), ...buttons.slice(0, index + 1)]
        rotated.find(button => button.textContent?.trim().toLowerCase().startsWith(event.key.toLowerCase()))?.focus()
        event.preventDefault(); event.stopImmediatePropagation()
      }
    }
    window.addEventListener("pointerdown", outside)
    window.addEventListener("focusin", outside)
    window.addEventListener("keydown", keys, true)
    return () => {
      window.removeEventListener("pointerdown", outside)
      window.removeEventListener("focusin", outside)
      window.removeEventListener("keydown", keys, true)
    }
  }, [open])

  return <>
    <button ref={trigger} type="button" className="action-bubble-trigger" title={label} aria-label={label}
      aria-haspopup="menu" aria-expanded={open} aria-controls={open ? id : undefined}
      onClick={() => { firstFocus.current = "first"; setOpen(value => !value) }}
      onKeyDown={event => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault(); event.stopPropagation(); firstFocus.current = event.key === "ArrowUp" ? "last" : "first"; setOpen(true)
        }
      }}><Icon name="plus" /></button>
    {open && createPortal(
      <div ref={menu} id={id} role="menu" aria-label={label} className={`action-bubbles ${upward ? "opens-up" : ""} align-${align}`} style={position}>
        {actions.map((action, index) => <button key={action.id} type="button" role={action.checked == null ? "menuitem" : "menuitemcheckbox"}
          aria-checked={action.checked} disabled={action.disabled || action.busy} tabIndex={-1} className="action-bubble"
          style={{ "--bubble-index": index } as CSSProperties} title={action.description}
          onClick={() => { close(true); void action.onSelect() }}>
          <span className="action-bubble-icon">{action.busy ? <span className="spinner" /> : <Icon name={action.icon} />}</span>
          <span>{action.label}</span>
          {action.checked && <Icon name="check" />}
        </button>)}
      </div>, document.body,
    )}
  </>
}
