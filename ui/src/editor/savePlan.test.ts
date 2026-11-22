import { describe, expect, it } from 'vitest'
import { afterSave, requestFor } from './savePlan'

describe('afterSave', () => {
  it('is clean when nothing was typed during the request', () => {
    expect(afterSave({ editedMeanwhile: false, mounted: true })).toBe('clean')
    expect(afterSave({ editedMeanwhile: false, mounted: false })).toBe('clean')
  })
  it('re-arms the debounce for mid-save keystrokes while mounted', () => {
    expect(afterSave({ editedMeanwhile: true, mounted: true })).toBe('rearm')
  })
  it('saves again at once for mid-save keystrokes after unmount', () => {
    expect(afterSave({ editedMeanwhile: true, mounted: false })).toBe('save-now')
  })
})

describe('requestFor', () => {
  let n = 0
  const mint = () => `rid-${++n}`
  it('a retry of the same request keeps its request_id', () => {
    const first = requestFor(null, '{"ops":[1]}', mint)
    expect(requestFor(first, '{"ops":[1]}', mint)).toBe(first)
  })
  it('a different request gets a new one', () => {
    const first = requestFor(null, '{"ops":[1]}', mint)
    const next = requestFor(first, '{"ops":[1,2]}', mint)
    expect(next.id).not.toBe(first.id)
    expect(next.body).toBe('{"ops":[1,2]}')
  })
})
