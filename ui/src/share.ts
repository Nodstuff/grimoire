// Share links (SERVER mode, ADR 0005): the snapshot the browser uploads and
// the /api/shares calls. Diagrams are rendered in the browser (the server
// cannot draw them), so each visual fence whose SVG is on screen becomes a
// `taisce-asset:` image; a fence with no rendered SVG stays as code.

import { withCsrf } from './auth'

/** fences the editor draws as a diagram (CodeBlockView) */
export const VISUAL_LANGS = new Set(['mermaid', 'reladraw', 'd2'])

export interface SnapshotAsset {
  name: string
  content_type: 'image/svg+xml'
  data: string
  width?: number
  height?: number
}

export interface Snapshot {
  title: string
  markdown: string
  assets: SnapshotAsset[]
  theme: 'light' | 'dark' | 'auto'
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
}

/** UTF-8 → base64 (btoa alone only takes Latin-1). */
export function base64Utf8(s: string): string {
  const bytes = new TextEncoder().encode(s)
  let bin = ''
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(bin)
}

function svgSize(svg: string): { width?: number; height?: number } {
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
 */
export function buildSnapshot(title: string, markdown: string, drawn: RenderedDiagram[], theme: Snapshot['theme'] = 'auto'): Snapshot {
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
        const name = `d${assets.length + 1}.svg`
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
