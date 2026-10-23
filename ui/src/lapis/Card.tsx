import type { HTMLAttributes, ReactNode } from 'react'
import { Icon, type IconName } from './icons'

/** The basic surface: card background, hairline, 14px radius, soft shadow.
 * `as="section"` for a region with a heading. `flush` drops the padding so
 * rows can run edge to edge (with their own top hairlines). */
export function Card({
  as: Tag = 'div',
  flush = false,
  className,
  children,
  ...rest
}: HTMLAttributes<HTMLElement> & { as?: 'div' | 'section' | 'article' | 'aside'; flush?: boolean; children?: ReactNode }) {
  return (
    <Tag className={['lp-card', flush ? 'lp-card-flush' : '', className ?? ''].filter(Boolean).join(' ')} {...rest}>
      {children}
    </Tag>
  )
}

/** A card's header row: icon, title (an <h2>), optional count, and actions
 * pushed right. */
export function CardHeader({
  icon,
  iconColor,
  title,
  id,
  count,
  actions,
}: {
  icon?: IconName
  iconColor?: string
  title: ReactNode
  /** the heading's id, for aria-labelledby on the card */
  id?: string
  count?: ReactNode
  actions?: ReactNode
}) {
  return (
    <div className="lp-card-head">
      {icon && <Icon name={icon} style={{ color: iconColor ?? 'var(--accent)' }} />}
      <h2 id={id}>{title}</h2>
      {count}
      {actions && <div className="lp-card-actions">{actions}</div>}
    </div>
  )
}

/** A row inside a flush card, with the hairline above it. */
export function CardRow({ className, children, ...rest }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={`lp-card-row${className ? ` ${className}` : ''}`} {...rest}>
      {children}
    </div>
  )
}

/** The small uppercase section label (sidebar groups, palette groups). */
export function SectionLabel({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={`lp-label${className ? ` ${className}` : ''}`}>{children}</div>
}
