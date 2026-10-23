// Pure seams for the Lapis doc view: tags from frontmatter, a word diff for
// flagged changes, who wrote a doc, the history grouped by version, and the
// snippet a backlink card shows. No DOM, no fetch.

import type { HistoryRow } from '../types'

/** Tags from a doc's frontmatter blocks (`tags: [a, b]`, `tags: a, b` or a
 * `- a` list under `tags:`), as written, deduped. */
export function frontmatterTags(contents: string[]): string[] {
  const out: string[] = []
  const add = (t: string) => {
    const v = t.trim().replace(/^['"]|['"]$/g, '').replace(/^#/, '').trim()
    if (v && !out.includes(v)) out.push(v)
  }
  for (const c of contents) {
    if (!c.startsWith('---')) continue
    const lines = c.split('\n')
    for (let i = 0; i < lines.length; i++) {
      const m = lines[i].match(/^tags\s*:\s*(.*)$/)
      if (!m) continue
      const rest = m[1].trim()
      if (rest.startsWith('[')) rest.replace(/^\[|\]$/g, '').split(',').forEach(add)
      else if (rest) rest.split(',').forEach(add)
      else
        for (let j = i + 1; j < lines.length; j++) {
          const li = lines[j].match(/^\s*-\s+(.*)$/)
          if (!li) break
          add(li[1])
        }
    }
  }
  return out
}

export type DiffSeg = { t: 'eq' | 'del' | 'ins'; s: string }

/** Word-level diff (LCS over whitespace-kept tokens). Inputs past a few
 * thousand tokens fall back to one del + one ins. */
export function wordDiff(before: string, after: string): DiffSeg[] {
  const a = before.match(/\s+|[^\s]+/g) ?? []
  const b = after.match(/\s+|[^\s]+/g) ?? []
  if (a.length * b.length > 4_000_000) {
    return [
      ...(before ? [{ t: 'del' as const, s: before }] : []),
      ...(after ? [{ t: 'ins' as const, s: after }] : []),
    ]
  }
  const n = a.length
  const m = b.length
  const L: Uint32Array[] = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1))
  for (let i = n - 1; i >= 0; i--)
    for (let j = m - 1; j >= 0; j--) L[i][j] = a[i] === b[j] ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1])
  const out: DiffSeg[] = []
  const push = (t: DiffSeg['t'], s: string) => {
    const last = out[out.length - 1]
    if (last && last.t === t) last.s += s
    else out.push({ t, s })
  }
  let i = 0
  let j = 0
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      push('eq', a[i])
      i++
      j++
    } else if (L[i + 1][j] >= L[i][j + 1]) push('del', a[i++])
    else push('ins', b[j++])
  }
  while (i < n) push('del', a[i++])
  while (j < m) push('ins', b[j++])
  return coalesce(out)
}

/** Reads like an editor's mark-up, not a word salad: a short unchanged run
 * (under three words) between two changes joins the change, and each change
 * run shows all of what went, then all of what came. */
export function coalesce(segs: DiffSeg[]): DiffSeg[] {
  const words = (s: string) => (s.match(/[^\s]+/g) ?? []).length
  const out: DiffSeg[] = []
  let del = ''
  let ins = ''
  const flush = () => {
    if (del) out.push({ t: 'del', s: del })
    if (ins) out.push({ t: 'ins', s: ins })
    del = ins = ''
  }
  for (let k = 0; k < segs.length; k++) {
    const g = segs[k]
    if (g.t === 'del') del += g.s
    else if (g.t === 'ins') ins += g.s
    else {
      const between = (del || ins) && k + 1 < segs.length && words(g.s) < 3
      if (between) {
        del += g.s
        ins += g.s
      } else {
        flush()
        out.push({ t: 'eq', s: g.s })
      }
    }
  }
  flush()
  return out
}

export interface Contributor {
  name: string
  agent: boolean
  yours: boolean
}

/** The people and agents who wrote this doc, most recent first: you first
 * when you are among them. */
export function contributors(rows: (HistoryRow & { principal_is_yours?: boolean })[], max = 3): Contributor[] {
  const seen = new Map<string, Contributor>()
  for (const r of rows) {
    const name = r.principal_name || 'someone'
    if (seen.has(name)) continue
    const agent = r.principal_kind !== 'human'
    seen.set(name, { name, agent, yours: !agent && !!r.principal_is_yours })
  }
  const list = [...seen.values()]
  list.sort((x, y) => Number(y.yours) - Number(x.yours))
  return list.slice(0, max)
}

