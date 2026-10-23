import type { HTMLAttributes, ReactNode } from 'react'
import { Icon, type IconName } from './icons'

/** A keyboard hint: <Kbd>⌘K</Kbd>. */
export function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return <kbd className={`lp-kbd${className ? ` ${className}` : ''}`}>{children}</kbd>
}

export type ChipTone = 'neutral' | 'accent' | 'green' | 'yellow' | 'red' | 'agent'

export interface ChipProps extends HTMLAttributes<HTMLElement> {
  tone?: ChipTone
  icon?: IconName
  /** a coloured dot before the label (a workspace colour) */
  dot?: string
  /** square-ish tag (6px radius) instead of a pill */
  tag?: boolean
  /** renders a <button> (a filter chip) */
  onClick?: () => void
  pressed?: boolean
  children: ReactNode
}

/** A pill (or `tag`) for a status, a workspace, a deadline, a filter. Tones
 * are soft fills with the matching ink; neutral is a hairline outline. */
export function Chip({ tone = 'neutral', icon, dot, tag, onClick, pressed, className, children, ...rest }: ChipProps) {
  const cls = ['lp-chip', `lp-chip-${tone}`, tag ? 'lp-tag' : '', onClick ? 'lp-chip-btn' : '', className ?? '']
    .filter(Boolean)
    .join(' ')
  const inner = (
    <>
      {dot && <span className="lp-dot" style={{ background: dot }} aria-hidden />}
      {icon && <Icon name={icon} size={13} />}
      {children}
    </>
  )
  if (onClick)
    return (
      <button type="button" className={cls} onClick={onClick} aria-pressed={pressed} {...(rest as HTMLAttributes<HTMLButtonElement>)}>
        {inner}
      </button>
    )
  return (
    <span className={cls} {...rest}>
      {inner}
    </span>
  )
}

/** A small count: `<CountPill n={5} />`. `tone="yellow"` is the attention
 * badge (Review's pending count). Renders nothing for 0 unless `zero`. */
export function CountPill({
  n,
  tone = 'neutral',
  zero = false,
  label,
  className,
}: {
  n: number
  tone?: 'neutral' | 'yellow' | 'plain'
  zero?: boolean
  /** read aloud instead of the bare number, e.g. "5 to review" */
  label?: string
  className?: string
}) {
  if (n === 0 && !zero) return null
  return (
    <span className={`lp-count lp-count-${tone}${className ? ` ${className}` : ''}`} aria-label={label}>
      {n > 999 ? '999+' : n}
    </span>
  )
}

/** The gate's three verdicts. Server colours (green/yellow/red) and words
 * (applied/flagged/parked) both map here. */
export type Verdict = 'applied' | 'flagged' | 'parked'

export function toVerdict(v: string | null | undefined): Verdict {
  if (v === 'red' || v === 'parked') return 'parked'
  if (v === 'yellow' || v === 'flagged') return 'flagged'
  return 'applied'
}

const VERDICT: Record<Verdict, { icon: IconName; word: string; long: string; tone: 'green' | 'yellow' | 'red' }> = {
  applied: { icon: 'applied', word: 'Applied', long: 'Applied', tone: 'green' },
  flagged: { icon: 'flagged', word: 'Flagged', long: 'Flagged · applied', tone: 'yellow' },
  parked: { icon: 'parked', word: 'Parked', long: 'Parked · not applied', tone: 'red' },
}

/** Colour + shape + word, never colour alone. `short` drops the
 * "· applied / · not applied" tail where space is tight. */
export function VerdictChip({ verdict, short = false, className }: { verdict: Verdict | string; short?: boolean; className?: string }) {
  const v = VERDICT[toVerdict(verdict)]
  return (
    <span className={`lp-verdict lp-verdict-${v.tone}${className ? ` ${className}` : ''}`}>
      <Icon name={v.icon} size={13} strokeWidth={2} />
      {short ? v.word : v.long}
    </span>
  )
}

/** Save state of an editor: a dot and a word (Lapis-System "Save state"). */
export type SaveState = 'saved' | 'saving' | 'unsaved' | 'failed' | 'waiting' | 'readonly'

const SAVE: Record<SaveState, { cls: string; word: string }> = {
  saved: { cls: 'saved', word: 'Saved' },
  saving: { cls: 'saving', word: 'Saving…' },
  unsaved: { cls: 'unsaved', word: 'Unsaved' },
  failed: { cls: 'failed', word: 'Not saved' },
  waiting: { cls: 'waiting', word: 'Your edit is waiting for review' },
  readonly: { cls: 'readonly', word: 'Read only' },
}

export function StatusDot({ state, children }: { state: SaveState; children?: ReactNode }) {
  const s = SAVE[state]
  return (
    <span className={`lp-status lp-status-${s.cls}`}>
      <i aria-hidden />
      {children ?? s.word}
    </span>
  )
}
