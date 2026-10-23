// Settings › E-readers (canvas Settings, #ereaders; SERVER mode only): the
// devices that pull from the library. Add a device and its password shows
// exactly ONCE in the setup card with everything a device needs (username,
// OPDS URLs, the kosync server); Rotate password (a fresh one, shown once)
// and Remove sit behind an inline two-step confirm (window.confirm is a
// no-op in the Tauri webview). Removed devices (revoked_at set) are hidden.

import { useCallback, useEffect, useMemo, useState } from 'react'
import { webSession } from './auth'
import { errText, notify } from './Notice'
import { relTime } from './time'
import { Ereader, activeEreaders, createEreader, deleteEreader, deviceSetup, listEreaders, rotateEreader } from './books'
import { DeviceIcon } from './screens/DeviceIcon'
import { Button, ConfirmButton } from './lapis'
import { Row, RowText, SecretCard, SettingsCard } from './screens/settingsParts'

/** The device-name limit the server enforces (opds.rs MAX_LABEL). */
export const EREADER_LABEL_MAX = 60

export { CopyButton } from './screens/settingsParts'

export default function Ereaders({ dataVersion }: { dataVersion: number }) {
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

  const load = useCallback(() => {
    listEreaders()
      .then((l) => setList(activeEreaders(l)))
      .catch((e) => {
        setList([])
        notify(errText(e))
      })
  }, [])
  useEffect(load, [load, dataVersion])

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

  const rotate = async (d: Ereader) => {
    try {
      const r = await rotateEreader(d.id)
      setSecret({ label: d.label, password: r.password })
    } catch (e) {
      notify(errText(e))
    }
  }

  const remove = async (d: Ereader) => {
    try {
      await deleteEreader(d.id)
      notify(`removed “${d.label}”`, 'ok')
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  return (
    <SettingsCard id="ereaders" title="E-readers" sub="Books in your library reach these devices over a catalog feed; reading progress syncs back.">
      {secret && <SetupCard label={secret.label} password={secret.password} username={username} onDone={() => setSecret(null)} />}
      <div>
        {list !== null && list.length === 0 && (
          <Row first>
            <span className="muted">No devices yet. Add one below and it signs in with the password you get next.</span>
          </Row>
        )}
        {list?.map((d, i) => (
          <Row key={d.id} first={i === 0}>
            <DeviceIcon />
            <RowText
              sub={`added ${relTime(d.created_at)}${d.last_used_at ? ` · last used ${relTime(d.last_used_at)}` : ' · never used'}${d.stream_downloads ? ' · streams downloads' : ''}`}
            >
              <b className="st-strong">{d.label}</b>
            </RowText>
            <ConfirmButton variant="secondary" confirmLabel="Rotate: old one stops" onConfirm={() => rotate(d)}>
              Rotate password…
            </ConfirmButton>
            <ConfirmButton confirmLabel="Remove" onConfirm={() => remove(d)}>
              Remove…
            </ConfirmButton>
          </Row>
        ))}
        <Row align="end">
          <form
            className="st-inline-form st-grow"
            onSubmit={(e) => {
              e.preventDefault()
              add()
            }}
          >
            <div className="st-fld st-grow">
              <label htmlFor="st-dev-name">Add a device</label>
              <input
                id="st-dev-name"
                className="st-inp"
                value={label}
                maxLength={EREADER_LABEL_MAX}
                placeholder="Device name, e.g. Kobo Libra"
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
            <label className="st-check" title="send book files through Taisce instead of redirecting the device to S3 (for readers that cannot follow the redirect)">
              <input type="checkbox" checked={stream} onChange={(e) => setStream(e.target.checked)} />
              Stream downloads
            </label>
            <Button type="submit" size="sm" className="st-tall" disabled={!label.trim() || busy}>
              {busy ? 'Adding…' : 'Add e-reader'}
            </Button>
          </form>
        </Row>
      </div>
    </SettingsCard>
  )
}

/** The show-once card: the password plus everything else a device needs to
 * be set up, each with a copy button. */
function SetupCard({ label, password, username, onDone }: { label: string; password: string; username: string | null; onDone: () => void }) {
  const setup = useMemo(() => deviceSetup(window.location.origin, username, password), [username, password])
  return (
    <SecretCard
      label={`Set up ${label}`}
      title={`Set up ${label}`}
      warning="You won’t see this password again"
      onDone={onDone}
      rows={[
        { name: 'Username', value: setup.username, hint: 'your Taisce name' },
        { name: 'Password', value: password, hint: 'if it is lost, Rotate password makes a new one' },
        { name: 'Catalog (OPDS)', value: setup.opds, hint: 'with the username and password above' },
        { name: 'Catalog, password in URL', value: setup.opdsPath, hint: 'for readers without a login field' },
        { name: 'Progress sync server', value: setup.kosync, hint: 'KOReader: Progress sync → custom server' },
      ]}
    />
  )
}
