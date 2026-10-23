import { afterEach, describe, expect, it, vi } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import { CSRF_HEADER } from './auth'
import { ApiError } from './types'
import {
  Book,
  UploadResult,
  chunks,
  collectEpubs,
  coverUrl,
  createEreader,
  currentlyReading,
  deleteBook,
  deleteEreader,
  fetchProgress,
  formatSize,
  initialsOf,
  listBooks,
  listEreaders,
  progressLabel,
  rotateEreader,
  searchFilter,
  sortByTitle,
  updateBook,
  uploadBooks,
  uploadResultSummary,
} from './books'
import Books from './Books'
import Ereaders from './Ereaders'

/* ---------- fetch mock (auth.test.ts pattern) ---------- */

type Call = { url: string; init?: RequestInit }
let calls: Call[] = []

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

function mockFetch(routes: Record<string, () => Response>) {
  calls = []
  vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
    calls.push({ url, init })
    const r = routes[url.split('?')[0]]
    if (!r) throw new Error(`unexpected fetch ${url}`)
    return r()
  })
}

const header = (c: Call, name: string) => new Headers(c.init?.headers).get(name)

afterEach(() => {
  vi.unstubAllGlobals()
})

function book(partial: Partial<Book>): Book {
  return {
    id: 'b1',
    title: 'A Book',
    author: null,
    series: null,
    series_index: null,
    book_identifier: 'uuid:x',
    original_filename: 'a-book.epub',
    size: 1024,
    has_cover: false,
    added_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...partial,
  }
}

/* ---------- pure helpers ---------- */

describe('searchFilter', () => {
  const shelf = [
    book({ id: '1', title: 'Dune', author: 'Frank Herbert', series: 'Dune' }),
    book({ id: '2', title: 'The Left Hand of Darkness', author: 'Ursula K. Le Guin' }),
    book({ id: '3', title: 'Ancillary Justice', author: 'Ann Leckie', series: 'Imperial Radch' }),
  ]
  it('matches title, author and series, any case, and trims', () => {
    expect(searchFilter(shelf, 'dune').map((b) => b.id)).toEqual(['1'])
    expect(searchFilter(shelf, 'LE GUIN').map((b) => b.id)).toEqual(['2'])
    expect(searchFilter(shelf, ' radch ').map((b) => b.id)).toEqual(['3'])
    expect(searchFilter(shelf, '')).toEqual(shelf)
    expect(searchFilter(shelf, '   ')).toEqual(shelf)
  })
  it('tolerates missing author/series', () => {
    expect(searchFilter([book({ title: 'Sans' })], 'sans')).toHaveLength(1)
    expect(searchFilter([book({ title: 'Sans' })], 'herbert')).toHaveLength(0)
  })
})

describe('sortByTitle', () => {
  it('orders case-insensitively and does not mutate', () => {
    const inList = [book({ title: 'beta' }), book({ title: 'Alpha' }), book({ title: 'Gamma' })]
    const out = sortByTitle(inList)
    expect(out.map((b) => b.title)).toEqual(['Alpha', 'beta', 'Gamma'])
    expect(inList.map((b) => b.title)).toEqual(['beta', 'Alpha', 'Gamma'])
  })
})

describe('uploadResultSummary', () => {
  const results: UploadResult[] = [
    { filename: 'a.epub', status: 'added' },
    { filename: 'b.epub', status: 'added' },
    { filename: 'c.epub', status: 'updated' },
    { filename: 'd.epub', status: 'already_here' },
    { filename: 'e.epub', status: 'clash', message: 'already shelved as “E”' },
    { filename: 'f.epub', status: 'error', message: 'not a zip' },
  ]
  it('counts every status', () => {
    const s = uploadResultSummary(results)
    expect(s).toContain('2 added')
    expect(s).toContain('1 updated')
    expect(s).toContain('1 already here')
    expect(s).toContain('1 clash')
    expect(s).toContain('1 failed')
  })
  it('puts clash/error messages first', () => {
    const s = uploadResultSummary(results)
    expect(s.indexOf('clash: e.epub — already shelved as “E”')).toBe(0)
    expect(s.indexOf('failed: f.epub — not a zip')).toBeLessThan(s.indexOf('2 added'))
  })
  it('an empty run says so', () => {
    expect(uploadResultSummary([])).toBe('nothing uploaded')
  })
})

