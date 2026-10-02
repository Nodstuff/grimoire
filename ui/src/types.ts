export interface Doc {
  id: string
  parent_id: string | null
  title: string
  review_policy: string | null
  current_epoch: number
  created_by: string
  status: 'draft' | 'in-review' | 'decided' | 'superseded' | null
  sort_key: string | null
  is_tended?: boolean
}

/** GET /api/profile — the owner's display name. */
export interface Profile {
  name: string
  principal_id: string
  /** false = still the install default; the user has never chosen a name */
  confirmed: boolean
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
    return sessionStorage.getItem('taisce.admin_token')
  } catch {
    return null
  }
}

const ADMIN_HEADER = 'Taisce-Admin'

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
      'open Taisce from the app (or add ?admin_token=… from ~/.grimoire/admin.token to the URL)',
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
