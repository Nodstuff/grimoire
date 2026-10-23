import { describe, expect, it } from 'vitest'
import {
  buildOpenRows,
  buildPlaces,
  buildSearchRows,
  crumbOf,
  docPath,
  editDistance,
  parseSearch,
  pushRecentId,
  rankCommands,
  recentDocs,
  scoreDoc,
  scoreText,
  snippetOf,
  stableSelection,
  type OmniCommand,
  type OpenRow,
  type SearchRow,
} from './omni'
import { buildCommands } from './commands'
import type { Block, Doc, SearchHit } from './types'

const doc = (id: string, title: string, parent_id: string | null = null): Doc => ({
  id,
  title,
  parent_id,
  review_policy: null,
  current_epoch: 0,
  created_by: 'p',
  status: null,
  sort_key: null,
})

const hit = (id: string, docId: string, docTitle: string, content: string, type = 'paragraph'): SearchHit => ({
  doc_title: docTitle,
  block: {
    id,
    doc_id: docId,
    parent_id: null,
    order_key: 'i',
    block_type: type,
    content,
    created_by: 'p',
    epoch: 1,
    deleted: false,
    refers_to: null,
  } as Block,
})

const cmd = (id: string, label: string, extra: Partial<OmniCommand> = {}): OmniCommand => ({ id, label, run: () => {}, ...extra })

const docs = [
  doc('g', 'Grimoire'),
  doc('r', 'Roadmap', 'g'),
  doc('a', 'Architecture', 'g'),
  doc('d', 'Daily'),
  doc('t', '2026-09-10', 'd'),
  doc('q', 'qompass notes'),
  doc('bn', 'Book notes'),
  doc('k', 'Kindle setup', 'bn'),
]
const ids = (rows: OpenRow[]) => rows.map((r) => (r.kind === 'doc' ? r.doc.id : `@${r.place.id}`))

describe('scoreText', () => {
  it('ranks the tiers: exact > prefix > word-start substring > substring > words > subsequence > typo', () => {
    const s = (q: string, t: string) => scoreText(q, t) ?? -1
    expect(s('roadmap', 'Roadmap')).toBe(1000)
    expect(s('road', 'Roadmap')).toBeGreaterThan(s('map', 'Road map'))
    expect(s('map', 'Road map')).toBeGreaterThan(s('map', 'Roadmap'))
    expect(s('map', 'Roadmap')).toBeGreaterThan(s('road arch', 'Architecture of the roadmap'))
    expect(s('road arch', 'Architecture of the roadmap')).toBeGreaterThan(s('rdmp', 'Roadmap'))
    expect(s('rdmp', 'Roadmap')).toBeGreaterThan(s('raodmap', 'Roadmap'))
    expect(s('raodmap', 'Roadmap')).toBeGreaterThan(0)
  })
  it('word starts and runs beat scattered letters', () => {
    expect(scoreText('qn', 'qompass notes')!).toBeGreaterThan(scoreText('qn', 'quinine')!)
  })
  it('a shorter title wins inside a tier', () => {
    expect(scoreText('book', 'Books')!).toBeGreaterThan(scoreText('book', 'Book club notes')!)
  })
  it('tolerates one typo per word of four or more letters, not more', () => {
    expect(scoreText('boosk', 'Books')).not.toBeNull() // transposition
    expect(scoreText('archetecture', 'Architecture')).not.toBeNull() // substitution
    expect(scoreText('arhci', 'Architecture')).not.toBeNull() // swapped prefix
    expect(scoreText('bxxks', 'Books')).toBeNull()
    expect(scoreText('zzz', 'Roadmap')).toBeNull()
  })
  it('restricted Damerau distance', () => {
    expect(editDistance('books', 'boosk')).toBe(1)
    expect(editDistance('books', 'books')).toBe(0)
    expect(editDistance('abc', 'xyz', 1)).toBe(2) // bails out above max
  })
})

