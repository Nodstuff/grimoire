// Shares page (#61): the human surface for federation, both directions.
// "Shared by me" = grants we own (GET /admin/shares); "Shared with me" =
// mirrors others granted us (GET /admin/mirrors); pending joins are redeems
// waiting for an offline owner; contacts are petnames over pubkeys. Minting a
// first invite lives on the doc itself (SharePanel) — re-inviting a revoked
// share lives here.

import { useCallback, useEffect, useState } from 'react'
import { ActivityItem, api, Contact, Doc, HubMember, HubMembersResponse, HubRow, HubTransfer, MirrorRow, Neighbour, PendingJoin, Profile, QueueRow, Share, ShareOffer } from './types'
import { describeChange } from './review'
import { errText, notify } from './Notice'
import { InviteLink, mintInvite, TrustControl } from './SharePanel'
import { EventsResponse, LiveEvent, mergeActivity } from './live'
import { groupShares, hubStandingLine, mirrorStatusLine, offerLine, publishedLine, shareTitle, shareWho, shortFingerprint, waitingLine } from './shares'
import { relTime } from './time'
import { refusalHint } from './hints'
import { loadProfile } from './Profile'

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

const post = (path: string, body?: unknown) =>
  api(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body ?? {}),
  })

/** Two-click destructive button: first click arms ("sure?"), second fires;
 * disarms itself after 2.5s. Never window.confirm. */
function ArmedButton({
  label,
  onFire,
  className = 'decline',
  title,
}: {
  label: string
  onFire: () => void
  className?: string
  title?: string
}) {
  const [armed, setArmed] = useState(false)
  return (
    <button
      className={`${className} ${armed ? 'armed' : ''}`}
      title={armed ? 'click again to confirm' : title}
      onClick={() => {
        if (!armed) {
          setArmed(true)
          setTimeout(() => setArmed(false), 2500)
          return
        }
        setArmed(false)
        onFire()
      }}
    >
      {armed ? 'sure?' : label}
    </button>
  )
}

function PermBadge({ p }: { p: string }) {
  return <span className={`badge perm-${p}`}>{p === 'propose' ? 'can propose' : 'view only'}</span>
}

function StateBadge({ s }: { s: string }) {
  return <span className={`badge state-${s}`}>{s}</span>
}

