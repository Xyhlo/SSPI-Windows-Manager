/* A small filled preview of an icon shape, for shape pickers. */
import { useEffect, useRef } from "react"
import { shapePath, type IconShape } from "@/lib/ps4-theme"

export function ShapeGlyph({ shape }: { shape: IconShape }) {
  const ref = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const ctx = ref.current?.getContext("2d")
    if (!ctx) return
    ctx.clearRect(0, 0, 36, 36)
    shapePath(ctx, 4, 4, 28, shape, 0.24)
    ctx.fillStyle = "rgba(255,255,255,.85)"
    ctx.fill()
  }, [shape])
  return <canvas ref={ref} width={36} height={36} aria-hidden="true" />
}
