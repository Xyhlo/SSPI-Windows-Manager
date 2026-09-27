/* Stats for nerds: a job's transfer speed over the last minute, sampled by the app while it runs. */
import { useEffect, useRef, useState } from "react"
import { fmtSpeed, MB } from "@/lib/format"
import { Spring, clamp, motionOK, onFrame } from "@/lib/motion"
import { speedNow, speedSamples } from "@/lib/speed"

const WINDOW = 60
const PAD = { l: 4, r: 46, t: 10, b: 4 }
const LINE = "216,216,221"

function niceCeil(value: number) {
  const v = Math.max(value, 1)
  const pow = 10 ** Math.floor(Math.log10(v)), n = v / pow
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 2.5 ? 2.5 : n <= 5 ? 5 : 10) * pow
}

export function SpeedChart({ jobId, active, stageLabel }: { jobId: string; active: boolean; stageLabel: (stage: string) => string }) {
  const hostRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const axisRef = useRef<HTMLDivElement>(null)
  const [hover, setHover] = useState<number | null>(null)
  const [tip, setTip] = useState<{ x: number; text: string; ago: string } | null>(null)
  const hoverRef = useRef<number | null>(null)
  hoverRef.current = hover
  const yMax = useRef(new Spring(20 * MB, 60, 16))
  const pulse = useRef(0)

  useEffect(() => {
    let lastTip = ""
    return onFrame(dt => {
      const canvas = canvasRef.current, host = hostRef.current
      if (!canvas || !host) return
      const w = host.clientWidth, h = host.clientHeight
      if (!w || !h) return
      const dpr = Math.min(window.devicePixelRatio || 1, 2)
      if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) { canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr) }
      const ctx = canvas.getContext("2d")
      if (!ctx) return
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      ctx.clearRect(0, 0, w, h)
      const plotW = w - PAD.l - PAD.r, plotH = h - PAD.t - PAD.b
      const now = speedNow()
      const samples = speedSamples(jobId).filter(s => now - s.t <= WINDOW + 2)
      let peak = 1
      for (const s of samples) peak = Math.max(peak, s.v)
      const y = yMax.current
      y.set(niceCeil(peak * 1.15))
      y.step(dt)
      const tickMax = niceCeil(y.target)
      const ticks = [0, tickMax / 2, tickMax]
      ctx.fillStyle = "rgba(255,255,255,.06)"
      for (const value of ticks) ctx.fillRect(PAD.l, Math.round(h - (PAD.b + (value / y.value) * plotH)), plotW, 1)
      const axis = axisRef.current
      if (axis) {
        const spans = axis.children
        ticks.forEach((value, i) => {
          const span = spans[i] as HTMLElement | undefined
          if (!span) return
          span.textContent = `${Math.round(value / MB)}${i === 2 ? " MB/s" : ""}`
          span.style.top = `${h - (PAD.b + (value / y.value) * plotH)}px`
        })
      }
      const pts = samples.map(s => [PAD.l + plotW * (1 - (now - s.t) / WINDOW), h - (PAD.b + clamp(s.v / y.value, 0, 1.08) * plotH)] as const)
      if (pts.length > 1) {
        const gradient = ctx.createLinearGradient(0, PAD.t, 0, h)
        gradient.addColorStop(0, `rgba(${LINE},.12)`)
        gradient.addColorStop(1, `rgba(${LINE},0)`)
        ctx.beginPath()
        ctx.moveTo(pts[0][0], h - PAD.b)
        for (const [x, py] of pts) ctx.lineTo(x, py)
        ctx.lineTo(pts[pts.length - 1][0], h - PAD.b)
        ctx.closePath()
        ctx.fillStyle = gradient
        ctx.fill()
        ctx.beginPath()
        pts.forEach(([x, py], i) => (i ? ctx.lineTo(x, py) : ctx.moveTo(x, py)))
        ctx.strokeStyle = `rgb(${LINE})`
        ctx.lineWidth = 1.6
        ctx.lineJoin = "round"
        ctx.stroke()
        const [lx, ly] = pts[pts.length - 1]
        if (active) {
          pulse.current = (pulse.current + dt / 1.6) % 1
          if (motionOK()) {
            ctx.beginPath()
            ctx.arc(lx, ly, 5 + pulse.current * 10, 0, Math.PI * 2)
            ctx.strokeStyle = `rgba(${LINE},${(1 - pulse.current) * 0.3})`
            ctx.lineWidth = 1.4
            ctx.stroke()
          }
          ctx.beginPath(); ctx.arc(lx, ly, 6, 0, Math.PI * 2); ctx.fillStyle = "#141416"; ctx.fill()
          ctx.beginPath(); ctx.arc(lx, ly, 4, 0, Math.PI * 2); ctx.fillStyle = `rgb(${LINE})`; ctx.fill()
        }
      }
      const hx = hoverRef.current
      if (hx != null && samples.length) {
        const targetT = now - (1 - (hx - PAD.l) / plotW) * WINDOW
        let best = samples[0]
        for (const s of samples) if (Math.abs(s.t - targetT) < Math.abs(best.t - targetT)) best = s
        const x = PAD.l + plotW * (1 - (now - best.t) / WINDOW)
        ctx.fillStyle = "rgba(255,255,255,.3)"
        ctx.fillRect(Math.round(x), 0, 1, h)
        const ago = Math.round(now - best.t)
        const text = `${fmtSpeed(best.v)}|${stageLabel(best.stage)}`
        const key = `${Math.round(x)}|${text}|${ago}`
        if (key !== lastTip) { lastTip = key; setTip({ x, text, ago: ago <= 1 ? "Now" : `${ago} s ago` }) }
      } else if (lastTip) { lastTip = ""; setTip(null) }
    })
  }, [jobId, active])

  const local = (event: React.PointerEvent) => {
    const rect = hostRef.current!.getBoundingClientRect()
    setHover(clamp(event.clientX - rect.left, PAD.l, rect.width - PAD.r))
  }
  return (
    <div
      ref={hostRef} className="tp-chart" tabIndex={0} aria-label="Transfer speed over the last minute. Use the arrow keys to read earlier values."
      onPointerMove={local} onPointerLeave={() => setHover(null)}
      onFocus={() => setHover((hostRef.current?.clientWidth || 0) - PAD.r)} onBlur={() => setHover(null)}
      onKeyDown={event => {
        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return
        event.preventDefault(); event.stopPropagation()
        const width = hostRef.current?.clientWidth || 0
        setHover(value => clamp((value ?? width - PAD.r) + (event.key === "ArrowLeft" ? -12 : 12), PAD.l, width - PAD.r))
      }}
    >
      <canvas ref={canvasRef} />
      <div className="tp-axis" ref={axisRef}><span /><span /><span /></div>
      {tip && <div className="tp-tip" style={{ left: tip.x > (hostRef.current?.clientWidth || 0) - 190 ? tip.x - 166 : tip.x + 12 }}><div className="tt">{tip.ago}</div><div><b>{tip.text.split("|")[0]}</b>{tip.text.split("|")[1]}</div></div>}
    </div>
  )
}
