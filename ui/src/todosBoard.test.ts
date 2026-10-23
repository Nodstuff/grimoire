// The To-dos screen's pure seams: grouping (Overdue / Today / Upcoming /
// Someday / Done) merged from each list's day and /api/todo/due, the lead
// line, the live chip (the daemon's parse, shown short), keys, reorder
// targets, swipe, and the write bodies every action posts.
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { TodoDay, TodoItem } from './todo'
import {
  boardLead,
  buildBoard,
  carryOverWrites,
  filterParam,
  groupOf,
  itemKey,
  openCount,
  parsedChip,
  reorderBefore,
  rowKey,
  snoozeDueAt,
  swipeAction,
  visibleOrder,
  writeBody,
  type BoardItem,
  type DueItem,
} from './todosBoard'
import { postTodo } from './screens/todoHooks'

// Fri 23 Oct 2026, 12:00 local
const NOW = new Date(2026, 9, 23, 12, 0)
const TODAY = '2026-10-23'
const DOC = 'doc-a'
const DOC_W = 'doc-w'

const item = (id: string, text: string, extra: Partial<TodoItem> = {}): TodoItem => ({ id, text, done: false, due_soon: false, ...extra })
const day = (doc: string, items: TodoItem[], ws: string | null = null): TodoDay => ({
  doc_id: doc,
  date: TODAY,
  today: TODAY,
  items,
  carried: 0,
  prev_date: null,
  epoch: 1,
  workspace_id: ws,
})
const at = (h: number, m = 0, d = 23) => new Date(2026, 9, d, h, m).toISOString()

describe('grouping', () => {
  it('sorts an item by its deadline at now, in this zone', () => {
    expect(groupOf({ done: true }, NOW)).toBe('done')
    expect(groupOf({ done: false }, NOW)).toBe('someday')
    expect(groupOf({ done: false, deadline: '2026-10-22' }, NOW)).toBe('overdue')
    expect(groupOf({ done: false, deadline: TODAY }, NOW)).toBe('today')
    expect(groupOf({ done: false, due_at: at(9) }, NOW)).toBe('overdue')
    expect(groupOf({ done: false, due_at: at(15) }, NOW)).toBe('today')
    expect(groupOf({ done: false, due_at: at(9, 0, 24) }, NOW)).toBe('upcoming')
    expect(groupOf({ done: false, deadline: '2026-11-02' }, NOW)).toBe('upcoming')
  })

  it('merges each list’s today with /due’s other days; moved-on lines are left out', () => {
    const lists = [
      {
        day: day(DOC, [
          item('0-a', 'renew passport', { deadline: TODAY }),
          item('1-b', 'call the bank'),
          item('2-c', 'ship it', { done: true }),
          item('3-d', 'moved on', { carried: true }),
          item('4-e', 'late one', { due_at: at(9) }),
          item('5-f', 'pick up cable'),
        ]),
        workspaceId: null,
      },
      { day: day(DOC_W, [item('0-w', 'work thing', { due_at: at(16) })], 'w1'), workspaceId: 'w1' },
    ]
    const due: DueItem[] = [
      // already on the board (today's day of DOC): not doubled
      { date: TODAY, id: '0-a', text: 'renew passport', deadline: TODAY, doc_id: DOC, workspace_id: null },
      // an agent's line under a later day
      { date: '2026-10-30', id: '0-z', text: 'book flights', deadline: '2026-10-30', doc_id: DOC, workspace_id: null },
      { date: '2026-10-25', id: '0-y', text: 'tomorrow-ish', due_at: at(9, 0, 24), doc_id: DOC_W, workspace_id: 'w1' },
    ]
    const b = buildBoard(lists, due, NOW)
    const texts = (xs: BoardItem[]) => xs.map((x) => x.text)
    expect(texts(b.overdue)).toEqual(['late one'])
    // soonest first: an all-day deadline sorts at the start of its day
    expect(texts(b.today)).toEqual(['renew passport', 'work thing'])
    expect(texts(b.upcoming)).toEqual(['tomorrow-ish', 'book flights'])
    // the doc's order
    expect(texts(b.someday)).toEqual(['call the bank', 'pick up cable'])
    expect(texts(b.done)).toEqual(['ship it'])
    // where each one lives rides along for its writes
    const flights = b.upcoming[1]
    expect(flights).toMatchObject({ docId: DOC, date: '2026-10-30', workspaceId: null, key: itemKey(DOC, '2026-10-30', '0-z') })
    expect(b.today[1].workspaceId).toBe('w1')
    expect(openCount(b)).toBe(7)
    expect(visibleOrder(b, false).map((x) => x.text)).toEqual(['late one', 'renew passport', 'work thing', 'tomorrow-ish', 'book flights', 'call the bank', 'pick up cable'])
    expect(visibleOrder(b, true).at(-1)!.text).toBe('ship it')
  })

  it('a list with no doc yet adds nothing', () => {
    const b = buildBoard([{ day: { ...day('', [item('0-a', 'x')]) }, workspaceId: null }], [], NOW)
    expect(openCount(b)).toBe(0)
  })

  it('the lead line counts what matters, zeros left out', () => {
    const lists = [{ day: day(DOC, [item('0-a', 'a', { deadline: '2026-10-20' }), item('1-b', 'b', { deadline: TODAY }), item('2-c', 'c'), item('3-d', 'd', { done: true })]), workspaceId: null }]
    expect(boardLead(buildBoard(lists, [], NOW))).toBe('3 open · 1 overdue · 1 due today · 1 done today')
    expect(boardLead(buildBoard([{ day: day(DOC, [item('0-a', 'a')]), workspaceId: null }], [], NOW))).toBe('1 open')
    expect(boardLead(buildBoard([{ day: day(DOC, [item('0-a', 'a', { done: true })]), workspaceId: null }], [], NOW))).toBe('All done · 1 done today')
    expect(boardLead(buildBoard([], [], NOW))).toMatch(/Nothing to do/)
  })
})

