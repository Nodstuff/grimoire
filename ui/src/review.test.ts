import { describe, expect, it } from 'vitest'
import { actionLabels, buildHighlightMap, describeChange, isDocOp, targetBlockOf, toneOf } from './review'
import type { Block, QueueRow } from './types'

function block(id: string, content: string): Block {
  return {
    id,
    doc_id: 'd',
    parent_id: null,
    order_key: 'i',
    block_type: 'paragraph',
    content,
    created_by: 'p',
    epoch: 1,
    deleted: false,
    refers_to: null,
  }
}

function row(
  kind: 'review' | 'parked',
  op: Record<string, unknown> & { op: string },
  prior: Block | null = null,
  current: string | null = null,
): QueueRow {
  return {
    item: {
      annotation: { id: `a-${Math.random()}`, doc_id: 'd', op_id: 'o', kind, status: 'open' },
      op: {
        id: 'o',
        kind: op,
        principal: 'remote',
        base_epoch: 1,
        epoch_applied: kind === 'review' ? 2 : null,
        verdict: kind === 'review' ? 'yellow' : 'red',
        confidence: null,
        prior,
        source_refs: [],
      },
    },
    doc_title: 'Doc',
    proposer: 'alice',
    current_content: current,
  }
}

describe('targetBlockOf', () => {
  it('replace/delete/move point at target', () => {
    expect(targetBlockOf(row('parked', { op: 'replace', target: 'b1' }))).toBe('b1')
    expect(targetBlockOf(row('parked', { op: 'delete', target: 'b2' }))).toBe('b2')
    expect(targetBlockOf(row('review', { op: 'move', target: 'b3' }))).toBe('b3')
  })
  it('falls back to prior.id when target is missing', () => {
    expect(targetBlockOf(row('review', { op: 'replace' }, block('b9', 'x')))).toBe('b9')
  })
  it('red insert has no existing block; yellow insert does', () => {
    expect(targetBlockOf(row('parked', { op: 'insert', block_id: 'n1', content: 'new' }))).toBeNull()
    expect(targetBlockOf(row('review', { op: 'insert', block_id: 'n1', content: 'new' }))).toBe('n1')
  })
})

describe('toneOf / buildHighlightMap', () => {
  it('maps kinds to tones', () => {
    expect(toneOf(row('review', { op: 'replace', target: 'b' }))).toBe('yellow')
    expect(toneOf(row('parked', { op: 'replace', target: 'b' }))).toBe('red')
    expect(toneOf(row('parked', { op: 'delete', target: 'b' }))).toBe('red-delete')
  })
  it('builds a blockId → tone map, skipping red inserts', () => {
    const m = buildHighlightMap([
      row('review', { op: 'replace', target: 'y1' }),
      row('parked', { op: 'replace', target: 'r1' }),
      row('parked', { op: 'delete', target: 'r2' }),
      row('parked', { op: 'insert', block_id: 'new', content: 'x' }),
    ])
    expect(m).toEqual({ y1: 'yellow', r1: 'red', r2: 'red-delete' })
  })
  it('pending red outranks applied yellow on the same block; delete outranks replace', () => {
    const m = buildHighlightMap([
      row('review', { op: 'replace', target: 'b' }),
      row('parked', { op: 'replace', target: 'b' }),
    ])
    expect(m.b).toBe('red')
    const m2 = buildHighlightMap([
      row('parked', { op: 'delete', target: 'b' }),
      row('parked', { op: 'replace', target: 'b' }),
    ])
    expect(m2.b).toBe('red-delete')
  })
})

