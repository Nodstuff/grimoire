# ADR 0002 — Federation: owner-authoritative mirrors over iroh

- **Status**: decided
- **Who/when**: Tom + Claude, 2026-09-01 (design session; realises PROJECT.md §6 / P2.7)
- **Tickets**: TBD (P2.7 slice-up)

## Decision

1. **Owner-authoritative mirrors, never multi-master.** Every doc has exactly one home
   instance. A share exports a doc or subtree (recursive containment, same rule as tend
   scopes); downstream holds a mirror pinned to an upstream epoch cursor. Sync is
   pull-based: `diff_since(doc, my_cursor)` over the wire — the primitive already exists.
2. **Pessimistic mirror.** A downstream edit ships upstream as a proposal through the
   owner's gate and renders locally as a pending overlay ("proposed, awaiting owner");
   the mirror itself stays a faithful replica and only changes when a pull lands. No
   optimistic local apply, no forks, no downstream merge machinery — declines cost
   nothing to reconcile.
3. **Remote = gardener, architecturally.** Downstream proposals arrive as ops from a
   `remote` principal against their synced base epoch and are scored by the same gate,
   routed by the share's review policy, resolved in the same review queue. Trust tiers
   are policy, not code: stranger → yellows at best; trusted peer → greens can flow.
4. **Per-share grants: `view` | `propose`**, enforced at the owner's federation surface.
   `view` serves snapshots + ops; `propose` also accepts proposals. Grants are never
   settable by the remote side, and not over MCP — same principle as review policies.
   Share table: `{share_id, root_doc_id, grantee_pubkey, permission, policy_override?,
   enabled}`.
5. **Transport & identity: iroh.** QUIC with hole-punching and public relays; an iroh
   node ID is an ed25519 public key and doubles as the remote principal's `pubkey` —
   identity and transport collapse into one primitive. No infra to run; relays are
   self-hostable later. UUIDs identify data; keys identify actors.
6. **Join is pair-once, share-many. No directory, no global discovery.**
   - **Keys are invisible.** The daemon mints its ed25519 keypair silently on first
     launch (an iroh endpoint requires one anyway) and stores it in the OS keychain.
     Users see petnames, never keys. Optional surfaces only: safety-number-style
     fingerprint verification in contact details; key export/import alongside
     `grimoire export` for machine migration (identity is per-machine in v1).
   - **First contact** exchanges keys via an invite ticket `{owner node ID + relay
     hint, share_id, one-time secret}` dressed as a `grimoire://join/…` link (QR for
     in-person; https fallback landing page that doubles as the install funnel).
     Grantee dials over iroh (mutual key proof in the handshake), redeems the secret;
     owner burns it. Redeem is async — the grantee's daemon retries in the background;
     secret validity ~7 days, single-use.
   - **Thereafter the person is a contact** (`{pubkey, petname, paired_at, verified?}`):
     sharing with a known contact queues an offer their instance picks up on next dial
     and surfaces as an accept card. No new secrets or pastes per share, and offers are
     explicit-accept on both sides — nothing appears silently on anyone's machine.
   - Revoke = disable the share row (or drop the contact entirely); next dial refused.
   - Multi-device identity (one person, several keys) is deferred; the share →
     contact → pubkey indirection is the seam that makes it retrofittable.
7. **The federation listener is its own surface**, deny-by-default per-share pubkey
   allowlists, serving only explicitly-shared subtrees. It never mounts `/api` or
   `/admin`. View-only ships first so the write path gets its own review pass.
8. **Hot docs (P2.1/P2.2) layer on top**, over the same iroh connection: a shared doc
   with two live editors goes hot (Yjs, epoch frozen); cool-down flattens to **one
   propose** through the owner's gate with session provenance. Joining a hot session
   requires `propose`; `view` gets read-only presence at most. Federation precedes hot
   docs — it is what makes a second concurrent editor exist.

## Edge semantics (locked early, cheap now, expensive later)

- Mirrors keep origin UUIDs — collision-free by federation tax #1 (ADR 0001 §5).
- Deletes ship as tombstone ops, like everything else.
- Wikilinks escaping the shared subtree render as dead links downstream; titles of
  unshared docs never leak.
- A doc moved into a shared subtree becomes visible on the grantee's next pull — the
  owner-side UI must make this loud (badge on the shared root, confirm on move-in).
