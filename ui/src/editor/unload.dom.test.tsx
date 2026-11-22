// @vitest-environment jsdom
// Reloading or closing the tab within the 1.2 s autosave debounce lost the
// last typing: the reload shortcut reloaded at once, and nothing handled the
// page going away.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { advance, block, mountEditor, scriptFetch, typeAtEnd, type Call } from './editorHarness'
import { reloadAfterSaving } from '../App'

const doc = { docId: 'd', epoch: 1, blocks: [block('p', 'hello')] }
const proposes = (calls: Call[]) => calls.filter((c) => c.path === '/api/propose')

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

function beforeUnload(): Event {
  const e = new Event('beforeunload', { cancelable: true })
  window.dispatchEvent(e)
  return e
}

describe('leaving the page', () => {
  it('asks before unloading over unsaved typing, not when clean', async () => {
    scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    expect(beforeUnload().defaultPrevented).toBe(false)
    await typeAtEnd(m, ' world')
    expect(beforeUnload().defaultPrevented).toBe(true)
    await advance(1300)
    expect(beforeUnload().defaultPrevented).toBe(false)
    await m.unmount()
  })

  it('as the page goes, sends the unsaved typing with a keepalive request', async () => {
    const calls = scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' world')
    window.dispatchEvent(new Event('pagehide'))
    const ps = proposes(calls)
    expect(ps).toHaveLength(1)
    expect(ps[0].init.keepalive).toBe(true)
    expect(ps[0].body!.ops).toEqual([{ kind: { op: 'replace', target: 'p', content: 'hello world' }, source_refs: [] }])
    expect(typeof ps[0].body!.request_id).toBe('string')
    // the page stayed after all (bfcache): the regular save of the same ops
    // carries the same request_id, so the daemon never applies them twice
    await advance(1300)
    const again = proposes(calls)
    expect(again).toHaveLength(2)
    expect(again[1].body!.request_id).toBe(ps[0].body!.request_id)
    await m.unmount()
  })

  it('a save already in flight is left to land, not sent twice', async () => {
    let answer: (v: unknown) => void = () => {}
    const calls = scriptFetch(() => ({ epoch: 2 }))
    // hold the first propose's answer
    const real = globalThis.fetch
    let held = false
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: string, init: RequestInit) => {
        if (path === '/api/propose' && !held) {
          held = true
          await new Promise((r) => (answer = r))
        }
        return real(path, init)
      }),
    )
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' world')
    await advance(1300) // the save goes out and waits
    window.dispatchEvent(new Event('pagehide'))
    expect(held).toBe(true)
    answer(null)
    await advance(10)
    expect(proposes(calls)).toHaveLength(1)
    expect(proposes(calls)[0].init.keepalive).toBeUndefined()
    await m.unmount()
  })
})

describe('the reload shortcut', () => {
  it('saves the unsaved typing first, then reloads', async () => {
    const calls = scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' world')
    const reload = vi.fn()
    let done: Promise<boolean> = Promise.resolve(false)
    done = reloadAfterSaving(reload)
    await advance(10)
    expect(await done).toBe(true)
    expect(proposes(calls)).toHaveLength(1)
    expect(reload).toHaveBeenCalledTimes(1)
    await m.unmount()
  })

  it('stays when the typing cannot be saved', async () => {
    scriptFetch(() => new TypeError('Failed to fetch'))
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' world')
    const reload = vi.fn()
    const done = reloadAfterSaving(reload)
    await advance(10)
    expect(await done).toBe(false)
    expect(reload).not.toHaveBeenCalled()
    await m.unmount()
  })
})
