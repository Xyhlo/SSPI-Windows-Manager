import React, { useState, useEffect } from 'react'
import {
  ArrowLeft, Plus, Trash2, ChevronUp, ChevronDown,
  Lock, Globe, Loader2, AlertTriangle
} from 'lucide-react'
import { QRCodeSVG } from 'qrcode.react'
import { useTranslation } from 'react-i18next'
import { cn, isPS5 } from '../../utils/helpers'

const ManageSourcesView = ({ onBack, ip, addToast, showConfirm }) => {
  const { t } = useTranslation()
  const [sources, setSources] = useState([])
  const [loading, setLoading] = useState(true)
  const [newUrl, setNewUrl] = useState('')
  const [adding, setAdding] = useState(false)
  const [addError, setAddError] = useState('')
  const [showAddForm, setShowAddForm] = useState(false)

  useEffect(() => {
    fetch('/sources_list')
      .then(r => r.json())
      .then(d => {
        if (d?.sources) setSources(d.sources)
      })
      .catch(() => { })
      .finally(() => setLoading(false))
  }, [])

  const saveSources = async (updated) => {
    try {
      const res = await fetch('/sources_set', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ sources: updated })
      })
      if (res.ok) {
        addToast(t("manage_sources.saved", "Sources saved"))
      } else {
        addToast(t("manage_sources.save_failed", "Failed to save sources"), 'error')
      }
    } catch {
      addToast(t("manage_sources.save_failed", "Failed to save sources"), 'error')
    }
  }

  const move = (idx, dir) => {
    if (idx + dir < 1 || idx + dir >= sources.length) return
    const updated = [...sources]
      ;[updated[idx], updated[idx + dir]] = [updated[idx + dir], updated[idx]]
    setSources(updated)
    saveSources(updated)
  }

  const remove = (idx) => {
    if (idx === 0) return
    const src = sources[idx]
    showConfirm(
      t("manage_sources.remove_title", "Remove Source"),
      t("manage_sources.remove_message", "Remove \"{{name}}\" from your sources?", { name: src.name }),
      () => {
        const updated = sources.filter((_, i) => i !== idx)
        setSources(updated)
        saveSources(updated)
      }
    )
  }

  const handleAdd = async (e) => {
    e.preventDefault()
    setAddError('')
    if (!newUrl.trim()) return
    setAdding(true)
    try {
      const res = await fetch(`/sources_add?url=${encodeURIComponent(newUrl.trim())}`)
      const data = await res.json()
      if (data.ok) {
        // Reload sources list
        const listRes = await fetch('/sources_list')
        const listData = await listRes.json()
        if (listData?.sources) setSources(listData.sources)
        setNewUrl('')
        setShowAddForm(false)
        addToast(t("manage_sources.added", "\"{{name}}\" added", { name: data.name }))
      } else {
        setAddError(data.message || t("manage_sources.add_failed", "Failed to add source"))
      }
    } catch {
      setAddError(t("manage_sources.add_request_failed", "Request failed. Check the URL and try again."))
    }
    setAdding(false)
  }

  return <div className="sspi-page sspi-sources">
    <div className="sspi-page-heading"><div className="sspi-inline-actions"><button className="sspi-icon-button" onClick={onBack} aria-label={t('sspi.back_settings', 'Back to settings')}><ArrowLeft size={21} /></button><h2>{t('manage_sources.web_title_1', 'Payload')} {t('manage_sources.web_title_2', 'Sources')}</h2></div></div>
    {isPS5 ? <div className="sspi-remote"><div className="sspi-qr"><QRCodeSVG value={`http://${ip}:8084`} size={152} level="M" /></div><div><h3>{ip}:8084</h3><p>{t('manage_sources.ps5_description', 'Open this address on your phone or PC to manage payload sources.')}</p></div></div> : <>
      {loading ? <div className="sspi-empty"><Loader2 className="animate-spin" size={30} /></div> : <div className="sspi-list">{sources.map((src, idx) => <div key={src.id} className="sspi-data-row">
        <span className="sspi-order">{idx + 1}</span>{src.removable ? <Globe size={20} /> : <Lock size={20} />}<span className="sspi-grow"><strong>{src.name}</strong><small className="sspi-source-url">{src.url}</small></span>
        {src.removable ? <div className="sspi-row-actions"><button className="sspi-icon-button" onClick={() => move(idx, -1)} disabled={idx <= 1} aria-label={t('manage_sources.move_up_btn', 'Move up')}><ChevronUp size={18} /></button><button className="sspi-icon-button" onClick={() => move(idx, 1)} disabled={idx === sources.length - 1} aria-label={t('manage_sources.move_down_btn', 'Move down')}><ChevronDown size={18} /></button><button className="sspi-icon-button danger" onClick={() => remove(idx)} aria-label={t('manage_sources.remove_btn', 'Remove source')}><Trash2 size={18} /></button></div> : <span className="sspi-count">{t('manage_sources.default_badge', 'Default')}</span>}
      </div>)}</div>}
      {!showAddForm ? <button className="sspi-button" onClick={() => { setShowAddForm(true); setAddError('') }}><Plus size={18} />{t('manage_sources.add_source_btn', 'Add Source')}</button> : <form className="sspi-form" onSubmit={handleAdd}>
        <h3>{t('manage_sources.add_new_title', 'Add a New Source')}</h3><p className="sspi-muted">{t('manage_sources.add_new_desc', 'Paste the URL to a JSON file.')}</p>
        <label className="sspi-field"><span>{t('manage_sources.source_url_label', 'Source URL')}</span><input type="url" value={newUrl} onChange={event => setNewUrl(event.target.value)} placeholder="https://example.com/payloads.json" autoFocus disabled={adding} /></label>
        <div className="sspi-inline-actions"><button type="submit" className="sspi-button primary" disabled={adding || !newUrl.trim()}>{adding ? t('manage_sources.validating', 'Validating...') : t('manage_sources.add_btn', 'Add')}</button><button type="button" className="sspi-button" disabled={adding} onClick={() => { setShowAddForm(false); setNewUrl(''); setAddError('') }}>{t('manage_sources.cancel_btn', 'Cancel')}</button></div>
        {addError && <p className="sspi-error" role="alert">{addError}</p>}
      </form>}
    </>}
  </div>
}

export default ManageSourcesView
