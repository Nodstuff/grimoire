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
      Run/                     runnable code blocks: FenceInfo, GoProgram, RunTrust, output, posix_spawn runner (Mac)
      SQL/                     SQL blocks: SQLDriver, SQLite and ClickHouse drivers, results, data sources
      Migration/               SandboxMigration (the Mac's old App Sandbox container → ~/Library)
      Todo/                    Deadline (all-day | UTC instant), TodoClock, offline To-do parser
      Library/                 doc tree for the sidebar, wikilink lookups
    Sources/TaisceSQLPostgres/ SQL blocks' Postgres driver (PostgresNIO; the app links it on the Mac only)
    Tests/TaisceKitTests/    Swift Testing + a URLProtocol mock server
  App/                         the iOS app (xcodegen)
    project.yml
    Taisce/                    AppModel, Router, Theme (Lapis tokens, type, appearance, haptics),
                               Components (Lapis components), Presentation (pure view models),
                               Views (app views, block renderer), Screens
    TaisceTests/               view-model tests (Swift Testing, hosted in the app)
```

## Design: Lapis

The app speaks Lapis, the design language the web shares
(`ui/src/lapis/tokens.css`, canvas artboard Lapis-System). Screens never
name a hex or a size; they use the foundation in `Taisce/Theme/` and
`Taisce/Components/`:

- Colour: `Color.lapis.*` / `.foregroundStyle(.lapis.ink2)` / `UIColor.lapis.*`
  (`LapisColors.swift`): grounds and cards, three inks, lines, the lapis
  accent, agent violet, verdict green/yellow/red with soft fills and
  on-colours, workspace colours, shadows. Each is a dynamic `UIColor`
  (system appearance, Settings › Appearance, Increase Contrast). Three
  native-only fills (`greenFill`, `yellowFill`, `redFill`) carry white text
  at AA (swipe actions).
- Appearance: Settings › Appearance (System / Light / Dark), stored under
  `UserSettings.appearanceKey` (a device preference: it survives sign-out),
  applied as the window's `overrideUserInterfaceStyle` (`.lapisAppearance()`
  on the root), so sheets, the editor and Mac Catalyst windows follow.
- Type (`LapisType.swift`): docs, titles and covers in **Newsreader**; the
  interface in SF Pro's semantic styles; agent names and code in SF Mono.
  `.lapisFont(.docTitle)`, `.docBody`, `.docHeading1…3`, `.cardTitle`,
  `.screenTitle`, `.display`, `.coverTitle`; serif styles scale with Dynamic
  Type on iOS and with `docScale` on the Mac (Catalyst answers text styles at
  `.large`). The editor uses the same scale (`SurfaceLook`).
- Newsreader is bundled as two variable fonts (`Resources/Fonts/`,
  `UIAppFonts` in both Info plists), `opsz` following the point size.
  Provenance: `ofl/newsreader/` in google/fonts at `8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5`
  (upstream productiontype/Newsreader), SIL OFL 1.1 (`Newsreader.OFL.txt`).
  `Newsreader-VF.ttf` = `Newsreader[opsz,wght].ttf`, sha256
  `8a08d13f8a6c0d51be379a60af84f945f65369a67e509ee3c3bdcc421254d7c1`;
  `Newsreader-Italic-VF.ttf` = `Newsreader-Italic[opsz,wght].ttf`, sha256
  `796668611f80b64d5adf182fde3b6f29ed83b4e7cbec7b96937e84ac01364792`.
- Metrics: `Lapis.Space`, `Lapis.Radius`, `Lapis.Layout` (44 pt targets,
  680 pt reading width), `Lapis.Motion`, `.lapisShadow()`, `.card()`.
- Components (each with light and dark previews): `VerdictChip`,
  `SaveStateDot`, `LapisAvatar` / `AgentBadge`, `StatusChip`, `TagChip`,
  `FilterChip`, `CountPill`, `DeadlinePill`, `.buttonStyle(.lapis(…))`
  (primary, secondary, ghost, destructive, keep, revert), `LapisSection`,
  `LapisCard`, `EmptyState`, `EmptyCard`, `InlineChangeCard` (+ `WordDiff`),
  `.lapisToast`, `LapisBanner`, `SkeletonBlock` / `SkeletonLines` /
  `.shimmering()`, `ProgressBar`, `BookCover` (the web's typographic covers:
  same 12 palettes, same FNV-1a hash). `Haptics` for imperative feedback.
- Tests: `LapisTests` (tokens against the web, contrast, the font's axes and
  scaling, word diff, cover hashes against the web's).

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

Dependencies (SPM only): GRDB.swift 7.11.1, swift-markdown 0.9.0, Valet 5.1.1, and on the Mac
postgres-nio 1.33.1 (SwiftNIO, NIOSSL, swift-crypto, swift-log; SQL blocks) (swift-markdown pulls
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
a 403 with the read-only message, a 404 with "This doc no longer exists…", both kept as failed; only a revoked row drops writes);
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
  (`EditAccess`) gets a read-only surface, no new doc, checkboxes, to-do add/done/snooze
  or move; an editor can't move docs out of someone else's workspace; Manage
  workspaces edits only your own. A 403 on replay fails that entry ("You can only
  view this workspace…", never retried); a 404 fails it too, kept, so the text is recoverable. Sign-out tries one last replay and asks before losing unsent changes.
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

## Library tree

The Library (iPhone tab) and the sidebar's Library section (iPad, Mac) are
`LibraryTree` (`Views/LibraryTree.swift`): the tree drawn row by row in a
plain `ScrollView`, not a `List`. List rows never receive drops on iOS or
Mac Catalyst (a row could be lifted and put nowhere), so the tree owns its
rows: selection and hover highlight, a chevron strip (the row's leading
edge) that opens and closes a folder, a click or tap elsewhere opens the
doc, arrow keys in the sidebar (after a click: up/down move, right opens or
enters, left closes or goes to the parent), VoiceOver rows (one element
each, Expand/Collapse on folders). A folder is any doc with children.
Context menu: Pin, New doc here, **New folder here**, **Move to…**, Move to
workspace… (iPhone), Delete.

**New folder** (the folder button by the Library header on iPad and the Mac,
next to + on iPhone; File › New Folder, ⇧⌘N; New folder here on a row): a
name, then an empty doc made there, not opened, shown and scrolled to. A
doc never written in (`current_epoch` 0) is a folder before anything is in
it (`DocInfo.isEmpty`, `LibraryNode.isFolder`; the web's `isEmptyFolder`
in `ui/src/tree.ts` is the same rule), so every client shows it the same
way with no server change. Opening it shows an empty page (nothing is
written unless you type); open in the tree it says "Empty — drag docs
here", and a drop on that line goes in.

Moving a doc (`POST /api/doc/{id}/move` `{parent_id, sort_key}`):

- **Drag** a row (iPhone: press, then move) onto another. Each row has three
  zones: its top quarter puts the doc just **before** it, the middle
  **inside** it (the last child; the folder opens), the bottom quarter just
  **after** it; on an open folder the bottom edge means first inside it. An
  insertion line or an outlined row shows where; a place it can't go shows
  nothing and refuses the drop. A closed folder opens after a drag rests on
  it for 0.7 s; the space under the last row and the Library header are the
  top level; a drag resting near the top or bottom edge (36 pt, after
  0.4 s) scrolls the tree (`TreeScroller`). A drag carries the doc id as `UTType.taisceDoc`
  (`ie.null.taisce.doc-reference`, declared in `UTExportedTypeDeclarations`
  in `project.yml` and `Support/Info-Debug.plist`; data, this app only), so
  the editor and other apps never take a row.
- **Move to…** (`MoveDocSheet`): a searchable list of the docs in the same
  workspace it can go under, plus Top level; the non-drag path. Sheets shown
  from the sidebar get `AppModel` passed by hand (`.environment(model)`):
  without it, Move to… trapped on the Mac.
- Rules (`DocMoves`, pure): never into itself or its own subtree, only within
  its own workspace (a workspace change is Move to workspace…), never in a
  workspace you only view (`EditAccess.canReparent`: owner or editor; LOCAL
  mode and Unsorted are yours). The top level is an owner's call
  (`canMove`). Before/after (`DocMoves.place`) is a reorder among siblings,
  otherwise the same rules; never beside a doc with no order key (it sorts
  last and bounds nothing). A move to the top level labels a doc whose
  workspace comes from its parent first (a queued `PUT
  /api/docs/{id}/workspace`), undone if the move is then refused.
- Offline first: `Cache.enqueueDocMove` queues the write and places the
  cached row at once (an append's key the server picks when it lands). A
  move that just landed keeps showing for `Cache.landedMoveGrace` (30 s)
  wherever a tree row still has the doc elsewhere: a tree fetched just before
  it landed can arrive just after, and put the doc back (it looked as if the
  move hadn't taken, and Tom moved docs twice). A
  refusal puts the doc back and says why ("Couldn't move" alert this
  session, the top banner for an older one).
- UI tests: `MoveDocUITests` drive real drags against a LOCAL daemon
  (`TEST_RUNNER_TAISCE_UI_URL`): a click-drag on the Mac
  (`click(forDuration:thenDragTo:)`; a touch-style press starts nothing
  there), press-and-move on iPhone (held still, the long-press shows the
  menu). Mac UI runs need Automation Mode
  (`sudo automationmodetool enable-automationmode-without-authentication`).

## Editing

A doc is ONE always-live text surface, as on the web (Tiptap) and in any doc
app: no Edit/Done, no ⌘E, no visible blocks. Opening a doc you may edit puts
you in it (a click places the caret; a doc just made here opens with the caret
at its start). Viewers (ADR 0004), canvases, mirrors get the same surface,
not editable, links tap-to-follow. Made a viewer mid-edit: what's typed is
flushed to the outbox, then the surface turns read-only in place. Until the
body is in the offline cache the screen shows loading / error / folder
(`DocContent`). The nav bar chip says what happened: Saved · Saving… · Offline
· N pending · Doc is open in a live session · will retry · Waiting for review
· N edits not saved (refused; tap to retry, never dropped).

Model (TaisceKit `Edit/`):

- `Surface`: the doc as one attributed string. One line per paragraph,
  heading, quote, list item or non-text block, each ended by `\n` (the string
  always ends with one); a soft break inside a block is U+2028; a non-text
  block (code, diagrams, tables, callouts, anything `EditorBlockContent` keeps
  raw) is one U+FFFC carrying its markdown (`taisce.raw`). Every character
  carries hidden `taisce.block` (the block id) and `taisce.line` (the line's
  kind). A line's identity is its terminator's attributes, so typing anywhere,
  even into an empty line, can't change which block it is. Edits that join or
  split lines are made by `Surface` (never by UIKit): Return and Backspace at
  a line's start (via `EditorCommands`), deletions and typing across lines,
  paste, cut. A split's first half keeps the id and the new half gets a fresh
  one; a join keeps the first line's id. New blocks get a client-minted id at
  once (an id the baseline lacks saves as an insert with that id). `normalize`
  restores the invariants after any change (final `\n`, U+FFFC alone on its
  line, one id per line, markers numbered); scoped to the typed lines while
  typing. If it ever has to mint an id (a duplicate, a line without one) it
  asserts in debug builds: every edit is meant to keep ids whole, and every
  `SurfaceTests` sequence checks it.
- `DocDiff`: a port of `ui/src/editor/diff.ts` (`computeOps`, `keyBetween`,
  `inferBlockType`, `rebuildBaseline`). Parents come from heading levels and
  order keys are per-sibling base-36 fractions, exactly as on the web: the
  shared vectors in `ui/src/editor/diff.vectors.json` are read by both
  `DocDiffTests` and `diff.vectors.test.ts`. A save moves a block only when
  the edit changed where it belongs (its heading section before vs after)
  and the server had nested it by that section: nesting the server holds
  that disagrees with the headings (flat, misnested) is left alone, so a
  typo fix or a heading level change never re-parents untouched blocks; a
  new block lands beside its neighbour above. The server tree after a save always reads in the
  editor's order (a block whose kept parent would put it out of place goes
  to its heading-rule parent); every vector and 2000 random edits check it.
- `SurfaceMerge`: a sync refresh while the doc is open. Untouched blocks take
  the server's text; a block changed on both sides stays ours and is in
  conflict on the older epoch (`SurfaceConflict`); one the server deleted but
  we edited comes back as an insert under a fresh id; one we deleted but
  they edited goes out as a delete on the older epoch; new and reordered
  remote blocks land in place. The app rebuilds only the blocks that differ,
  keeping the caret (block + offset) and the scroll position. Our edits the
  gate parked (red, `ParkedEdits`, only from a review queue the server gave
  us) stay on screen as ours, in conflict on their propose's epoch so later
  saves stay flagged, until a human decides; then the overlay and the
  conflict go.
- `InlineCodec` (inline markdown ↔ `AttributedString`: marks, links,
  `[[wikilinks]]`; canonical serialisation), `EditorBlockContent` (paragraph /
  heading / quote / list / raw; a block is structured only when its canonical
  form means the same; an untouched block is never written),
  `EditorCommands` (Return, Backspace at the start, shortcuts `# `…, `- `,
  `1. `, `[ ] `, `> `, ```` ``` ```` + Return, indent/outdent, kinds, to-do),
  `WikiCompletion`, `SaveScheduler`, `NewDoc` are unchanged.

Saving: `SaveScheduler` debounces 800 ms after the last change; leaving the
doc, backgrounding and a demotion flush. A save diffs the surface's entries
against the baseline (`DocDiff`) and queues one propose on the session's base
epoch; a single `replace` goes through `Cache.enqueueCoalescing` (typing in one
block makes one write). Ops on a block in conflict go in their own propose on
its older epoch, flagged (`Cache.enqueue(_:conflict:)`), so the gate scores
it. One disputed block has at most one pending flagged write: autosave sends
the first (so the text is safe in the outbox, offline too), later saves
rewrite that row in place while it waits (`Cache.rewriteUnsentPropose`);
once it has gone out, more typing in the block waits for leaving the doc
(or backgrounding, or demotion), which sends it once, and the same text is
never proposed twice (a refresh moving the block's baseline is no reason).
A conflict ends only on the server's own text (its cached body, never our
queued write overlaid) or on Keep mine / Take theirs; Keep mine drops the
pending flagged write and writes ours on the current epoch. Blocks the
surface leaves out (frontmatter, comments) keep their place: a new first
block goes after the frontmatter. The saved entries become the new baseline. `Cache.enqueue` still lifts a
propose's base past our own landed writes; queued-but-unwritten proposes are
overlaid on a refresh so nothing saved rolls back on screen. Review marks:
`Cache.reviews(for:openOps:)` tints the block's text (amber flagged, rose
parked / refused / in conflict); notes above the text offer Keep mine / Take
theirs and Retry / Discard.

App (`App/Taisce/Editor/`): `SurfaceModel` owns the one `SurfaceTextView`
(TextKit 2, scrolls itself, header and children list as subviews, own undo
stack) and is its delegate: plain typing in a line is UIKit's; Return,
Backspace at a line's start, cross-line edits, wikilink edits (atomic, the
caret never inside one), deletions touching a non-text block, paste, cut and
copy (markdown, attachments as source) are `Surface` edits, each one undoable
step. `SurfaceLook` draws the model attributes (fonts, colours, list markers,
review tints, links). Non-text blocks are `BlockAttachment`s whose view
provider hosts the reading view's renderer (`NodeStack`: Run on code, charts,
mermaid / vega / reladraw, tables, callouts); tapping one (or its pencil)
opens `SourceEditorSheet` on its markdown; saving it replaces that block
(text again if it now is text). Whole blocks cut and pasted back unchanged
in the same doc keep their ids (a move, so comments and `^refs` survive).
Undo is the surface's own stack (UIKit doesn't record typing on it): typing
groups into runs, each command is one step; a refresh keeps the steps that
don't touch the refreshed blocks (the first that does, and older ones, go;
redo clears); demotion to viewer clears it. Drops from other apps go through
`Surface` (a text drop delegate, anchored to its block while the items
load); Writing Tools is off on the surface (nothing shows its rewrites go
through the delegate, and one that didn't would lose block ids).
Checkbox taps tick the box (the To-do doc's
go through the to-do API). Links: a tap places the caret; ⌘-click (Mac) and
the edit menu (long-press on iOS, right-click on the Mac: "Open …") follow
`[[wikilinks]]` and URLs. Keys: ⌘B ⌘I, ⇧⌘E inline code, ⌘K link / `[[`,
Tab ⇧Tab, Esc; `FormattingBar` rides the keyboard (inline on the Mac).

UI test: `EditorUITests` seeds a doc on a LOCAL daemon
(`TEST_RUNNER_TAISCE_UI_URL`, default `http://127.0.0.1:7518`), edits it in
the surface (type, Return, Backspace-merge, `[[` pick) and checks the daemon's
markdown after autosave. `LaunchTests` needs a doc titled "Welcome" headed
"Welcome to Taisce".

## Docs screens (Lapis)

- Library (iPhone): LIBRARY over the workspace name, which is the workspace
  switcher (doc counts, who shares it), a count line (`38 docs · only you`,
  `shared by Aoife · view only`), a title filter (every word; hits show
  where they live), the tree in one card (48 pt rows, counts on folders,
  short edited times on docs, flag dots). New folder / New doc in the bar.
  iPad sidebar rows are 40 pt, the Mac's 28 pt.
- Doc: status and tag pills from the frontmatter, the serif title, a byline
  (newest three authors, `You` for you, version, edited). Body text is
  Newsreader at 19 pt on iPhone and 20 pt on a regular-width iPad
  (`SurfaceLook.bodyFactor`; the Mac follows `docScale`). Headings are SF
  Pro bold, wikilinks the accent with a soft rule, inline code a rounded,
  outlined pill and checkboxes one drawn box, ticked or not (both drawn by
  `SurfaceLayoutFragment` / `CheckboxLayoutFragment`, TextKit 2).
- Inline change cards: each open review item on the doc (flagged: Keep /
  Revert; parked: Apply / Discard, a parked delete asks twice) covers its
  block on the live surface. The covered block takes no typing until it is
  decided; moves and blocks no longer in the doc are listed in the
  inspector only. Old words struck in red, new ones underlined in green
  (`WordDiff`). In a wide column the buttons sit in the card's header row
  when they fit, else under the text.
- Inspector (Review, Comments, History, About): a sheet on iPhone, a column
  on iPad and the Mac (the doc's bar items sit after `.inspector`, so they
  stay in the doc's own bar). History groups a run of edits by one author.
- iPhone hides the tab bar while you type (the glass keyboard would show it
  through). Find in Doc is ⌘F (find only); Search is ⇧⌘F.

## What's stubbed

- Editing: no rich table editing (tables are raw source), no title rename,
  no drag to reorder blocks, no soft line breaks typed (Return splits); an
  input method's composition clears the undo history.
- Editor follow-ups: the random diff sweep has no "paragraph as parent"
  mode; the server-order check treats a parent that isn't on the surface
  (a comment, frontmatter) as the top level; a failed review-queue fetch
  keeps the last queue (it can be stale until the next good fetch); pasting
  "x\n" into text leaves an empty paragraph after it; a parked block's
  conflict has no `theirs`, so it shows no Keep mine / Take theirs until a
  reviewer decides.
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
  by `CODE_SIGN_ENTITLEMENTS[sdk=macosx*]`): push (and Associated Domains in
  Release), **no App Sandbox** (runnable code blocks run your own tools on
  your own files); hardened runtime is on. Xcode signs `aps-environment` as
  `com.apple.developer.aps-environment` too, and `PushConfig` reads the
  profile from `Contents/embedded.provisionprofile` with either key.
- Files (`AppPaths`): unsandboxed, `~/Library/Application Support` and
  `~/Library/Caches` are shared by every app, so the Mac keeps its caches in
  `Application Support/ie.null.taisce/` and diagrams in
  `Caches/ie.null.taisce/diagrams/`; preferences are
  `~/Library/Preferences/ie.null.taisce.plist`. A test host (XCTest inside
  the app) never boots the model, never migrates, and keeps its caches in a
  temp folder, so a Mac test run doesn't touch the real app's data (its
  `UserDefaults.standard` is still the app's domain: tests clean up the keys they set).
- The cache skips data-protection classes on the Mac (`Cache.appliesFileProtection`);
  FileVault covers it there.

**Leaving the sandbox** (`SandboxMigration`, run by `AppDelegate.init` before
the model reads a preference): the sandboxed builds kept everything in
`~/Library/Containers/ie.null.taisce/Data/Library/`. On the first unsandboxed
launch the caches (`cache-*.sqlite` with `-wal`/`-shm`: docs, bodies, to-dos,
cursor and the outbox of unsent writes) are staged beside the new folder and
moved in database-last (a failed move takes back what it moved), and the
container's preferences are merged in key by key (a key already set keeps
its value). A cache already at the destination is kept if the migration put
it there (`migration.sandboxContainer.created`), set aside under
`.replaced-<time>/` and replaced if it has no owner and no queued writes, kept
if the container's copy has nothing unsent, and otherwise left with an error:
the pass is never marked done while the container holds writes that weren't
copied. A destination byte-identical (size and SHA-256) to the staged copy
counts as the migration's own (a crash before it was recorded, an older
build's copy). When both really have data, the blocked screen offers "Use the
copy already here (the old one stays untouched)", which logs the old copy's
unsent count, or "Use the old copy, with its unsent changes", which sets this
one aside under `.replaced-<time>/`; either way the migration runs again and
the app opens. While a pass is incomplete (unreadable container, a failed copy, that
conflict) the app opens no cache at all and shows "Quit and open Taisce from
Finder to finish moving your data" (`MigrationBlockedView`), so it can never
create the empty cache a later pass would mistake for data. The container is
never changed or deleted; `Caches/diagrams` is not copied (re-rendered). A
marker key (`migration.sandboxContainer.v1`) makes later launches return at
once; each pass is logged (file and key names, never values) to
`Application Support/ie.null.taisce/migration.log`.

- macOS app-data protection: a container is readable only by the app it
  belongs to, and only when that app is its own "responsible process" —
  launched by Launch Services (Finder, Dock, `open`). Started from a shell
  (or anything that makes the shell responsible) the reads fail with EPERM:
  the pass is logged as incomplete and retried at the next launch, never
  marked done. Verified 2026-10-02 with a throwaway bundle id
  (`ie.null.taisce.migtest`): a sandboxed build wrote a file, a preference and
  a data-protection Keychain item; the same bundle id rebuilt unsandboxed
  read all three when opened with `open`, and none of the files when its
  binary was run from a shell. Then the real app, built unsandboxed as
  `ie.null.taisce.migtest` and opened with `open`, migrated a seeded
  container (a cache with two pending outbox rows, three preferences):
  both rows and every key arrived, and a second launch did nothing.
- The Keychain doesn't move: Valet uses the data-protection keychain with
  the default access group, `6UP35L9425.ie.null.taisce` (the
  `application-identifier` from the provisioning profile), which the
  sandbox doesn't change, so the sign-in tokens stay readable. A build
  signed without that profile (e.g. Developer ID, no `application-identifier`)
  can't see them.

**Runnable code blocks** (Mac only; the iPhone shows the plain card). In
read mode, ```` ```go ````, `bash`, `sh` and `zsh` fences get ▶ Run (⌘↩ while
the try line or practice text has focus), Stop, and the output beneath:
stdout and stderr interleaved in arrival order, stderr rose, the exit code
and duration, "compiled ✓" for a Go block that only compiles.

- Isolation: every run is its own process group (`posix_spawn` with
  `POSIX_SPAWN_SETPGROUP`; Foundation's `Process` isn't on Catalyst) in
  `$TMPDIR/taisce-run/<uuid>`, deleted before the run reports done; stdin is
  /dev/null. Nothing is shared between runs. When the leader exits, the rest
  of its group is always sent SIGTERM (SIGKILL 2 s later) and the run
  reports done only once the group is gone, so a backgrounded child never
  outlives it: `nohup … &`, `caffeinate &`, `ssh -f` and the like end with
  the block. To leave something running, hand it to something that
  outlives the run: `launchctl` / `brew services`, or a terminal. A program
  that leaves its group (`setsid`, a double fork into a new group) is out of
  reach, as for any process group.
- Environment: your login shell's (`$SHELL -l -c 'env -0'` from a minimal
  seed, once per launch, 5 s bound, else the app's own). cwd is the run's
  temp dir, or the fence's `cwd=` (```` ```bash cwd=~/code/portus ````; `~`
  expanded). `FenceInfo` splits the info string (cmark hands over the whole
  string as the "language"); `RenderNode.code` carries `attributes`.
- Go (`GoProgram`, pure): `package main` + `func main` runs as written;
  top-level declarations get `package main` and a `main` that prints the
  try line (`fmt.Println(<expr>)`, so `pairSum(xs, 9)` prints `1 3 true`;
  empty = compile only); bare statements are wrapped in `func main()` with
  any func/type declarations hoisted. Imports: `goimports` when it's on your
  PATH, else standard-library packages inferred from qualified names (a fixed
  table; names the block declares are never imported), and an "imported and
  not used" build error drops that import and builds again (wrapped shapes).
  Built with a minimal `go.mod` (`module run`, the toolchain's version) as
  `go build`, then the binary runs; the build cache is Go's usual one.
- Limits: 5 minutes, then (or on Stop) SIGTERM to the whole group and
  SIGKILL 2 s later; 1 MB of output per run, then "(output truncated)";
  ANSI escapes stripped; output reaches the main actor in ~50 ms batches and
  only the last 64 K characters are laid out (Copy output has all of it).
- Edit to try: the code becomes editable in place (monospaced, no edit
  mode); Run uses it; Revert goes back; Save to doc replaces the fence body
  through the outbox like an editor save (the review gate applies). Practice
  text and try lines last for the session.
- Trust (`RunTrust`, pure): the doc's ledger (`/api/doc/{id}/history`
  carries each op's principal, target block, epoch, `source_refs`, content,
  and `principal_is_yours`, which the server works out for the caller) says
  who last wrote the block's content (insert/replace). **You** are the
  signed-in human, or one of your own agents (`principal_is_yours`, from ADR
  0004 `owner_user`, never a name) in a workspace only you can see (not
  shared; Unsorted counts as yours alone). So Claude writing code at your
  request in your own workspace runs without a question; in a shared
  workspace every agent asks, your own included, and other people and their
  agents always ask. An older server (no `principal_is_yours`) or unknown
  sharing asks. The caveat: a prompt-injected edit by your own agent in a
  private workspace runs without a prompt; the block's history still shows
  which agent wrote it. Provenance is read only on your own (the human's)
  ops: a decline's revert (`review:decline:`) is unknown; a rename's link
  rewrite (`rename:`) that changed nothing outside `[[…]]` looks to the
  write before it (unknown if that isn't in the history), one that changed
  more is an ordinary write of yours; the same tags on anyone else's op mean
  nothing. The server refuses those prefixes (and its other reserved ones)
  from callers; of `review:` only `review:decline:` is reserved (the only
  form the server writes), so any other `review:…` ref is a caller's note
  and the write is theirs. A write of yours repeating content someone else wrote
  earlier (a whole-doc save re-inserting their block) is theirs; that look
  back covers only the history the server returns (the newest 100 ops). Run shows "Checking who wrote this…" while the ledger
  and profile load (5 s at most; then, or on an error, it asks and says
  why). Yours (`/api/profile` principal id)
  runs; anyone else's (an agent, another person), or unknown (offline,
  history hidden, older than the last 100 ops) first shows the code with
  "Last edited by <name>. Run it on this Mac?". A yes is remembered per doc
  on this device (`RunApprovals`, per server, forgotten at sign-out) until
  someone other than you changes the doc: the newest op by others must be
  no newer than at approval (offline: the doc's epoch must not have moved).
  Practice text you typed never asks; untouched practice text follows the
  doc's code (and its trust), so a Run never executes an older version.
  The sheet says the code runs as you with your login environment, tokens
  included. No server change was needed.
- "written by <principal>" sits quietly beside Run (read mode) whenever
  someone other than the signed-in human last wrote the block, your own
  agents included even where they run without asking (`RunTrust.writtenBy`,
  same rules as the trust check). Nothing for your own writes or when it
  can't tell. It reads `CodeRunStore`'s cached ledger: one
  `/api/doc/{id}/history` fetch per doc and epoch, shared by every block in
  the doc and refreshed by each Run's trust check; never one per render.
- **SQL blocks** (Mac only): ```` ```sql db=<name> ```` (also `sqlite`,
  `postgres`/`postgresql`, `clickhouse`, which only accept a source of that
  kind) runs on a data source defined in Settings › SQL blocks › Data
  sources: name (what `db=` says, unique, letters/digits/`-_.`), kind and
  connection (SQLite file; Postgres host, port, database, user, TLS mode;
  ClickHouse HTTP URL, database, user) and **Allow writes** (off by
  default). TLS defaults to verify-full; Prefer (which can fall back to
  plain text) is refused unless the host is this Mac (localhost,
  127.0.0.0/8, ::1), and ClickHouse needs https:// unless the host is this
  Mac; Off, Require (unverified) and a local http:// are allowed, and the
  editor says plainly what they give up. The list is `datasources.json` beside the cache; passwords are
  in the Keychain (Valet identifier `ie.null.taisce.datasource`, account =
  source id). Never synced, never sent to the server. No `db=`: Run is a
  menu of the sources, and after a run "Save db=<name> to doc" writes it
  into the fence through the outbox. An unknown name says so, with Add….
  Read-only is the database's: SQLite opens the file `SQLITE_OPEN_READONLY`
  (and every SQLite connection, writable or not, allows no `ATTACH`, so no
  `VACUUM INTO` either, and runs in defensive mode), a read-only Postgres
  run is one `BEGIN TRANSACTION READ ONLY`, always rolled back, checked
  after every statement to be the same transaction (`now()`) and still
  read-only, else rolled back and stopped (so `COMMIT; BEGIN READ WRITE`,
  `SET default_transaction_read_only=off` or a `DO` block's `COMMIT` write
  nothing). Use a SELECT-only role for a hard boundary all the same: a
  read-only transaction doesn't stop `COPY … TO PROGRAM`, which needs a
  role without superuser or `pg_execute_server_program`. ClickHouse gets `readonly=1` (writes and table functions
  such as `url()`, `s3()`, `remote()`, `file()` refused, no setting can be
  changed; 1 rather than 2, which still runs table functions, so a query's
  own `SETTINGS` clause is refused too).
  Statements run in order and stop at the first error; the last one's
  rows show as a table (types where known, NULL dimmed, 1000 rows kept;
  reading stops at the 1001st and the status says "1,000+ rows": SQLite
  stops stepping, ClickHouse gets `max_result_rows=1001` with
  `result_overflow_mode=break` and the response is dropped, a read-only
  Postgres run cancels the statement (rolled back to a savepoint so the
  run goes on), and with Allow writes Postgres discards the rest unread so
  a capped `INSERT … RETURNING` still finishes); a cell over 64 KB is cut
  with a marker (`SQLCell`; SQLite text is read by its length, so a NUL
  inside doesn't end it), Copy as TSV / Markdown. Same 5 min limit; Stop
  cancels (SQLite `sqlite3_interrupt`, Postgres `pg_cancel_backend` from a
  second connection, ClickHouse `KILL QUERY` by `query_id`). Trust is the
  RunTrust rule above, and a source with Allow writes asks before every
  run. Drivers live in TaisceKit `SQL/` behind `SQLDriver`; Postgres is
  PostgresNIO 1.33.1 in its own product `TaisceSQLPostgres`, linked on Mac
  Catalyst only (the iPhone app has no NIO). Server tests:
  `eval "$(apple/scripts/sql-servers.sh)" && (cd apple/TaisceKit && swift test --filter ServerTests)`
  (Apple `container` or Docker; skipped without the variables).
- Layout: regular width (every Mac window, iPad) is the split view
  (`RootLayout`): a sidebar on the Lapis `panel` ground with Today, To-dos
  (open count, the same number the To-dos screen lists), Review (the
  queue's count in yellow), Books and Search, then the workspace switcher
  and the Library tree; the doc on the right. Compact width (iPhone) keeps
  the tab bar. Minimum window 820×560, first launch 1280×860; macOS
  restores the size after that.
- Menu bar (`TaisceCommands`, routed through the front window's `Router`):
  ⌘N new doc, ⌘, Settings, Doc › Refresh ⌘R and Share Link…, Go › Search
  ⌘F, Today, To-dos and the workspaces ⌘1…⌘9. Nothing is enabled before
  sign-in. Docs are always live (no Edit / Done); inline code is ⇧⌘E. iPad
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
- Theme: Lapis, the accent first, then green, yellow, red, teal, violet,
  orange, grey (each token a light/dark pair). A temporal or categorical x with more than 24
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

## Share links and PDF export (SERVER mode)

Design: Taisce doc "Grimoire › Share links"; server side on `share-links-server`.
Everything here is hidden for LOCAL daemons and signed-out accounts
(`ShareLinkStore.isAvailable` = signed in).

- **Snapshot** (`ShareSnapshotBuilder`, TaisceKit `Share/`): the cached blocks
  (queued edits included) in order, comments/deleted/canvas blocks out,
  frontmatter and a leading `# <title>` stripped. Each ```` mermaid ````,
  ```` reladraw ```` and ```` vega-lite ```` fence becomes
  `![alt](taisce-asset:dN.svg)`: diagrams are SVG from a dedicated offscreen
  `DiagramWebView` (`renderSVG`, `format: 'svg'` in the page function), charts
  PNG from Swift Charts via `ImageRenderer` (no vector export). One draw at a
  time, each bounded (15 s); a failure or timeout becomes `dN-error.svg`, a
  small card with the renderer's message, and is listed in "Not everything made
  it". `![alt](https://…)` images are fetched (`URLSessionShareImageLoader`:
  2 MB, PNG/JPEG/WebP/SVG by magic bytes) as `iN.ext`, else become a plain link.
  `SafeShareImageLoader` fetches https only, refuses IP literals and local or
  single-label names, checks every resolved address (and every redirect, at most
  3) against private, loopback, CGNAT, link-local, ULA and multicast ranges, uses
  an ephemeral session (no cookies, cache or credentials; auth challenges
  refused) and stops at 2 MB. A DNS rebind between the check and the fetch is
  still possible (accepted). The sheet (and PDF export) first asks "Include N
  images from x.com, y.org?" or keeps them as links. Only paragraph, heading,
  code, mermaid/D2 diagram and decision blocks are published. A link is built
  from a fresh server copy plus only this doc's pending writes (refused offline);
  a PDF may use this device's copy, with a note.
  The server's limits (2 MB an asset, 200 assets, 10 MB) are kept; an asset
  over them is left out with a note. Theme: the app's current appearance.
- **Client**: `APIClient` conforms to `ShareService` (create, list, PATCH with a
  tri-state `expires_at`, revoke, preview, comments, owner reply, delete).
- **UI**: the doc's ⋯ menu › Share link… (Mac: also Doc › Share Link…):
  create with an expiry (1 hour, 1 day, 7 days default, 30 days, custom, never)
  and comments on/off; then Copy link, comments toggle, Update link
  (republishes the current doc at the same URL), Change expiry…, Revoke (asks
  first); views, last opened, expired/revoked. Settings › Shared links lists
  them all with unread badges. A doc whose links have comments shows
  "Comments from shared links (N new)", which opens `LinkCommentsScreen`
  (threads with author, quote, body; reply as owner; delete). Opening them
  marks them read (`POST /api/shares/{id}/comments/read`). Viewers (ADR 0004)
  get no Share link… (the server wants owner or editor) but can export a PDF.
  Refusals (403, 413, 429 per-user caps) show the server's own message. An
  expired link can be given a new expiry; only a revoked one is over.
- **Push**: the server's alert push `{"kind":"share_comment","doc_id",…}`; a
  tap (`DueAlertDelegate` → `ShareCommentPush`) opens that doc and its link
  comments.
- **PDF**: a light snapshot → `POST /api/shares/preview` → offscreen WKWebView
  (no JavaScript) → `UIPrintPageRenderer` over its print formatter, paginated on
  A4 (Letter in US/CA/…, `PaperSize`), 40 pt margins; one long page via
  `createPDF` only if the formatter gives no pages. Mac: File › Export as PDF…
  ⌥⌘E (⇧⌘E is inline code) and a save panel; iPhone: the share sheet.
  The file is named after the doc.
- Tests: `ShareTests` (kit, mock server), `ShareLinkTests` (app: store against a
  fake server, commands, push, real SVG/PNG renders, a long page printed to
  several A4 pages), and `ServerIntegrationTests.t3d_shareLinksEndToEnd`
  against a SERVER daemon (`apple/scripts/integration.sh`).

## Home screens and the shell

The iPhone-Today, iPhone-Todos, iPhone-Search and iPhone-Settings artboards
(Mac-Main / iPad-Library-Doc for the sidebar):

- Tabs (iPhone): Today, Library, Books, To-dos, and Search on its own (the
  iOS 26 search tab). Today's badge is the review queue's count; Review is a
  card on Today, not a tab (`Route.review` pushes the list). Books is
  `BooksPlaceholder` (Screens/BooksHooks.swift) until the Books screens land;
  `Route.book(id)` / `Route.bookReader(id)` and `router.readBook(id)` are its hooks.
- Today: the date and sync state, the serif title; the Review card
  (`GET /api/queue`: "3 agent changes want a look", the first three with
  verdict, doc and proposer; tap the card for the list, a row for the doc);
  Continue reading (`GET /api/books/currently-reading`, SERVER mode: the
  newest started, unfinished book, its colour cover fetched with the app's
  token, typographic otherwise; Read opens the reader over Books); Due
  (overdue, today's dated items and the coming week, `DeadlinePill`s); Pinned
  (workspace colour, folder, serif title, last editor and time). The toolbar
  has New doc, Capture a to-do (a sheet with the live due pill) and, on a
  phone, the account initial for Settings. `HomeStore` holds the queue, the
  book and the profile name; it resets with the rest of the person's state.
- Review (`ReviewScreen`): flagged (Keep / Revert) and parked (Apply /
  Discard) through `POST /api/resolve` as the person; applying a parked doc
  delete asks first. A toast says what happened; the doc catches up.
- To-dos: "N open · N overdue · N today", the add field at the top (the
  server's reading of `due …` as a pill), Overdue / Today / Upcoming with
  short pills ("Thu", "15:00"), swipe right Done, left Snooze or More, a
  context menu, and a filter menu.
- Search: the system field (`.searchable` on the screen: the navigation
  bar on iPhone, as iOS 27 places it even in the search tab; the toolbar on
  iPad/Mac; the artboard's bottom field is not something the app can force),
  Docs / Books / Ask. Docs as before (offline FTS fallback, tag and
  workspace chips); Books searches `GET /api/books?search=` (SERVER mode);
  Ask posts `/api/ask`, reads the answer doc (`AskAnswerText`: the
  synthesis, the docs it cites) and follows it while the synthesis is
  written; "Open the answer" opens the doc.
- Settings: the account card (name, server, how you signed in) opens
  Account (server URL, sign out, sync details); E-readers and Upload Tokens
  (SERVER mode, the person's own app): list with last use, add (the password
  or token shown once with copy buttons and the OPDS / progress-sync
  addresses, never stored), new password, remove or revoke with a
  confirmation; Appearance (theme; on the Mac the doc text size);
  Notifications; Shared links; Data sources (Mac); Sync.
- Large text: at accessibility sizes a to-do's pill and a review row's
  chip, title and author stack under each other instead of widening the page.
- Tests: `ShellShotsUITests` (landscape iPad shots of these screens, skipped
  unless `TEST_RUNNER_TAISCE_SHOTS` names a directory), `HomeTests` (app: review copy, continue reading, due rows, pills,
  Ask parsing, routes), `HomeAPITests` (kit: queue, resolve, books, ask,
  e-readers, tokens).

## Running the app against a scratch daemon

Launch arguments (UserDefaults' argument domain) make screenshots and UI
runs reproducible: `-serverURL http://127.0.0.1:7517`, `-tab
today|library|books|todos|search|review`, `-openDoc "<title>"`, `-searchQuery <text>`,
`-showSettings YES`, and on a doc `-showInspector YES` / `-editDoc YES`
(`-openDoc` on iPhone opens it in the Library tab). For example
`xcrun simctl launch booted ie.null.taisce -serverURL http://127.0.0.1:7517 -openDoc "Roadmap"`.
Start the daemon with `HOME=<tmp>` so it does not import this machine's
Claude memory files. Landscape iPad screenshots for a design comparison:
`TEST_RUNNER_TAISCE_SHOTS=<dir> TEST_RUNNER_TAISCE_SHOTS_SUFFIX=light|dark
TEST_RUNNER_TAISCE_UI_URL=<daemon> xcodebuild … -only-testing:TaisceUITests/DocsShotsUITests`
on an iPad simulator (skipped otherwise; it opens a doc titled Architecture). Tests: `xcodebuild -scheme Taisce -destination 'platform=iOS Simulator,name=iPhone 18 Pro' test`.
