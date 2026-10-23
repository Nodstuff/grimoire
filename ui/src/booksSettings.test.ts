// The Lapis Books and Settings screens: their pure helpers and the pieces
// that render without effects.
import { describe, expect, it } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import type { Book, BookProgress, UploadResult } from './books'
import { Cover, bookPatch, booksLead, coverBadge, pctOf, progressWhere, shelfFilter } from './BookLibrary'
import { resultLine } from './BookUpload'
import { COVER_ART_COUNT, coverArt } from './bookCovers'
import { expiresLabel, threadComments, type ShareComment } from './shareLinks'
import { workspaceLine } from './screens/SettingsWorkspaces'
import { SecretCard, Segmented } from './screens/settingsParts'
import { gardenerRows } from './Settings'
import type { Workspace } from './workspaces'
import type { GardenerRun } from './types'

const book = (id: string, extra: Partial<Book> = {}): Book => ({
  id,
  title: `Book ${id}`,
  author: 'Ursula K. Le Guin',
  series: null,
  series_index: null,
  book_identifier: id,
  original_filename: `${id}.epub`,
  size: 1000,
  has_cover: false,
  added_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  ...extra,
})
const prog = (status: BookProgress['status'], percentage = 0): BookProgress => ({ status, percentage, device: 'Kindle', updated_at: '2026-01-01T00:00:00Z' })

describe('books: shelves, badges, progress', () => {
  const books = [book('a'), book('b'), book('c')]
  const progress = { a: prog('reading', 40), b: prog('finished', 100), c: null }
  it('All / Unread / Finished over the loaded books', () => {
    expect(shelfFilter(books, progress, 'all').map((b) => b.id)).toEqual(['a', 'b', 'c'])
    expect(shelfFilter(books, progress, 'finished').map((b) => b.id)).toEqual(['b'])
    // no progress row is unread; reading is neither
    expect(shelfFilter(books, progress, 'unread').map((b) => b.id)).toEqual(['c'])
  })
  it('the corner badge: Finished, New within a day, else none', () => {
    const now = Date.parse('2026-01-01T12:00:00Z')
    expect(coverBadge(book('a'), prog('finished'), now)).toBe('Finished')
    expect(coverBadge(book('a'), null, now)).toBe('New')
    expect(coverBadge(book('a'), null, now + 2 * 86_400_000)).toBeNull()
  })
  it('the bar percentage clamps and finished is full', () => {
    expect(pctOf(prog('reading', 41.6))).toBe(42)
    expect(pctOf(prog('reading', 140))).toBe(100)
    expect(pctOf(prog('finished', 3))).toBe(100)
    expect(pctOf(null)).toBe(0)
    expect(progressWhere(null)).toBe('not started')
    expect(progressWhere(prog('reading', 3))).toMatch(/^Kindle · /)
  })
  it('the lead line', () => {
    expect(booksLead({ books: 60, more: true, reading: 3, finished: 54, ereaders: 2 })).toBe('60+ books · 3 reading · 54 finished · synced to 2 e-readers')
    expect(booksLead({ books: 1, more: false, reading: 0, finished: 0, ereaders: 0 })).toBe('1 book · 0 reading · 0 finished · no e-readers yet')
    expect(booksLead({ books: 2, more: false, reading: 0, finished: 0, ereaders: null })).toBe('2 books · 0 reading · 0 finished')
  })
})

describe('books: the details form', () => {
  const b = book('a', { series: 'Hainish Cycle', series_index: 4 })
  const same = { title: b.title, author: b.author!, series: 'Hainish Cycle', number: '4' }
  it('sends only what changed', () => {
    expect(bookPatch(b, same)).toEqual({})
    expect(bookPatch(b, { ...same, title: '  New title ' })).toEqual({ title: 'New title' })
    expect(bookPatch(b, { ...same, author: '' })).toEqual({ author: '' })
    expect(bookPatch(b, { ...same, series: '', number: '' })).toEqual({ series: null, series_index: null })
    expect(bookPatch(b, { ...same, number: '4.5' })).toEqual({ series_index: 4.5 })
  })
  it('an empty title is not sent (a book keeps a title)', () => {
    expect(bookPatch(b, { ...same, title: '  ' })).toEqual({})
  })
  it('refuses a series number that is not a number', () => {
    expect(() => bookPatch(b, { ...same, number: 'four' })).toThrow(/number/)
  })
})

describe('books: covers', () => {
  it('the typographic cover is stable per id and spread over the palette', () => {
    expect(coverArt('abc')).toBe(coverArt('abc'))
    const seen = new Set(Array.from({ length: 200 }, (_, i) => coverArt(`id-${i}`).bg))
    expect(seen.size).toBe(COVER_ART_COUNT)
  })
  it('a book with a cover shows the cover route; one without shows its title and author', () => {
    const withImg = renderToString(createElement(Cover, { book: book('a', { has_cover: true }), size: 'grid' }))
    expect(withImg).toContain('src="/api/books/a/cover"')
    const typo = renderToString(createElement(Cover, { book: book('b'), size: 'detail', badge: 'Finished' }))
    expect(typo).toContain('Book b')
    expect(typo).toContain('Ursula K. Le Guin')
    expect(typo).toContain('Finished')
    expect(typo).not.toContain('<img')
  })
})

