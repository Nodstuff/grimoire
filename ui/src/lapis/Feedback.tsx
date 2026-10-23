import type { CSSProperties, ReactNode } from 'react'
import { Icon, type IconName } from './icons'

/** What a place shows with nothing in it: an icon in a soft tile, one
 * sentence, and the next step. Calm, never an error. */
export function EmptyState({
  icon = 'doc',
  title,
  children,
  action,
  compact = false,
}: {
  icon?: IconName
  title: ReactNode
  /** one line on what fills it, or why it is empty */
  children?: ReactNode
  /** a Button or two */
  action?: ReactNode
  /** inside a card: less padding */
  compact?: boolean
}) {
  return (
    <div className={`lp-empty${compact ? ' lp-empty-compact' : ''}`}>
      <span className="lp-empty-icon" aria-hidden>
        <Icon name={icon} size={22} />
      </span>
      <div className="lp-empty-title">{title}</div>
      {children && <div className="lp-empty-text">{children}</div>}
      {action && <div className="lp-empty-action">{action}</div>}
    </div>
  )
}

/** An indeterminate ring, `label` read aloud ("Loading…"). */
export function Spinner({ size = 16, label = 'Loading…' }: { size?: number; label?: string }) {
  return (
    <span className="lp-spinner" role="status" aria-label={label} style={{ width: size, height: size }}>
      <svg viewBox="0 0 24 24" width={size} height={size} aria-hidden>
        <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeOpacity="0.2" strokeWidth="2.5" />
        <path d="M21 12a9 9 0 0 0-9-9" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" />
      </svg>
    </span>
  )
}

/** A shimmering placeholder block while data loads. */
export function Skeleton({ width = '100%', height = 12, radius = 6, style }: { width?: number | string; height?: number | string; radius?: number; style?: CSSProperties }) {
  return <span className="lp-skeleton" aria-hidden style={{ width, height, borderRadius: radius, ...style }} />
}

/** A few skeleton lines, for a list or a card body. */
export function SkeletonLines({ lines = 3 }: { lines?: number }) {
  const widths = ['92%', '78%', '64%', '86%', '70%']
  return (
    <div className="lp-skeleton-lines" role="status" aria-label="Loading…">
      {Array.from({ length: lines }, (_, i) => (
        <Skeleton key={i} width={widths[i % widths.length]} />
      ))}
    </div>
  )
}
