import { describe, expect, it } from 'vitest'
import { saveErrorText } from './hints'

describe('saveErrorText', () => {
  it('explains an unreachable daemon and a moved doc', () => {
    expect(saveErrorText(new TypeError('Failed to fetch'))).toMatch(/not responding/)
    expect(saveErrorText(new Error('stale base epoch 3 (doc is at 5)'))).toMatch(/changed underneath you/)
    expect(saveErrorText(new Error('propose: base epoch 9 is ahead of doc epoch 7'))).toMatch(/changed underneath you/)
  })
  it('keeps unknown causes but says the edit is kept', () => {
    expect(saveErrorText('weird')).toBe('not saved: weird — your edit is kept here and will retry')
  })
})
