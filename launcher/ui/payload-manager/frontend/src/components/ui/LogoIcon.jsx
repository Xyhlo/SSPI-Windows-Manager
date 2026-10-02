import React from 'react'
import logo from '../../assets/sspi-logo.png'

export default function LogoIcon({ className = '' }) {
  return <img src={logo} className={`sspi-logo ${className}`} alt="SSPI" />
}
