// The persistent left sidebar (canvas Main / Library): the ◈ Taisce brand
// with the hide button (⌘T), the workspace switcher, the places (Home,
// Inbox, To-dos, Review, Search & Ask, Books), the doc tree (Pinned, then
// Library), and at the foot Gardeners, Trash, Settings and who you are.
// On a phone the same panel is a drawer (AppShell decides).

import type { ReactNode } from 'react'
import Link from '../Link'
import { Avatar, CountPill, Icon, IconButton, Kbd, LapisMark } from '../lapis'
import type { Route } from '../router'
import { isCurrent, type NavItem } from './nav'

function NavRow({ item, route, onNavigate }: { item: NavItem; route: Route; onNavigate?: () => void }) {
  if (!item.to) return null
  const on = isCurrent(item, route)
  return (
    <Link to={item.to} className={`lp-nav-row${on ? ' on' : ''}`} aria-current={on ? 'page' : undefined} onClick={onNavigate}>
      <Icon name={item.icon} />
      <span className="lp-nav-label">{item.label}</span>
      {item.badge ? (
        <CountPill n={item.count ?? 0} tone="yellow" label={`${item.count} to review`} className="lp-nav-end" />
      ) : item.count != null && item.count > 0 ? (
        <CountPill n={item.count} tone="plain" className="lp-nav-end lp-nav-n" />
      ) : item.kbd ? (
        <Kbd className="lp-nav-end lp-nav-kbd">{item.kbd}</Kbd>
      ) : null}
    </Link>
  )
}

export default function Sidebar({
  route,
  primary,
  secondary,
  workspaceMenu,
  tree,
  userName,
  host,
  onHide,
  onNavigate,
  drawer = false,
}: {
  route: Route
  primary: NavItem[]
  secondary: NavItem[]
  workspaceMenu: ReactNode
  tree: ReactNode
  userName: string | null
  host: string
  onHide: () => void
  /** a nav row was followed (the phone drawer closes) */
  onNavigate?: () => void
  drawer?: boolean
}) {
  return (
    <nav className={`lp-side${drawer ? ' lp-side-drawer' : ''}`} aria-label="Main">
      <div className="lp-brand">
        <LapisMark size={26} />
        <b>Taisce</b>
        <IconButton icon={drawer ? 'close' : 'sidebar'} label={drawer ? 'Close menu' : 'Hide sidebar (⌘T)'} onClick={onHide} className="lp-brand-hide" />
      </div>
      {workspaceMenu}
      <div className="lp-nav">
        {primary.map((it) => (
          <NavRow key={it.id} item={it} route={route} onNavigate={onNavigate} />
        ))}
      </div>
      <div className="lp-side-tree">{tree}</div>
      <div className="lp-nav lp-side-foot">
        {secondary.map((it) => (
          <NavRow key={it.id} item={it} route={route} onNavigate={onNavigate} />
        ))}
        <Link to={{ kind: 'settings', section: 'profile' }} className="lp-nav-row lp-me" onClick={onNavigate}>
          <Avatar name={userName || '?'} agent={false} size={22} />
          <span className="lp-me-text">
            <span className="lp-me-name">{userName || 'You'}</span>
            <span className="lp-me-host">{host}</span>
          </span>
        </Link>
      </div>
    </nav>
  )
}
