// Profile: the owner's display name and who this browser is signed in as.
// Two surfaces share the same save path: the Settings page's Profile section
// (canvas Settings: name + Save, Account, Signed in) and the first-run
// prompt shown until the install-default name has been confirmed once.
// Also the version + diagnostics helpers the About section uses.

import { useEffect, useState } from 'react'
import { Button } from './lapis/Button'
import { CopyButton, Row, RowText, SettingsCard } from './screens/settingsParts'
import { Dialog } from './lapis/Dialog'
import { TextField } from './lapis/Field'
import { api, Profile as ProfileRow } from './types'
import { errText, notify } from './Notice'

interface Diagnostics {
  version: string
  log_path: string | null
  log_tail: string
}

/** Clipboard write that also works where navigator.clipboard is unavailable
 * (older WKWebView, non-secure contexts): fall back to a hidden textarea. */
export async function copyText(text: string): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text)
      return
    }
  } catch {
    // fall through to the textarea path
  }
  const ta = document.createElement('textarea')
  ta.value = text
  ta.setAttribute('readonly', '')
  ta.style.position = 'fixed'
  ta.style.opacity = '0'
  document.body.appendChild(ta)
  ta.select()
  const ok = document.execCommand('copy')
  document.body.removeChild(ta)
  if (!ok) throw new Error('clipboard unavailable')
}

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
 * click-outside — a name is how your edits are attributed. */
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
    <Dialog
      onClose={() => {}}
      locked
      title="What should others call you?"
      description="Your edits and reviews are signed with this name. You can change it later in Settings › Profile."
      footer={
        <Button variant="primary" disabled={!ok || busy} onClick={save}>
          {busy ? 'Saving…' : 'Save'}
        </Button>
      }
    >
      <TextField
        autoFocus
        data-autofocus
        label="Your name"
        hideLabel
        value={name}
        maxLength={64}
        placeholder="Your name"
        error={!!err}
        help={err ?? undefined}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') save()
          if (e.key === 'Escape') e.stopPropagation()
        }}
      />
    </Dialog>
  )
}

/** Settings › Profile (canvas Settings, #profile). */
export default function Profile({
  dataVersion,
  onChanged,
  onSignOut,
}: {
  dataVersion: number
  onChanged?: (p: ProfileRow) => void
  /** SERVER mode, signed in through this browser */
  onSignOut?: () => void
}) {
  const [profile, setProfile] = useState<ProfileRow | null | undefined>(undefined)
  const [name, setName] = useState('')
  const [busy, setBusy] = useState(false)

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

  if (profile === null) {
    return (
      <SettingsCard id="profile" title="Profile">
        <p className="st-note">Taisce is out of date: reinstall the latest release.</p>
      </SettingsCard>
    )
  }

  return (
    <SettingsCard id="profile" title="Profile">
      <form
        className="st-inline-form"
        onSubmit={(e) => {
          e.preventDefault()
          save()
        }}
      >
        <div className="st-fld st-grow">
          <label htmlFor="st-name">Your name</label>
          <input id="st-name" className="st-inp" value={name} maxLength={64} disabled={profile === undefined} onChange={(e) => setName(e.target.value)} />
        </div>
        <Button type="submit" size="sm" className="st-tall" disabled={!ok || !dirty || busy}>
          {busy ? 'Saving…' : 'Save'}
        </Button>
      </form>
      <p className="st-note">
        Your edits and reviews are attributed to this name.
        {profile && !profile.confirmed && <b> It is still the install default: pick your own.</b>}
      </p>
      {profile && (
        <Row>
          <RowText>Account</RowText>
          <span className="mono muted st-mono" title={profile.principal_id}>
            {shortId(profile.principal_id)}
          </span>
          <CopyButton what="account id" value={profile.principal_id} variant="ghost" />
        </Row>
      )}
      {onSignOut && (
        <Row>
          <RowText>Signed in with a passkey · {typeof location === 'undefined' ? 'this server' : location.host}</RowText>
          <Button size="sm" onClick={onSignOut}>
            Sign out
          </Button>
        </Row>
      )}
    </SettingsCard>
  )
}

/** The version line and Copy diagnostics (Settings › About). */
export function useVersion(appVersion: string | null): string | null {
  // App polls /api/stamp every 2.5s and newer daemons put the version on it;
  // only an older daemon (no version there) costs a /api/buildinfo call
  const [fetched, setFetched] = useState<string | null>(null)
  useEffect(() => {
    if (appVersion) return
    api<{ version?: string }>('/api/buildinfo')
      .then((b) => setFetched(b.version ?? null))
      .catch(() => setFetched(null))
  }, [appVersion])
  return appVersion ?? fetched
}

export async function copyDiagnostics(): Promise<void> {
  try {
    const d = await api<Diagnostics>('/api/diagnostics')
    const lines = [`Taisce ${d.version}`, `log: ${d.log_path ?? '—'}`, '', d.log_tail]
    await copyText(lines.join('\n'))
    notify('diagnostics copied — paste them into your report', 'ok')
  } catch (e) {
    notify(`could not copy diagnostics: ${errText(e)}`)
  }
}
