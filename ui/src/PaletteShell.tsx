// The centred modal chrome shared by every palette (⌘K, ⌘O, ⌘P, help) and
// the first-run name prompt. `locked` disables the click-outside dismissal
// (and, via the data attribute, the global Esc in App).

import { useEffect, useRef } from 'react'

const FOCUSABLE =
  'input:not([disabled]), textarea:not([disabled]), button:not([disabled]), [href], [tabindex]:not([tabindex="-1"])'

export default function PaletteShell({
  children,
  onClose,
  locked = false,
}: {
  children: React.ReactNode
  onClose: () => void
  /** true → clicking the backdrop does nothing (caller also owns Esc) */
  locked?: boolean
}) {
  const box = useRef<HTMLDivElement>(null)
  // focus returns to whatever had it when the palette opened
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null
    return () => {
      if (prev && document.contains(prev)) prev.focus()
    }
  }, [])
  const trapTab = (e: React.KeyboardEvent) => {
    if (e.key !== 'Tab' || !box.current) return
    const items = Array.from(box.current.querySelectorAll<HTMLElement>(FOCUSABLE))
    if (items.length === 0) {
      e.preventDefault()
      return
    }
    const first = items[0]
    const last = items[items.length - 1]
    const active = document.activeElement
    if (e.shiftKey && (active === first || !box.current.contains(active))) {
      e.preventDefault()
      last.focus()
    } else if (!e.shiftKey && active === last) {
      e.preventDefault()
      first.focus()
    }
  }
  return (
    <div className="palette-backdrop" data-locked={locked || undefined} onMouseDown={locked ? undefined : onClose}>
      <div
        ref={box}
        className="palette"
        role="dialog"
        aria-modal="true"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={trapTab}
      >
        {children}
      </div>
    </div>
  )
}
