// @vitest-environment jsdom
// The byline's "version N" follows your own saves, not only tree refetches.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { advance } from '../editor/editorHarness'
import { fakeServer, makeTree, mountDocView } from './docViewHarness'

beforeEach(() => vi.useFakeTimers())
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

const version = (el: HTMLElement) => el.querySelector('.dv-byline')!.textContent!.match(/version\s*v?(\d+)/)![1]

describe('the byline version', () => {
  it('moves with each save of yours', async () => {
    fakeServer(makeTree(5, [['a', 'alpha']]))
    const v = await mountDocView()
    await advance(10)
    expect(version(v.el)).toBe('5')
    await act(async () => {
      v.editor()!.commands.insertContentAt(1 + 'alpha'.length, '!')
    })
    await advance(1300)
    expect(version(v.el)).toBe('6')
    await act(async () => {
      v.editor()!.commands.insertContentAt(1 + 'alpha!'.length, '?')
    })
    await advance(1300)
    expect(version(v.el)).toBe('7')
    await v.unmount()
  })
})
