// Pure seams behind the To-dos screen (screens/Todos.tsx). The lists live in
// the daemon (`todo.rs`: one To-do doc per workspace, one `## YYYY-MM-DD`
// heading per day, one block per item); this file merges what the screen
// reads — each list's today (`GET /api/todo`, carry-forward included) and
// every open deadlined item across days (`GET /api/todo/due`, the Apple
// app's board does the same) — into the screen's groups, and holds the
// small decisions the screen makes (lead line, snooze target, keys,
// reorder target, write bodies). No DOM, no fetch: vitest covers it.

import { dueDate, isOverdue, isoDate, type TodoDay, type TodoItem } from './todo'

/** One item of `GET /api/todo/due` (no `workspace`: every list). */
export interface DueItem {
  date: string
  id: string
  text: string
  deadline?: string
  due_at?: string
  legacy_time?: boolean
  note?: string
  carried_from?: string
  doc_id?: string
  workspace_id?: string | null
}

export interface DueList {
  doc_id: string | null
  docs?: { doc_id: string; workspace_id: string | null; epoch: number }[]
  items: DueItem[]
}

/** A list's day as the screen loaded it. */
export interface ListDay {
  day: TodoDay
  /** the list's workspace (null: the unlabelled root To-do, Unsorted's) */
  workspaceId: string | null
}

/** An item on the board: where it lives (list + day) rides along, because
 * every write is addressed `{workspace, date, item_id}`. */
export interface BoardItem extends TodoItem {
  /** unique on the board: doc + day + item id */
  key: string
  docId: string
  date: string
  workspaceId: string | null
}

export type GroupId = 'overdue' | 'today' | 'upcoming' | 'someday' | 'done'

export interface Board {
  overdue: BoardItem[]
  today: BoardItem[]
  upcoming: BoardItem[]
  someday: BoardItem[]
  done: BoardItem[]
}

export const GROUPS: { id: GroupId; label: string }[] = [
  { id: 'overdue', label: 'Overdue' },
  { id: 'today', label: 'Today' },
  { id: 'upcoming', label: 'Upcoming' },
  { id: 'someday', label: 'Someday' },
  { id: 'done', label: 'Done' },
]

export function itemKey(docId: string, date: string, id: string): string {
  return `${docId}|${date}|${id}`
}

/** Which open group an item belongs in at `now` (in this zone). */
export function groupOf(it: Pick<TodoItem, 'deadline' | 'due_at' | 'done' | 'carried'>, now: Date = new Date()): GroupId {
  if (it.done) return 'done'
  const day = dueDate(it)
  if (!day) return 'someday'
  if (isOverdue(it, now)) return 'overdue'
  return day === isoDate(now) ? 'today' : 'upcoming'
}

/** Sort key for a dated item: a timed one at its instant, an all-day one at
 * the start of its local day (the daemon's `/due` order). */
function dueMs(it: Pick<TodoItem, 'deadline' | 'due_at'>): number {
  if (it.due_at) return new Date(it.due_at).getTime()
  if (it.deadline) {
    const [y, m, d] = it.deadline.split('-').map(Number)
    return new Date(y, m - 1, d).getTime()
  }
  return Number.POSITIVE_INFINITY
}

/** Merge each list's today with every open deadlined item across days.
 * Today's items come from the day (done ones too); `/due` adds the dated
 * ones that sit under other days (an agent's line on a later day, a past
 * day never carried). `[>]` (moved on) items are left out: their copy is
 * where they went. Dated groups sort soonest first; Someday and Done keep
 * the doc's order (that order is what a drag changes). */
