// Share links (SERVER mode): publish a read-only snapshot of this doc at a
// public URL, copy it, revoke it. The snapshot is taken from what is on
// screen (diagrams as their rendered SVGs).

import { useCallback, useEffect, useState } from 'react'
import { errText, notify } from './Notice'
import { copyText } from './Profile'
import { buildSnapshot, createShare, isLive, listShares, renderedDiagrams, revokeShare, type Share } from './share'
import { api } from './types'

const EXPIRY: { label: string; days: number | null }[] = [
  { label: '1 day', days: 1 },
  { label: '7 days', days: 7 },
  { label: '30 days', days: 30 },
  { label: 'never', days: null },
]

export default function SharePanel({ docId, title, onClose }: { docId: string; title: string; onClose: () => void }) {
  const [shares, setShares] = useState<Share[]>([])
  const [days, setDays] = useState<number | null>(7)
  const [busy, setBusy] = useState(false)
  const [confirming, setConfirming] = useState<string | null>(null)

  const load = useCallback(() => {
    listShares(docId).then(setShares).catch((e) => notify(errText(e)))
  }, [docId])
  useEffect(load, [load])

  const create = async () => {
    setBusy(true)
    try {
      const { markdown } = await api<{ markdown: string }>(`/api/doc/${docId}/markdown`)
      const s = await createShare(docId, buildSnapshot(title, markdown, renderedDiagrams()), days)
      await copyText(s.url).catch(() => {})
      notify('link made and copied', 'ok')
      load()
    } catch (e) {
      notify(errText(e))
    } finally {
      setBusy(false)
    }
  }

  const revoke = async (id: string) => {
    setConfirming(null)
    try {
      await revokeShare(id)
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  const live = shares.filter((s) => isLive(s))
  return (
    <aside className="panel share-panel">
      <div className="panel-head">
        <span>share link</span>
        <button onClick={onClose}>esc</button>
      </div>
      <p className="meta">
        Anyone with the link sees a read-only snapshot of this doc as it is now, and can comment. Diagrams go as
        they are drawn on screen.
      </p>
      <div className="share-new">
        <label className="meta">
          expires{' '}
          <select value={days ?? ''} onChange={(e) => setDays(e.target.value ? Number(e.target.value) : null)}>
            {EXPIRY.map((x) => (
              <option key={x.label} value={x.days ?? ''}>
                {x.label}
              </option>
            ))}
          </select>
        </label>
        <button className="chip on" disabled={busy} onClick={create}>
          {busy ? 'making…' : 'Make link'}
        </button>
      </div>
      {live.length === 0 && <div className="meta share-empty">no live links</div>}
      {live.map((s) => (
        <div key={s.id} className="share-row">
          <div className="share-url" title={s.url}>
            {s.url}
          </div>
          <div className="meta">
            {s.expires_at ? `expires ${new Date(s.expires_at).toLocaleDateString()}` : 'no expiry'} · {s.views} view
            {s.views === 1 ? '' : 's'} · {s.comment_count} comment{s.comment_count === 1 ? '' : 's'}
          </div>
          <div className="share-actions">
            <button className="chip" onClick={() => copyText(s.url).then(() => notify('copied', 'ok'))}>
              copy
            </button>
            {confirming === s.id ? (
              <>
                <button className="chip danger" onClick={() => revoke(s.id)}>
                  revoke for good
                </button>
                <button className="chip" onClick={() => setConfirming(null)}>
                  keep
                </button>
              </>
            ) : (
              <button className="chip" onClick={() => setConfirming(s.id)}>
                revoke…
              </button>
            )}
          </div>
        </div>
      ))}
    </aside>
  )
}