export default function Sharing({
  docs,
  dataVersion,
  onOpenDoc,
  onOpenProfile,
  prefillLink,
  onPrefillConsumed,
}: {
  docs: Doc[]
  dataVersion: number
  onOpenDoc: (id: string) => void
  onOpenProfile?: () => void
  prefillLink?: string | null
  onPrefillConsumed?: () => void
}) {
  const [contacts, setContacts] = useState<Contact[]>([])
  const [shares, setShares] = useState<Share[]>([])
  const [mirrors, setMirrors] = useState<MirrorRow[] | null>(null)
  const [joins, setJoins] = useState<PendingJoin[]>([])
  const [offers, setOffers] = useState<ShareOffer[]>([])
  const [neighbours, setNeighbours] = useState<Neighbour[]>([])
  // hubs I belong to (local rows; members fetched when a card is expanded)
  const [hubs, setHubs] = useState<HubRow[]>([])
  const [offerBusy, setOfferBusy] = useState<string | null>(null)
  const [activity, setActivity] = useState<ActivityItem[]>([])
  const [events, setEvents] = useState<LiveEvent[]>([])
  const [profile, setProfile] = useState<Profile | null>(null)
  const [joinLink, setJoinLink] = useState(prefillLink ?? '')
  useEffect(() => {
    if (prefillLink) onPrefillConsumed?.()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  const [joinMsg, setJoinMsg] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [pulling, setPulling] = useState(false)
  const [showRevoked, setShowRevoked] = useState(false)
  // re-invite result: which share row it belongs to, and the fresh link
  const [reinvite, setReinvite] = useState<{ shareId: string; link: string } | null>(null)
  const [now, setNow] = useState(() => Date.now())

  const load = useCallback(() => {
    setNow(Date.now())
    api<Contact[]>('/admin/contacts').then(setContacts).catch(() => setContacts([]))
    api<Share[]>('/admin/shares').then((s) => setShares(Array.isArray(s) ? s : [])).catch(() => setShares([]))
    // older daemon → no route → null (section hides itself)
    api<MirrorRow[]>('/admin/mirrors').then((m) => setMirrors(Array.isArray(m) ? m : [])).catch(() => setMirrors(null))
    api<PendingJoin[]>('/admin/joins').then((j) => setJoins(Array.isArray(j) ? j : [])).catch(() => setJoins([]))
    // invites v2: requests to join someone's share, and Grimoires nearby
    api<ShareOffer[]>('/admin/offers').then((o) => setOffers(Array.isArray(o) ? o : [])).catch(() => setOffers([]))
    api<Neighbour[]>('/admin/neighbours').then((n) => setNeighbours(Array.isArray(n) ? n : [])).catch(() => setNeighbours([]))
    api<HubRow[]>('/admin/hubs').then((h) => setHubs(Array.isArray(h) ? h : [])).catch(() => setHubs([]))
    api<ActivityItem[]>('/api/activity?limit=20')
      .then((a) => setActivity(Array.isArray(a) ? a : []))
      .catch(() => setActivity([]))
    // owner nudges received by this instance; older daemon → no route → []
    api<EventsResponse>('/api/events?since=0')
      .then((r) => setEvents(Array.isArray(r?.events) ? r.events : []))
      .catch(() => setEvents([]))
    loadProfile().then(setProfile)
  }, [])
  useEffect(load, [load, dataVersion])
  const rows = mergeActivity(activity, events)

  const act = (p: Promise<unknown>, okMsg?: string) =>
    p.then(
      () => {
        if (okMsg) notify(okMsg, 'ok')
        load()
      },
      (e) => notify(errText(e)),
    )

  const join = async () => {
    const link = joinLink.trim()
    if (!link || busy) return
    setBusy(true)
    setJoinMsg('connecting to owner…')
    try {
      const r = await api<{
        joined?: { root_title: string; owner_name: string; permission: string; root_doc: string; is_hub?: boolean; membership?: string }
        docs?: number
        pull_error?: string
        queued?: boolean
        pending?: boolean
      }>(
        '/admin/join',
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ link }),
        },
      )
      if (r.joined && r.pending) {
        setJoinMsg(`asked to join ${r.joined.owner_name} — an admin has to approve you; the team folder arrives as a share request`)
        setJoinLink('')
      } else if (r.joined) {
        // the daemon pulls the tree before answering, so we can say how much arrived
        const got =
          typeof r.docs === 'number'
            ? ` — ${r.docs} doc${r.docs === 1 ? '' : 's'} synced`
            : r.pull_error
              ? ` — first sync pending: ${refusalHint(r.pull_error) ?? r.pull_error}`
              : ''
        setJoinMsg(`joined “${r.joined.root_title}” from ${r.joined.owner_name} (${r.joined.permission})${got}`)
        setJoinLink('')
      } else if (r.queued) {
        setJoinMsg('owner unreachable — queued, will keep retrying in the background')
        setJoinLink('')
      }
    } catch (e) {
      setJoinMsg(errText(e))
    }
    setBusy(false)
    load()
  }

  const pullNow = () => {
    if (pulling) return
    setPulling(true)
    api('/admin/pull', { method: 'POST' })
      .then(() => notify('pulled', 'ok'), (e) => notify(errText(e)))
      .finally(() => {
        setPulling(false)
        load()
      })
  }

  const acceptOffer = async (o: ShareOffer) => {
    if (offerBusy) return
    setOfferBusy(o.id)
    try {
      const r = await api<{ joined?: { root_title: string }; docs?: number; pull_error?: string }>('/admin/offers/accept', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: o.id }),
      })
      const got = typeof r.docs === 'number' ? ` — ${r.docs} doc${r.docs === 1 ? '' : 's'} synced` : ''
      notify(`joined “${o.root_title}” from ${o.from_petname}${got}`, 'ok')
    } catch (e) {
      notify(refusalHint(errText(e)) ?? errText(e))
    } finally {
      setOfferBusy(null)
      load()
    }
  }
  const declineOffer = (o: ShareOffer) =>
    act(post('/admin/offers/decline', { id: o.id }), `declined “${o.root_title}”`)

  const reinviteShare = async (sh: Share) => {
    try {
      const link = await mintInvite(sh.root_doc, sh.permission)
      setReinvite({ shareId: sh.id, link })
      setShowRevoked(true)
      load()
    } catch (e) {
      notify(errText(e))
    }
  }

  const titleOf = (id: string) => docs.find((d) => d.id === id)?.title
  const petnameOf = (id: string) => contacts.find((c) => c.id === id)?.petname
  const groups = groupShares(shares)

  const shareRow = (sh: Share) => (
    <div key={sh.id} className={`card share-card ${sh.state === 'revoked' ? 'revoked' : ''}`}>
      <div className="card-head">
        <span className="card-doc" onClick={() => onOpenDoc(sh.root_doc)}>
          {shareTitle(sh, titleOf)}
        </span>
        {sh.doc_count != null && (
          <span className="meta">{sh.doc_count} doc{sh.doc_count === 1 ? '' : 's'}</span>
        )}
        <span className="meta share-who">
          {sh.state === 'offered' ? 'not yet joined' : `with ${shareWho(sh, petnameOf)}`}
        </span>
        <PermBadge p={sh.permission} />
        <StateBadge s={sh.state} />
        <span className="gardener-actions">
          {sh.state !== 'revoked' && (
            <ArmedButton label="revoke" onFire={() => act(post('/admin/shares/revoke', { id: sh.id }), 'revoked')} />
          )}
          {sh.state === 'revoked' && (
            <>
              <button className="chip" title="mint a fresh invite link for the same subtree" onClick={() => reinviteShare(sh)}>
                re-invite
              </button>
              <ArmedButton
                label="clear"
                title="permanently remove this row and its invites"
                onFire={() => {
                  if (reinvite?.shareId === sh.id) setReinvite(null)
                  act(post('/admin/shares/delete', { id: sh.id }))
                }}
              />
            </>
          )}
        </span>
      </div>
      <div className="meta share-dates">
        created {relTime(sh.created_at, now)}
        {sh.redeemed_at ? ` · joined ${relTime(sh.redeemed_at, now)}` : ''}
      </div>
      {sh.permission === 'propose' && sh.state === 'active' && (
        <TrustControl shareId={sh.id} trust={sh.trust} onChanged={load} />
      )}
      {reinvite?.shareId === sh.id && (
        <div className="reinvite">
          <div className="meta">new invite for this subtree — the old row stays for history</div>
          <InviteLink link={reinvite.link} />
        </div>
      )}
    </div>
  )

  return (
    <div className="queue">
      <div className="shares-head">
        <h1 className="queue-title">shares</h1>
        {profile && (
          <button className="chip name-chip" title="your profile — the name contacts see" onClick={onOpenProfile}>
            {profile.name}
            {!profile.confirmed && <span className="meta"> · unset</span>}
          </button>
        )}
      </div>

      {/* ---- share requests (invites v2): durable, never just a toast ---- */}
      {offers.length > 0 && (
        <>
          <h2 className="runs-title">share requests</h2>
          {offers.map((o) => (
            <div key={o.id} className="card share-card offer-card">
              <div className="card-head">
                <span className="offer-line">{offerLine(o)}</span>
                <span className="meta mono" title={o.from_pubkey}>
                  {shortFingerprint(o.from_pubkey)}
                </span>
                <span className="meta">{relTime(o.created_at, now)}</span>
                <span className="gardener-actions">
                  <button className="accept" disabled={offerBusy === o.id} onClick={() => acceptOffer(o)}>
                    {offerBusy === o.id ? 'joining…' : 'accept'}
                  </button>
                  <button className="decline" disabled={!!offerBusy} onClick={() => declineOffer(o)}>
                    decline
                  </button>
                </span>
              </div>
              <div className="meta">accepting adds their docs under your tree, read-only unless they said you can propose edits</div>
            </div>
          ))}
        </>
      )}

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
        {neighbours.length > 0 && (
          <div className="neighbours">
            <div className="meta">nearby on this network</div>
            <div className="neighbour-list">
              {neighbours.map((n) => (
                <span
                  key={n.pubkey}
                  className={`chip neighbour ${n.contact_id ? 'known' : ''}`}
                  title={n.pubkey}
                >
                  {n.contact_petname ?? n.name ?? 'a Grimoire'} · {shortFingerprint(n.pubkey).slice(0, 4)}
                  {n.contact_id ? (n.blocked ? ' — blocked' : ' — contact') : ' — not a contact yet: ask them to share with you'}
                </span>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* ---- hubs: GET /admin/hubs (members on expand, never polled) ---- */}
      {hubs.length > 0 && (
        <>
          <h2 className="runs-title">hubs</h2>
          {hubs.map((h) => (
            <HubCard key={h.contact_id} hub={h} onOpenDoc={onOpenDoc} onChanged={load} now={now} />
          ))}
        </>
      )}

      {/* ---- shared by me: GET /admin/shares ---- */}
      <h2 className="runs-title">shared by me</h2>
      {shares.length === 0 && (
        <div className="palette-empty">
          none — open a doc and use its <b>share</b> chip
        </div>
      )}
      {groups.active.map(shareRow)}
      {groups.offered.map(shareRow)}
      {groups.revoked.length > 0 && (
        <div className="revoked-bar">
          <button className="chip" onClick={() => setShowRevoked((v) => !v)}>
            {showRevoked ? '▾' : '▸'} {groups.revoked.length} revoked
          </button>
          <ArmedButton
            label="clear all"
            title="permanently remove every revoked share"
            onFire={() => {
              setReinvite(null)
              act(Promise.all(groups.revoked.map((sh) => post('/admin/shares/delete', { id: sh.id }))), 'cleared')
            }}
          />
        </div>
      )}
      {showRevoked && groups.revoked.map(shareRow)}

      {/* ---- shared with me: GET /admin/mirrors ---- */}
      {mirrors !== null && (
        <>
          <div className="section-head">
            <h2 className="runs-title">shared with me</h2>
            {mirrors.length > 0 && (
              <button className="chip" disabled={pulling} onClick={pullNow} title="pull every mirror now">
                {pulling ? 'pulling…' : 'pull now'}
              </button>
            )}
          </div>
          {mirrors.length === 0 && <div className="palette-empty">nothing yet — paste an invite link above</div>}
          {mirrors.map((m) => {
            const st = mirrorStatusLine(m, now)
            return (
              <div key={m.share_id} className={`card share-card ${st.kind === 'failing' ? 'red' : ''}`}>
                <div className="card-head">
                  <span className="card-doc" onClick={() => onOpenDoc(m.root_doc_id)}>
                    {m.root_title}
                  </span>
                  {m.doc_count != null && (
                    <span className="meta">{m.doc_count} doc{m.doc_count === 1 ? '' : 's'}</span>
                  )}
                  <span className="meta share-who" title={m.owner_pubkey}>
                    from {m.owner_petname}
                  </span>
                  <PermBadge p={m.permission} />
                  {m.owner_tended && <span className="badge tended" title="the owner has agents tending this">tended</span>}
                  <span className="gardener-actions">
                    <ArmedButton
                      label="leave"
                      title="remove this share and its mirrored docs from this install"
                      onFire={() => act(post('/admin/mirrors/leave', { share_id: m.share_id }), `left “${m.root_title}”`)}
                    />
                  </span>
                </div>
                <div className={`meta sync-line ${st.kind}`} title={m.last_error ?? undefined}>
                  {st.kind === 'failing' ? (refusalHint(m.last_error) ?? st.text) : st.text}
                </div>
              </div>
            )
          })}
        </>
      )}

      {/* ---- pending joins: GET /admin/joins ---- */}
      {joins.length > 0 && (
        <>
          <div className="section-head">
            <h2 className="runs-title">pending joins</h2>
            <ArmedButton label="clear all" onFire={() => act(post('/admin/joins/clear', {}), 'cleared')} />
          </div>
          <div className="meta activity-blurb">
            invites whose owner was unreachable — retried in the background until they succeed or you clear them
          </div>
          {joins.map((j) => (
            <div key={j.id} className="card share-card">
              <div className="card-head">
                <span className="mono join-ticket" title={j.ticket}>
                  {j.ticket.slice(0, 12)}…
                </span>
                <span className="meta">
                  {j.attempts} attempt{j.attempts === 1 ? '' : 's'} · queued {relTime(j.created_at, now)}
                </span>
                <span className="gardener-actions">
                  <ArmedButton label="clear" onFire={() => act(post('/admin/joins/clear', { id: j.id }))} />
                </span>
              </div>
              {j.last_error && (
                <div className="meta sync-line failing" title={j.last_error}>
                  {refusalHint(j.last_error) ?? j.last_error}
                </div>
              )}
            </div>
          ))}
        </>
      )}

      {/* ---- contacts ---- */}
      <h2 className="runs-title">contacts</h2>
      {contacts.length === 0 && (
        <div className="palette-empty">
          nobody yet — share a doc to mint an invite link, or paste one above
        </div>
      )}
      {contacts.map((c) => (
        <ContactRow key={c.id} c={c} onChanged={load} />
      ))}

      {/* ---- recent activity ---- */}
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

  const rename = async () => {
    setEditing(false)
    const petname = name.trim()
    if (!petname || petname === c.petname) return
    await api('/admin/contacts/rename', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: c.id, petname }),
    }).catch((e) => notify(errText(e)))
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
          {shortFingerprint(c.pubkey)}
        </span>
        {c.revoked && (
          <>
            <span className="verdict v-red" title="blocked: a new invite from them is refused until you unblock">blocked</span>
            <span className="gardener-actions">
              <button
                className="chip"
                title="let this person pair again with a fresh invite"
                onClick={() => post('/admin/contacts/unrevoke', { id: c.id }).then(onChanged, (e) => notify(errText(e)))}
              >
                unblock
              </button>
              <ArmedButton
                label="remove"
                title="forget this contact entirely (their past edits keep their name)"
                onFire={() => post('/admin/contacts/remove', { id: c.id }).then(onChanged, (e) => notify(errText(e)))}
              />
            </span>
          </>
        )}
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
                }).then(onChanged, (e) => notify(errText(e)))
              }
            >
              {c.verified ? '✓ verified' : 'verify'}
            </button>
            <ArmedButton
              label="remove"
              title="revoke every share to them and forget the contact — they can pair again with a fresh invite"
              onFire={() => post('/admin/contacts/remove', { id: c.id }).then(onChanged, (e) => notify(errText(e)))}
            />
            <ArmedButton
              label="block"
              title="revoke every share to them AND refuse any future invite from this Mac until unblocked"
              onFire={() => post('/admin/contacts/revoke', { id: c.id }).then(onChanged, (e) => notify(errText(e)))}
            />
          </span>
        )}
      </div>
    </div>
  )
}

