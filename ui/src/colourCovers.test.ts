// Colour covers: the calls (books.ts), the pure helpers, and the pieces of
// ColourCovers.tsx that render without effects.
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import { CSRF_HEADER } from './auth'
import { ApiError } from './types'
import {
  type Book,
  type CoverReviewRow,
  chooseCover,
  coverCandidates,
  coverReview,
  coverUrl,
  findCoverCandidates,
  goodMatches,
  oneAtATime,
  scanCoverReview,
  showColourPass,
  wantsColourCover,
} from './books'
import { ColourCoverPanel, candidateLabel, serially, settledText } from './ColourCovers'

type Call = { url: string; init?: RequestInit }
let calls: Call[] = []
const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
function mockFetch(routes: Record<string, (init?: RequestInit) => Response>) {
  calls = []
  vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
    calls.push({ url, init })
    const r = routes[`${(init?.method ?? 'GET').toUpperCase()} ${url}`]
    if (!r) throw new Error(`unexpected fetch ${init?.method ?? 'GET'} ${url}`)
    return r(init)
  })
}
afterEach(() => vi.unstubAllGlobals())

const book = (extra: Partial<Book> = {}): Book => ({
  id: 'b1',
  title: 'Horus Rising',
  author: 'Dan Abnett',
  series: null,
  series_index: null,
  book_identifier: 'urn:isbn:9781849706186',
  original_filename: 'horus.epub',
  size: 1000,
  has_cover: true,
  has_embedded_cover: true,
  has_color_cover: false,
  cover_greyscale: true,
  cover_choice: 'none',
  cover_version: 0,
  added_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  ...extra,
})
const cand = { id: 'c1', source: 'isbn' as const, width: 330, height: 500, colorfulness: 41.2, url: '/api/books/b1/cover-candidates/c1' }

describe('coverUrl', () => {
  it('busts the cache by version and can ask for the embedded cover', () => {
    expect(coverUrl('b1')).toBe('/api/books/b1/cover')
    expect(coverUrl('b1', 0)).toBe('/api/books/b1/cover')
    expect(coverUrl('b1', 1700000000)).toBe('/api/books/b1/cover?v=1700000000')
    expect(coverUrl('b1', 5, 'embedded')).toBe('/api/books/b1/cover?which=embedded&v=5')
  })
})

describe('which books want a colour cover', () => {
  it('a greyscale cover nobody chose, and no colour one yet', () => {
    expect(wantsColourCover(book())).toBe(true)
    expect(wantsColourCover(book({ cover_greyscale: false }))).toBe(false)
    expect(wantsColourCover(book({ cover_greyscale: null }))).toBe(false)
    expect(wantsColourCover(book({ cover_choice: 'embedded' }))).toBe(false)
    expect(wantsColourCover(book({ cover_choice: 'color', has_color_cover: true }))).toBe(false)
    // an older server's book carries none of the fields
    expect(wantsColourCover({ cover_greyscale: undefined, cover_choice: undefined, has_color_cover: undefined })).toBe(false)
  })
  it('the pass shows for those, and for covers not looked at yet', () => {
    expect(showColourPass([book({ cover_greyscale: false })])).toBe(false)
    expect(showColourPass([book({ cover_greyscale: false }), book()])).toBe(true)
    expect(showColourPass([book({ cover_greyscale: null })])).toBe(true)
    expect(showColourPass([book({ cover_greyscale: null, has_embedded_cover: false, has_cover: false })])).toBe(false)
    expect(showColourPass([book({ cover_greyscale: null, cover_choice: 'embedded' })])).toBe(false)
    expect(showColourPass([])).toBe(false)
  })
  it('accept-all takes rows with a match that are not settled', () => {
    const rows: CoverReviewRow[] = [
      { book: book({ id: 'a' }), looked_up: true, best: cand },
      { book: book({ id: 'b' }), looked_up: true, best: null },
      { book: book({ id: 'c' }), looked_up: false, best: null },
      { book: book({ id: 'd' }), looked_up: true, best: cand },
    ]
    expect(goodMatches(rows, {}).map((r) => r.book.id)).toEqual(['a', 'd'])
    expect(goodMatches(rows, { d: 'skipped' }).map((r) => r.book.id)).toEqual(['a'])
  })
})

