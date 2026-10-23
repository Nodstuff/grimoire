// Pure text helpers behind the Search & Ask page (canvas Search): the
// highlighted snippet of a hit, the time hidden in a UUIDv7, and the shape
// of an ask-the-vault answer doc (question, synthesis with numbered
// citations, the quoted receipts). Kept out of the component so they test
// without a DOM.

import type { BlockNode } from '../types'

/* ---------- highlighting ---------- */

export interface Seg {
  t: string
  /** this run matched the query */
  m?: boolean
}

/** Little words a question is full of: never highlighted. */
const STOP = new Set(
  'the and that this with from what when where which while about have does did how why who are was were will would should could there their they them then than into onto over under after before also just like some any our your its for not but you can all one a an of to in on at by is it be as or if so do my me we us'.split(' '),
)

/** The words worth highlighting: 2+ characters, not a little word, deduped,
 * longest first (so "review gate" marks "review" before "re"). */
export function queryTerms(q: string): string[] {
  const words = q
    .toLowerCase()
    .split(/[^\p{L}\p{N}_-]+/u)
    .map((w) => w.replace(/^-+|-+$/g, ''))
    .filter((w) => w.length >= 2 && !STOP.has(w))
  return [...new Set(words)].sort((a, b) => b.length - a.length)
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** Split `text` into runs, the query's words marked (case-insensitive). */
export function highlight(text: string, q: string): Seg[] {
  const terms = queryTerms(q)
  if (!terms.length || !text) return text ? [{ t: text }] : []
  const re = new RegExp(`(${terms.map(escapeRe).join('|')})`, 'giu')
  const out: Seg[] = []
  let last = 0
  for (const m of text.matchAll(re)) {
    const i = m.index ?? 0
    if (i > last) out.push({ t: text.slice(last, i) })
    out.push({ t: m[0], m: true })
    last = i + m[0].length
  }
  if (last < text.length) out.push({ t: text.slice(last) })
  return out
}

/** A block's content as one readable line: heading hashes, list bullets and
 * quote marks dropped, emphasis markers dropped, lines joined with " · ". */
export function flatText(content: string): string {
  return content
    .split('\n')
    .filter((l) => !/^\s*(```|~~~)/.test(l))
    .map((l) =>
      l
        .trim()
        .replace(/^#{1,6}\s+/, '')
        .replace(/^>\s?/, '')
        .replace(/^[-*+]\s+(\[[ xX]\]\s+)?/, '')
        .replace(/^\|?[-:| ]+\|?$/, ''),
    )
    .filter((l) => l && l !== '---')
    .join(' · ')
    .replace(/\*\*|__|`/g, '')
    .replace(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|([^\]]+))?\]\]/g, (_, t, alias) => alias || t)
}

/** At most `max` characters of `content`, centred on the first query word
 * when it sits past the start (with … on the cut sides). */
export function snippetAround(content: string, q: string, max = 240): string {
  const s = flatText(content)
  if (s.length <= max) return s
  const terms = queryTerms(q)
  const lower = s.toLowerCase()
  let at = -1
  for (const t of terms) {
    const i = lower.indexOf(t)
    if (i >= 0 && (at < 0 || i < at)) at = i
  }
  if (at < max * 0.6) return `${s.slice(0, max - 1).trimEnd()}…`
  const start = Math.max(0, Math.min(at - Math.floor(max * 0.3), s.length - max))
  const body = s.slice(start, start + max).trim()
  return `${start > 0 ? '…' : ''}${body}${start + max < s.length ? '…' : ''}`
}

/* ---------- time ---------- */

/** The millisecond timestamp in a UUIDv7 (ops, docs and blocks all get
 * one), or null for any other version. */
export function uuidTime(id: string): number | null {
  const hex = id.replace(/-/g, '')
  if (hex.length !== 32 || hex[12] !== '7') return null
  const ms = parseInt(hex.slice(0, 12), 16)
  return Number.isFinite(ms) ? ms : null
}

/** "12 min", "09:14", "Mon", "2 Oct", "Sep 2024": the Library's Updated
 * column (canvas Library). */
