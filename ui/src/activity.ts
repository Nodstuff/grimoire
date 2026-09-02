// Owner notification feed: maintainer-tier (green) edits applied directly by
// remote principals. Newest first from GET /api/activity.

import type { ActivityItem } from './types'

export const ACTIVITY_SEEN_KEY = 'grimoire.activity.lastSeenOpId'

/** Items newer than `lastSeen` (list is newest-first).
 * - lastSeen null (first run): nothing is "new" — baseline silently.
 * - lastSeen present in the list: everything before it.
 * - lastSeen absent (more than a page happened, or it was pruned): all. */
export function unseenActivity(items: ActivityItem[], lastSeen: string | null): ActivityItem[] {
  if (items.length === 0) return []
  if (lastSeen === null) return []
  const idx = items.findIndex((it) => it.op_id === lastSeen)
  return idx === -1 ? items : items.slice(0, idx)
}

export function loadLastSeen(): string | null {
  try {
    return localStorage.getItem(ACTIVITY_SEEN_KEY)
  } catch {
    return null
  }
}

export function storeLastSeen(opId: string) {
  try {
    localStorage.setItem(ACTIVITY_SEEN_KEY, opId)
  } catch {
    // private mode / blocked storage — state still tracks it for this session
  }
}

export function activityLine(it: ActivityItem): string {
  return `${it.principal_name} edited “${it.doc_title}”`
}