export function buildBoard(lists: ListDay[], due: DueItem[], now: Date = new Date()): Board {
  const b: Board = { overdue: [], today: [], upcoming: [], someday: [], done: [] }
  const seen = new Set<string>()
  const loadedDays = new Set<string>()
  for (const { day, workspaceId } of lists) {
    if (!day.doc_id) continue
    loadedDays.add(`${day.doc_id}|${day.date}`)
    for (const it of day.items) {
      if (it.carried) continue
      const key = itemKey(day.doc_id, day.date, it.id)
      if (seen.has(key)) continue
      seen.add(key)
      b[groupOf(it, now)].push({ ...it, key, docId: day.doc_id, date: day.date, workspaceId })
    }
  }
  for (const d of due) {
    if (!d.doc_id) continue
    // that day is on the board already, every item of it
    if (loadedDays.has(`${d.doc_id}|${d.date}`)) continue
    const key = itemKey(d.doc_id, d.date, d.id)
    if (seen.has(key)) continue
    seen.add(key)
    const it: BoardItem = {
      id: d.id,
      text: d.text,
      done: false,
      deadline: d.deadline,
      due_at: d.due_at,
      legacy_time: d.legacy_time,
      note: d.note,
      carried_from: d.carried_from,
      due_soon: false,
      key,
      docId: d.doc_id,
      date: d.date,
      workspaceId: d.workspace_id ?? null,
    }
    b[groupOf(it, now)].push(it)
  }
  const byDue = (x: BoardItem, y: BoardItem) => dueMs(x) - dueMs(y)
  b.overdue.sort(byDue)
  b.today.sort(byDue)
  b.upcoming.sort(byDue)
  return b
}

/** The rows in screen order (Done only when it is open). */
export function visibleOrder(b: Board, doneOpen: boolean): BoardItem[] {
  return [...b.overdue, ...b.today, ...b.upcoming, ...b.someday, ...(doneOpen ? b.done : [])]
}

export function openCount(b: Board): number {
  return b.overdue.length + b.today.length + b.upcoming.length + b.someday.length
}

/** The page's lead line: `6 open · 2 overdue · 1 due today · 3 done today`.
 * Zero parts are left out; nothing at all → a calm empty line. */
export function boardLead(b: Board): string {
  const open = openCount(b)
  if (open === 0 && b.done.length === 0) return 'Nothing to do yet. Add the first one above.'
  const out: string[] = [open === 0 ? 'All done' : `${open} open`]
  if (b.overdue.length) out.push(`${b.overdue.length} overdue`)
  if (b.today.length) out.push(`${b.today.length} due today`)
  if (b.done.length) out.push(`${b.done.length} done today`)
  return out.join(' · ')
}

/** Snooze = tomorrow 09:00 here (the Apple app's "Tomorrow 09:00",
 * `Due.defaultAlertHour`), as a UTC instant for `/api/todo/deadline`. */
export function snoozeDueAt(now: Date = new Date()): string {
  return new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 9, 0, 0, 0).toISOString()
}

/** The body every write carries besides the client clock: the list
 * (`workspace`, left out for the root list) and the item's day + id. */
export function writeBody(it: Pick<BoardItem, 'workspaceId' | 'date' | 'id'>, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return { ...(it.workspaceId ? { workspace: it.workspaceId } : {}), date: it.date, item_id: it.id, ...extra }
}

/** After adding a copy of `it` to another list (moving it between
 * workspaces), the writes that carry its deadline and note over to the new
 * item at `at` (`{workspace, date, item_id}` of the copy). */
export function carryOverWrites(it: Pick<BoardItem, 'deadline' | 'due_at' | 'note'>, at: Record<string, unknown>): [string, Record<string, unknown>][] {
  const out: [string, Record<string, unknown>][] = []
  if (it.due_at) out.push(['/api/todo/deadline', { ...at, due_at: it.due_at }])
  else if (it.deadline) out.push(['/api/todo/deadline', { ...at, deadline: it.deadline }])
  if (it.note) out.push(['/api/todo/note', { ...at, note: it.note }])
  return out
}

/** The `workspace` param for a filter: `all` reads every list. */
export function filterParam(filter: string): string | null {
  return filter === 'all' ? null : filter
}

export type RowKey = 'next' | 'prev' | 'toggle' | 'edit' | 'remove' | 'moveUp' | 'moveDown' | 'expand' | 'add'

