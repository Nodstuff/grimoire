import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import { CSRF_HEADER } from './auth'
import {
  DiagramPng,
  EreaderDiagram,
  EreaderStatus,
  IMAGES_PER_POST,
  PNG_MAX_HEIGHT,
  PNG_WIDTH,
  SentBook,
  base64Bytes,
  bytesToBase64,
  canRasterize,
  chaptersLabel,
  downloadEpub,
  ereaderChipText,
  ereaderStatusOf,
  fallbackEpubName,
  filenameFromDisposition,
  getEreaderStatus,
  listSentBooks,
  loadEreaderStatus,
  pngBatches,
  postEreader,
  resetDiagramSync,
  sendLabel,
  setSending,
  syncDiagramImages,
  targetSize,
} from './ereader'
import { ereaderCommands } from './commands'
import { SentShelf, sentBookLine } from './BookLibrary'
import EreaderChip from './EreaderChip'

/* ---------- fetch mock (books.test.ts pattern) ---------- */

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
const body = (c: Call) => JSON.parse(String(c.init?.body))

function status(p: Partial<EreaderStatus> = {}): EreaderStatus {
  return { sent: false, tagged: false, in_sent_book: null, filename: 'Taisce - Doc.epub', chapters: 1, diagrams: [], ...p }
}

function diagram(i: number, p: Partial<EreaderDiagram> = {}): EreaderDiagram {
  return { block_id: `b${i}`, lang: 'mermaid', source: `graph TD; A${i}-->B`, source_hash: `h${i}`, has_image: false, ...p }
}

beforeEach(() => resetDiagramSync())
afterEach(() => {
  vi.unstubAllGlobals()
})

/* ---------- Download EPUB ---------- */

describe('filenameFromDisposition', () => {
  it('prefers filename* (UTF-8, percent-encoded) over filename', () => {
    expect(
      filenameFromDisposition(`attachment; filename="Taisce - Cafe.epub"; filename*=UTF-8''Taisce%20-%20Caf%C3%A9.epub`),
    ).toBe('Taisce - Café.epub')
  })
  it('reads a quoted filename, unescaping', () => {
    expect(filenameFromDisposition('attachment; filename="Taisce - A \\"B\\".epub"')).toBe('Taisce - A "B".epub')
  })
  it('reads a bare token, and falls back from a malformed filename*', () => {
    expect(filenameFromDisposition('attachment; filename=book.epub')).toBe('book.epub')
    expect(filenameFromDisposition(`attachment; filename*=UTF-8''%E0%A4%A; filename="ok.epub"`)).toBe('ok.epub')
  })
  it('null without one', () => {
    expect(filenameFromDisposition(null)).toBeNull()
    expect(filenameFromDisposition('attachment')).toBeNull()
  })
})

describe('fallbackEpubName', () => {
  it('Taisce - <Title>.epub, path characters dashed', () => {
    expect(fallbackEpubName('Notes')).toBe('Taisce - Notes.epub')
    expect(fallbackEpubName('a/b: c?')).toBe('Taisce - a-b- c-.epub')
    expect(fallbackEpubName('  ')).toBe('Taisce - Untitled.epub')
  })
})

describe('downloadEpub', () => {
  it('fetches the doc’s EPUB with credentials and saves it under the header’s name', async () => {
    mockFetch({
      '/api/docs/d%201/epub': () =>
        new Response(new Uint8Array([80, 75, 3, 4]), {
          status: 200,
          headers: { 'content-type': 'application/epub+zip', 'content-disposition': `attachment; filename="x.epub"; filename*=UTF-8''Taisce%20-%20Doc.epub` },
        }),
    })
    const saved: { name: string; size: number; type: string }[] = []
    const name = await downloadEpub('d 1', 'Doc', (b, n) => saved.push({ name: n, size: b.size, type: b.type }))
    expect(name).toBe('Taisce - Doc.epub')
    expect(saved).toEqual([{ name: 'Taisce - Doc.epub', size: 4, type: 'application/epub+zip' }])
    expect(calls[0].init?.credentials).toBe('same-origin')
  })
  it('falls back to the title when the header is absent', async () => {
    mockFetch({ '/api/docs/d1/epub': () => new Response(new Uint8Array([1]), { status: 200 }) })
    const names: string[] = []
    await downloadEpub('d1', 'My Doc', (_b, n) => names.push(n))
    expect(names).toEqual(['Taisce - My Doc.epub'])
  })
  it('throws the server’s error and saves nothing', async () => {
    mockFetch({ '/api/docs/d1/epub': () => json(404, { error: 'no such doc', code: 'not_found' }) })
    const save = vi.fn()
    await expect(downloadEpub('d1', 'Doc', save)).rejects.toThrow('no such doc')
    expect(save).not.toHaveBeenCalled()
  })
})

