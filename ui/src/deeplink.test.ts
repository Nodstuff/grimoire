import { describe, expect, it } from 'vitest'
import { parseDeepLink, scrubDeepLink, tabTarget } from './deeplink'
import { settingsSections } from './Settings'

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
    expect(parseDeepLink('?tab=nope')).toBeNull()
  })
  it('is null for an empty or unrelated query', () => {
    expect(parseDeepLink('')).toBeNull()
    expect(parseDeepLink('?capture=1')).toBeNull()
  })
})

describe('scrubDeepLink', () => {
  it('removes only the deep-link params', () => {
    expect(scrubDeepLink(`?doc=${D}&block=${B}&tab=review`)).toBe('')
    expect(scrubDeepLink(`?capture=1&doc=${D}`)).toBe('?capture=1')
    expect(scrubDeepLink('')).toBe('')
  })
})

describe('settings deep links', () => {
  it('?tab=settings opens Settings on Profile', () => {
    expect(parseDeepLink('?tab=settings')).toEqual({ tab: 'settings' })
    expect(tabTarget('settings')).toEqual({ kind: 'settings', section: 'profile' })
  })
  it('the old ?tab=profile and ?tab=ereaders land on their Settings sections', () => {
    expect(tabTarget(parseDeepLink('?tab=profile')!.tab!)).toEqual({ kind: 'settings', section: 'profile' })
    expect(tabTarget(parseDeepLink('?tab=ereaders')!.tab!)).toEqual({ kind: 'settings', section: 'ereaders' })
  })
  it('other tabs are their own views', () => {
    for (const t of ['home', 'review', 'runs', 'graph', 'trash', 'books'] as const) expect(tabTarget(t)).toEqual({ kind: t })
  })
  it('E-readers and Upload tokens show in SERVER mode only; asked for in LOCAL, Settings opens on Profile', () => {
    expect(settingsSections(true, 'ereaders')).toMatchObject({ active: 'ereaders' })
    expect(settingsSections(true, 'uploads')).toMatchObject({ active: 'uploads' })
    expect(settingsSections(true, 'ereaders').shown.map((s) => s.id)).toEqual(['profile', 'appearance', 'ereaders', 'uploads', 'workspaces', 'links', 'gardeners', 'data', 'about'])
    for (const want of ['ereaders', 'uploads', 'links'] as const) {
      const local = settingsSections(false, want)
      expect(local.active).toBe('profile')
      expect(local.shown.map((s) => s.id)).toEqual(['profile', 'appearance', 'workspaces', 'gardeners', 'data', 'about'])
    }
    expect(settingsSections(false, 'appearance').active).toBe('appearance')
  })
})
