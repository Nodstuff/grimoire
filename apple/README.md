# Taisce for iPhone, iPad and Mac

**Naming:** the product is now **Taisce** (Irish: treasure kept safe). On the
Apple side that means the app target and display name `Taisce`, the bundle id
`ie.null.taisce`, and the package `TaisceKit` (formerly GrimoireKit). Wiki links
open in-app as `taisce://wiki/<title>`. Server-side, the crate, `~/.grimoire`,
API paths and the MCP server keep the name "grimoire" for now; the bootstrap
header is `Taisce-Seq` (TaisceKit falls back to the old `X-Grimoire-Seq`).
The app never sends `Taisce-Principal`: in SERVER mode identity is the token.

A native SwiftUI client for a Grimoire (Taisce) server: a view into its docs with an
offline cache. No web views for UI. iPhone and iPad, and the Mac via Mac
Catalyst from the same target (see **Mac** below).

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
      Todo/                    Deadline (all-day | UTC instant), TodoClock, offline To-do parser
      Library/                 doc tree for the sidebar, wikilink lookups
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

Integration against real scratch daemons (LOCAL on 7515, SERVER on 7516,
or `TAISCE_IT_LOCAL_PORT` / `TAISCE_IT_SERVER_PORT`
signed in headlessly with the daemon's `softpasskey` example; temp dirs,
killed on exit):

```sh
# in the grimoire checkout: cargo build --release -p taisce && cargo build --release -p taisce --example softpasskey
apple/scripts/integration.sh [path to the grimoire checkout]
# binaries built elsewhere (e.g. another branch):
TAISCE_BIN=… SOFTPASSKEY_BIN=… apple/scripts/integration.sh
```

The UI launch test (`TaisceUITests`) needs a LOCAL daemon serving a doc
titled "Welcome" (`TEST_RUNNER_TAISCE_UI_URL`, default 127.0.0.1:7515); its
screenshot is an xcresult attachment. Use your own simulator
(`xcrun simctl create …`): another agent's test run on a shared one
replaces the installed app.

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

- `GET /api/docs` sends `Taisce-Seq: <head>`, read in the same transaction
  as the tree. `GET /api/changes?since=0&limit=0` returns just `{seq, changes: [], more: false}`.
- `GET /api/changes?since=<seq>&limit=<n>` →
  `{"seq": Int, "changes": [{"seq", "doc_id", "kind": "doc"|"tree"|"deleted"|"restored", "epoch"?, "at", "doc"?}], "more": Bool}`.
  `seq` is the journal HEAD, not the page's last row: page on from
  `changes.last.seq` while `more`.
  `doc` = `{title, parent_id, sort_key, status, current_epoch, deleted}` as
  of serving time, not as of `seq`. It is absent only for a hard-deleted doc.
  Errors use real statuses (400/404/500) with a `{"error"}` body.
- `GET /api/changes/stream` (SSE): `id: <seq>`, `event: change`,
  `data: <one change>`, `retry: 3000` on connect, `: ping` every 25 s,
  resumes from `Last-Event-ID`.

`SyncEngine` (actor, foreground-only `start()` / `stop()`):

1. Fresh cache (cursor 0): load the whole tree from `/api/docs` and start
   the cursor at its `Taisce-Seq`. Without the header (an older daemon),
   start at 0.