- Downstream mirrors are read-only in the editor under `view`; under `propose` the
  editor writes ops into an outbound proposal instead of the local store.
- Owner-side rename/move inside the subtree is just ops; the mirror follows.

## Sequencing

1. Share model + identity: `shares` + `contacts` tables and the offer queue, remote
   principals with pubkeys, silent keygen + keychain storage, invite mint/redeem as
   `grimoire://` links + QR, grant/revoke in app + `/admin`.
2. Read path (view-only complete): authenticated snapshot + `diff_since` over iroh;
   downstream mirror docs flagged `origin: remote`; pull loop (poll-on-open + periodic).
3. Propose path: downstream edit → remote propose → owner's gate; pending-overlay UI
   downstream; a `my_proposals` equivalent tells downstream what happened.
4. Hot docs (P2.1/P2.2) inside shared sections.

## Alternatives

- **Tailscale-assumed plain HTTPS**: least new code, but cross-person sharing means
  tailnet-sharing friction, and identity needs a pubkey handshake anyway — iroh gives
  both for one dependency.
- **Hosted mailbox relay**: infra someone must operate before anyone shares anything;
  against the local-first ethos for v1.
- **Optimistic local apply downstream**: multi-master with extra steps — every mirror a
  potential fork, declines require downstream reverts, anchors drift. Rejected.
- **Global discovery/directory**: the Matrix/SSB graveyard — a server mapping names to
  keys is infrastructure and a trust root. Invites are out-of-band by design.
- **Wormhole-style short codes (SPAKE2)**: `4-guitar-sunset` spoken aloud —
  cryptographically sound (interactive exchange, one guess then dead) and lovely for
  in-person pairing, but needs a rendezvous mechanism and a PAKE dependency for a flow
  the link already covers. Parked, not rejected: revisit if verbal pairing matters.
- **Manual key ceremony**: rejected outright — key generation, display, or exchange as
  a user-visible step kills adoption; the secret-over-existing-channel model already
  bounds security at the channel, so visible keys add friction without adding trust.

## Local trust boundary (added 2026-09-02)

Localhost is no longer trusted absolutely. Every gate-weakening route — shares,
trust tiers, contact revoke/unrevoke, gardeners, review policy, joins — lives under
`/admin/*` and requires a per-boot random token (`X-Grimoire-Admin`). The daemon
writes it to `<db dir>/admin.token` (mode 0600, replaced on every start); the Tauri
shell reads it and hands it to the page as `?admin_token=` (stripped from the URL at
boot, held in `sessionStorage`), and the CLI reads the same file. `/api/*` and `/mcp`
stay open on 127.0.0.1: reading, proposing through the gate, and the profile are the
"local agent" surface and are not gate-weakening. MCP still exposes no admin tool.

Honest boundary: any process running **as the same user** can read the token file, so
the token does not protect against a compromised user account. What it does refuse is
everything else that can reach 127.0.0.1 — browser tabs and web content (which cannot
read files), sandboxed apps, and other users on a shared machine. That is the same
boundary the identity key already has.

## The one scary thing

A write-accepting network surface on a daemon that currently trusts localhost
absolutely — a bug in federation auth is a bug in the entire private vault. Mitigations
are decisions 6 and 7: separate listener, deny-by-default pubkey allowlists, only
shared subtrees reachable, view-only first.

## Invites v2 (added 2026-09-03)

**Short links.** `grimoire://join/<node-id-hex>/<secret>` — ~107 characters, readable
aloud. The secret is 16 random bytes in lowercase RFC 4648 base32 without padding (26
chars). The share id no longer travels: the owner resolves the invite by the secret's
hash alone (`redeem_invite` already keyed on `secret_hash`). The v1 form
`grimoire://join/<base64url(json)>` still parses so links minted before this change keep
working for their 7-day life. A link remains the way to make FIRST contact.