export interface Version {
  epoch: number
  who: string
  agent: boolean
  yours: boolean
  verdict: 'green' | 'yellow' | 'red' | null
  /** "edited 1 block", "inserted 2 blocks", "renamed the doc" */
  summary: string
  rows: HistoryRow[]
}

const VERB: Record<string, string> = {
  insert: 'inserted',
  replace: 'edited',
  delete: 'deleted',
  move: 'moved',
  rename_doc: 'renamed the doc',
  move_doc: 'moved the doc',
  set_status: 'set the status',
  delete_doc: 'trashed the doc',
}

function plural(n: number) {
  return `${n} block${n === 1 ? '' : 's'}`
}

/** One entry per version (epoch), newest first; parked ops (never applied)
 * are their own group under epoch -1. */
export function versions(rows: (HistoryRow & { principal_is_yours?: boolean })[]): Version[] {
  const m = new Map<number, HistoryRow[]>()
  for (const r of rows) {
    const e = r.op.epoch_applied ?? -1
    if (!m.has(e)) m.set(e, [])
    m.get(e)!.push(r)
  }
  return [...m.entries()]
    .sort((a, b) => b[0] - a[0])
    .map(([epoch, ops]) => {
      const first = ops[0] as HistoryRow & { principal_is_yours?: boolean }
      const counts = new Map<string, number>()
      for (const o of ops) counts.set(o.op.kind.op, (counts.get(o.op.kind.op) ?? 0) + 1)
      const parts = [...counts.entries()].map(([op, n]) => {
        const v = VERB[op] ?? op
        return op in VERB && !op.endsWith('_doc') && op !== 'set_status' ? `${v} ${plural(n)}` : v
      })
      const agent = first.principal_kind !== 'human'
      const verdicts = ops.map((o) => o.op.verdict)
      const verdict = verdicts.includes('red') ? 'red' : verdicts.includes('yellow') ? 'yellow' : (verdicts[0] ?? null)
      return {
        epoch,
        who: first.principal_name || 'someone',
        agent,
        yours: !agent && !!first.principal_is_yours,
        verdict,
        summary: epoch < 0 ? `proposed: ${parts.join(', ')}` : parts.join(', '),
        rows: ops,
      }
    })
}

/** The few words around the [[link]] to `title` in a backlinking block. */
export function backlinkSnippet(content: string, title: string, radius = 48): string {
  const flat = content
    .replace(/```[^\n]*\n[\s\S]*?(```|$)/g, ' ')
    .replace(/^\s*(#+|[-*+>]|\d+\.)\s+/gm, '')
    .replace(/(\*\*|__|`)/g, '')
    .replace(/\s+/g, ' ')
    .trim()
  const lower = flat.toLowerCase()
  let at = lower.indexOf(`[[${title.toLowerCase()}`)
  if (at < 0) at = lower.indexOf(title.toLowerCase())
  if (at < 0) return flat.length > radius * 2 ? flat.slice(0, radius * 2) + '…' : flat
  const start = Math.max(0, at - radius)
  const end = Math.min(flat.length, at + title.length + 4 + radius)
  return `${start > 0 ? '…' : ''}${flat.slice(start, end).trim()}${end < flat.length ? '…' : ''}`
}

/** A doc status and the dot colour token it wears. */
export const STATUSES = ['draft', 'in-review', 'decided', 'superseded'] as const
export type DocStatus = (typeof STATUSES)[number]
export function statusTone(s: string | null | undefined): string {
  switch (s) {
    case 'decided':
      return 'var(--green)'
    case 'in-review':
      return 'var(--yellow-mark)'
    case 'superseded':
      return 'var(--red)'
    default:
      return 'var(--ink3)'
  }
}

/** The frontmatter with `tag` added to its tags (a new block when none). */
export function withTag(frontmatter: string | null, tag: string): string {
  if (!frontmatter) return `---\ntags: [${tag}]\n---`
  const lines = frontmatter.split('\n')
  const at = lines.findIndex((l) => /^tags\s*:/.test(l))
  if (at < 0) {
    lines.splice(1, 0, `tags: [${tag}]`)
    return lines.join('\n')
  }
  const rest = lines[at].replace(/^tags\s*:\s*/, '').trim()
  if (rest.startsWith('[')) {
    const inner = rest.replace(/^\[|\]$/g, '').trim()
    lines[at] = `tags: [${inner ? `${inner}, ` : ''}${tag}]`
  } else if (rest) lines[at] = `tags: ${rest}, ${tag}`
  else {
    let j = at + 1
    while (j < lines.length && /^\s*-\s+/.test(lines[j])) j++
    lines.splice(j, 0, `  - ${tag}`)
  }
  return lines.join('\n')
}

