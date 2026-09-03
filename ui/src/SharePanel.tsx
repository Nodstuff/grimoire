// Per-doc share panel (#61 / #57): who can see this subtree, and minting a
// new one-time invite link (+ QR) for someone new. The link IS the secret —
// the UI says so out loud.

import { useEffect, useState } from 'react'
import QRCode from 'qrcode'
import { api, Contact, Doc, DocFederation, HubRow, Share, ShareTrust } from './types'
import { errText, notify } from './Notice'
import { TRUST_TIERS, trustHint } from './trust'
import { shortFingerprint } from './shares'

/** Three-state trust control for an active propose share. */
export function TrustControl({
  shareId,
  trust,
  onChanged,
}: {
  shareId: string
  trust: ShareTrust | string | null | undefined
  onChanged: () => void
}) {
  const [busy, setBusy] = useState(false)
  const set = (next: ShareTrust) => {
    if (busy || next === trust) return
    setBusy(true)
    api('/admin/shares/trust', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: shareId, trust: next }),
    })
      .then(onChanged, (e) => notify(errText(e)))
      .finally(() => setBusy(false))
  }
  return (
    <div className="trust-control">
      <div className="trust-seg" role="radiogroup" aria-label="trust">
        {TRUST_TIERS.map((t) => (
          <button
            key={t.value}
            role="radio"
            aria-checked={trust === t.value}
            className={`trust-opt ${t.value} ${trust === t.value ? 'on' : ''}`}
            disabled={busy}
            title={t.hint}
            onClick={() => set(t.value)}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div className="meta trust-hint">{trustHint(trust)}</div>
    </div>
  )
}

/** A freshly minted invite: the link (click to copy), a QR, and the warning
 * that the link is the secret. Shared by the per-doc panel and the Shares
 * page's re-invite action. */
export function InviteLink({ link }: { link: string }) {
  const [qr, setQr] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    QRCode.toDataURL(link, { margin: 1, width: 220, color: { dark: '#d6d6dd', light: '#17171d' } })
      .then(setQr)
      .catch(() => setQr(null))
  }, [link])

  const copy = () => {
    navigator.clipboard.writeText(link).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1800)
    })
  }

  return (
    <div className="share-link">
      <div className="link-box mono" onClick={copy} title="click to copy">
        {link.length > 140 ? `${link.slice(0, 52)}…` : link}
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
  )
}

