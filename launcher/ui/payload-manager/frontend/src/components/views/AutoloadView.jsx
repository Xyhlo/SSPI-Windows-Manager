import React, { useState, useEffect } from 'react'
import { RefreshCw, ArrowLeft, ArrowRight, Activity, Zap, ChevronUp, ChevronDown, Trash2, CheckCircle2 } from 'lucide-react'
import { cn, isPS5, isSystemPayload } from '../../utils/helpers'
import { useTranslation } from 'react-i18next'
import PayloadName from '../ui/PayloadName'
import Modal from '../ui/Modal'

const AutoloadView = ({ payloads, config, onSaveConfig, onToast, onRedirect }) => {
  const { t } = useTranslation()
  const [subView, setSubView] = useState('list')
  const [enabled, setEnabled] = useState(false)
  const [autoloadList, setAutoloadList] = useState([])
  const [showDelayModal, setShowDelayModal] = useState(false)
  const [customDelay, setCustomDelay] = useState('')
  const [saved, setSaved] = useState(false)
  const [saving, setSaving] = useState(false)
  const [isInitialized, setIsInitialized] = useState(false)
  const lastSyncedRef = React.useRef('')

  // Load initial config
  useEffect(() => {
    if (config) {
      const en = config.AUTOLOAD_ENABLED === true || config.AUTOLOAD_ENABLED === "true"
      const listStr = config.AUTOLOAD_LIST || ''
      setEnabled(en)
      setAutoloadList(listStr.split(',').filter(x => x))
      lastSyncedRef.current = `${en}:${listStr}`
      setIsInitialized(true)
    }
  }, [config])

  // Debounced Auto-Save
  useEffect(() => {
    if (!isInitialized) return

    const currentState = `${enabled}:${autoloadList.join(',')}`
    if (currentState === lastSyncedRef.current) return

    const timer = setTimeout(async () => {
      const shouldEnable = enabled
      const finalList = autoloadList.map(p => p === 'DELAY' ? '!1000' : p)
      const finalStr = finalList.join(',')

      setSaving(true)
      const success = await onSaveConfig({
        AUTOLOAD_ENABLED: shouldEnable,
        AUTOLOAD_LIST: finalStr
      })

      if (success) {
        lastSyncedRef.current = `${shouldEnable}:${finalStr}`
        setSaved(true)
        setTimeout(() => setSaved(false), 2000)
      }
      setSaving(false)
    }, 1500)

    return () => clearTimeout(timer)
  }, [autoloadList, enabled, isInitialized, onSaveConfig])

  const internalPayloads = payloads.filter(p => !p.includes('/mnt/usb') && !isSystemPayload(p)).map(p => p.split('/').pop())
  const availablePayloads = internalPayloads.filter(p => !autoloadList.includes(p))

  const handleToggle = (val) => {
    setEnabled(val)
  }

  const addPayload = (p) => {
    const isKstuff = p.toLowerCase().includes('kstuff');
    if (isKstuff) {
      const existing = autoloadList.find(x => x.toLowerCase().includes('kstuff'));
      if (existing) {
        onToast(t("autoload.conflict_kstuff", "Conflict: Multiple KStuff payloads detected."), 'error');
        return;
      }
    }
    setAutoloadList([...autoloadList, p]);
    setSubView('list')
  }

  const addDelay = (ms) => {
    setAutoloadList([...autoloadList, `!${ms}`])
    setShowDelayModal(false)
    setSubView('list')
  }

  const moveUp = (index) => {
    if (index === 0) return
    const newList = [...autoloadList]
      ;[newList[index - 1], newList[index]] = [newList[index], newList[index - 1]]
    setAutoloadList(newList)
  }

  const moveDown = (index) => {
    if (index === autoloadList.length - 1) return
    const newList = [...autoloadList]
      ;[newList[index + 1], newList[index]] = [newList[index], newList[index + 1]]
    setAutoloadList(newList)
  }

  return (
    <div className="sspi-page">
      <div className="sspi-page-heading">
        <div><h2>{t('autoload.sequence_title_1', 'Autoload')} {t('autoload.sequence_title_2', 'Sequence')}</h2>
          <p className="sspi-muted">{saving ? t('autoload.saving', 'Saving Changes...') : saved ? t('autoload.saved', 'All Changes Saved') : t('autoload.enable_desc', 'Chain multiple payloads to be executed automatically every time Payload Manager starts.')}</p>
        </div>
        <button className={enabled ? 'sspi-button' : 'sspi-button primary'} onClick={() => handleToggle(!enabled)}>{enabled ? t('autoload.disable_btn', 'Disable Autoload') : t('autoload.enable_btn', 'Enable Autoload')}</button>
      </div>
      {enabled ? <div className="sspi-autoload-columns">
        <section className="sspi-section">
          <h3>{t('autoload.sequence_title_2', 'Sequence')} <span className="sspi-count">{autoloadList.length}</span></h3>
          <div className="sspi-list">
            {autoloadList.map((p, i) => <div key={`${p}-${i}`} className="sspi-data-row">
              <span className="sspi-order">{i + 1}</span><PayloadName path={p} stacked />
              <div className="sspi-row-actions">
                <button className="sspi-icon-button" aria-label={t('sspi.move_earlier', 'Move {{name}} earlier', { name: p })} onClick={() => moveUp(i)} disabled={i === 0}><ChevronUp size={18} /></button>
                <button className="sspi-icon-button" aria-label={t('sspi.move_later', 'Move {{name}} later', { name: p })} onClick={() => moveDown(i)} disabled={i === autoloadList.length - 1}><ChevronDown size={18} /></button>
                <button className="sspi-icon-button danger" aria-label={t('sspi.remove_item', 'Remove {{name}}', { name: p })} onClick={() => setAutoloadList(autoloadList.filter((_, idx) => idx !== i))}><Trash2 size={18} /></button>
              </div>
            </div>)}
            {!autoloadList.length && <div className="sspi-empty"><RefreshCw size={32} /><p>{t('autoload.sequence_empty', 'Sequence Empty')}</p></div>}
          </div>
        </section>
        <section className="sspi-section">
          <h3>{t('autoload.available_title', 'Available Payloads')}</h3>
          <div className="sspi-list">
            {availablePayloads.map(p => {
              const blocked = p.toLowerCase().includes('kstuff') && autoloadList.some(item => item.toLowerCase().includes('kstuff'))
              return <button key={p} className="sspi-data-row interactive" onClick={() => !blocked && addPayload(p)} disabled={blocked}><PayloadName path={p} stacked /><ArrowRight size={19} /></button>
            })}
          </div>
          <div className="sspi-inline-actions">
            <button className="sspi-button" onClick={() => setShowDelayModal(true)}><Zap size={18} />{t('autoload.add_delay_btn', 'Add Delay')}</button>
            <button className="sspi-button quiet" onClick={() => onRedirect('storage', 'usb-storage')}>{t('autoload.move_usb_btn', 'Move from USB to Internal')}</button>
          </div>
          <p className="sspi-muted">{t('autoload.move_usb_desc', 'Required for payloads you want to use in the Autoload sequence.')}</p>
        </section>
      </div> : <div className="sspi-empty"><RefreshCw size={36} /><p>{t('sspi.autoload_off', 'Autoload is off. Your saved sequence is kept.')}</p></div>}
      <Modal show={showDelayModal} title={t('autoload.delay_modal.title', 'Configure Delay')} onClose={() => setShowDelayModal(false)} footer={<button className="sspi-button" onClick={() => setShowDelayModal(false)}>{t('autoload.delay_modal.cancel', 'Cancel')}</button>}>
        <div className="sspi-inline-actions">{[1, 3, 5].map(seconds => <button className="sspi-button" key={seconds} onClick={() => addDelay(seconds * 1000)}>{seconds}s</button>)}</div>
        <label className="sspi-field"><span>{t('autoload.delay_modal.custom_delay_label', 'Custom Delay (ms)')}</span><div className="sspi-inline-actions"><input type="number" min="1" value={customDelay} onChange={event => setCustomDelay(event.target.value)} placeholder={t('autoload.delay_modal.placeholder', 'e.g. 2500')} /><button className="sspi-button primary" onClick={() => customDelay && addDelay(parseInt(customDelay))}>{t('autoload.delay_modal.add_btn', 'Add')}</button></div></label>
      </Modal>
    </div>
  )
}

export default AutoloadView
