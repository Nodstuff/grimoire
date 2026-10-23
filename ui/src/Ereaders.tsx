// E-readers (view kind 'ereaders', a Settings surface): the devices that
// pull from the library. Add a device and its password shows exactly ONCE
// in a setup card with everything a device needs (username, OPDS URLs, the
// kosync server); rotate (a fresh password, shown once) and remove sit
// behind arm-then-confirm (window.confirm is a no-op in the Tauri webview).
// Removed devices (revoked_at set) are hidden.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { webSession } from './auth'
import { errText, notify } from './Notice'
import { copyText } from './Profile'
import { relTime } from './time'
import { Ereader, activeEreaders, createEreader, deleteEreader, deviceSetup, listEreaders, rotateEreader } from './books'

/** The device-name limit the server enforces (opds.rs MAX_LABEL). */
export const EREADER_LABEL_MAX = 60

export default function Ereaders({ dataVersion, embedded = false }: { dataVersion: number; /** inside Settings: no title of its own */ embedded?: boolean }) {
  const [list, setList] = useState<Ereader[] | null>(null)
  const [label, setLabel] = useState('')
  const [stream, setStream] = useState(false)
  const [busy, setBusy] = useState(false)
  // the one-time password reveal: set by create and by rotate
  const [secret, setSecret] = useState<{ label: string; password: string } | null>(null)
  // the person's Taisce name: the username every device signs in with
  const [username, setUsername] = useState<string | null>(null)
  useEffect(() => {
    webSession().then((w) => setUsername(w?.name ?? null))
  }, [])
  // arm-then-confirm key: `${id}:rotate` or `${id}:remove`
  const [armed, setArmed] = useState<string | null>(null)
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const load = useCallback(() => {
    listEreaders()
      .then((l) => setList(activeEreaders(l)))
      .catch((e) => {
        setList([])
        notify(errText(e))
      })
  }, [])
  useEffect(load, [load, dataVersion])

  useEffect(
    () => () => {
      if (armTimer.current) clearTimeout(armTimer.current)
    },
    [],
  )

  const arm = (key: string, then: () => void) => {
    if (armed === key) {
      setArmed(null)
      if (armTimer.current) clearTimeout(armTimer.current)
      then()
      return
    }
    setArmed(key)
    if (armTimer.current) clearTimeout(armTimer.current)
    armTimer.current = setTimeout(() => setArmed(null), 2500)
  }

  const add = async () => {
    const l = label.trim()
    if (!l || busy) return
    setBusy(true)
    try {
      const r = await createEreader(l, stream)
      setSecret({ label: r.ereader.label, password: r.password })
      setLabel('')
      setStream(false)
      load()
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }

  const rotate = (d: Ereader) =>
    arm(`${d.id}:rotate`, async () => {
      try {
        const r = await rotateEreader(d.id)
        setSecret({ label: d.label, password: r.password })
      } catch (e) {
        notify(errText(e))
      }
    })

  const remove = (d: Ereader) =>
    arm(`${d.id}:remove`, async () => {
      try {
        await deleteEreader(d.id)
        notify(`removed “${d.label}”`, 'ok')
        load()
      } catch (e) {
        notify(errText(e))
      }
    })

  return (
    <div className={embedded ? 'settings-section' : 'queue'}>
      {!embedded && <h1 className="queue-title">e-readers</h1>}

      <div className="card">
        <div className="card-head">
          <span>add a device</span>
          <span className="meta">it signs in with the password you get next</span>
        </div>
        <div className="profile-name-row">
          <input
            className="join-input profile-name"
            value={label}
            maxLength={EREADER_LABEL_MAX}
            placeholder="device name — “kobo”, “boox”…"
            onChange={(e) => setLabel(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && add()}
          />
          <label
            className="meta ereader-stream"
            title="send book files through Taisce instead of redirecting the device to S3 (for readers that cannot follow the redirect)"
          >
            <input type="checkbox" checked={stream} onChange={(e) => setStream(e.target.checked)} />
            stream downloads
          </label>
          <button className="accept" disabled={!label.trim() || busy} onClick={add}>
            {busy ? 'adding…' : 'add'}
          </button>
        </div>
      </div>

      {secret && <SetupCard label={secret.label} password={secret.password} username={username} onDone={() => setSecret(null)} />}

      {list !== null && list.length === 0 && <div className="palette-empty">no devices yet — add one above</div>}
      {list !== null && list.length > 0 && (
        <div className="card">
          <div className="card-head">
            <span>devices</span>
          </div>
          {list.map((d) => (
            <div key={d.id} className="ereader-row">
              <span className="ereader-label">{d.label}</span>
              <span className="meta">
                added {relTime(d.created_at)}
                {d.last_used_at ? ` · last used ${relTime(d.last_used_at)}` : ' · never used'}
                {d.stream_downloads ? ' · streams downloads' : ''}
              </span>
              <span className="ereader-actions">
                <button
                  className={`chip ${armed === `${d.id}:rotate` ? 'armed' : ''}`}
                  title="a fresh password; the old one stops working"
                  onClick={() => rotate(d)}
                >
                  {armed === `${d.id}:rotate` ? 'rotate?' : 'rotate…'}
                </button>
                <button className={`chip ${armed === `${d.id}:remove` ? 'armed' : ''}`} onClick={() => remove(d)}>
                  {armed === `${d.id}:remove` ? 'remove?' : 'remove…'}
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

/** The show-once card: the password plus everything else a device needs to
 * be set up, each with a copy button. */
function SetupCard({
  label,
  password,
  username,
  onDone,
}: {
  label: string
  password: string
  username: string | null
  onDone: () => void
}) {
  const setup = useMemo(() => deviceSetup(window.location.origin, username, password), [username, password])
  const rows: { name: string; value: string | null; hint?: string }[] = [
    { name: 'username', value: setup.username, hint: 'your Taisce name' },
    { name: 'password', value: password },
    { name: 'OPDS catalog', value: setup.opds, hint: 'with the username and password above' },
    { name: 'OPDS, no login', value: setup.opdsPath, hint: 'for readers without a login field: the password is in the URL' },
    { name: 'progress sync (kosync)', value: setup.kosync, hint: 'KOReader: Progress sync → custom server' },
  ]
  return (
    <div className="card yellow ereader-setup" role="region" aria-label={`set up ${label}`}>
      <div className="card-head">
        <span>set up {label}</span>
      </div>
      <div className="ereader-warn">
        You won’t see this password again. Copy what the device needs now — if it is lost, Rotate makes a new one.
      </div>
      {rows.map((r) => (
        <div key={r.name} className="profile-kv ereader-setup-row">
          <span className="meta">{r.name}</span>
          <span className="mono ereader-pw">{r.value ?? '—'}</span>
          {r.value ? <CopyButton what={r.name} value={r.value} /> : <span />}
          {r.hint && <span className="meta dim ereader-setup-hint">{r.hint}</span>}
        </div>
      ))}
      <div className="actions">
        <button className="chip" onClick={onDone}>
          done — I’ve saved it
        </button>
      </div>
    </div>
  )
}

function CopyButton({ what, value }: { what: string; value: string }) {
  return (
    <button
      className="chip"
      aria-label={`copy ${what}`}
      onClick={() =>
        copyText(value)
          .then(() => notify(`copied ${what}`, 'ok'))
          .catch((e) => notify(errText(e)))
      }
    >
      copy
    </button>
  )
}
