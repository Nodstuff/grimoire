import { afterEach, describe, expect, it, vi } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import { CSRF_HEADER } from './auth'
import { ApiError } from './types'
import {
  Book,
  Ereader,
  PROGRESS_CHUNK,
  UploadToken,
  activeUploadTokens,
  createUploadToken,
  deleteUploadToken,
  listUploadTokens,
  uploadCurl,
  UPLOAD_BATCH_BYTES,
  UploadResult,
  activeEreaders,
  byteBatches,
  chunks,
  collectEpubs,
  coverUrl,
  createEreader,
  currentlyReading,
  deleteBook,
  deviceSetup,
  deleteEreader,
  fetchProgress,
  formatSize,
  initialsOf,
  listBooks,
  listEreaders,
  parseSeriesIndex,
  progressLabel,
  rotateEreader,
  searchFilter,
  skippedCount,
  updateBook,
  uploadBooks,
  uploadResultSummary,
} from './books'
import Books from './BookLibrary'
import Ereaders from './Ereaders'
import { buildCommands } from './commands'
import { buildPlaces } from './omni'
import { relTime } from './time'

/* ---------- fetch mock (auth.test.ts pattern) ---------- */

type Call = { url: string; init?: RequestInit }
let calls: Call[] = []

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

function mockFetch(routes: Record<string, (init?: RequestInit) => Response>) {
  calls = []
  vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
    calls.push({ url, init })
    const r = routes[url.split('?')[0]]
    if (!r) throw new Error(`unexpected fetch ${url}`)
    return r(init)
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
    // book_json: RFC 3339 strings, whole seconds, UTC "Z"
    added_at: '2026-01-01T09:30:00Z',
    updated_at: '2026-01-02T18:05:12Z',
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

describe('book dates', () => {
  it('added_at/updated_at are RFC 3339 and read as relative times', () => {
    const b = book({})
    const now = Date.parse('2026-01-03T09:30:00Z')
    expect(relTime(b.added_at, now)).toBe('2d ago')
    expect(relTime(b.updated_at, now)).toBe('15h ago')
  })
  it('searchFilter keeps the server order (newest first): no title sort', () => {
    const page = [book({ id: '3', title: 'Zed' }), book({ id: '2', title: 'Alpha' }), book({ id: '1', title: 'Mid' })]
    expect(searchFilter(page, '').map((b) => b.id)).toEqual(['3', '2', '1'])
  })
})

describe('parseSeriesIndex', () => {
  it('"" clears, numbers pass through, junk is refused before the server', () => {
    expect(parseSeriesIndex('')).toBeNull()
    expect(parseSeriesIndex('  ')).toBeNull()
    expect(parseSeriesIndex('2.5')).toBe(2.5)
    expect(() => parseSeriesIndex('two')).toThrow('series # must be a number')
  })
})

describe('deviceSetup', () => {
  it('builds the OPDS, path-form OPDS and kosync URLs from the origin', () => {
    expect(deviceSetup('https://taisce.null.ie/', 'tom', 'k7mq-4xpt')).toEqual({
      username: 'tom',
      opds: 'https://taisce.null.ie/opds/',
      opdsPath: 'https://taisce.null.ie/opds/k7mq-4xpt/',
      kosync: 'https://taisce.null.ie/kosync',
    })
  })
})

describe('activeEreaders', () => {
  it('drops revoked devices', () => {
    const d = (id: string, revoked_at: string | null): Ereader => ({
      id,
      label: id,
      stream_downloads: false,
      created_at: '2026-01-01T00:00:00Z',
      last_used_at: null,
      revoked_at,
    })
    expect(activeEreaders([d('a', null), d('b', '2026-02-01T00:00:00Z')]).map((x) => x.id)).toEqual(['a'])
  })
})

describe('the library is SERVER-mode only', () => {
  const ids = (library: boolean) => buildCommands({ library, docId: null, onAction: () => {} }).map((c) => c.id)
  it('LOCAL hides the Add EPUBs action; SERVER shows it', () => {
    expect(ids(false)).not.toContain('add-books')
    expect(ids(true)).toContain('add-books')
  })
  it('LOCAL hides the Books and E-readers places; SERVER shows them', () => {
    const places = (library: boolean) => buildPlaces({ library }).map((p) => p.id)
    for (const id of ['books', 'ereaders'] as const) {
      expect(places(false)).not.toContain(id)
      expect(places(true)).toContain(id)
    }
  })
  it('Add EPUBs shows the Books view (where the upload cards render)', () => {
    const acts: string[] = []
    const cmd = buildCommands({ library: true, docId: null, onAction: (a) => acts.push(a) }).find(
      (c) => c.id === 'add-books',
    )!
    vi.stubGlobal('document', { getElementById: () => null })
    cmd.run()
    expect(acts).toEqual(['books'])
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
  it('counts the skipped non-EPUB files', () => {
    expect(uploadResultSummary([{ filename: 'a.epub', status: 'added' }], 2)).toBe('1 added · 2 skipped (not EPUB)')
    expect(uploadResultSummary([], 0)).toBe('nothing uploaded')
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

describe('collectEpubs / chunks / byteBatches', () => {
  it('keeps only .epub, any case, and counts the rest', () => {
    const files = [new File(['x'], 'a.epub'), new File(['x'], 'b.EPUB'), new File(['x'], 'c.pdf')]
    expect(collectEpubs(files).map((f) => f.name)).toEqual(['a.epub', 'b.EPUB'])
    expect(skippedCount(files)).toBe(1)
  })
  it('batches by bytes: ≤ 20 MB per request, in order', () => {
    const MB = 1024 * 1024
    expect(UPLOAD_BATCH_BYTES).toBe(20 * MB)
    const sizes = [8 * MB, 8 * MB, 8 * MB, 1 * MB, 3 * MB]
    expect(byteBatches(sizes, (n) => n)).toEqual([[8 * MB, 8 * MB], [8 * MB, 1 * MB, 3 * MB]])
    // exactly the limit still fits
    expect(byteBatches([10 * MB, 10 * MB], (n) => n)).toEqual([[10 * MB, 10 * MB]])
  })
  it('a file over the limit goes alone', () => {
    const MB = 1024 * 1024
    expect(byteBatches([1 * MB, 45 * MB, 2 * MB, 30 * MB], (n) => n)).toEqual([[1 * MB], [45 * MB], [2 * MB], [30 * MB]])
    expect(byteBatches([45 * MB], (n) => n)).toEqual([[45 * MB]])
    expect(byteBatches([], (n: number) => n)).toEqual([])
  })
  it('also caps the file count per request', () => {
    expect(byteBatches([1, 1, 1, 1, 1], (n) => n, 100, 2)).toEqual([[1, 1], [1, 1], [1]])
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
  it(`asks at most ${PROGRESS_CHUNK} ids per request and merges the replies`, async () => {
    const ids = Array.from({ length: 450 }, (_, i) => `b${i}`)
    calls = []
    vi.stubGlobal('fetch', async (url: string) => {
      calls.push({ url })
      const asked = new URL(url, 'http://x').searchParams.get('ids')!.split(',')
      return json(200, { progress: Object.fromEntries(asked.map((id) => [id, null])) })
    })
    const out = await fetchProgress(ids)
    expect(calls.map((c) => new URL(c.url, 'http://x').searchParams.get('ids')!.split(',').length)).toEqual([200, 200, 50])
    expect(Object.keys(out)).toHaveLength(450)
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
  const dev: Ereader = {
    id: 'e1',
    label: 'kobo',
    stream_downloads: false,
    created_at: '2026-01-01T00:00:00Z',
    last_used_at: null,
    revoked_at: null,
  }
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

describe('upload tokens', () => {
  const tok: UploadToken = { id: 't1', label: 'chaptarr', created_at: '2026-01-01T00:00:00Z', last_used_at: null, revoked_at: null }
  it('lists, creates (the token comes back once), removes', async () => {
    mockFetch({
      '/api/upload-tokens': (init) =>
        init?.method === 'POST' ? json(201, { upload_token: tok, token: 'tbu_x' }) : json(200, { upload_tokens: [tok] }),
      '/api/upload-tokens/t1': () => new Response(null, { status: 204 }),
    })
    expect(await listUploadTokens()).toEqual([tok])
    expect((await createUploadToken('chaptarr')).token).toBe('tbu_x')
    await deleteUploadToken('t1')
    expect(calls.map((c) => [c.url, c.init?.method ?? 'GET'])).toEqual([
      ['/api/upload-tokens', 'GET'],
      ['/api/upload-tokens', 'POST'],
      ['/api/upload-tokens/t1', 'DELETE'],
    ])
    expect(JSON.parse(String(calls[1].init?.body))).toEqual({ label: 'chaptarr' })
  })
  it('hides removed tokens', () => {
    expect(activeUploadTokens([tok, { ...tok, id: 't2', revoked_at: '2026-01-02T00:00:00Z' }]).map((t) => t.id)).toEqual(['t1'])
  })
  it('the curl line names the token, a files field and this origin', () => {
    expect(uploadCurl('https://taisce.example/', 'tbu_abc')).toBe(
      'curl -H "Authorization: Bearer tbu_abc" -F files=@"$path" https://taisce.example/api/books',
    )
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
