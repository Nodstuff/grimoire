export interface Doc {
  id: string
  parent_id: string | null
  title: string
  review_policy: string | null
  current_epoch: number
  created_by: string
  status: 'draft' | 'in-review' | 'decided' | 'superseded' | null
  sort_key: string | null
  is_canvas?: boolean
  is_tended?: boolean
  /** true when this doc is a mirror tended by agents on the OWNER's side */
  owner_tended?: boolean
  /** present when this doc is a mirror synced from a remote owner */
  mirror_permission?: 'view' | 'propose'
  /** true when this doc is the root of an active share we own */
  is_shared?: boolean
}

/* ---------- federation (ADR 0002) ---------- */

export interface Contact {
  id: string
  pubkey: string
  petname: string
  principal: string
  verified: boolean
  revoked: boolean
  paired_at: string
}

export interface Share {
  id: string
  root_doc: string
  contact: string | null
  permission: 'view' | 'propose'
  state: 'offered' | 'active' | 'revoked'
  policy_override: string | null
  created_at: string
  /** review = proposals park (default); yellow = trusted, edits apply flagged;
   * green = maintainer, edits apply directly and the owner is notified */
  trust: ShareTrust
  /* newer daemons enrich GET /admin/shares; every field below is optional */
  root_title?: string
  doc_count?: number
  contact_petname?: string | null
  redeemed_at?: string | null
  /** invites v2: an unredeemed invite was offered over the wire to this contact */
  offered_to_petname?: string | null
}

/** Invites v2 — GET /admin/offers: a share someone offered us, awaiting our answer. */
export interface ShareOffer {
  id: string
  from_contact: string
  from_petname: string
  from_pubkey: string
  owner_node: string
  share_id: string
  root_title: string
  permission: 'view' | 'propose'
  state: 'open' | 'accepted' | 'declined' | 'expired'
  created_at: string
  expires_at: string
}

/** GET /admin/neighbours: a Grimoire visible on this network right now. */
export interface Neighbour {
  pubkey: string
  name: string | null
  seen_secs_ago: number
  contact_id: string | null
  contact_petname: string | null
  blocked: boolean
}

export type ShareTrust = 'review' | 'yellow' | 'green'

/** A share someone else granted us — GET /admin/mirrors. */
export interface MirrorRow {
  share_id: string
  owner_petname: string
  owner_pubkey: string
  permission: 'view' | 'propose'
  root_doc_id: string
  root_title: string
  doc_count?: number
  synced_epoch_max?: number
  /** docs whose owner epoch is past what we landed (paged / failed) */
  behind?: number
  last_pulled_at?: string | null
  last_error?: string | null
  owner_tended?: boolean
}

/** GET /api/profile — who this install is to its contacts. */
export interface Profile {
  name: string
  principal_id: string
  node_id?: string
  fingerprint?: string
  /** false = still the install default; the user has never chosen a name */
  confirmed: boolean
}

/** One maintainer-tier (green) edit applied directly by a remote principal —
 * the owner's notification feed (GET /api/activity). */
export interface ActivityItem {
  op_id: string
  doc_id: string
  doc_title: string
  principal: string
  principal_name: string
  op_type: string
  epoch: number
  created_at: string
}

export interface PendingJoin {
  id: string
  ticket: string
  attempts: number
  last_error: string | null
  created_at: string
}

export interface OutboundProposal {
  id: string
  doc_id: string
  share_id: string
  owner: string
  op_ids: string[]
  note: string
  state: 'pending' | 'accepted' | 'declined' | 'mixed'
  created_at: string
}

export interface DocFederation {
  mirror: {
    owner: string
    owner_petname: string
    permission: 'view' | 'propose'
    synced_epoch: number
    /** true when the owner has agents tending their copy */
    owner_tended?: boolean
  } | null
  shares: {
    id: string
    root_doc: string
    permission: 'view' | 'propose'
    state: string
    petname: string | null
    trust: ShareTrust
  }[]
  outbound: OutboundProposal[]
}

export interface Block {
  id: string
  doc_id: string
  parent_id: string | null
  order_key: string
  block_type: string
  content: string
  created_by: string
  epoch: number
  deleted: boolean
  refers_to: string | null
}

export interface BlockNode {
  block: Block
  children: BlockNode[]
}

export interface DocTree {
  doc: Doc
  roots: BlockNode[]
}

/** A block op as the gate stores it. Fields are per-variant and optional so
 * callers can read them defensively. */
export type OpKind = Record<string, unknown> & {
  op: 'insert' | 'replace' | 'delete' | 'move' | string
  target?: string
  content?: string
  block_id?: string
  parent_id?: string | null
  order_key?: string
  new_parent?: string | null
  new_order_key?: string
}

export interface QueueRow {
  item: {
    annotation: {
      id: string
      doc_id: string
      op_id: string
      kind: 'review' | 'parked'
      status: string
      resolved_by?: string | null
    }
    op: {
      id: string
      kind: OpKind
      principal: string
      base_epoch: number
      epoch_applied: number | null
      verdict: 'green' | 'yellow' | 'red' | null
      confidence: number | null
      prior: Block | null
      source_refs: string[]
    }
  }
  doc_title: string
  proposer: string
  current_content: string | null
}

export interface SearchHit {
  block: Block
  doc_title: string
}

export interface GardenerRun {
  id: string
  gardener: string
  gardener_name: string
  started_at: string
  status: string
  summary: string | null
  tokens_used: number | null
  tool_calls: number | null
}

/** An `{error, code?}` reply from the daemon. `code` is the machine-readable
 * cause when there is one (e.g. `stale_base`); branch on it, never on text. */
export class ApiError extends Error {
  code?: string
  constructor(message: string, code?: string) {
    super(message)
    this.name = 'ApiError'
    this.code = code
  }
}

/** The per-boot admin token (picked up from the URL at boot by main.tsx). */
export function adminToken(): string | null {
  try {
    return sessionStorage.getItem('grimoire.admin_token')
  } catch {
    return null
  }
}

const ADMIN_HEADER = 'X-Grimoire-Admin'

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  // gate-weakening surfaces live under /admin/ and need the token
  if (path.startsWith('/admin/')) {
    const token = adminToken()
    if (token) {
      const headers = new Headers(init?.headers)
      headers.set(ADMIN_HEADER, token)
      init = { ...init, headers }
    }
  }
  const r = await fetch(path, init)
  const j = await r.json().catch(() => null)
  if (r.status === 401 && path.startsWith('/admin/')) {
    throw new ApiError(
      'open Grimoire from the app (or add ?admin_token=… from ~/.grimoire/admin.token to the URL)',
      'admin_token',
    )
  }
  if (j && typeof j === 'object' && 'error' in j) {
    const code = 'code' in j && typeof j.code === 'string' ? j.code : undefined
    throw new ApiError(String(j.error), code)
  }
  if (j === null) throw new ApiError(`${r.status} ${r.statusText}`.trim())
  return j as T
}

export interface Principal {
  id: string
  kind: 'human' | 'agent' | 'remote'
  display_name: string
}

export interface HistoryRow {
  op: {
    id: string
    kind: Record<string, unknown> & { op: string }
    principal: string
    base_epoch: number
    epoch_applied: number | null
    verdict: 'green' | 'yellow' | 'red' | null
    confidence: number | null
    prior: Block | null
    source_refs: string[]
  }
  principal_name: string
  principal_kind: string
}