/** One hub I belong to: my standing, what I published (with unpublish), and —
 * once expanded — the member list (approve / eject for admins). */
function HubCard({
  hub,
  onOpenDoc,
  onChanged,
  now,
}: {
  hub: HubRow
  onOpenDoc: (id: string) => void
  onChanged: () => void
  now: number
}) {
  const [open, setOpen] = useState(false)
  const [members, setMembers] = useState<HubMember[] | null | undefined>(undefined)
  const [status, setStatus] = useState<HubMembersResponse['status'] | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [invite, setInvite] = useState<string | null>(null)
  // slice 2 (admins): transfers members offered, and proposals on hub-owned docs
  const [transfers, setTransfers] = useState<HubTransfer[]>([])
  const [queue, setQueue] = useState<QueueRow[]>([])
  const [showQueue, setShowQueue] = useState(false)

  const fetchAdminExtras = () => {
    api<{ transfers?: HubTransfer[]; error?: string }>(`/admin/hubs/transfers?hub=${hub.contact_id}`)
      .then((r) => setTransfers(r.transfers ?? []))
      .catch(() => setTransfers([]))
    api<{ items?: QueueRow[]; error?: string }>(`/admin/hubs/queue?hub=${hub.contact_id}`)
      .then((r) => setQueue(r.items ?? []))
      .catch(() => setQueue([]))
  }
  const fetchMembers = () => {
    setErr(null)
    api<HubMembersResponse>(`/admin/hubs/members?hub=${hub.contact_id}`)
      .then((r) => {
        setStatus(r.status)
        setMembers(r.members)
        if (r.error) setErr(r.error)
        if (r.status?.role === 'admin' && r.status.membership === 'active') fetchAdminExtras()
      })
      .catch((e) => {
        setErr(errText(e))
        setMembers(null)
      })
  }
  const decide = (path: string, body: unknown, okMsg: string, key: string) => {
    if (busy) return
    setBusy(key)
    post(path, body)
      .then(() => {
        notify(okMsg, 'ok')
        fetchAdminExtras()
        onChanged()
      }, (e) => notify(errText(e)))
      .finally(() => setBusy(null))
  }
  const toggle = () => {
    const next = !open
    setOpen(next)
    if (next && members === undefined) fetchMembers()
  }
  const act = (path: string, body: unknown, okMsg: string, key: string) => {
    if (busy) return
    setBusy(key)
    post(path, body)
      .then(() => {
        notify(okMsg, 'ok')
        fetchMembers()
        onChanged()
      }, (e) => notify(errText(e)))
      .finally(() => setBusy(null))
  }
  const mintInviteLink = () => {
    if (busy) return
    setBusy('invite')
    api<{ link?: string }>('/admin/hubs/invite', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ hub: hub.contact_id }),
    })
      .then((r) => setInvite(r.link ?? null), (e) => notify(errText(e)))
      .finally(() => setBusy(null))
  }

  const role = status?.role ?? hub.role
  const membership = status?.membership ?? hub.membership
  const isAdmin = role === 'admin' && membership === 'active'
  const pending = (members ?? []).filter((m) => m.membership === 'pending')
  const active = (members ?? []).filter((m) => m.membership === 'active')

  return (
    <div className="card share-card hub-card">
      <div className="card-head">
        <span className="hub-mark" title="a hub: a team Grimoire">⌂</span>
        {hub.root_doc_id ? (
          <span className="card-doc" onClick={() => onOpenDoc(hub.root_doc_id!)}>
            {hub.name}
          </span>
        ) : (
          <span>{hub.name}</span>
        )}
        <span className="meta">{hubStandingLine(role, membership)}</span>
        {hub.relayed_docs > 0 && (
          <span className="meta" title="docs other members published, relayed to you">
            {hub.relayed_docs} relayed doc{hub.relayed_docs === 1 ? '' : 's'}
          </span>
        )}
        <span className="gardener-actions">
          {membership === 'active' && (
            <button className="chip" onClick={toggle}>
              {open ? '▾ members' : '▸ members'}
            </button>
          )}
        </span>
      </div>
      {hub.publications.length > 0 && (
        <div className="hub-pubs">
          <div className="meta">{publishedLine(hub.name, hub.publications.length)}</div>
          {hub.publications.map((p) => (
            <div key={p.share_id} className="share-row">
              <span className="card-doc" onClick={() => onOpenDoc(p.root_doc)}>
                {p.root_title || '(untitled)'}
              </span>
              <span className="meta">
                {p.doc_count} doc{p.doc_count === 1 ? '' : 's'}
                {p.state === 'offered' ? ' · sending…' : ''}
              </span>
              <ArmedButton
                label="unpublish"
                title={`stop publishing this subtree to ${hub.name} — members lose it on their next sync`}
                onFire={() => act('/admin/shares/revoke', { id: p.share_id }, `unpublished “${p.root_title}”`, p.share_id)}
              />
            </div>
          ))}
        </div>
      )}
      {(hub.transfers ?? []).length > 0 && (
        <div className="hub-pubs">
          <div className="meta">handed to {hub.name}</div>
          {(hub.transfers ?? []).map((t) => (
            <div key={t.id} className="share-row">
              <span className="card-doc" onClick={() => onOpenDoc(t.root_doc)}>
                {t.root_title || '(untitled)'}
              </span>
              <span className="meta">
                {t.state === 'offered' ? 'offered — waiting for an admin to accept' : `owned by ${hub.name} since ${relTime(t.at, now)}`}
              </span>
            </div>
          ))}
        </div>
      )}
      {open && (
        <div className="hub-members">
          {err && <div className="meta err">{err}</div>}
          {isAdmin && transfers.some((t) => t.state === 'offered' || t.state === 'accepted') && (
            <>
              <div className="meta">folders offered to {hub.name}</div>
              {transfers
                .filter((t) => t.state === 'offered' || t.state === 'accepted')
                .map((t) => (
                  <div key={t.id} className="share-row">
                    <span>
                      {t.member} offers “{t.title}”
                    </span>
                    <span className="meta">
                      {t.doc_count} doc{t.doc_count === 1 ? '' : 's'} · {relTime(t.at, now)}
                      {t.state === 'accepted' ? ' · taking over…' : ''}
                    </span>
                    {t.state === 'offered' && (
                      <>
                        <button
                          className="accept"
                          disabled={!!busy}
                          title={`${hub.name} takes “${t.title}” over; ${t.member} keeps a read-only copy`}
                          onClick={() => decide('/admin/hubs/transfers/accept', { hub: hub.contact_id, id: t.id }, `taking “${t.title}” over from ${t.member}`, t.id)}
                        >
                          {busy === t.id ? 'accepting…' : 'accept'}
                        </button>
                        <ArmedButton
                          label="decline"
                          onFire={() => decide('/admin/hubs/transfers/decline', { hub: hub.contact_id, id: t.id }, `declined “${t.title}”`, t.id)}
                        />
                      </>
                    )}
                  </div>
                ))}
            </>
          )}
          {isAdmin && queue.length > 0 && (
            <>
              <div className="share-row">
                <span className="meta">{waitingLine(queue.length)} on {hub.name}’s own docs</span>
                <button className="chip" onClick={() => setShowQueue((v) => !v)}>
                  {showQueue ? 'hide' : 'review'}
                </button>
              </div>
              {showQueue &&
                queue.map((r) => {
                  const d = describeChange(r)
                  const id = r.item.annotation.id
                  return (
                    <div key={id} className="share-row hub-queue-item">
                      <span>
                        <b>{r.doc_title}</b> · {r.proposer} {d.headline}
                      </span>
                      {d.after && <span className="meta">“{d.after.text.slice(0, 120)}”</span>}
                      <button
                        className="accept"
                        disabled={!!busy}
                        onClick={() => decide('/admin/hubs/resolve', { hub: hub.contact_id, annotation_id: id, decision: 'accept' }, `accepted ${r.proposer}’s edit to “${r.doc_title}”`, id)}
                      >
                        ✓ accept
                      </button>
                      <button
                        className="decline"
                        disabled={!!busy}
                        onClick={() => decide('/admin/hubs/resolve', { hub: hub.contact_id, annotation_id: id, decision: 'decline' }, `declined ${r.proposer}’s edit to “${r.doc_title}”`, id)}
                      >
                        ✗ decline
                      </button>
                    </div>
                  )
                })}
            </>
          )}
          {members === undefined && !err && <div className="meta">asking {hub.name}…</div>}
          {members === null && !err && status && (
            <div className="meta">
              {status.members} member{status.members === 1 ? '' : 's'}
              {status.pending > 0 ? ` · ${status.pending} waiting` : ''} — only admins see who
            </div>
          )}
          {members && (
            <>
              {pending.length > 0 && (
                <>
                  <div className="meta">waiting to join</div>
                  {pending.map((m) => (
                    <div key={m.contact_id} className="share-row">
                      <span>{m.petname}</span>
                      <span className="meta mono" title={m.pubkey}>{shortFingerprint(m.pubkey)}</span>
                      <span className="meta">asked {relTime(m.paired_at, now)}</span>
                      {isAdmin && (
                        <>
                          <button
                            className="accept"
                            disabled={!!busy}
                            onClick={() => act('/admin/hubs/approve', { hub: hub.contact_id, contact_id: m.contact_id }, `${m.petname} approved — ${hub.name} is being sent to them`, m.contact_id)}
                          >
                            {busy === m.contact_id ? 'approving…' : 'approve'}
                          </button>
                          <ArmedButton
                            label="reject"
                            onFire={() => act('/admin/hubs/eject', { hub: hub.contact_id, contact_id: m.contact_id }, `${m.petname} rejected`, m.contact_id)}
                          />
                        </>
                      )}
                    </div>
                  ))}
                </>
              )}
              <div className="meta">members</div>
              {active.map((m) => (
                <div key={m.contact_id} className="share-row">
                  <span>{m.petname}</span>
                  <span className="meta mono" title={m.pubkey}>{shortFingerprint(m.pubkey)}</span>
                  {m.role === 'admin' && <span className="badge state-active">admin</span>}
                  {m.publications.length > 0 && (
                    <span className="meta">
                      published {m.publications.length}: {m.publications.map((p) => p.root_title).join(', ')}
                    </span>
                  )}
                  {isAdmin && (
                    <span className="gardener-actions">
                      <button
                        className="chip"
                        disabled={!!busy}
                        title={m.role === 'admin' ? 'make a plain member' : 'make an admin (can approve and remove members)'}
                        onClick={() =>
                          act(
                            '/admin/hubs/role',
                            { hub: hub.contact_id, contact_id: m.contact_id, role: m.role === 'admin' ? 'member' : 'admin' },
                            `${m.petname} is now ${m.role === 'admin' ? 'a member' : 'an admin'}`,
                            m.contact_id,
                          )
                        }
                      >
                        {m.role === 'admin' ? 'make member' : 'make admin'}
                      </button>
                      <ArmedButton
                        label="remove"
                        title={`remove ${m.petname} from ${hub.name}: their published docs go, and they cannot rejoin until unblocked on the hub`}
                        onFire={() => act('/admin/hubs/eject', { hub: hub.contact_id, contact_id: m.contact_id }, `${m.petname} removed`, m.contact_id)}
                      />
                    </span>
                  )}
                </div>
              ))}
              {isAdmin && (
                <div className="hub-invite">
                  <button className="chip" disabled={!!busy} onClick={mintInviteLink}>
                    invite someone
                  </button>
                  {invite && <InviteLink link={invite} />}
                </div>
              )}
            </>
          )}
        </div>
      )}
    </div>
  )
}
