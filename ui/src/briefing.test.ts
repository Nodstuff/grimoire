import { describe, expect, it } from 'vitest'
import { clockOf, findDailyDoc, localDateTitle, sinceItems } from './briefing'
import type { ActivityItem, Doc, GardenerRun } from './types'

describe('sinceItems', () => {
  const now = new Date('2026-09-10T12:00:00Z').getTime()
  const runs: GardenerRun[] = [
    { id: 'r1', gardener: 'g', gardener_name: 'tagger', started_at: '2026-09-10T09:00:00Z', status: 'ok', summary: '2 yellow', tokens_used: null, tool_calls: null },
    { id: 'r0', gardener: 'g', gardener_name: 'tagger', started_at: '2026-09-08T09:00:00Z', status: 'ok', summary: null, tokens_used: null, tool_calls: null },
    { id: 'rr', gardener: 'g', gardener_name: 'auditor', started_at: '2026-09-07T09:00:00Z', status: 'running', summary: 'working… 4s · 1 tool call', tokens_used: null, tool_calls: null },
  ]
  const activity: ActivityItem[] = [
    { op_id: 'o1', doc_id: 'd', doc_title: 'Notes', principal: 'p', principal_name: 'alice', op_type: 'replace', epoch: 3, created_at: '2026-09-10T10:00:00Z' },
  ]
  const docs = [{ id: 'n', title: 'Answer', created_by_name: 'scribe', created_by_kind: 'agent', created_at: '2026-09-10T11:00:00Z' }]

  it('merges newest-first and drops anything before the stamp — except a run still going', () => {
    const items = sinceItems('2026-09-09T00:00:00Z', runs, activity, docs, now)
    expect(items.map((i) => i.kind)).toEqual(['newdoc', 'edit', 'run', 'run'])
    expect(items[2].kind === 'run' && items[2].run.id).toBe('r1')
    expect(items[3].kind === 'run' && items[3].run.status).toBe('running')
    expect(items[1].kind === 'edit' && items[1].who).toBe('alice')
    expect(items[0].kind === 'newdoc' && items[0].docTitle).toBe('Answer')
  })
  it('without a stamp shows the last 24h', () => {
    expect(sinceItems(null, runs.slice(0, 2), [], [], now)).toHaveLength(1)
    expect(sinceItems('2026-09-10T11:30:00Z', runs.slice(0, 2), activity, docs, now)).toHaveLength(0)
  })
  it('clock stamps carry the weekday when not today', () => {
    const today = new Date(2026, 8, 10, 21, 34).toISOString()
    const n = new Date(2026, 8, 10, 23, 0).getTime()
    expect(clockOf(today, n)).toMatch(/^\d{1,2}:34/)
    expect(clockOf(new Date(2026, 8, 8, 9, 5).toISOString(), n)).toMatch(/^\w{3} /)
    expect(clockOf('nope', n)).toBe('')
  })
})

describe('daily doc lookup', () => {
  it('formats local dates', () => {
    expect(localDateTitle(new Date(2026, 8, 10))).toBe('2026-09-10')
  })
  it('finds the titled doc only under the Daily root', () => {
    const doc = (id: string, title: string, parent_id: string | null): Doc => ({
      id,
      title,
      parent_id,
      review_policy: null,
      current_epoch: 0,
      created_by: 'p',
      status: null,
      sort_key: null,
    })
    const docs = [
      doc('daily', 'Daily', null),
      doc('sep', '2026-09', 'daily'),
      doc('t', '2026-09-10', 'sep'),
      doc('elsewhere', '2026-09-11', null),
    ]
    expect(findDailyDoc(docs, '2026-09-10')?.id).toBe('t')
    expect(findDailyDoc(docs, '2026-09-11')).toBeNull()
    expect(findDailyDoc([doc('x', '2026-09-10', null)], '2026-09-10')).toBeNull()
  })
})
