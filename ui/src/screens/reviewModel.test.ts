import { describe, expect, it } from 'vitest'
import {
  bulkWord,
  captions,
  docLocation,
  groupByAgent,
  groupByDoc,
  isReorder,
  sentenceOf,
  sideOf,
  uuidTime,
  whenLabel,
  wordDiff,
} from './reviewModel'
import type { Doc, QueueRow } from '../types'
import type { Workspace } from '../workspaces'

function row(
  id: string,
  doc: string,
  proposer: string,
  kind: 'review' | 'parked',
  opKind: Record<string, unknown> & { op: string },
): QueueRow {
  return {
    item: {
      annotation: { id, doc_id: doc, op_id: `op-${id}`, kind, status: 'open' },
      op: {
        id: `op-${id}`,
        kind: opKind,
        principal: 'p',
        base_epoch: 0,
        epoch_applied: kind === 'review' ? 1 : null,
        verdict: kind === 'review' ? 'yellow' : 'red',
        confidence: 1,
        prior: null,
        source_refs: [],
      },
    },
    doc_title: `T-${doc}`,
    proposer,
    current_content: null,
  }
}

function doc(id: string, title: string, parent: string | null, ws: string | null = null): Doc {
  return {
    id,
    parent_id: parent,
    title,
    review_policy: null,
    current_epoch: 1,
    created_by: 'p',
    status: null,
    sort_key: null,
    workspace_id: ws,
  }
}

describe('uuidTime / whenLabel', () => {
  it('reads the ms out of a UUIDv7 and nothing else', () => {
    const ms = 0x01a169c4da86
    expect(uuidTime('01a169c4-da86-7143-943e-d2835e7de6e6')).toBe(ms)
    expect(uuidTime('01a169c4-da86-4143-943e-d2835e7de6e6')).toBeNull() // v4
    expect(uuidTime('nope')).toBeNull()
  })
  it('says the time today, Yesterday, then the date', () => {
    const now = new Date(2026, 9, 23, 12, 0).getTime()
    expect(whenLabel(new Date(2026, 9, 23, 9, 14).getTime(), now)).toBe('09:14')
    expect(whenLabel(new Date(2026, 9, 22, 21, 40).getTime(), now)).toBe('Yesterday 21:40')
    expect(whenLabel(new Date(2026, 9, 3, 8, 2).getTime(), now)).toBe('Oct 3 08:02')
    expect(whenLabel(new Date(2025, 0, 5, 8, 2).getTime(), now)).toBe('Jan 5 2025 08:02')
  })
})

describe('wordDiff', () => {
  it('marks the inserted tail only', () => {
    const p = wordDiff('Tags browser over /api/tags', 'Tags browser over /api/tags, with filter chips')
    expect(p.filter((x) => x.kind !== 'same').map((x) => [x.kind, x.text])).toEqual([
      ['ins', ', with filter chips'],
    ])
    expect(sideOf(p, 'old').map((x) => x.text).join('')).toBe('Tags browser over /api/tags')
    expect(sideOf(p, 'new').map((x) => x.text).join('')).toBe('Tags browser over /api/tags, with filter chips')
  })
  it('a pure insert of words keeps the rest same', () => {
    const p = wordDiff('a c', 'a b c')
    expect(p).toEqual([
      { kind: 'same', text: 'a ' },
      { kind: 'ins', text: 'b ' },
      { kind: 'same', text: 'c' },
    ])
  })
  it('falls back to whole del/ins for huge texts', () => {
    const big = Array.from({ length: 700 }, (_, i) => `w${i}`).join(' ')
    expect(wordDiff(big, big + ' x').map((x) => x.kind)).toEqual(['del', 'ins'])
  })
})

