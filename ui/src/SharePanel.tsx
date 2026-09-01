// Per-doc share panel (#61 / #57): who can see this subtree, and minting a
// new one-time invite link (+ QR) for someone new. The link IS the secret —
// the UI says so out loud.

import { useEffect, useState } from 'react'
import QRCode from 'qrcode'
import { api, Doc, DocFederation, Share } from './types'

export default function SharePanel({
  doc,
  fed,
  onChanged,
  onClose,
}: {
  doc: Doc
  fed: DocFederation
  onChanged: () => void
  onClose: () => void
}) {
  const [permission, setPermission] = useState<'view' | 'propose'>('view')
  const [link, setLink] = useState<string | null>(null)
  const [qr, setQr] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!link) {
      setQr(null)
      return
    }
    QRCode.toDataURL(link, { margin: 1, width: 220, color: { dark: '#d6d6dd', light: '#17171d' } })
      .then(setQr)
      .catch(() => setQr(null))
  }, [link])

  const mint = async () => {
    setError(null)
    try {
      const r = await api<{ share: Share; link: string }>('/admin/shares', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ root_doc: doc.id, permission }),
      })
      setLink(r.link)
      onChanged()
    } catch (e) {
      setError(String(e))
    }
  }

  const copy = () => {
    if (!link) return
    navigator.clipboard.writeText(link).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1800)
    })
  }

  // shares rooted here vs inherited from an ancestor
  const rootedHere = fed.shares.filter((s) => s.root_doc === doc.id)
  const inherited = fed.shares.filter((s) => s.root_doc !== doc.id)

  return (
    <aside className="panel">
      <div className="panel-head">
        <span>share</span>
        <button onClick={onClose}>esc</button>
      </div>

      {inherited.length > 0 && (
        <div className="share-inherited">
          ⚠ already visible through a share of an ancestor
          {inherited.map((s) => (
            <div key={s.id} className="meta">
              {s.petname ?? 'invite pending'} · {s.permission} · {s.state}
            </div>
          ))}
        </div>
      )}

      {rootedHere.length > 0 && (
        <div className="share-list">
          <div className="meta">this subtree is shared with</div>
          {rootedHere.map((s) => (
            <div key={s.id} className="share-row">
              <span>{s.petname ?? 'invite pending'}</span>
              <span className="meta">
                {s.permission} · {s.state}
              </span>
              {s.state !== 'revoked' && (
                <button
                  className="decline"
                  onClick={() =>
                    api('/admin/shares/revoke', {
                      method: 'POST',
                      headers: { 'Content-Type': 'application/json' },
                      body: JSON.stringify({ id: s.id }),
                    }).then(onChanged, (e) => alert(String(e)))
                  }
                >
                  revoke
                </button>
              )}
            </div>
          ))}
        </div>
      )}

      <div className="share-new">
        <div className="meta">
          share “{doc.title}” and everything nested under it
        </div>
        <div className="share-mint">
          <select
            value={permission}
            onChange={(e) => setPermission(e.target.value as 'view' | 'propose')}
          >
            <option value="view">view only</option>
            <option value="propose">can propose edits</option>
          </select>
          <button className="accept" onClick={mint}>
            mint invite link
          </button>
        </div>
        {error && <div className="meta err">{error}</div>}
        {link && (
          <div className="share-link">
            <div className="link-box mono" onClick={copy} title="click to copy">
              {link.slice(0, 52)}…
            </div>
            <button className="chip" onClick={copy}>
              {copied ? 'copied ✓' : 'copy link'}
            </button>
            {qr && <img className="share-qr" src={qr} alt="invite QR" />}
            <div className="meta">
              one-time use · expires in 7 days · the link IS the secret — send
              it over a private channel
            </div>
          </div>
        )}
      </div>
    </aside>
  )
}
