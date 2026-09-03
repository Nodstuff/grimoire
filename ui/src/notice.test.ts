import { describe, expect, it, vi } from 'vitest'
import { MAX_VISIBLE, currentNotices, dismiss, notify } from './Notice'

describe('notify cap', () => {
  it('keeps at most MAX_VISIBLE, dropping the oldest auto-dismissing one first', () => {
    vi.useFakeTimers()
    for (const n of currentNotices()) dismiss(n.id)
    const sticky = notify('boom') // error: sticky
    for (let i = 0; i < MAX_VISIBLE + 2; i++) notify(`ok ${i}`, 'ok')
    const list = currentNotices()
    expect(list.length).toBe(MAX_VISIBLE)
    expect(list.some((n) => n.id === sticky)).toBe(true)
    expect(list.map((n) => n.message)).toEqual(['boom', 'ok 3', 'ok 4', 'ok 5', 'ok 6'])
    vi.runAllTimers()
    expect(currentNotices().map((n) => n.message)).toEqual(['boom'])
    dismiss(sticky)
    vi.useRealTimers()
  })
})
