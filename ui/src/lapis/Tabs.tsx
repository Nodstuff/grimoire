import { useRef, type ReactNode } from 'react'

export interface TabItem<T extends string> {
  id: T
  label: ReactNode
  /** e.g. a CountPill */
  extra?: ReactNode
}

/** A row of tabs (role=tablist), arrow keys move between them. `variant`
 * 'underline' for page sections (Settings), 'segmented' for a view switch
 * (List / Cards). The caller renders the panel for `value`. */
export function Tabs<T extends string>({
  items,
  value,
  onChange,
  label,
  variant = 'underline',
  idPrefix = 'tab',
}: {
  items: TabItem<T>[]
  value: T
  onChange: (id: T) => void
  /** the tablist's accessible name */
  label: string
  variant?: 'underline' | 'segmented'
  /** tab ids are `${idPrefix}-${id}`; give the panel aria-labelledby that */
  idPrefix?: string
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([])
  const onKey = (e: React.KeyboardEvent, i: number) => {
    const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0
    if (!d) return
    e.preventDefault()
    const n = (i + d + items.length) % items.length
    refs.current[n]?.focus()
    onChange(items[n].id)
  }
  return (
    <div className={`lp-tabs lp-tabs-${variant}`} role="tablist" aria-label={label}>
      {items.map((t, i) => (
        <button
          key={t.id}
          ref={(el) => {
            refs.current[i] = el
          }}
          type="button"
          role="tab"
          id={`${idPrefix}-${t.id}`}
          aria-selected={t.id === value}
          tabIndex={t.id === value ? 0 : -1}
          className={`lp-tab${t.id === value ? ' on' : ''}`}
          onClick={() => onChange(t.id)}
          onKeyDown={(e) => onKey(e, i)}
        >
          {t.label}
          {t.extra}
        </button>
      ))}
    </div>
  )
}
