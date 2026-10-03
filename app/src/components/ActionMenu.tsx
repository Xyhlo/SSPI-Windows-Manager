/* =====================================================================
   Action menu — SSPI's popover for secondary actions: a button that
   springs a menu open from itself, rows that settle in one after another,
   and a quick fade on the way out. Arrow keys, Home/End and first letters
   move through it; Escape returns focus to the button.
   ===================================================================== */
import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties } from "react"
import { createPortal } from "react-dom"
import { motionOK } from "@/lib/motion"
import { CheckDraw, Icon, type IconName } from "./Icon"
import "./action-menu.css"

export type MenuAction = {
  id: string
  label: string
  icon: IconName
  description?: string
  disabled?: boolean
  busy?: boolean
  checked?: boolean
  tone?: "danger"
  onSelect: () => void | Promise<void>
}

type Trigger = { icon?: IconName; text?: string; title?: string }

export function ActionMenu({ label, actions, align = "end", trigger = {}, disabled }: {
  label: string; actions: MenuAction[]; align?: "start" | "end"; trigger?: Trigger; disabled?: boolean
}) {
  const [open, setOpen] = useState(false)
  const [closing, setClosing] = useState(false)
  const [position, setPosition] = useState<CSSProperties>({ visibility: "hidden" })
  const [upward, setUpward] = useState(false)
  const button = useRef<HTMLButtonElement>(null)
  const menu = useRef<HTMLDivElement>(null)
  const firstFocus = useRef<"first" | "last">("first")
  const id = useId()
  const icon = trigger.icon || "plus"
  const enabled = () => Array.from(menu.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") || [])

  const close = (restoreFocus = false) => {
    if (!open || closing) return
    if (restoreFocus) button.current?.focus()
    if (!motionOK()) { setOpen(false); return }
    setClosing(true)
  }
  const toggle = () => { if (open) close(); else { firstFocus.current = "first"; setClosing(false); setOpen(true) } }

  useLayoutEffect(() => {
    if (!open) { setPosition({ visibility: "hidden" }); return }
    const place = () => {
      if (!button.current || !menu.current) return
      const rect = button.current.getBoundingClientRect()
      const width = Math.min(menu.current.offsetWidth, window.innerWidth - 24)
      const height = menu.current.scrollHeight
      const below = window.innerHeight - rect.bottom - 16
      const above = rect.top - 16
      const up = height > below && above > below
      const left = Math.max(12, Math.min(align === "end" ? rect.right - width : rect.left, window.innerWidth - width - 12))
      setUpward(up)
      setPosition({ left, ...(up ? { bottom: window.innerHeight - rect.top + 8 } : { top: rect.bottom + 8 }), maxHeight: Math.max(120, up ? above : below) })
    }
    place()
    window.addEventListener("resize", place)
    window.addEventListener("scroll", place, true)
    return () => { window.removeEventListener("resize", place); window.removeEventListener("scroll", place, true) }
  }, [open, align])

  useLayoutEffect(() => {
    if (!open || closing || position.visibility === "hidden") return
    const buttons = enabled()
    ;(firstFocus.current === "last" ? buttons[buttons.length - 1] : buttons[0])?.focus({ preventScroll: true })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, position.visibility])

  useEffect(() => {
    if (!open || closing) return
    const outside = (event: PointerEvent | FocusEvent) => {
      const target = event.target as Node
      if (!menu.current?.contains(target) && !button.current?.contains(target)) close()
    }
    const keys = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); event.stopImmediatePropagation(); close(true); return }
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
      if (next >= 0) { event.preventDefault(); event.stopImmediatePropagation(); buttons[next].focus(); return }
      // Keep page shortcuts out of the menu. A letter moves to the next matching action.
      if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey && event.key !== " ") {
        const rotated = [...buttons.slice(index + 1), ...buttons.slice(0, index + 1)]
        rotated.find(item => item.textContent?.trim().toLowerCase().startsWith(event.key.toLowerCase()))?.focus()
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, closing])

  const origin = `${upward ? "bottom" : "top"} ${align === "end" ? "right" : "left"}`
  return <>
    <button ref={button} type="button" className={`btn sm am-trigger ${trigger.text ? "" : "icon"} ${icon === "plus" ? "spins" : ""}`}
      title={trigger.title || label} aria-label={trigger.text ? undefined : label} disabled={disabled}
      aria-haspopup="menu" aria-expanded={open && !closing} aria-controls={open ? id : undefined}
      onClick={toggle}
      onKeyDown={event => {
        if (event.key === "ArrowDown" || event.key === "ArrowUp") {
          event.preventDefault(); event.stopPropagation()
          firstFocus.current = event.key === "ArrowUp" ? "last" : "first"; setClosing(false); setOpen(true)
        }
      }}>
      <Icon name={icon} />{trigger.text && <span className="lbl">{trigger.text}</span>}
    </button>
    {open && createPortal(
      <div ref={menu} id={id} role="menu" aria-label={label}
        className={`am-menu ${closing ? "is-closing" : ""}`} style={{ ...position, transformOrigin: origin }}
        onAnimationEnd={event => { if (event.target === menu.current && closing) { setClosing(false); setOpen(false) } }}>
        {actions.map((action, index) => (
          <button key={action.id} type="button" role={action.checked == null ? "menuitem" : "menuitemcheckbox"}
            aria-checked={action.checked} disabled={action.disabled || action.busy} tabIndex={-1}
            className={`am-item ${action.tone === "danger" ? "danger" : ""}`} style={{ "--i": index } as CSSProperties}
            onClick={() => { close(true); void action.onSelect() }}>
            <span className="am-ico">{action.busy ? <span className="spinner" /> : <Icon name={action.icon} />}</span>
            <span className="am-text"><strong>{action.label}</strong>{action.description && <span>{action.description}</span>}</span>
            {action.checked != null && <span className="am-check"><CheckDraw on={action.checked} /></span>}
          </button>
        ))}
      </div>, document.body,
    )}
  </>
}
