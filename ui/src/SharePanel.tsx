// Share links (SERVER mode): publish a read-only snapshot of this doc at a
// public URL, copy it, revoke it. The snapshot is taken from what is on
// screen (diagrams as their rendered SVGs). A doc with docs under it can go
// with them, as one link: each sub-page's diagrams are drawn here first.

import { useCallback, useEffect, useState } from 'react'
import { errText, notify } from './Notice'
import { copyText } from './Profile'
import { buildSnapshot, buildTreeSnapshot, createShare, isLive, listShares, renderedDiagrams, revokeShare, subPages, type Share } from './share'
import { api, type Doc } from './types'
import { Button, useTheme } from './lapis'
import { drawDiagram } from './editor/CodeBlockView'

const EXPIRY: { label: string; days: number | null }[] = [
  { label: '1 day', days: 1 },
  { label: '7 days', days: 7 },
  { label: '30 days', days: 30 },
  { label: 'never', days: null },
]

export default function SharePanel({ docId, title }: { docId: string; title: string; onClose?: () => void }) {
  const [shares, setShares] = useState<Share[]>([])
  const [days, setDays] = useState<number | null>(7)
  const [busy, setBusy] = useState(false)
  const [confirming, setConfirming] = useState<string | null>(null)
  // the docs under this one (0 = nothing to offer), and whether they go too
  const [under, setUnder] = useState(0)
  const [withPages, setWithPages] = useState(true)
  const { theme } = useTheme()

  const load = useCallback(() => {
    listShares(docId).then(setShares).catch((e) => notify(errText(e)))
  }, [docId])
  useEffect(load, [load])
  useEffect(() => {
    let stale = false
    api<Doc[]>('/api/docs')
      .then((docs) => !stale && setUnder(subPages(docs, docId).length))
      .catch(() => {})
    return () => {
      stale = true
    }
  }, [docId])

  const create = async () => {
    setBusy(true)
    try {
      const { markdown } = await api<{ markdown: string }>(`/api/doc/${docId}/markdown`)
      let snapshot = buildSnapshot(title, markdown, renderedDiagrams())
      let problems: string[] = []
      if (withPages && under > 0) {
        // the tree as it is now, each page's markdown from the server
        const pages = []
        for (const doc of subPages(await api<Doc[]>('/api/docs'), docId)) {
          pages.push({ doc, markdown: (await api<{ markdown: string }>(`/api/doc/${doc.id}/markdown`)).markdown })
        }
        const built = await buildTreeSnapshot(snapshot, docId, pages, (lang, code) => drawDiagram(lang, code, theme === 'dark'))
        snapshot = built.snapshot
        problems = built.problems
      }
      const s = await createShare(docId, snapshot, days)
      await copyText(s.url).catch(() => {})
      notify(s.pages ? `link to this doc and ${s.pages} sub-page${s.pages === 1 ? '' : 's'} made and copied` : 'link made and copied', 'ok')
      if (problems.length) notify(problems.length === 1 ? problems[0] : `${problems.length} diagrams stayed as code: ${problems[0]}`)
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
    <div className="sh-pane">
      <p className="sh-lead">
        Anyone with the link sees a read-only snapshot of this doc{withPages && under > 0 ? ' and its sub-pages' : ''} as{' '}
        {withPages && under > 0 ? 'they are' : 'it is'} now, and can comment. Diagrams go as they are drawn on screen.
      </p>
      {under > 0 && (
        <label className="sh-pages">
          <input type="checkbox" checked={withPages} onChange={(e) => setWithPages(e.target.checked)} />
          <span>
            Include the {under} sub-page{under === 1 ? '' : 's'} under this doc
          </span>
        </label>
      )}
      <div className="sh-new">
        <label className="sh-expiry">
          <span>Expires</span>
          <select value={days ?? ''} onChange={(e) => setDays(e.target.value ? Number(e.target.value) : null)}>
            {EXPIRY.map((x) => (
              <option key={x.label} value={x.days ?? ''}>
                {x.label}
              </option>
            ))}
          </select>
        </label>
        <Button variant="primary" size="sm" icon="link" disabled={busy} onClick={create}>
          {busy ? 'Making…' : 'Make link'}
        </Button>
      </div>
      <div className="sh-sec">Live links</div>
      {live.length === 0 && <div className="sh-empty">No live links</div>}
      {live.map((s) => (
        <div key={s.id} className="sh-row">
          <div className="sh-url" title={s.url}>
            {s.url}
          </div>
          <div className="sh-meta">
            {s.expires_at ? `expires ${new Date(s.expires_at).toLocaleDateString()}` : 'no expiry'} · {s.views} view
            {s.views === 1 ? '' : 's'} · {s.comment_count} comment{s.comment_count === 1 ? '' : 's'}
            {s.pages ? ` · with ${s.pages} sub-page${s.pages === 1 ? '' : 's'}` : ''}
          </div>
          <div className="sh-actions">
            <Button size="sm" icon="copy" onClick={() => copyText(s.url).then(() => notify('copied', 'ok'))}>
              Copy
            </Button>
            {confirming === s.id ? (
              <>
                <Button size="sm" variant="primary" className="lp-btn-danger-fill" onClick={() => revoke(s.id)}>
                  Revoke for good
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setConfirming(null)}>
                  Keep
                </Button>
              </>
            ) : (
              <Button size="sm" variant="danger" onClick={() => setConfirming(s.id)}>
                Revoke…
              </Button>
            )}
          </div>
        </div>
      ))}
    </div>
  )
}