describe('formatSize', () => {
  it('picks B / KB / MB / GB', () => {
    expect(formatSize(860)).toBe('860 B')
    expect(formatSize(2048)).toBe('2.0 KB')
    expect(formatSize(348_160)).toBe('340 KB')
    expect(formatSize(1_258_291)).toBe('1.2 MB')
    expect(formatSize(1_610_612_736)).toBe('1.5 GB')
    expect(formatSize(-1)).toBe('—')
  })
})

describe('progressLabel', () => {
  it('labels the three states', () => {
    expect(progressLabel(null)).toBe('unread')
    expect(progressLabel({ percentage: 0, status: 'unread' })).toBe('unread')
    expect(progressLabel({ percentage: 42.4, status: 'reading' })).toBe('42%')
    expect(progressLabel({ percentage: 100, status: 'finished' })).toBe('finished')
    expect(progressLabel({ percentage: 140, status: 'reading' })).toBe('100%')
  })
})

describe('initialsOf', () => {
  it('takes up to two uppercase initials', () => {
    expect(initialsOf('Ursula K. Le Guin')).toBe('UK')
    expect(initialsOf('herbert')).toBe('H')
    expect(initialsOf('  ')).toBe('')
  })
})

describe('collectEpubs / chunks', () => {
  it('keeps only .epub, any case', () => {
    const files = [new File(['x'], 'a.epub'), new File(['x'], 'b.EPUB'), new File(['x'], 'c.pdf')]
    expect(collectEpubs(files).map((f) => f.name)).toEqual(['a.epub', 'b.EPUB'])
  })
  it('batches in order', () => {
    expect(chunks([1, 2, 3, 4, 5], 2)).toEqual([[1, 2], [3, 4], [5]])
    expect(chunks([], 10)).toEqual([])
  })
})

/* ---------- API functions ---------- */

describe('listBooks', () => {
  it('builds the query string and unwraps the page', async () => {
    const page = { books: [book({})], next_before: 'b1' }
    mockFetch({ '/api/books': () => json(200, page) })
    expect(await listBooks()).toEqual(page)
    await listBooks({ search: 'dune', before: 'b1', limit: 30 })
    expect(calls.map((c) => c.url)).toEqual(['/api/books', '/api/books?search=dune&before=b1&limit=30'])
  })
})

describe('fetchProgress', () => {
  it('joins the ids and unwraps the map', async () => {
    const progress = { u1: { percentage: 42, device: 'kobo', updated_at: '2026-01-01T00:00:00Z', status: 'reading' }, u2: null }
    mockFetch({ '/api/books/progress': () => json(200, { progress }) })
    expect(await fetchProgress(['u1', 'u2'])).toEqual(progress)
    expect(calls[0].url).toBe('/api/books/progress?ids=u1,u2')
  })
  it('no ids means no request', async () => {
    mockFetch({})
    expect(await fetchProgress([])).toEqual({})
    expect(calls).toHaveLength(0)
  })
})

describe('currentlyReading', () => {
  it('unwraps the shelf', async () => {
    const row = { ...book({}), percentage: 12, device: 'kobo', updated_at: '2026-01-02T00:00:00Z' }
    mockFetch({ '/api/books/currently-reading': () => json(200, { books: [row] }) })
    expect(await currentlyReading()).toEqual([row])
  })
})

describe('updateBook', () => {
  it('PATCHes the patch as JSON', async () => {
    const updated = book({ title: 'New Title' })
    mockFetch({ '/api/books/b1': () => json(200, updated) })
    expect(await updateBook('b1', { title: 'New Title' })).toEqual(updated)
    expect(calls[0].init?.method).toBe('PATCH')
    expect(header(calls[0], CSRF_HEADER)).toBe('1')
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({ title: 'New Title' })
  })
})

