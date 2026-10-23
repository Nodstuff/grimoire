// Per-browser shell preferences (localStorage, every access in try/catch):
// the sidebar's open state and the workspace the tree shows.

import type { WorkspaceSel } from '../workspaces'

export const SIDEBAR_KEY = 'taisce.sidebar'
/** the key the old ⌘T tree used: kept so the stored choice survives */
export const WORKSPACE_KEY = 'taisce.tree.workspace'
/** phone width: the sidebar is a drawer and the tab bar shows */
export const PHONE_QUERY = '(max-width: 760px)'

type Get = Pick<Storage, 'getItem'> | null
type Set = Pick<Storage, 'setItem'> | null

function storage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage
  } catch {
    return null
  }
}

/** Open unless the person closed it (the default is open, as on the canvas). */
export function loadSidebarOpen(s: Get = storage()): boolean {
  try {
    return s?.getItem(SIDEBAR_KEY) !== 'closed'
  } catch {
    return true
  }
}

export function storeSidebarOpen(open: boolean, s: Set = storage()) {
  try {
    s?.setItem(SIDEBAR_KEY, open ? 'open' : 'closed')
  } catch {
    /* storage blocked: lasts this page */
  }
}

export function loadWorkspaceSel(s: Get = storage()): WorkspaceSel {
  try {
    return s?.getItem(WORKSPACE_KEY) || 'all'
  } catch {
    return 'all'
  }
}

export function storeWorkspaceSel(sel: WorkspaceSel, s: Set = storage()) {
  try {
    s?.setItem(WORKSPACE_KEY, sel)
  } catch {
    /* storage blocked: lasts this page */
  }
}
