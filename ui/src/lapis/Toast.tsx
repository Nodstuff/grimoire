import type { ReactNode } from 'react'
import { Icon, type IconName } from './icons'

export type ToastKind = 'ok' | 'warn' | 'error'

const KIND: Record<ToastKind, { icon: IconName; label: string }> = {
  ok: { icon: 'applied', label: 'Done' },
  warn: { icon: 'flagged', label: 'Note' },
  error: { icon: 'error', label: 'Problem' },
}

/** One toast. The stack and its timers live in Notice.tsx (`notify()`);
 * this is only the look. `onClick` makes the whole toast the action (undo,
 * open); `actionLabel` names it on the right. */
export function Toast({
  kind,
  children,
  onClick,
  actionLabel,
  onDismiss,
}: {
  kind: ToastKind
  children: ReactNode
  onClick?: () => void
  actionLabel?: string
  onDismiss?: () => void
}) {
  const k = KIND[kind]
  return (
    <div className={`lp-toast lp-toast-${kind}${onClick ? ' lp-toast-action' : ''}`} role={kind === 'error' ? 'alert' : undefined}>
      <Icon name={k.icon} size={16} strokeWidth={2} className="lp-toast-icon" label={k.label} />
      {onClick ? (
        <button type="button" className="lp-toast-msg" onClick={onClick}>
          {children}
          {actionLabel && <span className="lp-toast-cta">{actionLabel}</span>}
        </button>
      ) : (
        <span className="lp-toast-msg">{children}</span>
      )}
      {onDismiss && (
        <button type="button" className="lp-toast-x" aria-label="Dismiss" onClick={onDismiss}>
          <Icon name="close" size={14} />
        </button>
      )}
    </div>
  )
}