describe('Open (⌘O) ranking', () => {
  const places = buildPlaces({ library: true })
  const open = (q: string, recentIds: string[] = []) => buildOpenRows({ q, docs, places, recentIds })

  it('a title match beats the same match on the path', () => {
    // "grimoire" IS Grimoire's title, and only in Roadmap's and Architecture's path
    expect(ids(open('grimoire'))).toEqual(['g', 'r', 'a'])
    const byId = new Map(docs.map((d) => [d.id, d]))
    expect(scoreDoc('grimoire', 'Grimoire', 'Grimoire')!).toBeGreaterThan(scoreDoc('grimoire', 'Roadmap', docPath(docs[1], byId))!)
  })
  it('matches across the breadcrumb: parent and title words together', () => {
    expect(ids(open('grim road'))[0]).toBe('r')
    expect(ids(open('book kindle'))[0]).toBe('k')
  })
  it('finds a title through a typo', () => {
    expect(ids(open('roadmpa'))[0]).toBe('r')
    expect(ids(open('archtecture'))[0]).toBe('a')
  })
  it('"books" finds the Books place first (Tom: ⌘K buried it under everything else)', () => {
    expect(ids(open('books'))[0]).toBe('@books')
    expect(ids(open('boks'))[0]).toBe('@books')
    expect(ids(open('book'))).toEqual(expect.arrayContaining(['@books', 'bn']))
  })
  it('places answer to their keywords: "profile" finds Settings, "kobo" E-readers', () => {
    expect(ids(open('profile'))[0]).toBe('@settings')
    expect(ids(open('kobo'))[0]).toBe('@ereaders')
  })
  it('a recently opened doc wins a tie', () => {
    const two = [doc('x1', 'Notes'), doc('x2', 'Notes')]
    expect(ids(buildOpenRows({ q: 'notes', docs: two, places: [], recentIds: ['x2'] }))).toEqual(['x2', 'x1'])
  })
  it('empty query: recent docs, then every place, nothing else', () => {
    const rows = open('', ['t', 'gone', 'r'])
    expect(ids(rows)).toEqual(['t', 'r', ...places.map((p) => `@${p.id}`)])
  })
  it('never lists actions or content: only doc and place rows', () => {
    for (const q of ['', 'new doc', 'export', 'sign out', 'back up', 'omnibox']) {
      for (const r of open(q)) expect(['doc', 'place']).toContain(r.kind)
    }
    expect(ids(open('sign out')).filter((x) => !x.startsWith('@') && !docs.some((d) => d.id === x))).toEqual([])
  })
  it('keys are unique', () => {
    const rows = open('o')
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length)
  })
})

describe('places', () => {
  it('Home, Review, Gardeners, Graph, Trash and Settings always; Books and E-readers only with the library', () => {
    const local = buildPlaces({ library: false }).map((p) => p.id)
    expect(local).toEqual(['home', 'review', 'runs', 'graph', 'trash', 'settings'])
    const server = buildPlaces({ library: true }).map((p) => p.id)
    expect(server).toEqual(expect.arrayContaining([...local, 'books', 'ereaders']))
  })
  it('carry their shortcuts', () => {
    const keys = Object.fromEntries(buildPlaces({ library: true }).map((p) => [p.id, p.keys]))
    expect(keys).toMatchObject({ home: '⌘W', review: '⌘⇧R', runs: '⌘G', settings: '⌘,' })
  })
  it('Review shows the open count', () => {
    expect(buildPlaces({ library: false, queueCount: 3 }).find((p) => p.id === 'review')!.hint).toBe('3 open')
  })
})

describe('recent docs', () => {
  it('moves the id to the front, dedupes, caps', () => {
    expect(pushRecentId(['a', 'b'], 'b')).toEqual(['b', 'a'])
    expect(pushRecentId(['a', 'b'], 'c', 2)).toEqual(['c', 'a'])
  })
  it('resolves to live docs only, in order', () => {
    expect(recentDocs(['t', 'gone', 'g'], docs).map((d) => d.id)).toEqual(['t', 'g'])
  })
  it('builds breadcrumb paths', () => {
    const byId = new Map(docs.map((d) => [d.id, d]))
    expect(docPath(docs[1], byId)).toBe('Grimoire › Roadmap')
    expect(docPath(docs[0], byId)).toBe('Grimoire')
  })
})

