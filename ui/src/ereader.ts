// EPUB export and the e-reader feed: a doc (or a folder of docs, one
// chapter each) becomes a book. "Download EPUB" saves GET /api/docs/{id}/epub;
// "Send to e-readers" asks the server to set the doc's `ereader` frontmatter
// tag (the server writes it, as the person: the UI never edits frontmatter),
// which puts the book in this person's catalog feed. The server cannot draw
// diagrams, so the browser renders each diagram fence of a sent book to a PNG
// for e-ink (white ground, black strokes) and uploads it; a diagram that
// fails to draw is skipped and the book shows its source instead.

import { useSyncExternalStore } from 'react'
import { signalUnauthorized } from './auth'
import { loadMermaid } from './editor/mermaidLoader'
import { renderReladraw } from './editor/reladraw'
import { svgSize } from './share'
import { api, ApiError } from './types'
import type { BookProgress } from './books'

/* ---------- the contract's shapes ---------- */

export type DiagramLang = 'mermaid' | 'd2' | 'reladraw' | 'vega' | 'vega-lite'

export interface EreaderDiagram {
  block_id: string
  lang: DiagramLang | string
  source: string
  source_hash: string
  /** a current PNG is already stored for (block_id, source_hash) */
  has_image: boolean
}

export interface EreaderStatus {
  /** a book in THIS person's e-reader feed */
  sent: boolean
  /** the `ereader` tag is present (possibly set by someone else) */
  tagged: boolean
  /** the nearest sent folder this doc is a chapter of */
  in_sent_book: { id: string; title: string } | null
  filename: string
  chapters: number
  /** every diagram fence in the whole book */
  diagrams: EreaderDiagram[]
}

export interface SentBook {
  id: string
  title: string
  filename: string
  workspace: string | null
  chapters: number
  /** RFC 3339 */
  updated_at: string
  size: number
  progress: BookProgress | null
}

export interface DiagramPng {
  block_id: string
  source_hash: string
  /** base64, no data: prefix */
  png: string
  width: number
  height: number
}

const JSON_HEADERS = { 'Content-Type': 'application/json' }
const docPath = (id: string) => `/api/docs/${encodeURIComponent(id)}`

/* ---------- calls ---------- */

export function getEreaderStatus(docId: string): Promise<EreaderStatus> {
  return api<EreaderStatus>(`${docPath(docId)}/ereader`)
}

export function postEreader(docId: string, send: boolean): Promise<EreaderStatus> {
  return api<EreaderStatus>(`${docPath(docId)}/ereader`, { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ send }) })
}

export function postEpubImages(docId: string, images: DiagramPng[]): Promise<{ stored: number; ignored: number }> {
  return api<{ stored: number; ignored: number }>(`${docPath(docId)}/epub-images`, {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify({ images }),
  })
}

export function listSentBooks(): Promise<SentBook[]> {
  return api<{ docs: SentBook[] }>('/api/ereader/sent').then((r) => (Array.isArray(r?.docs) ? r.docs : []))
}

/* ---------- Download EPUB ---------- */

/** The file name a Content-Disposition carries: `filename*=UTF-8''…` first,
 * else `filename="…"`; null when there is none. */
export function filenameFromDisposition(h: string | null | undefined): string | null {
  if (!h) return null
  const star = h.match(/filename\*\s*=\s*([\w-]+)?'[^']*'([^;]+)/i)
  if (star) {
    try {
      const v = decodeURIComponent(star[2].trim().replace(/^"(.*)"$/, '$1'))
      if (v) return v
    } catch {
      // a malformed escape: fall through to the plain parameter
    }
  }
  const quoted = h.match(/(?:^|;)\s*filename\s*=\s*"((?:[^"\\]|\\.)*)"/i)
  if (quoted) return quoted[1].replace(/\\(.)/g, '$1') || null
  const bare = h.match(/(?:^|;)\s*filename\s*=\s*([^;\s]+)/i)
  return bare ? bare[1] : null
}

