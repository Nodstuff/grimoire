import { describe, expect, it } from 'vitest'
import { BaselineBlock, Entry, computeOps, keyBetween } from './diff'

let n = 0
const newId = () => `new-${++n}`

function base(id: string, content: string, parent: string | null, key: string): BaselineBlock {
  return { id, content, parent, order_key: key }
}

describe('keyBetween', () => {
  it('orders', () => {
    const a = keyBetween(null, null)
    const b = keyBetween(a, null)
    const c = keyBetween(a, b)
    expect(a < b).toBe(true)
    expect(a < c && c < b).toBe(true)
  })

  // Fixed vectors shared with the Rust reference (crates/store/src/order_key.rs
  // `between`); both sides assert the same table so the two ports cannot drift.
  it('matches the Rust order_key::between vectors', () => {
    const vectors: [string | null, string | null, string][] = [
      [null, null, 'i'], // (0+36)/2 = 18
      ['i', null, 'r'], // (18+36)/2 = 27
      [null, 'i', '9'], // (0+18)/2 = 9
      ['i', 'r', 'm'], // (18+27)/2 = 22
      ['i', 'j', 'ii'], // adjacent digits: keep 'i', bisect (0, 36) → 'i'
      ['z', null, 'zi'], // 35 vs open end 36: keep 'z', bisect → 'i'
    ]
    for (const [a, b, want] of vectors) {
      expect(keyBetween(a, b), `keyBetween(${a}, ${b})`).toBe(want)
    }
  })

  it('never emits a key ending in 0 and stays strictly inside the bounds', () => {
    const cases: [string | null, string | null][] = [
      [null, null],
      ['i', null],
      [null, 'i'],
      ['i', 'r'],
      ['i', 'j'],
      ['z', null],
      ['ii', 'ij'],
      ['zz', null],
    ]
    for (const [a, b] of cases) {
      const k = keyBetween(a, b)
      expect(k.endsWith('0')).toBe(false)
      if (a != null) expect(a < k).toBe(true)
      if (b != null) expect(k < b).toBe(true)
    }
  })
})

describe('computeOps', () => {
  const baseline: BaselineBlock[] = [
    base('h1', '# Title', null, 'i'),
    base('p1', 'first para', 'h1', 'i'),
    base('p2', 'second para', 'h1', 'q'),
  ]
  const entries: Entry[] = [
    { id: 'h1', content: '# Title', level: 1 },
    { id: 'p1', content: 'first para', level: 0 },
    { id: 'p2', content: 'second para', level: 0 },
  ]

  it('no changes → no ops', () => {
    expect(computeOps(baseline, entries, newId)).toEqual([])
  })

  it('content change → replace only', () => {
    const e = entries.map((x) => (x.id === 'p1' ? { ...x, content: 'edited' } : x))
    const ops = computeOps(baseline, e, newId)
    expect(ops).toHaveLength(1)
    expect(ops[0].kind).toMatchObject({ op: 'replace', target: 'p1', content: 'edited' })
  })

  it('removed entry → delete', () => {
    const e = entries.filter((x) => x.id !== 'p2')
    const ops = computeOps(baseline, e, newId)
    expect(ops).toHaveLength(1)
    expect(ops[0].kind).toMatchObject({ op: 'delete', target: 'p2' })
  })

  it('new entry between siblings → insert with key between', () => {
    const e: Entry[] = [
      entries[0],
      entries[1],
      { id: null, content: 'inserted', level: 0 },
      entries[2],
    ]
    const ops = computeOps(baseline, e, newId)
    expect(ops).toHaveLength(1)
    const k = ops[0].kind as unknown as { op: string; order_key: string; parent_id: string }
    expect(k.op).toBe('insert')
    expect(k.parent_id).toBe('h1')
    expect(k.order_key > 'i' && k.order_key < 'q').toBe(true)
  })

  it('swapped siblings → one move, not two', () => {
    const e: Entry[] = [entries[0], entries[2], entries[1]]
    const ops = computeOps(baseline, e, newId)
    expect(ops).toHaveLength(1)
    const k = ops[0].kind as unknown as { op: string; target: string; new_order_key: string }
    expect(k.op).toBe('move')
    // either block may move; its key must land on the right side of the other
    if (k.target === 'p2') expect(k.new_order_key < 'i').toBe(true)
    else expect(k.new_order_key > 'q').toBe(true)
  })

  it('deleting a heading reparents its children', () => {
    const e: Entry[] = [entries[1], entries[2]] // heading gone
    const ops = computeOps(baseline, e, newId)
    const del = ops.find((o) => o.kind.op === 'delete')
    expect(del?.kind).toMatchObject({ target: 'h1' })
    const moves = ops.filter((o) => o.kind.op === 'move')
    expect(moves).toHaveLength(2)
    for (const m of moves) expect((m.kind as unknown as { new_parent: null }).new_parent).toBeNull()
  })

  it('new heading adopts following paragraphs', () => {
    const e: Entry[] = [
      ...entries,
      { id: null, content: '## New Section', level: 2 },
      { id: null, content: 'section body', level: 0 },
    ]
    const ops = computeOps(baseline, e, newId)
    const inserts = ops.filter((o) => o.kind.op === 'insert')
    expect(inserts).toHaveLength(2)
    const head = inserts.find((o) => (o.kind.content as string).startsWith('##'))!
    const body = inserts.find((o) => o.kind.content === 'section body')!
    expect(body.kind.parent_id).toBe(head.kind.block_id)
    expect(head.kind.block_type).toBe('heading')
    expect(body.kind.block_type).toBe('paragraph')
  })
})
