# Taisce for iPhone, iPad (and later Mac)

**Naming:** the product is now **Taisce** (Irish: treasure kept safe). On the
Apple side that means the app target and display name `Taisce`, the bundle id
`ie.null.taisce`, and the package `TaisceKit` (formerly GrimoireKit). Wiki links
open in-app as `taisce://wiki/<title>`. Everything server-side keeps the name
"grimoire" for now (the crate, `~/.grimoire`, API paths, the MCP server, and
`X-Grimoire-*` headers); that rename is a separate cleanup later.

A native SwiftUI client for a Grimoire (Taisce) server: a view into its docs with an
offline cache. No web views for UI. iPhone + iPad first; Mac via Mac Catalyst
later.

## Layout

```
apple/
  TaisceKit/                 Swift package: all logic, tested on macOS
    Sources/TaisceKit/
      Models/                  Doc, DocSummary, Block, BlockNode, Change, Todo/Due, BlockOp
      API/                     ServerConfig (+ TokenProvider seam), APIClient
      Auth/                    OAuthClient, AuthSession (TokenProvider), PKCE, Keychain TokenStore
      Edit/                    OrderKey (port of the store's), DocEditor (BlockEdit → BlockOp)
      Cache/                   GRDB cache (docs, blocks + FTS5, todos, sync_state, outbox)
      Sync/                    SSEParser, changeStream, Backoff, SyncEngine (actor)
      Render/                  block markdown → RenderNode (swift-markdown), inline/wikilinks
      Todo/                    offline parser for the To-do doc
    Tests/TaisceKitTests/    Swift Testing + a URLProtocol mock server
  App/                         the iOS app (xcodegen)
    project.yml
    Taisce/                    AppModel, Router, Theme (tokens), Presentation (pure view models),
                               Views (components, block renderer), Screens
    TaisceTests/               view-model tests (Swift Testing, hosted in the app)
```

## Build and test

```sh
cd apple/TaisceKit && swift test
# read-only smoke against a real daemon (GETs only; never /api/todo):
TAISCE_LIVE_URL=http://127.0.0.1:7425 swift test --filter LiveServer
# public OAuth metadata of a server-mode daemon (no registration, no grant):
TAISCE_LIVE_AUTH_URL=https://taisce.null.ie swift test --filter LiveAuth

cd apple/App && xcodegen generate && open Taisce.xcodeproj
xcodebuild -project Taisce.xcodeproj -scheme Taisce -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' build
```

Integration against real scratch daemons (LOCAL on 7515, SERVER on 7516
signed in headlessly with the daemon's `softpasskey` example; temp dirs,
killed on exit):

```sh
# in the grimoire checkout: cargo build --release -p grimoire && cargo build --release -p grimoire --example softpasskey
apple/scripts/integration.sh [path to the grimoire checkout]
```

App-hosted tests (Keychain persistence, first-launch view models, failed
sign-in) run on a simulator:

```sh
cd apple/App && xcodegen generate
xcodebuild -scheme Taisce -destination 'platform=iOS Simulator,name=iPhone 18 Pro' test
```

`xcodebuild` needs the iOS platform/simulator runtime installed (Xcode ›
Settings › Components). Without it, the app sources can still be
type-checked against the simulator SDK by cross-building TaisceKit
(`swift build --triple arm64-apple-ios26.0-simulator --sdk $(xcrun --sdk iphonesimulator --show-sdk-path)`)
and running `swiftc -typecheck` over `App/Taisce/*.swift`.

Dependencies (SPM only): GRDB.swift 7.11.1, swift-markdown 0.9.0 (pulls
swift-cmark 0.9.0). SSE is hand-rolled (`SSEParser`, ~120 lines) rather than a
package: `URLSession.AsyncBytes.lines` drops the blank lines that end SSE
events, the resume cursor is ours (cache `last_seq`, not a library's
in-memory id), and the parser is pure and tested byte-by-byte.

## Sync contract

The client follows a global change cursor:

