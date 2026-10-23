// Settings › Upload tokens (SERVER mode only): book upload tokens for
// scripts and integrations (a Readarr/Chaptarr custom script, a cron job).
// A token opens POST /api/books and nothing else. Add one and it shows
// exactly ONCE in the show-once card with a ready-to-paste curl line;
// Remove sits behind an inline two-step confirm (window.confirm is a no-op
// in the Tauri webview). Removed tokens (revoked_at set) are hidden. Not on
// the canvas: it follows the E-readers section's shapes.

import { useCallback, useEffect, useMemo, useState } from 'react'
import { errText, notify } from './Notice'
import { relTime } from './time'
import { EREADER_LABEL_MAX } from './Ereaders'
import { UploadToken, activeUploadTokens, createUploadToken, deleteUploadToken, listUploadTokens, uploadCurl } from './books'
import { Button, ConfirmButton, Icon } from './lapis'
import { Row, RowText, SecretCard, SettingsCard } from './screens/settingsParts'

export default function UploadTokens({ dataVersion }: { dataVersion: number }) {
  const [list, setList] = useState<UploadToken[] | null>(null)
  const [label, setLabel] = useState('')
  const [busy, setBusy] = useState(false)
  // the one-time token reveal
  const [secret, setSecret] = useState<{ label: string; token: string } | null>(null)

  const load = useCallback(() => {
    listUploadTokens()
      .then((l) => setList(activeUploadTokens(l)))
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
      const r = await createUploadToken(l)
      setSecret({ label: r.upload_token.label, token: r.token })
      setLabel('')
      load()
    } catch (e) {
      notify(errText(e))
    }
    setBusy(false)
  }

  const remove = (t: UploadToken) =>
    deleteUploadToken(t.id)
      .then(() => {
        notify(`removed “${t.label}”`, 'ok')
        load()
      })
      .catch((e) => notify(errText(e)))

  return (
    <SettingsCard id="uploads" title="Upload tokens" sub="For a script that sends books here (a Chaptarr custom script, a cron job): a token can upload EPUBs, nothing else.">
      {secret && <TokenCard label={secret.label} token={secret.token} onDone={() => setSecret(null)} />}
      <div>
        {list !== null && list.length === 0 && (
          <Row first>
            <span className="muted">No upload tokens yet.</span>
          </Row>
        )}
        {list?.map((t, i) => (
          <Row key={t.id} first={i === 0}>
            <Icon name="upload" style={{ color: 'var(--ink2)' }} />
            <RowText sub={`added ${relTime(t.created_at)}${t.last_used_at ? ` · last used ${relTime(t.last_used_at)}` : ' · never used'}`}>
              <b className="st-strong">{t.label}</b>
            </RowText>
            <ConfirmButton confirmLabel="Remove" onConfirm={() => remove(t)}>
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
              <label htmlFor="st-tok-name">Add an upload token</label>
              <input
                id="st-tok-name"
                className="st-inp"
                value={label}
                maxLength={EREADER_LABEL_MAX}
                placeholder="What uses it, e.g. chaptarr"
                onChange={(e) => setLabel(e.target.value)}
              />
            </div>
            <Button type="submit" size="sm" className="st-tall" disabled={!label.trim() || busy}>
              {busy ? 'Adding…' : 'Add token'}
            </Button>
          </form>
        </Row>
      </div>
    </SettingsCard>
  )
}

/** The show-once card: the token and a ready-to-paste upload command. */
export function TokenCard({ label, token, onDone }: { label: string; token: string; onDone: () => void }) {
  const curl = useMemo(() => uploadCurl(window.location.origin, token), [token])
  return (
    <SecretCard
      label={`Token for ${label}`}
      title={`Token for ${label}`}
      warning="You won’t see this token again"
      onDone={onDone}
      rows={[
        { name: 'Token', value: token, hint: 'if it is lost, remove it and add a new one' },
        { name: 'Upload command', value: curl, hint: 'set $path to an .epub; the reply lists each file’s result' },
      ]}
    />
  )
}
