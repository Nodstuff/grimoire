// Pure seams behind the briefing home (Home.tsx): the "since you were here"
// merge of gardener runs, remote edits and agent-made docs, plus the daily
// log lookup for the "today's log →" link. The to-do list has its own seams
// in todo.ts (and its storage in the daemon, todo.rs). No DOM, no fetch —
// vitest covers all of it.

import type { ActivityItem, Doc, GardenerRun } from './types'

/* ---------- since you were here ---------- */

export interface NewDoc {
  id: string
  title: string
  created_by_name: string
  created_by_kind: string
  created_at: string
}

export type SinceItem =
  | { at: string; kind: 'run'; run: GardenerRun }
  | { at: string; kind: 'edit'; who: string; docId: string; docTitle: string }
  | { at: string; kind: 'newdoc'; who: string; docId: string; docTitle: string }

const DAY_MS = 24 * 60 * 60 * 1000

/** Merge runs, remote edits and agent-created docs into one newest-first list
 * of what happened after `since`. A missing stamp (first visit) means the
 * last 24 hours. A still-running run is always in (it is happening now). */
export function sinceItems(
  since: string | null,
  runs: GardenerRun[],
  activity: ActivityItem[],
  newDocs: NewDoc[],
  now: number = Date.now(),
  limit = 12,
): SinceItem[] {
  const floor = since ? new Date(since).getTime() : now - DAY_MS
  const items: SinceItem[] = []
  for (const r of runs) {
    if (r.status !== 'running' && new Date(r.started_at).getTime() < floor) continue
    items.push({ at: r.started_at, kind: 'run', run: r })
  }
  for (const a of activity) {
    if (new Date(a.created_at).getTime() < floor) continue
    items.push({ at: a.created_at, kind: 'edit', who: a.principal_name, docId: a.doc_id, docTitle: a.doc_title })
  }
  for (const d of newDocs) {
    if (new Date(d.created_at).getTime() < floor) continue
    items.push({ at: d.created_at, kind: 'newdoc', who: d.created_by_name, docId: d.id, docTitle: d.title })
  }
  items.sort((a, b) => new Date(b.at).getTime() - new Date(a.at).getTime())
  return items.slice(0, limit)
}

/** `21:34` for a timeline stamp on the same day, `Tue 21:34` otherwise. */
export function clockOf(iso: string, now: number = Date.now()): string {
  const t = new Date(iso)
  if (Number.isNaN(t.getTime())) return ''
  const time = t.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  if (new Date(now).toDateString() === t.toDateString()) return time
  return `${t.toLocaleDateString(undefined, { weekday: 'short' })} ${time}`
}

/* ---------- the daily doc ---------- */

export const DAILY_ROOT = 'Daily'

/** Local calendar date as the daily doc's title, `YYYY-MM-DD`. */
export function localDateTitle(d: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

export function findDailyRoot(docs: Doc[]): Doc | null {
  return docs.find((d) => d.parent_id === null && d.title === DAILY_ROOT) ?? null
}

/** The doc titled `title` anywhere under the root doc titled Daily. */
export function findDailyDoc(docs: Doc[], title: string): Doc | null {
  const root = findDailyRoot(docs)
  if (!root) return null
  const parentOf = new Map(docs.map((d) => [d.id, d.parent_id]))
  const underDaily = (id: string): boolean => {
    let cur: string | null | undefined = parentOf.get(id)
    while (cur) {
      if (cur === root.id) return true
      cur = parentOf.get(cur)
    }
    return false
  }
  return docs.find((d) => d.title === title && underDaily(d.id)) ?? null
}
