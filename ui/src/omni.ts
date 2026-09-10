// Pure seams behind the omnibox (Omnibox.tsx): the prefix-mode parser, doc
// ranking (what ⌘O did, plus paths), the recent-docs list, and the mixer that
// lays docs / content hits / commands / the trailing ask row out as ONE keyed
// list in a fixed group order so the arrow keys walk it as a whole.

import type { Doc, SearchHit } from './types'

/* ---------- modes ---------- */

/** `mixed` is ⌘K; `docs` / `content` / `ask` are the ⌘O / ⌘P / ⌘/ aliases
 * (they only narrow which groups show — the same box, the same keys). `>`
 * and `?` typed into the box force `commands` / `ask` for that query. */
export type OmniMode = 'mixed' | 'docs' | 'content' | 'commands' | 'ask'

export interface ParsedQuery {
  mode: OmniMode
  /** the query without its prefix, trimmed */
  q: string
}

/** `>` → commands only, `?` → ask only, otherwise the mode the box opened
 * in. The prefix is stripped from the query; whitespace around it is fine. */
export function parseQuery(raw: string, base: OmniMode = 'mixed'): ParsedQuery {
  const s = raw.trimStart()
  if (s.startsWith('>')) return { mode: 'commands', q: s.slice(1).trim() }
  if (s.startsWith('?')) return { mode: 'ask', q: s.slice(1).trim() }
  return { mode: base, q: raw.trim() }
}

/* ---------- docs ---------- */

/** Subsequence match (what ⌘O used): every char of needle appears in hay in order. */
export function fuzzyMatch(needle: string, hay: string): boolean {
  let i = 0
  for (const c of hay) {
    if (c === needle[i]) i++
    if (i === needle.length) return true
  }
  return false
}

/** "Parent › Child › Title" for a doc (mirrors find_doc's breadcrumb). */
export function docPath(doc: Doc, byId: Map<string, Doc>): string {
  const parts = [doc.title]
  let cur = doc.parent_id
  let guard = 0
  while (cur && guard++ < 32) {
    const p = byId.get(cur)
    if (!p) break
    parts.unshift(p.title)
    cur = p.parent_id
  }
  return parts.join(' › ')
}

/** Docs matching `q`, best first: exact title > title prefix > title
 * substring > path substring > fuzzy title. Ties keep list order. */
export function rankDocs(docs: Doc[], q: string, limit = 5): Doc[] {
  const needle = q.trim().toLowerCase()
  if (!needle) return docs.slice(0, limit)
  const byId = new Map(docs.map((d) => [d.id, d]))
  const scored: { d: Doc; s: number }[] = []
  for (const d of docs) {
    const t = d.title.toLowerCase()
    let s = 0
    if (t === needle) s = 5
    else if (t.startsWith(needle)) s = 4
    else if (t.includes(needle)) s = 3
    else if (docPath(d, byId).toLowerCase().includes(needle)) s = 2
    else if (fuzzyMatch(needle, t)) s = 1
    if (s) scored.push({ d, s })
  }
  scored.sort((a, b) => b.s - a.s)
  return scored.slice(0, limit).map((x) => x.d)
}

/* ---------- recent docs ---------- */

export const RECENT_KEY = 'grimoire.recentDocs'
export const RECENT_MAX = 8

export function loadRecentIds(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY)
    const v = raw ? (JSON.parse(raw) as unknown) : []
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []
  } catch {
    return []
  }
}

/** `id` moved to the front, capped. Pure: returns the new list. */
export function pushRecentId(list: string[], id: string, max = RECENT_MAX): string[] {
  return [id, ...list.filter((x) => x !== id)].slice(0, max)
}

export function storeRecentIds(list: string[]) {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(list))
  } catch {
    // storage blocked: the list lives for this session only
  }
}

/** Recent ids resolved to live docs, in recency order (deleted ones drop). */
export function recentDocs(ids: string[], docs: Doc[]): Doc[] {
  const byId = new Map(docs.map((d) => [d.id, d]))
  return ids.map((id) => byId.get(id)).filter((d): d is Doc => !!d)
}

/* ---------- commands ---------- */

export interface OmniCommand {
  id: string
  label: string
  hint?: string
  /** keyboard shortcut shown on the selected row */
  keys?: string
  run: () => void
}

export function rankCommands(cmds: OmniCommand[], q: string, limit: number): OmniCommand[] {
  const needle = q.trim().toLowerCase()
  if (!needle) return cmds.slice(0, limit)
  const words = needle.split(/\s+/)
  const scored = cmds
    .map((c) => {
      const l = c.label.toLowerCase()
      const h = (c.hint ?? '').toLowerCase()
      let s = 0
      if (l.startsWith(needle)) s = 3
      else if (l.includes(needle)) s = 2
      else if (words.every((w) => l.includes(w) || h.includes(w))) s = 1
      return { c, s }
    })
    .filter((x) => x.s > 0)
  scored.sort((a, b) => b.s - a.s)
  return scored.slice(0, limit).map((x) => x.c)
}

/* ---------- the mixed list ---------- */

export type OmniGroup = 'recent' | 'docs' | 'content' | 'commands' | 'ask'

export type OmniRow =
  | { key: string; group: 'recent' | 'docs'; doc: Doc; path: string }
  | { key: string; group: 'content'; hit: SearchHit; crumb: string; snippet: string }
  | { key: string; group: 'commands'; cmd: OmniCommand }
  | { key: string; group: 'ask'; query: string }

export const GROUP_LABEL: Record<OmniGroup, string> = {
  recent: 'Recent',
  docs: 'Docs',
  content: 'Content',
  commands: 'Commands',
  ask: 'Ask the vault',
}

