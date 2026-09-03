// Pure seams for the Shares page: grouping/ordering of the shares we own, and
// the one-line status of a mirror someone shared with us.

import type { HubMembership, HubRole, MirrorRow, Share, ShareOffer } from './types'
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
  if (s.offered_to_petname) return `waiting for ${s.offered_to_petname}`
  return 'not yet joined'
}

/** One line for a share request card: who, what, grant. */
export function offerLine(o: Pick<ShareOffer, 'from_petname' | 'root_title' | 'permission'>): string {
  const grant = o.permission === 'propose' ? 'can propose edits' : 'view only'
  return `${o.from_petname} wants to share “${o.root_title}” with you · ${grant}`
}

/** Header-chip text combining review items and open share requests. */
export function chipText(reviewCount: number, offerCount: number): string | null {
  const parts: string[] = []
  if (reviewCount > 0) parts.push(`${reviewCount} to review`)
  if (offerCount > 0) parts.push(`${offerCount} share request${offerCount === 1 ? '' : 's'}`)
  return parts.length ? parts.join(' · ') : null
}

/** Title for a share row: the daemon's root_title when present, else the
 * caller's lookup, else a short id. */
export function shareTitle(s: Share, titleOf?: (docId: string) => string | undefined): string {
  return s.root_title ?? titleOf?.(s.root_doc) ?? s.root_doc.slice(0, 8)
}

export type MirrorStatus = { kind: 'ok' | 'behind' | 'failing' | 'never'; text: string }

/** "synced 12s ago" / "sync failing: <err>" / "never synced". A last_error
 * wins over a stale last_pulled_at — that is the row that used to read
 * "titles but no content". */
export function mirrorStatusLine(row: MirrorRow, now: number = Date.now()): MirrorStatus {
  const err = row.last_error?.trim()
  if (err) return { kind: 'failing', text: `sync failing: ${err}` }
  if (row.behind && row.behind > 0) {
    const n = row.behind
    return {
      kind: 'behind',
      text: `${n} doc${n === 1 ? '' : 's'} behind · synced ${row.last_pulled_at ? relTime(row.last_pulled_at, now) : 'never'}`,
    }
  }
  if (row.last_pulled_at) return { kind: 'ok', text: `up to date · synced ${relTime(row.last_pulled_at, now)}` }
  return { kind: 'never', text: 'never synced' }
}

/** Short, spaced fingerprint of a hex pubkey for display. */
export function shortFingerprint(pubkey: string): string {
  return (pubkey.slice(0, 16).match(/.{1,4}/g) ?? []).join(' ')
}

/** My standing at a hub, in words. */
export function hubStandingLine(role: HubRole, membership: HubMembership): string {
  if (membership === 'pending') return 'waiting for an admin to approve you'
  if (membership === 'ejected') return 'you were removed'
  return role === 'admin' ? 'you are an admin' : 'you are a member'
}

/** "published to Team: 2" */
export function publishedLine(hubName: string, n: number): string {
  return `published to ${hubName}: ${n}`
}

/** The banner line for a doc relayed through a hub: who really owns it. */
export function relayedLine(hubName: string, ownerName: string): string {
  return `⌂ ${hubName} · owned by ${ownerName} · read-only for now`
}
