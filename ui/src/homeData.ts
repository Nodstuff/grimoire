// Pure seams behind the Home screen (screens/Home.tsx, canvas Main and
// Mobile-Home): the greeting, the lead line, the "Needs you" grouping of the
// review queue, the to-do groups and deadline pills, and the "Pick up where
// you left off" card text. No DOM, no fetch: vitest covers all of it.

import type { Doc, QueueRow } from './types'
import { addDays, dueTime, isOverdue, isoDate, type TodoItem } from './todo'

/* ---------- header ---------- */

/** `Good morning` before 12, `Good afternoon` before 18, else `Good evening`;
 * `, Tom` with the first word of the person's name when there is one. */
export function greeting(name: string | null | undefined, now: Date = new Date()): string {
  const h = now.getHours()
  const part = h < 12 ? 'morning' : h < 18 ? 'afternoon' : 'evening'
  const first = (name ?? '').trim().split(/\s+/)[0]
  return first ? `Good ${part}, ${first}` : `Good ${part}`
}

/** The lead line under the greeting:
 * `5 to-dos, 2 due today · 5 agent changes to look at · 2 in Inbox · quiet since 22:07`.
 * Unknown parts (null) are left out; nothing at all → `All clear`. */
export function leadLine(p: {
  todos: number | null
  dueToday: number
  overdue: number
  review: number | null
  inbox: number | null
  since: string | null
  now?: number
}): string {
  const out: string[] = []
  if (p.todos != null) {
    if (p.todos === 0) out.push('no to-dos')
    else {
      const extra: string[] = []
      if (p.overdue) extra.push(`${p.overdue} overdue`)
      if (p.dueToday) extra.push(`${p.dueToday} due today`)
      out.push([`${p.todos} to-do${p.todos === 1 ? '' : 's'}`, ...extra].join(', '))
    }
  }
  if (p.review) out.push(`${p.review} agent change${p.review === 1 ? '' : 's'} to look at`)
  if (p.inbox) out.push(`${p.inbox} in Inbox`)
  if (p.since) {
    const when = sinceClock(p.since, p.now)
    if (when) out.push(`quiet since ${when}`)
  }
  return out.length ? out.join(' · ') : 'All clear'
}

/** `22:07` on the same day, `22:07 yesterday`, else `Tue 22:07`. */
export function sinceClock(iso: string, now: number = Date.now()): string {
  const t = new Date(iso)
  if (Number.isNaN(t.getTime())) return ''
  const time = t.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  const today = isoDate(new Date(now))
  const day = isoDate(t)
  if (day === today) return time
  if (day === addDays(today, -1)) return `${time} yesterday`
  return `${t.toLocaleDateString(undefined, { weekday: 'short' })} ${time}`
}

/* ---------- ids carry time ---------- */

/** Milliseconds encoded in a UUIDv7 (ops, docs, annotations), else null. */
export function uuidV7Millis(id: string): number | null {
  const hex = id.replace(/-/g, '')
  if (hex.length !== 32 || hex[12] !== '7') return null
  const ms = parseInt(hex.slice(0, 12), 16)
  return Number.isFinite(ms) ? ms : null
}

/* ---------- to-dos ---------- */

export interface TodoGroups {
  overdue: TodoItem[]
  today: TodoItem[]
  /** checked off, or moved to a later day */
  done: TodoItem[]
}

/** Split the day's items the way the Today card reads: overdue first, then
 * the rest of today's open items, then what is done or moved on. */
export function groupTodos(items: TodoItem[], now: Date = new Date()): TodoGroups {
  const g: TodoGroups = { overdue: [], today: [], done: [] }
  for (const it of items) {
    if (it.done || it.carried) g.done.push(it)
    else if (isOverdue(it, now)) g.overdue.push(it)
    else g.today.push(it)
  }
  return g
}

/** Open items with a deadline on `today` that has not passed yet. */
export function dueTodayCount(items: TodoItem[], today: string, now: Date = new Date()): number {
  return items.filter((it) => !it.done && !it.carried && !isOverdue(it, now) && dueDay(it) === today).length
}

function dueDay(it: Pick<TodoItem, 'deadline' | 'due_at'>): string | undefined {
  if (it.deadline) return it.deadline
  return it.due_at ? isoDate(new Date(it.due_at)) : undefined
}

