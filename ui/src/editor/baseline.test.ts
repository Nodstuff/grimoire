// Autosave seam: after a save, the written entries become the baseline via
// rebuildBaseline. Re-diffing the same entries against that baseline must be
// a no-op — otherwise every autosave tick would emit phantom replaces/moves.
import { describe, expect, it } from 'vitest'
import { BaselineBlock, Entry, computeOps } from './diff'
import { rebuildBaseline } from './DocEditor'

let n = 0
const newId = () => `new-${++n}`

const baseline: BaselineBlock[] = [
  { id: 'h1', content: '# Title', parent: null, order_key: 'i' },
  { id: 'p1', content: 'first para', parent: 'h1', order_key: 'i' },
  { id: 'p2', content: 'second para', parent: 'h1', order_key: 'q' },
]

describe('rebuildBaseline', () => {
  it('unchanged doc: baseline is preserved and re-diff is empty', () => {
    const entries: Entry[] = [
      { id: 'h1', content: '# Title', level: 1 },
      { id: 'p1', content: 'first para', level: 0 },
      { id: 'p2', content: 'second para', level: 0 },
    ]
    const ops = computeOps(baseline, entries, newId)
    const next = rebuildBaseline(baseline, entries, ops)
    expect(next).toEqual(baseline)
    expect(computeOps(next, entries, newId)).toEqual([])
  })

  it('edited content: new baseline carries the edit, no phantom replace afterwards', () => {
    const entries: Entry[] = [
      { id: 'h1', content: '# Title', level: 1 },
      { id: 'p1', content: 'first para, edited', level: 0 },
      { id: 'p2', content: 'second para', level: 0 },
    ]
    const ops = computeOps(baseline, entries, newId)
    expect(ops.map((o) => o.kind.op)).toEqual(['replace'])
    const next = rebuildBaseline(baseline, entries, ops)
    expect(next.find((b) => b.id === 'p1')).toEqual({
      id: 'p1',
      content: 'first para, edited',
      parent: 'h1',
      order_key: 'i',
    })
    expect(computeOps(next, entries, newId)).toEqual([])
  })

  it('inserted entries adopt the ids/keys the insert ops assigned', () => {
    const entries: Entry[] = [
      { id: 'h1', content: '# Title', level: 1 },
      { id: 'p1', content: 'first para', level: 0 },
      { id: null, content: 'between', level: 0 },
      { id: 'p2', content: 'second para', level: 0 },
      { id: null, content: '## Section', level: 2 },
      { id: null, content: 'body', level: 0 },
    ]
    const ops = computeOps(baseline, entries, newId)
    const inserts = ops.filter((o) => o.kind.op === 'insert')
    expect(inserts).toHaveLength(3)
    const next = rebuildBaseline(baseline, entries, ops)
    expect(next).toHaveLength(6)
    const between = next[2]
    expect(between.id).toBe(inserts[0].kind.block_id)
    expect(between.parent).toBe('h1')
    expect(between.order_key > 'i' && between.order_key < 'q').toBe(true)
    const section = next[4]
    const body = next[5]
    expect(body.parent).toBe(section.id)
    // the editor stamps these ids onto the nodes; the next diff sees a clean doc
    const stamped: Entry[] = entries.map((e, i) => ({ ...e, id: next[i].id }))
    expect(computeOps(next, stamped, newId)).toEqual([])
  })

  it('moved entries take the new parent and key from the move op', () => {
    const entries: Entry[] = [
      { id: 'h1', content: '# Title', level: 1 },
      { id: 'p2', content: 'second para', level: 0 },
      { id: 'p1', content: 'first para', level: 0 },
    ]
    const ops = computeOps(baseline, entries, newId)
    const move = ops.find((o) => o.kind.op === 'move')!
    const next = rebuildBaseline(baseline, entries, ops)
    const movedRow = next.find((b) => b.id === move.kind.target)!
    expect(movedRow.order_key).toBe(move.kind.new_order_key)
    expect(movedRow.parent).toBe(move.kind.new_parent)
    // sibling order in the baseline now matches the editor order
    const kids = next.filter((b) => b.parent === 'h1')
    expect(kids.map((b) => b.id)).toEqual(['p2', 'p1'])
    expect(kids[0].order_key < kids[1].order_key).toBe(true)
    expect(computeOps(next, entries, newId)).toEqual([])
  })

  it('deleted entries vanish from the baseline', () => {
    const entries: Entry[] = [
      { id: 'h1', content: '# Title', level: 1 },
      { id: 'p2', content: 'second para', level: 0 },
    ]
    const ops = computeOps(baseline, entries, newId)
    expect(ops.map((o) => o.kind.op)).toEqual(['delete'])
    const next = rebuildBaseline(baseline, entries, ops)
    expect(next.map((b) => b.id)).toEqual(['h1', 'p2'])
    expect(computeOps(next, entries, newId)).toEqual([])
  })
})
