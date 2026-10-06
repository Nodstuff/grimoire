# ADR 0005 — Share links: a public, read-only snapshot of a doc

- **Status**: accepted (2026-10-06; revised the same day after the security review)
- **Who/when**: Tom + Claude, 2026-10-06
- **Scope**: SERVER mode only. In LOCAL mode every route below answers 404.
- **Design**: the Taisce doc "Grimoire › Share links". The wire contract the server and the
  apps build against is in that doc's tree; this ADR records the server's decisions.

## Context

People want to send a doc to someone with no Taisce account, and read their comments. Until
now every route on the server needed a token, and every read went through a person's `Scope`.
This is the first surface a stranger reaches with no identity at all.

## Decision

### 1. A snapshot, never the live doc

The owner's app renders the doc (diagrams become SVGs, which only the client can draw) and
uploads `{title, markdown, assets, theme}`. The server stores it in its own tables
(`share_links`, `share_link_assets`, `share_link_comments`; schema v10). The public routes
**never read docs or blocks**: they run in `Scope::Public`, which sees no doc and owns
nothing. `Scope::user()` is None for it as for System/Local, so every path keyed on the user
denies it explicitly (`SqliteStore::deny_public`, or an empty predicate in the workspace,
settings, principal, change-feed, gardener, history, agent and idempotency queries); an
isolation test runs those lists and writes as a link reader. A new snapshot (`PATCH`) bumps
`revision`; the link stays.

The page is rendered from the stored markdown when it is served, by the running renderer,
and cached in memory per (link, revision); nothing rendered is stored (an older build's
output can never be served, and no stored HTML carries the token in its image URLs).

The tables are not called `shares`: federation's old `shares` table is dropped by name on
every open (`FEDERATION_TABLES`), which would have taken the links with it.

### 2. Who may make one, and for how long

Making, listing, changing and revoking links is a human surface, exactly like workspace
membership (ADR 0004): the person's own app (first-party OAuth client) or web session (with
CSRF). A connector token gets 403 (`Viewer::require_human_surface`,
`refuse_connector_writes`); a PAT never opens `/api` (401); no MCP tool exists. Every owner
route finds only the caller's own links (someone else's is 404, never 403), and each is in
the isolation suite's `COVERAGE`.

- Making or changing a link needs **owner or editor** on the doc (`may_write`: an invisible
  doc is 404, a viewer gets 403). Unsorted is its owner's own.
- Leaving a workspace (unshare) revokes the leaver's links to its docs. A doc deleted for good
  revokes its links (a trigger on `docs`; trashing keeps them). A demoted editor's links stay
  live (they can still read the doc) but cannot be changed.
- Caps per person: 100 live links (429) and 250 MB of live snapshots (413); 30 new snapshots
  an hour per link (429, counted only once the link is found to be the caller's).
- Revoking drops the snapshot (markdown and images) at once; the row stays so the link
  answers 410. Housekeeping (at start, then hourly) deletes links revoked or expired more
  than 30 days ago, comments included.
- Reading a link's comments (`GET`) changes nothing; `POST /api/shares/{id}/comments/read`
  marks them read.

### 3. The token

`b64url(HMAC-SHA256(key, "taisce-share-token\0" ‖ link id))`, 43 chars. The key is 32 random
bytes in `share-links.key` beside the database (created on first start, 0600), never in the
database: litestream replicates only `ks.db` (`infra/deploy/litestream.yml`), and backups are
`VACUUM INTO` copies of the database alone. Only the token's SHA-256 is stored, behind a
unique index that every public request is looked up by; the owner's URL is recomputed on
demand. Losing the key changes every link's URL (the old ones answer 404), so it belongs in
the box's own backup of `/var/lib/taisce`. Revoke is permanent; a passed `expires_at` answers
410 as well.

### 4. The page is hostile-content safe

- Markdown is rendered with pulldown-cmark 0.13; raw HTML is escaped (an HTML block shows as
  code), links keep only http, https, mailto and `#` (anything else, e.g. `javascript:`,
  becomes `#`; an email autolink becomes `mailto:`), `taisce-asset:<name>` becomes the link's
  image URL (a missing name renders a box), other images are not loaded.
- `GET /s/{token}`: a strict CSP with a fresh nonce for the one inline script (the server's
  own, `share/page.js`) **and `sandbox allow-scripts allow-popups
  allow-popups-to-escape-sandbox`** (no `allow-same-origin`): the page runs in an opaque
  origin on the web UI's host, so its script cannot reach the session cookie, the UI's
  storage or same-origin `/api`, and `/api`'s exact-Origin CSRF check refuses its `Origin:
  null`. Only the public comment routes answer `Origin: null` (CORS, no credentials, a
  preflight). Plus `X-Robots-Tag: noindex, nofollow`, `Referrer-Policy: no-referrer`,
  `Cache-Control: private, no-store`, framing refused. A subdomain of its own would be the
  stronger split; not now.
- Images (`/s/{token}/a/{name}`) are served with `CSP: default-src 'none'; style-src
  'unsafe-inline'; sandbox`, `nosniff` and `Content-Disposition: inline`: an SVG with a
  script in it is displayed, never run. Types are limited to SVG, PNG, JPEG and WebP and the
  bytes must match the type. They are cached in memory per (link, revision, name), 32 MB LRU.
- Every public response, 500s included, carries the public headers; a 500 says nothing more.
- Limits: 10 MB a snapshot, 2 MB an image or the markdown, 200 images (413 over).
- The preview (`POST /api/shares/preview`, for PDF) is the same page without the script, its
  images inline as data: URLs and loaded eagerly (a printed page never scrolls).

### 5. Comments from strangers

Off or on per link. A comment is `{name, body, anchor?, parent_id?}`, JSON only (415
otherwise), threads one level deep.

- Text: NFC and trimmed; names, titles and quotes lose format characters (Cf: bidi
  controls, zero-width characters, the BOM) and line/paragraph separators (Zl, Zp); bodies
  lose bidi controls. A name that reads as the owner's (NFKC, case-insensitive) is signed
  `<name> (guest)`. Limits: 60 and 4000 characters.
- Abuse: a honeypot field (`website`: 201, nothing stored); a per-IP and a per-link token
  bucket over every `/s/` request; stored-comment limits of 10 an hour and 30 a day per IP
  and link, 200 a day per link (429). IPv6 is keyed on its /64. Only `HMAC(key, link ‖ ip)`
  is stored, never the IP, and it is cleared after a day.
- A view is counted at most once a minute per link.
- A new comment sends the owner a visible APNs alert (`Push::ShareComment`, `kind:
  share_comment`).

**Client IP behind portus.** `client_ip` takes the last `X-Forwarded-For` hop when
`--trusted-proxy` is set (the box sets `TAISCE_TRUSTED_PROXY=true`). Portus (the
portus-dataplane in qork-pingora-proxy, both its pingora and rama paths) **appends** the
peer address to any `X-Forwarded-For` the client sent and replaces the header with that one
value, so the last hop is always the address portus saw: a client cannot choose it.

## Rejected

- **Serving the live doc behind a token.** It would put an unauthenticated reader on the
  doc store's read paths and make every future doc query a leak risk. A snapshot is also what
  the person meant to send.
- **Rendering diagrams on the server.** Only the clients have the renderers (reladraw,
  mermaid, vega-lite); the snapshot carries them as images.
- **Storing the token** (even beside its hash) so the URL can be shown again: it would sit in
  every backup and replica. Deriving it from a key kept outside the database costs one HMAC.
- **Storing the rendered HTML.** It carried the token (image URLs) and froze the renderer of
  the day.
