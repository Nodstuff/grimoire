// A filter chip that opens a short menu of radio choices ("Workspace: All",
// "Written by: anyone", "Status: any"). Esc and a click outside close it;
// arrow keys move. Shared by Search and the Library.

import { useEffect, useRef, useState } from 'react'
import { Icon } from '../lapis'
import './chipmenu.css'

/** A filter chip that opens a short menu of choices (radio items). */
export default function ChipMenu<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string
  value: T
  options: { id: T; label: string; dot?: string }[]
  onChange: (v: T) => void
}) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLDivElement>(null)
  const btn = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    if (!open) return
    const off = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false)
    }
    window.addEventListener('mousedown', off)
    box.current?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus()
    return () => window.removeEventListener('mousedown', off)
  }, [open])
  const cur = options.find((o) => o.id === value)
  return (
    <div className="lp-chipmenu" ref={box}>
      <button
        ref={btn}
        type="button"
        className={`lp-fchip${value !== options[0]?.id ? ' on' : ''}`}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        {label}: {cur?.label ?? value}
        <Icon name="chevronDown" size={12} />
      </button>
      {open && (
        <div
          className="lp-chipmenu-list"
          role="menu"
          aria-label={label}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              e.stopPropagation()
              setOpen(false)
              btn.current?.focus()
              return
            }
            if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
            e.preventDefault()
            const items = Array.from(box.current?.querySelectorAll<HTMLElement>('[role="menuitemradio"]') ?? [])
            const i = items.indexOf(document.activeElement as HTMLElement)
            items[e.key === 'ArrowDown' ? Math.min(i + 1, items.length - 1) : Math.max(i - 1, 0)]?.focus()
          }}
        >
          {options.map((o) => (
            <button
              key={o.id}
              type="button"
              role="menuitemradio"
              aria-checked={o.id === value}
              onClick={() => {
                onChange(o.id)
                setOpen(false)
                btn.current?.focus()
              }}
            >
              {o.dot && <span className="lp-dot" style={{ background: o.dot }} aria-hidden />}
              <span>{o.label}</span>
              {o.id === value && <Icon name="check" size={14} className="lp-chipmenu-check" />}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}