describe('the calls', () => {
  it('reads the cache with a GET and finds with a POST (CSRF, refresh)', async () => {
    const reply = { book: book(), looked_up_at: null, candidates: [cand], unavailable: false }
    mockFetch({
      'GET /api/books/b1/cover-candidates': () => json(200, reply),
      'POST /api/books/b1/cover-candidates': () => json(200, reply),
    })
    expect((await coverCandidates('b1')).candidates[0].id).toBe('c1')
    await findCoverCandidates('b1')
    await findCoverCandidates('b1', true)
    expect(calls.map((c) => c.init?.body ?? null)).toEqual([null, '{"refresh":false}', '{"refresh":true}'])
    expect(new Headers(calls[1].init?.headers).get(CSRF_HEADER)).toBe('1')
  })
  it('chooses a candidate by id or the embedded cover; a pass says unless_chosen', async () => {
    mockFetch({ 'POST /api/books/b1/cover': () => json(200, book({ cover_choice: 'color', has_color_cover: true })) })
    await chooseCover('b1', { candidate: 'c1' })
    await chooseCover('b1', 'embedded')
    await chooseCover('b1', { candidate: 'c1' }, true)
    expect(calls.map((c) => JSON.parse(String(c.init?.body)))).toEqual([
      { candidate: 'c1', unless_chosen: false },
      { use: 'embedded', unless_chosen: false },
      { candidate: 'c1', unless_chosen: true },
    ])
  })
  it('a pass meeting a cover the person chose gets cover_chosen', async () => {
    mockFetch({ 'POST /api/books/b1/cover': () => json(409, { error: 'already chosen', code: 'cover_chosen' }) })
    const e = await chooseCover('b1', { candidate: 'c1' }, true).catch((x) => x)
    expect(e).toBeInstanceOf(ApiError)
    expect((e as ApiError).code).toBe('cover_chosen')
  })
  it('the review: GET reads, POST looks at the next batch', async () => {
    const r = { greyscale: 1, unscanned: 3, books: [] }
    mockFetch({
      'GET /api/books/cover-review': () => json(200, r),
      'POST /api/books/cover-review': () => json(200, { ...r, unscanned: 0, scanned: 3 }),
    })
    expect((await coverReview()).unscanned).toBe(3)
    expect((await scanCoverReview()).scanned).toBe(3)
  })
})

describe('pacing', () => {
  it('oneAtATime runs in order, one at a time, and stops when told', async () => {
    const log: string[] = []
    let live = 0
    let peak = 0
    await oneAtATime([1, 2, 3], async (n) => {
      live++
      peak = Math.max(peak, live)
      await new Promise((r) => setTimeout(r, 1))
      log.push(String(n))
      live--
    })
    expect(log).toEqual(['1', '2', '3'])
    expect(peak).toBe(1)
    const seen: number[] = []
    await oneAtATime([1, 2, 3], async (n) => void seen.push(n), () => seen.length >= 2)
    expect(seen).toEqual([1, 2])
  })
  it('serially never overlaps two lookups, and a failure does not jam the lane', async () => {
    let live = 0
    let peak = 0
    const job = (fail: boolean) => async () => {
      live++
      peak = Math.max(peak, live)
      await new Promise((r) => setTimeout(r, 2))
      live--
      if (fail) throw new Error('boom')
      return 'ok'
    }
    const out = await Promise.allSettled([serially(job(false)), serially(job(true)), serially(job(false))])
    expect(out.map((o) => o.status)).toEqual(['fulfilled', 'rejected', 'fulfilled'])
    expect(peak).toBe(1)
  })
})

describe('the details panel', () => {
  it('offers a colour cover for a black-and-white one', () => {
    const html = renderToString(createElement(ColourCoverPanel, { book: book(), onPatched: () => {} }))
    expect(html).toContain('This cover is black and white.')
    expect(html).toContain('Find a colour cover…')
  })
  it('offers to revert while a colour one shows', () => {
    const html = renderToString(createElement(ColourCoverPanel, { book: book({ has_color_cover: true, cover_choice: 'color' }), onPatched: () => {} }))
    expect(html).toContain('Revert to the book’s own cover')
    expect(html).toContain('The book itself is unchanged.')
    expect(html).not.toContain('Find a colour cover')
  })
  it('a colour cover on a coverless book is removed, not reverted', () => {
    const html = renderToString(createElement(ColourCoverPanel, { book: book({ has_color_cover: true, cover_choice: 'color', has_embedded_cover: false }), onPatched: () => {} }))
    expect(html).toContain('Remove the colour cover')
  })
  it('labels and settled states read plainly', () => {
    expect(candidateLabel(cand)).toBe('This edition · 330×500')
    expect(candidateLabel({ ...cand, source: 'search' })).toBe('Title match · 330×500')
    expect(settledText('kept')).toBe('Kept: you chose this cover')
    expect(settledText('accepted')).toBe('Colour cover set')
  })
})
