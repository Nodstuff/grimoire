// The SERVER-mode sign-in screen and the gate that shows it. Before the
// first sign-in nothing of the app mounts (it would only collect 401s);
// when a session lapses mid-use the screen is an overlay, so an open
// editor keeps its unsaved text and saves once you are back.

import { useEffect, useState, type ReactNode } from 'react'
import { UNAUTHORIZED_EVENT, needsSignIn, signIn, signInError } from './auth'

/** checking: probing /api · signin: signed out, app not mounted ·
 * app: running · expired: running, session lapsed (overlay). */
export type GateState = 'checking' | 'signin' | 'app' | 'expired'

/** What an /api 401 does to the gate. */
export function onUnauthorized(s: GateState): GateState {
  if (s === 'app') return 'expired'
  if (s === 'checking') return 'signin'
  return s
}

export function SignIn({ expired = false, onSignedIn }: { expired?: boolean; onSignedIn: (name: string) => void }) {
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<string | null>(null)
  const supported = typeof window === 'undefined' || 'PublicKeyCredential' in window
  const go = async () => {
    if (busy) return
    setBusy(true)
    setMsg('Waiting for your passkey…')
    try {
      const name = await signIn()
      setMsg(null)
      onSignedIn(name)
    } catch (e) {
      setMsg(signInError(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className={`signin ${expired ? 'signin-overlay' : ''}`} role="dialog" aria-modal="true" aria-labelledby="signin-title">
      <div className="signin-card">
        <div className="signin-mark">◈</div>
        <h1 id="signin-title" className="signin-title">{expired ? 'Signed out' : 'Sign in to Taisce'}</h1>
        <p className="meta">
          {expired
            ? 'Your session ended. Sign in again to keep working; nothing you typed is lost.'
            : 'Use the passkey you enrolled for this server.'}
        </p>
        <button className="accept signin-go" disabled={busy || !supported} onClick={go} autoFocus>
          Sign in with passkey
        </button>
        <p className="meta signin-msg" role="status">
          {supported ? msg : 'This browser does not support passkeys.'}
        </p>
      </div>
    </div>
  )
}

export default function AuthGate({ children, initial }: { children: ReactNode; initial?: GateState }) {
  const [state, setState] = useState<GateState>(initial ?? 'checking')
  useEffect(() => {
    const on = () => setState(onUnauthorized)
    window.addEventListener(UNAUTHORIZED_EVENT, on)
    if (!initial) needsSignIn().then((need) => setState((s) => (s === 'checking' ? (need ? 'signin' : 'app') : s)))
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, on)
  }, [initial])
  if (state === 'checking') return null
  if (state === 'signin') return <SignIn onSignedIn={() => setState('app')} />
  return (
    <>
      {children}
      {state === 'expired' && <SignIn expired onSignedIn={() => setState('app')} />}
    </>
  )
}
