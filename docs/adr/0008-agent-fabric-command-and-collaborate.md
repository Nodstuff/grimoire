# ADR 0008 — Agent Fabric: read, command, ask, collaborate (Slices 2–5)

- **Status**: accepted (2026-11-21). The contract half (Phase A): the wire messages, the
  signing rules and the viewer API. The server's implementation follows on the same branch
  and amends this ADR where building it teaches something.
- **Who/when**: Tom + Claude, 2026-11-21
- **Builds on**: ADR 0007 (Slice 1, Watch: the channel, the fleet, flow control, isolation).
  Everything there still holds.
- **Design**: the Taisce docs under "Agent Fabric" (What You Can Do, How It Works, Trust and
  Safety, Runtime Channel, Build Plan). Slice 6 (spawn, doc watches) is out of scope.
- **Contract**: `proto/taisce/runtime/v1/runtime.proto` is the source of truth for the wire,
  including SIGNING. `testdata/signing-vectors.json` beside it holds test vectors (made by
  `testdata/make_vectors.py`, checked by `crates/runtime-proto/tests/signing_vectors.rs`).
  `default-settings.json` beside it holds the guardrails and limits every runtime starts with.

## Context

Slice 1 shows the fleet. Slices 2–5 let the person read what a session did, command it from
the phone, answer the questions agents put to them, and let agents talk to each other. Three
programs build against this at once: this server, the Taisce Apple app, and Workbox (Swift,
grpc-swift-2). So the contract is fixed first, here.

The design's core rule is unchanged: Workbox is the only thing that touches a session, the
hub keeps the record and relays, and **nothing the hub sends carries the person's authority
unless a paired device signed it**.

## Decision

### 1. What travels on the channel

| Direction | Message | Kind | What it is |
|---|---|---|---|
| Up 4 | `SessionState` | restorable | Gains `fabric_id`, `turn_id` (latest turn), `queue` (queued order ids), `queue_paused`. |
| Up 3 | `Snapshot` | not an event | Gains `open_requests`: the hub clears any other open item of this runtime. |
| Up 12 | `RuntimeStatus` | restorable | The runtime's signing key, Hold state, settings version in force, paired devices, and whether the person is at the Mac. Sent after every Snapshot and on change. |
| Up 5 | `Turn` | durable | One finished turn: prompt, reply, tool count, git outcome, how it ended, where its prompt came from. |
| Up 6 | `Attention` | durable | An item raised (a runtime-signed `AttentionRequest`: approval, question, dialog, tamper) or cleared. |
| Up 7 | `Result` | durable | What happened to an order, a nudge or a Settings: done, queued, refused, failed, with a reason code. |
| Up 9 | `Pairing` | durable | What happened to a pairing offer: paired, rejected, expired, invalid. |
| Down 3 | `Signed` order | event | A device-signed `Order`: a `Command` to one session, or `Hold`. |
| Down 7 | `Signed` settings | event | The device-signed `Settings` (guardrails, limits, fabrics), newest version. |
| Down 5 | `PairingOffer` | event | A device's public key, sealed with the pairing code's secret. |
| Down 4 | `Nudge` | event | "Session X has mail" (message id, sender, kind, may it wake). Never the text. |
| rpc | `Call` | unary | An agent's fabric tool: `who`, `send`, `inbox`, `inbox_ack`. |

Slice 1 is wire-compatible: no field was renumbered or retyped. A Slice 1 runtime never sends
the new bodies and the hub never sends it a new Down body unless it reported devices or a
settings version, which a Slice 1 runtime cannot.

### 2. Durable events and replay

Slice 1 kept no log: a snapshot restored everything. Turns, attention, results and pairing
outcomes cannot be restored that way, so:

- **Up**: the runtime keeps each durable event on disk until acknowledged, and after a
  reconnect sends those above `Welcome.last_up_seq` again with their original numbers, before
  anything new. The hub **writes a durable event before acknowledging it**, and stores
  `last_up_seq` in the same transaction, so `Welcome.last_up_seq` never names an event the
  hub has not written. Restorable events stay as in ADR 0007 (memory, flushed within 5 s).
