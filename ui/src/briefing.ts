// Pure seams behind the briefing home (Home.tsx): grouping the review
// queue by doc, reading a gardener run's verdict counts, the "since you were
// here" merge, and the daily-doc helpers (find today's log, lift the Plans
// checkboxes out of its blocks, toggle one, carry yesterday forward). No DOM,
// no fetch — vitest covers all of it.

import type { ActivityItem, Block, BlockNode, Doc, GardenerRun, QueueRow } from './types'

/* ---------- review: proposals grouped by doc ---------- */

export interface DocGroup {
  docId: string
  title: string
  rows: QueueRow[]
  proposers: string[]
  annotationIds: string[]
}

/** Queue rows grouped by doc, in queue order (oldest doc first). */
export function groupQueueByDoc(rows: QueueRow[]): DocGroup[] {
  const out: DocGroup[] = []
  const byDoc = new Map<string, DocGroup>()
  for (const r of rows) {
    const id = r.item.annotation.doc_id
    let g = byDoc.get(id)
    if (!g) {
      g = { docId: id, title: r.doc_title || 'untitled', rows: [], proposers: [], annotationIds: [] }
      byDoc.set(id, g)
      out.push(g)
    }
    g.rows.push(r)
    g.annotationIds.push(r.item.annotation.id)
    if (r.proposer && !g.proposers.includes(r.proposer)) g.proposers.push(r.proposer)
  }
  return out
}

/* ---------- gardener runs ---------- */

export interface VerdictCounts {
  green: number
  yellow: number
  red: number
}

/** "verdicts: 3 yellow, 1 red" → {green:0, yellow:3, red:1}; null when the
 * summary names no verdicts at all. */
export function runVerdictCounts(summary: string | null | undefined): VerdictCounts | null {
  if (!summary) return null
  const counts: VerdictCounts = { green: 0, yellow: 0, red: 0 }
  let any = false
  for (const m of summary.matchAll(/(\d+)\s+(green|yellow|red)\b/gi)) {
    counts[m[2].toLowerCase() as keyof VerdictCounts] += Number(m[1])
    any = true
  }
  return any ? counts : null
}

export function verdictLine(c: VerdictCounts | null): string {
  if (!c) return ''
  const parts: string[] = []
  if (c.green) parts.push(`${c.green} green`)
  if (c.yellow) parts.push(`${c.yellow} yellow`)
  if (c.red) parts.push(`${c.red} red`)
  return parts.join(' · ')
}

/* ---------- since you were here ---------- */

export interface NewDoc {
  id: string
  title: string
  created_by_name: string
  created_by_kind: string
  created_at: string
}

export interface SinceItem {
  at: string
  kind: 'run' | 'edit' | 'newdoc'
  text: string
  /** run status / verdict line, shown dim after the text */
  detail?: string
  docId?: string
  status?: string
}

const DAY_MS = 24 * 60 * 60 * 1000

/** Merge runs, remote edits and agent-created docs into one newest-first list
 * of what happened after `since`. A missing stamp (first visit) means the
 * last 24 hours. */
export function sinceItems(
  since: string | null,
  runs: GardenerRun[],
  activity: ActivityItem[],
  newDocs: NewDoc[],
  now: number = Date.now(),
  limit = 12,
): SinceItem[] {
  const floor = since ? new Date(since).getTime() : now - DAY_MS
  const items: SinceItem[] = []
  for (const r of runs) {
    if (new Date(r.started_at).getTime() < floor) continue
    const v = verdictLine(runVerdictCounts(r.summary))
    items.push({
      at: r.started_at,
      kind: 'run',
      text: `${r.gardener_name} ran`,
      detail: [r.status, v].filter(Boolean).join(' · '),
      status: r.status,
    })
  }
  for (const a of activity) {
    if (new Date(a.created_at).getTime() < floor) continue
    items.push({ at: a.created_at, kind: 'edit', text: `${a.principal_name} edited “${a.doc_title}”`, docId: a.doc_id })
  }
  for (const d of newDocs) {
    if (new Date(d.created_at).getTime() < floor) continue
    items.push({ at: d.created_at, kind: 'newdoc', text: `${d.created_by_name} created “${d.title}”`, docId: d.id })
  }
  items.sort((a, b) => new Date(b.at).getTime() - new Date(a.at).getTime())
  return items.slice(0, limit)
}

