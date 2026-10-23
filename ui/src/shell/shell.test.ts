// The shell's places and its render: sidebar order per the canvas, Books
// only in SERVER mode, the current row, and the primitives' markup.
import { describe, expect, it } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import { isCurrent, primaryNav, secondaryNav, tabNav, type ShellCounts } from './nav'
import { loadSidebarOpen, loadWorkspaceSel, storeSidebarOpen, SIDEBAR_KEY } from './prefs'
import Sidebar from './Sidebar'
import TopBar from './TopBar'
import { Avatar, Button, ConfirmButton, CountPill, EmptyState, VerdictChip, toVerdict } from '../lapis'
import { workspaceColor, workspaceSummary, type Workspace } from '../workspaces'

const counts: ShellCounts = { review: 5, inbox: 2, todos: 3, trash: 1, docs: 40 }
const D = '0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d'
const noop = () => {}

describe('shell nav', () => {
  it('primary places in canvas order; Inbox/To-dos only once their docs exist; Books in SERVER mode', () => {
    expect(primaryNav({ server: false, counts, inboxDoc: null, todoDoc: null }).map((i) => i.id)).toEqual(['home', 'review', 'search'])
    expect(primaryNav({ server: true, counts, inboxDoc: D, todoDoc: D }).map((i) => i.id)).toEqual(['home', 'inbox', 'todos', 'review', 'search', 'books'])
    expect(secondaryNav(counts).map((i) => i.id)).toEqual(['runs', 'trash', 'settings'])
    expect(tabNav({ server: true, counts }).map((i) => i.id)).toEqual(['home', 'library', 'review', 'books', 'search'])
    expect(tabNav({ server: false, counts }).map((i) => i.id)).not.toContain('books')
  })
  it('the current row follows the route; doc-backed rows light for their doc', () => {
    const [home, inbox] = primaryNav({ server: false, counts, inboxDoc: D, todoDoc: null })
    expect(isCurrent(home, { kind: 'home' })).toBe(true)
    expect(isCurrent(home, { kind: 'review' })).toBe(false)
    expect(isCurrent(inbox, { kind: 'doc', id: D })).toBe(true)
    expect(isCurrent(inbox, { kind: 'doc', id: '11111111-2222-4333-8444-555555555555' })).toBe(false)
  })
  it('sidebar open by default and remembered', () => {
    const m = new Map<string, string>()
    const s = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) }
    expect(loadSidebarOpen(s)).toBe(true)
    storeSidebarOpen(false, s)
    expect(m.get(SIDEBAR_KEY)).toBe('closed')
    expect(loadSidebarOpen(s)).toBe(false)
    expect(loadWorkspaceSel(s)).toBe('all')
  })
})

describe('shell render', () => {
  it('sidebar: brand, places with the review badge, tree slot, foot', () => {
    const html = renderToString(
      createElement(Sidebar, {
        route: { kind: 'review' },
        primary: primaryNav({ server: true, counts, inboxDoc: D, todoDoc: null }),
        secondary: secondaryNav(counts),
        workspaceMenu: createElement('div', { id: 'wsm' }),
        tree: createElement('div', { id: 'tree' }),
        userName: 'Tom',
        host: 'taisce.null.ie',
        onHide: noop,
      }),
    )
    for (const t of ['Taisce', 'Home', 'Inbox', 'Review', 'Search &amp; Ask', 'Books', 'Gardeners', 'Trash', 'Settings', 'Tom', 'taisce.null.ie']) {
      expect(html).toContain(t)
    }
    expect(html).toContain('lp-count-yellow')
    expect(html).toMatch(/class="lp-nav-row on"[^>]*href="\/review"|href="\/review"[^>]*class="lp-nav-row on"/)
    expect(html.indexOf('id="wsm"')).toBeLessThan(html.indexOf('Home'))
    expect(html.indexOf('id="tree"')).toBeLessThan(html.indexOf('Gardeners'))
  })
  it('top bar: open and actions hints, capture, new doc, theme', () => {
    const html = renderToString(
      createElement(TopBar, {
        crumb: 'Home',
        sidebarHidden: false,
        onShowSidebar: noop,
        onOpen: noop,
        onActions: noop,
        onCapture: noop,
        onNewDoc: noop,
        onBack: noop,
        onForward: noop,
      }),
    )
    expect(html).toContain('Open a doc or place…')
    expect(html).toContain('⌘O')
    expect(html).toContain('⌘K')
    expect(html).toContain('New doc')
    expect(html).toContain('Theme:')
  })
})

describe('primitives', () => {
  it('verdicts carry an icon and a word, from either vocabulary', () => {
    expect(toVerdict('green')).toBe('applied')
    expect(toVerdict('yellow')).toBe('flagged')
    expect(toVerdict('red')).toBe('parked')
    const html = renderToString(createElement(VerdictChip, { verdict: 'red' }))
    expect(html).toContain('Parked · not applied')
    expect(html).toContain('<svg')
    expect(renderToString(createElement(VerdictChip, { verdict: 'flagged', short: true }))).toContain('>Flagged<')
  })
  it('a person is round, an agent a violet square with a mono name', () => {
    expect(renderToString(createElement(Avatar, { name: 'Tom' }))).toContain('lp-avatar-person')
    const agent = renderToString(createElement(Avatar, { name: 'claude:docs', showName: true }))
    expect(agent).toContain('lp-avatar-agent')
    expect(agent).toContain('lp-agent-name')
  })
  it('buttons, counts, empty states and the two-step confirm render', () => {
    expect(renderToString(createElement(Button, { variant: 'primary', kbd: '⌘N' }, 'New doc'))).toContain('lp-btn-primary')
    expect(renderToString(createElement(CountPill, { n: 0 }))).toBe('')
    expect(renderToString(createElement(CountPill, { n: 1200 }))).toContain('999+')
    expect(renderToString(createElement(EmptyState, { title: 'Nothing here' }))).toContain('Nothing here')
    const c = renderToString(createElement(ConfirmButton, { onConfirm: noop, children: 'Delete…' }))
    expect(c).toContain('Delete…')
    expect(c).not.toContain('Cancel')
  })
  it('workspace colours and the switcher summary', () => {
    const ws: Workspace[] = [
      { id: 'w1', name: 'Work', color: null, icon: null, sort_key: null, doc_ids: ['a', 'b'], role: 'owner' },
      { id: 'w2', name: 'Family', color: '#D9692A', icon: null, sort_key: null, doc_ids: [], doc_count: 12, role: 'editor', shared: true },
    ]
    expect(workspaceColor(ws[0], 0)).toBe('var(--ws-1)')
    expect(workspaceColor(ws[1], 1)).toBe('#D9692A')
    expect(workspaceSummary('all', ws, 3)).toEqual({ name: 'All workspaces', sub: 'Unsorted, Work, Family' })
    expect(workspaceSummary('w1', ws, 3)).toEqual({ name: 'Work', sub: 'Owner · 2 docs' })
    expect(workspaceSummary('w2', ws, 3).sub).toBe('Editor · shared · 12 docs')
  })
})