/** `Taisce - <Title>.epub`, with the characters a file name cannot hold dashed. */
export function fallbackEpubName(title: string): string {
  const t = title.replace(/[/\\:*?"<>|\u0000-\u001f]/g, '-').trim() || 'Untitled'
  return `Taisce - ${t}.epub`
}

function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = name
  a.rel = 'noopener'
  document.body.appendChild(a)
  a.click()
  a.remove()
  // Safari reads the blob after the click returns
  setTimeout(() => URL.revokeObjectURL(url), 30_000)
}

/** Fetch the doc's EPUB and save it; the file name it was saved as. */
export async function downloadEpub(docId: string, title: string, save: (b: Blob, name: string) => void = saveBlob): Promise<string> {
  const r = await fetch(`${docPath(docId)}/epub`, { credentials: 'same-origin' })
  if (r.status === 401) {
    signalUnauthorized()
    throw new ApiError('sign in to continue', 'unauthorized')
  }
  if (!r.ok) {
    const j = await r.json().catch(() => null)
    const msg = j && typeof j.error === 'string' ? j.error : `${r.status} ${r.statusText}`.trim()
    throw new ApiError(msg, j && typeof j.code === 'string' ? j.code : undefined)
  }
  const name = filenameFromDisposition(r.headers.get('content-disposition')) ?? fallbackEpubName(title)
  save(await r.blob(), name)
  return name
}

/* ---------- the open doc's status, shared by the doc view and ⌘K ---------- */

const statuses = new Map<string, EreaderStatus | null>()
const subs = new Set<() => void>()
function put(docId: string, s: EreaderStatus | null) {
  statuses.set(docId, s)
  subs.forEach((f) => f())
}
function subscribe(f: () => void) {
  subs.add(f)
  return () => {
    subs.delete(f)
  }
}

/** The last known status of a doc; null = unknown or no e-reader routes. */
export function ereaderStatusOf(docId: string | null): EreaderStatus | null {
  return docId ? (statuses.get(docId) ?? null) : null
}

export function useEreaderStatus(docId: string | null): EreaderStatus | null {
  return useSyncExternalStore(
    subscribe,
    () => ereaderStatusOf(docId),
    () => null,
  )
}

/** GET the status into the shared store. A daemon without the route (or a
 * doc it refuses) stores null: the e-reader actions stay hidden. */
export async function loadEreaderStatus(docId: string): Promise<EreaderStatus | null> {
  try {
    const s = await getEreaderStatus(docId)
    put(docId, s)
    return s
  } catch {
    put(docId, null)
    return null
  }
}

/** Fired on `window` after a send / stop: the server rewrote the doc's
 * frontmatter, so the open doc view refreshes. detail: { docId } */
export const EREADER_EVENT = 'taisce:ereader'

/** Send the doc to this person's e-readers (or stop). A sent book's diagrams
 * are drawn and uploaded in the background. */
export async function setSending(docId: string, send: boolean): Promise<EreaderStatus> {
  const s = await postEreader(docId, send)
  put(docId, s)
  try {
    window.dispatchEvent(new CustomEvent(EREADER_EVENT, { detail: { docId } }))
  } catch {
    // no window (tests in node): nothing to refresh
  }
  if (s.sent) void syncDiagramImages(docId, { status: s })
  return s
}

/* ---------- labels ---------- */

/** The send / stop action's label for a doc. */
export function sendLabel(status: Pick<EreaderStatus, 'sent'>, hasChildren: boolean): string {
  if (status.sent) return 'Stop sending to e-readers'
  return hasChildren ? 'Send folder to e-readers' : 'Send to e-readers'
}

/** The doc header chip: "On e-readers", "On e-readers · in <Folder>", or none. */
export function ereaderChipText(status: Pick<EreaderStatus, 'sent' | 'in_sent_book'> | null): string | null {
  if (!status) return null
  if (status.sent) return 'On e-readers'
  if (status.in_sent_book) return `On e-readers · in ${status.in_sent_book.title}`
  return null
}

/** "1 chapter", "12 chapters". */
export function chaptersLabel(n: number): string {
  return `${n} chapter${n === 1 ? '' : 's'}`
}

/* ---------- diagrams → PNG ---------- */

/** The PNG's width; a tall diagram is capped at PNG_MAX_HEIGHT (narrower). */
export const PNG_WIDTH = 1200
export const PNG_MAX_HEIGHT = 1800
/** the server's limits: per PNG, per request (count), per request (body) */
export const PNG_MAX_BYTES = 2 * 1024 * 1024
export const IMAGES_PER_POST = 32
export const POST_MAX_BYTES = 15 * 1024 * 1024

/** The pixel size a diagram of w×h draws at, aspect kept. */
export function targetSize(w: number | undefined, h: number | undefined): { width: number; height: number } {
  if (!w || !h || !(w > 0) || !(h > 0)) return { width: PNG_WIDTH, height: Math.round(PNG_WIDTH * 0.6) }
  let width = PNG_WIDTH
  let height = Math.round((width * h) / w)
  if (height > PNG_MAX_HEIGHT) {
    height = PNG_MAX_HEIGHT
    width = Math.max(1, Math.round((height * w) / h))
  }
  return { width, height: Math.max(1, height) }
}

/** Bytes → base64 (chunked: String.fromCharCode takes a bounded arg list). */
export function bytesToBase64(bytes: Uint8Array): string {
  let bin = ''
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(bin)
}

/** Decoded size of a base64 string. */
export function base64Bytes(b64: string): number {
  const pad = b64.endsWith('==') ? 2 : b64.endsWith('=') ? 1 : 0
  return Math.floor((b64.length * 3) / 4) - pad
}

/** Uploads in batches the server takes: at most `count` images and about
 * `bytes` of JSON each (one oversized image still travels alone). */
export function pngBatches(images: DiagramPng[], count = IMAGES_PER_POST, bytes = POST_MAX_BYTES): DiagramPng[][] {
  const out: DiagramPng[][] = []
  let cur: DiagramPng[] = []
  let size = 0
  for (const im of images) {
    const n = im.png.length + im.block_id.length + im.source_hash.length + 96
    if (cur.length > 0 && (cur.length >= count || size + n > bytes)) {
      out.push(cur)
      cur = []
      size = 0
    }
    cur.push(im)
    size += n
  }
  if (cur.length) out.push(cur)
  return out
}

/** mermaid for e-ink: white ground, black lines and text, SVG text labels
 * (an HTML label is a foreignObject, which taints the canvas). */
export const EINK_MERMAID = {
  startOnLoad: false,
  theme: 'base' as const,
  darkMode: false,
  htmlLabels: false,
  flowchart: { htmlLabels: false },
  fontFamily: 'Helvetica, Arial, sans-serif',
  themeVariables: {
    darkMode: false,
    background: '#ffffff',
    fontFamily: 'Helvetica, Arial, sans-serif',
    fontSize: '16px',
    primaryColor: '#ffffff',
    primaryTextColor: '#000000',
    primaryBorderColor: '#000000',
    secondaryColor: '#eeeeee',
    secondaryTextColor: '#000000',
    secondaryBorderColor: '#000000',
    tertiaryColor: '#f7f7f7',
    tertiaryTextColor: '#000000',
    tertiaryBorderColor: '#000000',
    lineColor: '#000000',
    textColor: '#000000',
    edgeLabelBackground: '#ffffff',
    clusterBkg: '#f7f7f7',
    clusterBorder: '#000000',
    noteBkgColor: '#ffffff',
    noteTextColor: '#000000',
    noteBorderColor: '#000000',
    actorBkg: '#ffffff',
    actorBorder: '#000000',
    actorTextColor: '#000000',
    signalColor: '#000000',
    signalTextColor: '#000000',
  },
}

let einkSeq = 0

/** A diagram's SVG in light colours; null for a language the browser cannot draw. */
async function diagramSvg(d: EreaderDiagram): Promise<string | null> {
  const lang = d.lang.toLowerCase()
  if (lang === 'mermaid') {
    const mermaid = await loadMermaid()
    mermaid.initialize(EINK_MERMAID)
    return (await mermaid.render(`eink-${++einkSeq}`, d.source)).svg
  }
  if (lang === 'reladraw') {
    const r = await renderReladraw(d.source, 'light')
    if ('err' in r) throw new Error(r.err)
    return r.svg
  }
  if (lang === 'd2') {
    return (await api<{ svg: string }>('/api/render/d2', { method: 'POST', headers: JSON_HEADERS, body: JSON.stringify({ source: d.source }) })).svg
  }
  // vega / vega-lite: no renderer in the app; the book shows the source
  return null
}

/** The SVG with an explicit pixel size (and a viewBox to scale from). */
function sizedSvg(svg: string, width: number, height: number, natural: { width?: number; height?: number }): string {
  const xml = new DOMParser().parseFromString(svg, 'image/svg+xml')
  const root = xml.documentElement
  if (root.nodeName.toLowerCase() !== 'svg' || xml.getElementsByTagName('parsererror').length) throw new Error('not an SVG')
  if (!root.getAttribute('viewBox') && natural.width && natural.height) root.setAttribute('viewBox', `0 0 ${natural.width} ${natural.height}`)
  root.setAttribute('width', String(width))
  root.setAttribute('height', String(height))
  root.setAttribute('preserveAspectRatio', 'xMidYMid meet')
  root.removeAttribute('style')
  if (!root.getAttribute('xmlns')) root.setAttribute('xmlns', 'http://www.w3.org/2000/svg')
  return new XMLSerializer().serializeToString(root)
}

/** SVG → PNG on a white canvas. */
async function rasterize(svg: string): Promise<{ png: string; width: number; height: number }> {
  const natural = svgSize(svg)
  const { width, height } = targetSize(natural.width, natural.height)
  const img = new Image()
  img.decoding = 'async'
  // a data: URL, not a blob: one (Safari taints the canvas for blob SVGs)
  img.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sizedSvg(svg, width, height, natural))}`
  await img.decode()
  const canvas = document.createElement('canvas')
  canvas.width = width
  canvas.height = height
  const ctx = canvas.getContext('2d')
  if (!ctx) throw new Error('no 2d canvas')
  ctx.fillStyle = '#ffffff'
  ctx.fillRect(0, 0, width, height)
  ctx.drawImage(img, 0, 0, width, height)
  const url = canvas.toDataURL('image/png')
  return { png: url.slice(url.indexOf(',') + 1), width, height }
}

export type RenderPng = (d: EreaderDiagram) => Promise<{ png: string; width: number; height: number } | null>

/** The browser renderer: null when the diagram's language has none. */
export const renderDiagramPng: RenderPng = async (d) => {
  const svg = await diagramSvg(d)
  return svg ? rasterize(svg) : null
}

/** Can this page draw to a canvas at all? (Not in node, a test, a worker.) */
export function canRasterize(): boolean {
  return (
    typeof document !== 'undefined' &&
    typeof Image !== 'undefined' &&
    typeof DOMParser !== 'undefined' &&
    typeof HTMLCanvasElement !== 'undefined' &&
    typeof HTMLImageElement !== 'undefined' &&
    typeof HTMLImageElement.prototype.decode === 'function'
  )
}

export interface SyncResult {
  rendered: number
  failed: number
  stored: number
  ignored: number
}

// a (doc, missing hash set) is tried once per page load; one run per doc at a time
const tried = new Set<string>()
const running = new Map<string, Promise<SyncResult>>()

/** For tests: forget what was tried. */
export function resetDiagramSync() {
  tried.clear()
  running.clear()
}

/** Draw every diagram of the book that has no current PNG and upload them.
 * Fire and forget: never throws, never runs where nothing can draw. */
export function syncDiagramImages(docId: string, opts: { status?: EreaderStatus; render?: RenderPng } = {}): Promise<SyncResult> {
  const none: SyncResult = { rendered: 0, failed: 0, stored: 0, ignored: 0 }
  const render = opts.render ?? (canRasterize() ? renderDiagramPng : null)
  if (!render) return Promise.resolve(none)
  const cur = running.get(docId)
  if (cur) return cur
  const run = (async (): Promise<SyncResult> => {
    const res = { ...none }
    let status = opts.status
    if (!status) {
      try {
        status = await getEreaderStatus(docId)
      } catch {
        return res
      }
    }
    const seen = new Set<string>()
    const missing = (status.diagrams ?? []).filter((d) => {
      const k = `${d.block_id}\n${d.source_hash}`
      if (d.has_image || seen.has(k)) return false
      seen.add(k)
      return true
    })
    if (missing.length === 0) return res
    const key = `${docId}\n${[...seen].sort().join('\n')}`
    if (tried.has(key)) return res
    tried.add(key)
    const images: DiagramPng[] = []
    // one at a time: mermaid's config is global, and the page stays responsive
    for (const d of missing) {
      try {
        const out = await render(d)
        if (!out) continue
        if (base64Bytes(out.png) > PNG_MAX_BYTES) {
          res.failed++
          continue
        }
        images.push({ block_id: d.block_id, source_hash: d.source_hash, png: out.png, width: out.width, height: out.height })
        res.rendered++
      } catch (e) {
        // the server falls back to the diagram's source
        res.failed++
        console.warn('e-reader diagram not drawn', d.block_id, e)
      }
    }
    for (const batch of pngBatches(images)) {
      try {
        const r = await postEpubImages(docId, batch)
        res.stored += r.stored ?? 0
        res.ignored += r.ignored ?? 0
      } catch (e) {
        // a later open tries again
        tried.delete(key)
        console.warn('e-reader diagrams not uploaded', docId, e)
      }
    }
    return res
  })().finally(() => running.delete(docId))
  running.set(docId, run)
  return run
}
