import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { debounce, throttle } from './timing'

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

describe('debounce', () => {
  it('fires once, ms after the LAST arm — not the first', () => {
    const fn = vi.fn()
    const d = debounce(1200, fn)
    d.arm()
    vi.advanceTimersByTime(1000)
    d.arm() // keystroke at t=1000 pushes the save out
    vi.advanceTimersByTime(1000) // t=2000: 1200 after first arm has passed
    expect(fn).not.toHaveBeenCalled()
    vi.advanceTimersByTime(200) // t=2200: 1200 after the last arm
    expect(fn).toHaveBeenCalledTimes(1)
    expect(d.pending()).toBe(false)
  })

  it('flush runs a pending call immediately and only once; cancel drops it', () => {
    const fn = vi.fn()
    const d = debounce(500, fn)
    d.flush() // nothing pending → no call
    expect(fn).not.toHaveBeenCalled()
    d.arm()
    d.flush()
    expect(fn).toHaveBeenCalledTimes(1)
    vi.advanceTimersByTime(1000)
    expect(fn).toHaveBeenCalledTimes(1)
    d.arm()
    d.cancel()
    vi.advanceTimersByTime(1000)
    expect(fn).toHaveBeenCalledTimes(1)
  })
})

describe('throttle', () => {
  it('leading call at once, a burst collapses to one trailing call', () => {
    const fn = vi.fn()
    const t = throttle(60, fn)
    for (let i = 0; i < 30; i++) {
      t.call()
      vi.advanceTimersByTime(1)
    }
    expect(fn).toHaveBeenCalledTimes(1)
    vi.advanceTimersByTime(60)
    expect(fn).toHaveBeenCalledTimes(2)
    vi.advanceTimersByTime(500)
    expect(fn).toHaveBeenCalledTimes(2)
  })

  it('a steady drag yields about one call per window', () => {
    const fn = vi.fn()
    const t = throttle(60, fn)
    for (let i = 0; i < 600; i++) {
      t.call()
      vi.advanceTimersByTime(1) // 600ms at 1000fps
    }
    vi.advanceTimersByTime(60)
    expect(fn.mock.calls.length).toBeGreaterThanOrEqual(10)
    expect(fn.mock.calls.length).toBeLessThanOrEqual(12)
  })

  it('flush lands the queued trailing call now; cancel drops it', () => {
    const fn = vi.fn()
    const t = throttle(60, fn)
    t.call()
    t.call()
    t.flush()
    expect(fn).toHaveBeenCalledTimes(2)
    t.call() // window is over after flush → leading call again
    t.call() // queued trailing
    expect(fn).toHaveBeenCalledTimes(3)
    t.cancel()
    vi.advanceTimersByTime(200)
    expect(fn).toHaveBeenCalledTimes(3)
  })
})
