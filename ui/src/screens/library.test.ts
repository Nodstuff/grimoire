import { describe, expect, it } from 'vitest'
import type { QueueRow } from '../types'
import { pendingByDoc } from './Library'

function row(doc: string, kind: 'review' | 'parked', op: string, verdict: 'yellow' | 'red', proposer = 'keeper:API'): QueueRow {
  return {
    item: {
      annotation: { id: 'a', doc_id: doc, op_id: 'o', kind, status: 'open' },
      op: { id: 'o', kind: { op }, principal: 'p', base_epoch: 1, epoch_applied: null, verdict, confidence: 1, prior: null, source_refs: [] },
    },
    doc_title: 'T',
    proposer,
    current_content: null,
  }
}

describe('pendingByDoc', () => {
  it('counts changes waiting and parked ones per doc, naming who wants a trash', () => {
    const m = pendingByDoc([row('a', 'review', 'replace', 'yellow'), row('a', 'review', 'insert', 'yellow'), row('b', 'parked', 'delete_doc', 'red'), row('c', 'parked', 'replace', 'red')])
    expect(m.get('a')).toEqual({ review: 2, parked: 0, parkedTrash: null })
    expect(m.get('b')).toEqual({ review: 0, parked: 1, parkedTrash: 'keeper:API' })
    expect(m.get('c')?.parkedTrash).toBeNull()
    expect(m.has('d')).toBe(false)
  })
})
