// PS5 browser d-pad events use the same arrow keys as a keyboard. Keep text
// editing and native selects untouched; choose the closest control geometrically.
export function nextControl(current, candidates, direction) {
  const from = current.getBoundingClientRect()
  const horizontal = direction === 'ArrowLeft' || direction === 'ArrowRight'
  const forward = direction === 'ArrowRight' || direction === 'ArrowDown'
  const originMain = horizontal ? (from.left + from.right) / 2 : (from.top + from.bottom) / 2
  const originCross = horizontal ? (from.top + from.bottom) / 2 : (from.left + from.right) / 2
  let next = null, score = Infinity
  candidates.forEach(candidate => {
    if (candidate === current) return
    const rect = candidate.getBoundingClientRect()
    const main = (horizontal ? (rect.left + rect.right) / 2 : (rect.top + rect.bottom) / 2) - originMain
    if (forward ? main <= 2 : main >= -2) return
    const cross = Math.abs((horizontal ? (rect.top + rect.bottom) / 2 : (rect.left + rect.right) / 2) - originCross)
    const overlap = horizontal ? rect.top < from.bottom && rect.bottom > from.top : rect.left < from.right && rect.right > from.left
    const rank = Math.abs(main) + cross * 3 + (overlap ? 0 : 1000)
    if (rank < score) { score = rank; next = candidate }
  })
  return next
}

export function installControllerFocus(doc = document) {
  const selector = 'button:not(:disabled),a[href],input:not(:disabled):not([type="hidden"]),select:not(:disabled),textarea:not(:disabled),[tabindex="0"]'
  const visible = el => el.getClientRects().length && !el.closest('[hidden],[aria-hidden="true"]') && getComputedStyle(el).visibility !== 'hidden'
  function keydown(event) {
    if (event.defaultPrevented) return
    const dialogs = Array.from(doc.querySelectorAll('[role="dialog"]')).filter(visible)
    const dialog = dialogs[dialogs.length - 1]
    if (event.key === 'Tab' && dialog) {
      const controls = Array.from(dialog.querySelectorAll(selector)).filter(visible)
      const first = controls[0], last = controls[controls.length - 1]
      if (!first) { event.preventDefault(); return }
      if (event.shiftKey && (doc.activeElement === first || !dialog.contains(doc.activeElement))) { event.preventDefault(); last.focus() }
      else if (!event.shiftKey && (doc.activeElement === last || !dialog.contains(doc.activeElement))) { event.preventDefault(); first.focus() }
      return
    }
    if (!/^Arrow(Up|Down|Left|Right)$/.test(event.key) || event.altKey || event.ctrlKey || event.metaKey) return
    const current = doc.activeElement
    if (current && (current.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(current.tagName))) return
    const root = dialog || doc
    const candidates = Array.from(root.querySelectorAll(selector)).filter(visible)
    const next = !current || !candidates.includes(current) ? candidates[0] : nextControl(current, candidates, event.key)
    if (next) { event.preventDefault(); next.focus(); next.scrollIntoView({ block: 'nearest', inline: 'nearest' }) }
  }
  doc.addEventListener('keydown', keydown)
  return () => doc.removeEventListener('keydown', keydown)
}