/** The list's keys, when focus is not in a field: J/K (and ↓/↑) move, X
 * toggles, E edits, Enter shows the note, ⌫/Delete removes (after a
 * confirm), ⌥J/⌥K (⌥↓/⌥↑) reorder, N focuses the add field. */
export function rowKey(e: { key: string; code?: string; altKey?: boolean; metaKey?: boolean; ctrlKey?: boolean; shiftKey?: boolean }): RowKey | null {
  if (e.metaKey || e.ctrlKey) return null
  // ⌥ turns e.key into a symbol on a Mac: read the physical key
  const k = e.altKey ? (e.code === 'KeyJ' ? 'j' : e.code === 'KeyK' ? 'k' : e.key) : e.key
  if (e.altKey) {
    if (k === 'j' || k === 'ArrowDown') return 'moveDown'
    if (k === 'k' || k === 'ArrowUp') return 'moveUp'
    return null
  }
  if (e.shiftKey) return null
  switch (k) {
    case 'j':
    case 'ArrowDown':
      return 'next'
    case 'k':
    case 'ArrowUp':
      return 'prev'
    case 'x':
    case ' ':
      return 'toggle'
    case 'e':
      return 'edit'
    case 'Enter':
      return 'expand'
    case 'Backspace':
    case 'Delete':
      return 'remove'
    case 'n':
      return 'add'
  }
  return null
}

/** Where a reorder lands: the item `key` moved to position `to` of `group`
 * (its index after the move). Answers the `before` id for
 * `/api/todo/reorder` — the next item of the SAME list and day at or after
 * that position, or null (last in the day). Null result = nothing to do
 * (same place, or not an item of this group). */
export function reorderBefore(group: BoardItem[], key: string, to: number): { before: string | null } | null {
  const from = group.findIndex((x) => x.key === key)
  if (from < 0 || to === from) return null
  const it = group[from]
  const rest = group.filter((x) => x.key !== key)
  const at = Math.max(0, Math.min(rest.length, to))
  const next = rest.slice(at).find((x) => x.docId === it.docId && x.date === it.date)
  // already directly before that item (or already last): nothing moves
  const sameDay = group.filter((x) => x.docId === it.docId && x.date === it.date)
  const i = sameDay.findIndex((x) => x.key === key)
  const curNext = sameDay[i + 1]
  if ((next?.key ?? null) === (curNext?.key ?? null)) return null
  return { before: next ? next.id : null }
}

/** Swipe on a phone row: past ±80px right completes, left snoozes. */
export function swipeAction(dx: number, threshold = 80): 'done' | 'snooze' | null {
  if (dx >= threshold) return 'done'
  if (dx <= -threshold) return 'snooze'
  return null
}

/** The live chip beside the add field: what the daemon read off the draft's
 * `due …` phrase, short — `Fri 15:00`, `Today`, `Tomorrow 09:30`, `12 Nov`.
 * A warning shows as itself; nothing read → null. */
export function parsedChip(res: { deadline: string | null; due_at?: string | null; warning?: string } | null, today: string): { text: string; warn: boolean } | null {
  if (!res) return null
  if (res.warning) return { text: res.warning, warn: true }
  const date = res.deadline ?? (res.due_at ? isoDate(new Date(res.due_at)) : null)
  if (!date) return null
  const [y, m, d] = date.split('-').map(Number)
  const [ty, tm, td] = today.split('-').map(Number)
  const days = Math.round((new Date(y, m - 1, d).getTime() - new Date(ty, tm - 1, td).getTime()) / 86_400_000)
  const at = new Date(y, m - 1, d)
  const word =
    days === 0
      ? 'Today'
      : days === 1
        ? 'Tomorrow'
        : days > 1 && days <= 6
          ? at.toLocaleDateString(undefined, { weekday: 'short' })
          : at.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
  if (!res.due_at) return { text: word, warn: false }
  const t = new Date(res.due_at)
  const p = (n: number) => String(n).padStart(2, '0')
  return { text: `${word} ${p(t.getHours())}:${p(t.getMinutes())}`, warn: false }
}
