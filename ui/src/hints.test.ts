import { describe, expect, it } from 'vitest'
import { isPermanentSaveError, permanentSaveErrorText, saveErrorText } from './hints'
import { ApiError } from './types'

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

describe('isPermanentSaveError', () => {
  it('stops on refusals a retry cannot fix', () => {
    for (const code of ['not_found', 'forbidden', 'invalid', 'csrf']) expect(isPermanentSaveError(new ApiError('no', code))).toBe(true)
  })
  it('keeps retrying network failures, bare HTTP errors and a lapsed sign-in', () => {
    expect(isPermanentSaveError(new TypeError('Failed to fetch'))).toBe(false)
    expect(isPermanentSaveError(new ApiError('502 Bad Gateway'))).toBe(false)
    expect(isPermanentSaveError(new ApiError('sign in to continue', 'unauthorized'))).toBe(false)
    expect(isPermanentSaveError(new ApiError('database is locked', 'internal'))).toBe(false)
  })
  it('says the edit is kept and how to try again', () => {
    expect(permanentSaveErrorText(new ApiError('doc not found', 'not_found'))).toMatch(/doc not found.*kept here.*will not retry/)
  })
})
