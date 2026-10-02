import React, { useState, useEffect, useRef } from 'react'
import { CheckCircle2, AlertTriangle, Loader2 } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { cn } from '../../utils/helpers'
import PayloadName from '../ui/PayloadName'
import LogoIcon from '../ui/LogoIcon'

const AutoloadOverlay = ({ status, onCancel, onFinish, isPS5 }) => {
  const { t } = useTranslation()
  const isCountdown = status.remaining > 0 || (status.remaining === 0 && !status.current);
  const isExecuting = status.remaining === 0 && !!status.current && status.current !== 'DONE';
  const isDone = status.current === 'DONE';
  const payloadList = (typeof status.list === 'string') ? status.list.split(',').filter(p => p.trim() !== '') : [];
  const listRef = useRef(null);
  const displayTotal = status.total > 0 ? status.total : payloadList.length;
  const progress = displayTotal > 0 ? (status.done / displayTotal) : 0;

  const [localMs, setLocalMs] = useState(status.remaining_ms ?? (status.remaining * 1000));

  useEffect(() => {
    const serverMs = status.remaining_ms ?? (status.remaining * 1000);
    // Only sync downward (forward in time): server can pull us closer to 0 if we drift,
    // but never push us back. This ensures a smooth, non-jumping countdown.
    setLocalMs(prev => serverMs < prev ? serverMs : prev);
  }, [status.remaining_ms, status.remaining]);

  const isActiveRef = useRef(true);

  useEffect(() => {
    if (!isCountdown) return;
    let lastTime = performance.now();
    let frameId;
    const animate = (time) => {
      const delta = time - lastTime;
      lastTime = time;
      if (isActiveRef.current) {
        setLocalMs(prev => Math.max(0, prev - delta));
      }
      frameId = requestAnimationFrame(animate);
    };
    frameId = requestAnimationFrame(animate);
    return () => cancelAnimationFrame(frameId);
  }, [isCountdown]);

  useEffect(() => {
    if (listRef.current) {
      const activeItem = listRef.current.querySelector('[data-active="true"]');
      if (activeItem) {
        activeItem.scrollIntoView({ behavior: 'smooth', block: 'center' });
      }
    }
  }, [status.done]);

  return <div className="sspi-autoload-screen">
    <header className="sspi-autoload-brand"><LogoIcon /><span>Payload Manager</span></header>
    <main className="sspi-autoload-content">
      <div className="sspi-autoload-summary">
        <p className="sspi-muted">{t('autoload_overlay.autoloading', 'Autoloading')}</p>
        <h1>{isDone ? t('autoload_overlay.done_title_2', 'Done') : isCountdown ? `${Math.ceil(localMs / 1000)}s` : t('autoload_overlay.executing', 'Executing')}</h1>
        <p className="sspi-muted">{isDone ? t('autoload_overlay.all_loaded', 'All payloads loaded') : isCountdown ? t('autoload_overlay.waiting', 'Waiting for manual abort...') : t('autoload_overlay.loading', 'Loading Payloads...')}</p>
        <div className="sspi-progress"><span style={{ width: `${Math.max(0, Math.min(100, isDone ? 100 : progress * 100))}%` }} /></div>
        {!isDone && payloadList.some(p => p.toLowerCase().includes('etahen')) && payloadList.some(p => p.toLowerCase().includes('kstuff')) && <p className="sspi-warning">{t('autoload_overlay.conflict', 'Conflict: etaHEN + KStuff active')}</p>}
        <div className="sspi-inline-actions">{isDone ? <button className="sspi-button primary" autoFocus onClick={onFinish}>{t('autoload_overlay.return_btn', 'Return to Dashboard')}</button> : isCountdown ? <button className="sspi-button" autoFocus onClick={onCancel}>{t('autoload_overlay.abort_btn', 'Abort Autoload')}</button> : <p className="sspi-muted">{status.done} / {displayTotal}</p>}</div>
      </div>
      <section className="sspi-section"><h3>{t('autoload_overlay.payload_list', 'Payload List')} <span className="sspi-count">{isDone ? displayTotal : status.done} / {displayTotal}</span></h3><div className="sspi-list sspi-autoload-progress-list" ref={listRef}>{payloadList.map((name, index) => {
        const active = !isDone && isExecuting && index === status.done
        const done = isDone || index < status.done
        return <div key={index} data-active={active} className={`sspi-data-row${active ? ' is-active' : ''}`}><span className="sspi-order">{done ? <CheckCircle2 size={19} className="sspi-success" /> : active ? <Loader2 size={19} className="animate-spin" /> : index + 1}</span><PayloadName path={name} stacked />{done && <small className="sspi-success">{t('autoload_overlay.success', 'Success')}</small>}</div>
      })}</div></section>
    </main>
  </div>
}

export default AutoloadOverlay
