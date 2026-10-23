// Inline two-step confirm (window.confirm is a no-op in the Mac app). The
// first press arms: the button turns into "Confirm" + "Cancel" in place;
// the second press runs the action. It disarms by itself after `timeoutMs`,
// on Cancel, and on Esc.

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { Button, type ButtonSize } from './Button'
import type { IconName } from './icons'

export const ARM_MS = 4000

/** Arm state with an auto-disarm timer. `armed` is the key that is armed
 * (several rows can share one hook: only one is armed at a time). */
export function useArm<K = true>(timeoutMs = ARM_MS) {
  const [armed, setArmed] = useState<K | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const clear = () => {
    if (timer.current) clearTimeout(timer.current)
    timer.current = null
  }
  useEffect(() => clear, [])
  const disarm = useCallback(() => {
    clear()
    setArmed(null)
  }, [])
  const arm = useCallback(
    (key: K) => {
      clear()
      setArmed(() => key)
      timer.current = setTimeout(() => setArmed(null), timeoutMs)
    },
    [timeoutMs],
  )
  /** first call arms `key`; a second call with the same key returns true
   * (go ahead) and disarms */
  const press = useCallback(
    (key: K): boolean => {
      if (armed !== null && armed === key) {
        disarm()
        return true
      }
      arm(key)
      return false
    },
    [armed, arm, disarm],
  )
  return { armed, arm, disarm, press }
}

export function ConfirmButton({
  children,
  confirmLabel = 'Confirm',
  onConfirm,
  icon,
  size = 'sm',
  variant = 'danger',
  disabled,
  timeoutMs = ARM_MS,
  hint,
}: {
  /** the resting label, e.g. "Delete…" */
  children: ReactNode
  /** the armed label, e.g. "Delete for good" */
  confirmLabel?: ReactNode
  onConfirm: () => void
  icon?: IconName
  size?: ButtonSize
  /** resting style; armed is always the filled danger */
  variant?: 'danger' | 'secondary' | 'ghost'
  disabled?: boolean
  timeoutMs?: number
  /** a short line beside the armed buttons ("Moves 3 docs to Trash") */
  hint?: ReactNode
}) {
  const { armed, press, disarm } = useArm<true>(timeoutMs)
  if (!armed)
    return (
      <Button variant={variant} size={size} icon={icon} disabled={disabled} onClick={() => press(true)}>
        {children}
      </Button>
    )
  return (
    <span
      className="lp-confirm"
      role="group"
      aria-label="Confirm"
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.stopPropagation()
          disarm()
        }
      }}
    >
      <Button
        variant="primary"
        size={size}
        className="lp-btn-danger-fill"
        autoFocus
        onClick={() => {
          if (press(true)) onConfirm()
        }}
      >
        {confirmLabel}
      </Button>
      <Button variant="ghost" size={size} onClick={disarm}>
        Cancel
      </Button>
      {hint && <span className="lp-confirm-hint">{hint}</span>}
    </span>
  )
}