/* ---------- status and sending ---------- */

describe('status calls', () => {
  it('GET and POST /api/docs/{id}/ereader; the POST carries {send} and the CSRF header', async () => {
    mockFetch({
      '/api/docs/d1/ereader': (init) => json(200, status({ sent: init?.method === 'POST' && JSON.parse(String(init.body)).send })),
    })
    expect((await getEreaderStatus('d1')).sent).toBe(false)
    expect(calls[0].init?.method ?? 'GET').toBe('GET')
    const s = await postEreader('d1', true)
    expect(s.sent).toBe(true)
    expect(calls[1].init?.method).toBe('POST')
    expect(body(calls[1])).toEqual({ send: true })
    expect(header(calls[1], CSRF_HEADER)).toBe('1')
  })
  it('loadEreaderStatus stores the answer, and null on a daemon without the route', async () => {
    mockFetch({ '/api/docs/d1/ereader': () => json(200, status({ sent: true })), '/api/docs/d2/ereader': () => new Response('<html>', { status: 404 }) })
    expect((await loadEreaderStatus('d1'))?.sent).toBe(true)
    expect(ereaderStatusOf('d1')?.sent).toBe(true)
    expect(await loadEreaderStatus('d2')).toBeNull()
    expect(ereaderStatusOf('d2')).toBeNull()
    expect(ereaderStatusOf(null)).toBeNull()
  })
  it('setSending updates the store; with nothing to draw with (node), no diagrams are uploaded', async () => {
    mockFetch({ '/api/docs/d3/ereader': () => json(200, status({ sent: true, diagrams: [diagram(1)] })) })
    expect(canRasterize()).toBe(false)
    await setSending('d3', true)
    expect(ereaderStatusOf('d3')?.sent).toBe(true)
    await new Promise((r) => setTimeout(r, 0))
    expect(calls.map((c) => c.url)).toEqual(['/api/docs/d3/ereader'])
  })
  it('listSentBooks reads docs', async () => {
    mockFetch({ '/api/ereader/sent': () => json(200, { docs: [sent({})] }) })
    expect((await listSentBooks()).map((b) => b.id)).toEqual(['s1'])
  })
})

/* ---------- diagram sync ---------- */

const fakePng = (d: EreaderDiagram) => Promise.resolve({ png: bytesToBase64(new TextEncoder().encode(`png:${d.block_id}`)), width: 1200, height: 600 })

