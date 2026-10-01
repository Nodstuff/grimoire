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
      Cache/                   GRDB cache (docs, blocks + FTS5, todos, sync_state, outbox)
      Sync/                    SSEParser, changeStream, Backoff, SyncEngine (actor)
      Render/                  block markdown → RenderNode (swift-markdown), inline/wikilinks
      Todo/                    offline parser for the To-do doc
    Tests/TaisceKitTests/    Swift Testing + a URLProtocol mock server
  App/                         thin iOS app shell (xcodegen)
    project.yml
    Taisce/                    SwiftUI views + AppModel
```

## Build and test

```sh
cd apple/TaisceKit && swift test
# read-only smoke against a real daemon (GETs only; never /api/todo):
TAISCE_LIVE_URL=http://127.0.0.1:7425 swift test --filter LiveServer

cd apple/App && xcodegen generate && open Taisce.xcodeproj
xcodebuild -project Taisce.xcodeproj -scheme Taisce -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' build
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

- `GET /api/docs` sends `X-Grimoire-Seq: <head>`, read under the same lock
  as the list.
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
`deadline` and the time as `due_time`; a date-only deadline keeps the item's
existing time.

## What's stubbed

- Outbox replay: queues, orders and replays once (idempotent via
  `request_id`), but no conflict handling, no UI.
- Editing: `APIClient` wires `propose`, `propose_markdown` and the to-do
  writes; no editing UI.
- Auth: `ServerConfig.tokenProvider` sends a Bearer token when one is
  supplied; default is none (localhost).
- Pinned: sidebar placeholder.
- Diagrams (Mermaid, Vega-Lite, D2) render as labelled placeholder cards.
- APNs / background refresh, Keychain (Valet), Mac Catalyst.
