// Real URLs. The address bar says where you are, back/forward walk it, a
// reload or a pasted link lands on the same view. The daemon answers every
// unknown non-API GET with index.html (main.rs serve_embedded_ui), so these
// paths deep-link in a browser tab and in the Mac app alike.
//
//   /                       Home
//   /doc/<uuid>[?block=<uuid>]   a doc (scrolled to a block)
//   /library[/<workspace>][?folder=<uuid>]  the Library (all, `unsorted`, or a
//                           workspace id; a folder's page)
//   /review                 the review queue
//   /search[?q=…]           Search & Ask
//   /books                  the EPUB library (SERVER mode)
//   /gardeners              gardener runs
//   /graph                  the link graph
//   /trash                  the Trash
//   /settings[/<section>]   Settings
//
// The old query deep links (`?doc=`, `?block=`, `?tab=`, deeplink.ts) are
// rewritten to these paths on boot (legacyRedirect), with any other params
// (e.g. `capture=1`) kept.

import { useEffect, useState } from 'react'
import { parseDeepLink, tabTarget, type SettingsSection } from './deeplink'

export type Route =
  | { kind: 'home' }
  | { kind: 'doc'; id: string; block?: string }
  | { kind: 'library'; workspace: string | null; folder?: string }
  | { kind: 'review' }
  | { kind: 'search'; q: string }
  | { kind: 'books' }
  | { kind: 'runs' }
  | { kind: 'graph' }
  | { kind: 'trash' }
  | { kind: 'settings'; section: SettingsSection }
  | { kind: 'notfound'; path: string }

export type RouteKind = Route['kind']

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i
export const SETTINGS_SECTION_IDS: SettingsSection[] = ['profile', 'ereaders', 'uploads']

function isSection(s: string): s is SettingsSection {
  return (SETTINGS_SECTION_IDS as string[]).includes(s)
}

/** Path (+ search) → route. Trailing slashes and case of fixed segments are
 * forgiven; ids are not (a bad id is a not-found, not a guess). */
export function parseRoute(pathname: string, search = ''): Route {
  const parts = pathname
    .split('/')
    .filter(Boolean)
    .map((p) => decodeURIComponent(p))
  const params = new URLSearchParams(search)
  const head = (parts[0] ?? '').toLowerCase()
  if (parts.length === 0) return { kind: 'home' }
  switch (head) {
    case 'doc':
      if (parts.length === 2 && UUID.test(parts[1])) {
        const block = params.get('block')
        return block && UUID.test(block) ? { kind: 'doc', id: parts[1], block } : { kind: 'doc', id: parts[1] }
      }
      break
    case 'library': {
      // ?folder=<uuid>: that folder's page (canvas Library)
      const folder = params.get('folder')
      const f = folder && UUID.test(folder) ? { folder } : {}
      if (parts.length === 1) return { kind: 'library', workspace: null, ...f }
      if (parts.length === 2) return { kind: 'library', workspace: parts[1], ...f }
      break
    }
    case 'review':
      if (parts.length === 1) return { kind: 'review' }
      break
    case 'search':
      if (parts.length === 1) return { kind: 'search', q: params.get('q') ?? '' }
      break
    case 'books':
      if (parts.length === 1) return { kind: 'books' }
      break
    case 'gardeners':
      if (parts.length === 1) return { kind: 'runs' }
      break
    case 'graph':
      if (parts.length === 1) return { kind: 'graph' }
      break
    case 'trash':
      if (parts.length === 1) return { kind: 'trash' }
      break
    case 'settings':
      if (parts.length === 1) return { kind: 'settings', section: 'profile' }
      if (parts.length === 2 && isSection(parts[1])) return { kind: 'settings', section: parts[1] }
      break
  }
  return { kind: 'notfound', path: pathname }
}

