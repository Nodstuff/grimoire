# ADR 0007 — Agent Fabric: the runtime channel and the fleet (Slice 1, Watch)

- **Status**: accepted (2026-11-20; revised the same day after the transport spike (§5a, the
  rpc's name, the proxy route) and after an independent review (§5b, the silence deadline,
  the status codes)). Covers Slice 1 only; later slices add to it.
- **Who/when**: Tom + Claude, 2026-11-20
- **Scope**: SERVER mode only. In LOCAL mode the viewer routes answer 404 and no runtime can
  connect.
- **Design**: the Taisce docs under "Agent Fabric" (How It Works, Runtime Channel, Trust and
  Safety, Build Plan). The wire contract is `proto/taisce/runtime/v1/runtime.proto`, which is
  the source of truth; this ADR records the server's decisions.
- **Numbering**: 0007, not 0006. Code comments on `main` (`main.rs`, `isolation_tests.rs`,
  since the EPUB library landed) cite "ADR 0006" for the EPUB library. No such file was ever
  committed, on any branch, but the number is spoken for: a file here named 0006 would make
  those citations point at the wrong document.

## Context

A person runs many Claude Code sessions in terminals on their Mac, hosted by a *runtime*
(Workbox). They want to see, from their phone, what each session is doing and to be told
when one stops or asks for them. The Mac sleeps and sits behind NAT; the Taisce server is
always on and is where the phone already signs in. So the runtime dials out to the server
(the *hub*) and reports; the phone reads the hub. The phone never talks to the Mac.

Slice 1 is watch-only. Nothing travels from the hub to a runtime except the answer to its
Hello, so nothing here can make a Mac do anything.

## Decision

### 1. gRPC, on the runtime leg only

The runtime and the hub speak gRPC: one bidirectional stream per runtime
(`Runtime.Attach(stream Up) returns (stream Down)`). The runtime is a native Swift app built
in another repository by another session, and a `.proto` file is a contract both sides
generate from. Everything else keeps the transport it has: the phone and the web UI use
`/api` over HTTPS with SSE, agents use `/mcp`, pushes go by APNs.

- The generated code lives in its own crate, `taisce-runtime-proto`. Its `build.rs` compiles
  the `.proto` with protox (pure Rust) and hands the descriptors to tonic's generator, so the
  build never needs `protoc`: the server is cross-built with cargo zigbuild.
- Field numbers follow the design's sketch. The ones later slices use are `reserved`, each
  with the name it will carry. `HumanMessage` is dropped. `Runtime.Call` is not declared until
  Slice 4: adding an rpc is a compatible change, its messages have no agreed shape yet, and a
  call to it today already answers UNIMPLEMENTED.
- A body the hub does not know is ignored, so a runtime one slice ahead still works.
- The rpc is `Attach`, not `Connect`: tonic's generated client already has a `connect()`
  constructor, and an rpc of that name does not compile beside it (E0592).

### 2. Its own listener

The service listens on its own loopback port (`serve --runtime-grpc-port`, env
`TAISCE_RUNTIME_GRPC_PORT`; off unless set), cleartext HTTP/2: the proxy terminates TLS and
routes `/taisce.runtime.v1.Runtime/` to it (`infra/deploy/portus.yaml`; §5a says what that
route must and must not set). The axum app, its router and its
`require_auth` layering are untouched: no data route became reachable in a new way, and the
service cannot be reached through a path `require_auth` guards differently.

### 3. A credential of its own kind

A runtime signs in with a *runtime credential*: `trt_` + 256 random bits, sent as
`authorization: Bearer …` on the call.

- It is minted on the box (`taisce fabric runtime add --name <n>`), or minted on the Mac and
  registered by hash (`--hash <sha256>`), so the secret need never reach the box. Only its
  SHA-256 is stored, on the runtime's own row (`fabric_runtimes.credential_hash`).
- **It opens the runtime channel and nothing else.** `require_auth` never looks one up, so on
  `/api`, `/mcp` and the web UI it is an unknown token (401).
