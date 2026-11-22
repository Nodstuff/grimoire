// @vitest-environment jsdom
// A share link's snapshot is the server's markdown: typing within the 1.2 s
// autosave debounce was left out of it. The editor is flushed first.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import SharePanel from './SharePanel'
import { advance, block, mountEditor, scriptFetch, typeAtEnd } from './editor/editorHarness'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

async function mountPanel() {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const root = createRoot(el)
  await act(async () => root.render(<SharePanel docId="d" title="T" />))
  return { el, unmount: () => act(async () => root.unmount()) }
}
const makeLink = (el: HTMLElement) => [...el.querySelectorAll('button')].find((b) => b.textContent?.includes('Make link'))!

describe('making a share link', () => {
  it('lands unsaved typing before taking the snapshot', async () => {
    let serverText = 'hello'
    const calls = scriptFetch((c) => {
      if (c.path === '/api/propose') {
        serverText = 'hello world'
        return { epoch: 2 }
      }
      if (c.path.startsWith('/api/shares?')) return { shares: [] }
      if (c.path === '/api/docs') return []
      if (c.path === '/api/doc/d/markdown') return { markdown: serverText }
      if (c.path === '/api/shares') return { id: 's1', url: 'https://x/s/1', pages: 0 }
      return {}
    })
    const ed = await mountEditor({ docId: 'd', epoch: 1, blocks: [block('p', 'hello')] })
    const panel = await mountPanel()
    await typeAtEnd(ed, ' world')
    await act(async () => makeLink(panel.el).click())
    await advance(20)
    const paths = calls.map((c) => c.path)
    expect(paths.indexOf('/api/propose')).toBeGreaterThan(-1)
    expect(paths.indexOf('/api/propose')).toBeLessThan(paths.indexOf('/api/doc/d/markdown'))
    const share = calls.find((c) => c.path === '/api/shares')!
    expect(JSON.stringify(share.body)).toContain('hello world')
    await panel.unmount()
    await ed.unmount()
  })

  it('makes no link when the typing cannot be saved', async () => {
    const calls = scriptFetch((c) => {
      if (c.path === '/api/propose') return new TypeError('Failed to fetch')
      if (c.path.startsWith('/api/shares?')) return { shares: [] }
      if (c.path === '/api/docs') return []
      return {}
    })
    const ed = await mountEditor({ docId: 'd', epoch: 1, blocks: [block('p', 'hello')] })
    const panel = await mountPanel()
    await typeAtEnd(ed, ' world')
    await act(async () => makeLink(panel.el).click())
    await advance(20)
    expect(calls.some((c) => c.path === '/api/shares' && c.init.method === 'POST')).toBe(false)
    await panel.unmount()
    await ed.unmount()
  })
})