describe('deleteBook', () => {
  it('a 204 resolves; an error body throws', async () => {
    mockFetch({ '/api/books/b1': () => new Response(null, { status: 204 }) })
    await deleteBook('b1')
    expect(calls[0].init?.method).toBe('DELETE')
    expect(header(calls[0], CSRF_HEADER)).toBe('1')
    mockFetch({ '/api/books/b1': () => json(404, { error: 'no such book' }) })
    const err = await deleteBook('b1').catch((e) => e)
    expect(err).toBeInstanceOf(ApiError)
    expect((err as ApiError).message).toBe('no such book')
  })
})

describe('uploadBooks', () => {
  it('posts one multipart part per file under the field name "files", letting the browser set the boundary', async () => {
    const results: UploadResult[] = [{ filename: 'a.epub', status: 'added' }]
    mockFetch({ '/api/books': () => json(200, { results }) })
    const out = await uploadBooks([new File(['x'], 'a.epub'), new File(['y'], 'b.epub')])
    expect(out).toEqual(results)
    expect(calls[0].init?.method).toBe('POST')
    expect(header(calls[0], CSRF_HEADER)).toBe('1')
    // no explicit content-type: fetch invents the multipart boundary
    expect(header(calls[0], 'content-type')).toBeNull()
    const form = calls[0].init?.body as FormData
    expect(form).toBeInstanceOf(FormData)
    expect(form.getAll('files').map((f) => (f as File).name)).toEqual(['a.epub', 'b.epub'])
  })
  it('surfaces an {error} reply', async () => {
    mockFetch({ '/api/books': () => json(413, { error: 'too large' }) })
    const err = await uploadBooks([new File(['x'], 'a.epub')]).catch((e) => e)
    expect(err).toBeInstanceOf(ApiError)
    expect((err as ApiError).message).toBe('too large')
  })
})

describe('coverUrl', () => {
  it('is the person-scoped route', () => {
    expect(coverUrl('b1')).toBe('/api/books/b1/cover')
  })
})

describe('ereaders', () => {
  const dev = { id: 'e1', label: 'kobo', stream_downloads: false, created_at: '2026-01-01T00:00:00Z', last_used_at: null }
  it('lists, creates, rotates, removes', async () => {
    mockFetch({
      '/api/ereaders': () => json(200, { ereaders: [dev] }),
      '/api/ereaders/e1/rotate': () => json(200, { password: 'pw2' }),
      '/api/ereaders/e1': () => new Response(null, { status: 204 }),
    })
    expect(await listEreaders()).toEqual([dev])
    expect(await rotateEreader('e1')).toEqual({ password: 'pw2' })
    await deleteEreader('e1')
    expect(calls.map((c) => [c.url, c.init?.method ?? 'GET'])).toEqual([
      ['/api/ereaders', 'GET'],
      ['/api/ereaders/e1/rotate', 'POST'],
      ['/api/ereaders/e1', 'DELETE'],
    ])
  })
  it('create posts the label and stream flag; the password comes back once', async () => {
    mockFetch({ '/api/ereaders': () => json(201, { ereader: dev, password: 'pw1' }) })
    const r = await createEreader('kobo', true)
    expect(r.password).toBe('pw1')
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({ label: 'kobo', stream_downloads: true })
  })
})

/* ---------- view smoke tests (initial paint only; effects do not run) ---------- */

describe('views render their shells', () => {
  it('Books', () => {
    const html = renderToString(createElement(Books, { dataVersion: 0 }))
    expect(html).toContain('books')
    expect(html).toContain('drop EPUBs')
  })
  it('Ereaders', () => {
    const html = renderToString(createElement(Ereaders, { dataVersion: 0 }))
    expect(html).toContain('e-readers')
    expect(html).toContain('add a device')
  })
})
