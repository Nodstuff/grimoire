import { describe, expect, it } from 'vitest'
import {
  buildRows,
  crumbOf,
  docPath,
  groupStarts,
  parseQuery,
  pushRecentId,
  rankCommands,
  rankDocs,
  recentDocs,
  snippetOf,
  stableSelection,
  type OmniCommand,
  type OmniRow,
} from './omni'

describe('stableSelection', () => {
  const D = (id: string): OmniRow => ({ key: `docs:${id}`, group: 'docs', doc: doc(id, id), path: id })
  const C = (id: string): OmniRow => ({ key: `content:${id}`, group: 'content', hit: hit(id, 'r', 'R', 'x'), crumb: 'R', snippet: 'x' })
  const K = (id: string): OmniRow => ({ key: `cmd:${id}`, group: 'commands', cmd: cmd(id, id) })
  const ASK: OmniRow = { key: 'ask', group: 'ask', query: 'q' }

  it('keeps the selected row by key when content lands between docs and commands', () => {
    const before = [D('a'), D('b'), K('review'), ASK]
    const after = [D('a'), D('b'), C('h1'), C('h2'), K('review'), ASK]
    expect(stableSelection(before, 1, after)).toBe(1)
    expect(stableSelection(before, 2, after)).toBe(4) // the command moved down, selection follows it
    expect(stableSelection(before, 3, after)).toBe(5)
  })
  it('a vanished row falls back to the same group at the same offset, clamped', () => {
    const before = [D('a'), C('h1'), C('h2'), C('h3'), K('k')]
    const after = [D('a'), C('h9'), C('h8'), K('k')]
    expect(stableSelection(before, 3, after)).toBe(2) // third content hit → last content hit
    expect(stableSelection(before, 1, after)).toBe(1)
  })
  it('a vanished group clamps the index; an empty list is 0', () => {
    expect(stableSelection([D('a'), C('h1')], 1, [D('a')])).toBe(0)
    expect(stableSelection([D('a')], 0, [])).toBe(0)
    expect(stableSelection([], 0, [D('a'), D('b')])).toBe(0)
    expect(stableSelection([D('a'), D('b'), D('c')], 2, [K('x')])).toBe(0)
  })
})
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

const cmd = (id: string, label: string, hint?: string): OmniCommand => ({ id, label, hint, run: () => {} })

const docs = [
  doc('g', 'Grimoire'),
  doc('r', 'Roadmap', 'g'),
  doc('a', 'Architecture', 'g'),
  doc('d', 'Daily'),
  doc('t', '2026-09-10', 'd'),
  doc('q', 'qompass notes'),
]
const commands = [
  cmd('review', 'Review queue', '⌘⇧R'),
  cmd('newdoc', 'New doc…'),
  cmd('gardeners', 'Gardeners'),
  cmd('backup', 'Back up database now', 'daily snapshot'),
  cmd('home', 'Home'),
  cmd('trash', 'Trash', 'restore deleted docs'),
  cmd('export', 'Export all docs as Markdown…'),
]

describe('parseQuery', () => {
  it('> forces commands and ? forces ask, prefix stripped', () => {
    expect(parseQuery('> back')).toEqual({ mode: 'commands', q: 'back' })
    expect(parseQuery('  ?why is it slow')).toEqual({ mode: 'ask', q: 'why is it slow' })
    expect(parseQuery('>')).toEqual({ mode: 'commands', q: '' })
  })
  it('otherwise keeps the mode the box opened in', () => {
    expect(parseQuery(' road ')).toEqual({ mode: 'mixed', q: 'road' })
    expect(parseQuery('road', 'docs')).toEqual({ mode: 'docs', q: 'road' })
    expect(parseQuery('', 'ask')).toEqual({ mode: 'ask', q: '' })
  })
})

