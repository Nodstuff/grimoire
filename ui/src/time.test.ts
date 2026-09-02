import { describe, expect, it } from 'vitest'
import { relTime } from './time'

const NOW = Date.parse('2026-09-02T12:00:00Z')
const ago = (ms: number) => new Date(NOW - ms).toISOString()

describe('relTime', () => {
  it('rounds down to the coarsest unit that fits', () => {
    expect(relTime(ago(2_000), NOW)).toBe('just now')
    expect(relTime(ago(12_000), NOW)).toBe('12s ago')
    expect(relTime(ago(59_000), NOW)).toBe('59s ago')
    expect(relTime(ago(60_000), NOW)).toBe('1m ago')
    expect(relTime(ago(45 * 60_000), NOW)).toBe('45m ago')
    expect(relTime(ago(3 * 3_600_000), NOW)).toBe('3h ago')
    expect(relTime(ago(2 * 86_400_000), NOW)).toBe('2d ago')
    expect(relTime(ago(45 * 86_400_000), NOW)).toBe('1mo ago')
    expect(relTime(ago(400 * 86_400_000), NOW)).toBe('1y ago')
  })
  it('clamps future stamps (clock skew) to just now', () => {
    expect(relTime(ago(-30_000), NOW)).toBe('just now')
  })
  it('passes garbage through untouched', () => {
    expect(relTime('not a date', NOW)).toBe('not a date')
  })
})
