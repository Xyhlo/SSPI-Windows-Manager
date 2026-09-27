/* =====================================================================
   System — what the receiver reads about the console, laid out like the
   PS4's System Information: the console, a list of values, and storage.
   Values the receiver can't read are left out and named once at the end.
   ===================================================================== */
import { useEffect, useRef, useState } from "react"
import { Icon } from "../Icon"
import type { Hint } from "../Shell"
import { consoleSystemInfo } from "@/lib/console-api"
import { formatTemp, formatUptime, hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProbe, ConsoleSystemInfo } from "@/lib/console-types"
import { errorText, fmtBytes } from "@/lib/format"
import type { ConsoleKind, Settings } from "@/types"

type Props = { target: ConsoleKind; settings: Settings; demo: boolean; probe?: ConsoleProbe; onLoadReceiver: () => void; onHints: (hints: Hint[]) => void }

const CAPABILITY_NAMES: Record<string, string> = {
  "installed-library-v1": "Installed titles",
  "title-icons-v1": "Covers",
  "system-info-v1": "System information",
  "shell-refresh-v1": "Home screen refresh",
  "pkg-install": "Package installs",
  "fih-install": "Game folder installs",
  "parallel-upload": "Parallel uploads",
  "stop": "Remote reload",
}

export function SystemPanel({ target, settings, demo, probe, onLoadReceiver, onHints }: Props) {
  const name = target.toUpperCase()
  const [info, setInfo] = useState<ConsoleSystemInfo | null>(null)
  const [error, setError] = useState("")
  const [updatedAt, setUpdatedAt] = useState(0)
  const [, tick] = useState(0)
  const receiver = probe?.receiver.state
  const unreachable = !demo && (!probe || receiver === "offline" || receiver === "unconfigured" || receiver === "error")
  const unsupported = !demo && !unreachable && (receiver === "outdated" || !hasCapability(probe, "system-info-v1"))
  const fetching = useRef(false)

  useEffect(() => {
    setInfo(null)
    setError("")
    if (unreachable || unsupported) return
    let live = true
    const { host, port } = receiverEndpoint(settings, target)
    const load = async () => {
      if (fetching.current || document.hidden) return
      fetching.current = true
      try {
        const next = await consoleSystemInfo({ target, host, port, demo })
        if (live) { setInfo(next); setUpdatedAt(Date.now()); setError("") }
      } catch (reason) { if (live) setError(errorText(reason)) }
      finally { fetching.current = false }
    }
    void load()
    const timer = window.setInterval(() => void load(), 5000)
    const clock = window.setInterval(() => tick(n => n + 1), 1000)
    return () => { live = false; window.clearInterval(timer); window.clearInterval(clock) }
  }, [target, demo, unreachable, unsupported, settings.ps5Host, settings.ps5Port, settings.ps4Host, settings.ps4ReceiverPort])

  useEffect(() => { onHints([]) }, [])

  if (unreachable || unsupported) {
    return (
      <div className="empty-state">
        <Icon name={unsupported ? "upload" : "signal"} />
        <h3>{unsupported ? `Load receiver ${probe?.receiver.expectedVersion || ""} to see system information`.replace("  ", " ") : `Your ${name} isn't answering`}</h3>
        <p>{unsupported ? `The receiver running on your ${name} is older and doesn't report system information.` : !probe?.host ? `Add your ${name}'s address in Options, Consoles, then load the receiver.` : `Nothing answered at ${probe.host}:${probe.receiver.port}. Load the receiver on your ${name}, or check that it's on.`}</p>
        {probe?.host && <div className="row"><button type="button" className="btn sm" onClick={onLoadReceiver}><Icon name="upload" />Load receiver</button></div>}
      </div>
    )
  }

  if (!info) {
    return error
      ? <div className="empty-state"><Icon name="alert" /><h3>System information didn't load</h3><p>{error}</p></div>
      : <div className="sys"><div className="sys-col"><div className="skeleton" style={{ height: 96 }} /><div className="skeleton" style={{ height: 220 }} /></div><div className="sys-col"><div className="skeleton" style={{ height: 140 }} /></div></div>
  }

  const rows: Array<[string, string]> = []
  if (info.firmware) rows.push(["System software", info.firmware])
  if (info.sdkVersion) rows.push(["SDK version", info.sdkVersion])
  if (info.model) rows.push(["Model", info.model])
  if (info.uptimeSeconds != null) rows.push(["Running for", formatUptime(info.uptimeSeconds)])
  if (info.consoleName) rows.push(["Host name", info.consoleName])
  if (info.network?.ip) rows.push(["IP address", info.network.ip])
  if (info.cpuTempC != null) rows.push(["CPU temperature", formatTemp(info.cpuTempC)])
  if (info.socTempC != null) rows.push(["SoC temperature", formatTemp(info.socTempC)])
  if (info.memory) rows.push(["Memory free", `${fmtBytes(info.memory.freeBytes)} of ${fmtBytes(info.memory.totalBytes)}`])
  if (info.runningTitleId) rows.push(["Running title", info.runningTitleId])
  rows.push(["Receiver", `${info.receiverVersion}${probe?.receiver.latencyMs != null ? `, answers in ${probe.receiver.latencyMs} ms` : ""}`])
  const missing = [
    info.model ? "" : "model", info.cpuTempC == null && info.socTempC == null ? "temperatures" : "",
    info.memory ? "" : "memory", info.uptimeSeconds == null ? "uptime" : "", info.network?.ip ? "" : "network",
  ].filter(Boolean)
  const seconds = Math.max(0, Math.round((Date.now() - updatedAt) / 1000))

  return (
    <div className="sys">
      <div className="sys-col">
        <div className="sys-hero">
          <span className="sys-kind">{name}</span>
          <div>
            <strong>{info.consoleName || `Your ${name}`}</strong>
            <span>{info.firmware ? `System software ${info.firmware}` : "System software not reported"}</span>
          </div>
          <span className="sys-live"><span className="dot good live" />{seconds < 2 ? "Updated just now" : `Updated ${seconds} s ago`}</span>
        </div>
        <dl className="sys-rows">
          {rows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
        </dl>
        {missing.length > 0 && <p className="sys-note">The receiver doesn't report the {missing.length === 1 ? missing[0] : `${missing.slice(0, -1).join(", ")} or ${missing[missing.length - 1]}`} for this console.</p>}
      </div>
      <div className="sys-col">
        <p className="sys-label">Storage</p>
        {info.storage.length ? info.storage.map(volume => {
          const used = Math.max(0, volume.totalBytes - volume.freeBytes)
          const pct = volume.totalBytes ? (used / volume.totalBytes) * 100 : 0
          const tone = volume.totalBytes && volume.freeBytes / volume.totalBytes < 0.08 ? "fail" : volume.totalBytes && volume.freeBytes / volume.totalBytes < 0.18 ? "warn" : ""
          return (
            <div key={volume.path} className="meter-block sys-volume">
              <div className="meter-top"><span>{volume.label}</span><strong>{fmtBytes(volume.freeBytes)} free</strong></div>
              <div className="meter" role="img" aria-label={`${volume.label}: ${fmtBytes(used)} used of ${fmtBytes(volume.totalBytes)}`}><i className={`need ${tone}`} style={{ width: `${pct}%` }} /></div>
              <div className="meter-legend">
                <div><span><i style={{ background: "var(--ink)" }} />Used</span><b>{fmtBytes(used)}</b></div>
                <div><span><i style={{ background: "rgba(255,255,255,.12)" }} />Total</span><b>{fmtBytes(volume.totalBytes)}</b></div>
              </div>
            </div>
          )
        }) : <p className="sys-note">The receiver didn't report any storage.</p>}
        <p className="sys-label">Receiver features</p>
        <div className="chips sys-caps">
          {info.capabilities.map(cap => CAPABILITY_NAMES[cap]).filter(Boolean).map(label => <span key={label} className="tag">{label}</span>)}
        </div>
      </div>
    </div>
  )
}
