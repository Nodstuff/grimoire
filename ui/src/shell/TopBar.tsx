// The top bar (canvas Main/Library header): back / forward, the crumb, the
// "Open a doc or place… ⌘O" field in the middle, Actions ⌘K, Capture ⌘⇧I,
// New doc, and the theme toggle. On a phone: the menu button, the crumb,
// and icon buttons for Open and Actions.

import type { ReactNode } from 'react'
import { Button, Icon, IconButton, Kbd, THEME_LABEL, useTheme } from '../lapis'

export function ThemeToggle() {
  const { pref, theme, cycle } = useTheme()
  const icon = pref === 'system' ? 'system' : theme === 'dark' ? 'moon' : 'sun'
  const next = pref === 'system' ? 'Light' : pref === 'light' ? 'Dark' : 'System'
  return <IconButton icon={icon} label={`Theme: ${THEME_LABEL[pref]} (switch to ${next})`} onClick={cycle} />
}

export default function TopBar({
  crumb,
  sidebarHidden,
  onShowSidebar,
  onOpen,
  onActions,
  onCapture,
  onNewDoc,
  onBack,
  onForward,
}: {
  crumb: ReactNode
  /** desktop sidebar hidden, or phone (drawer): show the menu button */
  sidebarHidden: boolean
  onShowSidebar: () => void
  onOpen: () => void
  onActions: () => void
  onCapture: () => void
  onNewDoc: () => void
  onBack: () => void
  onForward: () => void
}) {
  return (
    <header className="lp-top" data-tauri-drag-region>
      {sidebarHidden && <IconButton icon="sidebar" label="Show sidebar (⌘T)" onClick={onShowSidebar} className="lp-top-side" />}
      <IconButton icon="menu" label="Menu" onClick={onShowSidebar} className="lp-top-menu" />
      <IconButton icon="back" label="Back (⌘[)" onClick={onBack} className="lp-top-hist lp-top-back" />
      <IconButton icon="forward" label="Forward (⌘])" onClick={onForward} className="lp-top-hist lp-top-fwd" />
      <div className="lp-crumb">{crumb}</div>
      <button type="button" className="lp-omni" onClick={onOpen} aria-label="Open a doc or place (⌘O)">
        <Icon name="search" size={15} />
        <span>Open a doc or place…</span>
        <Kbd>⌘O</Kbd>
      </button>
      <IconButton icon="search" label="Open a doc or place" onClick={onOpen} className="lp-top-phone lp-top-find" />
      {/* a screen's own top-bar actions (the doc's save state and Share)
          portal in here */}
      <div className="lp-top-slot" id="lp-top-slot" />
      <Button variant="ghost" size="sm" onClick={onActions} aria-label="Actions (⌘K)" className="lp-top-wide">
        Actions <Kbd>⌘K</Kbd>
      </Button>
      <IconButton icon="actions" label="Actions" onClick={onActions} className="lp-top-phone" />
      <Button size="sm" icon="capture" kbd="⌘⇧I" onClick={onCapture} className="lp-top-wide lp-top-capture">
        Capture
      </Button>
      <Button variant="primary" size="sm" icon="plus" onClick={onNewDoc} className="lp-top-wide lp-top-new">
        New doc
      </Button>
      <span className="lp-top-theme">
        <ThemeToggle />
      </span>
    </header>
  )
}
