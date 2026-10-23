// The Review screen's pure seams (canvas Review artboard): when a change was
// made, the word diff a block card shows, how items group (by doc, by agent),
// the sentence each item reads as, and its button captions. No DOM, no fetch.

import type { Doc, QueueRow } from '../types'
import type { Workspace } from '../workspaces'
import { isDocOp } from '../review'

/** Ops and comments are UUIDv7: the first 48 bits are the Unix ms they were
 * minted at. null for any other id. */
export function uuidTime(id: string): number | null {
  const hex = id.replace(/-/g, '')
  if (hex.length !== 32 || hex[12] !== '7') return null
  const ms = parseInt(hex.slice(0, 12), 16)
  return Number.isFinite(ms) ? ms : null
}

const pad = (n: number) => String(n).padStart(2, '0')
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/** "09:14" today, "Yesterday 21:40", "Oct 3 08:02" this year, else with the year. */
export function whenLabel(ms: number, now: number = Date.now()): string {
  const d = new Date(ms)
  const n = new Date(now)
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime()
  const diff = Math.round((day(n) - day(d)) / 86_400_000)
  if (diff <= 0) return hm
  if (diff === 1) return `Yesterday ${hm}`
  const md = `${MONTHS[d.getMonth()]} ${d.getDate()}`
  return d.getFullYear() === n.getFullYear() ? `${md} ${hm}` : `${md} ${d.getFullYear()} ${hm}`
}

/** words, whitespace runs, and punctuation on its own (so "tags" → "tags," marks only the comma) */
const TOKEN = /\s+|[^\s,.;:!?()"“”‘’]+|[,.;:!?()"“”‘’]/g

export type DiffPart = { kind: 'same' | 'del' | 'ins'; text: string }

/** A word-level diff of two texts (whitespace kept with its word). LCS over
 * tokens; long texts (> 600 tokens a side) fall back to whole-text del/ins
 * so a big block never stalls the page. */
export function wordDiff(a: string, b: string): DiffPart[] {
  const ta = a.match(TOKEN) ?? []
  const tb = b.match(TOKEN) ?? []
  if (ta.length > 600 || tb.length > 600) {
    return [
      ...(a ? [{ kind: 'del' as const, text: a }] : []),
      ...(b ? [{ kind: 'ins' as const, text: b }] : []),
    ]
  }
  const m = ta.length
  const n = tb.length
  const L: number[][] = Array.from({ length: m + 1 }, () => new Array<number>(n + 1).fill(0))
  for (let i = m - 1; i >= 0; i--)
    for (let j = n - 1; j >= 0; j--) L[i][j] = ta[i] === tb[j] ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1])
  const out: DiffPart[] = []
  const push = (kind: DiffPart['kind'], text: string) => {
    const last = out[out.length - 1]
    if (last && last.kind === kind) last.text += text
    else out.push({ kind, text })
  }
  let i = 0
  let j = 0
  while (i < m && j < n) {
    if (ta[i] === tb[j]) {
      push('same', ta[i])
      i++
      j++
    } else if (L[i + 1][j] >= L[i][j + 1]) push('del', ta[i++])
    else push('ins', tb[j++])
  }
  while (i < m) push('del', ta[i++])
  while (j < n) push('ins', tb[j++])
  return out
}

/** The side of a diff each column shows: the old one keeps same + del, the
 * new one same + ins. */
export function sideOf(parts: DiffPart[], side: 'old' | 'new'): DiffPart[] {
  return parts.filter((p) => p.kind === 'same' || p.kind === (side === 'old' ? 'del' : 'ins'))
}

export const isParked = (r: QueueRow) => r.item.annotation.kind === 'parked'
export const idOf = (r: QueueRow) => r.item.annotation.id

export interface DocGroup {
  docId: string
  title: string
  rows: QueueRow[]
}

/** Items grouped by doc, in queue order (first appearance). */
export function groupByDoc(rows: QueueRow[]): DocGroup[] {
  const m = new Map<string, DocGroup>()
  for (const r of rows) {
    const id = r.item.annotation.doc_id
    if (!m.has(id)) m.set(id, { docId: id, title: r.doc_title, rows: [] })
    m.get(id)!.rows.push(r)
  }
  return [...m.values()]
}

export interface AgentGroup {
  agent: string
  rows: QueueRow[]
  /** the flagged (already applied) ones: what "Keep all / Revert all" acts on */
  flagged: QueueRow[]
}

/** Items grouped by proposer. Parked items count but are never in `flagged`:
 * each needs its own yes. */
