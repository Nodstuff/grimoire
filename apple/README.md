# Grimoire for iPhone, iPad (and later Mac)

A native SwiftUI client for a Grimoire server: a view into its docs with an
offline cache. No web views for UI. iPhone + iPad first; Mac via Mac Catalyst
later.

## Layout

```
apple/
  GrimoireKit/                 Swift package: all logic, tested on macOS
    Sources/GrimoireKit/
      Models/                  Doc, DocSummary, Block, BlockNode, Change, Todo/Due, BlockOp
      API/                     ServerConfig (+ TokenProvider seam), APIClient
      Cache/                   GRDB cache (docs, blocks + FTS5, todos, sync_state, outbox)
      Sync/                    SSEParser, changeStream, Backoff, SyncEngine (actor)
      Render/                  block markdown → RenderNode (swift-markdown), inline/wikilinks
      Todo/                    offline parser for the To-do doc
    Tests/GrimoireKitTests/    Swift Testing + a URLProtocol mock server
  App/                         thin iOS app shell (xcodegen)
    project.yml
    Grimoire/                  SwiftUI views + AppModel
```

## Build and test

```sh
cd apple/GrimoireKit && swift test
# read-only smoke against a real daemon (GETs only; never /api/todo):
GRIMOIRE_LIVE_URL=http://127.0.0.1:7425 swift test --filter LiveServer

cd apple/App && xcodegen generate && open Grimoire.xcodeproj
xcodebuild -project Grimoire.xcodeproj -scheme Grimoire -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' build
```

`xcodebuild` needs the iOS platform/simulator runtime installed (Xcode ›
Settings › Components). Without it, the app sources can still be
type-checked against the simulator SDK by cross-building GrimoireKit
(`swift build --triple arm64-apple-ios26.0-simulator --sdk $(xcrun --sdk iphonesimulator --show-sdk-path)`)
and running `swiftc -typecheck` over `App/Grimoire/*.swift`.

Dependencies (SPM only): GRDB.swift 7.11.1, swift-markdown 0.9.0 (pulls
swift-cmark 0.9.0). SSE is hand-rolled (`SSEParser`, ~120 lines) rather than a
package: `URLSession.AsyncBytes.lines` drops the blank lines that end SSE
events, the resume cursor is ours (cache `last_seq`, not a library's
in-memory id), and the parser is pure and tested byte-by-byte.

## Sync contract

The client follows a global change cursor:

- `GET /api/changes?since=<seq>&limit=<n>` →
  `{"seq": Int, "changes": [{"seq", "doc_id", "kind": "doc"|"tree"|"deleted"|"restored", "epoch"?, "at"}], "more": Bool}`
- `GET /api/changes/stream` (SSE): `id: <seq>`, `event: change`,
  `data: <one change>`, `: ping` every 25 s, resumes from `Last-Event-ID`.

`SyncEngine` (actor, foreground-only `start()` / `stop()`):

1. Fresh cache (cursor 0): load the whole tree from `/api/docs`.
2. Catch up: page `/api/changes` from `last_seq` until `more` is false;
   store the cursor after each page.
3. Follow the stream with `Last-Event-ID: <last_seq>`; a 60 s idle timeout
   (two missed heartbeats) counts as a drop.
4. On drop: exponential backoff with jitter (1 s → 30 s cap, floored by the
   server's `retry:`), reset after any healthy connection; resume from
   `last_seq`.

Applying a batch: last change per doc wins; `deleted` drops the doc and its
blocks; `tree` / `restored` refetch `/api/docs` once; `doc` refetches the body
only for docs we hold (or the To-do doc, or `alwaysFetch`), otherwise just
marks the cached row stale (`current_epoch > body_epoch`) so opening it
fetches. UI listens via `updates()` (AsyncStream) and GRDB `ValueObservation`.

To-dos: deadlines are `· due YYYY-MM-DD` or `· due YYYY-MM-DD HH:MM`; a
date-only deadline alerts at 09:00 local (`Due.alertDate`).

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
