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

/** A workspace's colour: its own, else one of the six token colours by
 * position (stable for a given list order). */
export function workspaceColor(w: Pick<Workspace, 'color'>, index: number): string {
  if (w.color && /^#[0-9a-f]{3,8}$/i.test(w.color)) return w.color
  return `var(--ws-${(index % 6) + 1})`
}

/** The switcher button's two lines for a selection. */
export function workspaceSummary(sel: WorkspaceSel, list: Workspace[], docCount: number): { name: string; sub: string } {
  if (sel === 'all') {
    const names = ['Unsorted', ...list.map((w) => w.display_name || w.name)]
    return { name: 'All workspaces', sub: names.join(', ') }
  }
  if (sel === 'unsorted') return { name: 'Unsorted', sub: `Not in a workspace · ${docCount} docs` }
  const w = list.find((x) => x.id === sel)
  if (!w) return { name: 'All workspaces', sub: '' }
  const role = w.role ? w.role[0].toUpperCase() + w.role.slice(1) : 'Owner'
  const n = w.doc_count ?? w.doc_ids.length
  return { name: w.display_name || w.name, sub: `${role}${w.shared ? ' · shared' : ''} · ${n} doc${n === 1 ? '' : 's'}` }
}

/** The workspace a doc lives in: by its resolved id, else by membership. */
export function docWorkspace(workspaces: Workspace[], docId: string, workspaceId?: string | null): Workspace | null {
  return workspaceId ? (workspaces.find((w) => w.id === workspaceId) ?? null) : (workspaces.find((w) => w.doc_ids?.includes(docId)) ?? null)
}