/* ---------- the daily doc ---------- */

export const DAILY_ROOT = 'Daily'

/** Local calendar date as the daily doc's title, `YYYY-MM-DD`. */
export function localDateTitle(d: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

/** The calendar day before a `YYYY-MM-DD` title. */
export function dayBefore(title: string): string {
  const [y, m, d] = title.split('-').map(Number)
  return localDateTitle(new Date(y, m - 1, d - 1))
}

export function findDailyRoot(docs: Doc[]): Doc | null {
  return docs.find((d) => d.parent_id === null && d.title === DAILY_ROOT) ?? null
}

/** The doc titled `title` anywhere under the root doc titled Daily. */
export function findDailyDoc(docs: Doc[], title: string): Doc | null {
  const root = findDailyRoot(docs)
  if (!root) return null
  const parentOf = new Map(docs.map((d) => [d.id, d.parent_id]))
  const underDaily = (id: string): boolean => {
    let cur: string | null | undefined = parentOf.get(id)
    while (cur) {
      if (cur === root.id) return true
      cur = parentOf.get(cur)
    }
    return false
  }
  return docs.find((d) => d.title === title && underDaily(d.id)) ?? null
}

/** Blocks in reading order (the tree nests body under headings). */
export function flattenBlocks(roots: BlockNode[]): Block[] {
  const out: Block[] = []
  const walk = (ns: BlockNode[]) => {
    for (const n of ns) {
      out.push(n.block)
      walk(n.children)
    }
  }
  walk(roots)
  return out
}

export interface PlanItem {
  blockId: string
  /** line index inside the block's content */
  line: number
  text: string
  checked: boolean
}

export interface PlanGroup {
  project: string
  items: PlanItem[]
}

const CHECKBOX = /^(\s*[-*]\s+)\[( |x|X)\]\s+(.*)$/
const HEADING = /^(#{1,6})\s+(.*)$/

/** Level and text of a heading line, or null. */
function headingOf(line: string): { level: number; text: string } | null {
  const m = HEADING.exec(line.trim())
  return m ? { level: m[1].length, text: m[2].trim() } : null
}

/** Every `- [ ]` / `- [x]` line under a `### <section>` heading, grouped by
 * the enclosing `## <project>` heading. Blocks are one-or-more lines; the
 * item remembers its block and line so a toggle can rewrite just that block. */
export function extractPlans(blocks: Pick<Block, 'id' | 'content'>[], section = 'Plans'): PlanGroup[] {
  const groups: PlanGroup[] = []
  let project = ''
  let inSection = false
  const want = section.toLowerCase()
  for (const b of blocks) {
    const lines = b.content.split('\n')
    lines.forEach((raw, i) => {
      const h = headingOf(raw)
      if (h) {
        if (h.level <= 2) {
          project = h.level === 2 ? h.text : project
          inSection = false
        } else if (h.level === 3) {
          inSection = h.text.toLowerCase() === want
        }
        return
      }
      if (!inSection) return
      const m = CHECKBOX.exec(raw)
      if (!m) return
      let g = groups.find((x) => x.project === project)
      if (!g) {
        g = { project, items: [] }
        groups.push(g)
      }
      g.items.push({ blockId: b.id, line: i, text: m[3].trim(), checked: m[2] !== ' ' })
    })
  }
  return groups
}

export interface SectionLines {
  project: string
  lines: string[]
}

/** Non-empty, non-heading lines under `### <section>` per project, with a
 * leading list bullet stripped (the Open Questions of yesterday's log). */
export function extractSectionLines(blocks: Pick<Block, 'id' | 'content'>[], section: string): SectionLines[] {
  const groups: SectionLines[] = []
  let project = ''
  let inSection = false
  const want = section.toLowerCase()
  for (const b of blocks) {
    for (const raw of b.content.split('\n')) {
      const h = headingOf(raw)
      if (h) {
        if (h.level <= 2) {
          project = h.level === 2 ? h.text : project
          inSection = false
        } else if (h.level === 3) {
          inSection = h.text.toLowerCase() === want
        }
        continue
      }
      if (!inSection) continue
      const text = raw.trim().replace(/^[-*]\s+/, '')
      if (!text || text.startsWith('---')) continue
      let g = groups.find((x) => x.project === project)
      if (!g) {
        g = { project, lines: [] }
        groups.push(g)
      }
      g.lines.push(text)
    }
  }
  return groups
}

/** The block's content with the checkbox on `line` flipped. Anything that is
 * not a checkbox line comes back unchanged. */
export function toggleCheckboxLine(content: string, line: number): string {
  const lines = content.split('\n')
  const raw = lines[line]
  if (raw === undefined) return content
  const m = CHECKBOX.exec(raw)
  if (!m) return content
  const box = m[2] === ' ' ? '[x]' : '[ ]'
  lines[line] = `${m[1]}${box} ${m[3]}`
  return lines.join('\n')
}

/** Items already present in a section, for the carry-forward dedupe. */
function normalise(s: string): string {
  return s
    .replace(/\s*\(carried from \d{4}-\d{2}-\d{2}\)\s*$/, '')
    .trim()
    .toLowerCase()
}

/** Today's markdown with `lines` appended under `## project` → `### section`,
 * both headings created when missing. Plans land as unchecked boxes, other
 * sections as bullets; every line is stamped `(carried from <fromDate>)`.
 * Lines already present (ignoring the stamp) are skipped; when nothing is
 * left to add the markdown comes back byte-identical. */
export function carryForward(
  markdown: string,
  project: string,
  section: 'Plans' | 'Open Questions' | string,
  lines: string[],
  fromDate: string,
): string {
  const src = markdown.replace(/\s+$/, '')
  const out = src.length ? src.split('\n') : []
  const isTop = (l: string) => {
    const h = headingOf(l)
    return !!h && h.level <= 2
  }
  // locate ## project
  let pStart = out.findIndex((l) => {
    const h = headingOf(l)
    return !!h && h.level === 2 && h.text.toLowerCase() === project.toLowerCase()
  })
  let pEnd: number
  if (pStart === -1) {
    if (out.length) out.push('')
    out.push(`## ${project}`)
    pStart = out.length - 1
    pEnd = out.length
  } else {
    pEnd = out.findIndex((l, i) => i > pStart && isTop(l))
    if (pEnd === -1) pEnd = out.length
  }
  // locate ### section inside the project
  let sStart = -1
  for (let i = pStart + 1; i < pEnd; i++) {
    const h = headingOf(out[i])
    if (h && h.level === 3 && h.text.toLowerCase() === section.toLowerCase()) {
      sStart = i
      break
    }
  }
  let sEnd: number
  if (sStart === -1) {
    // insert at the end of the project section (trim trailing blanks first)
    let at = pEnd
    while (at > pStart + 1 && out[at - 1].trim() === '') at--
    out.splice(at, 0, '', `### ${section}`)
    sStart = at + 1
    sEnd = sStart + 1
  } else {
    sEnd = sStart + 1
    while (sEnd < out.length && !headingOf(out[sEnd])) sEnd++
  }
  const present = new Set<string>()
  for (let i = sStart + 1; i < sEnd; i++) {
    const t = out[i].trim().replace(/^[-*]\s+(\[[ xX]\]\s+)?/, '')
    if (t) present.add(normalise(t))
  }
  const bullet = section.toLowerCase() === 'plans' ? '- [ ] ' : '- '
  const additions = lines
    .map((l) => l.trim())
    .filter((l) => l && !present.has(normalise(l)))
    .map((l) => `${bullet}${l.replace(/\s*\(carried from \d{4}-\d{2}-\d{2}\)\s*$/, '')} (carried from ${fromDate})`)
  if (additions.length === 0) return markdown
  // attach to the list: drop blank lines at the end of the section
  let at = sEnd
  while (at > sStart + 1 && out[at - 1].trim() === '') at--
  out.splice(at, 0, ...additions)
  return out.join('\n') + '\n'
}

/** Markdown for a fresh daily doc: nothing but a place to type. The
 * carry-forward builder adds the `## project` / `### Plans` scaffolding. */
export function emptyDailyMarkdown(): string {
  return ''
}
