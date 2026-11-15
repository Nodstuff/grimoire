// Share links (SERVER mode, ADR 0005): the snapshot the browser uploads and
// the /api/shares calls. Diagrams are rendered in the browser (the server
// cannot draw them), so each visual fence whose SVG is on screen becomes a
// `taisce-asset:` image; a fence with no rendered SVG stays as code.
// A link can carry the doc's sub-pages too (`snapshot.pages`): those docs are
// not on screen, so their diagrams are drawn here, one by one.

import { withCsrf } from './auth'
import { childrenIndex } from './tree'
import type { Doc } from './types'

/** fences the editor draws as a diagram (CodeBlockView) */
export const VISUAL_LANGS = new Set(['mermaid', 'reladraw', 'd2'])

export interface SnapshotAsset {
  name: string
  content_type: 'image/svg+xml'
  data: string
  width?: number
  height?: number
}

/** One sub-page of a link: a doc under the shared doc. */
export interface SnapshotPage {
  doc_id: string
  /** the page above it; null = directly under the shared doc */
  parent_doc_id: string | null
  title: string
  markdown: string
}

export interface Snapshot {
  title: string
  markdown: string
  /** every page's images, in one namespace */
  assets: SnapshotAsset[]
  theme: 'light' | 'dark' | 'auto'
  /** the doc's sub-pages in tree order (a parent before its children);
   *  absent = a single-doc link */
  pages?: SnapshotPage[]
}

export interface Share {
  id: string
  doc_id: string
  url: string
  created_at: string
  expires_at: string | null
  revoked_at: string | null
  comments_enabled: boolean
  revision: number
  views: number
  comment_count: number
  unread_comments: number
  title: string
  /** sub-pages published with the doc (0 or absent = the doc alone) */
  pages?: number
}

/** UTF-8 → base64 (btoa alone only takes Latin-1). */
export function base64Utf8(s: string): string {
  const bytes = new TextEncoder().encode(s)
  let bin = ''
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(bin)
}

export function svgSize(svg: string): { width?: number; height?: number } {
  const num = (attr: string) => {
    const m = svg.match(new RegExp(`<svg[^>]*\\s${attr}="([\\d.]+)(px)?"`))
    return m ? Math.round(Number(m[1])) || undefined : undefined
  }
  let width = num('width')
  let height = num('height')
  if (!width || !height) {
    const vb = svg.match(/<svg[^>]*\sviewBox="[\d.\-]+[\s,]+[\d.\-]+[\s,]+([\d.]+)[\s,]+([\d.]+)"/)
    if (vb) {
      width = width ?? Math.round(Number(vb[1]))
      height = height ?? Math.round(Number(vb[2]))
    }
  }
  return { width, height }
}

/** A diagram drawn in the editor: its fence's language and code, and the SVG. */
export interface RenderedDiagram {
  lang: string
  code: string
  svg: string
}

/** Code compared without trailing spaces or surrounding blank lines. */
export function codeKey(lang: string, code: string): string {
  return `${lang.toLowerCase()}\n${code.split('\n').map((l) => l.trimEnd()).join('\n').trim()}`
}

/**
 * The snapshot of a doc. A visual fence becomes its image only when a drawn
 * diagram has the same language and code (each drawing used once); one that
 * cannot be paired stays as code, so an unpaired fence never shifts the
 * others onto the wrong picture. Frontmatter is left for the server.
 * `prefix` goes before each image's name (a link's pages share one set of
 * names).
 */
export function buildSnapshot(
  title: string,
  markdown: string,
  drawn: RenderedDiagram[],
  theme: Snapshot['theme'] = 'auto',
  prefix = '',
): Snapshot {
  const assets: SnapshotAsset[] = []
  const lines = markdown.split('\n')
  const out: string[] = []
  const pool = new Map<string, string[]>()
  for (const d of drawn) {
    const k = codeKey(d.lang, d.code)
    pool.set(k, [...(pool.get(k) ?? []), d.svg])
  }
  for (let i = 0; i < lines.length; i++) {
    const open = lines[i].match(/^(\s{0,3})(`{3,}|~{3,})\s*([\w-]+)?.*$/)
    if (!open) {
      out.push(lines[i])
      continue
    }
    // the close: the same character, at least as long, nothing after it
    const fence = open[2]
    const closes = (l: string) => {
      const m = l.match(/^\s{0,3}(`{3,}|~{3,})\s*$/)
      return !!m && m[1][0] === fence[0] && m[1].length >= fence.length
    }
    let j = i + 1
    while (j < lines.length && !closes(lines[j])) j++
    const lang = (open[3] ?? '').toLowerCase()
    const block = lines.slice(i, Math.min(j + 1, lines.length))
    if (VISUAL_LANGS.has(lang)) {
      const svg = pool.get(codeKey(lang, lines.slice(i + 1, j).join('\n')))?.shift()
      if (svg) {
        const name = `${prefix}d${assets.length + 1}.svg`
        assets.push({ name, content_type: 'image/svg+xml', data: base64Utf8(svg), ...svgSize(svg) })
        out.push(`${open[1]}![${lang} diagram](taisce-asset:${name})`)
        i = j
        continue
      }
    }
    out.push(...block)
    i = j
  }
  return { title, markdown: out.join('\n'), assets, theme }
}

