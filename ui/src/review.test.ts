import { describe, expect, it } from 'vitest'
import { buildHighlightMap, describeChange, targetBlockOf, toneOf } from './review'
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