describe('syncDiagramImages', () => {
  it('draws only diagrams without a current image, once per (block, hash), and posts them', async () => {
    mockFetch({ '/api/docs/d1/epub-images': (init) => json(200, { stored: JSON.parse(String(init?.body)).images.length, ignored: 0 }) })
    const render = vi.fn(fakePng)
    const r = await syncDiagramImages('d1', {
      status: status({ sent: true, diagrams: [diagram(1), diagram(2, { has_image: true }), diagram(1), diagram(3, { lang: 'vega' })] }),
      render: (d) => (d.lang === 'vega' ? Promise.resolve(null) : render(d)),
    })
    expect(render.mock.calls.map((c) => c[0].block_id)).toEqual(['b1'])
    expect(r).toEqual({ rendered: 1, failed: 0, stored: 1, ignored: 0 })
    expect(calls).toHaveLength(1)
    expect(calls[0].init?.method).toBe('POST')
    expect(header(calls[0], CSRF_HEADER)).toBe('1')
    const sentImages: DiagramPng[] = body(calls[0]).images
    expect(sentImages).toEqual([{ block_id: 'b1', source_hash: 'h1', png: btoa('png:b1'), width: 1200, height: 600 }])
    expect(sentImages[0].png.startsWith('data:')).toBe(false)
  })
  it('batches 32 images per request', async () => {
    mockFetch({ '/api/docs/d1/epub-images': (init) => json(200, { stored: JSON.parse(String(init?.body)).images.length, ignored: 1 }) })
    const diagrams = Array.from({ length: 40 }, (_, i) => diagram(i))
    const r = await syncDiagramImages('d1', { status: status({ diagrams }), render: fakePng })
    expect(calls.map((c) => body(c).images.length)).toEqual([IMAGES_PER_POST, 8])
    expect(r.stored).toBe(40)
    expect(r.ignored).toBe(2)
  })
  it('skips a diagram that fails to draw; the rest still upload', async () => {
    mockFetch({ '/api/docs/d1/epub-images': () => json(200, { stored: 1, ignored: 0 }) })
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const r = await syncDiagramImages('d1', {
      status: status({ diagrams: [diagram(1), diagram(2)] }),
      render: (d) => (d.block_id === 'b1' ? Promise.reject(new Error('Parse error')) : fakePng(d)),
    })
    warn.mockRestore()
    expect(r).toMatchObject({ rendered: 1, failed: 1 })
    expect(body(calls[0]).images.map((i: DiagramPng) => i.block_id)).toEqual(['b2'])
  })
  it('tries a hash set once per page load; a changed source runs again', async () => {
    mockFetch({ '/api/docs/d1/epub-images': () => json(200, { stored: 1, ignored: 0 }) })
    const render = vi.fn(fakePng)
    await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1)] }), render })
    await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1)] }), render })
    expect(render).toHaveBeenCalledTimes(1)
    await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1, { source_hash: 'h1b' })] }), render })
    expect(render).toHaveBeenCalledTimes(2)
    expect(calls).toHaveLength(2)
  })
  it('a failed upload is retried on the next call', async () => {
    let fail = true
    mockFetch({ '/api/docs/d1/epub-images': () => (fail ? json(500, { error: 'boom' }) : json(200, { stored: 1, ignored: 0 })) })
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1)] }), render: fakePng })
    fail = false
    const r = await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1)] }), render: fakePng })
    warn.mockRestore()
    expect(r.stored).toBe(1)
  })
  it('GETs the status itself when not given one', async () => {
    mockFetch({
      '/api/docs/d1/ereader': () => json(200, status({ sent: true, diagrams: [diagram(1), diagram(2, { has_image: true })] })),
      '/api/docs/d1/epub-images': () => json(200, { stored: 1, ignored: 0 }),
    })
    const r = await syncDiagramImages('d1', { render: fakePng })
    expect(calls.map((c) => c.url)).toEqual(['/api/docs/d1/ereader', '/api/docs/d1/epub-images'])
    expect(r.stored).toBe(1)
  })
  it('nothing missing: no POST; no DOM and no renderer: no fetch at all', async () => {
    mockFetch({})
    await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1, { has_image: true })] }), render: fakePng })
    await syncDiagramImages('d2')
    expect(calls).toHaveLength(0)
  })
  it('drops a PNG over 2 MB', async () => {
    mockFetch({})
    const big = 'A'.repeat(Math.ceil(((2 * 1024 * 1024 + 10) * 4) / 3))
    const r = await syncDiagramImages('d1', { status: status({ diagrams: [diagram(1)] }), render: () => Promise.resolve({ png: big, width: 1, height: 1 }) })
    expect(r).toMatchObject({ rendered: 0, failed: 1 })
    expect(calls).toHaveLength(0)
  })
})

