// @vitest-environment jsdom
// Review-queue keys: A twice in quick succession resolved the same item
// twice (`busy` cleared before the reload answered, and the key handler read
// it from the last render), and a late, older load brought a resolved row back.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import ReviewQueue from './ReviewQueue'
import { yellowReplace } from './docViewHarness'
import type { QueueRow } from '../types'

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  document.body.innerHTML = ''
})

interface Server {
  queue: QueueRow[]
  calls: string[]
  /** hold /api/queue answers while true */
  holdQueue: boolean
  held: (() => void)[]
}

function server(queue: QueueRow[]): Server {
  const s: Server = { queue, calls: [], holdQueue: false, held: [] }
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: string, init: RequestInit = {}) => {
      s.calls.push(`${init.method ?? 'GET'} ${path}`)
      let r: unknown = []
      if (path === '/api/queue') {
        const snapshot = [...s.queue]
        if (s.holdQueue) await new Promise<void>((res) => s.held.push(res))
        r = snapshot
      } else if (path === '/api/resolve') {
        const id = JSON.parse(String(init.body)).annotation_id
        s.queue = s.queue.filter((q) => q.item.annotation.id !== id)
        r = { ok: true }
      }
      return new Response(JSON.stringify(r), { status: 200, headers: { 'Content-Type': 'application/json' } })
    }),
  )
  return s
}

async function mount(dataVersion = 0) {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const root = createRoot(el)
  const render = (dv: number) =>
    act(async () => root.render(<ReviewQueue onChange={() => {}} onOpenDoc={() => {}} dataVersion={dv} />))
  await render(dataVersion)
  await act(async () => {
    await vi.advanceTimersByTimeAsync(10)
  })
  return { el, render }
}
const press = (key: string) => window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }))
const resolves = (s: Server) => s.calls.filter((c) => c === 'POST /api/resolve').length

describe('review queue keys', () => {
  it('A pressed twice resolves the selected item once, then the next one', async () => {
    const s = server([yellowReplace('ann1', 'a', 'x', 'y'), yellowReplace('ann2', 'b', 'x', 'y')])
    await mount()
    await act(async () => {
      press('a')
      press('a')
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10)
    })
    expect(resolves(s)).toBe(1)
    expect(s.queue.map((q) => q.item.annotation.id)).toEqual(['ann2'])
    await act(async () => press('a'))
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10)
    })
    expect(resolves(s)).toBe(2)
    expect(s.queue).toEqual([])
  })

  it('a resolved row leaves at once and an older load answering late cannot bring it back', async () => {
    const s = server([yellowReplace('ann1', 'a', 'x', 'y'), yellowReplace('ann2', 'b', 'x', 'y')])
    const v = await mount()
    // a stamp tick's load starts and is slow
    s.holdQueue = true
    await v.render(1)
    s.holdQueue = false
    await act(async () => press('a'))
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10)
    })
    // the old load (taken before the resolve) answers last
    await act(async () => {
      s.held.forEach((r) => r())
      await vi.advanceTimersByTimeAsync(10)
    })
    expect(v.el.querySelectorAll('[data-sel="ann1"]').length).toBe(0)
    expect(v.el.querySelectorAll('[data-sel="ann2"]').length).toBe(1)
  })
})
