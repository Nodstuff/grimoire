// Upload tokens (a Settings section, SERVER mode only): book upload tokens
// for scripts and integrations (a Readarr/Chaptarr custom script, a cron
// job). A token opens POST /api/books and nothing else. Add one and it shows
// exactly ONCE in a card with a ready-to-paste curl line; remove sits behind
// arm-then-confirm (window.confirm is a no-op in the Tauri webview). Removed
// tokens (revoked_at set) are hidden.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { errText, notify } from './Notice'
import { relTime } from './time'
import { CopyButton, EREADER_LABEL_MAX } from './Ereaders'
import { UploadToken, activeUploadTokens, createUploadToken, deleteUploadToken, listUploadTokens, uploadCurl } from './books'

export default function UploadTokens({ dataVersion }: { dataVersion: number }) {
  const [list, setList] = useState<UploadToken[] | null>(null)
  const [label, setLabel] = useState('')
  const [busy, setBusy] = useState(false)
  // the one-time token reveal
  const [secret, setSecret] = useState<{ label: string; token: string } | null>(null)
  const [armed, setArmed] = useState<string | null>(null)
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const load = useCallback(() => {
    listUploadTokens()
      .then((l) => setList(activeUploadTokens(l)))
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

  const remove = (t: UploadToken) => {
    if (armed === t.id) {
      setArmed(null)
      if (armTimer.current) clearTimeout(armTimer.current)
      deleteUploadToken(t.id)
        .then(() => {
          notify(`removed “${t.label}”`, 'ok')
          load()
        })
        .catch((e) => notify(errText(e)))
      return
    }
    setArmed(t.id)
    if (armTimer.current) clearTimeout(armTimer.current)
    armTimer.current = setTimeout(() => setArmed(null), 2500)
  }

  return (
    <div className="settings-section">
      <div className="card">
        <div className="card-head">
          <span>add an upload token</span>
          <span className="meta">for a script that sends books here: it can upload, nothing else</span>
        </div>
        <div className="profile-name-row">
          <input
            className="join-input profile-name"
            value={label}
            maxLength={EREADER_LABEL_MAX}
            placeholder="what uses it — “chaptarr”, “nas cron”…"
            onChange={(e) => setLabel(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && add()}
          />
          <button className="accept" disabled={!label.trim() || busy} onClick={add}>
            {busy ? 'adding…' : 'add'}
          </button>
        </div>
      </div>

      {secret && <TokenCard label={secret.label} token={secret.token} onDone={() => setSecret(null)} />}

      {list !== null && list.length === 0 && <div className="palette-empty">no upload tokens yet — add one above</div>}
      {list !== null && list.length > 0 && (
        <div className="card">
          <div className="card-head">
            <span>upload tokens</span>
          </div>
          {list.map((t) => (
            <div key={t.id} className="ereader-row">
              <span className="ereader-label">{t.label}</span>
              <span className="meta">
                added {relTime(t.created_at)}
                {t.last_used_at ? ` · last used ${relTime(t.last_used_at)}` : ' · never used'}
              </span>
              <span className="ereader-actions">
                <button className={`chip ${armed === t.id ? 'armed' : ''}`} onClick={() => remove(t)}>
                  {armed === t.id ? 'remove?' : 'remove…'}
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

/** The show-once card: the token and a ready-to-paste upload command. */
export function TokenCard({ label, token, onDone }: { label: string; token: string; onDone: () => void }) {
  const curl = useMemo(() => uploadCurl(window.location.origin, token), [token])
  const rows: { name: string; value: string; hint?: string }[] = [
    { name: 'token', value: token },
    { name: 'upload command', value: curl, hint: 'set $path to an .epub; the reply lists each file’s result' },
  ]
  return (
    <div className="card yellow ereader-setup" role="region" aria-label={`token for ${label}`}>
      <div className="card-head">
        <span>token for {label}</span>
      </div>
      <div className="ereader-warn">You won’t see this token again. Copy it now — if it is lost, remove it and add a new one.</div>
      {rows.map((r) => (
        <div key={r.name} className="profile-kv ereader-setup-row">
          <span className="meta">{r.name}</span>
          <span className="mono ereader-pw">{r.value}</span>
          <CopyButton what={r.name} value={r.value} />
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