**Offers: sharing with a contact needs no link.** The owner mints the same one-time
invite and delivers it over the wire as `Request::Offer {share, root_title, permission,
secret, expires_at}` to a contact's daemon. The recipient accepts an `Offer` only from a
known, non-revoked contact (the same gate as `Notify`), stores it in a durable
`share_offers` table (state open|accepted|declined|expired, expiry = the invite's), and
raises a `share_offered` runtime event. The app shows open offers as **Share requests**
at the top of the Shares page with accept/decline and counts them in the header chip; the
toast only points there. Accepting redeems the stored secret exactly like a pasted link
(`join_once`, then the first pull) — nothing new is trusted. Declining closes the request
locally; the owner is not told (the invite simply expires). The owner's side records
`share_invites.offered_to`, so an unredeemed offered invite reads "waiting for alice".
If the contact is unreachable the invite still exists and the reply carries the link for
a manual send (`delivered: false`).

**Neighbours.** The existing mDNS address lookup now also feeds a presence list: the
daemon advertises the profile name as iroh user data and subscribes to discovery events;
`GET /admin/neighbours` returns Grimoires on the LAN with their name, whether they are
already a contact, and whether they are blocked. Presence grants nothing — a neighbour
still needs an invite or an offer — it only saves reading out a key.

## Hub (slice 1, added 2026-09-03)

A **hub** is an ordinary Grimoire run headless on an always-on box (`grimoire serve --hub
--name "Team"`; persisted in `settings` as `hub.enabled` / `hub.name` / `hub.root_doc`, so
later plain `serve` runs stay a hub). It is a peer like any other — same identity, invites,
mirrors, trust tiers — plus three rules:

1. **Membership has a gate.** Redeeming a hub invite pairs you as `pending` (no shares) until
   an admin approves; the very first contact ever paired becomes the first admin (`contacts.role`
   admin|member, `contacts.membership` pending|active|ejected). Admins act **over the wire from
   their own Grimoire** (`Request::HubAdmin { ListMembers | Approve | Eject | SetRole | Invite }`,
   authorized by the caller's role on the hub) or with `grimoire hub …` on the box; nobody needs
   the hub's UI. A pending member may only ask `HubStatus`. Approval mints a `propose` share of
   the hub root and delivers it as an `Offer` (it lands in the member's Share requests; accepting
   files "⌂ Team" in their tree). Ejection = membership `ejected` + contact blocked + every
   publication of theirs dropped + their folder removed.
2. **Members publish, the hub relays.** Publishing is a plain `propose` share **offered to the
   hub**; a hub auto-accepts offers from active members: it redeems, pulls, files the mirror root
   under `<hub root>/<member>` (folder created on first publication, owned by the hub) and records
   it in `hub_publications(share_id, member_contact, root_doc, published_at)`. Unpublish = the
   member revokes; the hub's `ShareRevoked` path drops the mirror and the publication, and other
   members lose it on their next pull.
3. **Every doc keeps one home.** `served_docs` still excludes mirrors everywhere — except, on a
   hub, mirrors that are hub publications under the hub root. Those ship with `origin_owner`
   (pubkey) and `origin_owner_name` in `WireDocMeta`; grantees store them on the mirror row
   (`mirrors.origin_owner`, `origin_owner_name`) and the app shows "owned by alice". In this slice
   the hub refuses `Propose`/`HotStart`/`HotEnd`/`EditPing` on any relayed doc with
   `RefusalCode::RelayedReadOnly` ("this doc is owned by alice — edits go to them, not the hub
   (coming soon)"); relayed docs open read-only in members' editors. Hub-owned docs (no
   origin_owner) behave like any owner's docs.

Wire additions: `HubAdmin`, `HubStatus` → `HubStatusIs{name, role, membership, members, pending}`,
`HubMembers`, `HubInvite`; `Redeemed` carries `is_hub` and `membership`; new refusal codes
`NotAllowed`, `Unsupported`, `RelayedReadOnly`. Contacts carry `is_hub`. Local routes: on any
Grimoire `GET /admin/hubs`, `GET /admin/hubs/members?hub=`, `POST /admin/hubs/{approve,eject,role,
invite}` (dial the hub); on the hub box `/admin/hub/{members,approve,eject,role,invite}`.

**Slice 2 (planned):** proposals on relayed docs travel member → hub → owner as a proposal *from
the member* (the hub forwards, never decides) and flow back through the relay; live sessions on
relayed docs bridge through the hub to the owner's daemon; editing hub-owned docs (gate
`agent-review`, admins resolve); **ownership transfer** as a two-sided ledgered op (offer/accept,
UUIDs preserved, the former owner's copy flips to a mirror; refused while any doc in the subtree is
hot or has open proposals; reversible by an admin) — the Qompass docs are the first transfer.
Deployment: the EC2 recipe from the 2026-09-02 verification, `serve --hub` under systemd.