/** POST /admin/shares — mint a one-time invite for a subtree. */
export async function mintInvite(rootDoc: string, permission: 'view' | 'propose'): Promise<string> {
  const r = await api<{ share: Share; link: string }>('/admin/shares', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ root_doc: rootDoc, permission }),
  })
  return r.link
}

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
  const [error, setError] = useState<string | null>(null)
  // invites v2: share straight with a contact — no link
  const [contacts, setContacts] = useState<Contact[]>([])
  const [pick, setPick] = useState<string>('')
  const [sending, setSending] = useState(false)
  const [sent, setSent] = useState<string | null>(null)
  const [fallback, setFallback] = useState<{ to: string; link: string; reason: string } | null>(null)
  useEffect(() => {
    api<Contact[]>('/admin/contacts')
      .then((cs) => setContacts(Array.isArray(cs) ? cs.filter((c) => !c.revoked) : []))
      .catch(() => setContacts([]))
  }, [])

  const offer = async () => {
    if (!pick || sending) return
    setSending(true)
    setError(null)
    setSent(null)
    setFallback(null)
    try {
      const r = await api<{ delivered: boolean; to: string; link?: string; reason?: string }>('/admin/shares/offer', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ root_doc: doc.id, permission, contact_id: pick }),
      })
      if (r.delivered) {
        setSent(`request sent to ${r.to} — it shows under their Share requests until they answer`)
      } else if (r.link) {
        setFallback({ to: r.to, link: r.link, reason: r.reason ?? 'they are offline' })
      }
      onChanged()
    } catch (e) {
      setError(errText(e))
    } finally {
      setSending(false)
    }
  }

  const mint = async () => {
    setError(null)
    try {
      setLink(await mintInvite(doc.id, permission))
      onChanged()
    } catch (e) {
      setError(errText(e))
    }
  }

  // shares rooted here vs inherited from an ancestor
  const rootedHere = fed.shares.filter((s) => s.root_doc === doc.id)
  const inherited = fed.shares.filter((s) => s.root_doc !== doc.id)
  // hubs I am an active member of: publishing = a propose share offered to the hub
  const hubs = contacts.filter((c) => c.is_hub && (c.membership ?? 'active') === 'active')
  const [publishing, setPublishing] = useState<string | null>(null)
  const publish = async (hub: Contact) => {
    if (publishing) return
    setPublishing(hub.id)
    setError(null)
    try {
      const r = await api<{ delivered: boolean; to: string; reason?: string }>('/admin/shares/offer', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ root_doc: doc.id, permission: 'propose', contact_id: hub.id }),
      })
      if (r.delivered) notify(`published “${doc.title}” to ${r.to} — members see it on their next sync`, 'ok')
      else notify(`${r.to} is offline — publishing will complete when it is back (${r.reason ?? 'unreachable'})`)
      onChanged()
    } catch (e) {
      setError(errText(e))
    } finally {
      setPublishing(null)
    }
  }
  const publishedTo = (hub: Contact) =>
    fed.shares.find((s) => s.to_hub && s.state !== 'revoked' && s.petname === hub.petname)
  // hub (slice 2): hand the folder over. Armed confirm (two clicks), never window.confirm.
  const [hubRows, setHubRows] = useState<HubRow[]>([])
  const [transferArmed, setTransferArmed] = useState<string | null>(null)
  const [transferring, setTransferring] = useState<string | null>(null)
  useEffect(() => {
    if (hubs.length === 0) return
    api<HubRow[]>('/admin/hubs')
      .then((rows) => setHubRows(Array.isArray(rows) ? rows : []))
      .catch(() => setHubRows([]))
  }, [hubs.length])
  const transferOffered = (hub: Contact) =>
    hubRows
      .find((r) => r.contact_id === hub.id)
      ?.transfers?.find((t) => t.root_doc === doc.id && t.state === 'offered')
  const transfer = async (hub: Contact) => {
    if (transferring) return
    if (transferArmed !== hub.id) {
      setTransferArmed(hub.id)
      setTimeout(() => setTransferArmed((a) => (a === hub.id ? null : a)), 6000)
      return
    }
    setTransferArmed(null)
    setTransferring(hub.id)
    setError(null)
    try {
      const r = await api<{ doc_count: number }>('/admin/hubs/transfer', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ hub: hub.id, root_doc: doc.id }),
      })
      notify(`offered “${doc.title}” (${r.doc_count} doc${r.doc_count === 1 ? '' : 's'}) to ${hub.petname} — an admin has to accept before anything changes`, 'ok')
      const rows = await api<HubRow[]>('/admin/hubs').catch(() => [] as HubRow[])
      setHubRows(Array.isArray(rows) ? rows : [])
      onChanged()
    } catch (e) {
      setError(errText(e))
    } finally {
      setTransferring(null)
    }
  }

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
            <div key={s.id} className="share-row-block">
              <div className="share-row">
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
                      }).then(onChanged, (e) => notify(errText(e)))
                    }
                  >
                    revoke
                  </button>
                )}
              </div>
              {s.state === 'active' && s.permission === 'propose' && (
                <TrustControl shareId={s.id} trust={s.trust} onChanged={onChanged} />
              )}
            </div>
          ))}
        </div>
      )}

      {hubs.length > 0 && (
        <div className="share-hubs">
          {hubs.map((h) => {
            const already = publishedTo(h)
            return (
              <div key={h.id} className="share-row">
                <span className="hub-mark">⌂</span>
                {already ? (
                  <span className="meta">
                    {already.root_doc === doc.id ? `published to ${h.petname}` : `published to ${h.petname} via a parent`}
                  </span>
                ) : (
                  <button className="accept" disabled={!!publishing} onClick={() => publish(h)}>
                    {publishing === h.id ? 'publishing…' : `Publish to ${h.petname}`}
                  </button>
                )}
              </div>
            )
          })}
          <div className="meta">publishing puts “{doc.title}” and everything under it in the team folder every member sees; you stay the owner</div>
          {hubs.map((h) => {
            const offered = transferOffered(h)
            return (
              <div key={`t-${h.id}`} className="share-transfer">
                {offered ? (
                  <div className="meta">transfer to {h.petname} offered — waiting for an admin to accept</div>
                ) : (
                  <button
                    className={`decline ${transferArmed === h.id ? 'armed' : ''}`}
                    disabled={!!transferring}
                    title={`${h.petname} will own this folder. You keep a read-only copy and can propose edits like anyone.`}
                    onClick={() => transfer(h)}
                  >
                    {transferring === h.id
                      ? 'offering…'
                      : transferArmed === h.id
                        ? `yes, hand “${doc.title}” to ${h.petname}`
                        : `Transfer to ${h.petname}…`}
                  </button>
                )}
                {transferArmed === h.id && (
                  <div className="meta">
                    {h.petname} will own this folder. You keep a read-only copy and can propose edits like anyone.
                    Everything inside must be idle: no live sessions, nothing waiting for review.
                  </div>
                )}
              </div>
            )
          })}
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
        </div>
        {contacts.length > 0 && (
          <div className="share-with">
            <div className="meta">with a contact — they get a request to accept, no link needed</div>
            <div className="share-mint">
              <select value={pick} onChange={(e) => setPick(e.target.value)}>
                <option value="">choose a contact…</option>
                {contacts.filter((c) => !c.is_hub).map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.petname} · {shortFingerprint(c.pubkey).slice(0, 4)}
                  </option>
                ))}
              </select>
              <button className="accept" disabled={!pick || sending} onClick={offer}>
                {sending ? 'sending…' : 'send request'}
              </button>
            </div>
            {sent && <div className="meta ok">{sent}</div>}
            {fallback && (
              <div className="share-fallback">
                <div className="meta">
                  {fallback.to} is offline or unreachable right now — send them this link instead
                  <span className="meta dim"> ({fallback.reason})</span>
                </div>
                <InviteLink link={fallback.link} />
              </div>
            )}
          </div>
        )}
        <div className="share-link-flow">
          <div className="meta">{contacts.length > 0 ? '…or make an invite link for someone new' : 'make an invite link — the first time you share with someone'}</div>
          <button className="chip" onClick={mint}>
            make invite link
          </button>
        </div>
        {error && <div className="meta err">{error}</div>}
        {link && <InviteLink link={link} />}
      </div>
    </aside>
  )
}
