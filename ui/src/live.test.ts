import { describe, expect, it } from 'vitest'
import {
  advanceEvents,
  INITIAL_CURSOR,
  liveEventIso,
  liveEventLine,
  liveEventVerb,
  mergeActivity,
  viewersWriteChip,
  type LiveEvent,
} from './live'
import type { ActivityItem } from './types'

function ev(seq: number, kind: LiveEvent['kind'] = 'live_started'): LiveEvent {
  return { seq, kind, doc_id: 'd', doc_title: 'Plan', from: 'alice', at: '2026-09-02T10:00:00Z' }
}

describe('advanceEvents', () => {
  it('baselines silently on the first response for old/history events', () => {
    // ev() timestamps are 2026-09-02T10:00Z; "now" is an hour later → history
    const now = Date.parse('2026-09-02T11:00:00Z')
    const r = advanceEvents(INITIAL_CURSOR, { next: 7, events: [ev(5), ev(6), ev(4, 'doc_added')] }, now)
    expect(r.fresh).toEqual([])
    expect(r.cursor).toEqual({ since: 7, baselined: true })
  })
  it('still surfaces a live_started from the last 90s on the baseline poll', () => {
    // the owner went live 20s before the app finished loading — that session
    // is almost certainly still live, so the join toast must not be swallowed
    const now = Date.parse('2026-09-02T10:00:20Z')
    const r = advanceEvents(
      INITIAL_CURSOR,
      { next: 3, events: [ev(1, 'doc_changed'), ev(2, 'doc_added'), ev(3)] },
      now,
    )
    expect(r.fresh.map((e) => e.seq)).toEqual([3]) // only the recent live_started
    expect(r.cursor).toEqual({ since: 3, baselined: true })
    // but a live_started older than the window is history
    const old = advanceEvents(INITIAL_CURSOR, { next: 3, events: [ev(3)] }, now + 100_000)
    expect(old.fresh).toEqual([])
  })
  it('surfaces only events past the cursor, in seq order, once', () => {
    const cur = { since: 7, baselined: true }
    const r = advanceEvents(cur, { next: 10, events: [ev(9), ev(8), ev(8), ev(7)] })
    expect(r.fresh.map((e) => e.seq)).toEqual([8, 9])
    expect(r.cursor.since).toBe(10)
  })
  it('never moves the cursor backwards', () => {
    const r = advanceEvents({ since: 10, baselined: true }, { next: 3, events: [] })
    expect(r.cursor.since).toBe(10)
    expect(r.fresh).toEqual([])
  })
  it('ignores a malformed response', () => {
    const cur = { since: 4, baselined: true }
    expect(advanceEvents(cur, null)).toEqual({ cursor: cur, fresh: [] })
    expect(advanceEvents(cur, {} as never)).toEqual({ cursor: cur, fresh: [] })
    // events not an array → treated as none
    expect(advanceEvents(cur, { next: 5, events: undefined as never }).fresh).toEqual([])
  })
})

describe('liveEventLine', () => {
  it('formats the clickable nudges', () => {
    expect(liveEventLine(ev(1, 'live_started'))).toBe('alice is live on “Plan” — click to join')
    expect(liveEventLine(ev(1, 'doc_added'))).toBe('alice added “Plan”')
  })
  it('is silent for doc_changed and unknown kinds', () => {
    expect(liveEventLine(ev(1, 'doc_changed'))).toBeNull()
    expect(liveEventLine(ev(1, 'something_new'))).toBeNull()
  })
  it('has a verb for every kind and passes unknown kinds through', () => {
    expect(liveEventVerb('live_started')).toBe('went live on')
    expect(liveEventVerb('doc_added')).toBe('added')
    expect(liveEventVerb('doc_changed')).toBe('changed')
    expect(liveEventVerb('x')).toBe('x')
  })
  it('normalises timestamps', () => {
    expect(liveEventIso('2026-09-02T10:00:00Z')).toBe('2026-09-02T10:00:00Z')
    expect(liveEventIso(1_756_800_000)).toBe('2025-09-02T08:00:00.000Z')
    expect(liveEventIso(1_756_800_000_000)).toBe('2025-09-02T08:00:00.000Z')
    expect(liveEventIso(undefined)).toBe('')
  })
})

describe('mergeActivity', () => {
  const op = (id: string, at: string): ActivityItem => ({
    op_id: id,
    doc_id: 'd1',
    doc_title: 'Plan',
    principal: 'remote:x',
    principal_name: 'bob',
    op_type: 'replace',
    epoch: 1,
    created_at: at,
  })
  it('interleaves edits and nudges newest first with distinct keys', () => {
    const rows = mergeActivity(
      [op('a', '2026-09-02T10:00:00Z')],
      [
        { ...ev(3, 'doc_added'), at: '2026-09-02T11:00:00Z' },
        { ...ev(4, 'doc_changed'), at: '2026-09-02T09:00:00Z' },
      ],
    )
    expect(rows.map((r) => r.key)).toEqual(['ev:3', 'op:a', 'ev:4'])
    expect(rows[0]).toMatchObject({ who: 'alice', verb: 'added', doc_title: 'Plan' })
    expect(rows[1]).toMatchObject({ who: 'bob', verb: 'replace' })
    expect(rows[2].verb).toBe('changed')
  })
  it('caps the list', () => {
    const many = Array.from({ length: 30 }, (_, i) => op(`o${i}`, `2026-09-02T10:${String(i).padStart(2, '0')}:00Z`))
    expect(mergeActivity(many, [])).toHaveLength(20)
    expect(mergeActivity(many, [], 5)).toHaveLength(5)
  })
})

describe('viewersWriteChip', () => {
  it('maps the toggle state to its label', () => {
    expect(viewersWriteChip(true).label).toBe('👥 everyone can edit')
    expect(viewersWriteChip(true).title).toMatch(/watch only/)
    expect(viewersWriteChip(false).label).toBe('👁 watch only')
    expect(viewersWriteChip(false).title).toMatch(/everyone/)
  })
})

describe('share_offered event', () => {
  it('toasts a pointer to Share requests', () => {
    const line = liveEventLine({ seq: 9, kind: 'share_offered', doc_id: 'offer-1', doc_title: 'Plan', from: 'alice', at: '2026-09-03T10:00:00Z' })
    expect(line).toBe('alice wants to share “Plan” with you — see Share requests')
    expect(liveEventVerb('share_offered')).toBe('offered to share')
  })
})
