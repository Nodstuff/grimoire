// SERVER-mode sign-in for the web UI: a passkey opens a same-origin session
// cookie (`__Host-taisce_session`, HttpOnly — this script never sees it).
// The daemon answers /api with 401 until then; `api()` turns that into
// UNAUTHORIZED_EVENT, and AuthGate shows the sign-in screen. LOCAL mode
// never answers 401 and has no /auth/web routes, so none of this shows.

/** Every state-changing request carries it (with the browser's Origin):
 * the daemon refuses a cookie write without it (CSRF). */
export const CSRF_HEADER = 'Taisce-CSRF'

/** Fired on `window` when an /api call answers 401 (signed out, expired). */
export const UNAUTHORIZED_EVENT = 'taisce:unauthorized'

const SAFE = new Set(['GET', 'HEAD'])

/** `init` with the CSRF header added on anything but GET/HEAD. */
export function withCsrf(init?: RequestInit): RequestInit | undefined {
  const method = (init?.method ?? 'GET').toUpperCase()
  if (SAFE.has(method)) return init
  const headers = new Headers(init?.headers)
  headers.set(CSRF_HEADER, '1')
  return { ...init, headers }
}

export function signalUnauthorized(): void {
  try {
    window.dispatchEvent(new Event(UNAUTHORIZED_EVENT))
  } catch {
    // no window (tests in node): nothing to tell
  }
}

export interface WebSession {
  signed_in: boolean
  name: string | null
}

/** The browser's session in SERVER mode; null in LOCAL mode (the route is
 * absent there: the SPA fallback answers HTML, not JSON). */
export async function webSession(): Promise<WebSession | null> {
  try {
    const r = await fetch('/auth/web/session', { cache: 'no-store' })
    if (!r.ok) return null
    const j = await r.json()
    if (!j || typeof j !== 'object' || j.server !== true) return null
    return { signed_in: !!j.signed_in, name: typeof j.name === 'string' ? j.name : null }
  } catch {
    return null
  }
}

/** Does this page need to sign in before it can load anything? Only a 401
 * from /api says so; a daemon that is down is the app's banner, not ours. */
export async function needsSignIn(): Promise<boolean> {
  try {
    const r = await fetch('/api/stamp', { cache: 'no-store' })
    return r.status === 401
  } catch {
    return false
  }
}

// ---- base64url ⇄ ArrayBuffer around navigator.credentials ----

export function b64urlToBuf(s: string): ArrayBuffer {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4)
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out.buffer
}

export function bufToB64url(b: ArrayBuffer): string {
  const bytes = new Uint8Array(b)
  let bin = ''
  for (const x of bytes) bin += String.fromCharCode(x)
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

async function postJson<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, withCsrf({ method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }))
  const j = await r.json().catch(() => null)
  if (!r.ok) throw new Error((j && typeof j.error === 'string' && j.error) || `HTTP ${r.status}`)
  return j as T
}

interface BeginReply {
  ceremony: string
  options: { publicKey: Record<string, unknown> & { challenge: string; allowCredentials?: { id: string; type: string }[] } }
}

/** The bit of `navigator.credentials` sign-in uses (injectable for tests). */
export interface CredentialsLike {
  get(o: CredentialRequestOptions): Promise<Credential | null>
}

/** One passkey tap: challenge → assertion → session cookie. The signed-in
 * person's name. Throws `NotAllowedError` when the person cancels. */
export async function signIn(creds: CredentialsLike = navigator.credentials): Promise<string> {
  const b = await postJson<BeginReply>('/auth/web/begin', {})
  const pk = { ...b.options.publicKey } as Record<string, unknown>
  pk.challenge = b64urlToBuf(b.options.publicKey.challenge)
  if (b.options.publicKey.allowCredentials) {
    pk.allowCredentials = b.options.publicKey.allowCredentials.map((c) => ({ ...c, id: b64urlToBuf(c.id) }))
  }
  const a = (await creds.get({ publicKey: pk as unknown as PublicKeyCredentialRequestOptions })) as PublicKeyCredential | null
  if (!a) throw new Error('no passkey was chosen')
  const res = a.response as AuthenticatorAssertionResponse
  const credential = {
    id: a.id,
    rawId: bufToB64url(a.rawId),
    type: a.type,
    extensions: a.getClientExtensionResults ? a.getClientExtensionResults() : {},
    response: {
      authenticatorData: bufToB64url(res.authenticatorData),
      clientDataJSON: bufToB64url(res.clientDataJSON),
      signature: bufToB64url(res.signature),
      userHandle: res.userHandle ? bufToB64url(res.userHandle) : null,
    },
  }
  const done = await postJson<{ ok: boolean; name: string }>('/auth/web/finish', { ceremony: b.ceremony, credential })
  return done.name
}

/** Sign out: the daemon revokes this browser's session and clears the cookie. */
export async function signOut(): Promise<void> {
  await postJson<{ ok: boolean }>('/auth/web/logout', {})
}

/** What to tell the person when sign-in fails. */
export function signInError(e: unknown): string {
  if (e && typeof e === 'object' && 'name' in e && (e as { name: string }).name === 'NotAllowedError') return 'Cancelled.'
  return `Sign-in failed: ${e instanceof Error ? e.message : String(e)}`
}
