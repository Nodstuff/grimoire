// @vitest-environment jsdom
// The Comment / Link bubble is fixed to the viewport: it was placed once, so
// it stayed put while the text under it scrolled away. It follows now.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act } from 'react'
import { advance } from '../editor/editorHarness'
import { fakeServer, makeTree, mountDocView } from './docViewHarness'

let rect = { left: 100, top: 300, width: 40 }
beforeEach(() => {
  vi.useFakeTimers()
  rect = { left: 100, top: 300, width: 40 }
  vi.spyOn(window, 'getSelection').mockImplementation(
    () =>
      ({
        rangeCount: 1,
        isCollapsed: false,
        getRangeAt: () => ({ getBoundingClientRect: () => ({ ...rect, height: 18, right: rect.left + rect.width, bottom: rect.top + 18 }) }),
      }) as unknown as Selection,
  )
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

const bubble = (el: HTMLElement) => el.querySelector<HTMLElement>('.dv-sel')

describe('the selection bubble', () => {
  it('follows the selection when the page or a pane scrolls', async () => {
    fakeServer(makeTree(5, [['a', 'alpha beta gamma']]))
    const v = await mountDocView()
    await advance(10)
    await act(async () => {
      v.editor()!.commands.setTextSelection({ from: 2, to: 6 })
    })
    await advance(20)
    expect(bubble(v.el)!.style.top).toBe('300px')
    expect(bubble(v.el)!.style.left).toBe('120px')
    // the doc scrolls by 120 px inside its pane
    rect = { ...rect, top: 180 }
    await act(async () => {
      v.el.dispatchEvent(new Event('scroll'))
    })
    await advance(20)
    expect(bubble(v.el)!.style.top).toBe('180px')
    // and the window
    rect = { ...rect, top: 60, left: 50 }
    await act(async () => {
      window.dispatchEvent(new Event('scroll'))
    })
    await advance(20)
    expect(bubble(v.el)!.style.top).toBe('60px')
    expect(bubble(v.el)!.style.left).toBe('70px')
    await v.unmount()
  })
})