describe('Actions (⌘K)', () => {
  const real = (docId: string | null, signedIn = true) =>
    buildCommands({ signedIn, library: true, onSignOut: () => {}, docId, onAction: () => {} })

  it('lists no docs and no places: every row is an action with a run()', () => {
    const cmds = real('r')
    const placeLabels = new Set(buildPlaces({ library: true }).map((p) => p.label))
    for (const c of cmds) {
      expect(typeof c.run).toBe('function')
      expect(placeLabels.has(c.label)).toBe(false)
      expect(docs.some((d) => d.title === c.label)).toBe(false)
    }
    expect(cmds.map((c) => c.id)).not.toEqual(expect.arrayContaining(['home', 'trash', 'profile', 'books', 'ereaders', 'inbox']))
  })
  it('the open doc’s actions come first; without a doc they are absent', () => {
    const withDoc = rankCommands(real('r'), '')
    expect(withDoc.slice(0, 3).map((c) => c.scope)).toEqual(['doc', 'doc', 'doc'])
    expect(withDoc.slice(3).every((c) => c.scope !== 'doc')).toBe(true)
    expect(real(null).some((c) => c.scope === 'doc')).toBe(false)
  })
  it('the palette shortcuts are rows, with their keys', () => {
    const keys = Object.fromEntries(real(null).map((c) => [c.id, c.keys]))
    expect(keys).toMatchObject({ newdoc: '⌘N', capture: '⌘⇧I', open: '⌘O', search: '⌘⇧F', ask: '⌘/', tree: '⌘T', help: '?' })
  })
  it('"Ask the library…" is an action', () => {
    expect(rankCommands(real(null), 'ask')[0].id).toBe('ask')
  })
  it('filters fuzzily over labels, then hints and keywords', () => {
    const cmds = [
      cmd('backup', 'Back up database now', { hint: 'daily snapshot' }),
      cmd('newdoc', 'New doc…'),
      cmd('export', 'Export all docs as Markdown…'),
      cmd('signout', 'Sign out', { keywords: 'log out logout' }),
    ]
    expect(rankCommands(cmds, 'back').map((c) => c.id)[0]).toBe('backup')
    expect(rankCommands(cmds, 'snapshot').map((c) => c.id)).toEqual(['backup'])
    expect(rankCommands(cmds, 'logout').map((c) => c.id)).toEqual(['signout'])
    expect(rankCommands(cmds, 'exprot').map((c) => c.id)[0]).toBe('export') // typo
    expect(rankCommands(cmds, 'zzz')).toEqual([])
  })
})

describe('Search (⌘⇧F)', () => {
  const hits = [hit('b1', 'r', 'Roadmap', '## Omnibox\n', 'heading'), hit('b2', 'a', 'Architecture', 'the omnibox mixes three lists')]

  it('a leading ? asks; the prefix is stripped', () => {
    expect(parseSearch('  ?why is it slow')).toEqual({ ask: true, q: 'why is it slow' })
    expect(parseSearch(' road ')).toEqual({ ask: false, q: 'road' })
    expect(parseSearch('>back')).toEqual({ ask: false, q: '>back' }) // no command mode here
  })
  it('content hits, then the ask row; never a doc-title or command row', () => {
    const rows = buildSearchRows({ ask: false, q: 'omni', hits })
    expect(rows.map((r) => r.group)).toEqual(['content', 'content', 'ask'])
    const last = rows[rows.length - 1]
    expect(last.group === 'ask' && last.query).toBe('omni')
  })
  it('? is the ask row alone; an empty query is nothing', () => {
    expect(buildSearchRows({ ask: true, q: 'why', hits })).toEqual([{ key: 'ask', group: 'ask', query: 'why' }])
    expect(buildSearchRows({ ask: false, q: '', hits })).toEqual([])
  })
  it('crumb adds the heading when the hit is one; snippet strips hashes and cuts', () => {
    expect(crumbOf(hit('b', 'r', 'Roadmap', '## Omnibox', 'heading'))).toBe('Roadmap › Omnibox')
    expect(crumbOf(hit('b', 'r', 'Roadmap', 'body'))).toBe('Roadmap')
    expect(snippetOf('---\ntags:\n---\n\n# Title line\nmore')).toBe('tags:')
    expect(snippetOf('# Heading only')).toBe('Heading only')
    expect(snippetOf('x'.repeat(200), 10)).toBe('xxxxxxxxx…')
  })
})

describe('stableSelection', () => {
  const C = (id: string): SearchRow => ({ key: `content:${id}`, group: 'content', hit: hit(id, 'r', 'R', 'x'), crumb: 'R', snippet: 'x' })
  const ASK: SearchRow = { key: 'ask', group: 'ask', query: 'q' }

  it('keeps the selected row by key when hits land around it', () => {
    expect(stableSelection([C('a'), ASK], 1, [C('a'), C('b'), C('c'), ASK])).toBe(3)
    expect(stableSelection([C('a'), C('b'), ASK], 1, [C('x'), C('b'), ASK])).toBe(1)
  })
  it('a vanished row falls back to the same group at the same offset, clamped', () => {
    expect(stableSelection([C('h1'), C('h2'), C('h3'), ASK], 2, [C('h9'), C('h8'), ASK])).toBe(1)
  })
  it('a vanished group clamps the index; an empty list is 0', () => {
    expect(stableSelection([C('a'), C('b')], 1, [ASK])).toBe(0)
    expect(stableSelection([C('a')], 0, [])).toBe(0)
    expect(stableSelection([], 0, [C('a'), C('b')])).toBe(0)
  })
})
