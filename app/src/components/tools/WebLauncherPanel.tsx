import { useEffect, useRef, useState } from "react"
import { getWebLauncher, startWebLauncher, stopWebLauncher, launcherAvailable, type WebLauncherStatus } from "@/lib/launcher-api"
import { errorText } from "@/lib/format"
import { Icon } from "../Icon"
import { toast } from "../toasts"
import "./WebLauncherPanel.css"

export function WebLauncherPanel({ demo }: { demo: boolean }) {
  const [status, setStatus] = useState<WebLauncherStatus | null>(null)
  const [address, setAddress] = useState("")
  const [busy, setBusy] = useState("")
  const [error, setError] = useState("")
  const live = useRef(true)
  const available = launcherAvailable(demo)
  useEffect(() => {
    live.current = true
    if (!available) return () => { live.current = false }
    let fetching = false
    const poll = async () => {
      if (fetching) return
      fetching = true
      try { const result = await getWebLauncher(demo); if (live.current) { setStatus(result); setAddress(old => old || result.address) } }
      catch (reason) { if (live.current) setError(errorText(reason)) }
      finally { fetching = false }
    }
    void poll(); const timer = window.setInterval(() => void poll(), 2500)
    return () => { live.current = false; clearInterval(timer) }
  }, [demo, available])
  const run = async (operation: string, action: () => Promise<WebLauncherStatus>) => {
    if (busy) return
    setBusy(operation); setError("")
    try {
      const result = await action()
      if (live.current) setStatus(result)
    } catch (reason) { if (live.current) setError(errorText(reason)) }
    finally { if (live.current) setBusy("") }
  }
  const running = status?.running
  const copyAddress = async () => {
    try { await navigator.clipboard.writeText(status?.address || address); toast({ tone: "success", title: "DNS address copied", text: status?.address || address }) }
    catch (reason) { setError(errorText(reason)) }
  }
  return <div className="wl scroll">
    <div className="wl-main">
      <div className="wl-heading"><span className={`dot ${running ? "good" : status?.phase === "error" ? "fail" : ""}`} /><span>{running ? status?.phase === "loading" ? "Loading on the console" : status?.phase === "connected" ? "Browser connected" : "Host ready" : "Local web host"}</span>{demo && <span className="wl-preview">Offline preview</span>}</div>
      <h2>SSPI Web Launcher</h2>
      <p className="wl-intro">Open the SSPI console interface from your PS5’s User’s Guide. Payload Manager starts automatically after setup.</p>
      <div className="wl-action">
        <button type="button" className={`btn ${running ? "" : "primary"}`} disabled={!!busy || !status} onClick={() => void run(running ? "stop" : "start", () => running ? stopWebLauncher(demo) : startWebLauncher(address, demo))}>{busy === "start" || busy === "stop" ? <span className="spinner" /> : <Icon name={running ? "x" : "play"} />}{busy === "start" ? "Starting host" : busy === "stop" ? "Stopping" : running ? "Stop hosting" : "Host on this PC"}</button>
        <span className="wl-status" role="status">{!available ? "Available in the SSPI app." : status?.message || (error ? "Host unavailable." : "Loading host settings…")}</span>
      </div>
      {error && <p className="pl-error" role="alert">{error}</p>}
      <div className={`wl-setup ${running ? "is-ready" : ""}`}>
        <div className="wl-address"><span>PS5 primary DNS</span><strong>{status?.address || address || "Connect this PC to your network"}</strong><button type="button" className="btn sm ghost icon" aria-label="Copy DNS address" disabled={!address} onClick={() => void copyAddress()}><Icon name="copy" /></button></div>
        <ol>
          <li>Connect this PC and your PS5 to the same local network. Start hosting above; allow SSPI on your private network if Windows Firewall asks.</li>
          <li>On PS5, open <strong>Settings → Network → Settings → Set Up Internet Connection</strong>. Select your connected network, then <strong>Advanced Settings → DNS Settings → Manual</strong>. Enter the address above as primary DNS and <strong>0.0.0.0</strong> as secondary.</li>
          <li>Open <strong>Settings → User’s Guide, Health & Safety, and Other Information → User’s Guide</strong>. The SSPI launcher starts automatically. Follow the console’s setup progress.</li>
        </ol>
        <p>When setup finishes, Payload Manager opens. For later sessions, use the <strong>WebKit Autoloader</strong> home screen app. Restore your previous DNS settings after you finish using this host; this DNS blocks other domains.</p>
      </div>
      <details className="tech wl-details">
        <summary>Host details, updates and compatibility</summary>
        <div className="tech-body">
          <p>SSPI console runtime {status?.version || "—"}</p>
          <label className="wl-ip">This PC’s LAN address<input className="field" aria-label="Web launcher LAN IPv4 address" value={address} disabled={!!running || !!busy} placeholder="192.168.1.10" onChange={e => setAddress(e.target.value)} /></label>
          <p>DNS UDP 53 and HTTPS TCP 443 are required. HTTP TCP 80 provides a browser preview when available. A busy port is reported without stopping another app.</p>
          {running && status?.url && <p>Preview: <a href={status.url} target="_blank" rel="noreferrer">{status.url}</a>. A PC browser may show a self-signed certificate notice over HTTPS.</p>}
          {status?.lastClient && <p>Last client {status.lastClient} · {status.requests} requests{status.lastRequestAt ? ` · ${new Date(status.lastRequestAt).toLocaleTimeString()}` : ""}. Serving a page does not confirm console installation.</p>}
          <p>Upstream supports firmware 1.00–5.50 and 7.00–13.60. Its own page checks firmware and chooses the supported chain. Relapse requires an active local network interface; internet access is not required.</p>
          <p>Payload Manager keeps your catalog, repositories, history and autoload settings. The ELF loader starts with the launcher and accepts payloads on port 9021. Existing autoload.txt entries run in their saved order; the SSPI Manager starts once. If automatic browser opening is disabled in Manager settings, it remains disabled.</p>
          <p>Launcher updates arrive with SSPI Windows through Options → Updates. After updating Windows, host again and rerun setup to refresh the console’s cached SSPI interfaces. Saved payloads and autoload.txt are preserved.</p>
          {!!status?.logs.length && <pre className="wl-log">{status.logs.join("\n")}</pre>}
        </div>
      </details>
    </div>
  </div>
}
