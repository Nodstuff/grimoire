import { describe, expect, it } from 'vitest'
import { parseDeepLink, scrubDeepLink } from './deeplink'

const D = '0b3a5a0e-2c8e-4c33-9e5e-1f2a3b4c5d6e'
const B = '7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f'

describe('parseDeepLink', () => {
  it('reads doc, block and tab', () => {
    expect(parseDeepLink(`?doc=${D}&block=${B}`)).toEqual({ doc: D, block: B })
    expect(parseDeepLink(`?tab=review`)).toEqual({ tab: 'review' })
    expect(parseDeepLink(`?admin_token=x&doc=${D}&tab=graph`)).toEqual({ doc: D, tab: 'graph' })
  })
  it('drops non-uuid ids, block without doc, unknown tabs', () => {
    expect(parseDeepLink('?doc=not-a-uuid')).toBeNull()
    expect(parseDeepLink(`?block=${B}`)).toBeNull()
    expect(parseDeepLink(`?doc=${D}&block=nope`)).toEqual({ doc: D })
    expect(parseDeepLink('?tab=settings')).toBeNull()
  })
  it('is null for an empty or unrelated query', () => {
    expect(parseDeepLink('')).toBeNull()
    expect(parseDeepLink('?join=abc')).toBeNull()
  })
})

describe('scrubDeepLink', () => {
  it('removes only the deep-link params', () => {
    expect(scrubDeepLink(`?doc=${D}&block=${B}&tab=review`)).toBe('')
    expect(scrubDeepLink(`?join=abc&doc=${D}`)).toBe('?join=abc')
    expect(scrubDeepLink('')).toBe('')
  })
})
