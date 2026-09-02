// Sharing & contacts (#61): the human surface for federation. Contacts are
// petnames over pubkeys; shares are grants we own; pending joins are redeems
// waiting for an offline owner. Minting an invite lives on the doc itself
// (SharePanel) — this screen is the overview.

import { useCallback, useEffect, useState } from 'react'
import { ActivityItem, api, Contact, Doc, PendingJoin, Share } from './types'
import { errText, notify } from './Notice'
import { TrustControl } from './SharePanel'
import { trustLabel } from './trust'
import { EventsResponse, LiveEvent, mergeActivity } from './live'

function when(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function fingerprint(pubkey: string): string {
  return (pubkey.slice(0, 16).match(/.{1,4}/g) ?? []).join(' ')
}

export default function Sharing({
  docs,
  dataVersion,
  onOpenDoc,
  prefillLink,
  onPrefillConsumed,
}: {
  docs: Doc[]
  dataVersion: number
  onOpenDoc: (id: string) => void
  prefillLink?: string | null
  onPrefillConsumed?: () => void
}) {
  const [contacts, setContacts] = useState<Contact[]>([])
  const [shares, setShares] = useState<Share[]>([])
  const [joins, setJoins] = useState<PendingJoin[]>([])
  const [activity, setActivity] = useState<ActivityItem[]>([])
  const [events, setEvents] = useState<LiveEvent[]>([])
  const [joinLink, setJoinLink] = useState(prefillLink ?? '')
  useEffect(() => {
    if (prefillLink) onPrefillConsumed?.()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  const [joinMsg, setJoinMsg] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const load = useCallback(() => {
    api<Contact[]>('/admin/contacts').then(setContacts).catch(() => setContacts([]))
    api<Share[]>('/admin/shares').then(setShares).catch(() => setShares([]))
    api<PendingJoin[]>('/admin/joins').then(setJoins).catch(() => setJoins([]))
    api<ActivityItem[]>('/api/activity?limit=20')
      .then((a) => setActivity(Array.isArray(a) ? a : []))
      .catch(() => setActivity([]))
    // owner nudges received by this instance; older daemon → no route → []
    api<EventsResponse>('/api/events?since=0')
      .then((r) => setEvents(Array.isArray(r?.events) ? r.events : []))
      .catch(() => setEvents([]))
  }, [])
  useEffect(load, [load, dataVersion])
  const rows = mergeActivity(activity, events)

  const join = async () => {
    const link = joinLink.trim()
    if (!link || busy) return
    setBusy(true)
    setJoinMsg('connecting to owner…')
    try {
      const r = await api<{ joined?: { root_title: string; owner_name: string; permission: string; root_doc: string }; queued?: boolean }>(
        '/admin/join',
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ link }),
        },
      )
      if (r.joined) {
        setJoinMsg(`joined “${r.joined.root_title}” from ${r.joined.owner_name} (${r.joined.permission})`)
        setJoinLink('')
        // pull the content right away so the doc isn't an empty shell
        api('/admin/pull', { method: 'POST' }).catch(() => {})
      } else if (r.queued) {
        setJoinMsg('owner unreachable — queued, will keep retrying in the background')
        setJoinLink('')
      }
    } catch (e) {
      setJoinMsg(String(e))
    }
    setBusy(false)
    load()
  }

  const titleOf = (id: string) => docs.find((d) => d.id === id)?.title ?? id.slice(0, 8)

  return (
    <div className="queue">
      <h1 className="queue-title">sharing</h1>

      <div className="card">
        <div className="card-head">
          <span>join a share</span>
        </div>
        <div className="join-row">
          <input
            className="join-input"
            placeholder="paste a grimoire://join/… link"
            value={joinLink}
            onChange={(e) => setJoinLink(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && join()}
          />
          <button className="accept" disabled={busy || !joinLink.trim()} onClick={join}>
            join
          </button>
        </div>
        {joinMsg && <div className="meta">{joinMsg}</div>}
        {joins.map((j) => (
          <div key={j.id} className="pending-join">
            <span className="meta">
              queued join · {j.attempts} attempts
              {j.last_error ? ` · ${j.last_error.slice(0, 80)}` : ''}
            </span>
          </div>
        ))}
      </div>

      <h2 className="runs-title">contacts</h2>
      {contacts.length === 0 && (
        <div className="palette-empty">
          nobody yet — share a doc to mint an invite link, or paste one above
        </div>
      )}
      {contacts.map((c) => (
        <ContactRow key={c.id} c={c} onChanged={load} />
      ))}

      <h2 className="runs-title">shares I own</h2>
      {shares.length === 0 && (
        <div className="palette-empty">
          none — open a doc and use its <b>share</b> chip
        </div>
      )}
      {shares.map((sh) => (
        <div key={sh.id} className={`card ${sh.state === 'revoked' ? 'revoked' : ''}`}>
          <div className="card-head">
            <span className="card-doc" onClick={() => onOpenDoc(sh.root_doc)}>
              {titleOf(sh.root_doc)}
            </span>
            <span className="meta">
              {sh.permission} · {sh.state}
              {sh.permission === 'propose' && sh.state === 'active' && ` · ${trustLabel(sh.trust)}`}
              {sh.contact
                ? ` · with ${contacts.find((c) => c.id === sh.contact)?.petname ?? '?'}`
                : ' · invite not yet redeemed'}
            </span>
            {sh.state !== 'revoked' && (
              <button
                className="decline"
                onClick={() =>
                  api('/admin/shares/revoke', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ id: sh.id }),
                  }).then(load, (e) => notify(errText(e)))
                }
              >
                revoke
              </button>
            )}
          </div>
          {sh.permission === 'propose' && sh.state === 'active' && (
            <TrustControl shareId={sh.id} trust={sh.trust} onChanged={load} />
          )}
        </div>
      ))}

      <h2 className="runs-title">recent activity</h2>
      <div className="meta activity-blurb">
        edits maintainers applied directly to your docs, and owners going live on or adding
        to docs shared with you — you were notified as they landed
      </div>
      {rows.length === 0 && <div className="palette-empty">nothing yet</div>}
      {rows.length > 0 && (
        <div className="card activity-list">
          {rows.map((a) => (
            <div key={a.key} className="activity-row">
              <span className="who remote">{a.who}</span>
              <span className="activity-verb">{a.verb}</span>
              <span className="card-doc" onClick={() => onOpenDoc(a.doc_id)}>
                {a.doc_title}
              </span>
              <span className="meta activity-when">{when(a.at)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

function ContactRow({ c, onChanged }: { c: Contact; onChanged: () => void }) {
  const [editing, setEditing] = useState(false)
  const [name, setName] = useState(c.petname)
  const [armed, setArmed] = useState(false)

  const rename = async () => {
    setEditing(false)
    const petname = name.trim()
    if (!petname || petname === c.petname) return
    await api('/admin/contacts/rename', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: c.id, petname }),
    }).catch((e) => notify(String(e)))
    onChanged()
  }

  return (
    <div className={`card contact ${c.revoked ? 'revoked' : ''}`}>
      <div className="card-head">
        {editing ? (
          <input
            className="petname-edit"
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') rename()
              if (e.key === 'Escape') setEditing(false)
            }}
            onBlur={rename}
          />
        ) : (
          <span
            className="contact-name"
            title="click to rename"
            onClick={() => {
              setName(c.petname)
              setEditing(true)
            }}
          >
            {c.petname}
          </span>
        )}
        <span className="meta mono" title={c.pubkey}>
          {fingerprint(c.pubkey)}
        </span>
        {c.revoked && <span className="verdict v-red">revoked</span>}
        {!c.revoked && (
          <span className="gardener-actions">
            <button
              className={`chip ${c.verified ? 'on' : ''}`}
              title="mark verified after checking fingerprints out-of-band"
              onClick={() =>
                api('/admin/contacts/verify', {
                  method: 'POST',
                  headers: { 'Content-Type': 'application/json' },
                  body: JSON.stringify({ id: c.id, verified: !c.verified }),
                }).then(onChanged, (e) => notify(String(e)))
              }
            >
              {c.verified ? '✓ verified' : 'verify'}
            </button>
            <button
              className={`decline ${armed ? 'armed' : ''}`}
              onClick={() => {
                if (!armed) {
                  setArmed(true)
                  setTimeout(() => setArmed(false), 2500)
                  return
                }
                api('/admin/contacts/revoke', {
                  method: 'POST',
                  headers: { 'Content-Type': 'application/json' },
                  body: JSON.stringify({ id: c.id }),
                }).then(onChanged, (e) => notify(String(e)))
              }}
            >
              {armed ? 'sure?' : 'revoke'}
            </button>
          </span>
        )}
      </div>
    </div>
  )
}
