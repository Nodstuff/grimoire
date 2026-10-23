// The phone's floating bottom tab bar (canvas Mobile-Home) and the capture
// button above it. Hidden from 761px up (shell.css).

import Link from '../Link'
import { Icon } from '../lapis'
import type { Route } from '../router'
import { isCurrent, type NavItem } from './nav'

export default function TabBar({ items, route, onCapture }: { items: NavItem[]; route: Route; onCapture: () => void }) {
  return (
    <>
      <button type="button" className="lp-fab" aria-label="Capture a thought" onClick={onCapture}>
        <Icon name="capture" size={22} />
      </button>
      <nav className="lp-tabbar" aria-label="Main">
        {items.map((it) => {
          if (!it.to) return null
          const on = isCurrent(it, route)
          return (
            <Link key={it.id} to={it.to} className={on ? 'on' : ''} aria-current={on ? 'page' : undefined}>
              <Icon name={it.icon} size={20} />
              {it.label}
              {it.badge && (it.count ?? 0) > 0 && (
                <span className="lp-tab-badge" aria-label={`${it.count} to review`}>
                  {it.count}
                </span>
              )}
            </Link>
          )
        })}
      </nav>
    </>
  )
}
