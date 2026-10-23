// The EPUB library: typed calls for the /api/books and /api/ereaders
// routes, plus the pure helpers the views lean on (search, sort, upload
// batching, size/progress labels). Covers render straight from
// /api/books/{id}/cover (a person-scoped 302 to S3) in an <img>.

import { withCsrf } from './auth'
import { api, ApiError } from './types'

/* ---------- the contract's shapes ---------- */

export interface Book {
  id: string
  title: string
  author: string | null
  series: string | null
  series_index: number | null
  book_identifier: string
  original_filename: string
  size: number
  has_cover: boolean
  added_at: string
  updated_at: string
}

export type ProgressStatus = 'unread' | 'reading' | 'finished'

export interface BookProgress {
  percentage: number
  device: string
  updated_at: string
  status: ProgressStatus
}

/** A GET /api/books/currently-reading row: the book plus its progress. */
export interface ReadingBook extends Book {
  percentage: number
  device: string
  updated_at: string
}

export type UploadStatus = 'added' | 'updated' | 'already_here' | 'clash' | 'error'

export interface UploadResult {
  filename: string
  status: UploadStatus
  book?: Book
  message?: string
}

export interface Ereader {
  id: string
  label: string
  stream_downloads: boolean
  created_at: string
  last_used_at: string | null
}

const JSON_HEADERS = { 'Content-Type': 'application/json' }

/* ---------- books ---------- */

export interface BookPage {
  books: Book[]
  next_before: string | null
}

/** One page of the library, newest first; `before` is the previous page's
 * next_before. */
export function listBooks(opts: { search?: string; before?: string | null; limit?: number } = {}): Promise<BookPage> {
  const p = new URLSearchParams()
  if (opts.search?.trim()) p.set('search', opts.search.trim())
  if (opts.before) p.set('before', opts.before)
  if (opts.limit) p.set('limit', String(opts.limit))
  const q = p.toString()
  return api<BookPage>(`/api/books${q ? `?${q}` : ''}`)
}

/** Reading progress for a set of book ids; ids without a row map to null. */
export function fetchProgress(ids: string[]): Promise<Record<string, BookProgress | null>> {
  if (ids.length === 0) return Promise.resolve({})
  return api<{ progress: Record<string, BookProgress | null> }>(
    `/api/books/progress?ids=${ids.map(encodeURIComponent).join(',')}`,
  ).then((r) => r.progress)
}

export function currentlyReading(): Promise<ReadingBook[]> {
  return api<{ books: ReadingBook[] }>('/api/books/currently-reading').then((r) => r.books)
}

export interface BookPatch {
  title?: string
  author?: string | null
  series?: string | null
  series_index?: number | null
}

export function updateBook(id: string, patch: BookPatch): Promise<Book> {
  return api<Book>(`/api/books/${id}`, {
    method: 'PATCH',
    headers: JSON_HEADERS,
    body: JSON.stringify(patch),
  })
}

/** The <img> src for a cover. */
export function coverUrl(id: string): string {
  return `/api/books/${id}/cover`
}

/* ---------- upload ---------- */

/** One POST carries at most this many files. */
export const UPLOAD_CHUNK = 10

/** POST /api/books as multipart form, field "files" once per file. The
 * browser must set the multipart boundary itself — do not set Content-Type. */
export async function uploadBooks(files: File[]): Promise<UploadResult[]> {
  const form = new FormData()
  for (const f of files) form.append('files', f, f.name)
  const r = await fetch('/api/books', withCsrf({ method: 'POST', body: form }))
  const j = await r.json().catch(() => null)
  if (j && typeof j === 'object' && 'error' in j) {
    const code = 'code' in j && typeof j.code === 'string' ? j.code : undefined
    throw new ApiError(String(j.error), code)
  }
  if (!r.ok || !j || typeof j !== 'object' || !Array.isArray((j as { results?: unknown }).results)) {
    throw new ApiError(`${r.status} ${r.statusText}`.trim())
  }
  return (j as { results: UploadResult[] }).results
}

/** Of a pick/drop, the files that are EPUBs (the rest cannot be shelved). */
export function collectEpubs(files: FileList | File[]): File[] {
  return Array.from(files).filter((f) => /\.epub$/i.test(f.name))
}