/** Route → path (+ search). The inverse of parseRoute. */
export function routePath(r: Route): string {
  switch (r.kind) {
    case 'home':
      return '/'
    case 'doc':
      return `/doc/${r.id}${r.block ? `?block=${r.block}` : ''}`
    case 'library':
      return (
        (r.workspace && r.workspace !== 'all' ? `/library/${encodeURIComponent(r.workspace)}` : '/library') +
        (r.folder ? `?folder=${r.folder}` : '')
      )
    case 'review':
      return '/review'
    case 'search':
      return r.q ? `/search?q=${encodeURIComponent(r.q)}` : '/search'
    case 'books':
      return '/books'
    case 'runs':
      return '/gardeners'
    case 'graph':
      return '/graph'
    case 'trash':
      return '/trash'
    case 'settings':
      return r.section === 'profile' ? '/settings' : `/settings/${r.section}`
    case 'notfound':
      return r.path
  }
}

/** Two routes are the same place (for "is this nav row current"). */
export function sameRoute(a: Route, b: Route): boolean {
  return routePath(a) === routePath(b)
}

/** The old `?doc=…&block=…&tab=…` links → the new path (other params kept),
 * or null when the URL carries none. */
export function legacyRedirect(pathname: string, search: string): string | null {
  const link = parseDeepLink(search)
  if (!link) return null
  const p = new URLSearchParams(search)
  for (const k of ['doc', 'block', 'tab']) p.delete(k)
  let target: Route
  if (link.doc) target = link.block ? { kind: 'doc', id: link.doc, block: link.block } : { kind: 'doc', id: link.doc }
  else if (link.tab) {
    const t = tabTarget(link.tab)
    target = t.kind === 'settings' ? { kind: 'settings', section: t.section } : { kind: t.kind }
  } else return null
  // only rewrite from the root (a link like /review?tab=books is not legacy)
  if (pathname !== '/' && pathname !== '' && pathname !== '/index.html') return null
  const path = routePath(target)
  const rest = p.toString()
  if (!rest) return path
  return path + (path.includes('?') ? '&' : '?') + rest
}

/* ---------- navigation ---------- */

/** What rides along with a navigation but is not in the URL: a heading
 * anchor to scroll to, "open the review rail". Lives in history.state, so
 * back to the entry restores it; a reload drops it (by design). */
export interface NavState {
  anchor?: string
  review?: boolean
  /** /search: arrived to ask the query (the palette's ⌘↵, ⌘/) */
  ask?: boolean
  /** /search: the answer doc this entry showed */
  answer?: string
}

const NAV_EVENT = 'taisce:navigate'

export function currentRoute(): Route {
  return parseRoute(location.pathname, location.search)
}

export function currentNavState(): NavState {
  const s = history.state as { nav?: NavState } | null
  return s?.nav ?? {}
}

/** Go somewhere. `replace` swaps the current entry (redirects, a search
 * query being typed). Navigating to where you already are is a no-op unless
 * state changes. */
export function navigate(to: Route | string, opts: { replace?: boolean; state?: NavState } = {}) {
  const path = typeof to === 'string' ? to : routePath(to)
  const here = location.pathname + location.search
  const state = { nav: opts.state ?? {} }
  if (path === here && !opts.state) return
  if (opts.replace || path === here) history.replaceState(state, '', path + (path === here ? location.hash : ''))
  else history.pushState(state, '', path)
  window.dispatchEvent(new Event(NAV_EVENT))
}

export const goBack = () => history.back()
export const goForward = () => history.forward()

/** The current route and its nav state, re-rendered on every navigate()
 * and every back/forward. */
export function useRoute(): { route: Route; state: NavState; key: number } {
  const read = () => ({ route: currentRoute(), state: currentNavState(), key: Date.now() })
  const [v, setV] = useState(read)
  useEffect(() => {
    const on = () => setV(read())
    window.addEventListener('popstate', on)
    window.addEventListener(NAV_EVENT, on)
    return () => {
      window.removeEventListener('popstate', on)
      window.removeEventListener(NAV_EVENT, on)
    }
  }, [])
  return v
}

/** Should a click on a link be left to the browser (new tab, download…)? */
export function isModifiedClick(e: { button: number; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean }): boolean {
  return e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey
}
