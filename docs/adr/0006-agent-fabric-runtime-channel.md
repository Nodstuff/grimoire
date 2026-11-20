# ADR 0006 — Agent Fabric: the runtime channel and the fleet (Slice 1, Watch)

- **Status**: accepted (2026-11-20). Covers Slice 1 only; later slices add to it.
- **Who/when**: Tom + Claude, 2026-11-20
- **Scope**: SERVER mode only. In LOCAL mode the viewer routes answer 404 and no runtime can
  connect.
- **Design**: the Taisce docs under "Agent Fabric" (How It Works, Runtime Channel, Trust and
  Safety, Build Plan). The wire contract is `proto/taisce/runtime/v1/runtime.proto`, which is
  the source of truth; this ADR records the server's decisions.
- **Numbering**: some code comments cite "ADR 0006" for the EPUB library. No such file was
  written; this is the only ADR 0006.

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
(`Runtime.Connect(stream Up) returns (stream Down)`). The runtime is a native Swift app built
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

### 2. Its own listener

The service listens on its own loopback port (`serve --runtime-grpc-port`, env
`TAISCE_RUNTIME_GRPC_PORT`; off unless set), cleartext HTTP/2: the proxy terminates TLS and
routes `/taisce.runtime.v1.Runtime/` to it (`infra/deploy/portus.yaml`, ahead of the
catch-all, with no timeout and no body limit). The axum app, its router and its
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
| A session appears or changes state | At once. |
| Anything else about it changes (tool, note, last line, context) | Within 5 seconds. |
| The same report again | Never. |
| A headroom report, the last event number | With the next flush. |
| A runtime connects or disconnects | At once. |

Every write of a runtime's state takes one path, one writer at a time per runtime, and each
writer takes the rows as they are when its turn comes. An older picture cannot land over a
newer one. A crash loses at most 5 seconds of detail, never a change of state.

- **Order.** Each side numbers its events per instance (a fresh id per launch of the
  runtime). A reconnecting runtime is told the last event the hub took and replays what came
  after; an event at or below it is ignored. The number is kept on the runtime's row, so it
  survives a daemon restart.
- **Snapshots.** Every stream starts with a snapshot of all sessions, naming the event number
  it replaces. A state event at or below that number is older than the snapshot and is
  ignored. A session the hub knew that the snapshot lacks is shown as exited and kept.
- **One stream per runtime.** A new stream supersedes the old, which ends with ABORTED.
- **Liveness.** The runtime sends a heartbeat every 20 seconds when idle; three missed ends
  the stream. HTTP/2 pings stop at the proxy, so the heartbeat is a message.
- **Limits.** 1 MiB a message, 200 sessions a runtime, 16 runtimes a person, byte caps per
  field, a burst of 400 messages then 50 a second. A breach ends the stream with a status
  that names the reason. At 200 sessions the exited one longest unchanged makes room.

### 6. Retention: 7 days

Sessions are history. A row goes 7 days after its last update (swept at start and hourly).
A session a connected runtime still reports is not history: the sweep first records that it
exists, so a long-lived session is not expired under a Mac that never sleeps. Runtimes are
settings and stay until removed; removing one deletes its sessions.

### 7. What a person's devices read

| Route | Answers |
|---|---|
| `GET /api/fabric/sessions` | The fleet: runtimes (online, last seen, headroom) and sessions sorted by attention (needs_you, stalled, finished, waiting_on_peer, working, idle, asleep, exited). |
| `GET /api/fabric/runtimes` | The enrolled runtimes alone. |
| `GET /api/fabric/stream` | SSE: the whole fleet, then one event per change. |

All three answer the person's own app or web session only. A fleet carries project names
and the last lines of terminal sessions, so a connector gets 403, as on the other human
surfaces; a PAT never opens `/api` (401); no MCP tool reads a fleet. The stream has no
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

## Alternatives rejected

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
  `taisce.service`. The route must carry no timeout.
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
