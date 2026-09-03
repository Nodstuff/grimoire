// Pure helpers behind the file tree (DocTree.tsx): grouping the root into
// sections, the title filter, ancestor walks and the persisted open state.
// No React here so vitest covers the logic without a DOM.

import { compareSortKey } from './editor/diff'
import type { Doc } from './types'

/** System folders the daemon/gardeners own: pinned above everything else. */
export const PINNED_TITLES = new Set(['Daily', 'Answers', 'Claude Memory', 'Worktrees'])

export type Section = 'pinned' | 'folders' | 'notes'

export interface RootGroups {
  pinned: Doc[]
  folders: Doc[]
  notes: Doc[]
}

/** children by parent id in the server's order (sort_key, null last). */
export function childrenIndex(docs: Doc[]): Map<string | null, Doc[]> {
  const m = new Map<string | null, Doc[]>()
  for (const d of docs) {
    const k = d.parent_id
    if (!m.has(k)) m.set(k, [])
    m.get(k)!.push(d)
  }
  for (const kids of m.values()) kids.sort((a, b) => compareSortKey(a.sort_key, b.sort_key))
  return m
}

/** Root docs in three groups: Pinned = system folders by exact title plus
 * anything that came from a hub; Folders = the remaining roots with
 * children, alphabetical; Notes = root leaves in the server's order. */
export function groupRoot(docs: Doc[], children: Map<string | null, Doc[]>): RootGroups {
  const pinned: Doc[] = []
  const folders: Doc[] = []
  const notes: Doc[] = []
  for (const d of children.get(null) ?? []) {
    if (PINNED_TITLES.has(d.title) || d.from_hub) pinned.push(d)
    else if (children.get(d.id)?.length) folders.push(d)
    else notes.push(d)
  }
  const byTitle = (a: Doc, b: Doc) => a.title.localeCompare(b.title, undefined, { sensitivity: 'base' })
  pinned.sort((a, b) => {
    // system folders first, in their canonical order; hubs after, by name
    const ai = [...PINNED_TITLES].indexOf(a.title)
    const bi = [...PINNED_TITLES].indexOf(b.title)
    if (ai !== -1 || bi !== -1) return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi)
    return byTitle(a, b)
  })
  folders.sort(byTitle)
  return { pinned, folders, notes }
}

/** ids of every ancestor of `id` (nearest first), by parent pointer. */
export function ancestorsOf(id: string | null, byId: Map<string, Doc>): string[] {
  const out: string[] = []
  let cur = id ? byId.get(id) : undefined
  const seen = new Set<string>()
  while (cur && cur.parent_id && !seen.has(cur.parent_id)) {
    seen.add(cur.parent_id)
    out.push(cur.parent_id)
    cur = byId.get(cur.parent_id)
  }
  return out
}

export interface FilterResult {
  /** docs whose title matches */
  matches: Set<string>
  /** matches plus every ancestor — the rows that stay visible */
  visible: Set<string>
  /** ancestors of a match — folders forced open while filtering */
  expanded: Set<string>
}

/** Case-insensitive substring on title. An empty/blank query means "no
 * filter" and returns null so the caller can skip the work. */
export function filterDocs(docs: Doc[], query: string, byId: Map<string, Doc>): FilterResult | null {
  const q = query.trim().toLowerCase()
  if (!q) return null
  const matches = new Set<string>()
  const visible = new Set<string>()
  const expanded = new Set<string>()
  for (const d of docs) {
    if (!d.title.toLowerCase().includes(q)) continue
    matches.add(d.id)
    visible.add(d.id)
    for (const a of ancestorsOf(d.id, byId)) {
      visible.add(a)
      expanded.add(a)
    }
  }
  return { matches, visible, expanded }
}

/** total descendants under a folder (what a collapsed row shows on hover). */
export function descendantCount(id: string, children: Map<string | null, Doc[]>): number {
  let n = 0
  const stack = [id]
  const seen = new Set<string>()
  while (stack.length) {
    const cur = stack.pop()!
    for (const k of children.get(cur) ?? []) {
      if (seen.has(k.id)) continue
      seen.add(k.id)
      n++
      stack.push(k.id)
    }
  }
  return n
}

/* ---------- persisted open state ---------- */

export const TREE_STATE_KEY = 'grimoire.tree.v1'

export interface TreeState {
  /** collapsed sections (open is the default, so we store the exceptions) */
  closedSections: Section[]
  /** folder ids that are open */
  open: string[]
}

const EMPTY: TreeState = { closedSections: [], open: [] }

export function loadTreeState(storage: Pick<Storage, 'getItem'> | null = safeStorage()): TreeState {
  try {
    const raw = storage?.getItem(TREE_STATE_KEY)
    if (!raw) return EMPTY
    const v = JSON.parse(raw) as Partial<TreeState>
    return {
      closedSections: Array.isArray(v.closedSections)
        ? v.closedSections.filter((s): s is Section => s === 'pinned' || s === 'folders' || s === 'notes')
        : [],
      open: Array.isArray(v.open) ? v.open.filter((s): s is string => typeof s === 'string') : [],
    }
  } catch {
    return EMPTY
  }
}

export function saveTreeState(state: TreeState, storage: Pick<Storage, 'setItem'> | null = safeStorage()): void {
  try {
    storage?.setItem(TREE_STATE_KEY, JSON.stringify(state))
  } catch {
    // private mode / quota: the tree just forgets on reload
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage
  } catch {
    return null
  }
}
