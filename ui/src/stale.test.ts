import { describe, expect, it } from 'vitest'
import { livingLabel, livingTone, verifiedLabel, LivingStatus } from './stale'

const NOW = Date.parse('2026-09-10T12:00:00Z')
const ago = (ms: number) => new Date(NOW - ms).toISOString()
const DAY = 86_400_000

describe('verifiedLabel', () => {
  it('reads never verified for a null stamp', () => {
    expect(verifiedLabel(null, NOW)).toBe('never verified')
    expect(verifiedLabel(undefined, NOW)).toBe('never verified')
  })
  it('reads verified <relative> otherwise', () => {
    expect(verifiedLabel(ago(3 * DAY), NOW)).toBe('verified 3d ago')
    expect(verifiedLabel(ago(2_000), NOW)).toBe('verified just now')
  })
})

describe('livingLabel', () => {
  const base: LivingStatus = { is_answer: true, question: 'q', sources: [], changed: 0, last_refreshed: ago(2 * 3_600_000) }
  it('is null for docs that are not answers', () => {
    expect(livingLabel(null, NOW)).toBeNull()
    expect(livingLabel({ ...base, is_answer: false }, NOW)).toBeNull()
    expect(livingTone({ ...base, is_answer: false })).toBeNull()
  })
  it('says verified with the last refresh time while nothing changed', () => {
    expect(livingLabel(base, NOW)).toBe('answer · verified 2h ago')
    expect(livingTone(base)).toBe('fresh')
  })
  it('counts changed sources and pluralises', () => {
    expect(livingLabel({ ...base, changed: 1 }, NOW)).toBe('answer · 1 source changed — refreshing')
    expect(livingLabel({ ...base, changed: 2 }, NOW)).toBe('answer · 2 sources changed — refreshing')
    expect(livingTone({ ...base, changed: 2 })).toBe('stale')
  })
})
