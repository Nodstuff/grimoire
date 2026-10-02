import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createElement } from 'react'
import { renderToString } from 'react-dom/server'
import { CSRF_HEADER, UNAUTHORIZED_EVENT, b64urlToBuf, bufToB64url, needsSignIn, signIn, signInError, signOut, webSession, withCsrf } from './auth'
import AuthGate, { onUnauthorized } from './SignIn'
import { ApiError, api } from './types'

type Call = { url: string; init?: RequestInit }
let calls: Call[] = []

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
}

/** fetch answering by path; every call recorded. */
function mockFetch(routes: Record<string, () => Response>) {
  calls = []
  vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
    calls.push({ url, init })
    const r = routes[url.split('?')[0]]
    if (!r) throw new Error(`unexpected fetch ${url}`)
    return r()
  })
}

const header = (c: Call, name: string) => new Headers(c.init?.headers).get(name)

beforeEach(() => {
  // node has no window: an EventTarget stands in for it
  vi.stubGlobal('window', new EventTarget())
})
afterEach(() => {
  vi.unstubAllGlobals()
})

describe('401 → sign-in', () => {
  it('an /api 401 throws `unauthorized` and tells the gate', async () => {
    mockFetch({ '/api/stamp': () => json(401, { error: 'authentication required', code: 'unauthorized' }) })
    let told = 0
    window.addEventListener(UNAUTHORIZED_EVENT, () => (told += 1))
    const err = await api('/api/stamp').catch((e) => e)
    expect(err).toBeInstanceOf(ApiError)
    expect((err as ApiError).code).toBe('unauthorized')
    expect(told).toBe(1)
  })

  it('needsSignIn is true only for a 401 (a down daemon is not a sign-in)', async () => {
    mockFetch({ '/api/stamp': () => json(401, {}) })
    expect(await needsSignIn()).toBe(true)
    mockFetch({ '/api/stamp': () => json(200, { stamp: 1 }) })
    expect(await needsSignIn()).toBe(false)
    vi.stubGlobal('fetch', async () => {
      throw new TypeError('network down')
    })
    expect(await needsSignIn()).toBe(false)
  })

  it('the gate shows the sign-in screen, not the app, when signed out', () => {
    const app = createElement('div', null, 'THE-APP')
    const signedOut = renderToString(createElement(AuthGate, { initial: 'signin', children: app }))
    expect(signedOut).toContain('Sign in with passkey')
    expect(signedOut).not.toContain('THE-APP')
    const running = renderToString(createElement(AuthGate, { initial: 'app', children: app }))
    expect(running).toContain('THE-APP')
    expect(running).not.toContain('Sign in with passkey')
    // a lapsed session overlays the running app (unsaved edits stay mounted)
    const lapsed = renderToString(createElement(AuthGate, { initial: 'expired', children: app }))
    expect(lapsed).toContain('THE-APP')
    expect(lapsed).toContain('Sign in with passkey')
  })

  it('a 401 before the app runs is sign-in; during it, the overlay', () => {
    expect(onUnauthorized('checking')).toBe('signin')
    expect(onUnauthorized('app')).toBe('expired')
    expect(onUnauthorized('expired')).toBe('expired')
    expect(onUnauthorized('signin')).toBe('signin')
  })
})

