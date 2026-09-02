import { describe, expect, it } from 'vitest'
import { proposeErrorText, refusalHint, saveErrorText } from './hints'

describe('refusalHint', () => {
  it('maps the typed refusal texts to a next action', () => {
    expect(refusalHint('owner refused pull: share is revoked')).toMatch(/ask for a fresh link/)
    expect(refusalHint('owner refused: unknown peer')).toMatch(/no longer recognises this Mac/)
    expect(refusalHint('owner refused the invite: expired')).toMatch(/no longer valid/)
    expect(refusalHint('invite already redeemed')).toMatch(/no longer valid/)
    expect(refusalHint('dial timed out after 10s (peer offline or unreachable)')).toMatch(/offline or unreachable/)
  })
  it('is null for empty or unknown text', () => {
    expect(refusalHint(null)).toBeNull()
    expect(refusalHint('')).toBeNull()
    expect(refusalHint('something else entirely')).toBeNull()
  })
})

describe('saveErrorText', () => {
  it('explains the freeze, an unreachable daemon and a moved doc', () => {
    expect(saveErrorText(new Error('doc is in a live session — edits go through the session'))).toMatch(/live session/)
    expect(saveErrorText(new TypeError('Failed to fetch'))).toMatch(/not responding/)
    expect(saveErrorText(new Error('stale base epoch 3 (doc is at 5)'))).toMatch(/changed underneath you/)
    expect(saveErrorText(new Error('propose: base epoch 9 is ahead of doc epoch 7'))).toMatch(/changed underneath you/)
  })
  it('keeps unknown causes but says the edit is kept', () => {
    expect(saveErrorText('weird')).toBe('not saved: weird — your edit is kept here and will retry')
  })
})

describe('proposeErrorText', () => {
  it('distinguishes our daemon from the owner', () => {
    expect(proposeErrorText(new TypeError('Failed to fetch'))).toMatch(/Grimoire is not responding/)
    expect(proposeErrorText(new Error('dial timed out after 10s'))).toMatch(/owner’s Mac is offline/)
    expect(proposeErrorText(new Error('share is view-only'))).toMatch(/no longer have permission/)
  })
})
