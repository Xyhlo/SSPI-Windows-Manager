import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { CaseCover } from "@/lib/covers"
import { cn } from "@/lib/utils"
import { caseThumb, type CoverSpec } from "@/stage/art"
import { getStage, type CaseSpec } from "@/stage/stage"

/** A transparent box the stage fills with a 3D case. Without WebGL it shows the flat case artwork instead. */
export function CaseAnchor({ spec, className, children, delay, role, label, hoverable }: { spec: CaseSpec; className?: string; children?: ReactNode; delay?: number; role?: string; label?: string; hoverable?: boolean }) {
  const ref = useRef<HTMLSpanElement>(null)
  const stage = getStage()
  const specRef = useRef(spec)
  specRef.current = spec
  useLayoutEffect(() => {
    const el = ref.current
    if (!el || !stage) return
    stage.cases.register(el, specRef.current, { delay })
    return () => stage.cases.unregister(el)
  }, [stage, spec.key])
  useEffect(() => {
    if (stage && ref.current) stage.cases.register(ref.current, spec)
  }, [stage, spec.cover, spec.title, spec.kind, spec.titleId])
  if (!stage) {
    return <span className={cn("case-anchor no-webgl has-case", className)} role={role} aria-label={label}><CaseCover source={spec.cover} title={spec.title} titleId={spec.titleId} />{children}</span>
  }
  return <span ref={ref} className={cn("case-anchor", className)} data-case-anchor data-hover-case={hoverable ? "" : undefined} role={role} aria-label={label}>{children}</span>
}

/** A small flat case image (search results, toasts). */
export function CaseThumb({ spec, className }: { spec: CoverSpec; className?: string }) {
  const [url, setUrl] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    setUrl(null)
    void caseThumb(spec).then(value => { if (live) setUrl(value) }).catch(() => undefined)
    return () => { live = false }
  }, [spec.cover, spec.title, spec.titleId])
  return url ? <img className={cn("rthumb", className)} src={url} alt="" draggable={false} /> : <span className={cn("rthumb rthumb-empty", className)} aria-hidden="true" />
}
