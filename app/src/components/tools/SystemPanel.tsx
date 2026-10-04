/* =====================================================================
   System — what the receiver reads about the console. Overview is laid
   out like the PS4's System Information; receivers with diagnostics-v1
   add the kernel log, processes, and other payloads' logs and crashes.
   Values the receiver can't read are left out and named once.
   ===================================================================== */
import { useEffect, useRef, useState } from "react"
import { Seg } from "../Controls"
import { Icon } from "../Icon"
import { KernelLogView, type KernelLogFocus } from "./system/KernelLogView"
import { CrashTimelineView } from "./system/CrashTimelineView"
import { LogsView } from "./system/LogsView"
import { ProcessesView } from "./system/ProcessesView"
import { consoleSystemInfo } from "@/lib/console-api"
import { formatTemp, formatUptime, hasCapability, receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleMount, ConsoleProbe, ConsoleSystemInfo } from "@/lib/console-types"
import { errorText, fmtBytes } from "@/lib/format"
import type { ConsoleKind, Settings } from "@/types"

type Props = { target: ConsoleKind; settings: Settings; demo: boolean; probe?: ConsoleProbe; onLoadReceiver: () => void; initialView?: SystemView }
export type SystemView = "overview" | "timeline" | "klog" | "processes" | "logs"
type View = SystemView

const CAPABILITY_NAMES: Record<string, string> = {
  "installed-library-v1": "Installed titles",
  "title-icons-v1": "Covers",
  "system-info-v1": "System information",
  "diagnostics-v1": "Diagnostics",
  "shell-refresh-v1": "Home screen refresh",
  "pkg-install": "Package installs",
  "fih-install": "Game folder installs",
  "parallel-upload": "Parallel uploads",
  "stop": "Remote reload",
}
const VIEWS: Array<[View, string]> = [["overview", "Overview"], ["timeline", "Crash timeline"], ["klog", "Kernel log"], ["processes", "Processes"], ["logs", "Logs & crashes"]]

