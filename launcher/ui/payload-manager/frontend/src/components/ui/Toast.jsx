import React, { useEffect } from 'react'
import { CheckCircle2, AlertTriangle } from 'lucide-react'

export default function Toast({ message, type = 'success', onClose }) {
  useEffect(() => { const timer = setTimeout(onClose, 3000); return () => clearTimeout(timer) }, [onClose])
  return <div className={`sspi-toast ${type === 'success' ? 'success' : 'error'}`} role="status">
    {type === 'success' ? <CheckCircle2 size={19} /> : <AlertTriangle size={19} />}<span>{message}</span>
  </div>
}
