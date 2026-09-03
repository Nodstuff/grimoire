import { describe, expect, it } from 'vitest'
import type { Doc } from './types'
import {
  ancestorsOf,
  childrenIndex,
  descendantCount,
  descendantCounts,
  filterDocs,
  groupRoot,
  loadTreeState,
  saveTreeState,
  TREE_STATE_KEY,
} from './tree'

function doc(id: string, title: string, parent: string | null = null, extra: Partial<Doc> = {}): Doc {
  return {
    id,
    title,
    parent_id: parent,
    review_policy: null,
    current_epoch: 1,
    created_by: 'me',
    status: null,
    sort_key: null,
    ...extra,
  }
}

const docs: Doc[] = [
  doc('daily', 'Daily'),
  doc('d1', '2026-09-03', 'daily'),
  doc('zeta', 'Zeta project'),
  doc('z1', 'Zeta design', 'zeta'),
  doc('z11', 'Zeta design detail', 'z1'),
  doc('alpha', 'Alpha project'),
  doc('a1', 'Alpha notes', 'alpha'),
  doc('hub', 'Team', null, { from_hub: true }),
  doc('loose', 'Loose note', null, { sort_key: 'b' }),
  doc('loose2', 'Another loose note', null, { sort_key: 'a' }),
]
const children = childrenIndex(docs)
const byId = new Map(docs.map((d) => [d.id, d]))

describe('groupRoot', () => {
  it('pins system folders and hub roots, sorts folders alphabetically, keeps note order', () => {
    const g = groupRoot(docs, children)
    expect(g.pinned.map((d) => d.id)).toEqual(['daily', 'hub'])
    expect(g.folders.map((d) => d.id)).toEqual(['alpha', 'zeta'])
    // server order: sort_key asc (loose2 'a' before loose 'b')
    expect(g.notes.map((d) => d.id)).toEqual(['loose2', 'loose'])
  })
})

describe('filterDocs', () => {
  it('is case-insensitive substring and expands ancestors of matches', () => {
    const r = filterDocs(docs, 'DETAIL', byId)!
    expect([...r.matches]).toEqual(['z11'])
    expect(r.visible).toEqual(new Set(['z11', 'z1', 'zeta']))
    expect(r.expanded).toEqual(new Set(['z1', 'zeta']))
  })
  it('blank query means no filter', () => {
    expect(filterDocs(docs, '   ', byId)).toBeNull()
  })
})

describe('ancestorsOf / descendantCount', () => {
  it('walks parents nearest-first and counts the whole subtree', () => {
    expect(ancestorsOf('z11', byId)).toEqual(['z1', 'zeta'])
    expect(ancestorsOf('daily', byId)).toEqual([])
    expect(descendantCount('zeta', children)).toBe(2)
    expect(descendantCount('loose', children)).toBe(0)
  })
  it('descendantCounts matches descendantCount for every doc, in one pass', () => {
    const all = descendantCounts(children)
    for (const kids of children.values()) {
      for (const d of kids) expect(all.get(d.id) ?? 0).toBe(descendantCount(d.id, children))
    }
    expect(all.get('zeta')).toBe(2)
    expect(all.get('loose')).toBe(0)
  })
})

describe('tree state persistence', () => {
  it('round-trips and survives garbage / a throwing storage', () => {
    const mem = new Map<string, string>()
    const storage = { getItem: (k: string) => mem.get(k) ?? null, setItem: (k: string, v: string) => mem.set(k, v) }
    saveTreeState({ closedSections: ['notes'], open: ['zeta'] }, storage)
    expect(loadTreeState(storage)).toEqual({ closedSections: ['notes'], open: ['zeta'] })
    mem.set(TREE_STATE_KEY, '{not json')
    expect(loadTreeState(storage)).toEqual({ closedSections: [], open: [] })
    mem.set(TREE_STATE_KEY, JSON.stringify({ closedSections: ['bogus', 'pinned'], open: [1, 'x'] }))
    expect(loadTreeState(storage)).toEqual({ closedSections: ['pinned'], open: ['x'] })
    const boom = {
      getItem: () => {
        throw new Error('denied')
      },
      setItem: () => {
        throw new Error('denied')
      },
    }
    expect(loadTreeState(boom)).toEqual({ closedSections: [], open: [] })
    expect(() => saveTreeState({ closedSections: [], open: [] }, boom)).not.toThrow()
  })
})
