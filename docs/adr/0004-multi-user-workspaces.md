# ADR 0004 — Multi-user server: per-user workspaces and isolation in the store

- **Status**: accepted (2026-10-02)
- **Who/when**: Tom + Claude, 2026-10-02
- **Scope**: SERVER mode only. LOCAL mode (no `--public-url`) stays single-user and behaves as before.

## Context

A family shares one SERVER-mode Taisce. Before this ADR `auth_users` existed (passkeys, OAuth
grants, PATs) but every doc, workspace, search result and sync row was global: any signed-in
person saw everything. Workspace names were unique server-wide, so two people could not both
have "Work".

## Decision

### 1. The model

- **Users** are `auth_users` rows. The first `owner`-role row is the *instance owner* (the
  admin, who also owns every pre-existing doc). Every user has their own human principal
  (`auth_users.principal_id`); their `/api` writes are attributed to it. A second person is
  added on the box: `taisce auth user add <name>`, then `taisce auth enroll --user <name>`
  mints their passkey link (`auth enroll` alone still enrolls the owner).
- **Workspaces have an owner** (`workspaces.owner_id`, NULL = the instance owner, which only
  happens on a LOCAL database that never had users). Names are unique **per owner**,
  case-insensitively: `UNIQUE (COALESCE(owner_id, ''), name COLLATE NOCASE)`.
- **Members**: `workspace_members (workspace_id, user_id, role, added_at, added_by)`, role
  `owner | editor | viewer`. The workspace owner is always a member with role `owner`. A
  workspace is **shared** when it has more than one member.
