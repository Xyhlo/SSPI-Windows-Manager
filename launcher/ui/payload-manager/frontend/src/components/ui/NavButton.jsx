import React from 'react'

export default function NavButton({ active, onClick, icon: Icon, label }) {
  return (
    <button type="button" onClick={onClick} className={`sspi-nav-item${active ? ' is-active' : ''}`} aria-current={active ? 'page' : undefined}>
      <Icon size={19} aria-hidden="true" />
      <span>{label}</span>
    </button>
  )
}