/** Upload batches: one POST per batch, sent in order. */
export function chunks<T>(list: T[], size = UPLOAD_CHUNK): T[][] {
  const out: T[][] = []
  for (let i = 0; i < list.length; i += size) out.push(list.slice(i, i + size))
  return out
}

/* ---------- deletes (204: api() insists on a JSON body, so these go their
   own way — the same shape as share.ts's call) ---------- */

async function del(path: string): Promise<void> {
  const r = await fetch(path, withCsrf({ method: 'DELETE' }))
  if (r.status === 204) return
  const j = await r.json().catch(() => null)
  if (j && typeof j === 'object' && 'error' in j) {
    const code = 'code' in j && typeof j.code === 'string' ? j.code : undefined
    throw new ApiError(String(j.error), code)
  }
  throw new ApiError(`${r.status} ${r.statusText}`.trim())
}

export function deleteBook(id: string): Promise<void> {
  return del(`/api/books/${id}`)
}

/* ---------- e-readers ---------- */

export function listEreaders(): Promise<Ereader[]> {
  return api<{ ereaders: Ereader[] }>('/api/ereaders').then((r) => r.ereaders)
}

/** The password is in the reply exactly once — the caller must show it now. */
export function createEreader(label: string, stream_downloads = false): Promise<{ ereader: Ereader; password: string }> {
  return api<{ ereader: Ereader; password: string }>('/api/ereaders', {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify({ label, stream_downloads }),
  })
}

/** A fresh password, shown once. */
export function rotateEreader(id: string): Promise<{ password: string }> {
  return api<{ password: string }>(`/api/ereaders/${id}/rotate`, {
    method: 'POST',
    headers: JSON_HEADERS,
    body: '{}',
  })
}

export function deleteEreader(id: string): Promise<void> {
  return del(`/api/ereaders/${id}`)
}

/* ---------- pure helpers ---------- */

/** Library search: title, author or series contains the query (any case). */
export function searchFilter(books: Book[], q: string): Book[] {
  const needle = q.trim().toLowerCase()
  if (!needle) return books
  return books.filter((b) =>
    [b.title, b.author ?? '', b.series ?? ''].some((s) => s.toLowerCase().includes(needle)),
  )
}

/** Title order, case-insensitive; the input list is not mutated. */
export function sortByTitle(books: Book[]): Book[] {
  return [...books].sort((a, b) => a.title.localeCompare(b.title, undefined, { sensitivity: 'base' }))
}

/** One toast line per upload run: clash/error messages first (they say why),
 * then the per-status counts. */
export function uploadResultSummary(results: UploadResult[]): string {
  const counts = new Map<UploadStatus, number>()
  for (const r of results) counts.set(r.status, (counts.get(r.status) ?? 0) + 1)
  const problems = results
    .filter((r) => (r.status === 'clash' || r.status === 'error') && r.message)
    .map((r) => `${r.status === 'clash' ? 'clash' : 'failed'}: ${r.filename} — ${r.message}`)
  const labels: [UploadStatus, string][] = [
    ['added', 'added'],
    ['updated', 'updated'],
    ['already_here', 'already here'],
    ['clash', 'clash'],
    ['error', 'failed'],
  ]
  const tally = labels.flatMap(([status, label]) => {
    const n = counts.get(status) ?? 0
    return n ? [`${n} ${label}`] : []
  })
  return [...problems, tally.length ? tally.join(' · ') : 'nothing uploaded'].join(' · ')
}

/** Human file size: "860 B", "340 KB", "1.2 MB", "1.5 GB". */
export function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes < 1024) return `${Math.round(bytes)} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`
  return `${(mb / 1024).toFixed(1)} GB`
}

/** The chip/bar label: "42%", "finished", "unread". */
export function progressLabel(p: Pick<BookProgress, 'percentage' | 'status'> | null | undefined): string {
  if (!p || p.status === 'unread') return 'unread'
  if (p.status === 'finished') return 'finished'
  const pct = Math.max(0, Math.min(100, Math.round(p.percentage)))
  return `${pct}%`
}

/** Up to two initials for the no-cover title card ("Ursula K. Le Guin" → "UK"). */
export function initialsOf(name: string): string {
  return name
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((w) => w[0]!.toUpperCase())
    .join('')
}
