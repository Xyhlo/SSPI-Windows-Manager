import React from 'react'
import { useTranslation } from 'react-i18next'
import { Package, Loader2, Globe, Star, ChevronUp, ChevronDown, CheckCircle2, Play } from 'lucide-react'
import PayloadName from './PayloadName'

export default function PayloadButton({ path, onClick, isLoading, sourceName, version, isFavorite, isLaunched, isEditMode, onMoveFavorite, canMoveLeft, canMoveRight }) {
  const { t } = useTranslation()
  const name = path.split('/').pop()
  return (
    <div className={`sspi-payload-row${isEditMode && isFavorite ? ' is-favorite' : ''}`}>
      <button type="button" onClick={onClick} disabled={isLoading} className="sspi-payload-launch" aria-pressed={isEditMode ? isFavorite : undefined}>
        <span className="sspi-payload-icon"><Package size={23} aria-hidden="true" /></span>
        <span className="sspi-payload-copy">
          <PayloadName path={path} version={version} className="sspi-payload-name" stacked />
          {path.startsWith('/mnt/usb') && <span className="sspi-payload-meta" title={path}>{path}</span>}
          {sourceName && !path.startsWith('/mnt/usb') && <span className="sspi-payload-meta"><Globe size={12} aria-hidden="true" />{sourceName}</span>}
        </span>
        <span className="sspi-payload-state">
          {isEditMode ? <Star size={20} fill={isFavorite ? 'currentColor' : 'none'} aria-hidden="true" /> :
            isLoading ? <Loader2 size={20} className="animate-spin" aria-hidden="true" /> :
            isLaunched ? <CheckCircle2 size={20} className="sspi-success" aria-label={t('app.dashboard.launched_tooltip', 'Launched recently')} /> :
            <Play size={18} aria-hidden="true" />}
        </span>
      </button>
      {isEditMode && isFavorite && (
        <div className="sspi-payload-reorder">
          <button type="button" onClick={() => onMoveFavorite(path, -1)} disabled={!canMoveLeft} aria-label={t('sspi.move_earlier', 'Move {{name}} earlier', { name })}><ChevronUp size={18} /></button>
          <button type="button" onClick={() => onMoveFavorite(path, 1)} disabled={!canMoveRight} aria-label={t('sspi.move_later', 'Move {{name}} later', { name })}><ChevronDown size={18} /></button>
        </div>
      )}
    </div>
  )
}
