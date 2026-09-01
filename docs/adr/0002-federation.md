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

## The one scary thing

A write-accepting network surface on a daemon that currently trusts localhost
absolutely — a bug in federation auth is a bug in the entire private vault. Mitigations
are decisions 6 and 7: separate listener, deny-by-default pubkey allowlists, only
shared subtrees reachable, view-only first.
