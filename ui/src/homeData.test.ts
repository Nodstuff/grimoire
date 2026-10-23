import { describe, expect, it } from 'vitest'
import {
  coverTone,
  dueTodayCount,
  duePill,
  greeting,
  groupTodos,
  leadLine,
  needsRows,
  needsSummary,
  previewText,
  rootOf,
  sinceClock,
  uuidV7Millis,
  whenShort,
} from './homeData'
import type { TodoItem } from './todo'
import type { Doc, QueueRow } from './types'

const at = (y: number, m: number, d: number, h = 12, min = 0) => new Date(y, m - 1, d, h, min)

describe('greeting', () => {
  it('follows the clock and uses the first name', () => {
    expect(greeting('Tom Meaney', at(2026, 10, 23, 9))).toBe('Good morning, Tom')
    expect(greeting('Tom', at(2026, 10, 23, 14))).toBe('Good afternoon, Tom')
    expect(greeting(null, at(2026, 10, 23, 20))).toBe('Good evening')
    expect(greeting('  ', at(2026, 10, 23, 12))).toBe('Good afternoon')
  })
})

describe('leadLine', () => {
  it('reads like the canvas', () => {
    expect(leadLine({ todos: 5, dueToday: 2, overdue: 0, review: 5, inbox: null, since: null })).toBe(
      '5 to-dos, 2 due today · 5 agent changes to look at',
    )
  })
  it('names overdue, inbox, and leaves zero parts out', () => {
    expect(leadLine({ todos: 1, dueToday: 0, overdue: 1, review: 0, inbox: 2, since: null })).toBe('1 to-do, 1 overdue · 2 in Inbox')
  })
  it('says all clear when nothing is known', () => {
    expect(leadLine({ todos: null, dueToday: 0, overdue: 0, review: null, inbox: null, since: null })).toBe('All clear')
    expect(leadLine({ todos: 0, dueToday: 0, overdue: 0, review: 0, inbox: 0, since: null })).toBe('no to-dos')
  })
  it('adds quiet since', () => {
    const now = at(2026, 10, 23, 15).getTime()
    const since = at(2026, 10, 23, 9, 5).toISOString()
    expect(leadLine({ todos: null, dueToday: 0, overdue: 0, review: null, inbox: null, since, now })).toMatch(/^quiet since \S+/)
  })
})

describe('sinceClock', () => {
  const now = at(2026, 10, 23, 15).getTime()
  it('drops the day today, says yesterday, else the weekday', () => {
    expect(sinceClock(at(2026, 10, 23, 9).toISOString(), now)).not.toMatch(/yesterday/)
    expect(sinceClock(at(2026, 10, 22, 22, 7).toISOString(), now)).toMatch(/yesterday$/)
    expect(sinceClock(at(2026, 10, 19, 22, 7).toISOString(), now)).toMatch(/^\S+ /)
    expect(sinceClock('nope', now)).toBe('')
  })
})

describe('uuidV7Millis', () => {
  it('reads the ms prefix of a v7 id', () => {
    expect(uuidV7Millis('01a169c4-da86-7143-943e-d2835e7de6e6')).toBe(0x01a169c4da86)
  })
  it('refuses other versions', () => {
    expect(uuidV7Millis('01a169c4-da86-4143-943e-d2835e7de6e6')).toBeNull()
    expect(uuidV7Millis('x')).toBeNull()
  })
})

const item = (p: Partial<TodoItem>): TodoItem => ({ id: p.id ?? Math.random().toString(36), text: 't', done: false, due_soon: false, ...p })

describe('groupTodos / dueTodayCount', () => {
  const now = at(2026, 10, 23, 14)
  const items = [
    item({ id: 'late', deadline: '2026-10-21' }),
    item({ id: 'timed-late', due_at: at(2026, 10, 23, 9).toISOString() }),
    item({ id: 'today', deadline: '2026-10-23' }),
    item({ id: 'timed', due_at: at(2026, 10, 23, 17).toISOString() }),
    item({ id: 'plain' }),
    item({ id: 'done', done: true }),
    item({ id: 'moved', carried: true }),
  ]
  it('splits overdue, today and done', () => {
    const g = groupTodos(items, now)
    expect(g.overdue.map((i) => i.id)).toEqual(['late', 'timed-late'])
    expect(g.today.map((i) => i.id)).toEqual(['today', 'timed', 'plain'])
    expect(g.done.map((i) => i.id)).toEqual(['done', 'moved'])
  })
  it('counts what is still due today', () => {
    expect(dueTodayCount(items, '2026-10-23', now)).toBe(2)
  })
})

describe('duePill', () => {
  const today = '2026-10-23'
  const now = at(2026, 10, 23, 14)
  it('red for overdue, naming the day', () => {
    const p = duePill(item({ deadline: '2026-10-21' }), today, now)!
    expect(p.tone).toBe('late')
    expect(p.text).toBe(new Date(2026, 9, 21).toLocaleDateString(undefined, { weekday: 'short' }))
  })
  it('red with the time when it passed today', () => {
    expect(duePill(item({ due_at: at(2026, 10, 23, 9).toISOString() }), today, now)).toEqual({ text: '09:00', tone: 'late' })
  })
  it('yellow for today', () => {
    expect(duePill(item({ deadline: today }), today, now)).toEqual({ text: 'today', tone: 'warn' })
    expect(duePill(item({ due_at: at(2026, 10, 23, 17).toISOString() }), today, now)).toEqual({ text: '17:00', tone: 'warn' })
  })
  it('quiet for later', () => {
    expect(duePill(item({ due_at: at(2026, 10, 24, 10).toISOString() }), today, now)).toEqual({ text: 'tomorrow 10:00', tone: 'plain' })
    expect(duePill(item({ deadline: '2026-12-01' }), today, now)!.tone).toBe('plain')
  })
  it('nothing without a deadline', () => {
    expect(duePill(item({}), today, now)).toBeNull()
  })
})

