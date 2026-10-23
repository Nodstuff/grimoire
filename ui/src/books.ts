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
  /** a cover to show: the colour one while chosen, else the book's own */
  has_cover: boolean
  /** colour covers (BookCovers.tsx); absent from an older server */
  has_embedded_cover?: boolean
  has_color_cover?: boolean
  /** the book's own cover: true = greyscale, false = colour, null = not looked at */
  cover_greyscale?: boolean | null
  /** who decided which cover shows: nobody, or the person (theirs / colour) */
  cover_choice?: CoverChoice
  /** moves whenever the shown cover does (cache-buster) */
  cover_version?: number
  /** RFC 3339, e.g. "2026-01-01T00:00:00Z" */
  added_at: string
  /** RFC 3339 */
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
  /** RFC 3339 when the device was removed; null while it can sign in */
  revoked_at: string | null
}

/** A book upload token as the owner routes list it: never the secret. */
export interface UploadToken {
  id: string
  label: string
  created_at: string
  last_used_at: string | null
  /** RFC 3339 when the token was removed; null while it uploads */
  revoked_at: string | null
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

/** One /api/books/progress request carries at most this many ids. */
export const PROGRESS_CHUNK = 200

/** Reading progress for a set of book ids; ids without a row map to null.
 * Asked PROGRESS_CHUNK ids at a time (the URL stays short), merged. */
export async function fetchProgress(ids: string[]): Promise<Record<string, BookProgress | null>> {
  if (ids.length === 0) return {}
  const parts = await Promise.all(
    chunks(ids, PROGRESS_CHUNK).map((part) =>
      api<{ progress: Record<string, BookProgress | null> }>(
        `/api/books/progress?ids=${part.map(encodeURIComponent).join(',')}`,
      ).then((r) => r.progress),
    ),
  )
  return Object.assign({}, ...parts)
}

export function currentlyReading(): Promise<ReadingBook[]> {
  return api<{ books: ReadingBook[] }>('/api/books/currently-reading').then((r) => r.books)
}

export interface BookPatch {
  title?: string
  /** "" clears the author (the server stores none) */
  author?: string
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

/** The <img> src for a cover: the shown one, or (`embedded`) the book's
 * own even while a colour one is shown. `version` busts the browser cache
 * when the shown cover changes. */
export function coverUrl(id: string, version?: number, which?: 'embedded'): string {
  const p = new URLSearchParams()
  if (which) p.set('which', which)
  if (version) p.set('v', String(version))
  const q = p.toString()
  return `/api/books/${id}/cover${q ? `?${q}` : ''}`
}

/* ---------- colour covers ---------- */

export type CoverChoice = 'none' | 'embedded' | 'color'

/** A colour cover the server found on Open Library and validated. `url`
 * is the server's own route to the image: clients never fetch Open
 * Library, and choose a candidate by its id. */
export interface CoverCandidate {
  id: string
  source: 'isbn' | 'search'
  width: number
  height: number
  colorfulness: number
  url: string
}

export interface CoverCandidates {
  book: Book
  /** RFC 3339 of the cached lookup; null = never looked up */
  looked_up_at: string | null
  candidates: CoverCandidate[]
  /** Open Library did not answer: try again later */
  unavailable: boolean
}

export interface CoverReviewRow {
  book: Book
  /** false: no lookup yet (POST cover-candidates) */
  looked_up: boolean
  best: CoverCandidate | null
}

export interface CoverReview {
  /** greyscale books nobody chose a cover for */
  greyscale: number
  /** covers not looked at yet (shelved before detection) */
  unscanned: number
  books: CoverReviewRow[]
  /** POST only: covers looked at by this call */
  scanned?: number
}

/** The cached candidates (no lookup). */
export function coverCandidates(id: string): Promise<CoverCandidates> {
  return api<CoverCandidates>(`/api/books/${id}/cover-candidates`)
}

/** The person's own search terms for "Search with a different title…". */
export interface CoverSearchTerms {
  title: string
  author?: string
}

/** Ask Open Library (the server does; a recent lookup is reused unless
 * `refresh`). With `terms`, the server searches those instead of the
 * book's own title and author (always asked; it still builds the URL). */
export function findCoverCandidates(id: string, refresh = false, terms?: CoverSearchTerms): Promise<CoverCandidates> {
  const body = terms ? { refresh: true, title: terms.title, author: terms.author ?? '' } : { refresh }
  return api<CoverCandidates>(`/api/books/${id}/cover-candidates`, {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify(body),
  })
}

/** A title without its trailing series brackets ("Red Tithe (Carcharodons
 * Book 1)" → "Red Tithe"), the start of a different-title search. */
export function withoutSeries(title: string): string {
  let t = title.trim()
  for (;;) {
    const m = /\s*[([][^()[\]]*[)\]]$/.exec(t)
    if (!m || m.index === 0) return t
    t = t.slice(0, m.index).trimEnd()
  }
}

/** Terms from the different-title form, or null while the title is empty. */
export function coverSearchTerms(title: string, author: string): CoverSearchTerms | null {
  const t = title.trim()
  if (!t) return null
  const a = author.trim()
  return a ? { title: t, author: a } : { title: t }
}

/** Show a candidate, or (`'embedded'`) the book's own cover. `unlessChosen`
 * is a library pass's accept: a cover the person already chose answers 409
 * (code cover_chosen) and stays. */
export function chooseCover(id: string, pick: { candidate: string } | 'embedded', unlessChosen = false): Promise<Book> {
  const body = pick === 'embedded' ? { use: 'embedded' } : { candidate: pick.candidate }
  return api<Book>(`/api/books/${id}/cover`, {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify({ ...body, unless_chosen: unlessChosen }),
  })
}

export function coverReview(): Promise<CoverReview> {
  return api<CoverReview>('/api/books/cover-review')
}

/** Look at the next batch of covers shelved before greyscale detection. */
export function scanCoverReview(): Promise<CoverReview> {
  return api<CoverReview>('/api/books/cover-review', { method: 'POST', headers: JSON_HEADERS, body: '{}' })
}

/** A colour match is worth offering: a greyscale cover nobody chose. */
export function wantsColourCover(b: Pick<Book, 'cover_greyscale' | 'cover_choice' | 'has_color_cover'>): boolean {
  return b.cover_greyscale === true && (b.cover_choice ?? 'none') === 'none' && !b.has_color_cover
}

/** Books shows "Find colour covers" when a loaded book wants one, or has a
 * cover nobody has looked at yet (shelved before detection). */
export function showColourPass(books: Book[]): boolean {
  return books.some(
    (b) =>
      wantsColourCover(b) ||
      (b.cover_greyscale === null && !!b.has_embedded_cover && (b.cover_choice ?? 'none') === 'none' && !b.has_color_cover),
  )
}

/** The review rows "Accept all good matches" takes: a match, not skipped,
 * not already settled. */
export function goodMatches(rows: CoverReviewRow[], settled: Record<string, unknown>): CoverReviewRow[] {
  return rows.filter((r) => r.best && !(r.book.id in settled))
}

/** Run `fn` over `items` one at a time (Open Library asks for gentle
 * clients), stopping early when `stopped()` says so. */
export async function oneAtATime<T>(items: T[], fn: (t: T) => Promise<void>, stopped: () => boolean = () => false): Promise<void> {
  for (const t of items) {
    if (stopped()) return
    await fn(t)
  }
}

/* ---------- upload ---------- */

/** One POST carries at most this many bytes of EPUB; a single file larger
 * than that goes in a POST of its own. */
export const UPLOAD_BATCH_BYTES = 20 * 1024 * 1024
/** …and at most this many files (the server refuses more than 100). */
export const UPLOAD_BATCH_FILES = 100

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

/** The files of a drop/pick that are not EPUBs (skipped, and counted). */
export function skippedCount(files: FileList | File[]): number {
  const all = Array.from(files)
  return all.length - collectEpubs(all).length
}

/** Upload batches by BYTES, in order: each batch holds at most `maxBytes`
 * (and `maxFiles` files); a single item larger than `maxBytes` goes alone. */
export function byteBatches<T>(
  list: T[],
  sizeOf: (t: T) => number,
  maxBytes = UPLOAD_BATCH_BYTES,
  maxFiles = UPLOAD_BATCH_FILES,
): T[][] {
  const out: T[][] = []
  let cur: T[] = []
  let bytes = 0
  for (const t of list) {
    const n = Math.max(0, sizeOf(t))
    if (cur.length > 0 && (bytes + n > maxBytes || cur.length >= maxFiles)) {
      out.push(cur)
      cur = []
      bytes = 0
    }
    cur.push(t)
    bytes += n
  }
  if (cur.length > 0) out.push(cur)
  return out
}

/** Fixed-size slices, in order. */
export function chunks<T>(list: T[], size: number): T[][] {
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

/* ---------- book upload tokens ---------- */

export function listUploadTokens(): Promise<UploadToken[]> {
  return api<{ upload_tokens: UploadToken[] }>('/api/upload-tokens').then((r) => r.upload_tokens)
}

/** The token is in the reply exactly once — the caller must show it now. */
export function createUploadToken(label: string): Promise<{ upload_token: UploadToken; token: string }> {
  return api<{ upload_token: UploadToken; token: string }>('/api/upload-tokens', {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify({ label }),
  })
}

export function deleteUploadToken(id: string): Promise<void> {
  return del(`/api/upload-tokens/${id}`)
}

/** Live tokens only: removed ones stay in the server's list but not here. */
export function activeUploadTokens(list: UploadToken[]): UploadToken[] {
  return list.filter((t) => !t.revoked_at)
}

/** The ready-to-paste upload command for a fresh token. */
export function uploadCurl(origin: string, token: string): string {
  return `curl -H "Authorization: Bearer ${token}" -F files=@"$path" ${origin.replace(/\/+$/, '')}/api/books`
}

/* ---------- device setup (the show-once card) ---------- */

export interface DeviceSetup {
  /** the person's Taisce name: the username for OPDS basic auth and kosync */
  username: string | null
  /** OPDS catalog, basic auth (username + password) */
  opds: string
  /** OPDS catalog for readers without basic auth: the password in the path */
  opdsPath: string
  /** KOReader progress sync server */
  kosync: string
}

export function deviceSetup(origin: string, username: string | null, password: string): DeviceSetup {
  const base = origin.replace(/\/+$/, '')
  return {
    username,
    opds: `${base}/opds/`,
    opdsPath: `${base}/opds/${encodeURIComponent(password)}/`,
    kosync: `${base}/kosync`,
  }
}

/** A series # as typed: "" clears it, otherwise a finite number (the server
 * range-checks it and answers 400 when it is out of range). */
export function parseSeriesIndex(v: string): number | null {
  const t = v.trim()
  if (t === '') return null
  const n = Number(t)
  if (!Number.isFinite(n)) throw new Error('series # must be a number')
  return n
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

/** Devices that can still sign in (revoked rows are history, not devices). */
export function activeEreaders(list: Ereader[]): Ereader[] {
  return list.filter((d) => !d.revoked_at)
}

/** One toast line per upload run: clash/error messages first (they say why),
 * then the per-status counts, then the non-EPUB files that were skipped. */
export function uploadResultSummary(results: UploadResult[], skipped = 0): string {
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
  if (skipped > 0) tally.push(`${skipped} skipped (not EPUB)`)
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
