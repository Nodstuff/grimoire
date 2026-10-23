// The shell's places, as data: what the sidebar lists (in canvas order),
// what the phone's tab bar shows, and which row is current for a route.

import type { IconName } from '../lapis'
import type { Route, RouteKind } from '../router'

export interface ShellCounts {
  review: number
  inbox: number | null
  todos: number | null
  trash: number | null
  docs: number
}

export interface NavItem {
  id: string
  label: string
  icon: IconName
  /** where it goes: a route, or a doc (Inbox, To-dos are docs) */
  to: Route | null
  /** route kinds that light this row */
  match: RouteKind[]
  kbd?: string
  count?: number | null
  /** yellow attention badge instead of a quiet count */
  badge?: boolean
}

/** Sidebar, top group (canvas Main): Home, Inbox, To-dos, Review, Search &
 * Ask, Books. Inbox and To-dos open their docs (absent until they exist);
 * Books only in SERVER mode. */
export function primaryNav({
  server,
  counts,
  inboxDoc,
  todoDoc,
}: {
  server: boolean
  counts: ShellCounts
  inboxDoc: string | null
  todoDoc: string | null
}): NavItem[] {
  const items: NavItem[] = [{ id: 'home', label: 'Home', icon: 'home', to: { kind: 'home' }, match: ['home'], kbd: '⌘W' }]
  if (inboxDoc) items.push({ id: 'inbox', label: 'Inbox', icon: 'inbox', to: { kind: 'doc', id: inboxDoc }, match: [], count: counts.inbox })
  if (todoDoc) items.push({ id: 'todos', label: 'To-dos', icon: 'todo', to: { kind: 'doc', id: todoDoc }, match: [], count: counts.todos })
  items.push(
    { id: 'review', label: 'Review', icon: 'review', to: { kind: 'review' }, match: ['review'], count: counts.review, badge: true },
    { id: 'search', label: 'Search & Ask', icon: 'search', to: { kind: 'search', q: '' }, match: ['search'], kbd: '⌘⇧F' },
  )
  if (server) items.push({ id: 'books', label: 'Books', icon: 'books', to: { kind: 'books' }, match: ['books'] })
  return items
}

/** Sidebar, bottom group: Gardeners, Trash, Settings. */
export function secondaryNav(counts: ShellCounts): NavItem[] {
  return [
    { id: 'runs', label: 'Gardeners', icon: 'gardeners', to: { kind: 'runs' }, match: ['runs'] },
    { id: 'trash', label: 'Trash', icon: 'trash', to: { kind: 'trash' }, match: ['trash'], count: counts.trash },
    { id: 'settings', label: 'Settings', icon: 'settings', to: { kind: 'settings', section: 'profile' }, match: ['settings'], kbd: '⌘,' },
  ]
}

/** The phone's floating tab bar (Mobile-Home): Home, Library, Review,
 * Books (SERVER), Search. */
export function tabNav({ server, counts }: { server: boolean; counts: ShellCounts }): NavItem[] {
  const items: NavItem[] = [
    { id: 'home', label: 'Home', icon: 'home', to: { kind: 'home' }, match: ['home'] },
    { id: 'library', label: 'Library', icon: 'folder', to: { kind: 'library', workspace: null }, match: ['library', 'doc'] },
    { id: 'review', label: 'Review', icon: 'review', to: { kind: 'review' }, match: ['review'], count: counts.review, badge: true },
  ]
  if (server) items.push({ id: 'books', label: 'Books', icon: 'books', to: { kind: 'books' }, match: ['books'] })
  items.push({ id: 'search', label: 'Search', icon: 'search', to: { kind: 'search', q: '' }, match: ['search'] })
  return items
}

/** Is `item` the current place? Doc-backed rows (Inbox, To-dos) light when
 * their doc is open. */
export function isCurrent(item: NavItem, route: Route): boolean {
  if (item.to?.kind === 'doc') return route.kind === 'doc' && route.id === item.to.id
  return item.match.includes(route.kind)
}

/** The top bar's crumb for a route: the place's name (a doc shows its path). */
export function placeTitle(route: Route): string {
  switch (route.kind) {
    case 'home':
      return 'Home'
    case 'library':
      return 'Library'
    case 'review':
      return 'Review'
    case 'search':
      return 'Search & Ask'
    case 'books':
      return 'Books'
    case 'runs':
      return 'Gardeners'
    case 'graph':
      return 'Graph'
    case 'trash':
      return 'Trash'
    case 'settings':
      return 'Settings'
    case 'doc':
      return 'Doc'
    case 'notfound':
      return 'Not found'
  }
}
