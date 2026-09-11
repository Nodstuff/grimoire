// Pure seams behind the home's To-do section (Todo.tsx). The list itself
// lives in the daemon (`todo.rs`: the To-do doc, one `## YYYY-MM-DD` heading
// per day, one block per item); this file is the client-side date arithmetic
// for the move menu, the deadline tone, and the header summary line.

export interface TodoItem {
  id: string
  text: string
  done: boolean
  /** `- [>]`: moved forward into a later day */
  carried?: boolean
  carried_from?: string
  deadline?: string
  note?: string
  overdue: boolean
  due_soon: boolean
}

export interface TodoDay {
  doc_id: string
  date: string
  today: string
  items: TodoItem[]
  carried: number
  prev_date: string | null
  epoch: number
  /** an add/edit whose trailing `due …` phrase could not be read */
  warning?: string
}

/** `GET /api/todo/parse?text=…`: what the daemon makes of a typed line. The
 * grammar (`due fri`, `by 12/9` day/month, `due 12 sep`, `due in 3 days`,
 * `due next mon`, ISO) lives ONLY in the daemon (`due.rs`); the UI just asks. */
export interface DueParse {
  text: string
  deadline: string | null
  warning?: string
}

/** Worth asking the daemon: the draft ends in `due …` / `by …` (≤ 3 words).
 * A trigger only — whether the phrase means anything is the daemon's call. */
export function hasDuePhrase(text: string): boolean {
  return /(^|\s)(due|by)\s+\S+(\s+\S+){0,2}\s*$/i.test(text)
}

/** The quiet hint under the add row: `→ Fri 12 Sep`, or the warning. */
export function previewLabel(res: DueParse | null, today: string): string {
  if (!res) return ''
  if (res.warning) return res.warning
  if (!res.deadline) return ''
  const rel = fmtDay(res.deadline, today)
  const abs = fromIso(res.deadline).toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' })
  return `→ ${rel === abs ? abs : `${rel} · ${abs}`}`
}

/** Local calendar date as `YYYY-MM-DD`. */
export function isoDate(d: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

function fromIso(date: string): Date {
  const [y, m, d] = date.split('-').map(Number)
  return new Date(y, m - 1, d)
}

/** `date` shifted by `n` calendar days (negative allowed). */
export function addDays(date: string, n: number): string {
  const d = fromIso(date)
  return isoDate(new Date(d.getFullYear(), d.getMonth(), d.getDate() + n))
}

/** The Monday after `date` (a Monday itself → the next one). */
export function nextMonday(date: string): string {
  const dow = fromIso(date).getDay() // 0 Sun … 6 Sat
  const ahead = dow === 0 ? 1 : 8 - dow
  return addDays(date, ahead)
}

/** Weekday + day for a date pill (`Thu 12 Sep`); `today` / `tomorrow` /
 * `yesterday` relative to `today`. */
export function fmtDay(date: string, today: string): string {
  if (date === today) return 'today'
  if (date === addDays(today, 1)) return 'tomorrow'
  if (date === addDays(today, -1)) return 'yesterday'
  return fromIso(date).toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' })
}

export type DeadlineTone = 'overdue' | 'soon' | 'later'

export function deadlineTone(it: Pick<TodoItem, 'overdue' | 'due_soon' | 'done'>): DeadlineTone {
  if (it.done) return 'later'
  if (it.overdue) return 'overdue'
  if (it.due_soon) return 'soon'
  return 'later'
}

/** `due 12 Sep` / `due Fri` (this week) / `due today` / `3d overdue` for the pill. */
export function fmtDeadline(deadline: string, today: string): string {
  const days = Math.round((fromIso(deadline).getTime() - fromIso(today).getTime()) / 86_400_000)
  if (days === 0) return 'due today'
  if (days === 1) return 'due tomorrow'
  if (days < 0) return `${-days}d overdue`
  if (days <= 6) return `due ${fromIso(deadline).toLocaleDateString(undefined, { weekday: 'short' })}`
  return `due ${fromIso(deadline).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })}`
}

/** Open (unchecked, not moved-forward) items — what still needs doing. */
export function openItems(items: TodoItem[]): TodoItem[] {
  return items.filter((i) => !i.done && !i.carried)
}

/** The header's one-line summary: `2 to-dos · 3 in Inbox · quiet since 21:34`.
 * Parts that are zero/unknown are left out; nothing at all → `all clear`. */
export function summaryLine(parts: { todos: number | null; inbox: number | null; since: string | null; now?: number }): string {
  const out: string[] = []
  if (parts.todos != null) out.push(parts.todos === 0 ? 'no to-dos' : `${parts.todos} to-do${parts.todos === 1 ? '' : 's'}`)
  if (parts.inbox) out.push(`${parts.inbox} in Inbox`)
  if (parts.since) {
    const t = new Date(parts.since)
    if (!Number.isNaN(t.getTime())) {
      const now = parts.now ?? Date.now()
      const sameDay = new Date(now).toDateString() === t.toDateString()
      const when = sameDay
        ? t.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
        : t.toLocaleDateString(undefined, { weekday: 'short', hour: '2-digit', minute: '2-digit' })
      out.push(`quiet since ${when}`)
    }
  }
  return out.length ? out.join(' · ') : 'all clear'
}
