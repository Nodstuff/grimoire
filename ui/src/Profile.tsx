// Profile: the name contacts see, plus this node's id and fingerprint. Two
// surfaces share the same save path: the Profile page (⌘K → Profile, or the
// name chip on the Shares page) and the first-run prompt shown until the
// install-default name has been confirmed once.

import { useEffect, useState } from 'react'
import PaletteShell from './PaletteShell'
import { api, Profile as ProfileRow } from './types'
import { errText, notify } from './Notice'

export function loadProfile(): Promise<ProfileRow | null> {
  // older daemon without the route → null, and every caller degrades to "no profile"
  return api<ProfileRow>('/api/profile').catch(() => null)
}

export async function saveName(name: string): Promise<string> {
  const r = await api<{ ok: boolean; name: string }>('/api/profile', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name }),
  })
  return r.name
}

/** Client-side mirror of the server rule (trim, 1..64) so the button state is
 * honest before the round-trip. */
export function validName(raw: string): string | null {
  const n = raw.trim()
  return n.length >= 1 && n.length <= 64 ? n : null
}

function shortId(id: string | undefined): string {
  if (!id) return '—'
  return id.length > 20 ? `${id.slice(0, 8)}…${id.slice(-6)}` : id
}

/** Shown on load while `confirmed === false`. Not dismissible by Esc or
 * click-outside — a name is required for sharing to mean anything. */
export function FirstRunName({ profile, onSaved }: { profile: ProfileRow; onSaved: (p: ProfileRow) => void }) {
  const [name, setName] = useState(profile.name)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const ok = validName(name)

  const save = async () => {
    if (!ok || busy) return
    setBusy(true)
    setErr(null)
    try {
      const saved = await saveName(ok)
      onSaved({ ...profile, name: saved, confirmed: true })
    } catch (e) {
      setErr(errText(e))
    }
    setBusy(false)
  }

  return (
    <PaletteShell onClose={() => {}} locked>
      <div className="first-run">
        <div className="first-run-title">What should others call you?</div>
        <div className="meta">
          this is the name your contacts see on shares, proposals and edits — you can change it
          later under Profile
        </div>
        <input
          autoFocus
          value={name}
          maxLength={64}
          placeholder="your name"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') save()
            if (e.key === 'Escape') e.stopPropagation()
          }}
        />
        <div className="first-run-actions">
          {err && <span className="meta err">{err}</span>}
          <button className="accept" disabled={!ok || busy} onClick={save}>
            {busy ? 'saving…' : 'save'}
          </button>
        </div>
      </div>
    </PaletteShell>
  )
}

/** The Profile page (view kind 'profile'). */
export default function Profile({ dataVersion, onChanged }: { dataVersion: number; onChanged?: (p: ProfileRow) => void }) {
  const [profile, setProfile] = useState<ProfileRow | null | undefined>(undefined)
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)
  const [copied, setCopied] = useState<string | null>(null)

  useEffect(() => {
    loadProfile().then((p) => {
      setProfile(p)
      if (p) setName(p.name)
    })
  }, [dataVersion])

  const dirty = profile ? name.trim() !== profile.name : false
  const ok = validName(name)

  const save = async () => {
    if (!profile || !ok || !dirty || busy) return
    setBusy(true)
    try {
      const saved = await saveName(ok)
      const next = { ...profile, name: saved, confirmed: true }
      setProfile(next)
      setName(saved)
      onChanged?.(next)
      notify('name saved', 'ok')
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }

  const copy = (label: string, value: string) => {
    navigator.clipboard.writeText(value).then(() => {
      setCopied(label)
      setTimeout(() => setCopied(null), 1800)
    })
  }

  if (profile === undefined) return <div className="queue"><h1 className="queue-title">profile</h1></div>
  if (profile === null) {
    return (
      <div className="queue">
        <h1 className="queue-title">profile</h1>
        <div className="palette-empty">this daemon does not expose a profile yet — update and restart it</div>
      </div>
    )
  }

  return (
    <div className="queue">
      <h1 className="queue-title">profile</h1>

      <div className="card">
        <div className="card-head">
          <span>name</span>
          <span className="meta">this is the name your contacts see</span>
        </div>
        <div className="profile-name-row">
          <input
            className="join-input profile-name"
            value={name}
            maxLength={64}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && save()}
          />
          <button className="accept" disabled={!ok || !dirty || busy} onClick={save}>
            {busy ? 'saving…' : 'save'}
          </button>
        </div>
        {!profile.confirmed && (
          <div className="meta">still the install default — pick something your contacts will recognise</div>
        )}
      </div>

      <div className="card">
        <div className="card-head">
          <span>identity</span>
          <span className="meta">share the fingerprint out-of-band so contacts can verify you</span>
        </div>
        <div className="profile-kv">
          <span className="meta">node id</span>
          <span className="mono" title={profile.node_id ?? ''}>{shortId(profile.node_id)}</span>
          {profile.node_id && (
            <button className="chip" onClick={() => copy('node', profile.node_id!)}>
              {copied === 'node' ? 'copied ✓' : 'copy'}
            </button>
          )}
        </div>
        <div className="profile-kv">
          <span className="meta">fingerprint</span>
          <span className="mono">{profile.fingerprint ?? '—'}</span>
          {profile.fingerprint && (
            <button className="chip" onClick={() => copy('fp', profile.fingerprint!)}>
              {copied === 'fp' ? 'copied ✓' : 'copy'}
            </button>
          )}
        </div>
        <div className="profile-kv">
          <span className="meta">principal</span>
          <span className="mono" title={profile.principal_id}>{shortId(profile.principal_id)}</span>
        </div>
      </div>
    </div>
  )
}
