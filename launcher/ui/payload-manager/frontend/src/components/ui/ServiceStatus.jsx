import React, { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { describeLoaderStatus, managerIsReady } from '../../utils/serviceStatus'

export default function ServiceStatus() {
  const { t } = useTranslation()
  const [manager, setManager] = useState(null)
  const [loader, setLoader] = useState(null)
  useEffect(() => {
    let active = true, timer
    const read = path => new Promise(resolve => {
      const controller = typeof AbortController === 'undefined' ? null : new AbortController()
      const timeout = setTimeout(() => { if (controller) controller.abort(); resolve(null) }, 4000)
      fetch(path, { cache: 'no-store', ...(controller ? { signal: controller.signal } : {}) })
        .then(response => response.ok ? response.json() : null).catch(() => null)
        .then(value => { clearTimeout(timeout); resolve(value) })
    })
    // Keep the recurring callback a normal named function: the console-target
    // async transform can lose a self-reference when it inlines an async arrow.
    function refresh() {
      return Promise.all([read('/sspi/identity'), read('/sspi/loader-status')]).then(results => {
        if (!active) return
        setManager(results[0]); setLoader(results[1])
        timer = setTimeout(refresh, 15000)
      })
    }
    refresh()
    return () => { active = false; clearTimeout(timer) }
  }, [])
  const loaderState = describeLoaderStatus(loader)
  return <div className="sspi-services" aria-label={t('sspi.services', 'Console services')}>
    <span className={`sspi-service ${managerIsReady(manager) ? 'ready' : ''}`}><span className="sspi-service-dot" />{t('sspi.manager', 'Manager')}<strong>{managerIsReady(manager) ? t('sspi.ready', 'Ready') : t('sspi.status_unknown', 'Status unavailable')}</strong></span>
    <span className={`sspi-service ${loaderState === 'listening' ? 'ready' : ''}`} title={t('sspi.loader_evidence', 'Console listener status. Reachability from your computer is separate.')}><span className="sspi-service-dot" />ELF loader · 9021<strong>{loaderState === 'listening' ? t('sspi.listening', 'Listening') : loaderState === 'unavailable' ? t('sspi.unavailable', 'Unavailable') : t('sspi.status_unknown', 'Status unavailable')}</strong></span>
  </div>
}