export function groupByAgent(rows: QueueRow[]): AgentGroup[] {
  const m = new Map<string, AgentGroup>()
  for (const r of rows) {
    if (!m.has(r.proposer)) m.set(r.proposer, { agent: r.proposer, rows: [], flagged: [] })
    const g = m.get(r.proposer)!
    g.rows.push(r)
    if (!isParked(r)) g.flagged.push(r)
  }
  return [...m.values()]
}

/** Where a doc lives, for a group header: "Work › Taisce" (workspace, then
 * the parent chain), "Unsorted" when it has no workspace. */
export function docLocation(docId: string, byId: Map<string, Doc>, workspaces: Workspace[]): { workspace: string | null; path: string } {
  const d = byId.get(docId)
  if (!d) return { workspace: null, path: '' }
  const parents: string[] = []
  let cur = d.parent_id
  let guard = 0
  while (cur && guard++ < 32) {
    const p = byId.get(cur)
    if (!p) break
    parents.unshift(p.title)
    cur = p.parent_id
  }
  const ws = d.workspace_id ? workspaces.find((w) => w.id === d.workspace_id) : null
  const wsName = ws ? ws.display_name || ws.name : d.workspace_id === undefined ? null : 'Unsorted'
  return { workspace: wsName, path: parents.join(' › ') }
}

/** A move_doc that keeps the parent only changes the doc's place among its
 * siblings: no "from → to" line to draw. */
export function isReorder(r: QueueRow): boolean {
  const k = r.item.op.kind
  return k.op === 'move_doc' && (k.from_parent ?? null) === (k.new_parent ?? null)
}

function str(v: unknown): string | null {
  return typeof v === 'string' ? v : null
}

/** The item's sentence after the agent's name, split so the key noun can be
 * bold: e.g. ["replaced a block", ""] or ["wants to move ", "Development", " to Trash"]. */
export function sentenceOf(r: QueueRow): { pre: string; strong?: string; post?: string } {
  const k = r.item.op.kind
  const parked = isParked(r)
  switch (k.op) {
    case 'replace':
      return { pre: parked ? 'wants to replace a block' : 'replaced a block' }
    case 'insert':
      return { pre: parked ? 'wants to add a block' : 'added a block' }
    case 'delete':
      return { pre: parked ? 'wants to delete a block' : 'deleted a block' }
    case 'move':
      return { pre: parked ? 'wants to move a block' : 'moved a block' }
    case 'rename_doc':
      return { pre: parked ? 'wants to rename this doc' : 'renamed this doc' }
    case 'move_doc': {
      if (isReorder(r)) return { pre: parked ? 'wants to reorder this doc among its siblings' : 'reordered this doc among its siblings' }
      const to = str(k.new_parent_title) ?? (k.new_parent == null ? 'the top level' : 'another doc')
      return { pre: parked ? 'wants to move this doc under ' : 'moved this doc under ', strong: to }
    }
    case 'set_status': {
      const to = str(k.status) ?? 'none'
      const from = str(k.from_status) ?? 'none'
      return { pre: parked ? 'wants to set status ' : 'set status ', strong: `${from} → ${to}` }
    }
    case 'delete_doc':
      return { pre: 'wants to move ', strong: str(k.title) || r.doc_title, post: ' to Trash' }
    default:
      return { pre: `${parked ? 'proposes' : 'made'} a ${k.op} change` }
  }
}

/** Button captions in the artboard's words. Accept on a flagged item keeps
 * what is already applied; decline writes the inverse. On a parked item
 * accept applies, decline discards. */
export function captions(r: QueueRow): { accept: string; decline: string; note: string } {
  const k = r.item.op.kind
  const parked = isParked(r)
  if (k.op === 'delete_doc') return { accept: 'Move to Trash', decline: 'Keep the doc', note: 'Nothing happens until you choose' }
  if (parked) return { accept: 'Apply', decline: 'Discard', note: 'Nothing happens until you choose' }
  const note = 'Reverting writes an inverse change, attributed to you'
  switch (k.op) {
    case 'insert':
      return { accept: 'Keep', decline: 'Remove it', note }
    case 'delete':
      return { accept: 'Keep deleted', decline: 'Restore it', note }
    case 'rename_doc':
      return { accept: 'Keep new name', decline: 'Rename back', note }
    case 'move_doc':
      return { accept: 'Keep', decline: isReorder(r) ? 'Put it back' : 'Move back', note }
    case 'set_status':
      return { accept: 'Keep', decline: str(k.from_status) ? 'Restore status' : 'Clear status', note }
    default:
      return { accept: 'Keep', decline: 'Revert', note }
  }
}

/** "Keep both" for two, "Keep all 3" past that. */
export function bulkWord(verb: 'Keep' | 'Revert', n: number): string {
  return n === 2 ? `${verb} both` : `${verb} all ${n}`
}

export { isDocOp }
