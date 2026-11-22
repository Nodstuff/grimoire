// @vitest-environment jsdom
// A save that landed but whose answer never came (a network failure after
// the daemon applied it, or the keepalive sent as the page went into the
// bfcache): a later, different save went out on the stale base, and its
// inserts (already applied) came back "already exists" and parked red.
// The earlier request is now asked again under its own request_id first,
// and its answer adopted.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { advance, block, mountEditor } from './editorHarness'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

interface Sent {
  base: number
  id: string
  keepalive: boolean
  ops: { kind: { op: string; block_id?: string; content?: string; target?: string } }[]
}

/** a daemon that applies a propose, remembers its answer by request_id, and
 * loses the answer of the first `lose` requests on the way back */
function daemon(lose: number) {
  const st = { epoch: 1, sent: [] as Sent[], answers: new Map<string, unknown>(), lose }
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init: RequestInit = {}) => {
      if (path !== '/api/propose') return new Response('[]', { status: 200 })
      const b = JSON.parse(String(init.body))
      st.sent.push({ base: b.base_epoch, id: b.request_id, keepalive: !!init.keepalive, ops: b.ops })
      let answer = st.answers.get(b.request_id)
      if (!answer) {
        st.epoch += 1
        answer = { doc_id: 'd', epoch: st.epoch, verdicts: [] }
        st.answers.set(b.request_id, answer)
      }
      if (st.lose > 0) {
        st.lose--
        throw new TypeError('Failed to fetch')
      }
      return new Response(JSON.stringify(answer), { status: 200, headers: { 'Content-Type': 'application/json' } })
    }),
  )
  return st
}

const doc = { docId: 'd', epoch: 1, blocks: [block('p', 'hello')] }

describe('a save whose answer was lost', () => {
  it('then edited again: the lost request goes again under its id, its answer is adopted, and the new edit is based on it', async () => {
    const st = daemon(1)
    const m = await mountEditor(doc)
    await act(async () => {
      m.editor().commands.insertContentAt(m.editor().state.doc.content.size, { type: 'paragraph', content: [{ type: 'text', text: 'new para' }] })
    })
    await advance(1300) // lands, answer lost
    expect(st.sent).toHaveLength(1)
    await act(async () => {
      m.editor().commands.insertContentAt(1 + 'hello'.length, '!')
    })
    await advance(1300)
    expect(st.sent.map((s) => [s.base, s.id])).toEqual([
      [1, st.sent[0].id],
      [1, st.sent[0].id],
      [2, st.sent[2].id],
    ])
    expect(st.sent[2].id).not.toBe(st.sent[0].id)
    // only the new typing; the paragraph is not inserted again
    expect(st.sent[2].ops.map((o) => o.kind)).toEqual([{ op: 'replace', target: 'p', content: 'hello!' }])
    expect(m.states.at(-1)).toBe('clean')
    await m.unmount()
  })

  it('back from the bfcache: the keepalive is asked again and adopted before a new edit is saved', async () => {
    const st = daemon(1) // the keepalive's answer is never read
    const m = await mountEditor(doc)
    await act(async () => {
      m.editor().commands.insertContentAt(m.editor().state.doc.content.size, { type: 'paragraph', content: [{ type: 'text', text: 'typed before leaving' }] })
    })
    window.dispatchEvent(new Event('pagehide'))
    expect(st.sent[0].keepalive).toBe(true)
    // the page comes back
    await act(async () => {
      const e = new Event('pageshow') as Event & { persisted: boolean }
      Object.defineProperty(e, 'persisted', { value: true })
      window.dispatchEvent(e)
    })
    await advance(10)
    expect(st.sent[1].id).toBe(st.sent[0].id)
    await act(async () => {
      m.editor().commands.insertContentAt(1 + 'hello'.length, '?')
    })
    await advance(1300)
    const last = st.sent.at(-1)!
    expect(last.base).toBe(2)
    expect(last.ops.map((o) => o.kind)).toEqual([{ op: 'replace', target: 'p', content: 'hello?' }])
    expect(st.epoch).toBe(3)
    await m.unmount()
  })
})