describe('grouping', () => {
  const rows = [
    row('1', 'A', 'claude:docs', 'review', { op: 'replace', content: 'x' }),
    row('2', 'B', 'keeper:API', 'parked', { op: 'delete_doc', title: 'B', doc_count: 1 }),
    row('3', 'A', 'keeper:API', 'review', { op: 'insert', content: 'y' }),
  ]
  it('by doc keeps first-appearance order', () => {
    expect(groupByDoc(rows).map((g) => [g.docId, g.rows.length])).toEqual([
      ['A', 2],
      ['B', 1],
    ])
  })
  it('by agent never puts a parked item in the bulk set', () => {
    const g = groupByAgent(rows)
    const keeper = g.find((x) => x.agent === 'keeper:API')!
    expect(keeper.rows.length).toBe(2)
    expect(keeper.flagged.map((r) => r.item.annotation.id)).toEqual(['3'])
  })
})

describe('docLocation', () => {
  const docs = [doc('t', 'Taisce', null, 'w'), doc('r', 'Roadmap', 't', 'w'), doc('u', 'Loose', null, null)]
  const byId = new Map(docs.map((d) => [d.id, d]))
  const ws = [{ id: 'w', name: 'Work', color: null, icon: null, sort_key: null, doc_ids: [] } as Workspace]
  it('names the workspace and the parent chain', () => {
    expect(docLocation('r', byId, ws)).toEqual({ workspace: 'Work', path: 'Taisce' })
    expect(docLocation('u', byId, ws)).toEqual({ workspace: 'Unsorted', path: '' })
    expect(docLocation('missing', byId, ws)).toEqual({ workspace: null, path: '' })
  })
})

describe('sentences and captions', () => {
  it('reads each op in the artboard words', () => {
    expect(sentenceOf(row('1', 'A', 'a', 'review', { op: 'replace' })).pre).toBe('replaced a block')
    expect(sentenceOf(row('1', 'A', 'a', 'review', { op: 'insert' })).pre).toBe('added a block')
    expect(sentenceOf(row('1', 'A', 'a', 'review', { op: 'set_status', status: 'draft', from_status: null }))).toEqual({
      pre: 'set status ',
      strong: 'none → draft',
    })
    expect(sentenceOf(row('1', 'A', 'a', 'parked', { op: 'delete_doc', title: 'Development' }))).toEqual({
      pre: 'wants to move ',
      strong: 'Development',
      post: ' to Trash',
    })
  })
  it('a move under the same parent is a reorder', () => {
    const r = row('1', 'A', 'a', 'review', { op: 'move_doc', from_parent: null, new_parent: null })
    expect(isReorder(r)).toBe(true)
    expect(sentenceOf(r).pre).toBe('reordered this doc among its siblings')
    const m = row('1', 'A', 'a', 'review', { op: 'move_doc', from_parent: null, new_parent: 'x', new_parent_title: 'Roadmap' })
    expect(isReorder(m)).toBe(false)
    expect(sentenceOf(m)).toEqual({ pre: 'moved this doc under ', strong: 'Roadmap' })
  })
  it('captions say what each button does', () => {
    expect(captions(row('1', 'A', 'a', 'review', { op: 'replace' }))).toMatchObject({ accept: 'Keep', decline: 'Revert' })
    expect(captions(row('1', 'A', 'a', 'review', { op: 'insert' }))).toMatchObject({ accept: 'Keep', decline: 'Remove it' })
    expect(captions(row('1', 'A', 'a', 'review', { op: 'rename_doc' }))).toMatchObject({ accept: 'Keep new name', decline: 'Rename back' })
    expect(captions(row('1', 'A', 'a', 'review', { op: 'set_status', from_status: null }))).toMatchObject({ decline: 'Clear status' })
    expect(captions(row('1', 'A', 'a', 'parked', { op: 'delete_doc' }))).toMatchObject({ accept: 'Move to Trash', decline: 'Keep the doc' })
    expect(captions(row('1', 'A', 'a', 'parked', { op: 'replace' }))).toMatchObject({ accept: 'Apply', decline: 'Discard' })
  })
  it('bulk words', () => {
    expect(bulkWord('Keep', 2)).toBe('Keep both')
    expect(bulkWord('Revert', 3)).toBe('Revert all 3')
  })
})