describe('the live chip (the daemon’s reading, shown short)', () => {
  it('“due fri 3pm” → Fri 15:00; days near and far; warnings as themselves', () => {
    expect(parsedChip(null, TODAY)).toBeNull()
    expect(parsedChip({ deadline: null, due_at: null }, TODAY)).toBeNull()
    expect(parsedChip({ deadline: null, due_at: at(15, 0, 23) }, TODAY)).toEqual({ text: 'Today 15:00', warn: false })
    expect(parsedChip({ deadline: '2026-10-24' }, TODAY)).toEqual({ text: 'Tomorrow', warn: false })
    const fri = new Date(2026, 9, 30).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
    expect(parsedChip({ deadline: '2026-10-30' }, TODAY)!.text).toBe(fri)
    const wed = new Date(2026, 9, 28).toLocaleDateString(undefined, { weekday: 'short' })
    expect(parsedChip({ deadline: null, due_at: at(9, 30, 28) }, TODAY)).toEqual({ text: `${wed} 09:30`, warn: false })
    expect(parsedChip({ deadline: null, warning: "couldn't read that date" }, TODAY)).toEqual({ text: "couldn't read that date", warn: true })
  })
})

describe('keys', () => {
  const k = (key: string, more: Record<string, unknown> = {}) => rowKey({ key, ...more })
  it('J/K X E Enter ⌫ N, ⌥J/⌥K reorder; ⌘ chords and shift are not ours', () => {
    expect(k('j')).toBe('next')
    expect(k('ArrowDown')).toBe('next')
    expect(k('k')).toBe('prev')
    expect(k('x')).toBe('toggle')
    expect(k(' ')).toBe('toggle')
    expect(k('e')).toBe('edit')
    expect(k('Enter')).toBe('expand')
    expect(k('Backspace')).toBe('remove')
    expect(k('Delete')).toBe('remove')
    expect(k('n')).toBe('add')
    // ⌥ makes e.key a symbol on a Mac: the physical key decides
    expect(k('∆', { altKey: true, code: 'KeyJ' })).toBe('moveDown')
    expect(k('˚', { altKey: true, code: 'KeyK' })).toBe('moveUp')
    expect(k('ArrowUp', { altKey: true })).toBe('moveUp')
    expect(k('k', { metaKey: true })).toBeNull()
    expect(k('X', { shiftKey: true })).toBeNull()
    expect(k('q')).toBeNull()
  })
})