- `GET /api/docs` sends `X-Grimoire-Seq: <head>`, read in the same transaction
  as the tree. `GET /api/changes?since=0&limit=0` returns just `{seq, changes: [], more: false}`.
- `GET /api/changes?since=<seq>&limit=<n>` →
  `{"seq": Int, "changes": [{"seq", "doc_id", "kind": "doc"|"tree"|"deleted"|"restored", "epoch"?, "at", "doc"?}], "more": Bool}`.
  `doc` = `{title, parent_id, sort_key, status, current_epoch, deleted}` as
  of serving time, not as of `seq`. It is absent only for a hard-deleted doc.
  Errors use real statuses (400/404/500) with a `{"error"}` body.
- `GET /api/changes/stream` (SSE): `id: <seq>`, `event: change`,
  `data: <one change>`, `retry: 3000` on connect, `: ping` every 25 s,
  resumes from `Last-Event-ID`.

`SyncEngine` (actor, foreground-only `start()` / `stop()`):

1. Fresh cache (cursor 0): load the whole tree from `/api/docs` and start
   the cursor at its `X-Grimoire-Seq`. Without the header (an older daemon),
   start at 0.
2. Catch up: page `/api/changes` from `last_seq` until `more` is false;
   store the cursor after each page.
3. Follow the stream with `Last-Event-ID: <last_seq>`; a 60 s idle timeout
   (two missed heartbeats) counts as a drop.
