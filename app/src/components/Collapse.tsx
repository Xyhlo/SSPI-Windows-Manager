import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { SPRINGS, motionOK, settle } from "@/lib/motion"

/** Opens and closes its content with a height animation, keeping it mounted until the close finishes. */
export function Collapse({ open, children, className }: { open: boolean; children: ReactNode; className?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const [mounted, setMounted] = useState(open)
  const first = useRef(true)
  useLayoutEffect(() => { if (open) setMounted(true) }, [open])
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const skip = first.current
    first.current = false
    if (skip && open) return
    if (!motionOK()) { if (!open) setMounted(false); return }
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
    })
    return () => { live = false }
  }, [open, mounted])
  if (!mounted) return null
  return <div ref={ref} className={className}><div>{children}</div></div>
}
