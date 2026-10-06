import { describe, expect, it } from 'vitest'
import { base64Utf8, buildSnapshot, isLive, type Share } from './share'

const SVG = '<svg xmlns="http://www.w3.org/2000/svg" width="640" height="320"><text>é</text></svg>'

describe('buildSnapshot', () => {
  it('turns each rendered diagram fence into a taisce-asset image, in order', () => {
    const md = '# T\n\n```mermaid\ngraph TD; A-->B\n```\n\ntext\n\n```reladraw\nbox a\n```\n\n```rust\nfn x() {}\n```\n'
    const s = buildSnapshot('T', md, [SVG, null])
    expect(s.markdown).toBe('# T\n\n![mermaid diagram](taisce-asset:d1.svg)\n\ntext\n\n```reladraw\nbox a\n```\n\n```rust\nfn x() {}\n```\n')
    expect(s.assets).toHaveLength(1)
    expect(s.assets[0]).toMatchObject({ name: 'd1.svg', content_type: 'image/svg+xml', width: 640, height: 320 })
    expect(new TextDecoder().decode(Uint8Array.from(atob(s.assets[0].data), (c) => c.charCodeAt(0)))).toBe(SVG)
  })

  it('takes the size from a viewBox, and leaves longer fences intact', () => {
    const svg = '<svg viewBox="0 0 200 100"></svg>'
    const s = buildSnapshot('T', '````d2\na -> b\n```\nstill code\n````\nafter', [svg])
    expect(s.markdown).toBe('![d2 diagram](taisce-asset:d1.svg)\nafter')
    expect(s.assets[0]).toMatchObject({ width: 200, height: 100 })
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