- **Down**: identity, not sequence. The hub keeps each order, settings, pairing offer and
  nudge until the runtime acknowledges it, and re-sends every unacknowledged one that is still
  live on each new stream, under fresh numbers. The runtime acknowledges a Down event only
  once handled durably (an order's counter recorded and its Result in the outbox), and
  recognises a repeat by its identity: (device, counter), settings version, pairing id,
  message id. `Hello.last_down_seq` acknowledges what arrived before the cut.
- A repeated order the runtime already accepted is answered `refused: replayed`. Its first
  Result always reaches the hub first (durable events replay in order before new ones), so
  the hub ignores a `replayed` refusal for an order that already has a Result.
- Every new event goes through the 64-event send window, both ways; nothing goes around it.

### 3. Agent tool calls use their own rpc, not the stream

`rpc Call(CallRequest) returns (CallReply)`, one call per HTTP/2 stream.

- An agent is blocked on the answer. On the Attach stream a call would queue behind events
  and the send window, and need its own correlation ids; a unary rpc has both for free.
- A retry carries the same `call_id` and gets the first answer (remembered 10 minutes), so a
  retried `send` posts one message. This is the MCP dedupe's rule, at the same TTL.
- Order between calls and events does not matter by construction: `ask_human` and `note`
  never reach the hub as calls (they are an `Attention` event and `SessionState.note`), and
  `send` answers with everything the caller needs.
- Typed messages per tool, not JSON in a string: two codebases agree more reliably on a
  `.proto` than on prose.
- At most 8 calls in flight per runtime (16 is RESOURCE_EXHAUSTED). A call needs an open
  Attach stream (else UNAVAILABLE: retry), so the hub's picture of the caller is current.

### 4. Signing

Normative text: SIGNING in the `.proto`. In short:

| | |
|---|---|
| Algorithm | ECDSA P-256, SHA-256. The phone's key lives in its Secure Enclave. |
| Signed bytes | `CONTEXT \|\| payload`, CONTEXT one of `taisce/order/v1\n`, `taisce/settings/v1\n`, `taisce/attention/v1\n`, `taisce/pairing/v1\n`. |
| Payload | The protobuf message, canonically encoded, carried verbatim (`Signed.payload`, `SignedRequest.request`, `PairingSeal.claim`). Verifiers check the bytes they received and never re-encode to verify. |
| Signature | 64 bytes `r \|\| s` (CryptoKit `rawRepresentation`); either s accepted. |
| Public key | 65 bytes X9.63 (`x963Representation`). |
| device_id | lowercase hex SHA-256 of the public key; fingerprint = its first 16 hex characters in fours. |
| Canonical | Ascending field numbers, defaults omitted, a set oneof member or sub-message written even when empty, no unknown fields. The hub refuses a payload that does not re-encode to the same bytes (`non_canonical`); that also refuses unknown fields. |

- **Why protobuf bytes, carried verbatim**: the phone and Workbox already share the `.proto`;
  verifying the received bytes makes canonicalisation a signer's discipline, not a
  verification hazard. Canonical JSON was rejected (number and escaping rules differ across
  JSON libraries); a hand-rolled text format was rejected (a second encoding to specify).
- **Why the hub insists on canonical bytes**: an unknown field in a signed payload means the
  signer asked for something a verifier cannot honour, and ignoring it could broaden what was
  signed. Refusing non-canonical bytes refuses exactly that, with prost, which drops unknown
  fields silently.
- **Domain separation**: the context labels stop an approval signature from being replayed as
  an order, or a pairing claim as settings.
- **Approvals bind everything shown**: `Command.request_hash` is SHA-256 of the exact
  `AttentionRequest` bytes the phone verified and displayed (runtime, session, tool, text,
  deadline), a superset of the design's "hash of the command text".
- **What a runtime checks**, in order: unknown_device, bad_signature, unsupported (unknown
  fields), wrong_runtime, replayed, expired. Past those it records the counter, so a refused
  order cannot later be replayed into success. Then held, then the command's own checks.
- The hub checks the same signature against the device keys the runtime last reported
  (`RuntimeStatus.devices`). That is a courtesy that keeps garbage out of the queue; the
  runtime's check is the one that matters.

### 5. Orders, settings and the two kinds of authority

- **An `Order` is momentary**: it names one runtime, carries a per-device counter and a
  device-chosen expiry (the hub takes at most 24 hours ahead), and is refused after it. A
  `Command` acts on one session: `send`, `interrupt`, `approve`, `deny`, `resume`, `seen`,
  `answer`, `cancel`, `reorder`, `release_queue`, `wake`, `set_fabric`, `compact`, `clear`
  (`spawn` is reserved for Slice 6). `Hold` is Hold all or its release on one runtime; Hold
  all across runtimes is one order to each.
- **`Settings` is standing**: guardrails, limits and fabrics, whole, versioned, fleet-wide.
  A runtime takes any version above its own (no counter, no expiry), so a Mac asleep for a
  day still wakes to the newest rules, and refuses one at or below. The hub takes a new
  version only at exactly its own plus one (409 otherwise), so two devices editing at once
  cannot overwrite each other silently. The design's `Policy` and `FabricEdit` orders became
  this: a configuration is versioned state, not an event, and expiring it would make a
  sleeping Mac miss a rule change.
- Moving a session into a fabric is a command (`set_fabric`) because it is about one session
  now; defining fabrics is settings.
- **Guardrails fail closed**: a pattern a runtime cannot compile, or a builtin it does not
  know, makes it refuse the whole Settings and keep its version. Patterns use the syntax RE2,
  Rust's `regex` and NSRegularExpression share; the hub compiles them with `regex` before
  storing a Settings and refuses one that does not compile.
- `send` and `answer` text never starts with `!` or `/` (`unsafe_text`): a leading `!` runs a
  shell command no hook sees, and `/` runs a slash command. `compact` and `clear` are fixed
  verbs instead.
- **Queue rules** (the runtime's, stated in the `.proto` under `VERB_SEND`): `after_turn`
  must be the session's latest turn or the send is refused (`wrong_turn`) unless `queued`; a
  queued prompt is typed only after the turn before it ends cleanly; any other end pauses
  the queue, and the queue's state is on `SessionState` so a snapshot restores it.
- **Undo** is the app holding a send for five seconds before posting it. After posting, an
  order not yet sent down the stream can be withdrawn (`DELETE /api/fabric/orders/{id}`;
  the hub may always drop traffic, so this needs no signature); a queued prompt already at
  the runtime is dropped by a signed `cancel`.

### 6. Limits, and which side enforces each

| Limit | Default | Enforced by | Why there |
|---|---|---|---|
| Messages per pair of sessions since the person last ordered either | 12 | Hub | It routes every message. |
| Agent-started turns per hour | 20 | Runtime | It does the waking. |
| Wakes of one session per hour | 4 | Runtime | Same. |
| No automatic wake of an idle session at ≥ 80% context, untouched 30 minutes | on | Runtime | It knows the context and the idle time. |

The numbers travel in the signed `Settings`, which the hub reads in the clear. Continuing a
paused pair is a human-only HTTP route and is **not** signed: the hub is the only enforcer of
that limit, so a signature it checks itself would protect nothing.

### 7. Messages between agents

- **Routing never guesses** (asked for by Tom: how is an agent stopped from messaging the
  wrong session?). In order:
  1. `reply_to` set: the receiver is the sender of that message, by session id; any `to` is
     ignored. A `reply` must carry it and it must name an ask sent to the caller.
  2. `to` with a `/` is an address, matched exactly against the owner's live sessions. Several
     matches (`DELIVERY_AMBIGUOUS`), only an exited one (`DELIVERY_RECEIVER_EXITED`) or none
     (`DELIVERY_NO_SUCH_ADDRESS`) store nothing and answer with `candidates`: the sessions the
     caller may have meant (address, session id, project, branch, state), never a pick.
  3. Otherwise `to` is a fabric's exact name, and only a `note` may go to a fabric.
- **The sender sees where it landed**: each `Sent` carries the resolved recipient (session
  id, address, project, branch, state, last line), so a wrong guess shows at once.
- **Bound to the session, end to end**: a message stores the receiver's (runtime, session id);
  its nudge names that session id; `inbox` returns only mail whose receiver is the caller's
  own session id, and each `Mail` carries `to_session_id` (which Workbox checks before
  handing it over) and the sender's address and session id. If an address is later reused,
  the message does not follow it.
- **One person's fleet only**: every resolution (address, fabric, message id) runs in the
  runtime owner's `Scope::User`. No path reaches another person's session, and the isolation
  suite proves it for `send`, `who`, `inbox` and `inbox_ack`.
- **Storage**: one row per receiver. The body is deleted when the receiver's runtime
  acknowledges it (`inbox_ack`), or after 24 hours unread, when it bounces. The envelope
  (who, to whom, kind, when, read, answered) stays 7 days, which is what the pair limit and
  the conversations view need.
- **Bounces** arrive in the sender's inbox as `MESSAGE_KIND_BOUNCE`, `reply_to` naming the
  message: unread after 24 hours, the receiver removed from the fleet, the runtime reporting
  the receiver exited after the message was stored, or the person saying stop to a paused
  pair. A refusal at send time (above) is an answer, not a bounce.
- **Nudges**: one per message, re-sent on reconnect until a Result answers it. `may_wake` is
  true for ask and reply, false for note and bounce of a note. A `cold` result becomes a
  "wake to answer?" item on the phone; an ask to an asleep or exited session becomes an
  "ask waiting" item, since only the person can resume it.
- **Limits**: 8,000 characters a body, 100 unread per receiver (`inbox_full`), the pair limit
  above. A paused pair holds new messages (`DELIVERY_PAUSED`) until continue (deliver) or
  stop (bounce); either restarts the pair's count. The person ordering either session
  (`send`, `answer`, `wake`) restarts the count of a pair that is not paused; a paused pair
  waits for its own answer.
- An agent's message never carries authority and never reaches a terminal: its text reaches
  a session only as the result of `inbox`.

### 8. Pairing

The runtime shows a code (`taisce-pair:?v=1&r=…&p=…&k=…&s=…&h=…`: runtime id, pairing id,
the runtime's public key, a 32-byte secret, the hub). The phone seals a signed
`PairingClaim` (its public key and name) with a key derived from the secret (HKDF-SHA256,
salt the pairing id, info `taisce/pairing/v1`; AES-256-GCM, additional data the runtime id)
and posts it. The hub relays it (`PairingOffer`) without being able to read or replace it; the
runtime opens it, checks the claim's signature and ids, shows the fingerprint, and the person
confirms at the Mac. The runtime answers with a `Pairing` event and the device appears in
`RuntimeStatus.devices`. A code lives 10 minutes. Removing a device happens at the Mac.

The runtime's own key reaches the phone in the code, never through the hub: it is what the
phone checks approval and question signatures against.

### 9. Text off and notification privacy are hub settings

Per project, `text_off` and `private_notifications` are stored by the hub and set over HTTP
by the person. They change only what the hub stores and pushes, never what a runtime does,
so they are not signed. With `text_off`, turns, `last_line` and `note` are not written to the
store (the live stream still carries them while the Mac is awake). Three things are kept for
as long as they must be and no longer: an order's payload until the runtime has it, a
message's body until it is read, and an open attention item's text and signed request until
it is cleared (the phone has to show and verify it to answer it). Turning Text off deletes
the project's stored text at once, except those three while they are still needed.

### 10. Notifications

ADR 0007 §8 sent no text "until the switch exists". The switch now exists, and the design's
decision (store, with previews) applies: alerts carry text unless the project has
`private_notifications`.

| Push `kind` | When | Title / body | Category | Collapse id |
|---|---|---|---|---|
| `fabric_session` (as now) | needs you, stalled | address / the last line, or the state's words when private | `FABRIC_SESSION` | `fabric:<session>` |
| `fabric_session` | a turn finishes | one passive notification: "N finished" / the addresses | `FABRIC_FINISHED` | `fabric:finished` |
| `fabric_attention` | an approval, a question, a wake offer, a paused pair, an ask waiting | address / the command or question, or "Needs your approval" when private | `FABRIC_APPROVAL`, `FABRIC_QUESTION`, `FABRIC_WAKE`, `FABRIC_PAIR`, `FABRIC_ASK` | `fabric:<item id>` |
| `fabric_order` | an order was refused or failed | address / the reason in words | `FABRIC_ORDER` | `fabric:order:<order id>` |

- `fabric_attention` carries `runtime_id`, `session_id`, `request_id`, `attention_kind` and,
  for a question that is not private, `choices: [{id, label}]`. The phone must still fetch and
  verify the signed request before acting: the push is a pointer, not the thing approved.
- While `RuntimeStatus.present` is true, the hub sends no visible push for that runtime's
  sessions, except a turn started from the phone finishing and a followed session.
- A session the person follows alerts on every finished turn, alone, not in the summary.
- The 30-second-per-state limit of ADR 0007 §5b and snooze (§8a) apply to every kind.

### 11. The viewer API

Every route is human-only, exactly like ADR 0007 §7: the person's own app or web session;
connector 403, PAT 401, LOCAL 404, never MCP. Errors are `{"error": "<words>", "code":
"<code>"}`. Bytes travel as standard base64 (with padding). Times are unix seconds. Ids are
lowercase UUIDs. A resource that is not the caller's is 404.

**Reading**

| Route | Answers |
|---|---|
| `GET /api/fabric/sessions` | As now. Each session gains `fabric_id`, `turn_id`, `queue`, `queue_paused`, `following`, `text_off`; each runtime gains `held`, `held_since`, `settings_version`, `present`, `public_key`, `devices: [{device_id, fingerprint, name, public_key, paired_at, counter}]`; the body gains `attention` (the open items). |
| `GET /api/fabric/sessions/{runtime_id}/{session_id}/turns?before=&limit=` | `{turns, more}`, newest first; `before` is a turn's `ended`; limit 1–100, default 30. |
| `GET /api/fabric/turns?since=&limit=` | Every session's turns ended after `since`, oldest first, with `address`: "Since you last looked". Limit up to 200. |
| `GET /api/fabric/attention` | `{items}`: open attention items, newest first. |
| `GET /api/fabric/orders?runtime_id=&session_id=&before=&limit=` | `{orders}`: the audit trail, newest first, 7 days. |
| `GET /api/fabric/orders/{id}` | One order. |
| `GET /api/fabric/settings` | `{version, issued_at, defaults, settings, signed, runtimes: [{runtime_id, settings_version}]}`. With nothing signed yet: version 0, `defaults: true`, `settings` = `default-settings.json`, `signed: null`. |
| `GET /api/fabric/fabrics` | `{fabrics}`: the named fabrics from settings, then each project's own, with their sessions. |
| `GET /api/fabric/projects` | `{projects: [{name, text_off, private_notifications, sessions}]}`. |
| `GET /api/fabric/pairing/{pairing_id}` | `{pairing_id, runtime_id, state, device_id, detail}`; state `pending`, `sent`, `paired`, `rejected`, `expired`, `invalid`. |
| `GET /api/fabric/conversations` | `{conversations}`, most recent first. |
| `GET /api/fabric/conversations/{id}?limit=` | `{conversation, messages}`, newest first. |
| `GET /api/fabric/stream` | As now, plus the events below. |

**Changing**

| Route | Body | Answers |
|---|---|---|
| `POST /api/fabric/orders` | `{payload, device_id, signature}` | 202 `{order}`. The same order id with the same bytes again: 200, the same order. |
| `DELETE /api/fabric/orders/{id}` | | 200 `{order}` withdrawn; 409 `already_sent`. |
| `PUT /api/fabric/settings` | `{payload, device_id, signature}` | 200 the settings view; 409 `stale_settings` with `version`. |
| `POST /api/fabric/pairing` | `{runtime_id, pairing_id, sealed}` | 202 the pairing view. |
| `PUT /api/fabric/projects/{name}` | `{text_off?, private_notifications?}` | 200 the project. |
| `POST`/`DELETE /api/fabric/sessions/{runtime_id}/{session_id}/follow` | | 200 the session. |
| `POST`/`DELETE …/snooze` | as now | as now |
| `DELETE /api/fabric/attention/{id}` | | 204: dismisses a hub item (wake offer, ask waiting); 409 for a runtime's item, which an order answers. |
| `POST /api/fabric/conversations/{id}/continue` | | 200: the pair's count restarts and held messages go. |
| `POST /api/fabric/conversations/{id}/stop` | | 200: held messages bounce to their sender ("your person stopped this exchange"); the pair's count restarts. |

**Order checks at the hub**, each with its code: 400 `malformed`, `non_canonical`; 404 the
runtime or session is not the caller's; 409 `replayed` (counter at or below the highest the hub
has seen for that runtime and device), `order_id_reused` (same id, other bytes); 422
`unknown_device` (not in the runtime's last device list), `bad_signature`, `expired`,
`too_far` (over 24 hours ahead), `unsafe_text`, `too_long`, `unsupported`; 429 `queue_full`
(256 undelivered orders for one runtime). Settings add 422 `invalid` (a pattern that does not
compile, a duplicate fabric name, a name with `/` or `:`, over the counts).

**Shapes** (all fields always present; `null` where none):

- *turn*: `runtime_id, session_id, address, turn_id, prompt, reply, tool_count, outcome
  {files_changed, lines_added, lines_removed, commits, pull_request_url,
  pull_request_number}, started, ended, truncated, context_used_pct, end (clean | question |
  error | interrupted | denied | unknown), source (mac | order | answer | wake | unknown),
  order_id, text_off`. With `text_off`, `prompt` and `reply` are empty on the routes.
- *attention item*: `id, source (runtime | hub), runtime_id, session_id, address, kind
  (approval | question | dialog | tamper | wake_offer | pair_paused | ask_waiting), text,
  choices [{id, label}], tool, guardrail_label, turn_id, raised_at, expires_at, signed
  {request, signature} (runtime items), conversation_id, message_id, from_address` (hub items).
- *order*: `id, runtime_id, session_id, device_id, counter, kind (command | hold), verb, hold,
  text, after_turn, queued, request_id, choice_id, order_ids, issued_at, expires_at,
  created_at, updated_at, state, reason, detail`. `state`: `pending` (held at the hub),
  `sent` (on the stream), `received` (acknowledged, no Result yet), `queued`, `done`,
  `refused`, `failed` (from the runtime), `expired` (the hub: its time passed before the
  runtime received it), `withdrawn`.
- *settings*: the `Settings` fields as JSON with proto names; limits written out at their
  effective values.
- *conversation*: `id, a {runtime_id, session_id, address}, b {…}, count, total, last_at,
  paused, unanswered_asks`.
- *message*: `id, conversation_id, from {runtime_id, session_id, address}, to {…}, kind (note
  | ask | reply | bounce), body (null once read, expired, or under text off), doc,
  reply_to, sent_at, read_at, state (unread | read | held | bounced), bounce_reason,
  answered`.

**New stream events**: `turn`, `attention`, `attention_cleared {id, runtime_id, session_id,
resolution}`, `order`, `settings`, `message`, `conversation`, `pairing`, `project`. The
`session` and `runtime` bodies grow as above. A client ignores an event name it does not
know. A `turn` event carries the text even under `text_off`: it is the live view.

### 12. Storage, retention and isolation

New tables in `fabric.sql`, all keyed to one person and read only in their `Scope::User`
(System for expiry, as in ADR 0007): turns, attention items, orders (with the signed bytes
and the audit trail), settings (the newest signed one), project settings, follows, pairing
offers, messages and conversations. Schema v16 is still deployed nowhere, so the tables are
added to it in place, as ADR 0007's snooze column was.

| What | Kept |
|---|---|
| Turns, orders, attention items, message envelopes, conversations | 7 days |
| A message's body | until read, 24 hours at most |
| A pairing offer | 10 minutes live; the row a day |
| Settings, project settings, follows | until changed (a follow goes with its session) |

Every new route and the `Call` rpc's reads get a `COVERAGE` entry in
`crates/daemon/src/isolation_tests.rs` proving another person sees nothing. A `Call` acts
only for sessions of the calling runtime, and `who` and `send` resolve only within that
runtime's owner's fleet: an address never reaches another person's session.

When an `answer` order to a question completes and the session's fabric is a named one with
a doc, the hub appends the decision to that doc ("2026-11-21 · `taisce/main` asked: … —
answered: …") through the review gate, attributed to `fabric`. Not under Text off.

### 13. Where it is built

| Piece | Where |
|---|---|
| Tables, and every query in the person's scope | `crates/store/src/fabric.sql`, `crates/store/src/fabric/{work,mail}.rs` (tests in `work_tests.rs`) |
| Durable events, orders, settings, pairing, projects, follow | `crates/daemon/src/fabric/record.rs` |
| What goes down a stream, and its bookkeeping | `crates/daemon/src/fabric/down.rs`; the pump in `grpc.rs` |
| Fabric tool calls, routing, the pair limit, bounces, conversations | `crates/daemon/src/fabric/mail.rs` |
| Viewer routes and stream events | `crates/daemon/src/fabric/routes.rs` |
| What a push says | `crates/daemon/src/fabric/mod.rs` (`alert_payload`), `hub.rs` (`finish_alerts`: presence, snooze, privacy) |
| SIGNING | `crates/runtime-proto/src/signing.rs` |
| The fake runtime (key, pairing, order checks, mail, calls) | `crates/runtime-proto/src/fake/work.rs`; `examples/demo-work-script.json` |
| Tests | `crates/daemon/src/fabric/work_e2e.rs` (end to end), `isolation_tests.rs` (`agent_fabric_work_is_its_owners_alone`), `crates/runtime-proto/tests/signing_vectors.rs` |

- Housekeeping runs every 60 seconds: orders that never reached their runtime before their
  expiry become `expired` (their person is told), unread mail older than a day bounces, and
  rows past retention go.
- The hub's own attention items have ids `wake:<message>`, `ask:<message>` and
  `pair:<conversation>:<time>`.
- A runtime refusing a Settings (`Result.settings_version`, REFUSED) is logged; the phone sees
  it as the runtime's `settings_version` staying behind (`GET /api/fabric/settings`).
- A refused or failed order alerts its person wherever they are (it was their doing), except
  `cancelled`.

## Alternatives rejected

- **Agent messages on the Attach stream**: §3.
- **Canonical JSON or a custom text format for signing**: §4.
- **Settings as counted, expiring orders** (the design's `Policy` and `FabricEdit`): §5.
- **One order addressed to every runtime** for Hold all and settings: an order names its
  runtime so a relay cannot send it to another Mac; settings are fleet-wide by nature and
  versioned instead.
- **Signing the pair-continue and Text off switches**: §6, §9.
- **The runtime remembering each order's result to answer repeats**: the hub's "first Result
  wins" rule gives the same effect with one integer per device on the runtime.
- **Approvals hashing only the command text**: §4; the request hash binds more for nothing.

## Decided after review (2026-11-21)

- **Text off still stores what must reach a sleeping Mac.** An order's signed payload and a
  message's body are stored until delivered or read, because the Mac may be asleep when they
  are sent; nothing of them stays after. Turns, attention text, `last_line` and `note` are not
  written for a Text off project. Accepted, and said in Trust and Safety.
- **Approvals hash the whole signed request**, not only the command text (§4). Accepted.
- **Removing a lost device from the phone** would need a signed order to each runtime. Deferred:
  in these slices a device is removed at the Mac.
- **`TAMPER` items inform only**; a held session is released at the Mac. Accepted.
- **Notification buttons for a question's choices** are the app's: categories registered at
  run time where they can carry the labels, a Notification Service Extension only if needed.

## Open questions

- Workbox to confirm: the `GitOutcome` fields, `TurnEnd`, and whether presence (`present`) is
  known reliably enough to suppress pushes.