/** Group order in the list, whatever the mode. */
export const GROUP_ORDER: OmniGroup[] = ['recent', 'docs', 'content', 'commands', 'ask']

export const LIMITS = { recent: RECENT_MAX, docs: 5, content: 5, commands: 4, topCommands: 6, docsOnly: 12, contentOnly: 12, commandsOnly: 20 }

/** One line of a content hit: first non-empty line, hashes stripped, cut. */
export function snippetOf(content: string, max = 110): string {
  const line = content
    .split('\n')
    .map((l) => l.trim())
    .find((l) => l && !l.startsWith('---'))
  const s = (line ?? '').replace(/^#{1,6}\s+/, '')
  return s.length > max ? `${s.slice(0, max - 1)}…` : s
}

/** "Doc title › heading" — the search endpoint returns the block and its doc
 * title only, so the heading crumb is the hit itself when it IS a heading. */
export function crumbOf(hit: SearchHit): string {
  if (hit.block.block_type === 'heading') {
    return `${hit.doc_title} › ${hit.block.content.replace(/^#{1,6}\s+/, '').trim()}`
  }
  return hit.doc_title
}

export interface MixInput {
  mode: OmniMode
  q: string
  docs: Doc[]
  recentIds: string[]
  hits: SearchHit[]
  commands: OmniCommand[]
}

/** The rows the omnibox shows, in group order, each with a stable key. */
export function buildRows({ mode, q, docs, recentIds, hits, commands }: MixInput): OmniRow[] {
  const byId = new Map(docs.map((d) => [d.id, d]))
  const docRow = (group: 'recent' | 'docs') => (d: Doc): OmniRow => ({
    key: `${group}:${d.id}`,
    group,
    doc: d,
    path: docPath(d, byId),
  })
  const hitRow = (h: SearchHit): OmniRow => ({
    key: `content:${h.block.id}`,
    group: 'content',
    hit: h,
    crumb: crumbOf(h),
    snippet: snippetOf(h.block.content),
  })
  const cmdRow = (c: OmniCommand): OmniRow => ({ key: `cmd:${c.id}`, group: 'commands', cmd: c })
  const askRow = (): OmniRow => ({ key: 'ask', group: 'ask', query: q })

  const rows: OmniRow[] = []
  switch (mode) {
    case 'commands':
      rows.push(...rankCommands(commands, q, LIMITS.commandsOnly).map(cmdRow))
      return rows
    case 'ask':
      rows.push(askRow())
      return rows
    case 'docs':
      if (!q) rows.push(...recentDocs(recentIds, docs).map(docRow('recent')))
      rows.push(
        ...rankDocs(docs, q, LIMITS.docsOnly)
          .filter((d) => !rows.some((r) => r.group === 'recent' && r.doc.id === d.id))
          .map(docRow('docs')),
      )
      return rows
    case 'content':
      rows.push(...hits.slice(0, LIMITS.contentOnly).map(hitRow))
      if (q) rows.push(askRow())
      return rows
    case 'mixed':
    default:
      if (!q) {
        rows.push(...recentDocs(recentIds, docs).map(docRow('recent')))
        rows.push(...commands.slice(0, LIMITS.topCommands).map(cmdRow))
        return rows
      }
      rows.push(...rankDocs(docs, q, LIMITS.docs).map(docRow('docs')))
      rows.push(...hits.slice(0, LIMITS.content).map(hitRow))
      rows.push(...rankCommands(commands, q, LIMITS.commands).map(cmdRow))
      rows.push(askRow())
      return rows
  }
}

/** Index of the first row of each group — where the group label renders. */
export function groupStarts(rows: OmniRow[]): Set<number> {
  const seen = new Set<OmniGroup>()
  const starts = new Set<number>()
  rows.forEach((r, i) => {
    if (!seen.has(r.group)) {
      seen.add(r.group)
      starts.add(i)
    }
  })
  return starts
}

/** Where the selection lands after the rows change under it. The selected
 * row keeps its identity by KEY when it survives (the Content group landing
 * 120 ms after Docs must not move the highlight off the doc the user was on);
 * a vanished row falls back to the first row of the same group, then to the
 * same index clamped, then to 0. Pure — the root cause of the old "jumpy"
 * mixed mode was an index-based selection over a list whose groups arrive
 * at different times. */
export function stableSelection(prevRows: OmniRow[], prevSel: number, nextRows: OmniRow[]): number {
  if (nextRows.length === 0) return 0
  const prev = prevRows[prevSel]
  if (!prev) return Math.min(prevSel, nextRows.length - 1)
  const byKey = nextRows.findIndex((r) => r.key === prev.key)
  if (byKey !== -1) return byKey
  const sameGroup = nextRows.findIndex((r) => r.group === prev.group)
  if (sameGroup !== -1) {
    // same group, same offset within it where possible
    const prevGroupStart = prevRows.findIndex((r) => r.group === prev.group)
    const offset = Math.max(0, prevSel - prevGroupStart)
    let i = sameGroup
    while (offset > i - sameGroup && i + 1 < nextRows.length && nextRows[i + 1].group === prev.group) i++
    return i
  }
  return Math.min(prevSel, nextRows.length - 1)
}

/** Placeholder for the box in each opening mode. */
export function placeholderFor(mode: OmniMode): string {
  switch (mode) {
    case 'docs':
      return 'Open a doc…  (> commands · ? ask)'
    case 'content':
      return 'Search everything… typos fine  (> commands · ? ask)'
    case 'ask':
      return 'Ask your notes a question… every claim cites its block'
    case 'commands':
      return 'Type a command…'
    default:
      return 'Search docs and content, run a command, or ask…  (> commands · ? ask)'
  }
}