describe('reorder', () => {
  const g = (specs: [string, string][]): BoardItem[] =>
    specs.map(([id, doc]) => ({ ...item(id, id), key: itemKey(doc, TODAY, id), docId: doc, date: TODAY, workspaceId: null }))
  it('answers the next item of the same list as `before`, null for last', () => {
    const grp = g([['a', DOC], ['b', DOC], ['c', DOC]])
    expect(reorderBefore(grp, grp[2].key, 0)).toEqual({ before: 'a' })
    expect(reorderBefore(grp, grp[0].key, 1)).toEqual({ before: 'c' })
    expect(reorderBefore(grp, grp[0].key, 2)).toEqual({ before: null })
    expect(reorderBefore(grp, grp[1].key, 1)).toBeNull()
    expect(reorderBefore(grp, 'nope', 0)).toBeNull()
  })
  it('skips other lists’ rows: a move is within its own doc and day', () => {
    const grp = g([['a', DOC], ['x', DOC_W], ['b', DOC]])
    // a below x: still before b in its own list, which is where it is now
    expect(reorderBefore(grp, grp[0].key, 1)).toBeNull()
    expect(reorderBefore(grp, grp[0].key, 2)).toEqual({ before: null })
    expect(reorderBefore(grp, grp[2].key, 0)).toEqual({ before: 'a' })
  })
})

describe('writes', () => {
  afterEach(() => vi.unstubAllGlobals())
  it('every write names the list (workspace), the day and the item', () => {
    const root = { workspaceId: null, date: TODAY, id: '1-b' }
    expect(writeBody(root, { done: true })).toEqual({ date: TODAY, item_id: '1-b', done: true })
    expect(writeBody({ ...root, workspaceId: 'w1', date: '2026-10-30' })).toEqual({ workspace: 'w1', date: '2026-10-30', item_id: '1-b' })
    expect(filterParam('all')).toBeNull()
    expect(filterParam('w1')).toBe('w1')
  })
  it('snooze is tomorrow 09:00 here, as an instant', () => {
    expect(new Date(snoozeDueAt(NOW)).getTime()).toBe(new Date(2026, 9, 24, 9, 0).getTime())
    // the last day of a month rolls over
    expect(new Date(snoozeDueAt(new Date(2026, 9, 31, 22))).getTime()).toBe(new Date(2026, 10, 1, 9, 0).getTime())
  })
  it('moving between workspaces carries the deadline (timed wins) and the note', () => {
    const a = { workspace: 'w2', date: TODAY, item_id: '3-x' }
    expect(carryOverWrites({ due_at: at(15), deadline: TODAY, note: 'n' }, a)).toEqual([
      ['/api/todo/deadline', { ...a, due_at: at(15) }],
      ['/api/todo/note', { ...a, note: 'n' }],
    ])
    expect(carryOverWrites({ deadline: TODAY }, a)).toEqual([['/api/todo/deadline', { ...a, deadline: TODAY }]])
    expect(carryOverWrites({}, a)).toEqual([])
  })
  it('postTodo POSTs JSON with the client clock beside the body', async () => {
    const fetch = vi.fn(async () => new Response(JSON.stringify({ ok: true }), { headers: { 'Content-Type': 'application/json' } }))
    vi.stubGlobal('fetch', fetch)
    await postTodo('/api/todo/toggle', writeBody({ workspaceId: 'w1', date: TODAY, id: '0-a' }, { done: true }))
    const [path, init] = fetch.mock.calls[0] as unknown as [string, RequestInit]
    expect(path).toBe('/api/todo/toggle')
    expect(init.method).toBe('POST')
    const body = JSON.parse(String(init.body))
    expect(body).toMatchObject({ workspace: 'w1', date: TODAY, item_id: '0-a', done: true })
    expect(body.today).toMatch(/^\d{4}-\d{2}-\d{2}$/)
    expect(body.utc_offset).toMatch(/^[+-]\d{2}:\d{2}$/)
  })
})

describe('swipe', () => {
  it('right completes, left snoozes, short is nothing', () => {
    expect(swipeAction(90)).toBe('done')
    expect(swipeAction(-90)).toBe('snooze')
    expect(swipeAction(40)).toBeNull()
    expect(swipeAction(-79)).toBeNull()
  })
})