- **Doc ownership** (`docs.owner_id`): the owner of the tree a doc is the root of. A doc's
  *space* is resolved from its ancestor chain, exactly like its workspace already is:
  - the nearest labelled ancestor (itself included) gives the workspace, else
  - the doc is in the **Unsorted of its root's owner** (`owner_id` of the top ancestor,
    NULL = the instance owner).

  Why the root's owner rather than each doc's own `owner_id`: a doc's workspace is already
  "whatever its nearest labelled ancestor says", so a subtree moves as a unit. Taking the
  owner from the root keeps that property — a child can never sit in someone else's
  Unsorted under a parent you own, and "children inherit the root's visibility" holds by
  construction. Every doc still gets `owner_id` at creation (a root: the creating user;
  a child: its root's owner), so a doc that later becomes a root keeps a meaningful owner;
  a move to the root sets it to the mover.
- **Visibility rule**: a user sees a doc iff its resolved workspace has them as a member, or
  it resolves to Unsorted and its root's owner is them. Anything else does not exist for
  them: by-id lookups answer **404**, never 403.
- **Write rule**: a user can write a doc iff its space is their own Unsorted, or they are
  `owner`/`editor` of its workspace. A `viewer` reads only (no edits, comments, proposals,
  resolves, moves, labels). A visible-but-read-only doc answers a 403-class `forbidden`
  error (the viewer can see it, so existence is not leaked).
- **Moves and labels are shares**: a move or label that changes a doc's space needs write on
  both the source and the destination, and leaving a space you do not own (a workspace
  where you are only an editor) is refused — only the owner moves docs out of a workspace.
  Merging across spaces is refused unless neither space is shared.

### 2. Enforcement: one place, the store, behind a `Scope`

`taisce_store::Scope`:

| Scope | Sees | Used by |
|---|---|---|
| `System` | everything | migrations, the embed indexer, backups, the admin CLI, push fan-out, gardener bookkeeping |
| `Local` | everything | LOCAL mode's one user (the behaviour before this ADR) |
| `User(id)` | the visibility rule above | every SERVER-mode request (OAuth or PAT: the token's user) |
| `Within { user, workspace }` | `User(user)` ∩ that workspace | automated writers (living-answer refresh, gardeners) whose target sits in a shared workspace, so nothing private to one member is copied where other members read it |

- The `SqliteStore` carries its scope. Every store method that touches docs, blocks, ops,
  annotations, comments, tags, edges, embeddings, workspaces, the change journal or
  idempotency rows filters by it: list queries add one visibility predicate (an
  `IN (<visible docs CTE>)` subquery with the user's id; `System`/`Local` add nothing, so
  LOCAL mode runs the same SQL as before); by-id methods check visibility first and answer
  `NotFound`; write methods check the write rule and answer `Forbidden`.
- The daemon never holds a bare store. It holds `taisce_store::SharedStore`, whose only way
  in is `lock(scope)` / `with_store(&store, scope, …)`: forgetting the scope is a compile
  error, and every `Scope::System` in the daemon is greppable. The raw `SqliteStore` is only
  reachable by a process that opened the file itself (CLI, tests), which is the System
  context by definition.
- The daemon derives the scope from the request: `auth::Authenticated` (inserted by
  `require_auth`) → `User(user_id)`; no identity and LOCAL mode → `Local`; no identity in
  SERVER mode → refused (deny by default).

### 3. Read-path inventory

Every surface reads through the scoped store, so each is filtered by construction; the
isolation suite (`crates/daemon/src/isolation_tests.rs`) proves each one with two users and
a guard test fails when a route or MCP tool is missing from its coverage table.

| Surface | Store paths | Notes |
|---|---|---|
| tree / nav (`GET /api/docs`, `orient`) | `list_docs`, `workspace_map` | |
| read (`/api/doc/{id}`, `read_doc`, markdown, export) | `read_doc`, `get_doc`, `read_block` | 404 when invisible |
| block refs `^abc123` | `blocks_by_id_suffix` | |
| find_doc / search / grep | `list_docs`, `search_blocks`, `live_blocks_with_titles` | FTS `LIMIT` applies after the filter |
| related / ask / embeddings | `block_vecs`, `blocks_as_hits`, `stale_block_vectors` | the in-memory vector index is global (System); candidates are cut to the viewer's visible blocks **before** top-k |
| backlinks / wikilinks / graph | `backlinks`, `linking_blocks`, `raw_links` | title resolution only matches visible docs |
| diff_since / history | `ops_since`, `ops_for_doc_limited` | |
| change feed (`/api/changes`, stream) | `changes_since` | rows for invisible docs dropped; access lost → targeted `deleted` row for that user |
| home / briefing / inbox / to-do / due | `list_docs`, `read_doc`, `create_doc*` | each user's own To-do and Inbox |
| pins / tags / tendings / flags | `list_tags`, `docs_by_tag`, `raw_tending`, `agent_flags` | |
| comments | `list_comments`, `add_comment` | viewers cannot comment |
| review queue / proposals | `review_queue`, `proposal_outcomes`, `resolve` | resolve needs write |
| trash | `list_trash`, `restore_doc` | |
| workspaces | `list_workspaces`, `find_workspace`, … | only member workspaces; `owner_name`, `display_name`, `role`, `shared` |
| gardeners / living / memory / filer | scoped to the gardener's (or answer doc's) owner, `Within` for shared targets | |
| push (APNs) | System fan-out → per-user visibility check | a device only hears about docs its user can see |
| idempotency | `idempotency_get/put` | key namespaced by the scope's user |

### 4. The change feed and revocation

`changes` gains `user_id` (NULL = a global row). Global rows are filtered by the viewer's
*current* visibility. Every write that can change who sees what (move, label, workspace
delete, member add/remove) snapshots each user's visible set before and after and writes
targeted rows: `deleted` for docs a user lost (the current app already drops a doc on
`deleted`; the row carries `access: "revoked"` for newer clients) and `tree` for docs a user
gained (the client fetches them).

### 5. MCP

- The MCP caller is the token's user (OAuth grant or PAT owner). Read tools take the request
  context like write tools so they know who is asking.
- Workspace names (`find_doc`, `orient`, `search`, `grep`, `related`, `create_doc`,
  `doc_op op=workspace`) resolve **only among the caller's visible workspaces**: an id wins;
  else the caller's own workspace of that name; else a single shared one; else an error
  listing the candidates as `Name · Owner (id …)`. `create_missing` creates under the caller.
- Sharing is never an MCP surface (the guard test also fails on any tool named like a
  sharing surface). An agent writing into a **shared** workspace always lands at least
  yellow: `propose`/`apply`/`add_comment`/`create_doc` content get the review cap
  (`tenancy::agent_into_shared`; the `auto` policy cannot clean it), an agent's link rewrite
  (from a rename) inside a shared doc is applied and flagged, an agent move into or out of a
  shared workspace parks red, an agent may not label a doc into or out of one, and an agent
  may not resolve proposals there (its members do).
- The principals list a user gets is their own human principal, the people they share a
  workspace with, and the agents that wrote in (or created) a doc they can see — an agent's
  `claude:<label>` can say what someone is working on. Identity resolution by name
  (`as`, `scribe`) stays unscoped so a filtered list never mints a duplicate principal.
- Per-user singletons: Inbox, the Unsorted To-do, Answers and Claude Memory are each
  person's own root (`own_root_titled`); `/api/todo/due` alerts only on the viewer's own
  lists (their Unsorted and the workspaces they own). Settings written in a user scope are
  that user's (`user.<id>.<key>`); the instance owner still reads the pre-0004 keys.

### 5a. After the adversarial review (2026-10-02)

- **Cross-doc references are same-doc or masked.** A comment's `refers_to` must be a block of
  its own doc (checked at projection, so `apply`, `propose`, `park` and a later accept all
  refuse it; a gated cross-doc anchor parks red and never applies); comment threads and
  agent flags dereference only same-doc, visible targets. An op's payload naming another doc
  (a doc move's old/new parent and their titles) is masked per reader on every read path
  (`mask_op`: history, `ops_since`, review queue, proposal outcomes); the stored row keeps
  the pre-image for decline-revert. A link rewrite's `rename:Old → New` provenance is shown
  only if the reader can see the renamed doc. Answer sources list only citations the reader
  can see.
- **Renames rewrite a `[[link]]` only when every reader of the linking doc can see the
  renamed doc** (`tenancy::link_reaches`: the linking space's audience ⊆ the renamed doc's);
  a bare title match is never enough.
- **Connector tokens on `/api`** (third-party OAuth clients — anything but the person's own
  app): the safer option was taken. They may write only through the gated routes
  (`/api/propose`, `/api/propose_markdown`, `/api/comment`, `POST /api/docs`), attributed to
  their agent principal so the share gate applies exactly as over MCP; every other `/api`
  write (move, rename, status, delete/restore, resolve, labels, workspaces, members, import,
  to-dos, profile) answers 403 for them (`viewer::refuse_connector_writes`).
- **Restore** revives only the docs the restorer can write and counts only those.
- **A workspace's labelled root moves only with the owner** (unless deeper into the same
  workspace). Deleting a workspace returns each labelled root to its **contributor** (the
  doc's owner: the workspace owner, or a member who filed their own doc in): it stays where it
  now falls if they can write there, else it comes to their own root, in their Unsorted.
  Ownership never changes hands as a side effect of someone else's delete (round 2, N1).
- **Agent principals are per person** (`principals.owner_user`; NULL and every pre-0004 agent
  = the instance owner): the same `claude:<label>` or PAT name under two users is two
  principals, an `as` by id must be one of your own agents, the scribe is per person, and
  another person's agent reads `claude:x (Name)` in names shown to you. `pinned_label`
  consults only the caller's own connectors.
- **No existence probes**: the share route takes a user id (names are resolved on the box's
  CLI) and answers an unknown id exactly like a known one; refusals of `as` names and ids
  are generic; an unlabel toward a hidden parent is a generic Forbidden; a viewer's GET of a
  shared workspace's missing To-do creates nothing. The instance owner still replays
  pre-0004 idempotency rows (bare principal key).

### 5b. Review round 2

- **History starts at access (N2).** A reader sees a doc's ops only from the moment it entered
  their view — their latest `granted` access row for it (written by the access journal on a
  share, label or move). Earlier ops, including text deleted before the share, are hidden
  from them on every history path (`ops_since`/`diff_since`, `/api/doc/{id}/history`, the
  review queue, proposal outcomes); members who saw the doc all along have no such row and
  see everything. A diff from an epoch before the access point simply starts there (no
  error); the current content and `propose_markdown`'s stale-base/missed-ops path work as
  before. The share CLI says "history before today stays private".
- **People's names are unique (N3)**, case-insensitively across users, human principals and
  agents, on rename and on user add; a CLI `--user <name>` must match exactly one user, and an
  ambiguous one is refused with the candidates' ids.
- **A GET is read-only for any non-first-party token (N4).** The only GET that wrote was
  `GET /api/todo` (find-or-create, carry-forward); for connectors it now only finds and
  answers 404 when there is no list.
- **First party is a pinned client_id (N5).** `oauth_first_party` holds the fixed,
  server-registered app client (`taisce-app`) and — once, at the first start of this build —
  the app's DCR clients that already existed (every redirect on `ie.null.taisce:`), so live
  app sessions keep working without a new sign-in. DCR maps the shipped app's exact
  registration (`ie.null.taisce:/oauth/callback` alone) to the fixed client and refuses any
  other use of the scheme; a client that merely declares the app's redirect is a connector.
  (The current app registers by DCR once per server and persists the `client_id`; it keeps
  working unchanged. A later app could ship `taisce-app` and skip DCR.)
- **Renames leave ambiguous links alone.** A link is rewritten only if every reader of the
  linking doc can see the renamed doc and none of them can see another doc of that title.

### 5c. Review round 3

- **Tombstones honour the history cut (S1).** For a user scope a deleted block is NotFound
  unless its delete op is inside the reader's history (they had access when it was deleted);
  a comment's `refers_to` is masked on every read for a reader who cannot read its anchor
  (`read_doc`, `read_block`, comment threads; flags drop deleted targets). Every by-id block
  path goes through `read_block` (full id, `^ref`, `read_doc block:`/`section:`, `related`,
  `add_comment`); `prior` pre-images only ride on ops inside the cut.
- **The cut is monotonic**: a grant row records the ops rowid high-water mark
  (`changes.ops_mark`); history, tending and the principals list compare op rowids against
  it, never timestamps.
- **The change feed shows only rows inside the reader's access window** (after their latest
  grant row for the doc); the page's `seq` is still the journal head, so the cursor advances
  exactly as before. Push uses the same window.
- **Grandfathering pins only real sign-ins**: the app's exact redirect AND a live grant
  (unrevoked, unexpired refresh token). A pre-registered client nobody signed in with is not
  pinned.
- **Names are compared normalised**: invisible/format characters stripped, NFKC, case-folded,
  whitespace collapsed, then the UTS #39 confusable skeleton (`unicode-security`, the
  unicode-rs crate rustc's confusable lints use); mixed-script names (e.g. Latin + Cyrillic)
  and names carrying invisible characters are refused. `principals.name_key` has a partial
  UNIQUE index for humans, so a CLI and the server cannot race two people into one name.
- **Revocation is per grant**: every app sign-in shares the client `taisce-app`; the CLI
  revokes grants and tokens by id, never by client (noted in `taisce auth revoke --help`).

**Stored cross-doc references and how each is scoped**

| Reference | Where | Scoped by |
|---|---|---|
| `blocks.refers_to` | comment anchors | same doc enforced on insert; reads join same-doc + `vis()` |
| `blocks.parent_id`, block `Move.new_parent` | block trees | `live_block(doc)`: same doc |
| `ops.payload` MoveDoc `new_parent`/`from_parent` + titles | doc moves | masked per reader (`mask_op`) |
| `ops.payload` RenameDoc `from_title`, DeleteDoc `title`/`doc_count` | same doc | the doc's own visibility; delete needs the whole subtree writable |
| `ops.prior` | pre-image | same-doc block |
| `ops.source_refs` `rename:Old → New` | link rewrites | shown only if the renamed doc is visible; rewrite only where `link_reaches` |
| `annotations` | doc + op | the doc's visibility (`vis(a.doc_id)`) |
| `answer_sources.block_id` | answers | rows of invisible/gone blocks dropped per reader |
| `audits.doc_id` | auditor coverage | read only by `audit_candidates` (`vis()`) |
| `gardeners.scope_doc` | tendings | gardener owner (`gardener_pred`); tendings 404 for invisible docs |
| `edges.to_target` (a title) | wikilinks | resolved at read with `vis()` on both ends |
| `doc_workspace`, `workspaces.doc_ids` | labels | `vis()` on the labelled docs |
| `changes.doc_id` | feed | visibility now, or rows addressed to the reader |
| `idempotency.response` | retries | keyed by user |
| settings `memory.origin.<doc>` | memory sync | per-user setting namespace |

### 6. Human-only sharing surfaces

- Box CLI: `taisce workspace list|members|share|unshare`, `taisce auth user add`.
- `/api/workspaces/{id}/members` (GET for members; POST/DELETE for the workspace owner),
  refused unless the token is the person's own app (`owner_app`) or LOCAL; PATs never reach
  `/api` at all, and connector tokens (Claude over OAuth) are refused.
- Every membership change, user creation and refused cross-tenant write is recorded in
  `audit_events` and logged on the `taisce::audit` target. (The pre-existing `audits` table
  is the auditor gardener's per-doc coverage bookkeeping — keyed by doc, with a foreign key
  to `docs` — so it cannot hold user or membership events; `audit_events` sits beside it.)

### 7. Migration (user_version 8)

Additive and idempotent, one transaction for the data part:
1. `docs.owner_id`, `gardeners.owner_id`, `changes.user_id` (ALTER ADD COLUMN).
2. `workspaces` rebuilt once (copy → drop → rename, foreign keys off and checked after, the
   same dance as the `ops` and `gardeners` rebuilds) to drop the server-wide `UNIQUE` on
   `name` and add `owner_id` + the per-owner unique index.
3. `workspace_members`, `audit_events` created.
4. *Adoption*: if an instance owner exists, every doc/workspace/gardener with a NULL owner
   becomes theirs and every workspace gets them as `owner` member. Adoption also runs when
   `auth_ensure_owner` first creates the owner (a LOCAL database promoted to a server).
   Three UPDATEs and one INSERT…SELECT: milliseconds on ~900 docs / 17k blocks
   (`migration_v8_on_a_realistic_db` measures it).

### 8. What the Apple app must change (follow-up; not in this change)

- Errors are real statuses now: a route that used to answer `200 {"error":"not found: …"}`
  answers `404` with the same body (the `/api` status layer, `viewer::error_status`), and a
  viewer's write answers `403 {"error":"forbidden: read-only: …"}`. Treat a 404 on a queued
  write as "gone or no longer shared with you" (drop it, do not retry), a 403 as read-only.

- Workspace switcher: show `display_name` (falls back to `name`), badge `shared`, and hide
  write affordances when `role == "viewer"`.
- Sync: treat a `changes` row with `access == "revoked"` (kind `deleted`) as "drop the doc
  and its cached blocks, do not show it in Trash"; a `tree` row for an unknown doc means
  "fetch it".
- Sign-out / account switch: wipe the per-user cache (docs, blocks, cursor, pending writes);
  cursors are per user, never shared across accounts on one device.
- 403 `forbidden` on a write means "read-only here" (viewer), not a bug.
- Follow-up (round 3, recorded, not built): first-party sign-in through a universal-link
  https redirect (an `apple-app-site-association` file on taisce.null.ie and an
  `https://taisce.null.ie/...` redirect the app claims) instead of the `ie.null.taisce:`
  custom scheme any local app could claim; and the passkey page showing which device and
  when is asking to sign in. Both need the app; until then first party rests on the pinned
  client_id plus the custom-scheme redirect.

The API stays backward compatible: every existing field is still present, new fields are
additive, and a single-user server answers exactly as before.

## Rejected alternatives

- **Server-wide unique workspace names** (status quo): two people cannot both have "Work",
  and a name collision would leak the existence of someone else's workspace.
- **One shared Unsorted**: Unsorted is where unfiled private notes land; sharing it would
  make every new doc visible to everyone until filed — the opposite of deny-by-default.
- **Row-level filtering in handlers**: ~70 routes and 16 tools each repeating the rule; one
  forgotten filter is a leak, and nothing makes it a compile error. Rejected for the store
  scope.
- **A separate database per user**: the strongest isolation, but sharing a workspace would
  mean cross-database queries or copies (and reconciling edits across copies is the
  federation problem ADR 0002 already abandoned); search, backlinks and the change feed
  would need fan-out. Rejected: one database, filtered in one place.
- **SQLite temp views / connection-level state for the viewer**: a temp-table write per lock
  fires the commit hook (the change feed's wake-up); the predicate is generated in Rust
  instead, with the user id as a validated UUID literal.

## Open questions (decided the safer way for now)

1. Can a `viewer` comment? **No** (comments are writes). Easy to relax later.
2. Can an editor move a doc out of a workspace they do not own? **No**; only the owner.
3. Does the review queue show proposals on read-only (viewer) docs? It shows proposals on
   visible docs; resolving needs write.
4. Gardeners are per owner (`gardeners.owner_id`, default the instance owner); admin-created
   gardeners belong to the instance owner. A per-user gardener UI is future work.
5. The change-feed *head* (`seq`) is global, so a user can observe that something changed
   somewhere (a number, never a doc id or title). Acceptable for a family server.
6. Deleting a user is not provided (revoke their passkeys/tokens instead); their docs would
   need a reassignment policy first.
7. An agent creating a doc under a shared parent: the doc (its title) is visible to the
   members at once; its content lands flagged. Creation has no op to gate, so the title is
   not reviewable. Creating a ROOT doc labelled into a shared workspace is refused for
   agents (it would be a share).
8. Gardeners: a gardener whose scope doc is in a shared workspace runs `Within` that
   workspace; one tending a private subtree that contains a shared labelled descendant runs
   in its owner's full scope, so it can read the private part — its writes into the shared
   descendant are flagged by the share gate, but a human should read them before accepting.
9. Deleting a subtree that holds docs the caller cannot write (a descendant labelled into a
   workspace they only view, or cannot see) is refused with a generic message: it reveals
   that such descendants exist, never which.
10. A visible doc whose parent the viewer cannot see is a root for them (`parent_id` is
    masked to null in lists, reads and change summaries).

Accepted as is after round 3: an unlabelled doc a member moved under the owner's workspace
doc travels with the owner's tree when the workspace is deleted (it is part of that tree,
not a labelled root the member contributed).

Accepted as is after the review (no change): the global change-feed `seq` and the head the
stream wakes on; `change_stamp`'s user variant still moves with corpus-wide bm25 statistics
only indirectly (FTS ranking uses whole-corpus stats); the admin diagnostics log tail
(owner-only, may name any doc); an inherited review policy is not explained to a viewer who
cannot see the ancestor that sets it; a gardener's scope is chosen at run start (its writes
are still gated); the SSE stream re-scans invisible rows after each wake; deleting a subtree
with hidden descendants answers a generic Forbidden.
