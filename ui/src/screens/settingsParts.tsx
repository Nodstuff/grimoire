// The pieces every Settings section is made of (canvas Settings): the
// section card (.sect: an h2, an optional sub line), hairline rows, the
// segmented radio group, and the show-once secret card with its
// label / value / Copy grid.

import { useRef, type ReactNode } from 'react'
import { errText, notify } from '../Notice'
import { copyText } from '../Profile'
import { Button, Icon } from '../lapis'
import type { SettingsSection } from '../deeplink'

export function SettingsCard({ id, title, sub, children }: { id: SettingsSection; title: string; sub?: ReactNode; children?: ReactNode }) {
  return (
    <section id={`settings-${id}`} data-section={id} className="lp-card st-sect" aria-labelledby={`st-h-${id}`}>
      <h2 id={`st-h-${id}`}>{title}</h2>
      {sub && <p className="st-sub">{sub}</p>}
      {children}
    </section>
  )
}

/** A row with the hairline above it; `first` drops the line (the first row under the heading). */
export function Row({ children, first, className, align }: { children: ReactNode; first?: boolean; className?: string; align?: 'start' | 'end' }) {
  return <div className={['st-row', first ? 'st-row-first' : '', align ? `st-row-${align}` : '', className ?? ''].filter(Boolean).join(' ')}>{children}</div>
}

/** The row's label, with an optional muted second line. */
export function RowText({ children, sub }: { children: ReactNode; sub?: ReactNode }) {
  return (
    <span className="st-row-text">
      {children}
      {sub && <span className="st-row-sub">{sub}</span>}
    </span>
  )
}

/** A segmented single choice (role=radiogroup; arrows move the choice). */
export function Segmented<T extends string>({ label, value, options, onChange }: { label: string; value: T; options: { id: T; label: ReactNode }[]; onChange: (v: T) => void }) {
  const refs = useRef<(HTMLButtonElement | null)[]>([])
  return (
    <div className="st-seg" role="radiogroup" aria-label={label}>
      {options.map((o, i) => (
        <button
          key={o.id}
          ref={(el) => {
            refs.current[i] = el
          }}
          type="button"
          role="radio"
          aria-checked={o.id === value}
          tabIndex={o.id === value ? 0 : -1}
          className={o.id === value ? 'on' : ''}
          onClick={() => onChange(o.id)}
          onKeyDown={(e) => {
            const d = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0
            if (!d) return
            e.preventDefault()
            const n = (i + d + options.length) % options.length
            refs.current[n]?.focus()
            onChange(options[n].id)
          }}
        >
          {o.label}
        </button>
      ))}
    </div>
  )
}

export function CopyButton({ what, value, variant = 'secondary' }: { what: string; value: string; variant?: 'secondary' | 'ghost' }) {
  return (
    <Button
      size="sm"
      variant={variant}
      aria-label={`Copy ${what}`}
      onClick={() =>
        copyText(value)
          .then(() => notify(`copied ${what}`, 'ok'))
          .catch((e) => notify(errText(e)))
      }
    >
      Copy
    </Button>
  )
}

/** The show-once card: an accent-ringed panel with a warning, label/value
 * rows each with Copy, and the "Done, I've saved it" button. */
export function SecretCard({
  title,
  warning,
  rows,
  onDone,
  label,
}: {
  title: ReactNode
  warning: string
  rows: { name: string; value: string | null; hint?: string }[]
  onDone: () => void
  /** the region's accessible name */
  label: string
}) {
  return (
    <div className="st-secret" role="region" aria-label={label}>
      <div className="st-secret-head">
        <b>{title}</b>
        <span className="st-secret-warn">
          <Icon name="info" size={15} />
          {warning}
        </span>
      </div>
      {rows.map((r) => (
        <div key={r.name} className="st-kv">
          <span className="muted">{r.name}</span>
          <code title={r.value ?? undefined}>{r.value ?? '—'}</code>
          {r.value ? <CopyButton what={r.name} value={r.value} /> : <span />}
          {r.hint && <span className="st-kv-hint">{r.hint}</span>}
        </div>
      ))}
      <div>
        <Button variant="primary" size="sm" onClick={onDone}>
          Done, I’ve saved it
        </Button>
      </div>
    </div>
  )
}
