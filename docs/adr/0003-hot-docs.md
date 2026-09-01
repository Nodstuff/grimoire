# ADR 0003 — Hot docs and comment channels

- **Status**: decided
- **Who/when**: Tom + Claude, 2026-09-01 (design session; realises PROJECT.md §7 P2.1–P2.4 on the ADR 0002 transport)
- **Tickets**: #64–#67

## Decision

1. **Comment channel first (federated comments, gate-free).** A comment is
   conversation, not content — a wrong comment annoys but destroys nothing, so
   remote comments BYPASS the review gate: a grantee's comment on a mirror
   ships over a dedicated wire request and applies directly on the owner as a
   comment block with remote-principal provenance. The pull loop distributes
   threads to every grantee; the app's live-poll makes the comments panel an
   async chat. Content edits keep the full gate — the bypass is for the
   `comment` block type only, enforced owner-side.
2. **Hot sessions: the owner's daemon is the authority.** It hosts the CRDT
   doc (yrs), applies updates, and fans them out. Buy-not-build holds: yrs on
   the daemon, Yjs + y-prosemirror + @tiptap/extension-collaboration in the
   editor. Never write YATA.
3. **Transport topology**: each UI syncs with ITS OWN daemon over a local
   websocket (`/ws/hot/{doc}`); a grantee's daemon bridges to the owner's
   daemon over a long-lived iroh bi-stream (ALPN `grimoire/hot/0`). Frames are
   the y-sync protocol (sync step 1/2 + update + awareness), length-prefixed.
4. **Going hot is explicit** (a "go live" action; auto-on-second-editor is a
   later refinement) and requires `propose` permission for remote peers. The
   doc's **epoch freezes** while hot: local direct writes and gardener
   proposals against a hot doc are deferred/refused; the collab session is the
   only writer.
5. **Cool-down flatten**: on explicit end or idle timeout, the owner flattens
   the Yjs doc to markdown and lands it through the EXISTING mddiff propose
   path as one commit at the frozen epoch — unchanged blocks keep their ids,
   so comment anchors and deep links survive; provenance records the session
   (participants, duration). The gate and gardeners never see character-level
   chaos (P2.2 verbatim).
6. **Crash safety**: the session journal (raw Yjs updates) is appended to disk
   from the moment a doc goes hot and deleted only after the flatten commit is
   confirmed applied. A daemon restart with a journal present recovers the
   session state and flattens it as "recovered session".
7. **Presence** (cursors, who's-here) rides the same channel via the Yjs
   awareness protocol and is NEVER persisted — presence is not document state
   (P2.4).

## Alternatives

- **Comments through the propose gate**: safe but turns conversation into
  paperwork; the review queue would fill with chatter. Rejected — provenance
  plus block-type restriction is protection enough, and comments remain
  deletable/moderatable by the owner.
- **Grantee UIs connecting straight to the owner's daemon websocket**: fewer
  hops, but it would punch the owner's HTTP surface through the federation
  boundary — the iroh listener stays the only cross-instance surface.
- **Automerge / diamond-types / hand-rolled OT**: yrs/Yjs is the only pairing
  with a first-class ProseMirror binding on both ends of our stack.
- **Epoch-per-keystroke (no freeze)**: floods the ledger with sub-semantic
  ops and breaks the "one epoch = one meaningful commit" invariant.

## The one scary thing

The flatten replaces real doc content at a frozen epoch. Mitigations: it goes
through the gate like any propose (pre-images make it revertible op-by-op),
mddiff minimises the blast radius to actually-changed blocks, and the on-disk
journal survives until the commit is confirmed.
