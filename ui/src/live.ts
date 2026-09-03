// Live-session seams (pure): owner→grantee nudges from GET /api/events and
// the "watch only" toggle on an owned doc's live session. Kept free of React
// so the baseline/de-dup rule and the chip labels are unit-testable.

import type { ActivityItem } from './types'

export type LiveEventKind = 'live_started' | 'doc_added' | 'doc_changed' | 'share_offered'

export interface LiveEvent {
  seq: number
  kind: LiveEventKind | string
  doc_id: string
  doc_title: string
  /** owner's petname */
  from: string
  /** ISO timestamp (or epoch seconds/millis from an older daemon) */
  at: string | number
}

export interface EventsResponse {
  next: number
  events: LiveEvent[]
}

/** Cursor for the events poll. `since` is what we send next; `baselined`
 * flips after the first successful response so events that were already
 * queued when the app launched never toast. */
export interface EventsCursor {
  since: number
  baselined: boolean
}

export const INITIAL_CURSOR: EventsCursor = { since: 0, baselined: false }

/** A `live_started` younger than this still toasts on the baseline poll: the
 * app may have loaded a few seconds after the owner went live (slow launch,
 * tunnel), and "X is live — join" is exactly the nudge you want then. */
export const BASELINE_LIVE_WINDOW_MS = 90_000

/** Advance the cursor and pick the events worth surfacing.
 * - first response: baseline — history is silent, EXCEPT a live_started from
 *   the last BASELINE_LIVE_WINDOW_MS (a session that is probably still live)
 * - later responses: everything with seq > since, de-duplicated by seq
 * - a malformed response leaves the cursor untouched */
export function advanceEvents(
  cur: EventsCursor,
  resp: Partial<EventsResponse> | null | undefined,
  now: number = Date.now(),
): { cursor: EventsCursor; fresh: LiveEvent[] } {
  if (!resp || typeof resp.next !== 'number') return { cursor: cur, fresh: [] }
  const events = Array.isArray(resp.events) ? resp.events : []
  const next = Math.max(cur.since, resp.next)
  if (!cur.baselined) {
    const recentLive = events
      .filter((ev) => ev?.kind === 'live_started' && typeof ev.seq === 'number')
      .filter((ev) => {
        const t = Date.parse(liveEventIso(ev.at))
        return Number.isFinite(t) && now - t >= 0 && now - t < BASELINE_LIVE_WINDOW_MS
      })
      .sort((a, b) => a.seq - b.seq)
    return { cursor: { since: next, baselined: true }, fresh: recentLive }
  }
  const seen = new Set<number>()
  const fresh: LiveEvent[] = []
  for (const ev of events) {
    if (typeof ev?.seq !== 'number' || ev.seq <= cur.since || seen.has(ev.seq)) continue
    seen.add(ev.seq)
    fresh.push(ev)
  }
  fresh.sort((a, b) => a.seq - b.seq)
  return { cursor: { since: next, baselined: true }, fresh }
}

/** Toast text for a nudge; null when the kind is silent (doc_changed) or unknown. */
export function liveEventLine(ev: LiveEvent): string | null {
  switch (ev.kind) {
    case 'live_started':
      return `${ev.from} is live on “${ev.doc_title}” — click to join`
    case 'doc_added':
      return `${ev.from} added “${ev.doc_title}”`
    case 'share_offered':
      // durable: the request sits under Share requests on the Shares page;
      // this toast only points there (doc_id is the offer id, not a doc)
      return `${ev.from} wants to share “${ev.doc_title}” with you — see Share requests`
    default:
      return null
  }
}

/** Verb for the Sharing screen's activity list. */
export function liveEventVerb(kind: string): string {
  switch (kind) {
    case 'live_started':
      return 'went live on'
    case 'doc_added':
      return 'added'
    case 'doc_changed':
      return 'changed'
    case 'share_offered':
      return 'offered to share'
    default:
      return kind
  }
}

/** Normalise `at` (ISO string, epoch seconds or millis) to an ISO string. */
export function liveEventIso(at: string | number | undefined): string {
  if (typeof at === 'number') {
    const ms = at < 1e12 ? at * 1000 : at
    return new Date(ms).toISOString()
  }
  return at ?? ''
}

/** One row of the Sharing screen's "recent activity": a maintainer edit to a
 * doc we own, or an owner nudge (live_started / doc_added / doc_changed) this
 * instance received. Newest first. */
export interface ActivityRow {
  key: string
  who: string
  verb: string
  doc_id: string
  doc_title: string
  /** ISO */
  at: string
}

export function mergeActivity(items: ActivityItem[], events: LiveEvent[], limit = 20): ActivityRow[] {
  const rows: ActivityRow[] = [
    ...items.map((a) => ({
      key: `op:${a.op_id}`,
      who: a.principal_name,
      verb: a.op_type || 'edited',
      doc_id: a.doc_id,
      doc_title: a.doc_title,
      at: a.created_at,
    })),
    ...events.map((e) => ({
      key: `ev:${e.seq}`,
      who: e.from,
      verb: liveEventVerb(e.kind),
      doc_id: e.doc_id,
      doc_title: e.doc_title,
      at: liveEventIso(e.at),
    })),
  ]
  rows.sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0))
  return rows.slice(0, limit)
}

/** The owner's "watch only" chip in the live banner. */
export function viewersWriteChip(enabled: boolean): { label: string; title: string } {
  return enabled
    ? { label: '👥 everyone can edit', title: 'click to make this session watch only' }
    : { label: '👁 watch only', title: 'click to let everyone in the share edit' }
}
