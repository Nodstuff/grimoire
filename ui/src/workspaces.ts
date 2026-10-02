// Workspaces in the web UI (ADR 0004): the switcher's labels and the tree
// filter. Pure, so vitest covers them; WorkspaceSwitcher.tsx is the view.

import type { Doc } from './types'

export type Role = 'owner' | 'editor' | 'viewer'

/** GET /api/workspaces → `workspaces[]`. Fields after `doc_count` arrived
 * with multi-user (ADR 0004); a pre-0004 daemon omits them. */
export interface Workspace {
  id: string
  name: string
  color: string | null
  icon: string | null
  sort_key: string | null
  doc_ids: string[]
  doc_count?: number
  owner_id?: string | null
  owner_name?: string | null
  /** `Name · Owner` when a workspace shared with you clashes by name */
  display_name?: string
  role?: Role
  shared?: boolean
}

export interface Member {
  user_id: string
  name: string
  role: Role
  added_at: string
}

/** The switcher's selection: everything, Unsorted, or one workspace id. */
export type WorkspaceSel = 'all' | 'unsorted' | string

/** What the switcher shows for a workspace: its display name (the owner's
 * name rides along when it clashes), marked when shared, read-only for a
 * viewer. */
export function switcherLabel(w: Workspace): string {
  let label = w.display_name || w.name
  if (w.shared) label += ' ·  shared'
  if (w.role === 'viewer') label += ' (read-only)'
  return label
}

/** Only the owner manages members. */
export function canManageMembers(w: Workspace | undefined): boolean {
  return !!w && (w.role === undefined || w.role === 'owner')
}

/** The docs the tree shows for a selection: those resolving to it, with a
 * parent outside the selection cut so the doc roots there. */
export function docsInWorkspace(docs: Doc[], sel: WorkspaceSel): Doc[] {
  if (sel === 'all') return docs
  const want = sel === 'unsorted' ? null : sel
  const keep = docs.filter((d) => (d.workspace_id ?? null) === want)
  const ids = new Set(keep.map((d) => d.id))
  return keep.map((d) => (d.parent_id && !ids.has(d.parent_id) ? { ...d, parent_id: null } : d))
}