describe('pure helpers', () => {
  it('pngBatches splits by count and by bytes; a lone big image travels alone', () => {
    const im = (i: number, n: number): DiagramPng => ({ block_id: `b${i}`, source_hash: 'h', png: 'x'.repeat(n), width: 1, height: 1 })
    expect(pngBatches([im(1, 10), im(2, 10), im(3, 10)], 2).map((b) => b.length)).toEqual([2, 1])
    expect(pngBatches([im(1, 500), im(2, 500), im(3, 500)], 32, 1200).map((b) => b.length)).toEqual([2, 1])
    expect(pngBatches([im(1, 5000), im(2, 10)], 32, 1200).map((b) => b.length)).toEqual([1, 1])
    expect(pngBatches([])).toEqual([])
  })
  it('targetSize: 1200 wide, aspect kept, tall ones capped', () => {
    expect(targetSize(600, 300)).toEqual({ width: PNG_WIDTH, height: 600 })
    expect(targetSize(100, 1000)).toEqual({ width: 180, height: PNG_MAX_HEIGHT })
    expect(targetSize(undefined, 50)).toEqual({ width: PNG_WIDTH, height: 720 })
  })
  it('bytesToBase64 / base64Bytes', () => {
    const bytes = new Uint8Array(70_000).map((_, i) => i % 251)
    const b64 = bytesToBase64(bytes)
    expect(base64Bytes(b64)).toBe(70_000)
    expect(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))).toEqual(bytes)
    expect(base64Bytes(btoa('ab'))).toBe(2)
  })
  it('labels', () => {
    expect(sendLabel({ sent: false }, false)).toBe('Send to e-readers')
    expect(sendLabel({ sent: false }, true)).toBe('Send folder to e-readers')
    expect(sendLabel({ sent: true }, true)).toBe('Stop sending to e-readers')
    expect(ereaderChipText(null)).toBeNull()
    expect(ereaderChipText(status())).toBeNull()
    expect(ereaderChipText(status({ sent: true }))).toBe('On e-readers')
    expect(ereaderChipText(status({ in_sent_book: { id: 'f', title: 'Handbook' } }))).toBe('On e-readers · in Handbook')
    expect(chaptersLabel(1)).toBe('1 chapter')
    expect(chaptersLabel(12)).toBe('12 chapters')
  })
})

/* ---------- ⌘K rows ---------- */

describe('ereaderCommands', () => {
  const labels = (s: EreaderStatus | null, kids = false) => ereaderCommands('d1', 'Doc', kids, s, () => {}).map((c) => c.label)
  it('Download EPUB always; Send / Stop only once the status is known', () => {
    expect(labels(null)).toEqual(['Download EPUB'])
    expect(labels(status())).toEqual(['Download EPUB', 'Send to e-readers'])
    expect(labels(status(), true)).toEqual(['Download EPUB', 'Send folder to e-readers'])
    expect(labels(status({ sent: true }))).toEqual(['Download EPUB', 'Stop sending to e-readers'])
  })
  it('the rows list under This doc', () => {
    expect(ereaderCommands('d1', 'Doc', false, status(), () => {}).every((c) => c.scope === 'doc')).toBe(true)
  })
})

/* ---------- views ---------- */

function sent(p: Partial<SentBook>): SentBook {
  return { id: 's1', title: 'Handbook', filename: 'Taisce - Handbook.epub', workspace: 'Work', chapters: 12, updated_at: '2026-10-01T09:00:00Z', size: 1_258_291, progress: null, ...p }
}

describe('From Taisce shelf', () => {
  const render = (list: SentBook[] | null) => renderToString(createElement(SentShelf, { list, onDownload: () => {}, onStop: () => {} }))
  it('hidden when the list is empty or absent', () => {
    expect(render(null)).toBe('')
    expect(render([])).toBe('')
  })
  it('a row per book: title linking to the doc, workspace · chapters · size, Download and Stop sending', () => {
    const html = render([sent({}), sent({ id: 's2', title: 'Notes', workspace: null, chapters: 1, progress: { percentage: 42.4, device: 'Kobo', updated_at: '2026-10-01T09:00:00Z', status: 'reading' } })])
    expect(html).toContain('From Taisce')
    expect(html).toContain('href="/doc/s1"')
    expect(html).toContain('Work · 12 chapters · ')
    expect(html).toContain('Unsorted · 1 chapter · ')
    expect(html).toContain('42%')
    expect(html).toContain('Kobo')
    expect(html.match(/Stop sending/g)).toHaveLength(2)
    expect(html.match(/Download/g)).toHaveLength(2)
  })
  it('sentBookLine', () => {
    expect(sentBookLine({ workspace: 'Work', chapters: 3, size: 2048 })).toMatch(/^Work · 3 chapters · /)
  })
})

describe('EreaderChip', () => {
  const render = (s: EreaderStatus | null) => renderToString(createElement(EreaderChip, { docId: 'd1', title: 'Doc', status: s, onOpenDoc: () => {} }))
  it('renders only for a sent doc or a chapter of a sent book', () => {
    expect(render(null)).toBe('')
    expect(render(status({ tagged: true }))).toBe('')
    expect(render(status({ sent: true }))).toContain('On e-readers')
    expect(render(status({ in_sent_book: { id: 'f', title: 'Handbook' } }))).toContain('On e-readers · in Handbook')
  })
})
