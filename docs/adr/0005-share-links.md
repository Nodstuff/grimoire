# ADR 0005 — Share links: a public, read-only snapshot of a doc

- **Status**: accepted (2026-10-06)
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
(`share_links`, `share_link_assets`, `share_link_comments`; schema v10) and renders the page
from that. The public routes **never read docs or blocks**: they run in a new
`Scope::Public`, whose visibility predicate is false for every doc, so even an accidental doc
read there is a `NotFound`. A new snapshot (`PATCH`) bumps `revision`; the link stays.

The tables are not called `shares`: federation's old `shares` table is dropped by name on
every open (`FEDERATION_TABLES`), which would have taken the links with it.

### 2. Who may make one

Making, listing, changing and revoking links is a human surface, exactly like workspace
membership (ADR 0004): the person's own app (first-party OAuth client) or web session (with
CSRF). A connector token gets 403 (`Viewer::require_human_surface`,
`refuse_connector_writes`); a PAT never opens `/api` (401); no MCP tool exists. A link may
be made of any doc the person can see (`see(doc)`, else 404); every other owner route finds
only the caller's own links (someone else's is 404, never 403). Each route is in the
isolation suite's `COVERAGE`.

### 3. The token

32 random bytes, base64url (43 chars). Stored twice: the token itself, so the owner's app
can show the URL again, and its SHA-256, which is the only thing a public request is looked
up by (a unique index; no comparison of secrets in Rust). Revoke is permanent: the row stays
with `revoked_at`, and the link answers 410. So does a passed `expires_at`.

### 4. The page is hostile-content safe

- Markdown is rendered server-side with pulldown-cmark 0.13; raw HTML is escaped (an HTML
  block shows as code), links keep only http, https, mailto and `#` (anything else, e.g.
  `javascript:`, becomes `#`), `taisce-asset:<name>` becomes the link's image URL (a missing
  name renders a box), other images are not loaded.
- `GET /s/{token}`: a strict CSP with a fresh nonce for the one inline script (the server's
  own, `share/page.js`), `X-Robots-Tag: noindex, nofollow`, `Referrer-Policy: no-referrer`,
  `Cache-Control: private, no-store`, framing refused.
- Images (`/s/{token}/a/{name}`) are served with `CSP: default-src 'none'; style-src
  'unsafe-inline'; sandbox`, `nosniff` and `Content-Disposition: inline`: an SVG with a
  script in it is displayed, never run. Types are limited to SVG, PNG, JPEG and WebP and the
  bytes must match the type.
- Limits: 10 MB a snapshot, 2 MB an image or the markdown, 200 images (413 over).

### 5. Comments from strangers

Off or on per link. A comment is `{name, body, anchor?, parent_id?}` (NFC, trimmed, 60 and
4000 characters), threads are one level deep. Abuse controls: a honeypot field (`website`:
201, nothing stored), a per-IP token bucket over every `/s/` request (`ratelimit::Class::Share`,
`--trusted-proxy` aware), and stored-comment limits of 10 an hour and 30 a day per IP and
link, 200 a day per link (429). Only a hash of (link, IP) is stored, never the IP. A new
comment sends the owner a visible APNs alert (`Push::ShareComment`, `kind: share_comment`).

## Rejected

- **Serving the live doc behind a token.** It would put an unauthenticated reader on the
  doc store's read paths and make every future doc query a leak risk. A snapshot is also what
  the person meant to send.
- **Rendering diagrams on the server.** Only the clients have the renderers (reladraw,
  mermaid, vega-lite); the snapshot carries them as images.
- **Looking the token up by value.** A hash index keeps the secret out of comparisons and
  query plans.
