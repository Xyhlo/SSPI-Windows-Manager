import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { SPRINGS, motionOK, settle } from "@/lib/motion"

/** Opens and closes its content with a height animation, keeping it mounted until the close finishes.
 *  With `reveal`, an opened drawer scrolls into view inside its scrolling list. */
export function Collapse({ open, children, className, reveal }: { open: boolean; children: ReactNode; className?: string; reveal?: boolean }) {
  const ref = useRef<HTMLDivElement>(null)
  const [mounted, setMounted] = useState(open)
  const first = useRef(true)
  useLayoutEffect(() => { if (open) setMounted(true) }, [open])
  useLayoutEffect(() => {
    // Only content that is open on the very first render skips the animation; a drawer that starts closed animates every time.
    const skip = first.current
    first.current = false
    const el = ref.current
    if (!el) return
    if (skip && open) return
    const show = () => { if (open && reveal) el.scrollIntoView({ block: "nearest", behavior: motionOK() ? "smooth" : "auto" }) }
    if (!motionOK()) { if (!open) setMounted(false); show(); return }
    const height = (el.firstElementChild as HTMLElement | null)?.offsetHeight || 0
    el.style.overflow = "hidden"
    const animation = el.animate(open
      ? [{ height: "0px", opacity: 0 }, { height: `${height}px`, opacity: 1 }]
      : [{ height: `${height}px`, opacity: 1 }, { height: "0px", opacity: 0 }],
    { duration: open ? SPRINGS.soft.ms : 200, easing: open ? SPRINGS.soft.easing : "cubic-bezier(.4,0,1,1)", fill: "forwards" })
    let live = true
    void settle(animation).then(() => {
      if (!live) return
      animation.cancel()
      el.style.overflow = ""
      if (!open) setMounted(false)
      show()
    })
    return () => { live = false }
  }, [open, mounted])
  if (!mounted) return null
  return <div ref={ref} className={className}><div>{children}</div></div>
}
