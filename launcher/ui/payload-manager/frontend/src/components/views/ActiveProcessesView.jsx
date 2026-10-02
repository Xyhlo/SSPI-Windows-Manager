import React, { useState, useEffect, useMemo } from 'react'
import { Cpu, RefreshCw, XCircle, Search, AlertTriangle, Activity, Loader2, Info, Trash2 } from 'lucide-react'
import { useTranslation, Trans } from 'react-i18next'
import { cn, isPS5 } from '../../utils/helpers'

const ActiveProcessesView = ({ ip, addToast, showConfirm }) => {
  const { t } = useTranslation()
  const [processes, setProcesses] = useState([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(false)
  const [showAll, setShowAll] = useState(false)
  const [search, setSearch] = useState('')

  const fetchProcesses = async (isBackground = false) => {
    if (!isBackground) setLoading(true)
    if (!isBackground) setError(false)
    try {
      const res = await fetch('/processes_list')
      if (!res.ok) throw new Error()
      const data = await res.json()
      if (data && data.processes) {
        setProcesses(data.processes)
      } else {
        setProcesses([])
      }
    } catch {
      if (!isBackground) setError(true)
    } finally {
      if (!isBackground) setLoading(false)
    }
  }

  useEffect(() => {
    fetchProcesses()
    
    const intervalId = setInterval(() => {
      fetchProcesses(true)
    }, 15000)

    return () => clearInterval(intervalId)
  }, [])

  const filteredProcesses = useMemo(() => {
    let result = processes
    if (!showAll) {
      result = result.filter(p => p.is_daemon)
    }
    if (search.trim() !== '') {
      const q = search.toLowerCase()
      result = result.filter(p => p.name.toLowerCase().includes(q))
    }
    return result
  }, [processes, showAll, search])

  const handleKill = (proc) => {
    const isCritical = proc.name === 'pldmgr.elf' || proc.name === 'elfldr.elf';
    if (isCritical) {
      addToast(t("active_processes.cannot_kill", "Cannot kill {{name}}", { name: proc.name }), "error")
      return
    }

    showConfirm(
      t("active_processes.kill_modal_title", "Kill Process"),
      t("active_processes.kill_modal_message", "Are you sure you want to kill {{name}} (PID: {{pid}})?", { name: proc.name, pid: proc.pid }),
      async () => {
        try {
          const res = await fetch(`/process_kill?pid=${proc.pid}`)
          const data = await res.json()
          if (res.ok) {
            setProcesses(prev => prev.filter(p => p.pid !== proc.pid));
            addToast(t("active_processes.kill_success", "Successfully killed {{name}}", { name: proc.name }))
            setTimeout(() => fetchProcesses(), 1000);
          } else {
            addToast(data.error || t("active_processes.kill_failed", "Failed to kill {{name}}", { name: proc.name }), "error")
          }
        } catch (e) {
          addToast(t("active_processes.kill_error", "Error killing {{name}}", { name: proc.name }), "error")
        }
      }
    )
  }

  return <div className="sspi-page">
    <div className="sspi-page-heading"><h2>{t('active_processes.title_active', 'Active')} {t('active_processes.title_processes', 'Processes')}</h2><label className="sspi-check"><input type="checkbox" checked={showAll} onChange={event => setShowAll(event.target.checked)} /><span>{t('active_processes.show_all', 'Show All System Processes')}</span></label></div>
    <label className="sspi-search"><Search size={19} /><input type="search" placeholder={t('active_processes.search_placeholder', 'Search processes by name...')} aria-label={t('active_processes.search_placeholder', 'Search processes by name...')} value={search} onChange={event => setSearch(event.target.value)} /></label>
    {loading && !processes.length ? <div className="sspi-empty"><Loader2 size={30} className="animate-spin" /><p>{t('active_processes.fetching', 'Fetching process list...')}</p></div> : error ? <div className="sspi-empty"><AlertTriangle size={30} /><p>{t('active_processes.error_loading', 'Failed to load processes')}</p><button className="sspi-button" onClick={() => fetchProcesses()}>{t('active_processes.retry', 'Retry')}</button></div> : !filteredProcesses.length ? <div className="sspi-empty"><Cpu size={30} /><p>{t('active_processes.no_processes', 'No processes found')}</p></div> : <div className="sspi-list">{filteredProcesses.map(p => {
      const critical = p.name === 'pldmgr.elf' || p.name === 'elfldr.elf'
      return <div key={p.pid} className="sspi-data-row"><span className="sspi-payload-icon"><Cpu size={22} /></span><span className="sspi-grow"><strong>{p.name}</strong><small>PID {p.pid} · {p.memory.toFixed(1)} MiB</small></span><button className="sspi-button danger" onClick={() => handleKill(p)} disabled={critical} title={critical ? t('active_processes.cannot_kill_tooltip', 'Cannot kill critical process') : undefined}><Trash2 size={16} />{t('active_processes.kill_button', 'Kill')}</button></div>
    })}</div>}
    <p className="sspi-note"><Trans i18nKey="active_processes.note_message" defaults="Some payloads that inject threads into system processes (like <1>SceShellCore</1>) will persist inside those processes even after their main process is killed." components={{ 1: <code /> }} /></p>
  </div>
}

export default ActiveProcessesView
