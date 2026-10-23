// Pure seams behind the three palettes. Each has one job and never shows the
// others' rows:
//   ⌘O  Open     (OpenPalette.tsx)    doc titles + breadcrumb paths + places
//   ⌘K  Actions  (ActionsPalette.tsx) commands only
//   ⌘⇧F Search   (SearchPalette.tsx)  full-text block hits + the ask row
// Here: the fuzzy scorer all three rank with, the places list, the recent-docs
// list, the row builders, and the selection-stability rule.

import type { Doc, SearchHit } from './types'

/* ---------- fuzzy scoring ---------- */

/** Subsequence match: every char of needle appears in hay in order. */
export function fuzzyMatch(needle: string, hay: string): boolean {
  let i = 0
  if (!needle) return true
  for (const c of hay) {
    if (c === needle[i]) i++
    if (i === needle.length) return true
  }
  return false
}

const WORD_SPLIT = /[\s›/\-_.:,()[\]]+/

/** Restricted Damerau–Levenshtein distance, bailing out above `max`. */
export function editDistance(a: string, b: string, max = 2): number {
  if (Math.abs(a.length - b.length) > max) return max + 1
  const prev2 = new Array<number>(b.length + 1).fill(0)
  let prev = Array.from({ length: b.length + 1 }, (_, j) => j)
  for (let i = 1; i <= a.length; i++) {
    const cur = new Array<number>(b.length + 1)
    cur[0] = i
    let rowMin = cur[0]
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1
      cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost)
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) {
        cur[j] = Math.min(cur[j], prev2[j - 2] + 1)
      }
      rowMin = Math.min(rowMin, cur[j])
    }
    if (rowMin > max) return max + 1
    for (let j = 0; j <= b.length; j++) prev2[j] = prev[j]
    prev = cur
  }
  return prev[b.length]
}

/** A needle word of 4+ letters is within one edit of a hay word, or of that
 * word's prefix of the same length (so "boosk" finds "books", "archi" →
 * "architecture" stays a prefix, "arhci" → "archi…" is one swap). */
function typoWord(w: string, hayWords: string[]): boolean {
  if (w.length < 4) return hayWords.some((h) => h.startsWith(w))
  return hayWords.some(
    (h) =>
      h.startsWith(w) ||
      editDistance(w, h, 1) <= 1 ||
      (h.length > w.length && editDistance(w, h.slice(0, w.length), 1) <= 1),
  )
}

/** Subsequence score with word-start and run bonuses, or null. */
function subsequenceScore(needle: string, hay: string): number | null {
  let i = 0
  let score = 0
  let run = 0
  let last = -2
  for (let j = 0; j < hay.length && i < needle.length; j++) {
    if (hay[j] !== needle[i]) continue
    const wordStart = j === 0 || WORD_SPLIT.test(hay[j - 1])
    score += wordStart ? 12 : 1
    run = j === last + 1 ? run + 1 : 0
    score += run * 4
    last = j
    i++
  }
  if (i < needle.length) return null
  return score
}

/**
 * How well `query` matches `text`, higher is better, null = no match. Tiers
 * (each wins over every lower one): exact 1000 · prefix ~900 · substring at a
 * word start ~800 · substring ~700 · every query word starts a word (any
 * order) ~600 · subsequence 300–590 (word starts and runs score) · every word
 * within one typo 200. Case-insensitive; the query is trimmed.
 */
export function scoreText(query: string, text: string): number | null {
  const q = query.trim().toLowerCase()
  if (!q) return 0
  const t = text.toLowerCase()
  if (t === q) return 1000
  // shorter texts win inside a tier: "Books" over "Book club notes"
  const shortness = Math.max(0, 40 - (t.length - q.length)) / 2
  if (t.startsWith(q)) return 880 + shortness
  const at = t.indexOf(q)
  if (at > 0) return (WORD_SPLIT.test(t[at - 1]) ? 780 : 680) + shortness
  const words = q.split(/\s+/).filter(Boolean)
  const hayWords = t.split(WORD_SPLIT).filter(Boolean)
  if (words.length > 1 && words.every((w) => hayWords.some((h) => h.startsWith(w)))) return 580 + shortness
  const sub = subsequenceScore(q.replace(/\s+/g, ''), t)
  if (sub !== null) return 300 + Math.min(270, sub) + shortness / 2
  if (words.every((w) => typoWord(w, hayWords))) return 200 + shortness / 2
  return null
}

