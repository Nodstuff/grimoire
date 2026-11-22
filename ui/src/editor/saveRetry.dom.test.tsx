// @vitest-environment jsdom
// A failed save: a retry carries the same request_id (the daemon dedupes on
// it, so a landed-but-unanswered save is never applied twice), and a refusal
// a retry cannot fix stops the 5 s retry clock until the next edit.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { advance, block, mountEditor, scriptFetch, typeAtEnd, type Call } from './editorHarness'

const doc = { docId: 'd', epoch: 1, blocks: [block('p', 'hello')] }

beforeEach(() => {
  vi.useFakeTimers()
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

const proposes = (calls: Call[]) => calls.filter((c) => c.path === '/api/propose')

describe('a failed save', () => {
  it('retries with the same request_id, then lands', async () => {
    const calls = scriptFetch((c, n) => (n <= 2 ? new TypeError('Failed to fetch') : { epoch: 2 }))
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' world')
    await advance(1300) // the debounce
    await advance(5100) // one retry
    await advance(5100) // another
    const ps = proposes(calls)
    expect(ps).toHaveLength(3)
    const ids = ps.map((c) => c.body!.request_id)
    expect(typeof ids[0]).toBe('string')
    expect(new Set(ids).size).toBe(1)
    expect(m.states.at(-1)).toBe('clean')
    // nothing more once it landed
    await advance(15000)
    expect(proposes(calls)).toHaveLength(3)
    await m.unmount()
  })

  it('a new edit is a new request with a new request_id', async () => {
    const calls = scriptFetch(() => ({ epoch: 2 }))
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' a')
    await advance(1300)
    await typeAtEnd(m, 'b')
    await advance(1300)
    const ids = proposes(calls).map((c) => c.body!.request_id)
    expect(ids).toHaveLength(2)
    expect(ids[0]).not.toBe(ids[1])
    await m.unmount()
  })

  it('a refusal a retry cannot fix stops the retry clock; the next edit tries again', async () => {
    const calls = scriptFetch(() => ({ error: 'forbidden: viewer', code: 'forbidden' }))
    const m = await mountEditor(doc)
    await typeAtEnd(m, ' world')
    await advance(1300)
    expect(proposes(calls)).toHaveLength(1)
    expect(m.states.at(-1)).toBe('failed')
    await advance(20000)
    expect(proposes(calls)).toHaveLength(1)
    await typeAtEnd(m, '!')
    await advance(1300)
    expect(proposes(calls)).toHaveLength(2)
    await m.unmount()
  })
})