describe('rankDocs', () => {
  it('exact > prefix > substring > path > fuzzy', () => {
    expect(rankDocs(docs, 'roadmap').map((d) => d.id)).toEqual(['r'])
    expect(rankDocs(docs, 'gri').map((d) => d.id)[0]).toBe('g')
    // "grimoire" is in the PATH of Roadmap and Architecture, not their titles
    expect(rankDocs(docs, 'grimoire').map((d) => d.id)).toEqual(['g', 'r', 'a'])
    // fuzzy: q…s spans "qompass notes"
    expect(rankDocs(docs, 'qns').map((d) => d.id)).toEqual(['q'])
    expect(rankDocs(docs, 'zzz')).toEqual([])
  })
  it('caps at the limit and returns the head when the query is empty', () => {
    expect(rankDocs(docs, '', 2).map((d) => d.id)).toEqual(['g', 'r'])
  })
  it('builds breadcrumb paths', () => {
    const byId = new Map(docs.map((d) => [d.id, d]))
    expect(docPath(docs[1], byId)).toBe('Grimoire › Roadmap')
    expect(docPath(docs[0], byId)).toBe('Grimoire')
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
})

describe('rankCommands', () => {
  it('matches labels first, then all words across label+hint', () => {
    expect(rankCommands(commands, 'back', 4).map((c) => c.id)).toEqual(['backup'])
    expect(rankCommands(commands, 'restore', 4).map((c) => c.id)).toEqual(['trash'])
    expect(rankCommands(commands, '', 2).map((c) => c.id)).toEqual(['review', 'newdoc'])
  })
})

describe('buildRows', () => {
  const hits = [hit('b1', 'r', 'Roadmap', '## Omnibox\n', 'heading'), hit('b2', 'a', 'Architecture', 'the omnibox mixes three lists')]

  it('empty mixed query → recent docs then top commands, no ask row', () => {
    const rows = buildRows({ mode: 'mixed', q: '', docs, recentIds: ['t', 'r'], hits: [], commands })
    expect(rows.map((r) => r.group)).toEqual(['recent', 'recent', ...Array(6).fill('commands')])
    expect(rows[0].key).toBe('recent:t')
    expect(rows.some((r) => r.group === 'ask')).toBe(false)
  })

  it('a typed mixed query → docs, content, commands, then the ask row, in that order', () => {
    const rows = buildRows({ mode: 'mixed', q: 'omni', docs, recentIds: ['t'], hits, commands })
    const groups = rows.map((r) => r.group)
    expect(groups.filter((g) => g === 'recent')).toHaveLength(0)
    expect(groups[groups.length - 1]).toBe('ask')
    // strictly non-decreasing in group order
    const order = ['docs', 'content', 'commands', 'ask']
    for (let i = 1; i < groups.length; i++) expect(order.indexOf(groups[i])).toBeGreaterThanOrEqual(order.indexOf(groups[i - 1]))
    expect(rows.filter((r) => r.group === 'content')).toHaveLength(2)
    const last = rows[rows.length - 1]
    expect(last.group === 'ask' && last.query).toBe('omni')
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length)
  })

  it('caps docs at 5, content at 5 and commands at 4 in mixed mode', () => {
    const many = Array.from({ length: 9 }, (_, i) => doc(`x${i}`, `xdoc ${i}`))
    const manyHits = Array.from({ length: 9 }, (_, i) => hit(`h${i}`, 'x0', 'xdoc 0', `xdoc hit ${i}`))
    const manyCmds = Array.from({ length: 9 }, (_, i) => cmd(`c${i}`, `xdoc command ${i}`))
    const rows = buildRows({ mode: 'mixed', q: 'xdoc', docs: many, recentIds: [], hits: manyHits, commands: manyCmds })
    const count = (g: string) => rows.filter((r) => r.group === g).length
    expect([count('docs'), count('content'), count('commands'), count('ask')]).toEqual([5, 5, 4, 1])
  })

  it('> shows commands only, ? shows the ask row only', () => {
    expect(buildRows({ mode: 'commands', q: 'back', docs, recentIds: [], hits, commands }).map((r) => r.key)).toEqual([
      'cmd:backup',
    ])
    const ask = buildRows({ mode: 'ask', q: 'why', docs, recentIds: [], hits, commands })
    expect(ask).toEqual([{ key: 'ask', group: 'ask', query: 'why' }])
  })

  it('docs mode lists recent then all docs when empty; content mode lists hits + ask', () => {
    const d = buildRows({ mode: 'docs', q: '', docs, recentIds: ['r'], hits: [], commands })
    expect(d[0].key).toBe('recent:r')
    expect(d.filter((r) => r.group === 'docs').map((r) => r.key)).not.toContain('docs:r')
    expect(d).toHaveLength(docs.length)
    const c = buildRows({ mode: 'content', q: 'omni', docs, recentIds: [], hits, commands })
    expect(c.map((r) => r.group)).toEqual(['content', 'content', 'ask'])
  })

  it('marks where each group starts', () => {
    const rows = buildRows({ mode: 'mixed', q: 'omni', docs, recentIds: [], hits, commands })
    const starts = [...groupStarts(rows)]
    expect(starts[0]).toBe(0)
    expect(starts.length).toBe(new Set(rows.map((r) => r.group)).size)
  })
})

describe('content rows', () => {
  it('crumb adds the heading when the hit is one; snippet strips hashes and cuts', () => {
    expect(crumbOf(hit('b', 'r', 'Roadmap', '## Omnibox', 'heading'))).toBe('Roadmap › Omnibox')
    expect(crumbOf(hit('b', 'r', 'Roadmap', 'body'))).toBe('Roadmap')
    expect(snippetOf('---\ntags:\n---\n\n# Title line\nmore')).toBe('tags:')
    expect(snippetOf('# Heading only')).toBe('Heading only')
    expect(snippetOf('x'.repeat(200), 10)).toBe('xxxxxxxxx…')
  })
})
