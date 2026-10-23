import { forwardRef, useId, type InputHTMLAttributes, type ReactNode, type TextareaHTMLAttributes } from 'react'
import { Icon, type IconName } from './icons'

export interface TextFieldProps extends InputHTMLAttributes<HTMLInputElement> {
  /** the visible label; with `hideLabel` it is read aloud only */
  label?: ReactNode
  hideLabel?: boolean
  /** a hint or error line under the field */
  help?: ReactNode
  error?: boolean
  /** a leading icon inside the field */
  icon?: IconName
  /** content after the input inside the field (a Kbd, a clear button) */
  trailing?: ReactNode
  /** 'inset' (card2 fill, the default) or 'card' (card fill, e.g. filters on the ground) */
  surface?: 'inset' | 'card'
}

/** A labelled text input, 40px tall, 10px radius. */
export const TextField = forwardRef<HTMLInputElement, TextFieldProps>(function TextField(
  { label, hideLabel, help, error, icon, trailing, surface = 'inset', id, className, ...rest },
  ref,
) {
  const auto = useId()
  const fid = id ?? auto
  return (
    <div className={`lp-fieldwrap${className ? ` ${className}` : ''}`}>
      {label && (
        <label htmlFor={fid} className={hideLabel ? 'sr-only' : 'lp-field-label'}>
          {label}
        </label>
      )}
      <div className={['lp-field', `lp-field-${surface}`, error ? 'lp-field-error' : ''].filter(Boolean).join(' ')}>
        {icon && <Icon name={icon} size={15} />}
        <input ref={ref} id={fid} aria-invalid={error || undefined} {...rest} />
        {trailing}
      </div>
      {help && <div className={`lp-field-help${error ? ' err' : ''}`}>{help}</div>}
    </div>
  )
})

export interface TextAreaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  label?: ReactNode
  hideLabel?: boolean
  help?: ReactNode
}

export const TextArea = forwardRef<HTMLTextAreaElement, TextAreaProps>(function TextArea(
  { label, hideLabel, help, id, className, ...rest },
  ref,
) {
  const auto = useId()
  const fid = id ?? auto
  return (
    <div className={`lp-fieldwrap${className ? ` ${className}` : ''}`}>
      {label && (
        <label htmlFor={fid} className={hideLabel ? 'sr-only' : 'lp-field-label'}>
          {label}
        </label>
      )}
      <textarea ref={ref} id={fid} className="lp-textarea" {...rest} />
      {help && <div className="lp-field-help">{help}</div>}
    </div>
  )
})
