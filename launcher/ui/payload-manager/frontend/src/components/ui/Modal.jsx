import React, { useEffect, useRef } from 'react'

export default function Modal({ show, title, children, onClose, footer }) {
  const dialog = useRef(null)
  const close = useRef(onClose)
  close.current = onClose
  useEffect(() => {
    if (!show || !dialog.current) return
    const previous = document.activeElement
    const panel = dialog.current
    const controls = () => Array.from(panel.querySelectorAll('button:not(:disabled),input:not(:disabled),select:not(:disabled),a[href],[tabindex="0"]')).filter(el => el.getClientRects().length)
    ;(controls()[0] || panel).focus()
    const keydown = event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close.current() }
      if (event.key !== 'Tab') return
      const items = controls()
      if (!items.length) { event.preventDefault(); return }
      const first = items[0], last = items[items.length - 1]
      if (event.shiftKey && (document.activeElement === first || document.activeElement === panel)) { event.preventDefault(); last.focus() }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
    }
    panel.addEventListener('keydown', keydown)
    return () => { panel.removeEventListener('keydown', keydown); if (previous && previous.isConnected) previous.focus() }
  }, [show])
  if (!show) return null
  return (
    <div className="sspi-modal-layer">
      <div className="sspi-modal-shade" onClick={onClose} />
      <div ref={dialog} className="sspi-modal" role="dialog" aria-modal="true" aria-label={title} tabIndex={-1}>
        <h3>{title}</h3>
        <div className="sspi-modal-content">{children}</div>
        {footer && <div className="sspi-modal-actions">{footer}</div>}
      </div>
    </div>
  )
}