describe('CSRF header', () => {
  it('rides on every write and on no read', async () => {
    mockFetch({ '/api/docs': () => json(200, { id: 'd' }), '/api/workspaces/w': () => json(200, { ok: true }) })
    await api('/api/docs')
    await api('/api/docs', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' })
    await api('/api/workspaces/w', { method: 'PATCH', body: '{}' })
    await api('/api/workspaces/w', { method: 'delete' })
    expect(header(calls[0], CSRF_HEADER)).toBeNull()
    expect(calls.slice(1).map((c) => header(c, CSRF_HEADER))).toEqual(['1', '1', '1'])
    expect(header(calls[1], 'content-type')).toBe('application/json')
  })

  it('withCsrf leaves GET/HEAD alone', () => {
    expect(withCsrf(undefined)).toBeUndefined()
    expect(withCsrf({ method: 'HEAD' })).toEqual({ method: 'HEAD' })
    expect(new Headers(withCsrf({ method: 'PUT' })!.headers).get(CSRF_HEADER)).toBe('1')
  })
})

describe('passkey sign-in and sign-out', () => {
  it('base64url round-trips', () => {
    const s = bufToB64url(new Uint8Array([251, 255, 0, 1, 2]).buffer)
    expect(s).toBe('-_8AAQI')
    expect(Array.from(new Uint8Array(b64urlToBuf(s)))).toEqual([251, 255, 0, 1, 2])
  })

  it('begin → navigator.credentials.get → finish, with the assertion base64url-encoded', async () => {
    const challenge = bufToB64url(new Uint8Array([1, 2, 3]).buffer)
    mockFetch({
      '/auth/web/begin': () =>
        json(200, { ceremony: 'cer1', options: { publicKey: { challenge, rpId: 'taisce.example', allowCredentials: [{ type: 'public-key', id: 'AQI' }] } } }),
      '/auth/web/finish': () => json(200, { ok: true, name: 'Tom' }),
    })
    let asked: CredentialRequestOptions | null = null
    const buf = (n: number) => new Uint8Array([n, n]).buffer
    const creds = {
      get: async (o: CredentialRequestOptions) => {
        asked = o
        return {
          id: 'cred',
          rawId: buf(9),
          type: 'public-key',
          getClientExtensionResults: () => ({}),
          response: { authenticatorData: buf(1), clientDataJSON: buf(2), signature: buf(3), userHandle: null },
        } as unknown as Credential
      },
    }
    expect(await signIn(creds)).toBe('Tom')
    const pk = asked!.publicKey!
    expect(Array.from(new Uint8Array(pk.challenge as ArrayBuffer))).toEqual([1, 2, 3])
    expect(Array.from(new Uint8Array(pk.allowCredentials![0].id as ArrayBuffer))).toEqual([1, 2])
    const fin = JSON.parse(String(calls[1].init!.body))
    expect(fin.ceremony).toBe('cer1')
    expect(fin.credential.rawId).toBe(bufToB64url(buf(9)))
    expect(fin.credential.response.signature).toBe(bufToB64url(buf(3)))
    expect(calls.map((c) => [c.url, c.init?.method, header(c, CSRF_HEADER)])).toEqual([
      ['/auth/web/begin', 'POST', '1'],
      ['/auth/web/finish', 'POST', '1'],
    ])
  })

  it('a refused assertion or a cancel says so', async () => {
    mockFetch({
      '/auth/web/begin': () => json(200, { ceremony: 'c', options: { publicKey: { challenge: 'AQ' } } }),
      '/auth/web/finish': () => json(401, { error: 'passkey not accepted' }),
    })
    const creds = {
      get: async () =>
        ({ id: 'x', rawId: new ArrayBuffer(1), type: 'public-key', response: { authenticatorData: new ArrayBuffer(1), clientDataJSON: new ArrayBuffer(1), signature: new ArrayBuffer(1), userHandle: null } }) as unknown as Credential,
    }
    const e = await signIn(creds).catch((x) => x)
    expect(signInError(e)).toBe('Sign-in failed: passkey not accepted')
    expect(signInError(Object.assign(new Error('x'), { name: 'NotAllowedError' }))).toBe('Cancelled.')
  })

  it('sign-out posts the logout with the CSRF header', async () => {
    mockFetch({ '/auth/web/logout': () => json(200, { ok: true }) })
    await signOut()
    expect(calls).toHaveLength(1)
    expect(calls[0].init?.method).toBe('POST')
    expect(header(calls[0], CSRF_HEADER)).toBe('1')
  })

  it('webSession: signed in on a server, null in LOCAL mode (the SPA fallback answers HTML)', async () => {
    mockFetch({ '/auth/web/session': () => json(200, { server: true, signed_in: true, name: 'Tom' }) })
    expect(await webSession()).toEqual({ signed_in: true, name: 'Tom' })
    mockFetch({ '/auth/web/session': () => new Response('<!doctype html>', { status: 200, headers: { 'content-type': 'text/html' } }) })
    expect(await webSession()).toBeNull()
  })
})
