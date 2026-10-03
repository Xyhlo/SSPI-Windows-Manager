import { useEffect, useState } from "react"
import { consoleProcesses, consoleProcessControl } from "@/lib/console-api"
import { receiverEndpoint } from "@/lib/console-helpers"
import type { ConsoleProcess } from "@/lib/console-types"
import { errorText } from "@/lib/format"
import type { ConsoleKind, Settings } from "@/types"
import { Icon } from "../../Icon"
import { toast } from "../../toasts"

export function RunningPayloads({ target, settings, demo, revision }: { target: ConsoleKind; settings: Settings; demo: boolean; revision: number }) {
  const { host, port } = receiverEndpoint(settings, target)
  const [processes, setProcesses] = useState<ConsoleProcess[]>([])
  const [error, setError] = useState("")
  const [busy, setBusy] = useState<number | null>(null)
  const [refresh, setRefresh] = useState(0)
  useEffect(() => {
    let active = true
    let timer = 0
    setProcesses([]); setError("")
    if (!host) return
    const poll = async () => {
      try {
        const result = await consoleProcesses({ target, host, port, demo })
        if (active) { setProcesses(result.processes.filter(p => p.control === "payload" && p.state !== "zombie")); setError("") }
      } catch (reason) { if (active) { setProcesses([]); setError(errorText(reason)) } }
      if (active) timer = window.setTimeout(() => void poll(), 3000)
    }
    void poll()
    return () => { active = false; clearTimeout(timer) }
  }, [target, host, port, demo, revision, refresh])
  const stop = async (process: ConsoleProcess, force = false) => {
    if (busy !== null) return
    setBusy(process.pid)
    try {
      const result = await consoleProcessControl({ target, host, port, demo, pid: process.pid, name: process.name, action: force ? "end" : "stop" })
      toast({ tone: result.exited ? "success" : "info", title: result.exited ? `${process.name} stopped` : "Stop requested", text: result.exited ? "Use Start beside its saved payload to launch it again." : "The process is still running. Refresh its status before trying End." })
      setRefresh(value => value + 1)
    } catch (reason) { toast({ tone: "error", title: "The payload could not be stopped", text: errorText(reason) }) }
    finally { setBusy(null) }
  }
  return <section className="pl-running" aria-label="Running payloads">
    <div className="pl-running-head"><strong>Running payloads</strong><button className="btn sm ghost" disabled={!host || busy !== null} onClick={() => setRefresh(value => value + 1)}><Icon name="refresh" />Refresh</button></div>
    {error ? <p className="muted">Load the current {target.toUpperCase()} receiver to manage running payloads. {error}</p> : processes.length ? <div className="pl-running-list">{processes.map(process => <div key={`${process.pid}:${process.startedAt}`} className="pl-running-row">
      <span className="dot good" /><span className="pl-running-name">{process.name}<small>PID {process.pid}</small></span>
      <button className="btn sm" disabled={busy !== null} onClick={() => void stop(process)}><Icon name="square" />{busy === process.pid ? "Stopping" : "Stop"}</button>
      <button className="btn sm ghost" disabled={busy !== null} onClick={() => void stop(process, true)} title="Force this payload process to exit">End</button>
    </div>)}</div> : <p className="muted">No separate payload processes reported by the receiver.</p>}
    {target === "ps4" && <p className="pl-running-note">Payloads running inside GoldHEN cannot be stopped separately. Start sends saved payloads through BinLoader.</p>}
  </section>
}
