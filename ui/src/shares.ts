// Pure seams for the Shares page: grouping/ordering of the shares we own, and
// the one-line status of a mirror someone shared with us.

import type { MirrorRow, Share } from './types'
import { relTime } from './time'

export interface ShareGroups {
  active: Share[]
  offered: Share[]
  revoked: Share[]
}

const byNewest = (a: Share, b: Share) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : 0)

/** active first, then offered (invite minted, not yet redeemed), then revoked;
 * newest first inside each group. Unknown states land in `revoked` so nothing
 * silently disappears. */
export function groupShares(rows: Share[]): ShareGroups {
  const g: ShareGroups = { active: [], offered: [], revoked: [] }
  for (const s of rows) {
    if (s.state === 'active') g.active.push(s)
    else if (s.state === 'offered') g.offered.push(s)
    else g.revoked.push(s)
  }
  g.active.sort(byNewest)
  g.offered.sort(byNewest)
  g.revoked.sort(byNewest)
  return g
}

/** Who a share is with: the contact's petname, or "not yet joined" while the
 * invite is unredeemed. Falls back to the id-only shape of older daemons. */
export function shareWho(s: Share, petnameOf?: (contactId: string) => string | undefined): string {
  if (s.contact_petname) return s.contact_petname
  if (s.contact) return petnameOf?.(s.contact) ?? '?'
  return 'not yet joined'
}

/** Title for a share row: the daemon's root_title when present, else the
 * caller's lookup, else a short id. */
export function shareTitle(s: Share, titleOf?: (docId: string) => string | undefined): string {
  return s.root_title ?? titleOf?.(s.root_doc) ?? s.root_doc.slice(0, 8)
}

export type MirrorStatus = { kind: 'ok' | 'failing' | 'never'; text: string }

/** "synced 12s ago" / "sync failing: <err>" / "never synced". A last_error
 * wins over a stale last_pulled_at — that is the row that used to read
 * "titles but no content". */
export function mirrorStatusLine(row: MirrorRow, now: number = Date.now()): MirrorStatus {
  const err = row.last_error?.trim()
  if (err) return { kind: 'failing', text: `sync failing: ${err}` }
  if (row.last_pulled_at) return { kind: 'ok', text: `synced ${relTime(row.last_pulled_at, now)}` }
  return { kind: 'never', text: 'never synced' }
}

/** Short, spaced fingerprint of a hex pubkey for display. */
export function shortFingerprint(pubkey: string): string {
  return (pubkey.slice(0, 16).match(/.{1,4}/g) ?? []).join(' ')
}