function fromIso(date: string): Date {
  const [y, m, d] = date.split('-').map(Number)
  return new Date(y, m - 1, d)
}

export type PillTone = 'late' | 'warn' | 'plain'

/** The deadline pill on a to-do row (canvas Main): overdue is red and names
 * when it was due (`Wed`, `09:00`), due today is yellow (`17:00`, `today`),
 * anything later is a quiet outline (`tomorrow 10:00`, `Fri`, `12 Sep`). */
export function duePill(
  it: Pick<TodoItem, 'deadline' | 'due_at' | 'done' | 'carried'>,
  today: string,
  now: Date = new Date(),
): { text: string; tone: PillTone } | null {
  const day = dueDay(it)
  if (!day) return null
  const time = it.due_at ? dueTime(it.due_at) : null
  const days = Math.round((fromIso(day).getTime() - fromIso(today).getTime()) / 86_400_000)
  const dayWord =
    days === 0
      ? 'today'
      : days === 1
        ? 'tomorrow'
        : days === -1
          ? 'yesterday'
          : Math.abs(days) <= 6
            ? fromIso(day).toLocaleDateString(undefined, { weekday: 'short' })
            : fromIso(day).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
  if (isOverdue(it, now)) return { text: days === 0 && time ? time : dayWord, tone: 'late' }
  if (it.done || it.carried) return { text: time && days === 0 ? time : dayWord, tone: 'plain' }
  if (days === 0) return { text: time ?? 'today', tone: 'warn' }
  return { text: time ? `${dayWord} ${time}` : dayWord, tone: 'plain' }
}

/* ---------- needs you (the review queue, grouped) ---------- */

export interface NeedsRow {
  key: string
  who: string
  /** the sentence after the name; `doc` is shown bold where `{doc}` sits */
  before: string
  doc: string
  after: string
  verdict: 'flagged' | 'parked'
  /** newest op in the group, ms (from the op's UUIDv7) */
  at: number | null
  docId: string
  /** a block op opens the doc with its review rail; a doc op the queue */
  blockId: string | null
  docOp: boolean
  count: number
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null
}

/** Group the queue by (proposer, doc, verdict) for block ops; every doc op
 * (rename, move, status, trash) is its own row. Newest first. */
export function needsRows(queue: QueueRow[], targetOf: (r: QueueRow) => string | null): NeedsRow[] {
  const rows: NeedsRow[] = []
  const groups = new Map<string, NeedsRow>()
  for (const r of queue) {
    const { annotation, op } = r.item
    const k = op.kind
    const verdict = annotation.kind === 'review' ? 'flagged' : 'parked'
    const at = uuidV7Millis(op.id)
    const who = r.proposer
    const title = r.doc_title
    const base = { who, verdict, at, docId: annotation.doc_id, count: 1 } as const
    if (k.op === 'rename_doc') {
      const from = str(k.from_title) ?? title
      const to = str(k.title) ?? title
      rows.push({ ...base, key: annotation.id, before: verdict === 'flagged' ? `renamed “${from}” to ` : `wants to rename “${from}” to `, doc: to, after: '', blockId: null, docOp: true })
      continue
    }
    if (k.op === 'delete_doc') {
      const n = typeof k.doc_count === 'number' ? k.doc_count : 1
      rows.push({ ...base, key: annotation.id, before: 'wants to trash ', doc: str(k.title) ?? title, after: n > 1 ? ` and ${n - 1} more` : '', blockId: null, docOp: true })
      continue
    }
    if (k.op === 'move_doc') {
      const to = str(k.new_parent_title) ?? 'the top level'
      rows.push({ ...base, key: annotation.id, before: verdict === 'flagged' ? 'moved ' : 'wants to move ', doc: title, after: ` under ${to}`, blockId: null, docOp: true })
      continue
    }
    if (k.op === 'set_status') {
      const to = str(k.status) ?? 'none'
      rows.push({ ...base, key: annotation.id, before: verdict === 'flagged' ? 'set ' : 'wants to set ', doc: title, after: ` to ${to}`, blockId: null, docOp: true })
      continue
    }
    const gk = `${who}\u0000${annotation.doc_id}\u0000${verdict}`
    const g = groups.get(gk)
    if (g) {
      g.count += 1
      if (at != null && (g.at == null || at > g.at)) g.at = at
      continue
    }
    const row: NeedsRow = { ...base, key: annotation.id, before: '', doc: title, after: '', blockId: targetOf(r), docOp: false }
    groups.set(gk, row)
    rows.push(row)
  }
  for (const g of groups.values()) {
    const n = g.count
    g.before = g.verdict === 'flagged' ? `edited ${n} block${n === 1 ? '' : 's'} in ` : `proposes ${n} change${n === 1 ? '' : 's'} to `
  }
  return rows.sort((a, b) => (b.at ?? 0) - (a.at ?? 0))
}

