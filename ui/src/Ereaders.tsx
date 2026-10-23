// E-readers (view kind 'ereaders', a Settings surface): the devices that
// pull from the library. Add a device and its password shows exactly ONCE
// in a reveal card; rotate (a fresh password, shown once) and remove sit
// behind arm-then-confirm. stream_downloads is display-only — the contract
// has no PATCH route for it, and the row says so.

import { useCallback, useEffect, useRef, useState } from 'react'
import { errText, notify } from './Notice'
import { copyText } from './Profile'
import { relTime } from './time'
import { Ereader, createEreader, deleteEreader, listEreaders, rotateEreader } from './books'

export default function Ereaders({ dataVersion }: { dataVersion: number }) {
  const [list, setList] = useState<Ereader[] | null>(null)
  const [label, setLabel] = useState('')
  const [stream, setStream] = useState(false)
  const [busy, setBusy] = useState(false)
  // the one-time password reveal: set by create and by rotate
  const [secret, setSecret] = useState<{ label: string; password: string } | null>(null)
  // arm-then-confirm key: `${id}:rotate` or `${id}:remove`
  const [armed, setArmed] = useState<string | null>(null)
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const load = useCallback(() => {
    listEreaders()
      .then(setList)
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
    <div className="queue">
      <h1 className="queue-title">e-readers</h1>

      <div className="card">
        <div className="card-head">
          <span>add a device</span>
          <span className="meta">it signs in with the password you get next</span>
        </div>
        <div className="profile-name-row">
          <input
            className="join-input profile-name"
            value={label}
            maxLength={64}
            placeholder="device name — “kobo”, “boox”…"
            onChange={(e) => setLabel(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && add()}
          />
          <label className="meta ereader-stream" title="download on read instead of syncing the whole library">
            <input type="checkbox" checked={stream} onChange={(e) => setStream(e.target.checked)} />
            stream downloads
          </label>
          <button className="accept" disabled={!label.trim() || busy} onClick={add}>
            {busy ? 'adding…' : 'add'}
          </button>
        </div>
      </div>

      {secret && (
        <div className="card yellow">
          <div className="card-head">
            <span>password for {secret.label}</span>
            <span className="meta">shown once — save it now</span>
          </div>
          <div className="profile-name-row">
            <span className="mono ereader-pw">{secret.password}</span>
            <button
              className="chip"
              onClick={() =>
                copyText(secret.password)
                  .then(() => notify('copied', 'ok'))
                  .catch((e) => notify(errText(e)))
              }
            >
              copy
            </button>
            <button className="chip" onClick={() => setSecret(null)}>
              saved it
            </button>
          </div>
        </div>
      )}

      {list !== null && list.length === 0 && <div className="palette-empty">no devices yet — add one above</div>}
      {list !== null && list.length > 0 && (
        <div className="card">
          <div className="card-head">
            <span>devices</span>
            <span className="meta">stream_downloads is display-only — there is no edit route for it yet</span>
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