/* ---------- docs ---------- */

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

/** A path match counts for less than the same match on the title. */
export const PATH_WEIGHT = 0.7

/** A doc's score: its title, or its breadcrumb path at PATH_WEIGHT. */
export function scoreDoc(q: string, title: string, path: string): number | null {
  const t = scoreText(q, title)
  const p = path !== title ? scoreText(q, path) : null
  if (t === null && p === null) return null
  return Math.max(t ?? -1, p === null ? -1 : p * PATH_WEIGHT)
}

/* ---------- recent docs ---------- */

export const RECENT_KEY = 'grimoire.recentDocs' // pre-rename key: kept so stored state survives
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

/* ---------- places (⌘O) ---------- */

export type PlaceId = 'home' | 'books' | 'review' | 'trash' | 'graph' | 'runs' | 'settings' | 'ereaders'

export interface Place {
  id: PlaceId
  label: string
  hint: string
  keys?: string
  /** extra words the place answers to ("profile" finds Settings) */
  keywords?: string
}

/** The places ⌘O lists. Books and E-readers only where the EPUB library
 * exists (SERVER mode). */
export function buildPlaces({ library, queueCount = 0 }: { library: boolean; queueCount?: number }): Place[] {
  const places: Place[] = [
    { id: 'home', label: 'Home', hint: 'the briefing', keys: '⌘W', keywords: 'briefing start' },
    ...(library ? [{ id: 'books' as const, label: 'Books', hint: 'your EPUB library', keywords: 'epub library reading' }] : []),
    {
      id: 'review',
      label: 'Review queue',
      hint: queueCount ? `${queueCount} open` : 'changes waiting for you',
      keys: '⌘⇧R',
      keywords: 'proposals flags gate',
    },
    { id: 'runs', label: 'Gardeners', hint: 'agent runs', keys: '⌘G', keywords: 'runs agents' },
    { id: 'graph', label: 'Graph', hint: 'docs as nodes, links as lines', keywords: 'links map' },
    { id: 'trash', label: 'Trash', hint: 'restore deleted docs', keywords: 'deleted restore bin' },
    {
      id: 'settings',
      label: 'Settings',
      hint: library ? 'profile, e-readers' : 'profile, version, diagnostics',
      keys: '⌘,',
      keywords: 'profile preferences name version diagnostics account sign out',
    },
  ]
  if (library) {
    places.push({ id: 'ereaders', label: 'E-readers', hint: 'Settings › E-readers', keywords: 'kobo kindle device opds' })
  }
  return places
}

function scorePlace(q: string, p: Place): number | null {
  const label = scoreText(q, p.label)
  const extra = p.keywords ? scoreText(q, p.keywords) : null
  if (label === null && extra === null) return null
  return Math.max(label ?? -1, extra === null ? -1 : extra * PATH_WEIGHT)
}

/* ---------- the Open list (⌘O) ---------- */

export type OpenRow =
  | { key: string; kind: 'doc'; doc: Doc; path: string; recent: boolean }
  | { key: string; kind: 'place'; place: Place }

export const OPEN_LIMIT = 50

/** Empty query: recent docs, then every place. Typed: docs (title or path)
 * and places in one list, best first; on a tie a place beats a doc, then a
 * recently opened doc beats an older one, then list order. Nothing else:
 * no commands, no content hits. */
export function buildOpenRows({
  q,
  docs,
  places,
  recentIds,
  limit = OPEN_LIMIT,
}: {
  q: string
  docs: Doc[]
  places: Place[]
  recentIds: string[]
  limit?: number
}): OpenRow[] {
  const byId = new Map(docs.map((d) => [d.id, d]))
  const recentRank = new Map(recentIds.map((id, i) => [id, i]))
  const docRow = (d: Doc): OpenRow => ({
    key: `doc:${d.id}`,
    kind: 'doc',
    doc: d,
    path: docPath(d, byId),
    recent: recentRank.has(d.id),
  })
  const placeRow = (p: Place): OpenRow => ({ key: `place:${p.id}`, kind: 'place', place: p })
  const query = q.trim()
  if (!query) return [...recentDocs(recentIds, docs).map(docRow), ...places.map(placeRow)]

  const scored: { row: OpenRow; s: number; tie: number; i: number }[] = []
  places.forEach((p, i) => {
    const s = scorePlace(query, p)
    if (s !== null) scored.push({ row: placeRow(p), s, tie: 0, i })
  })
  docs.forEach((d, i) => {
    const path = docPath(d, byId)
    const s = scoreDoc(query, d.title, path)
    if (s === null) return
    const r = recentRank.get(d.id)
    scored.push({ row: { key: `doc:${d.id}`, kind: 'doc', doc: d, path, recent: r !== undefined }, s, tie: r === undefined ? 1000 : 1 + r, i })
  })
  scored.sort((a, b) => b.s - a.s || a.tie - b.tie || a.i - b.i)
  return scored.slice(0, limit).map((x) => x.row)
}

