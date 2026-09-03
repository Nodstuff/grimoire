import { describe, expect, it } from 'vitest'
import { chipText, groupShares, mirrorStatusLine, offerLine, shareTitle, shareWho } from './shares'
import type { MirrorRow, Share } from './types'

function share(id: string, state: Share['state'], created_at: string, extra: Partial<Share> = {}): Share {
  return {
    id,
    root_doc: `doc-${id}`,
    contact: null,
    permission: 'view',
    state,
    policy_override: null,
    created_at,
    trust: 'review',
    ...extra,
  }
}

describe('groupShares', () => {
  it('orders active, offered, revoked; newest first within each', () => {
    const rows = [
      share('r1', 'revoked', '2026-09-01T00:00:00Z'),
      share('a1', 'active', '2026-08-01T00:00:00Z'),
      share('o1', 'offered', '2026-09-02T00:00:00Z'),
      share('a2', 'active', '2026-09-02T00:00:00Z'),
      share('r2', 'revoked', '2026-09-02T00:00:00Z'),
    ]
    const g = groupShares(rows)
    expect(g.active.map((s) => s.id)).toEqual(['a2', 'a1'])
    expect(g.offered.map((s) => s.id)).toEqual(['o1'])
    expect(g.revoked.map((s) => s.id)).toEqual(['r2', 'r1'])
  })
  it('parks unknown states with revoked rather than dropping them', () => {
    const g = groupShares([share('x', 'weird' as Share['state'], '2026-09-02T00:00:00Z')])
    expect(g.revoked.map((s) => s.id)).toEqual(['x'])
  })
  it('handles an empty list', () => {
    expect(groupShares([])).toEqual({ active: [], offered: [], revoked: [] })
  })
})

describe('shareWho / shareTitle', () => {
  it('prefers the daemon-provided petname', () => {
    expect(shareWho(share('a', 'active', '', { contact: 'c1', contact_petname: 'alice' }))).toBe('alice')
  })
  it('falls back to a contact lookup for older daemons', () => {
    expect(shareWho(share('a', 'active', '', { contact: 'c1' }), (id) => (id === 'c1' ? 'bob' : undefined))).toBe('bob')
    expect(shareWho(share('a', 'active', '', { contact: 'c9' }), () => undefined)).toBe('?')
  })
  it('says not yet joined for an unredeemed invite', () => {
    expect(shareWho(share('o', 'offered', ''))).toBe('not yet joined')
  })
  it('title: root_title, then lookup, then short id', () => {
    expect(shareTitle(share('a', 'active', '', { root_title: 'Plan' }))).toBe('Plan')
    expect(shareTitle(share('a', 'active', ''), () => 'Looked up')).toBe('Looked up')
    expect(shareTitle(share('abcdefghijk', 'active', ''))).toBe('doc-abcd')
  })
})

describe('mirrorStatusLine', () => {
  const NOW = Date.parse('2026-09-02T12:00:00Z')
  const row = (extra: Partial<MirrorRow>): MirrorRow => ({
    share_id: 's',
    owner_petname: 'alice',
    owner_pubkey: 'ab'.repeat(32),
    permission: 'view',
    root_doc_id: 'd',
    root_title: 'Plan',
    ...extra,
  })
  it('reports docs behind before saying up to date', () => {
    const r = mirrorStatusLine(row({ behind: 3, last_pulled_at: new Date(NOW - 5_000).toISOString() }), NOW)
    expect(r.kind).toBe('behind')
    expect(r.text).toMatch(/^3 docs behind/)
  })

  it('synced <rel time> when the last pull succeeded', () => {
    const r = mirrorStatusLine(row({ last_pulled_at: new Date(NOW - 12_000).toISOString() }), NOW)
    expect(r).toEqual({ kind: 'ok', text: 'up to date · synced 12s ago' })
  })
  it('sync failing wins over a stale successful pull', () => {
    const r = mirrorStatusLine(
      row({ last_pulled_at: new Date(NOW - 12_000).toISOString(), last_error: 'FOREIGN KEY constraint failed' }),
      NOW,
    )
    expect(r.kind).toBe('failing')
    expect(r.text).toBe('sync failing: FOREIGN KEY constraint failed')
  })
  it('never synced when nothing has been pulled', () => {
    expect(mirrorStatusLine(row({}), NOW)).toEqual({ kind: 'never', text: 'never synced' })
    expect(mirrorStatusLine(row({ last_pulled_at: null, last_error: '   ' }), NOW).kind).toBe('never')
  })
})

describe('invites v2 seams', () => {
  it('shareWho says who an offered invite is waiting for', () => {
    expect(shareWho(share('o', 'offered', '', { offered_to_petname: 'alice · 3f9a' }))).toBe('waiting for alice · 3f9a')
    expect(shareWho(share('o', 'offered', ''))).toBe('not yet joined')
  })
  it('offerLine reads naturally for both grants', () => {
    expect(offerLine({ from_petname: 'alice · 3f9a', root_title: 'Plan', permission: 'propose' })).toBe(
      'alice · 3f9a wants to share “Plan” with you · can propose edits',
    )
    expect(offerLine({ from_petname: 'bob', root_title: 'Notes', permission: 'view' })).toBe(
      'bob wants to share “Notes” with you · view only',
    )
  })
  it('chipText combines counts and hides when both are zero', () => {
    expect(chipText(0, 0)).toBeNull()
    expect(chipText(3, 0)).toBe('3 to review')
    expect(chipText(0, 1)).toBe('1 share request')
    expect(chipText(2, 2)).toBe('2 to review · 2 share requests')
  })
})

describe('hub lines', () => {
  it('says where I stand at a hub', async () => {
    const { hubStandingLine, publishedLine, relayedLine, transferredLine, waitingLine } = await import('./shares')
    expect(hubStandingLine('member', 'pending')).toBe('waiting for an admin to approve you')
    expect(hubStandingLine('admin', 'pending')).toBe('waiting for an admin to approve you')
    expect(hubStandingLine('member', 'active')).toBe('you are a member')
    expect(hubStandingLine('admin', 'active')).toBe('you are an admin')
    expect(hubStandingLine('member', 'ejected')).toBe('you were removed')
    expect(publishedLine('Team', 2)).toBe('published to Team: 2')
    expect(relayedLine('Team', 'alice')).toBe('⌂ Team · owned by alice · your edits go to alice as suggestions')
    expect(relayedLine('Team', 'alice', false)).toBe('⌂ Team · owned by alice · view only')
    expect(transferredLine('Team')).toBe('⌂ owned by Team (transferred from you) · your edits go to Team as suggestions')
    expect(waitingLine(0)).toBeNull()
    expect(waitingLine(1)).toBe('1 proposal waiting')
    expect(waitingLine(3)).toBe('3 proposals waiting')
  })
})