/** The visual fences of a doc's markdown, in order: what to draw for a page
 *  that is not on screen. The same fence rules as `buildSnapshot`. */
export function visualFences(markdown: string): { lang: string; code: string }[] {
  const out: { lang: string; code: string }[] = []
  const lines = markdown.split('\n')
  for (let i = 0; i < lines.length; i++) {
    const open = lines[i].match(/^(\s{0,3})(`{3,}|~{3,})\s*([\w-]+)?.*$/)
    if (!open) continue
    const fence = open[2]
    let j = i + 1
    while (j < lines.length) {
      const m = lines[j].match(/^\s{0,3}(`{3,}|~{3,})\s*$/)
      if (m && m[1][0] === fence[0] && m[1].length >= fence.length) break
      j++
    }
    const lang = (open[3] ?? '').toLowerCase()
    if (VISUAL_LANGS.has(lang)) out.push({ lang, code: lines.slice(i + 1, j).join('\n') })
    i = j
  }
  return out
}

/** The docs under `rootId`, in the sidebar's order: each parent before its
 *  children (the order a link's pages are sent and listed in). */
export function subPages(docs: Doc[], rootId: string): Doc[] {
  const kids = childrenIndex(docs)
  const out: Doc[] = []
  const seen = new Set<string>([rootId])
  const walk = (id: string) => {
    for (const d of kids.get(id) ?? []) {
      if (seen.has(d.id)) continue
      seen.add(d.id)
      out.push(d)
      walk(d.id)
    }
  }
  walk(rootId)
  return out
}

/** What a tree snapshot left as code: one line per diagram that did not draw. */
export interface TreeSnapshot {
  snapshot: Snapshot
  problems: string[]
}

/**
 * The snapshot of a doc and its sub-pages. `root` is built as a single doc
 * is (its diagrams from the screen); each sub-page's visual fences are drawn
 * with `draw`, one at a time, and a fence that fails stays as code (noted in
 * `problems`). Page k's images are named `p<k>-d<n>.svg`.
 */
export async function buildTreeSnapshot(
  root: Snapshot,
  rootId: string,
  pages: { doc: Doc; markdown: string }[],
  draw: (lang: string, code: string) => Promise<string>,
): Promise<TreeSnapshot> {
  const problems: string[] = []
  const assets = [...root.assets]
  const out: SnapshotPage[] = []
  for (const [k, { doc, markdown }] of pages.entries()) {
    const drawn: RenderedDiagram[] = []
    for (const f of visualFences(markdown)) {
      try {
        drawn.push({ ...f, svg: await draw(f.lang, f.code) })
      } catch (e) {
        problems.push(`${doc.title}: a ${f.lang} diagram stayed as code (${String(e instanceof Error ? e.message : e).split('\n')[0]})`)
      }
    }
    const page = buildSnapshot(doc.title, markdown, drawn, root.theme, `p${k + 1}-`)
    assets.push(...page.assets)
    out.push({
      doc_id: doc.id,
      parent_doc_id: doc.parent_id === rootId ? null : doc.parent_id,
      title: doc.title,
      markdown: page.markdown,
    })
  }
  return { snapshot: { ...root, assets, pages: out }, problems }
}

/** Every diagram drawn in the open editor, with the code it was drawn from. */
export function renderedDiagrams(root: ParentNode = document): RenderedDiagram[] {
  const out: RenderedDiagram[] = []
  root.querySelectorAll('.codeblock-view').forEach((v) => {
    const pre = v.querySelector('pre')
    const lang = pre?.getAttribute('data-language')?.toLowerCase() ?? ''
    const svg = v.querySelector('.diagram svg')
    if (!pre || !svg || !VISUAL_LANGS.has(lang)) return
    out.push({ lang, code: pre.textContent ?? '', svg: new XMLSerializer().serializeToString(svg) })
  })
  return out
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(path, withCsrf({ ...init, headers: { 'content-type': 'application/json', ...(init?.headers ?? {}) } }))
  if (r.status === 204) return undefined as T
  const j = await r.json().catch(() => null)
  if (!r.ok) throw new Error(j && typeof j.error === 'string' ? j.error : `${r.status} ${r.statusText}`)
  return j as T
}

export function listShares(docId: string): Promise<Share[]> {
  return call<{ shares: Share[] }>(`/api/shares?doc_id=${encodeURIComponent(docId)}`).then((r) => r.shares)
}

/** `days` null = never expires */
export function createShare(docId: string, snapshot: Snapshot, days: number | null): Promise<Share> {
  const expires_at = days ? new Date(Date.now() + days * 86_400_000).toISOString() : null
  return call<Share>('/api/shares', {
    method: 'POST',
    body: JSON.stringify({ doc_id: docId, snapshot, expires_at, comments_enabled: true }),
  })
}

export function revokeShare(id: string): Promise<void> {
  return call<void>(`/api/shares/${id}`, { method: 'DELETE' })
}

/** Live: not revoked and not past its expiry. */
export function isLive(s: Share, now = Date.now()): boolean {
  return !s.revoked_at && (!s.expires_at || Date.parse(s.expires_at) > now)
}