- **Nothing else opens the channel.** The service resolves only `trt_` credentials, only in
  `fabric_runtimes`. A personal access token, an app token or a connector token is an unknown
  credential there (UNAUTHENTICATED). The kinds live in different tables, so no lookup can
  confuse them.
- Failed sign-ins draw from a per-IP bucket (`ratelimit::Class::RuntimeAuth`), as failed
  upload tokens do.
- Enrolling and removing a runtime is a human surface. Today that is the box CLI
  (`taisce fabric runtime add|list|revoke`). Over HTTP it is deferred; over MCP it is never
  allowed, and the isolation suite fails if an MCP tool's name looks like one.
- The server restarts the service if it ever stops (it is under the same supervision as the
  daemon's other long-lived tasks), and at shutdown the hub's pending writes are flushed
  before the HTTP server drains its connections: an open SSE stream holds that drain open
  until the service manager gives up (observed), so a flush after it would be lost.
- A removed runtime's credential stops resolving at once. An open stream is ended within a
  minute (it is rechecked every 60 seconds, and at once when its owner next reads the fleet).

### 4. Isolation is in the store, as everywhere

`fabric_runtimes` and `fabric_sessions` (schema v16, `crates/store/src/fabric.rs`) are keyed
to one person.

| Scope | Sees |
|---|---|
| `User(u)` | `u`'s own runtimes and sessions. Anyone else's is `NotFound`. |
| `System` | Everything: credential resolution (before a user is known), expiry, the box CLI. |
| `Local` | Nothing. A fleet is keyed to a signed-in user and LOCAL mode has none. |
| `Within` | Nothing. A fleet lies in no workspace. |
| `Public` | Denied (`deny_public`). |

- The credential names the runtime; the runtime names its owner. Every write a stream causes
  runs in `Scope::User(owner)` and is keyed to that runtime, whatever ids the messages carry.
- A session's key is `(runtime, id)`: the id is the runtime's own (a terminal pane's UUID),
  so one runtime can never write a row another holds, even with the same id.
- A fleet is personal. Workspace sharing does not reach it.

### 5. An in-memory hub, with coalesced writes

`fabric/hub.rs` holds each person's fleet in memory. It is loaded from SQLite the first time
anything asks for it, in that person's own scope, with every runtime offline: *online* is
the hub's observation and lasts as long as a stream does.

SQLite is replicated by litestream, so a chatty writer is a costly one.

| What happened | When it is written |
|---|---|
| A session appears or changes state | At once, at most once per 5 seconds per session (§5b). |
| Anything else about it changes (tool, note, last line, context) | Within 5 seconds. |
| The same report again | Never, except once a day to say the session still exists (§6). |
| A headroom report | With the next flush. |
| A runtime connects or disconnects | At once, with the last event number taken. |

Every write of a runtime's state takes one path, one writer at a time per runtime, and each
writer takes the rows as they are when its turn comes and writes them in one transaction. An
older picture cannot land over a newer one. A crash loses at most 5 seconds of detail.

Each fleet publishes its changes on a channel of its own. A subscriber (an SSE stream) holds
one person's events and nobody else's: isolation does not rest on a route remembering to
filter, and one person's burst cannot make another's stream fall behind.

- **Order.** Each side numbers its events per instance (a fresh id per launch of the
  runtime). An event at or below the last one taken is a replay and is ignored. A
  reconnecting runtime is told the last event the hub holds.
- **Reconnecting, in this slice.** A runtime keeps no log of events and replays nothing: the
  mandatory snapshot restores every session and the accounts report that follows it restores
  the headroom. So the last event number is not worth a write per event. It is kept in
  memory and stored when a stream ends or the daemon stops; after a crash it is stale, which
  costs nothing. A later slice whose events a snapshot cannot restore changes this.
- **Unknown values are tolerated, on one rule.** A body the hub does not know is ignored. A
  session state it does not know (0 included) is shown as `unknown`, sorted beside working,
  and never alerts. Neither ends the stream, so a newer runtime does not loop on reconnect.
- **Snapshots.** Every stream starts with a snapshot of all sessions, naming the event number
  it replaces. A state event at or below that number is older than the snapshot and is
  ignored. A session the hub knew that the snapshot lacks is shown as exited and kept.
- **One stream per runtime.** A new stream supersedes the old, which ends with ABORTED.
- **Liveness.** The runtime sends a heartbeat every 20 seconds when idle; 60 seconds with no
  message ends the stream. HTTP/2 pings stop at the proxy, and a frozen or vanished client is
  not reliably signalled upstream, so the heartbeat is a message and the deadline is the
  hub's own. The deadline moves only when a message arrives. (As first built, any wake-up of
  the stream's loop re-armed it, and the 60 second recheck woke it every 60 seconds: a Mac
  that slept without closing its connection would have stayed online for ever.)
- **How a stream ends** is always a status that says why, and the `.proto` says what a
  runtime does for each: stop and ask for re-enrolment, wait long, fix the bug, or reconnect
  with backoff. Only the runtime closing its side is a clean end. A message that is too big
  or does not decode is INVALID_ARGUMENT or the decoder's own status, never a silent close
  that a client would answer by reconnecting in a loop. A store that cannot answer at
  sign-in is UNAVAILABLE and is not counted as a failed guess.
- **Flow control.** A send window of 64 unacknowledged events each way (§5a).
- **Limits.** 1 MiB a message, 200 sessions a runtime, 16 runtimes a person, byte caps per
  field. A breach ends the stream with a status that names the reason. At 200 sessions the
  exited one longest unchanged makes room.
- **Message rate.** Events are paced by the send window (§5a) and are not counted. Messages
  outside the window (snapshot, heartbeat, ack) are held to a burst of 30 and one a second.
  (The first build counted every message, 400 then 50 a second. The window made that
  redundant for events and wrong for an honest burst of them.)

### 5a. Constraints the proxy puts on the channel

On 2026-11-20 the channel was run locally through the real proxy: Portus 0.4.0 (on rama
0.4.0), a tonic 0.14.9 server, a grpc-swift-2 2.4.4 client.

| Observed | |
|---|---|
| The path route wins over the host-only catch-all | in either order in the file |
| An idle stream stays open | 15 to 16 minutes with no keepalive |
| A config hot-reload | leaves streams up |
| A Portus restart | cuts every stream after a 25 second drain |
| `authorization`, `x-forwarded-for`, `x-real-ip` | reach the backend |

Not tested: networks other than loopback, the ACME listener, the Linux build of Portus, and
whether Portus overwrites or appends to an `x-forwarded-for` the client sent. The hub keeps
the rule the rest of the server uses (the last hop is the address the proxy saw; ADR 0005
§5 records that Portus appends) and uses it only to rate-limit failed sign-ins.

The run also found three things the design has to hold to.

| What the proxy does | Cause | What the channel does about it |
|---|---|---|
| Ends an HTTP/2 connection once about 145 DATA frames under 256 bytes are buffered on it, in either direction (GOAWAY `too_many_data_frames`, or RST `ENHANCE_YOUR_CALM`). Any other stream sharing that pooled upstream connection dies with it. | A hard-coded budget in rama's HTTP/2 code. | **A send window with acknowledgements.** Neither side may have more than 64 events in flight that the other has not acknowledged. `Ack{seq}` (both directions, cumulative) is sent once 16 events are unacknowledged or 250 ms after the oldest. A full window and the acks it earns stay far below 145 frames. |
| Caps the total bytes a client sends on one stream at 10 MiB when a route sets no `max_request_body_bytes` (0 also means the default), and kills the stream with an HTTP/2 error, not a 413. | The request-body limit is applied to a stream's running total. | The gRPC route sets `max_request_body_bytes: 18446744073709551615`. |
| Cuts every stream, after a 25 second drain, each time it restarts. (A config hot-reload does not.) | A restart closes its connections. | **A stream is finite.** Resuming by sequence number is the normal path: see §5, Order and Snapshots. |

- The window is in the protocol, not in tuning. `taisce_runtime_proto::window` holds both
  halves (`SendWindow`, `Receipts`), shared by the hub and the fake runtime. Everything the
  hub sends goes through one sender (`fabric::grpc::DownSender`) that holds the hub's own
  events to the window. In this slice the hub sends only a Welcome and acks, both outside the
  window, so the sender is there for orders and nudges to inherit.
- Hello, Welcome, Snapshot, Heartbeat and Ack are outside the window. Each is rare by
  construction: one per stream, one per 20 idle seconds, or one per 16 events.
- **An Ack is flow control, not durability.** The hub writes a change of state at once and
  the rest within 5 seconds, so a crash can lose acknowledged detail. The resume point is
  the Welcome's `last_up_seq`. For this slice's events that is enough: a snapshot restores
  every session, and a runtime sends its accounts again after the snapshot that opens a
  stream. A later slice whose events a snapshot cannot restore (turns, results) must write
  them before acknowledging them.
- The hub cannot see whether a runtime obeys the window; a runtime that does not harms its
  own connection. The hub's side of the bargain is to acknowledge promptly, and it does so
  for replayed events too.
- Route order in `portus.yaml` does not matter: the longest path prefix wins.
- The proxy pings the backend every 30 seconds and drops every stream if unanswered for 20;
  tonic answers HTTP/2 pings by default.
- `max_request_body_bytes` has no "unlimited" value, and a large finite one only moves the
  cliff: the largest value is used. The real bound on what a runtime can make the hub hold is
  the hub's own 1 MiB per message.

Two other answers to the frame budget were rejected. **Padding every message to 256 bytes**
would step over the limit as it is measured today, and hide it: the next change to the
budget, or to what counts as small, would bring the failure back with nothing in the
protocol to bound it. **Relying on clients to batch their writes** leaves the limit to every
write path in two codebases: one careless path kills the connection, and a bystander's
stream with it.

### 5b. A session that flaps is not a flood

A runtime bug that flips a session between working and needing its person, as fast as the
window lets it, must cost one person's fleet nothing much and other people nothing at all.
APNs throttles a provider, not a device, so pushes are shared between everyone on the server.

- **Alerts**: a session is announced in the same alerting state (needs you, stalled,
  finished) at most once per 30 seconds. Entering a different alerting state is announced at
  once; each state keeps its own clock, so alternating between two is held to the same rate.
- **Writes**: a session's change of state is written at once at most once per 5 seconds; a
  change inside that window rides the 5 second flush. (A session's first appearance is
  written at once and does not start the window, so the first real change after it is
  prompt.)
- Nothing is lost by either: the fleet in memory and the live stream see every change.

### 6. Retention: 7 days

Sessions are history. A row goes 7 days after its last update (swept at start and hourly).
A session its runtime still reports is not history, and two things keep it:

- The sweep first records that every session of a connected runtime exists, so a long-lived
  session is not expired under a Mac that never sleeps.
- A report identical to what is held (every reconnect's snapshot is one) records that the
  session exists once the row is a day old. So a laptop that is online daily but never
  across a sweep keeps its week-idle session, which otherwise would be expired, reappear as
  a first sight, and announce itself again. Runtimes are
settings and stay until removed; removing one deletes its sessions.

### 7. What a person's devices read

| Route | Answers |
|---|---|
| `GET /api/fabric/sessions` | The fleet: runtimes (online, last seen, headroom) and sessions sorted by attention (needs_you, stalled, finished, waiting_on_peer, working, idle, asleep, exited). |
| `GET /api/fabric/runtimes` | The enrolled runtimes alone. |
| `GET /api/fabric/stream` | SSE: the whole fleet, then one event per change. |
| `POST /api/fabric/sessions/{runtime_id}/{session_id}/snooze` | Body `{"until": <unix seconds>}`: no push for that session before then (§8a). Answers the session. |
| `DELETE /api/fabric/sessions/{runtime_id}/{session_id}/snooze` | Lifts the snooze. Answers the session. |

All of them answer the person's own app or web session only. A fleet carries project names
and the last lines of terminal sessions, so a connector gets 403, as on the other human
surfaces; a PAT never opens `/api` (401); no MCP tool reads a fleet or snoozes. The stream has no
cursor: every connection opens with the whole fleet, and a stream that falls behind is ended
so the client reconnects for a fresh one.

### 8. One visible push per session

A push is sent when a session comes to need its person, stalls, or finishes a turn. Finishing
counts only when the session was in a turn before, so a session first seen finished, or one
moving between finished and idle, says nothing.

The notification's title is the session's address and its body is the state ("Needs you",
"Stalled", "Finished"). It carries the session and runtime ids and nothing of the session's
text. One session's notifications share an APNs thread id and collapse id, so a later one
replaces the earlier.

The design allows a preview of the session's last line in notifications, with a per-project
"Private notifications" switch. Neither is built in this slice: until the switch exists, the
safer default is to send no text.

### 8a. Snooze is honoured before the push is sent

A person snoozes one session until a time they choose. An app cannot take back a visible
push once APNs has it, so the hub holds it back: a snoozed session raises no alert.

- `fabric_sessions.quiet_until` (unix seconds, NULL = not snoozed) is the person's own
  setting on their own row, written only by `fabric_session_quiet` in their `Scope::User`.
  A runtime's report never carries or changes it. Anyone else's session is `NotFound`.
- The hub checks the time itself when a session changes state, so a snooze ends on time
  whatever the housekeeping is doing. The flush tick lifts the mark of a snooze that has run
  out and says so on the stream.
- A snoozed session stays in the fleet, with its `quiet_until`. Nothing is hidden.
- `until` must lie ahead of the server's clock and at most 7 days away (a session is history
  after that). The app chooses the time, because "until tomorrow" is a time on the person's
  own clock.

Not built: snoozing a whole project or fabric, following a session, and quiet hours, which
the design also names.

## Alternatives rejected

- **Padding messages, or trusting clients to batch**, against the proxy's frame budget: §5a.

- **WebSocket with JSON** for the runtime leg. The proxy carries it too. Rejected for the
  contract: two repositories built by two sessions agree more reliably on a `.proto` than on
  prose. It remains the fallback if gRPC through the proxy misbehaves.
- **Serving gRPC on the daemon's HTTP port.** One port fewer, but it would put the service
  behind `require_auth`'s path rules and make the main listener accept cleartext HTTP/2. A
  second listener keeps the existing surface exactly as it was.
- **Reusing personal access tokens**, with a kind column. One table, but then one lookup
  serves two kinds of credential and a missed filter would let a PAT open the channel or a
  runtime credential open `/mcp`. Separate tables make that mistake impossible to write.
- **Writing every event to SQLite.** Simple, and the store would be the one source of truth.
  Rejected for the replication cost: a working session changes its tool many times a minute.
- **Reading the fleet from SQLite with the pending changes overlaid.** Rejected: two sources
  to merge on every read. Memory is the truth while the daemon runs; SQLite is what survives.
- **Checking in the generated code.** Rejected while a pure-Rust compile works: nothing can
  drift from the `.proto`.
- **Deleting a session the snapshot lacks.** The design keeps sessions 7 days after exit, so
  the phone can still show what a closed session last said.
- **Notification previews now.** See §8.

## Consequences

- A second loopback port on the box (7426), a route in `portus.yaml` and a variable in
  `taisce.service`. The route must carry no timeout, and must carry the body limit of §5a.
- Every restart of the proxy disconnects every runtime for a moment. Each reconnects and
  resumes; the fleet shows them offline in between.
- A runtime must implement the send window. One that floods small events takes down its own
  proxy connection, and any stream sharing it.
- The daemon gains tonic, prost and tokio-stream; the build gains protox and
  tonic-prost-build. axum, hyper and tower are the versions already in the tree.
- The hub is in one process. A second daemon on the same database would not see its fleet;
  the box runs one.
- The box CLI edits the database under a running daemon. The hub picks up a runtime enrolled
  or removed that way when its owner next reads the fleet, when the runtime connects, or at
  an open stream's next recheck.
- After a daemon restart every runtime is offline until it reconnects, and each reconnects
  with a snapshot, so nothing is stale for longer than a reconnect.
- A broken-into server could show a false fleet. That is accepted in the design: session
  state is not signed, and nothing in this slice acts on it.
- Every later slice (turns, orders, messages) adds tables to `fabric.sql`, messages to the
  reserved numbers, and rows to the isolation suite's `COVERAGE`.
