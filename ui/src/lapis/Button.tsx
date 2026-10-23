import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from 'react'
import { Icon, type IconName } from './icons'
import { Kbd } from './Chip'

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger' | 'ok'
export type ButtonSize = 'sm' | 'md'

/** The class list a button-looking element wears (also for <Link>s styled as
 * buttons: `<Link className={buttonClass('primary', 'sm')}>`). */
export function buttonClass(variant: ButtonVariant = 'secondary', size: ButtonSize = 'md', extra?: string): string {
  return ['lp-btn', `lp-btn-${variant}`, size === 'sm' ? 'lp-btn-sm' : '', extra ?? ''].filter(Boolean).join(' ')
}

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant
  size?: ButtonSize
  /** a leading icon */
  icon?: IconName
  /** a trailing key hint, e.g. "⌘⌥N" (hidden on phones) */
  kbd?: string
  children?: ReactNode
}

/** primary = the one main action (lapis fill) · secondary = bordered card ·
 * ghost = text only · danger = red text, for Delete… (pair with Confirm) ·
 * ok = green soft fill, for Keep / Accept. */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = 'secondary', size = 'md', icon, kbd, className, children, type = 'button', ...rest },
  ref,
) {
  return (
    <button ref={ref} type={type} className={buttonClass(variant, size, className)} {...rest}>
      {icon && <Icon name={icon} size={size === 'sm' ? 15 : 16} />}
      {children}
      {kbd && <Kbd className="lp-btn-kbd">{kbd}</Kbd>}
    </button>
  )
})

export interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'children'> {
  icon: IconName
  /** required: the accessible name (and the tooltip) */
  label: string
  size?: 'sm' | 'md'
  /** pressed/current state (e.g. the sidebar toggle) */
  active?: boolean
}

/** A square, borderless icon-only button: 34px (md) or 24px (sm). */
export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { icon, label, size = 'md', active, className, type = 'button', title, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      title={title ?? label}
      className={['lp-ib', size === 'sm' ? 'lp-ib-sm' : '', active ? 'on' : '', className ?? ''].filter(Boolean).join(' ')}
      {...rest}
    >
      <Icon name={icon} size={size === 'sm' ? 15 : 18} />
    </button>
  )
})
