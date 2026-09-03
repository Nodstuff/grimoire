import { describe, expect, it } from 'vitest'
import { afterSave } from './savePlan'

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