describe('describeChange', () => {
  it('yellow replace shows only the pre-image', () => {
    const d = describeChange(row('review', { op: 'replace', target: 'b', content: 'new' }, block('b', 'old')))
    expect(d.badge).toBe('applied · flagged')
    expect(d.before).toEqual({ label: 'was', text: 'old' })
    expect(d.after).toBeNull()
  })
  it('red replace shows live vs proposed, preferring current_content', () => {
    const d = describeChange(
      row('parked', { op: 'replace', target: 'b', content: 'new' }, block('b', 'old'), 'live now'),
    )
    expect(d.badge).toBe('proposed · not applied')
    expect(d.before).toEqual({ label: 'current', text: 'live now' })
    expect(d.after).toEqual({ label: 'proposed', text: 'new' })
  })
  it('red insert shows the proposed block; red delete is a sentence', () => {
    const i = describeChange(row('parked', { op: 'insert', block_id: 'n', content: 'hello' }))
    expect(i.after).toEqual({ label: 'proposed', text: 'hello' })
    const del = describeChange(row('parked', { op: 'delete', target: 'b' }))
    expect(del.headline).toBe('proposes deleting this block')
    expect(del.before).toBeNull()
    expect(del.after).toBeNull()
  })
})

describe('doc ops (agent tree changes through the gate)', () => {
  it('are recognised and never point at a block', () => {
    for (const op of ['rename_doc', 'move_doc', 'set_status', 'delete_doc']) {
      expect(isDocOp(op)).toBe(true)
      expect(targetBlockOf(row('review', { op }))).toBeNull()
    }
    expect(isDocOp('replace')).toBe(false)
    expect(buildHighlightMap([row('parked', { op: 'delete_doc', title: 'X', doc_count: 1 })])).toEqual({})
  })
  it('yellow rename reads as a sentence with was/now', () => {
    const d = describeChange(row('review', { op: 'rename_doc', title: 'New', from_title: 'Old' }))
    expect(d.badge).toBe('applied · flagged')
    expect(d.headline).toBe('alice renamed “Old” to “New”')
    expect(d.before).toEqual({ label: 'was', text: 'Old' })
    expect(d.after).toEqual({ label: 'now', text: 'New' })
    expect(actionLabels(row('review', { op: 'rename_doc', title: 'New', from_title: 'Old' }))).toEqual({
      accept: 'keep',
      decline: 'revert',
    })
  })
  it('move uses parent titles, falling back to the top level', () => {
    const d = describeChange(
      row('review', {
        op: 'move_doc',
        new_parent: 'p2',
        new_parent_title: 'Archive',
        from_parent: null,
        from_parent_title: null,
      }),
    )
    expect(d.headline).toBe('alice moved this doc under “Archive”')
    expect(d.before).toEqual({ label: 'was under', text: 'the top level' })
    expect(d.after).toEqual({ label: 'now under', text: 'Archive' })
  })
  it('status shows none for a cleared status', () => {
    const d = describeChange(row('review', { op: 'set_status', status: null, from_status: 'decided' }))
    expect(d.headline).toBe('alice set status to none')
    expect(d.before).toEqual({ label: 'was', text: 'decided' })
  })
  it('parked delete_doc says who wants to trash what, and the buttons say trash/keep', () => {
    const r = row('parked', { op: 'delete_doc', title: 'Dup', doc_count: 3 })
    const d = describeChange(r)
    expect(d.badge).toBe('proposed · not applied')
    expect(d.headline).toBe('alice wants to trash “Dup” (3 docs)')
    expect(d.before).toBeNull()
    expect(d.after).toBeNull()
    expect(toneOf(r)).toBe('red')
    expect(actionLabels(r)).toEqual({ accept: 'trash it', decline: 'keep it' })
    expect(describeChange(row('parked', { op: 'delete_doc', title: 'One', doc_count: 1 })).headline).toBe(
      'alice wants to trash “One” (1 doc)',
    )
  })
  it('block ops keep the editor wording', () => {
    expect(actionLabels(row('parked', { op: 'insert', block_id: 'n', content: 'x' }))).toEqual({
      accept: 'apply',
      decline: 'discard',
    })
    expect(actionLabels(row('review', { op: 'replace', target: 'b', content: 'x' }))).toEqual({
      accept: 'keep',
      decline: 'revert',
    })
  })
})
