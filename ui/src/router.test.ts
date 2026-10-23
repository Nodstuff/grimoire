import { describe, expect, it } from 'vitest'
import { legacyRedirect, parseRoute, routePath, sameRoute, type Route } from './router'

const A = '0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d'
const B = '11111111-2222-4333-8444-555555555555'

describe('routes', () => {
  it('every route round-trips through its path', () => {
    const routes: Route[] = [
      { kind: 'home' },
      { kind: 'doc', id: A },
      { kind: 'doc', id: A, block: B },
      { kind: 'library', workspace: null },
      { kind: 'library', workspace: 'unsorted' },
      { kind: 'library', workspace: B },
      { kind: 'library', workspace: null, folder: A },
      { kind: 'library', workspace: 'unsorted', folder: A },
      { kind: 'review' },
      { kind: 'search', q: '' },
      { kind: 'search', q: 'review gate & co' },
      { kind: 'books' },
      { kind: 'runs' },
      { kind: 'graph' },
      { kind: 'trash' },
      { kind: 'settings', section: 'profile' },
      { kind: 'settings', section: 'ereaders' },
      { kind: 'settings', section: 'uploads' },
    ]
    for (const r of routes) {
      const p = routePath(r)
      const [path, search] = p.split('?')
      expect(parseRoute(path, search ? `?${search}` : '')).toEqual(r)
    }
  })
  it('paths read as the design names them', () => {
    expect(routePath({ kind: 'home' })).toBe('/')
    expect(routePath({ kind: 'doc', id: A })).toBe(`/doc/${A}`)
    expect(routePath({ kind: 'runs' })).toBe('/gardeners')
    expect(routePath({ kind: 'settings', section: 'profile' })).toBe('/settings')
    expect(routePath({ kind: 'search', q: 'a b' })).toBe('/search?q=a%20b')
    expect(routePath({ kind: 'library', workspace: 'all' })).toBe('/library')
    expect(routePath({ kind: 'library', workspace: null, folder: A })).toBe(`/library?folder=${A}`)
    // a bad folder id is ignored, not a not-found
    expect(parseRoute('/library', '?folder=nope')).toEqual({ kind: 'library', workspace: null })
  })
  it('forgives trailing slashes and case of fixed segments', () => {
    expect(parseRoute('/Review/')).toEqual({ kind: 'review' })
    expect(parseRoute('/settings/ereaders/')).toEqual({ kind: 'settings', section: 'ereaders' })
    expect(parseRoute('')).toEqual({ kind: 'home' })
  })
  it('bad ids and unknown places are not found, never guessed', () => {
    expect(parseRoute('/doc/not-a-uuid').kind).toBe('notfound')
    expect(parseRoute('/settings/nope').kind).toBe('notfound')
    expect(parseRoute('/wat').kind).toBe('notfound')
    expect(parseRoute(`/doc/${A}/extra`).kind).toBe('notfound')
    // a bad block is dropped, the doc still opens
    expect(parseRoute(`/doc/${A}`, '?block=zzz')).toEqual({ kind: 'doc', id: A })
  })
  it('sameRoute compares places', () => {
    expect(sameRoute({ kind: 'settings', section: 'profile' }, parseRoute('/settings'))).toBe(true)
    expect(sameRoute({ kind: 'doc', id: A }, { kind: 'doc', id: B })).toBe(false)
  })
})

describe('legacy query links', () => {
  it('?doc= and ?block= become /doc/<id>?block=', () => {
    expect(legacyRedirect('/', `?doc=${A}`)).toBe(`/doc/${A}`)
    expect(legacyRedirect('/', `?doc=${A}&block=${B}`)).toBe(`/doc/${A}?block=${B}`)
  })
  it('?tab= lands on its place; profile and ereaders are Settings sections', () => {
    expect(legacyRedirect('/', '?tab=review')).toBe('/review')
    expect(legacyRedirect('/', '?tab=runs')).toBe('/gardeners')
    expect(legacyRedirect('/', '?tab=books')).toBe('/books')
    expect(legacyRedirect('/', '?tab=trash')).toBe('/trash')
    expect(legacyRedirect('/', '?tab=profile')).toBe('/settings')
    expect(legacyRedirect('/', '?tab=ereaders')).toBe('/settings/ereaders')
    expect(legacyRedirect('/', '?tab=home')).toBe('/')
  })
  it('keeps other params (capture=1) and ignores non-root paths and junk', () => {
    expect(legacyRedirect('/', `?doc=${A}&capture=1`)).toBe(`/doc/${A}?capture=1`)
    expect(legacyRedirect('/', '?tab=review&capture=1')).toBe('/review?capture=1')
    expect(legacyRedirect('/', '?capture=1')).toBeNull()
    expect(legacyRedirect('/', '?doc=nope')).toBeNull()
    expect(legacyRedirect('/', '?tab=bogus')).toBeNull()
    expect(legacyRedirect('/review', `?doc=${A}`)).toBeNull()
  })
})