export function shortWhen(ms: number, now: number = Date.now()): string {
  const d = new Date(ms)
  const diff = now - ms
  if (diff < 60_000) return 'now'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} min`
  const today = new Date(now)
  if (d.toDateString() === today.toDateString()) return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  if (diff < 6 * 86_400_000) return d.toLocaleDateString(undefined, { weekday: 'short' })
  if (d.getFullYear() === today.getFullYear()) return d.toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
  return d.toLocaleDateString(undefined, { month: 'short', year: 'numeric' })
}

/* ---------- answers ---------- */

/** A run of answer prose: text, or a citation of block `id` in doc `title`. */
export type AnswerRun = { t: string; b?: boolean; i?: boolean } | { cite: string; title: string }

export interface Receipt {
  blockId: string
  title: string
  quote: string
}

export interface ParsedAnswer {
  question: string
  /** paragraphs of the synthesis; empty when none was written (no Claude
   * Code where Taisce runs: the receipts are the answer) */
  paras: AnswerRun[][]
  /** the synthesis is still being written (the placeholder stands) */
  writing: boolean
  /** a synthesis that could not be written says so in italics */
  failed: boolean
  receipts: Receipt[]
  /** "Asked 2026-10-23 · 9 blocks across 4 docs." */
  meta: string | null
}

function flatten(nodes: BlockNode[], out: BlockNode['block'][] = []): BlockNode['block'][] {
  for (const n of nodes) {
    if (!n.block.deleted) out.push(n.block)
    flatten(n.children, out)
  }
  return out
}

const CITE = /\[\[([^\]#|]+)#\^([0-9a-f-]{36})\]\]/gi

/** Prose → runs: **bold**, *italic*, and [[Title#^id]] citations. Other
 * wikilinks read as their title. */
export function answerRuns(text: string): AnswerRun[] {
  const out: AnswerRun[] = []
  const pushText = (s: string) => {
    if (!s) return
    s = s.replace(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|([^\]]+))?\]\]/g, (_, t, alias) => alias || t)
    const re = /\*\*([^*]+)\*\*|\*([^*]+)\*|`([^`]+)`/g
    let last = 0
    for (const m of s.matchAll(re)) {
      const i = m.index ?? 0
      if (i > last) out.push({ t: s.slice(last, i) })
      if (m[1] !== undefined) out.push({ t: m[1], b: true })
      else if (m[2] !== undefined) out.push({ t: m[2], i: true })
      else out.push({ t: m[3] })
      last = i + m[0].length
    }
    if (last < s.length) out.push({ t: s.slice(last) })
  }
  let last = 0
  for (const m of text.matchAll(CITE)) {
    const i = m.index ?? 0
    pushText(text.slice(last, i).replace(/\s+$/, i > last ? ' ' : ''))
    out.push({ cite: m[2], title: m[1].trim() })
    last = i + m[0].length
  }
  pushText(text.slice(last))
  return out
}

/** Read an answer doc (ask.rs: frontmatter, `# question`, the synthesis or
 * its placeholder, then `## Doc` sections of quoted receipts, a rule and an
 * italic "Asked …" line) back into its parts. */
export function parseAnswer(roots: BlockNode[]): ParsedAnswer {
  const blocks = flatten(roots)
  let question = ''
  const paras: AnswerRun[][] = []
  const receipts: Receipt[] = []
  let writing = false
  let failed = false
  let meta: string | null = null
  let section = ''
  blocks.forEach((b, idx) => {
    const c = b.content.trim()
    if (!c || c === '---') return
    if (/^---\n[\s\S]*\n---$/.test(c)) return // frontmatter
    const h = /^(#{1,6})\s+(.*)$/.exec(c)
    if (h && !c.includes('\n')) {
      if (h[1].length === 1 && !question) {
        question = h[2].trim()
        return
      }
      const next = blocks[idx + 1]?.content.trim() ?? ''
      // a doc's receipts sit under its title; any other heading is prose
      if (next.startsWith('>')) section = h[2].trim()
      else paras.push([{ t: h[2].trim(), b: true }])
      return
    }
    if (/^\*Asked .*\*$/.test(c)) {
      meta = c.slice(1, -1)
      return
    }
    if (c.startsWith('>')) {
      // > quote…\n>\n> — [[Title#^id]]
      const cite = [...c.matchAll(CITE)].pop()
      if (cite) {
        const quote = flatText(
          c
            .split('\n')
            .map((l) => l.replace(/^>\s?/, '').trim())
            .filter((l) => l && !/^—\s*\[\[/.test(l))
            .join(' ')
            .replace(/```\w*/g, ''),
        )
        receipts.push({ blockId: cite[2], title: cite[1].trim() || section, quote })
        return
      }
    }
    if (c.includes('Writing a synthesis from the excerpts')) {
      writing = true
      return
    }
    if (/^\*The synthesis could not be written/.test(c)) {
      failed = true
      return
    }
    for (const p of c.split(/\n{2,}/)) {
      const one = p.replace(/^#{1,6}\s+/, '').replace(/\n/g, ' ').trim()
      if (one) paras.push(answerRuns(one))
    }
  })
  return { question, paras, writing, failed, receipts, meta }
}

/** Number the citations by first appearance (1, 2, 3…) and order the
 * receipts to match: cited first (numbered), then the rest read but not
 * cited. */
export function numberCitations(a: ParsedAnswer): { num: Map<string, number>; cited: Receipt[]; uncited: Receipt[] } {
  const num = new Map<string, number>()
  for (const p of a.paras) for (const r of p) if ('cite' in r && !num.has(r.cite)) num.set(r.cite, num.size + 1)
  const byId = new Map(a.receipts.map((r) => [r.blockId, r]))
  const cited: Receipt[] = []
  for (const [id] of num) {
    const r = byId.get(id)
    cited.push(r ?? { blockId: id, title: '', quote: '' })
  }
  const uncited = a.receipts.filter((r) => !num.has(r.blockId))
  return { num, cited, uncited }
}

/** Two answer titles name the same question (trailing ? and case aside). */
export function sameQuestion(a: string, b: string): boolean {
  return answerTitle(a).toLowerCase() === answerTitle(b).toLowerCase()
}

/** ask.rs title_for: the answer doc's title for a question. */
export function answerTitle(question: string): string {
  const q = question.trim().replace(/\?+$/, '').trim()
  const chars = [...q]
  const t = chars.slice(0, 80).join('') + (chars.length > 80 ? '…' : '')
  return t || 'Question'
}
