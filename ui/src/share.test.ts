import { describe, expect, it } from 'vitest'
import { base64Utf8, buildSnapshot, buildTreeSnapshot, isLive, subPages, visualFences, type Share } from './share'
import type { Doc } from './types'

const SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320"><text>é</text></svg>'

describe('buildSnapshot', () => {
  it('turns each drawn diagram fence into a taisce-asset image', () => {
    const md = '# T\n\n```mermaid\ngraph TD; A-->B\n```\n\ntext\n\n```reladraw\nbox a\n```\n\n```rust\nfn x() {}\n```\n'
    const s = buildSnapshot('T', md, [{ lang: 'mermaid', code: 'graph TD; A-->B\n', svg: SVG }])
    expect(s.markdown).toBe('# T\n\n![mermaid diagram](taisce-asset:d1.svg)\n\ntext\n\n```reladraw\nbox a\n```\n\n```rust\nfn x() {}\n```\n')
    expect(s.assets).toHaveLength(1)
    expect(s.assets[0]).toMatchObject({ name: 'd1.svg', content_type: 'image/svg+xml', width: 640, height: 320 })
    expect(new TextDecoder().decode(Uint8Array.from(atob(s.assets[0].data), (c) => c.charCodeAt(0)))).toBe(SVG)
  })

  it('takes the size from a viewBox, and leaves longer fences intact', () => {
    const svg = '<svg viewBox="0 0 200 100"></svg>'
    const s = buildSnapshot('T', '````d2\na -> b\n```\nstill code\n````\nafter', [{ lang: 'd2', code: 'a -> b\n```\nstill code', svg }])
    expect(s.markdown).toBe('![d2 diagram](taisce-asset:d1.svg)\nafter')
    expect(s.assets[0]).toMatchObject({ width: 200, height: 100 })
  })

  it('never shifts a drawing onto another fence', () => {
    // the first fence sits in a list (indented: not seen as a fence here),
    // the editor drew both: each image must land on its own code
    const md = '- item\n\n      ```mermaid\n      graph A\n      ```\n\n```mermaid\ngraph B\n```\n\n```reladraw\nnot drawn\n```'
    const s = buildSnapshot('T', md, [
      { lang: 'mermaid', code: 'graph A', svg: '<svg id="A"/>' },
      { lang: 'mermaid', code: 'graph B', svg: '<svg id="B"/>' },
    ])
    expect(s.assets).toHaveLength(1)
    expect(atob(s.assets[0].data)).toBe('<svg id="B"/>')
    expect(s.markdown).toContain('![mermaid diagram](taisce-asset:d1.svg)')
    expect(s.markdown).toContain('```reladraw\nnot drawn\n```')
    expect(s.markdown).toContain('      graph A')
  })

  it('pairs two identical diagrams one each', () => {
    const md = '```mermaid\ng\n```\n\n```mermaid\ng\n```'
    const s = buildSnapshot('T', md, [{ lang: 'mermaid', code: 'g', svg: '<svg/>' }])
    expect(s.assets).toHaveLength(1)
    expect(s.markdown).toBe('![mermaid diagram](taisce-asset:d1.svg)\n\n```mermaid\ng\n```')
  })

  it('base64-encodes UTF-8', () => {
    expect(base64Utf8('é')).toBe('w6k=')
  })
})

describe('isLive', () => {
  const base: Share = {
    id: 'i', doc_id: 'd', url: 'u', created_at: '', expires_at: null, revoked_at: null, comments_enabled: true,
    revision: 1, views: 0, comment_count: 0, unread_comments: 0, title: 't',
  }
  it('is live until revoked or expired', () => {
    expect(isLive(base)).toBe(true)
    expect(isLive({ ...base, revoked_at: '2026-01-01T00:00:00Z' })).toBe(false)
    expect(isLive({ ...base, expires_at: '2026-01-01T00:00:00Z' }, Date.parse('2026-02-01T00:00:00Z'))).toBe(false)
    expect(isLive({ ...base, expires_at: '2026-03-01T00:00:00Z' }, Date.parse('2026-02-01T00:00:00Z'))).toBe(true)
  })
})

describe('a link with sub-pages', () => {
  const doc = (id: string, parent: string | null, sort: string | null, title = id): Doc => ({
    id,
    parent_id: parent,
    title,
    review_policy: null,
    current_epoch: 1,
    created_by: 'p',
    status: null,
    sort_key: sort,
  })
  const docs = [doc('root', null, 'a'), doc('b', 'root', 'b'), doc('a', 'root', 'a'), doc('a1', 'a', 'a'), doc('other', null, 'b'), doc('o1', 'other', 'a')]

  it('lists the docs under one in the sidebar order, parents first', () => {
    expect(subPages(docs, 'root').map((d) => d.id)).toEqual(['a', 'a1', 'b'])
    expect(subPages(docs, 'a1')).toEqual([])
    // a parent loop cannot hang it
    expect(subPages([doc('x', 'y', 'a'), doc('y', 'x', 'a')], 'x').map((d) => d.id)).toEqual(['y'])
  })

  it('finds the fences to draw, by the same rules as the snapshot', () => {
    const md = '```mermaid\ngraph A\n```\n\n````d2\na -> b\n```\nstill\n````\n\n```rust\nfn x() {}\n```\n\n~~~reladraw\nbox a\n~~~'
    expect(visualFences(md)).toEqual([
      { lang: 'mermaid', code: 'graph A' },
      { lang: 'd2', code: 'a -> b\n```\nstill' },
      { lang: 'reladraw', code: 'box a' },
    ])
  })

  it('draws each page, names its images apart, and sends the tree', async () => {
    const root = buildSnapshot('Root', '```mermaid\nr\n```', [{ lang: 'mermaid', code: 'r', svg: '<svg id="r"/>' }])
    const pages = subPages(docs, 'root').map((d) => ({ doc: d, markdown: d.id === 'b' ? '```mermaid\nbad\n```' : `# ${d.id}\n\n\`\`\`mermaid\n${d.id}\n\`\`\`` }))
    const drawn: string[] = []
    const { snapshot, problems } = await buildTreeSnapshot(root, 'root', pages, async (lang, code) => {
      drawn.push(`${lang}:${code}`)
      if (code === 'bad') throw new Error('Parse error on line 1\nmore')
      return `<svg id="${code}"/>`
    })
    expect(drawn).toEqual(['mermaid:a', 'mermaid:a1', 'mermaid:bad'])
    expect(snapshot.title).toBe('Root')
    expect(snapshot.markdown).toBe('![mermaid diagram](taisce-asset:d1.svg)')
    expect(snapshot.assets.map((a) => a.name)).toEqual(['d1.svg', 'p1-d1.svg', 'p2-d1.svg'])
    expect(snapshot.pages).toEqual([
      { doc_id: 'a', parent_doc_id: null, title: 'a', markdown: '# a\n\n![mermaid diagram](taisce-asset:p1-d1.svg)' },
      { doc_id: 'a1', parent_doc_id: 'a', title: 'a1', markdown: '# a1\n\n![mermaid diagram](taisce-asset:p2-d1.svg)' },
      { doc_id: 'b', parent_doc_id: null, title: 'b', markdown: '```mermaid\nbad\n```' },
    ])
    expect(problems).toEqual(['b: a mermaid diagram stayed as code (Parse error on line 1)'])
  })
})