/** The phone's one-line card: `4 flagged · 1 parked · from 3 agents`. */
export function needsSummary(queue: QueueRow[], flags: number): string {
  const flagged = queue.filter((r) => r.item.annotation.kind === 'review').length
  const parked = queue.length - flagged
  const who = new Set(queue.map((r) => r.proposer)).size
  const out: string[] = []
  if (flagged) out.push(`${flagged} flagged`)
  if (parked) out.push(`${parked} parked`)
  if (flags) out.push(`${flags} audit flag${flags === 1 ? '' : 's'}`)
  if (who) out.push(`from ${who} agent${who === 1 ? '' : 's'}`)
  return out.join(' · ')
}

/* ---------- pick up where you left off ---------- */

/** The first few lines of prose of a doc's markdown, for the card's page:
 * headings, frontmatter, fences, list and quote markers and inline marks
 * stripped. */
export function previewText(markdown: string, max = 160): string {
  let md = markdown.replace(/^---\n[\s\S]*?\n---\n?/, '')
  md = md.replace(/```[\s\S]*?(```|$)/g, ' ')
  const lines = md
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !/^#{1,6}\s/.test(l) && !/^(\||-{3,}|\*{3,})/.test(l))
    .map((l) =>
      l
        .replace(/^([-*+]|\d+[.)])\s+(\[[ xX>]\]\s+)?/, '')
        .replace(/^>\s?/, '')
        .replace(/!\[[^\]]*\]\([^)]*\)/g, '')
        .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
        .replace(/\[\[([^\]|]*)(\|[^\]]*)?\]\]/g, '$1')
        .replace(/[*_`~]+/g, ''),
    )
    .filter(Boolean)
  // a line that ends without punctuation (a list item, a short line) gets a
  // middle dot before the next, so items do not run together
  const text = lines
    .reduce((acc, l) => (acc ? `${acc}${/[.!?:;,…]$/.test(acc) ? ' ' : ' · '}${l}` : l), '')
    .replace(/\s+/g, ' ')
    .trim()
  return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text
}

/** The top-level doc a doc lives under (itself when it is top level): the
 * card's "where" line when it has no workspace. */
export function rootOf(doc: Doc, byId: Map<string, Doc>): Doc {
  let cur = doc
  const seen = new Set<string>()
  while (cur.parent_id && !seen.has(cur.id)) {
    seen.add(cur.id)
    const p = byId.get(cur.parent_id)
    if (!p) break
    cur = p
  }
  return cur
}

/** `12 min ago` within the hour, `09:14` today, `yesterday`, `Tue` this
 * week, else `12 Sep` — the card's "when" (canvas Main). */
export function whenShort(ms: number, now: number = Date.now()): string {
  const diff = now - ms
  if (diff < 60_000) return 'just now'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} min ago`
  const t = new Date(ms)
  const today = isoDate(new Date(now))
  const day = isoDate(t)
  if (day === today) return t.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  if (day === addDays(today, -1)) return 'yesterday'
  const days = Math.round((fromIso(today).getTime() - fromIso(day).getTime()) / 86_400_000)
  if (days > 0 && days < 7) return t.toLocaleDateString(undefined, { weekday: 'short' })
  return t.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
}

/* ---------- currently reading ---------- */

/** A stable cover colour (1…6) for a book without cover art. */
export function coverTone(title: string): number {
  let h = 0
  for (const c of title) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return (h % 6) + 1
}