describe('books: upload results', () => {
  const r = (status: UploadResult['status'], message?: string): UploadResult => ({ filename: 'x.epub', status, message })
  it('each outcome has a tone and a word', () => {
    expect(resultLine(r('added'))).toEqual({ tone: 'green', text: 'added' })
    expect(resultLine(r('updated'))).toEqual({ tone: 'green', text: 'updated · new file' })
    expect(resultLine(r('already_here'))).toEqual({ tone: 'dim', text: 'already here' })
    expect(resultLine(r('clash', 'same title as The Hobbit'))).toEqual({ tone: 'yellow', text: 'clash · same title as The Hobbit' })
    expect(resultLine(r('error', 'not a valid EPUB'))).toEqual({ tone: 'red', text: 'failed · not a valid EPUB' })
  })
})

describe('settings: share links', () => {
  const now = Date.parse('2026-10-20T12:00:00Z')
  it('expiry labels', () => {
    expect(expiresLabel(null, now)).toBe('never')
    expect(expiresLabel('2026-10-24T12:00:00Z', now)).toBe('in 4 days')
    expect(expiresLabel('2026-10-20T15:00:00Z', now)).toBe('in 3 hours')
    expect(expiresLabel('2026-10-19T12:00:00Z', now)).toBe('expired')
  })
  it('comments thread under their root, oldest first', () => {
    const c = (id: string, at: string, parent_id: string | null = null): ShareComment => ({ id, parent_id, author: 'Niamh', is_owner: !!parent_id, body: id, created_at: at, revision: 1 })
    const t = threadComments([c('r1', '2026-10-20T10:00:00Z', 'a'), c('b', '2026-10-20T09:00:00Z'), c('a', '2026-10-20T08:00:00Z')])
    expect(t.map((x) => [x.comment.id, x.replies.map((r) => r.id)])).toEqual([
      ['a', ['r1']],
      ['b', []],
    ])
  })
})

describe('settings: workspaces', () => {
  const ws = (extra: Partial<Workspace> = {}): Workspace => ({ id: 'w', name: 'Work', color: null, icon: null, sort_key: null, doc_ids: [], doc_count: 38, ...extra })
  it('owner lines say who else sees it', () => {
    expect(workspaceLine(ws(), [], true)).toBe('Owner · 38 docs · only you')
    const m = (role: 'owner' | 'editor' | 'viewer') => ({ user_id: role, name: role, role, added_at: '' })
    expect(workspaceLine(ws({ role: 'owner' }), [m('owner'), m('editor'), m('viewer')], true)).toBe('Owner · 38 docs · shared with 2 people')
    expect(workspaceLine(ws({ shared: true }), undefined, true)).toBe('Owner · 38 docs · shared')
    // LOCAL is single-user: no sharing line
    expect(workspaceLine(ws(), undefined, false)).toBe('Owner · 38 docs')
  })
  it('a workspace shared with you names its owner', () => {
    expect(workspaceLine(ws({ role: 'editor', owner_name: 'Aoife', doc_count: 1 }), undefined, true)).toBe('Editor · 1 doc · shared by Aoife')
  })
})

describe('settings: gardeners summary', () => {
  const run = (name: string, at: string, status = 'ok'): GardenerRun => ({ id: at, gardener: name, gardener_name: name, started_at: at, status, summary: `${name} did things`, tokens_used: null, tool_calls: null })
  it('from the registry with each one’s latest run', () => {
    const rows = gardenerRows(
      [{ id: '1', name: 'tagging', kind: 'tagging', principal: 'p', scope_doc: null, task_prompt: '', bindings: [], schedule: 'daily 07:00', confidence_policy: 'review', enabled: true }],
      [run('tagging', '2026-10-01T07:00:00Z', 'failed'), run('tagging', '2026-10-02T07:00:00Z')],
    )
    expect(rows).toEqual([{ name: 'tagging', what: 'Adds tags to new docs · daily 07:00', last: expect.objectContaining({ started_at: '2026-10-02T07:00:00Z' }), enabled: true }])
  })
  it('without the registry (SERVER mode), from the runs alone', () => {
    const rows = gardenerRows(null, [run('scribe', '2026-10-02T07:00:00Z'), run('filer', '2026-10-01T07:00:00Z')])
    expect(rows.map((r) => [r.name, r.what])).toEqual([
      ['filer', 'filer did things'],
      ['scribe', 'scribe did things'],
    ])
  })
})

describe('settings: parts', () => {
  it('the show-once card: warning, every row with a Copy, and Done', () => {
    const html = renderToString(
      createElement(SecretCard, { label: 'Set up kobo', title: 'Set up kobo', warning: 'You won’t see this password again', onDone: () => {}, rows: [{ name: 'Password', value: 'pw' }, { name: 'Username', value: null }] }),
    )
    expect(html).toContain('You won’t see this password again')
    expect(html).toContain('aria-label="Copy Password"')
    expect(html).not.toContain('aria-label="Copy Username"')
    expect(html).toContain('Done, I’ve saved it')
  })
  it('the segmented choice is a radiogroup with one checked', () => {
    const html = renderToString(createElement(Segmented, { label: 'Theme', value: 'dark', options: [{ id: 'system', label: 'System' }, { id: 'light', label: 'Light' }, { id: 'dark', label: 'Dark' }], onChange: () => {} }))
    expect(html).toContain('role="radiogroup"')
    expect(html.match(/aria-checked="true"/g)?.length).toBe(1)
    expect(html).toMatch(/aria-checked="true" tabindex="0" class="on">Dark/)
  })
})
