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
  /** review = proposals park (default); yellow = trusted, edits apply flagged */
  trust: 'review' | 'yellow'
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
  } | null
  shares: {
    id: string
    root_doc: string
    permission: 'view' | 'propose'
    state: string
    petname: string | null
    trust: 'review' | 'yellow'
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

export interface QueueRow {
  item: {
    annotation: {
      id: string
      doc_id: string
      op_id: string
      kind: 'review' | 'parked'
      status: string
    }
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

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, init)
  const j = await r.json()
  if (j && typeof j === 'object' && 'error' in j) throw new Error(String(j.error))
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