2. Catch up: page `/api/changes` from `last_seq` until `more` is false;
   store the cursor (the page's last row) after each page. A head below the
   cursor means the server's database was reset: re-bootstrap and mark
   every cached body stale. Then fetch the To-do doc if stale.
3. Follow the stream with `Last-Event-ID: <last_seq>`; a 60 s idle timeout
   (two missed heartbeats) counts as a drop.
4. On drop: exponential backoff with jitter (1 s → 30 s cap, floored by the
   server's `retry:`), reset after any healthy connection; resume from
   `last_seq`. An SSE event that doesn't decode leaves the cursor alone and
   falls back to a catch-up. The cursor never moves backwards (except that
   reset); `stop()` returns once the loop has unwound.

Applying a batch: last change per doc wins; `deleted` drops the doc and its
blocks; every row's `doc` state (title, parent, epoch) is applied in place, and `tree` / `restored` rows
refetch `/api/docs` only if a row lacks it; `doc` refetches the body
only for docs we hold (or the To-do doc, or `alwaysFetch`), otherwise just
marks the cached row stale (`current_epoch > body_epoch`) so opening it
fetches. A body fetch that fails doesn't stall the batch: a missing doc
(404, or the older 200 `not found`) is dropped, any other error leaves it
stale and is listed in `SyncEngine.failedDocs`. UI listens via `updates()`
(AsyncStream) and GRDB `ValueObservation`.

Outbox replay retries later on 5xx, 408, 429 and HTML (proxy) answers and
fails an entry only on the server's own refusal (a 4xx or `{error}`;
a 403 with the read-only message, a 404 is dropped: gone or no longer shared);
a live session's refusal waits too. A landed propose rebases the queued ones
for that doc only when the epoch moved by exactly our write (+1).

To-dos and time: the device owns local time; the server never reads a
local clock. A deadline is either an all-day date (` · due 2026-10-03`,
API `deadline`) or a UTC instant (` · due 2026-10-03T14:00Z`, API `due_at`);
`Deadline` models both. The pre-UTC ` · due D HH:MM` is read as UTC
(`legacy_time`), as the server reads it.

- Writing: a time picked on the phone is that wall time in the device's
  zone, converted to UTC (`Deadline.local`; in the October overlap the
  first 01:30 wins, in the March gap the time moves forward). Writes send
  `deadline` or `due_at` (plus the local day as `deadline`, which a pre-UTC
  server files the item under and a UTC one ignores).
- Reading: overdue, due-today and alerts are computed on the device, never
  taken from the server: an instant by the instant; an all-day date
  becomes overdue at local midnight after it and alerts at 09:00 local
  (so it follows the phone across time zones).
- Every to-do call sends `TodoClock` (`today`, `utc_offset`): SERVER mode
  refuses calls without `today`, and `utc_offset` lets it read a typed
  "due fri 3pm". Raw outbox writes go through `Cache.enqueueTodo` /
  `enqueueDeadline`, which add it.
- The Today view never calls `GET /api/todo` (a GET for today carries items
  forward on the server); it reads `GET /api/todo/due` (`dueToday(now:)`
  for the device's view) and the cached To-do doc, which sync fetches after
  bootstrap (`Cache.observeTodos()` to reload on).

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
   server metadata (RFC 9728 / 8414). "No auth" is believed only from a JSON
   404 or an HTML page from a loopback / plain-http host (a LOCAL daemon's
   SPA); a 5xx or HTML over https (proxy error, captive portal) is an error,
   never a silent downgrade.
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
   included); `invalid_grant` signs out and the app shows `SignInView`. The
   rotated tokens are kept in memory before the Keychain write, and the app
   wraps a refresh in `beginBackgroundTask` (`AuthSession.Shield`), so a
   suspension mid-rotation doesn't cost the grant.
5. Sign out (Settings): `POST /oauth/revoke` with the refresh token
   (revokes the grant server-side; best effort offline), then clear the
   Keychain tokens. The client id stays; the person's data goes (below).

Universal link (ADR 0004 follow-up): a Release build claims
`https://taisce.null.ie/oauth/app-callback` (Associated Domains `applinks:` and
`webcredentials:` for `TAISCE_APP_LINK_HOST`, mirrored into Info.plist as
`TaisceAppLinkHosts`; the server serves the `apple-app-site-association` file).
When the OS has `ASWebAuthenticationSession`'s `.https(host:path:)` callback
(iOS 17.4 / macOS 14.4; below the targets), the server is https and its host
is claimed (`OAuthCallback.appLink`), sign-in uses the cached client if it holds
that redirect, else registers `[https, custom]` (the server maps it to
`taisce-app`); a server that refuses it gets the custom scheme as before.
Debug builds have no Associated Domains (scratch daemons are http, and Mac test
runs need no profile change) and always use the custom scheme. A signed-in
device keeps its tokens and client.

One server, several people (ADR 0004):

- The cache is keyed to the signed-in person (`/api/profile` `principal_id`,
  `Cache.owner`). Sign-out wipes it (`Cache.wipe`: docs, bodies, FTS, to-dos,
  workspaces, cursor, outbox; `secure_delete`, VACUUM, WAL truncated, the
  iPhone's protection class re-applied) and the person's settings
  (`UserSettings`: pins, last workspace, Library folders), and re-plans alerts.
  At launch and after a sign-in, `CacheOwnership` wipes another person's (or,
  under a fresh sign-in, unclaimed) data before sync starts. A sign-out the app
  didn't ask for (revoked or expired grant) keeps the data until the next
  sign-in decides, so the same person keeps their queued writes.
- Workspaces carry `role`, `owner_name`, `display_name`, `shared`: the switcher
  shows `display_name`, your own first, `person.2` on shared ones. A viewer
  (`EditAccess`) gets no edit mode, ⌘E, new doc, checkboxes, to-do add/done/snooze
  or move; an editor can't move docs out of someone else's workspace; Manage
  workspaces edits only your own. A 403 on replay fails that entry ("You can only
  view this workspace…", never retried); a 404 drops it.
- `access: revoked` rows drop the doc with its bodies, search rows, to-dos and
  queued writes, unpin it and close it in every window ("This doc is no longer
  shared with you."); `granted` rows apply the state and fetch the body. Due
  alerts come only from your own lists.

To sign in: run the app, tap **Sign in**, and approve with your passkey
on the server's page. To enrol a passkey first, use an enrolment link from
`grimoire auth` on the server. To use a local daemon instead, set the server
URL to `http://127.0.0.1:7425` in Settings.

HTTPS only, end to end. Release builds carry no ATS exceptions and refuse a
non-`https` server URL (`ServerURLPolicy`; a stored one falls back to the
default). Debug builds alone use `App/Support/Info-Debug.plist` (the
generated `Taisce/Info.plist` plus a `localhost` exception and
`NSAllowsLocalNetworking`, needed for `127.0.0.1` since exception domains
can't name IP addresses) and accept `http://` to a loopback daemon. Keep the
two plists in step when `project.yml`'s `info` changes. The GRDB cache, its
WAL and SHM files and their directory are `completeUntilFirstUserAuthentication`.
Nothing logs tokens or doc content (no `Logger`/`os_log` in the app or kit).

## Due alerts (local notifications)

`DueAlertPlanner` (TaisceKit, pure) turns open to-dos into the notifications
that should be pending: timed items (`due_at` instant, or the older local
`deadline` + `due_time`) fire at their instant, all-day ones at 09:00 local
on their date (a floating calendar trigger, so it follows the device's time
zone). Identifier `todo:<day>/<item id>`, title the to-do text, body
`Due 15:00 · To-do` / `Due today · To-do`; done and past items are skipped,
soonest first, capped at iOS's 64. `DueAlertInput` adapts `TodoItem`,
`TodoDueList.Item`, cached `TodoRecord`s (server item id recomputed) and the
UTC model's fields.

`NotificationCoordinator` (app, conforms to `DueAlertPermission`) asks for
alert + sound, registers category `TODO_DUE` (Done / Snooze 1 hour /
Tomorrow 09:00), and reconciles pending requests against the plan (remove
stale, add new or changed) on foreground, after syncs touching the To-do
doc, and on a time zone change. Actions queue through the outbox
(`enqueueToggle`, `enqueueDeadline`), so they work offline; the answer is
held locally until a sync shows it, so the old alert doesn't come back.

## Push (APNs, silent)

APNs keeps the cache fresh in the background and cancels alerts for to-dos
changed elsewhere; the alerts themselves stay local. The server sends
`{"aps":{"content-available":1},"seq":N}`.

- Entitlement `aps-environment`: `Support/Taisce-Debug.entitlements`
  (development) and `Taisce-Release.entitlements` (production), picked per
  configuration by `CODE_SIGN_ENTITLEMENTS`; `UIBackgroundModes`
  `remote-notification` (in both Info plists).
- `PushRegistrar` (TaisceKit actor): after sign-in (every foreground
  `startSync`, SERVER mode only) the app calls
  `registerForRemoteNotifications`; the token (lowercase hex) goes to
  `POST /api/devices` `{token, platform: "ios", env: sandbox|production
  (#if DEBUG), app_version: "0.1.0 (1)"}` when the token, server, env or
  version changed, else at most once a day (`UserDefaultsPushStore`).
  Sign-out and a server switch `DELETE /api/devices/{token}` first (5 s
  bound, 404 = gone), then forget the record.
- `AppDelegate` (`UIApplicationDelegateAdaptor`, owns the `AppModel` so a
  background launch has one) answers a silent push with
  `BackgroundRefresh.run`: one `SyncEngine.catchUp()` raced against 20 s
  (`withTimeLimit`, which returns on time even if the work ignores
  cancellation), then `NotificationCoordinator.reconcile()`, then
  newData / noData / failed. A push whose `seq` the cursor already passed,
  or one arriving while the stream is live, only reconciles.

## Editing (groundwork, no UI)
## Editing

**Edit** on a doc (not canvases or mirrors) swaps the reading view for one
`UITextView` (TextKit 2) per block; **Done** saves and goes back. The nav bar
chip says what happened: Saved · Saving… · Offline · N pending · Doc is open
in a live session · will retry · Waiting for review (amber = applied and
flagged, rose = parked) · N edits not saved (refused; tap to retry, never dropped).

Model (TaisceKit `Edit/`):

- `InlineCodec`: inline markdown ↔ `AttributedString` with Taisce attributes
  (`taisceMarks` bold/italic/code/strike, `taisceLink`, `taisceWiki` = the
  text between `[[ ]]`, alias and `#^ref` included). `parse` returns nil for
  anything it can't hold exactly (images, HTML, hard breaks, titled links).
  `serialize` is canonical (`**`, `*`, `~~`, minimal escapes) and checks
  itself by re-parsing. Parsing uses `.disableSmartOpts` (no curly quotes).
- `EditorBlockContent`: paragraph / heading / quote / list (items with
  `ListPrefix`: depth, bullet or number, task box) / raw. A block is shown
  structured only when its canonical form parses to the same document
  (`sameMeaning`); otherwise it is edited as raw markdown source (code,
  mermaid/vega/d2, tables, callouts, frontmatter, anything unusual). An
  untouched block is never written; an edited one is written canonically.
- `EditorCommands`: Return (split, new list item, leave a list on an empty
  item, ```` ``` ```` + Return = code block), Backspace at the start (heading/quote/list item
  → text, nested item → outdent, paragraph → merge into the block above),
  shortcuts at a block's start (`# `…`###### `, `- `/`* `/`+ `, `1. `, `[ ] `, `> `),
  indent/outdent, block kinds, to-do toggle.
- `EditorSession`: the visible blocks over a `DocEditor`. Structure changes
  become one propose at once (a block after a heading becomes its first
  child; a merged-away block hands its children to its parent first).
  Text is committed on demand. `refresh` takes a server change without
  touching the focused or unsaved blocks; a block the server changed under
  you is proposed on its older epoch (the gate scores the conflict instead
  of it silently winning), one it deleted comes back as a draft insert.
- Saving: `SaveScheduler` debounces 600 ms per block; leaving a block,
  structure changes, Done and backgrounding flush. `Cache.enqueueCoalescing`
  rewrites the newest never-sent replace of the same block instead of
  queueing another. `Cache.enqueue` moves a propose's base up past our own
  landed writes (`base → base+1`, recorded in the outbox's new `outcome`
  column). The replayer claims rows atomically (`claimOutbox`).
- Review marks: `Cache.reviews(for:openOps:)` from landed outcomes (newest per
  block; green clears), filtered by the doc's open queue
  (`APIClient.openReviewOps`, `GET /api/doc/{id}/review`) when online.
- `[[` completion: `WikiCompletion.query/rank/target` (titles and
  breadcrumbs, diacritic-folded, `Folder/Title` when titles clash).
- New doc: `NewDoc.create` (`POST /api/docs` with a `request_id`; after a
  lost answer it looks for the doc before trying again, as the REST route
  doesn't dedupe creates; only MCP `create_doc` does).

App (`App/Taisce/Editor/`): `EditorText` maps content ↔ the text view's
attributed string (list markers are tagged text the caret skips);
`BlockTextView` handles Backspace at the start, atomic wikilinks, paste
(markdown into an empty paragraph becomes blocks), checkbox taps and the
hardware keys (⌘B ⌘I, ⇧⌘E inline code, ⌘K link / `[[`, Tab ⇧Tab, ↑↓ between blocks, Esc);
`FormattingBar` is the input accessory; `EditorModel` ties it together.
Queued edits also show in the reading view until they land.

UI test: `EditorUITests` seeds a doc on a LOCAL daemon
(`TEST_RUNNER_TAISCE_UI_URL`, default `http://127.0.0.1:7518`), edits it
(type, Return, Backspace-merge, `[[` pick, Done) and checks the daemon's
markdown. `LaunchTests` needs a doc titled "Welcome" headed "Welcome to Taisce".

## What's stubbed

- Editing: no rich table editing (tables are raw source), no title rename,
  no drag to reorder blocks, no soft line breaks typed (Return splits).
- Outbox idempotency: main's daemon keeps `request_id` dedupe in memory for
  120 s (server-identity makes it durable for 7 days). Client-minted insert
  ids make a doubled insert fail instead of duplicating; `replace` and
  `move` are idempotent; a doubled delete fails harmlessly.
- `propose_markdown` queues are rebased like `propose`, but a stale base is
  an error there (whole-doc diff), so they are not chained.
- Pins are local (UserDefaults, per server); pinned docs are always fetched by sync.
- D2 and canvases render as labelled cards; full Vega (not -Lite) too.
- Mac: one window (no multiple windows or tabs), no Format menu (the
  formatting bar and ⌘B ⌘I ⇧⌘E ⌘K carry it), not notarized or distributed.

## Mac (Catalyst)

The same `Taisce` target, built for `platform=macOS,variant=Mac Catalyst`.

- Idiom: **Optimize interface for Mac** (`TARGETED_DEVICE_FAMILY` `1,2,6`),
  not the scaled iPad idiom: text is native size and controls are AppKit's.
  The editor is `UITextView`, which the Mac idiom supports fully; it only
  ignores input accessory views, so on the Mac the formatting bar is pinned
  above the blocks (`EditorChrome.barIsInline`) without Hide keyboard.
- One bundle id everywhere (`DERIVE_MACCATALYST_PRODUCT_BUNDLE_IDENTIFIER`
  NO): the APNs topic and the Keychain items are `ie.null.taisce` on the Mac
  too, and the device registers as `platform: "ios"` (APNs treats a
  Catalyst token like an iOS one). Signing is automatic; the first build
  on a new Mac needs `-allowProvisioningUpdates -allowProvisioningDeviceRegistration`.
- Entitlements: `Support/Taisce-Mac-{Debug,Release}.entitlements` (picked
  by `CODE_SIGN_ENTITLEMENTS[sdk=macosx*]`) add the App Sandbox with network
  client only; hardened runtime is on. Xcode signs `aps-environment` as
  `com.apple.developer.aps-environment` too, and `PushConfig` reads the
  profile from `Contents/embedded.provisionprofile` with either key.
- The cache skips data-protection classes on the Mac (`Cache.appliesFileProtection`);
  the sandbox container and FileVault cover it there.
- Layout: regular width (every Mac window, iPad) is the split view
  (`RootLayout`): sidebar with the workspace switcher, search, Today,
  To-dos and the Library tree; the doc on the right. Compact width (iPhone)
  keeps the tab bar. Minimum window 820×560, first launch 1280×860; macOS
  restores the size after that.
- Menu bar (`TaisceCommands`, routed through the front window's `Router`):
  ⌘N new doc, ⌘, Settings, Doc › Edit / Done ⌘E and Refresh ⌘R, Go › Search
  ⌘F, Today, To-dos and the workspaces ⌘1…⌘9. Nothing is enabled before
  sign-in. ⌘E used to be inline code in the editor; that is now ⇧⌘E. iPad
  hardware keyboards get the same commands.

```sh
cd apple/App && xcodegen generate
xcodebuild -scheme Taisce -destination 'platform=macOS,variant=Mac Catalyst' \
  -allowProvisioningUpdates -allowProvisioningDeviceRegistration build
xcodebuild -scheme Taisce -destination 'platform=macOS,variant=Mac Catalyst' \
  -allowProvisioningUpdates -only-testing:TaisceTests test
```

## Charts and diagrams

**Charts** (` ```vega-lite ` fences) are drawn natively with Swift Charts; no
web view. `ChartSpec.parse` (TaisceKit `Charts/`, pure) maps the JSON to a
`ChartModel`; `ChartBlock` (app `Diagrams/`) renders it.

- Marks: bar, line, area, point, arc (pie; donut with `innerRadius`), rule,
  tick. Encodings: x, y, color, theta, size, xOffset (grouped bars), `datum`.
  Field types quantitative / temporal / nominal / ordinal (inferred when
  absent: numbers → quantitative, ISO dates → temporal, else nominal).
  `aggregate` sum / mean / count / min / max / median; `sort` (default
  ascending, `null`, `descending`, `"-y"`, an explicit list, `{op, order}`);
  `stack` (`normalize`, `center`, `null`); `title`, axis titles; `layer`
  (line + point, bar + mean rule); `"point": true`; `interpolate`; `height`
  (clamped 220-320 pt).
- Data: inline `data.values`, or `{"block": "^abc123"}` / `{"table": "^abc123"}`
  naming a GFM table in the same doc (the last six hex digits of its block
  id, as `read_doc refs: true` shows them). The header row is the fields;
  cells coerce to numbers (`1,234`, `12%`, `$5`) and ISO dates, so editing
  the table redraws the chart. `data.url` and named datasets show a note.
- Anything else (transforms, facets, concat, other marks) is a "Chart type
  not supported on iPhone yet" card naming the mark; invalid JSON is a card
  with the error and the offending line.
- Theme: the accent first, then green, amber, rose, teal, violet, tan, grey
  (each a dark/light pair). A temporal or categorical x with more than 24
  values scrolls horizontally (16 visible). Swift Charts supplies audio
  graphs; each mark carries a label and value, the chart a summary.

**Mermaid** (and reladraw, below) renders on the device with the bundled mermaid **12.0.0**
(`App/Taisce/Resources/mermaid.min.js`, MIT, `mermaid.LICENSE.txt`):

- Provenance: `dist/mermaid.min.js` from `https://registry.npmjs.org/mermaid/-/mermaid-12.0.0.tgz`;
  the tarball matches the registry's `dist.integrity`
  (`sha512-/wQXC9iBxoGV8p3erbvaXs9h77VyLDBH6GdayVjj3hEcSQhFU4N1WUhUppotCEqlIxI2pRMwjwBSwTB1MfZBgQ==`),
  and the file is byte-identical to jsdelivr's
  `mermaid@12.0.0/dist/mermaid.min.js`. sha256 of the bundled file:
  `28fca7ae6ebc7ed7bb63bde63136a74bfef14f296a57e403657eeb8b32836073`.
  To upgrade: fetch the new tarball, check its integrity, copy the file,
  bump `DiagramCacheKey.renderer` and this paragraph.
- One shared offscreen, non-persistent `WKWebView` (`DiagramWebView`),
  loaded with `loadHTMLString` at base URL `taisce-diagram://bundle/`, a
  `WKURLSchemeHandler` that serves only `.js` files from the app bundle
  (reladraw's ES modules need a real origin to import each other). CSP
  `default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; img-src data:`
  (no network of any kind); every navigation after the first is refused;
  `securityLevel: 'strict'`; `htmlLabels: false`. Theme follows the trait
  collection (`dark` / `default`). The page rasterises the SVG on a canvas
  at the device scale (capped at 16 Mpx) and returns PNG.
- **reladraw** (` ```reladraw ` fences) goes through the same web view and
  queue: `compile(source, {theme: THEMES.dark | THEMES.light})` from
  reladraw **0.15.1** (Apache-2.0, `LICENSE.txt` and `NOTICE.txt` beside it),
  imported as modules from `Resources/reladraw/` (a folder reference in
  `project.yml`). Its `SourceError` shows as `line N: message` on the error
  card. Provenance: `dist/*.js` except `cli.js` from
  `https://registry.npmjs.org/reladraw/-/reladraw-0.15.1.tgz`, verbatim; the
  tarball matches the registry's `dist.integrity`
  (`sha512-2Kn3Ojm4OkQBLm+i/mFJ17uqNtR08EhfnTEhTdPiRDppbNymyUHJ9ZCsphJoH93gGQoTud5h/H8+eONEBHF3Pw==`).
  Per-file sha256 in `Resources/reladraw/SHA256SUMS` (that file's sha256:
  `49dbaa4a6eb9f89c6ed14a061e4c0bb90313b3362b39db385c46f8e268db1c45`;
  `index.js`: `8c13ea18e9c5fd95b94b61dac8b0f3ec284cc81218ddb6aa67166d082fd7596b`).
  To upgrade: same steps, regenerate `SHA256SUMS`, bump `DiagramCacheKey.reladraw`.
- `DiagramRenderQueue` (TaisceKit actor) runs one render at a time, joins
  identical requests, caches PNGs in `Caches/diagrams/<sha256(renderer +
  theme + width + source)>.png` (renderer = mermaid-12.0.0 or reladraw-0.15.1), and fails a render after 5 s (the queue
  moves on). Widths are bucketed to 20 pt.
- Inline: a shimmer while rendering, then the image at its aspect (never
  upscaled); tap opens full screen with pinch-zoom, pan and double-tap.
  Errors (mermaid's parse message, the timeout) show as a card.

## Running the app against a scratch daemon

Launch arguments (UserDefaults' argument domain) make screenshots and UI
runs reproducible: `-serverURL http://127.0.0.1:7517`, `-tab
today|library|todos|search`, `-openDoc "<title>"`, `-searchQuery <text>`,
`-showSettings YES`. For example
`xcrun simctl launch booted ie.null.taisce -serverURL http://127.0.0.1:7517 -openDoc "Roadmap"`.
Start the daemon with `HOME=<tmp>` so it does not import this machine's
Claude memory files. Tests: `xcodebuild -scheme Taisce -destination 'platform=iOS Simulator,name=iPhone 18 Pro' test`.
