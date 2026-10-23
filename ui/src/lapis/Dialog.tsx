// Modal surfaces. window.alert/confirm are silent no-ops in the Mac app's
// WKWebView, so every question to the person is one of these (or an inline
// Confirm). Both trap Tab, close on Esc and on a scrim click (unless
// `locked`), and hand focus back to whatever had it.

import { useEffect, useId, useRef, type ReactNode } from 'react'
import { IconButton } from './Button'

const FOCUSABLE =
  'input:not([disabled]), textarea:not([disabled]), select:not([disabled]), button:not([disabled]), [href], [tabindex]:not([tabindex="-1"])'

/** Tab cycles inside `box`; focus returns to the opener on unmount. Returns
 * the keydown handler to put on the box. */
export function useFocusTrap(box: React.RefObject<HTMLElement | null>, { autoFocus = true } = {}) {
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null
    if (autoFocus && box.current && !box.current.contains(document.activeElement)) {
      const first =
        box.current.querySelector<HTMLElement>('[data-autofocus]') ?? box.current.querySelector<HTMLElement>(FOCUSABLE)
      first?.focus()
    }
    return () => {
      if (prev && document.contains(prev)) prev.focus()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  return (e: React.KeyboardEvent) => {
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
}

export interface DialogProps {
  open?: boolean
  onClose: () => void
  /** the heading; also the accessible name */
  title?: ReactNode
  /** a line under the title */
  description?: ReactNode
  children?: ReactNode
  /** the button row (right-aligned): primary action last */
  footer?: ReactNode
  /** no Esc, no scrim click: the caller owns closing (a request in flight) */
  locked?: boolean
  width?: number
  /** accessible name when there is no visible title */
  label?: string
}

/** A centred modal card on a blurred scrim. */
export function Dialog({ open = true, onClose, title, description, children, footer, locked = false, width = 480, label }: DialogProps) {
  const box = useRef<HTMLDivElement>(null)
  const trap = useFocusTrap(box)
  const titleId = useId()
  if (!open) return null
  return (
    <div className="lp-scrim" data-locked={locked || undefined} onMouseDown={locked ? undefined : onClose}>
      <div
        ref={box}
        className="lp-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        aria-label={title ? undefined : label}
        style={{ width: `min(${width}px, calc(100vw - 32px))` }}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation()
            if (!locked) onClose()
            return
          }
          trap(e)
        }}
      >
        {title && (
          <div className="lp-dialog-head">
            <h2 id={titleId}>{title}</h2>
            {!locked && <IconButton icon="close" label="Close" size="sm" onClick={onClose} />}
          </div>
        )}
        {description && <p className="lp-dialog-desc">{description}</p>}
        {children && <div className="lp-dialog-body">{children}</div>}
        {footer && <div className="lp-dialog-foot">{footer}</div>}
      </div>
    </div>
  )
}

/** A panel that slides in from the right on a desktop and up from the
 * bottom on a phone (`side="auto"`). For side work: members, filters,
 * a book's details. */
export function Sheet({
  open = true,
  onClose,
  title,
  children,
  footer,
  side = 'auto',
  locked = false,
  label,
}: Omit<DialogProps, 'width' | 'description'> & { side?: 'auto' | 'right' | 'bottom' }) {
  const box = useRef<HTMLDivElement>(null)
  const trap = useFocusTrap(box)
  const titleId = useId()
  if (!open) return null
  return (
    <div className="lp-scrim lp-scrim-sheet" data-locked={locked || undefined} onMouseDown={locked ? undefined : onClose}>
      <div
        ref={box}
        className={`lp-sheet lp-sheet-${side}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={title ? titleId : undefined}
        aria-label={title ? undefined : label}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation()
            if (!locked) onClose()
            return
          }
          trap(e)
        }}
      >
        <div className="lp-dialog-head">
          {title ? <h2 id={titleId}>{title}</h2> : <span />}
          {!locked && <IconButton icon="close" label="Close" size="sm" onClick={onClose} />}
        </div>
        <div className="lp-sheet-body">{children}</div>
        {footer && <div className="lp-dialog-foot">{footer}</div>}
      </div>
    </div>
  )
}