export function SystemPanel({ target, settings, demo, probe, onLoadReceiver, initialView }: Props) {
  const name = target.toUpperCase()
  const [view, setView] = useState<View>(initialView ?? "overview")
  const [klogFocus, setKlogFocus] = useState<KernelLogFocus | null>(null)
  const receiver = probe?.receiver.state
  const unreachable = !demo && (!probe || receiver === "offline" || receiver === "unconfigured" || receiver === "error")
  const unsupported = !demo && !unreachable && (receiver === "outdated" || !hasCapability(probe, "system-info-v1"))
  const diagnostics = demo || hasCapability(probe, "diagnostics-v1")
  const { host, port } = receiverEndpoint(settings, target)

  useEffect(() => { if (!diagnostics && view !== "timeline") setView("overview") }, [diagnostics, view])

  if ((unreachable || unsupported) && view !== "timeline") {
    return (
      <div className="empty-state">
        <Icon name={unsupported ? "upload" : "signal"} />
        <h3>{unsupported ? `Load receiver ${probe?.receiver.expectedVersion || ""} to see system information`.replace("  ", " ") : `Your ${name} isn't answering`}</h3>
        <p>{unsupported ? `The receiver running on your ${name} is older and doesn't report system information.` : !probe?.host ? `Add your ${name}'s address in Options, Consoles, then load the receiver.` : `Nothing answered at ${probe.host}:${probe.receiver.port}. Load the receiver on your ${name}, or check that it's on.`}</p>
        {probe?.host && <div className="row"><button type="button" className="btn sm" onClick={onLoadReceiver}><Icon name="upload" />Load receiver</button><button type="button" className="btn sm" onClick={() => setView("timeline")}><Icon name="rows" />Saved crash timeline</button></div>}
      </div>
    )
  }

  return (
    <div className="sysx">
      <div className="sysx-bar">
        {diagnostics || view === "timeline"
          ? <Seg<View> label="System view" value={view} options={VIEWS} onChange={setView} />
          : <p className="sys-note">Load receiver {probe?.receiver.expectedVersion} to read the kernel log, processes, and other payloads' logs and crash reports.</p>}
      </div>
      <div className="sysx-body swap-fade" key={view}>
        {view === "overview" && <Overview target={target} host={host} port={port} demo={demo} probe={probe} />}
        {view === "timeline" && <CrashTimelineView key={`${target}:${host}:${port}:${demo}`} target={target} host={host} port={port} demo={demo} available={!unreachable && !unsupported && diagnostics}
          onShowInKernelLog={event => { setKlogFocus({ text: event.excerpt, line: event.line, at: Date.now() }); setView("klog") }} />}
        {view === "klog" && <KernelLogView target={target} host={host} port={port} demo={demo} focus={klogFocus} />}
        {view === "processes" && <ProcessesView target={target} host={host} port={port} demo={demo} />}
        {view === "logs" && <LogsView target={target} host={host} port={port} demo={demo} />}
      </div>
    </div>
  )
}

function Overview({ target, host, port, demo, probe }: { target: ConsoleKind; host: string; port: number; demo: boolean; probe?: ConsoleProbe }) {
  const name = target.toUpperCase()
  const [info, setInfo] = useState<ConsoleSystemInfo | null>(null)
  const [error, setError] = useState("")
  const [updatedAt, setUpdatedAt] = useState(0)
  const [, tick] = useState(0)
  const fetching = useRef(false)

  useEffect(() => {
    setInfo(null); setError("")
    let live = true
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
  }, [target, host, port, demo])

  if (!info) {
    return error
      ? <div className="empty-state"><Icon name="alert" /><h3>System information didn't load</h3><p>{error}</p></div>
      : <div className="sys"><div className="sys-col"><div className="skeleton" style={{ height: 96 }} /><div className="skeleton" style={{ height: 220 }} /></div><div className="sys-col"><div className="skeleton" style={{ height: 140 }} /></div></div>
  }

  const extras = info.extras || {}
  const rows: Array<[string, string]> = []
  if (info.firmware) rows.push(["System software", info.firmware])
  if (info.sdkVersion) rows.push(["SDK version", info.sdkVersion])
  if (info.model) rows.push(["Model", info.model])
  if (info.uptimeSeconds != null) rows.push(["Running for", formatUptime(info.uptimeSeconds)])
  if (info.consoleName) rows.push(["Host name", info.consoleName])
  if (info.network?.ip) rows.push(["IP address", info.network.ip])
  if (info.network?.mac) rows.push(["MAC address", info.network.mac])
  if (info.cpuTempC != null) rows.push(["CPU temperature", formatTemp(info.cpuTempC)])
  if (info.socTempC != null) rows.push(["SoC temperature", formatTemp(info.socTempC)])
  if (extras.cpuFrequencyMhz) rows.push(["CPU clock", `${extras.cpuFrequencyMhz} MHz`])
  if (extras.loadAverage) rows.push(["Load (1, 5, 15 min)", extras.loadAverage])
  if (info.memory) rows.push(["Memory free", `${fmtBytes(info.memory.freeBytes)} of ${fmtBytes(info.memory.totalBytes)}`])
  if (extras.processCount) rows.push(["Processes", extras.processCount])
  if (info.runningTitleId) rows.push(["Running title", info.runningTitleId])
  rows.push(["Receiver", `${info.receiverVersion}${probe?.receiver.latencyMs != null ? `, answers in ${probe.receiver.latencyMs} ms` : ""}`])
  const missing = [
    info.model ? "" : "model", info.cpuTempC == null && info.socTempC == null ? "temperatures" : "",
    info.memory ? "" : "memory", info.uptimeSeconds == null ? "uptime" : "", info.network?.ip ? "" : "network",
  ].filter(Boolean)
  const seconds = Math.max(0, Math.round((Date.now() - updatedAt) / 1000))
  const hot = Math.max(info.cpuTempC ?? 0, info.socTempC ?? 0) >= 80

  return (
    <div className="sys">
      <div className="sys-col">
        <div className="sys-hero">
          <span className="sys-kind">{name}</span>
          <div>
            <strong>{info.consoleName || `Your ${name}`}</strong>
            <span>{info.firmware ? `System software ${info.firmware}` : "System software not reported"}{info.model ? ` · ${info.model}` : ""}</span>
          </div>
          <span className="sys-live"><span className={`dot ${hot ? "warn" : "good"}`} />{seconds < 2 ? "Updated just now" : `Updated ${seconds} s ago`}</span>
        </div>
        <dl className="sys-rows">
          {rows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
        </dl>
        {missing.length > 0 && <p className="sys-note">The receiver doesn't report the {missing.length === 1 ? missing[0] : `${missing.slice(0, -1).join(", ")} or ${missing[missing.length - 1]}`} for this console.</p>}
      </div>
      <div className="sys-col">
        <p className="sys-label">Storage</p>
        {info.storage.length ? info.storage.map(volume => <Volume key={volume.path} label={volume.label} path={volume.path} total={volume.totalBytes} free={volume.freeBytes} />)
          : <p className="sys-note">The receiver didn't report any storage.</p>}
        {info.mounts && info.mounts.length > 0 && <Mounts mounts={info.mounts} />}
        <p className="sys-label">Receiver features</p>
        <div className="chips sys-caps">
          {info.capabilities.map(cap => CAPABILITY_NAMES[cap]).filter(Boolean).map(label => <span key={label} className="tag">{label}</span>)}
        </div>
      </div>
    </div>
  )
}

function Volume({ label, path, total, free }: { label: string; path: string; total: number; free: number }) {
  const used = Math.max(0, total - free)
  const pct = total ? (used / total) * 100 : 0
  const tone = total && free / total < 0.08 ? "fail" : total && free / total < 0.18 ? "warn" : ""
  return (
    <div className="meter-block sys-volume">
      <div className="meter-top"><span>{label} <em className="sys-path">{path}</em></span><strong>{fmtBytes(free)} free</strong></div>
      <div className="meter" role="img" aria-label={`${label}: ${fmtBytes(used)} used of ${fmtBytes(total)}`}><i className={`need ${tone}`} style={{ width: `${pct}%` }} /></div>
      <div className="meter-legend">
        <div><span><i style={{ background: "var(--ink)" }} />Used</span><b>{fmtBytes(used)}</b></div>
        <div><span><i style={{ background: "rgba(255,255,255,.12)" }} />Total</span><b>{fmtBytes(total)}</b></div>
      </div>
    </div>
  )
}

/** Every mounted filesystem: devices first, then loopback and null mounts (ShadowMount, sandboxes). */
function Mounts({ mounts }: { mounts: ConsoleMount[] }) {
  const devices = mounts.filter(m => m.totalBytes > 0 && m.type !== "nullfs")
  const other = mounts.length - devices.length
  const sorted = [...devices, ...mounts.filter(m => !devices.includes(m))]
  return (
    <details className="sys-mounts">
      <summary><span>Mounted filesystems</span><em>{devices.length} with storage{other ? `, ${other} other` : ""}</em><Icon name="chevD" /></summary>
      <div className="sys-mount-list">
        {sorted.map((mount, index) => (
          <div key={`${mount.on}-${index}`} className="sys-mount">
            <b title={mount.on}>{mount.on}</b>
            <span title={mount.from}>{mount.from}</span>
            <em>{mount.type}{mount.readOnly ? ", read-only" : ""}</em>
            <strong>{mount.totalBytes ? `${fmtBytes(mount.freeBytes)} free of ${fmtBytes(mount.totalBytes)}` : "—"}</strong>
          </div>
        ))}
      </div>
    </details>
  )
}
