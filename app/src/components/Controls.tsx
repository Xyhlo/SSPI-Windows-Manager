/* Shared form controls: option rows, switches, text fields and segmented pickers. */
import { useLayoutEffect, useRef, type ReactNode } from "react"
import { Icon } from "./Icon"
import type { ConsoleProbe } from "@/lib/console-types"
import { glide } from "@/lib/motion"
import type { ConsoleKind } from "@/types"

const probeDot = (probe?: ConsoleProbe) => !probe ? "" : probe.receiver.state === "online" ? "good" : probe.receiver.state === "outdated" ? "warn" : probe.receiver.state === "unconfigured" ? "" : "fail"

/** PS5 | PS4 picker with each receiver's state as a dot. */
export function ConsoleSwitch({ value, onChange, probes, demo, label = "Console" }: { value: ConsoleKind; onChange: (target: ConsoleKind) => void; probes: Partial<Record<ConsoleKind, ConsoleProbe>>; demo?: boolean; label?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const thumb = useRef<HTMLSpanElement>(null)
  useLayoutEffect(() => { glide(thumb.current, ref.current?.querySelector<HTMLElement>("[aria-pressed=true]") || null, ref.current) }, [value])
  return (
    <div className="seg console-switch" role="group" aria-label={label} ref={ref}>
      <span className="seg-thumb" ref={thumb} />
      {(["ps5", "ps4"] as const).map(id => (
        <button key={id} type="button" aria-pressed={value === id} onClick={() => onChange(id)}>
          <span className={`dot ${probeDot(probes[id]) || (demo ? "good" : "")}`} />{id.toUpperCase()}
        </button>
      ))}
    </div>
  )
}

export function Row({ label, detail, children, tone, wide, className = "" }: { label: string; detail?: ReactNode; children?: ReactNode; tone?: "warn" | "good"; wide?: boolean; className?: string }) {
  return (
    <div className={`orow ${wide ? "wide" : ""} ${className}`}>
      <div className="ol"><strong>{label}</strong>{detail && <span className={tone || ""}>{detail}</span>}</div>
      {children && <div className="or">{children}</div>}
    </div>
  )
}

export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (value: boolean) => void; label: string; disabled?: boolean }) {
  return <button type="button" className="switch" role="switch" aria-checked={checked} aria-label={label} disabled={disabled} onClick={() => onChange(!checked)} />
}

export function Field({ value, onChange, placeholder, type = "text", width = 240, icon, label }: { value: string | number; onChange: (value: string) => void; placeholder?: string; type?: string; width?: number; icon?: Parameters<typeof Icon>[0]["name"]; label: string }) {
  return (
    <label className="field" style={{ width }}>
      {icon && <Icon name={icon} />}
      <input type={type} value={value} placeholder={placeholder} aria-label={label} autoComplete="off" spellCheck={false} onChange={event => onChange(event.target.value)} />
    </label>
  )
}

export function Seg<T extends string>({ value, options, onChange, label }: { value: T; options: Array<[T, string]>; onChange: (value: T) => void; label: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const thumb = useRef<HTMLSpanElement>(null)
  useLayoutEffect(() => { glide(thumb.current, ref.current?.querySelector<HTMLElement>("[aria-pressed=true]") || null, ref.current) }, [value])
  return (
    <div className="seg" role="group" aria-label={label} ref={ref}>
      <span className="seg-thumb" ref={thumb} />
      {options.map(([id, text]) => <button key={id} type="button" aria-pressed={value === id} onClick={() => onChange(id)}>{text}</button>)}
    </div>
  )
}