function row(p: { id: string; doc: string; title: string; who: string; kind: 'review' | 'parked'; op: Record<string, unknown> }): QueueRow {
  return {
    item: {
      annotation: { id: `a-${p.id}`, doc_id: p.doc, op_id: p.id, kind: p.kind, status: 'open' },
      op: { id: p.id, kind: p.op as never, principal: 'x', base_epoch: 0, epoch_applied: null, verdict: null, confidence: null, prior: null, source_refs: [] },
    },
    doc_title: p.title,
    proposer: p.who,
    current_content: null,
  }
}

describe('needsRows', () => {
  const t1 = '01a169c4-da80-7000-8000-000000000001'
  const t2 = '01a169c4-da90-7000-8000-000000000002'
  const t3 = '01a169c4-daa0-7000-8000-000000000003'
  const q = [
    row({ id: t1, doc: 'd1', title: 'Roadmap', who: 'claude:docs', kind: 'review', op: { op: 'replace', target: 'b1' } }),
    row({ id: t2, doc: 'd1', title: 'Roadmap', who: 'claude:docs', kind: 'review', op: { op: 'insert', block_id: 'b2' } }),
    row({ id: t3, doc: 'd2', title: 'Development', who: 'keeper:API', kind: 'parked', op: { op: 'delete_doc', title: 'Development', doc_count: 1 } }),
    row({ id: t1, doc: 'd3', title: 'Reading', who: 'filer', kind: 'review', op: { op: 'rename_doc', from_title: 'Reading notes', title: 'Reading' } }),
  ]
  it('groups block edits per agent and doc, newest first', () => {
    const rows = needsRows(q, () => 'blk')
    expect(rows.map((r) => `${r.who} ${r.before}${r.doc}${r.after}`)).toEqual([
      'keeper:API wants to trash Development',
      'claude:docs edited 2 blocks in Roadmap',
      'filer renamed “Reading notes” to Reading',
    ])
    expect(rows[1]).toMatchObject({ count: 2, verdict: 'flagged', docOp: false, blockId: 'blk', at: 0x01a169c4da90 })
    expect(rows[0]).toMatchObject({ verdict: 'parked', docOp: true })
  })
  it('summarises for the phone', () => {
    expect(needsSummary(q, 0)).toBe('3 flagged · 1 parked · from 3 agents')
    expect(needsSummary([], 2)).toBe('2 audit flags')
  })
})

describe('previewText', () => {
  it('strips markdown down to prose', () => {
    const md = '---\ntags: [a]\n---\n# Title\n\nThe daemon is **one** Rust binary.\n\n## Now\n\n- [x] a [link](http://x) and [[Wiki|w]]\n\n```js\ncode\n```\n> quoted'
    expect(previewText(md)).toBe('The daemon is one Rust binary. a link and Wiki · quoted')
  })
  it('caps with an ellipsis', () => {
    expect(previewText('word '.repeat(100), 20)).toMatch(/…$/)
    expect(previewText('word '.repeat(100), 20).length).toBeLessThanOrEqual(20)
  })
})

describe('rootOf', () => {
  const d = (id: string, parent_id: string | null): Doc => ({ id, parent_id, title: id, review_policy: null, current_epoch: 1, created_by: 'p', status: null, sort_key: null })
  it('walks to the top-level doc', () => {
    const docs = [d('a', null), d('b', 'a'), d('c', 'b')]
    const byId = new Map(docs.map((x) => [x.id, x]))
    expect(rootOf(docs[2], byId).id).toBe('a')
    expect(rootOf(docs[0], byId).id).toBe('a')
  })
})

describe('whenShort', () => {
  const now = at(2026, 10, 23, 15).getTime()
  it('minutes, clock, yesterday, weekday, date', () => {
    expect(whenShort(now - 12 * 60_000, now)).toBe('12 min ago')
    expect(whenShort(at(2026, 10, 23, 9, 14).getTime(), now)).not.toMatch(/ago|yesterday/)
    expect(whenShort(at(2026, 10, 22, 9).getTime(), now)).toBe('yesterday')
    expect(whenShort(at(2026, 10, 20, 9).getTime(), now)).toBe(new Date(2026, 9, 20).toLocaleDateString(undefined, { weekday: 'short' }))
  })
})

describe('coverTone', () => {
  it('is stable and in 1…6', () => {
    expect(coverTone('Piranesi')).toBe(coverTone('Piranesi'))
    for (const t of ['a', 'Thinking in Systems', 'A Psalm for the Wild-Built']) {
      expect(coverTone(t)).toBeGreaterThanOrEqual(1)
      expect(coverTone(t)).toBeLessThanOrEqual(6)
    }
  })
})