/* ---------- actions (⌘K) ---------- */

export interface OmniCommand {
  id: string
  label: string
  hint?: string
  /** keyboard shortcut, shown on every row */
  keys?: string
  /** extra words the action answers to */
  keywords?: string
  /** `doc` = acts on the open doc: listed first */
  scope?: 'doc' | 'app'
  run: () => void
}

/** Actions matching `q`, best first; with an empty query, the open doc's
 * actions first, then the rest in list order. */
export function rankCommands(cmds: OmniCommand[], q: string, limit = 50): OmniCommand[] {
  const ordered = [...cmds.filter((c) => c.scope === 'doc'), ...cmds.filter((c) => c.scope !== 'doc')]
  const query = q.trim()
  if (!query) return ordered.slice(0, limit)
  const scored: { c: OmniCommand; s: number; i: number }[] = []
  ordered.forEach((c, i) => {
    const l = scoreText(query, c.label)
    const extra = scoreText(query, [c.hint, c.keywords].filter(Boolean).join(' '))
    if (l === null && extra === null) return
    scored.push({ c, s: Math.max(l ?? -1, extra === null ? -1 : extra * 0.5), i })
  })
  scored.sort((a, b) => b.s - a.s || a.i - b.i)
  return scored.slice(0, limit).map((x) => x.c)
}

/* ---------- search (⌘⇧F) ---------- */

/** A leading `?` asks instead of searching; the prefix is stripped. */
export function parseSearch(raw: string): { ask: boolean; q: string } {
  const s = raw.trimStart()
  if (s.startsWith('?')) return { ask: true, q: s.slice(1).trim() }
  return { ask: false, q: raw.trim() }
}

export type SearchRow =
  | { key: string; group: 'content'; hit: SearchHit; crumb: string; snippet: string }
  | { key: string; group: 'ask'; query: string }

export const SEARCH_LIMIT = 20

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

/** Content hits then the trailing ask row; `?` = the ask row alone. */
export function buildSearchRows({ ask, q, hits }: { ask: boolean; q: string; hits: SearchHit[] }): SearchRow[] {
  const askRow: SearchRow = { key: 'ask', group: 'ask', query: q }
  if (ask) return [askRow]
  if (!q) return []
  const rows: SearchRow[] = hits.slice(0, SEARCH_LIMIT).map((h) => ({
    key: `content:${h.block.id}`,
    group: 'content',
    hit: h,
    crumb: crumbOf(h),
    snippet: snippetOf(h.block.content),
  }))
  rows.push(askRow)
  return rows
}

/* ---------- selection ---------- */

/** Where the selection lands after the rows change under it. The selected
 * row keeps its identity by KEY when it survives (hits landing 120 ms later
 * must not move the highlight); a vanished row falls back to the first row of
 * the same group at the same offset, then to the same index clamped, then 0. */
export function stableSelection<R extends { key: string; group?: string }>(prevRows: R[], prevSel: number, nextRows: R[]): number {
  if (nextRows.length === 0) return 0
  const prev = prevRows[prevSel]
  if (!prev) return Math.min(prevSel, nextRows.length - 1)
  const byKey = nextRows.findIndex((r) => r.key === prev.key)
  if (byKey !== -1) return byKey
  const sameGroup = nextRows.findIndex((r) => r.group === prev.group)
  if (sameGroup !== -1) {
    const prevGroupStart = prevRows.findIndex((r) => r.group === prev.group)
    const offset = Math.max(0, prevSel - prevGroupStart)
    let i = sameGroup
    while (offset > i - sameGroup && i + 1 < nextRows.length && nextRows[i + 1].group === prev.group) i++
    return i
  }
  return Math.min(prevSel, nextRows.length - 1)
}