4. On drop: exponential backoff with jitter (1 s → 30 s cap, floored by the
   server's `retry:`), reset after any healthy connection; resume from
   `last_seq`.

Applying a batch: last change per doc wins; `deleted` drops the doc and its
blocks; every row's `doc` state (title, parent, epoch) is applied in place, and `tree` / `restored` rows
refetch `/api/docs` only if a row lacks it; `doc` refetches the body
only for docs we hold (or the To-do doc, or `alwaysFetch`), otherwise just
marks the cached row stale (`current_epoch > body_epoch`) so opening it
fetches. UI listens via `updates()` (AsyncStream) and GRDB `ValueObservation`.

To-dos: deadlines are written `· due YYYY-MM-DD` or `· due YYYY-MM-DD HH:MM`.
The offline parser reads both forms. API items send the day in `deadline`,
plus `due_time` (`HH:MM`) and `alert_at` (local; 09:00 when there is no time).
`TodoItem.due` combines `deadline` and `due_time`.
The Today view reads due and overdue items from `GET /api/todo/due?until=`,
which is read-only and computes overdue by time. Today's list comes from the
cached To-do doc. The view never calls `GET /api/todo`, because a GET for
today carries items forward on the server. `todoSetDeadline` sends the day as
`deadline` and the time as `due_time` (also queueable via `Cache.enqueueDeadline`); a date-only deadline keeps the item's
existing time.

## Sign-in

The default server is `https://taisce.null.ie` (SERVER mode: every `/api`,
`/mcp` and `/ws` request needs a bearer). The server decides whether sign-in
is needed, not the URL: with tokens in the Keychain the app is signed in;
otherwise OAuth discovery runs, and a server without metadata (LOCAL mode,
e.g. `http://127.0.0.1:7425`) gets no sign-in screen. If discovery can't
reach the server, a loopback URL is assumed LOCAL and anything else shows
the sign-in screen.

Flow (`AuthSession`, driven by `AppModel`):

1. Discovery: `/.well-known/oauth-protected-resource` → authorization
   server metadata (RFC 9728 / 8414). No metadata = LOCAL mode, no auth.
2. Client: dynamic registration (`token_endpoint_auth_method: none`,
   redirect `ie.null.taisce:/oauth/callback`, which the server allows by
   default). The client id is cached in the Keychain per server and probed
   before reuse; a wiped server gets a fresh registration.
3. Browser: `ASWebAuthenticationSession` (`SystemWebAuthenticator`) on the
   authorize URL with PKCE S256 + `state`, callback scheme `ie.null.taisce`
   (also registered as `CFBundleURLTypes`). Not ephemeral, so the server's
   passkey page reaches iCloud Keychain passkeys. The callback's `state`
   and `iss` are checked before the code is exchanged.
4. Tokens: access + refresh in the Keychain (Valet), per server. Refresh is
   single-flight: the server rotates the refresh token on every use and
   revokes the grant if a spent one comes back (outside a 60 s grace), so
   concurrent 401s all join one refresh. A 401 renews once and retries (SSE
   included); `invalid_grant` signs out and the app shows `SignInView`.
5. Sign out (Settings): `POST /oauth/revoke` with the refresh token
   (revokes the grant server-side; best effort offline), then clear the
   Keychain tokens. The client id and the per-server cache stay.

To sign in: run the app, tap **Sign in**, and approve with your passkey
on the server's page. To enrol a passkey first, use an enrolment link from
`grimoire auth` on the server. To use a local daemon instead, set the server
URL to `http://127.0.0.1:7425` in Settings.

ATS: HTTPS everywhere, except a `localhost` exception plus
`NSAllowsLocalNetworking` (needed for `127.0.0.1`, since exception domains
can't name IP addresses).

## Editing (groundwork, no UI)

`DocEditor` holds one doc's blocks (the cached body plus every queued,
unsent propose for it: `Cache.editor(for:)`) and turns `BlockEdit`s into
`BlockOp`s for `POST /api/propose`:

| `BlockEdit` | ops |
|---|---|
| `.replaceText(id, text)` | `replace` (the server retypes it from the markdown) |
| `.insert(after:parent:type:content:id:)` | `insert` with a client-minted UUID and an order key between the neighbours |
| `.delete(id)` | `delete` for the subtree, children first (the server does not cascade) |
| `.move(id, after:parent:)` | `move` with a new order key; refuses cycles |

`OrderKey` is a port of `crates/store/src/order_key.rs`, tested against the
store's shared vectors. `Cache.enqueue(_:on:)` queues one propose per call
(its `request_id` is the outbox idempotency key) and folds it into the
editor. `OutboxReplayer` sends in order; when a propose lands at epoch N,
queued proposes for the same doc still based on the old epoch are rebased
to N, so a chain of offline edits applies green instead of scoring as stale
against our own writes. A live (hot) session's refusal keeps the queue for
later.

## What's stubbed

- Editing UI: none yet. No optimistic write of queued edits into the cache
  (the editor overlays them, the doc view does not), and no conflict UI: a
  stale base (someone else wrote first) comes back scored or red from the
  gate and is only recorded on the outbox row.
- Outbox idempotency: the server's `request_id` dedupe is in memory with a
  120 s TTL, so a replay long after a lost response can apply twice.
  Client-minted insert ids make a doubled insert fail instead of
  duplicating; `replace` and `move` are idempotent; a doubled delete fails
  harmlessly.
- `propose_markdown` queues are rebased like `propose`, but a stale base is
  an error there (whole-doc diff), so they are not chained.
- Pins are local (UserDefaults, per server); pinned docs are always fetched by sync.
- Doc checkboxes toggle through a queued `replace`; the rest of editing is the next pass.
- Diagrams (Mermaid, Vega-Lite, D2) render as labelled placeholder cards.
- APNs / background refresh, Mac Catalyst.

## Running the app against a scratch daemon

Launch arguments (UserDefaults' argument domain) make screenshots and UI
runs reproducible: `-serverURL http://127.0.0.1:7517`, `-tab
today|library|todos|search`, `-openDoc "<title>"`, `-searchQuery <text>`,
`-showSettings YES`. For example
`xcrun simctl launch booted ie.null.taisce -serverURL http://127.0.0.1:7517 -openDoc "Roadmap"`.
Start the daemon with `HOME=<tmp>` so it does not import this machine's
Claude memory files. Tests: `xcodebuild -scheme Taisce -destination 'platform=iOS Simulator,name=iPhone 18 Pro' test`.
