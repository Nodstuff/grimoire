// The owner's side of share links for Settings › Share links (SERVER mode,
// ADR 0005; routes in crates/daemon/src/shares.rs): every link the person
// made, and the visitor comments on each (read, mark read, reply, delete).
// Creating a link lives with the doc (share.ts / SharePanel.tsx).

import { withCsrf } from './auth'
import type { Share } from './share'

export interface ShareComment {
  id: string
  parent_id: string | null
  /** the visitor's name; empty on the owner's replies */
  author: string
  is_owner: boolean
  body: string
  created_at: string
  /** the snapshot revision the comment was left on */
  revision: number
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, withCsrf({ ...init, headers: { 'content-type': 'application/json', ...(init?.headers ?? {}) } }))
  if (r.status === 204) return undefined as T
  const j = await r.json().catch(() => null)
  if (!r.ok) throw new Error(j && typeof j.error === 'string' ? j.error : `${r.status} ${r.statusText}`)
  return j as T
}

/** Every link the person made (all docs). */
export function listAllShares(): Promise<Share[]> {
  return call<{ shares: Share[] }>('/api/shares').then((r) => r.shares)
}

export function shareComments(id: string): Promise<ShareComment[]> {
  return call<{ comments: ShareComment[] }>(`/api/shares/${id}/comments`).then((r) => r.comments)
}

export function markCommentsRead(id: string): Promise<{ marked: number }> {
  return call<{ marked: number }>(`/api/shares/${id}/comments/read`, { method: 'POST', body: '{}' })
}

export function replyToComment(id: string, body: string, parentId: string | null): Promise<ShareComment> {
  return call<ShareComment>(`/api/shares/${id}/comments`, { method: 'POST', body: JSON.stringify({ body, parent_id: parentId }) })
}

export function deleteComment(id: string, cid: string): Promise<void> {
  return call<void>(`/api/shares/${id}/comments/${cid}`, { method: 'DELETE' })
}

/** "in 4 days", "today", "never", "expired". */
export function expiresLabel(expiresAt: string | null, now = Date.now()): string {
  if (!expiresAt) return 'never'
  const t = Date.parse(expiresAt)
  if (!Number.isFinite(t)) return expiresAt
  const ms = t - now
  if (ms <= 0) return 'expired'
  const days = Math.round(ms / 86_400_000)
  if (days < 1) {
    const hours = Math.max(1, Math.round(ms / 3_600_000))
    return `in ${hours} hour${hours === 1 ? '' : 's'}`
  }
  return `in ${days} day${days === 1 ? '' : 's'}`
}

/** Comments as threads: each top-level comment followed by its replies, oldest first. */
export function threadComments(cs: ShareComment[]): { comment: ShareComment; replies: ShareComment[] }[] {
  const byTime = [...cs].sort((a, b) => a.created_at.localeCompare(b.created_at))
  const ids = new Set(byTime.map((c) => c.id))
  const roots = byTime.filter((c) => !c.parent_id || !ids.has(c.parent_id))
  return roots.map((r) => ({ comment: r, replies: byTime.filter((c) => c.parent_id === r.id) }))
}
