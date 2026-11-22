// @vitest-environment jsdom
// Keep / Revert while you have unsaved typing: the typing lands first (on the
// doc as it was), and the editor is never remounted over unsaved keystrokes
// (that hid them, and the unmount flush then parked them red on a stale base).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { advance } from '../editor/editorHarness'
import { fakeServer, makeTree, mountDocView, yellowReplace } from './docViewHarness'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

const button = (el: HTMLElement, label: string) =>
  [...el.querySelectorAll<HTMLButtonElement>('.rv-card-actions button')].find((b) => b.textContent === label)!

describe('Revert while typing', () => {
  it('saves the typing first, then shows the revert with the typing kept', async () => {
    const server = fakeServer(makeTree(5, [['a', 'agent text'], ['b', 'beta']]))
    server.review = [yellowReplace('ann1', 'a', 'mine', 'agent text')]
    let epoch = 5
    server.propose = (body) => {
      epoch = (body.base_epoch as number) + 1
      server.tree = makeTree(epoch, [['a', 'agent text'], ['b', 'beta typed']])
      return { doc_id: 'd', epoch, verdicts: [] }
    }
    server.resolve = () => {
      epoch += 1
      server.tree = makeTree(epoch, [['a', 'mine'], ['b', 'beta typed']])
      server.review = []
      return { ok: true }
    }
    const v = await mountDocView(0, 'review')
    await advance(10)
    await act(async () => {
      v.editor()!.commands.insertContentAt(v.editor()!.state.doc.content.size - 1, ' typed')
    })
    // no debounce wait: Revert right away
    await act(async () => {
      button(v.el, 'Revert').click()
    })
    await advance(50)
    const order = server.calls.filter((c) => c.method === 'POST').map((c) => c.path)
    expect(order).toEqual(['/api/propose', '/api/resolve'])
    const save = server.calls.find((c) => c.path === '/api/propose')!
    expect(save.body!.base_epoch).toBe(5)
    expect(v.text()).toContain('mine')
    expect(v.text()).toContain('beta typed')
    // nothing is left to save, and nothing was re-sent on a stale base
    await advance(6000)
    expect(server.calls.filter((c) => c.path === '/api/propose')).toHaveLength(1)
    await v.unmount()
  })

  it('a failed flush: the editor is not remounted over the typing', async () => {
    const server = fakeServer(makeTree(5, [['a', 'agent text'], ['b', 'beta']]))
    server.review = [yellowReplace('ann1', 'a', 'mine', 'agent text')]
    server.propose = () => ({ error: 'database is locked', code: 'internal' })
    server.resolve = () => {
      server.tree = makeTree(6, [['a', 'mine'], ['b', 'beta']])
      server.review = []
      return { ok: true }
    }
    const v = await mountDocView(0, 'review')
    await advance(10)
    await act(async () => {
      v.editor()!.commands.insertContentAt(v.editor()!.state.doc.content.size - 1, ' typed')
    })
    await act(async () => {
      button(v.el, 'Revert').click()
    })
    await advance(50)
    expect(v.text()).toContain('beta typed')
    // once the save lands, the revert shows too
    server.propose = (body) => {
      server.tree = makeTree(7, [['a', 'mine'], ['b', 'beta typed']])
      return { doc_id: 'd', epoch: (body.base_epoch as number) + 1, verdicts: [] }
    }
    await advance(5100)
    await advance(50)
    expect(v.text()).toContain('mine')
    expect(v.text()).toContain('beta typed')
    await v.unmount()
  })
})
