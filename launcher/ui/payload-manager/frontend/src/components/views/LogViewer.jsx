import React, { useState, useEffect, useRef } from 'react'
import { ChevronDown } from 'lucide-react'
import { useTranslation } from 'react-i18next'

export default function LogViewer({ logs }) {
  const { t } = useTranslation()
  const scrollRef = useRef(null)
  const [isAtBottom, setIsAtBottom] = useState(true)
  const [hasNewLogs, setHasNewLogs] = useState(false)
  const handleScroll = () => {
    if (!scrollRef.current) return
    const { scrollTop, scrollHeight, clientHeight } = scrollRef.current
    const atBottom = scrollHeight - scrollTop - clientHeight < 100
    setIsAtBottom(atBottom)
    if (atBottom) setHasNewLogs(false)
  }
  useEffect(() => {
    if (isAtBottom && scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight
    else setHasNewLogs(true)
  }, [logs, isAtBottom])
  const scrollToBottom = () => {
    if (scrollRef.current) scrollRef.current.scrollTop = scrollRef.current.scrollHeight
    setIsAtBottom(true); setHasNewLogs(false)
  }
  return <div className="sspi-log-viewer">
    <div ref={scrollRef} onScroll={handleScroll} className="sspi-log-lines" tabIndex={0} aria-label={t('app.logs.title', 'Logs')}>
      {logs.map((log, i) => <div key={`${i}-${log}`} className="sspi-log-line"><span>{i + 1}</span><code>{log}</code></div>)}
    </div>
    {!isAtBottom && hasNewLogs && <button className="sspi-button sspi-log-follow" onClick={scrollToBottom}><ChevronDown size={18} />{t('logs.new_activity_btn', 'New Activity Below')}</button>}
  </div>
}
