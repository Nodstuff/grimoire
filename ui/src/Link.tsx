// <Link to={…}>: a real <a href> (so ⌘-click, copy link and screen readers
// all work) that navigates in-page on a plain click.

import type { AnchorHTMLAttributes, ReactNode } from 'react'
import { isModifiedClick, navigate, routePath, type NavState, type Route } from './router'

export default function Link({
  to,
  state,
  replace,
  onClick,
  children,
  ...rest
}: Omit<AnchorHTMLAttributes<HTMLAnchorElement>, 'href'> & {
  to: Route | string
  state?: NavState
  replace?: boolean
  children?: ReactNode
}) {
  const href = typeof to === 'string' ? to : routePath(to)
  return (
    <a
      href={href}
      onClick={(e) => {
        onClick?.(e)
        if (e.defaultPrevented || isModifiedClick(e) || rest.target) return
        e.preventDefault()
        navigate(href, { state, replace })
      }}
      {...rest}
    >
      {children}
    </a>
  )
}
