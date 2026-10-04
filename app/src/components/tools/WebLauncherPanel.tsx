/* The local web host: setup, observed console traffic and the host log. */
import { useEffect, useRef, useState } from "react"
import { getWebLauncher, startWebLauncher, stopWebLauncher, launcherAvailable, type WebLauncherStatus } from "@/lib/launcher-api"
import { errorText } from "@/lib/format"
import { Icon } from "../Icon"
import { toast } from "../toasts"
import "./web-launcher.css"

const ago = (at: number | null, now: number) => {
  if (!at) return "No requests"
  const seconds = Math.max(0, Math.round((now - at) / 1000))
  if (seconds < 2) return "Just now"
  if (seconds < 60) return `${seconds}s ago`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  return `${Math.floor(seconds / 3600)}h ago`
}

export function WebLauncherPanel({ demo }: { demo: boolean }) {
  const [status, setStatus] = useState<WebLauncherStatus | null>(null)
  const [address, setAddress] = useState("")
  const [busy, setBusy] = useState("")
  const [error, setError] = useState("")
  const [pollError, setPollError] = useState("")
  const [now, setNow] = useState(Date.now)
  const [follow, setFollow] = useState(true)
  const live = useRef(true)
  const generation = useRef(0)
  const acting = useRef(false)
  const editedAddress = useRef(false)
  const feed = useRef<HTMLDivElement>(null)
  const available = launcherAvailable(demo)

  useEffect(() => {
    live.current = true
    let active = true
    let fetching = false
    if (!available) return () => { live.current = false }
    const poll = async () => {
      setNow(Date.now())
      if (fetching || acting.current) return
      fetching = true
      const current = generation.current
      try {
        const result = await getWebLauncher(demo)
        if (active && current === generation.current) {
          setStatus(result)
          if (!editedAddress.current) setAddress(result.address)
          setNow(Date.now())
          setPollError("")
        }
      } catch (reason) {
        if (active && current === generation.current) setPollError(errorText(reason))
      } finally { fetching = false }
    }
    void poll()
    const timer = window.setInterval(() => void poll(), 1000)
    return () => { active = false; live.current = false; clearInterval(timer) }
  }, [demo, available])

  const logs = status?.logs || []
  const lastLine = logs[logs.length - 1]
  useEffect(() => {
    if (follow && feed.current) feed.current.scrollTop = feed.current.scrollHeight
  }, [lastLine, logs.length, follow])

  const run = async (operation: "start" | "stop") => {
    if (acting.current) return
    acting.current = true
    generation.current++
    setBusy(operation)
    setError("")
    try {
      const result = await (operation === "stop" ? stopWebLauncher(demo) : startWebLauncher(address.trim(), demo))
      if (live.current) {
        setStatus(result)
        setAddress(result.address)
        editedAddress.current = false
        setNow(Date.now())
        setPollError("")
        if (operation === "start") setFollow(true)
      }
    } catch (reason) {
      if (live.current) setError(errorText(reason))
    } finally {
      acting.current = false
      generation.current++
      if (live.current) setBusy("")
    }
  }
  const copy = async (value: string, label: string) => {
    try {
      await navigator.clipboard.writeText(value)
      toast({ tone: "success", title: `${label} copied` })
    } catch (reason) {
      toast({ tone: "error", title: "Copying failed", text: errorText(reason) })
    }
  }

  const running = !!status?.running
  const phase = status?.phase || "idle"
  const host = running ? status?.address || address : address
  const phaseLabel = {
    idle: "Stopped",
    ready: "Listening",
    dns: "DNS received",
    connected: "Launcher requested",
    loading: "Serving launcher files",
    error: "Host error",
  }[phase]
  const consoleSeen = status?.consoleLastSeenAt ?? null
  const managerChecked = status?.managerCheckedAt ?? null
  const managerFresh = !!managerChecked && now - managerChecked < 15_000
  const managerReady = running && status?.managerReady && managerFresh
  const failure = error || pollError || (phase === "error" ? status?.message : "")

  if (!available) return (
    <div className="empty-state"><Icon name="globe" /><h3>Available in the SSPI app</h3><p>The web launcher runs a DNS and web host on this PC, so it needs the installed app.</p></div>
  )

  return (
    <div className="wl scroll">
      <div className="wl-wrap">
        <header className="wl-toolbar">
          <div className="wl-title">
            <h2>Web host</h2>
            <span className={`wl-state ${phase === "error" ? "fail" : running ? "on" : ""}`} role="status"><i />{phaseLabel}</span>
            {demo && <span className="wl-preview">Offline preview</span>}
          </div>
          <div className="wl-controls">
            <label className="wl-address">
              <span>This PC</span>
              <span className="field"><Icon name="monitor" /><input value={host} readOnly={running} disabled={!!busy} aria-label="This PC's LAN IPv4 address" spellCheck={false} placeholder="LAN IPv4 address"
                onChange={event => { editedAddress.current = true; setAddress(event.target.value) }}
                onKeyDown={event => { if (event.key === "Enter" && !running && !busy && status) void run("start") }} /></span>
            </label>
            <button type="button" className={`btn ${running ? "" : "primary"}`} disabled={!!busy || !status} onClick={() => void run(running ? "stop" : "start")}>
              {busy ? <span className="spinner" /> : <Icon name={running ? "square" : "play"} />}
              {busy === "start" ? "Starting…" : busy === "stop" ? "Stopping…" : running ? "Stop host" : "Start host"}
            </button>
          </div>
        </header>

        {failure && <div className="wl-error" role="alert"><Icon name="alert" /><span>{failure}</span></div>}

        <div className="wl-grid">
          <aside className="wl-setup" aria-labelledby="wl-setup-title">
            <h3 id="wl-setup-title">PS5 setup</h3>
            <p>Connect the PS5 to the same network as this PC, then start the host.</p>
            <div className="wl-dns">
              <DnsValue label="Primary DNS" value={host || "—"} disabled={!host} onCopy={() => void copy(host, "Primary DNS")} />
              <DnsValue label="Secondary DNS" value="0.0.0.0" onCopy={() => void copy("0.0.0.0", "Secondary DNS")} />
            </div>
            <ol className="wl-instructions">
              <li>
                <strong>Set DNS to Manual</strong>
                <p>Settings → Network → Settings → Set Up Internet Connection → your network → Advanced Settings.</p>
                <p>Enter the two DNS addresses above.</p>
              </li>
              <li>
                <strong>Open User’s Guide</strong>
                <p>Settings → User’s Guide, Health &amp; Safety, and Other Information → User’s Guide.</p>
                <p>Follow the launcher on the console. Setup saves it for later sessions.</p>
              </li>
            </ol>
            <p className="wl-setup-note">After setup, use the installed launcher on the PS5 home screen. Restore DNS to Automatic when you stop using this host.</p>
            {running && status?.url && <a className="wl-preview-link" href={status.url} target="_blank" rel="noreferrer"><Icon name="globe" />Open host in browser<Icon name="right" /></a>}
          </aside>

          <div className="wl-session">
            <section className="wl-connection" aria-label="Observed connection">
              <div className="wl-console">
                <Icon name="gamepad" />
                <div>
                  <span>PS5</span><strong>{status?.consoleClient || "Waiting for console"}</strong>
                  <p>{consoleSeen ? `Last seen ${ago(consoleSeen, now).toLowerCase()}` : status?.consoleClient ? "No traffic seen from this address" : running ? "Open User’s Guide using this PC’s DNS" : "Start the host to see console traffic"}</p>
                </div>
                <div className="wl-counts"><span><b>{status?.dnsRequests ?? 0}</b> DNS</span><span><b>{status?.requests ?? 0}</b> HTTP</span></div>
              </div>
              <dl className="wl-traffic">
                <div>
                  <dt>DNS request</dt>
                  <dd><strong>{status?.lastDnsClient || "—"}</strong><span title={status?.lastDnsName || ""}>{status?.lastDnsName || "Waiting for a lookup"}</span></dd>
                  <dd className="wl-when">{ago(status?.lastDnsAt ?? null, now)}</dd>
                </div>
                <div>
                  <dt>Web request</dt>
                  <dd><strong>{status?.lastClient || "—"}</strong><span>{status?.lastClient ? "Last browser or console request" : "Waiting for a browser"}</span></dd>
                  <dd className="wl-when">{ago(status?.lastRequestAt ?? null, now)}</dd>
                </div>
                <div>
                  <dt>Payload Manager</dt>
                  <dd><strong className={managerReady ? "wl-ready" : ""}>{managerReady ? "Ready" : "Manual start"}</strong><span>{managerReady ? "Current session confirmed" : "The launcher starts only the ELF loader"}</span></dd>
                  <dd className="wl-when">{managerChecked ? `Checked ${ago(managerChecked, now).toLowerCase()}` : "—"}</dd>
                </div>
              </dl>
            </section>

            <section className="wl-terminal" aria-labelledby="wl-log-title">
              <header className="wl-terminal-head">
                <h3 id="wl-log-title">Host log</h3>
                <div>
                  <button type="button" className={`btn sm wl-follow ${follow ? "is-on" : ""}`} aria-pressed={follow} onClick={() => setFollow(value => !value)}><Icon name="chevD" />Follow</button>
                  <button type="button" className="btn sm icon-only" title="Copy host log" aria-label="Copy host log" disabled={!logs.length} onClick={() => void copy(logs.join("\n"), "Host log")}><Icon name="copy" /></button>
                </div>
              </header>
              <div ref={feed} className="wl-feed scroll" role="log" aria-label="Host activity" aria-live={follow ? "polite" : "off"} aria-relevant="additions text" tabIndex={0}
                onScroll={event => { const node = event.currentTarget; if (node.scrollHeight - node.scrollTop - node.clientHeight > 32) setFollow(false) }}>
                {logs.map((line, n) => <div className={`wl-log-line ${/\b(error|failed|refused|fatal)\b/i.test(line) ? "fail" : ""}`} key={`${n}:${line}`}><span aria-hidden>{n + 1}</span><code>{line}</code></div>)}
                {!logs.length && <p className="wl-feed-empty">{!status ? "Reading host status…" : running ? "Waiting for DNS and web requests…" : "Host is stopped. Start it to see DNS, web requests and launcher output."}</p>}
              </div>
              <p className="wl-log-note">Traffic confirms contact with this PC. Follow the console and its reported log messages for setup progress.</p>
            </section>
          </div>
        </div>

        <details className="tech wl-details">
          <summary>Ports, runtime and updates</summary>
          <div className="tech-body">
            <p>Console runtime {status?.version || "—"}. DNS uses UDP 53, HTTPS uses TCP 443, and HTTP uses TCP 80 for the browser preview. Allow SSPI on private networks if Windows Firewall asks. A busy port is reported in the log.</p>
            <p>WebKit Autoloader uses its original interface and starts the ELF loader on port 9021. Payload Manager and saved autoload.txt entries do not run automatically.</p>
            <p>Launcher updates arrive with SSPI updates. Host again and rerun setup on the PS5 to refresh its cached interface. Saved payloads and autoload.txt are kept.</p>
          </div>
        </details>
      </div>
    </div>
  )
}

function DnsValue({ label, value, disabled, onCopy }: { label: string; value: string; disabled?: boolean; onCopy: () => void }) {
  return <button type="button" className="wl-dns-value" disabled={disabled} onClick={onCopy} title={`Copy ${label.toLowerCase()}`}><span>{label}</span><strong>{value}</strong><Icon name="copy" /></button>
}
