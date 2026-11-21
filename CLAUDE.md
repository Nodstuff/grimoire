# Taisce (formerly Grimoire) — working rules for Claude sessions

What it is now: ONE personal server, **Taisce** (`https://taisce.null.ie`), plus clients.
- The `taisce` binary runs in two modes. **SERVER** (`--public-url` /
  `TAISCE_PUBLIC_URL` set): every data route needs an OAuth 2.1 bearer token (or, on `/api`
  only, the web UI's passkey session cookie: ADR 0004 §5d), sign-in is
  passkeys (`taisce auth enroll|list|revoke` on the box), the proxy is trusted for
  rate-limiting (`--trusted-proxy`), APNs nudges the iOS app. **LOCAL** (no public URL): the
  Mac app's daemon on `127.0.0.1:7425`, loopback-only (DNS-rebinding guard), `/admin/*` behind
  the per-boot `admin.token`.
- Clients: this web UI (embedded in the binary), the native Apple app under `apple/`
  (TaisceKit + the Taisce app; Mac Catalyst on branch `catalyst`), Claude over MCP at `/mcp`.
- The server box: https://taisce.null.ie, EC2 `i-0fe83118e910aac2b` (t4g.micro, eu-west-1,
  personal account 895102116452, `AWS_PROFILE=taisce-tom`), 443-only SG, SSM only. `infra/` holds
  OpenTofu (state in `s3://taisce-tfstate-895102116452`, never local) and the systemd units for
  taisce + portus + litestream; `infra/deploy/install.sh` runs over SSM from the release bundle in
  `s3://taisce-backups-895102116452/releases/current/` (regenerate SHA256SUMS every deploy).
  Rebuild `ui/dist` (`cd ui && npm run build`) before the zigbuild: the UI is embedded.
- Cut on 2026-10-01 and gone from the code: peer-to-peer federation (iroh, shares, mirrors,
  hubs), hot docs and agents in the room, canvases, doc freshness views, the status chip, the
  reviewer gardener. ADRs 0002/0003 are marked superseded; a store migration dropped the
  federation tables.

Repo: github.com/Nodstuff/grimoire (the repo keeps its old name; personal account `Nodstuff`; `gh`'s active
account may be flipped to the work account by other sessions — push with
`git -c credential.helper= -c 'credential.helper=!f() { echo username=Nodstuff; echo password=$(gh auth token --user Nodstuff); }; f' push origin main`).

## Multi-user (ADR 0004)
- SERVER mode is multi-user: each person is an `auth_users` row (`taisce auth user add <name>`,
  then `taisce auth enroll --user <name>`), with their own Unsorted and workspaces; a workspace
  is shared through `workspace_members` (owner | editor | viewer). LOCAL mode is single-user and
  unchanged.
- **The Scope rule**: isolation lives in the store, nowhere else. Every store call runs in a
  `taisce_store::Scope` (`User(id)` for a request, `Local` in LOCAL mode, `System` only for
  migrations / the embed indexer / backups / the box CLI / push fan-out / gardener bookkeeping).
  The daemon reaches the store only through `SharedStore::lock(scope)` /
  `with_store(&store, scope, …)`; a request's scope comes from `viewer::Viewer` (HTTP) or
  `KsMcp::scope_of` (MCP). A new store query filters with `self.vis(col)` (lists) or
  `self.see(doc)` / `self.may_write(doc)` (by id): invisible = `NotFound` (404), never 403.
- **Every new read path needs an isolation test**: add its route or tool to
  `crates/daemon/src/isolation_tests.rs` `COVERAGE` with a test that proves B sees nothing of
  A's; `every_route_and_tool_is_covered` fails the build otherwise.
- **Sharing is human-only**: `taisce workspace share|unshare|members` on the box, or
  `/api/workspaces/{id}/members` from a person's own app. Never MCP, never a connector or PAT
  token. An agent's write into a shared workspace always lands flagged (the share gate).
- **Share links are human-only too** (ADR 0005, `crates/daemon/src/shares.rs`): `/api/shares`
  answers only the person's own app or web session (connector 403, PAT 401), never MCP. The
  public `/s/{token}` routes run in `Scope::Public` (sees no doc) and read only the
  `share_links*` tables, never docs or blocks: a link serves the snapshot the owner uploaded
  (the doc alone, or with its sub-pages: `snapshot.pages` → `share_link_pages`, served at
  `/s/{token}/p/{slug}`; each page must be a descendant the maker `may_write`).
  `Scope::Public` has `user() == None` like System/Local: a new store path keyed on the user
  must deny it (`deny_public`). Tokens are HMACs under `<db dir>/share-links.key` (not in the
  db; losing it breaks every link URL). On the box its copy is the SSM SecureString
  `/taisce/share-links/key` (base64), written to the file before every start by
  `infra/deploy/share-key.sh`. Never name a table `shares` (an open drops it).

## Agent Fabric (ADR 0007)
- A **runtime** (Workbox on the Mac) dials out to the server over gRPC and reports its Claude
  sessions; the server keeps each person's fleet and shows it to their own app. The contract is
  `proto/taisce/runtime/v1/runtime.proto` (crate `taisce-runtime-proto`, compiled without
  `protoc`); the service is `crates/daemon/src/fabric/`, the tables `crates/store/src/fabric.rs`.
- gRPC is on the runtime leg only, on its own loopback port (`serve --runtime-grpc-port`, 7426 on
  the box, SERVER mode only), apart from the axum app and `require_auth`. Everything else stays
  HTTPS / SSE / APNs.
- A runtime signs in with its own credential kind (`trt_…`, `taisce fabric runtime
  add|list|revoke` on the box). It opens that service and nothing else, and no other token
  opens the service. The fleet (`/api/fabric/*`) answers only the person's own app or web
  session: connector 403, PAT 401, never MCP.
- Two more `Scope::System` uses: resolving a runtime credential to its owner, and expiring
  sessions after 7 days. Everything a stream writes runs in its owner's `Scope::User`.
- The proxy forces two rules (both observed, 2026-11-20): the gRPC route in
  `infra/deploy/portus.yaml` keeps its explicit `max_request_body_bytes` (unset means 10 MiB
  over a stream's whole life), and neither side has more than 64 unacknowledged events in
  flight (Portus closes a connection holding about 145 small DATA frames). New messages go
  through the send window, never around it.
- `cargo run -p taisce-runtime-proto --features client --example fake_runtime` drives a scratch
  daemon; never a real server. With `--script crates/runtime-proto/examples/demo-work-script.json
  --pair` it plays Slices 2–5 and then answers what comes down (orders, settings, pairing, mail)
  as a runtime does, so the app can be driven end to end without Workbox.
- Slices 2–5 (ADR 0008): durable Up events (turn, attention, result, pairing) are written
  before they are acknowledged; Down events (orders, settings, pairing offers, nudges) are
  re-sent until acknowledged. Anything carrying the person's authority is a device-signed
  `Order` or `Settings` (SIGNING in the `.proto`, `taisce_runtime_proto::signing`, vectors in
  `proto/taisce/runtime/v1/testdata/`; regenerate with `make_vectors.py`, never by hand).
  Agent tool calls use `rpc Call`, not the stream.
- The phone signs only what it verified: approval sheets, answer buttons (in the app and on a
  notification) and allowed verbs come from the Mac-signed request, never from the server's item
  or the push payload. Releasing Hold all needs Face ID; holding is one tap.
- Live end to end: `apple/App/TaisceUITests/FabricWorkboxLiveUITests` drives the app against a
  real Workbox (its Debug Preview, `WORKBOX_FABRIC=1`) and a scratch SERVER daemon
  (`TEST_RUNNER_TAISCE_UI_URL`, `…_SIGNIN_RELAY`, `…_FABRIC_PAIR_CODE`, `…_FABRIC_SESSION`); the
  pairing fingerprint must be confirmed at the Mac while it runs.
- On the box (deployed 2026-11-21): enrol a runtime by hash only (`taisce fabric runtime add
  --name <n> --hash <sha256 of trt_…>`), so the secret never crosses SSM. `revoke` is a soft
  delete (`revoked_at`) that cascades the runtime's sessions, turns, orders and pairings.

## Start here
- **[[Roadmap]]** in Taisce (under the `[[Taisce]]` tree, renamed from Grimoire on 2026-10-14) is the outstanding list — read it first in any
  new session; the daily doc carries narrative, the roadmap carries the work.
- Test with `cargo test -p taisce -p taisce-store -p taisce-runtime-proto` (the shell crate's build.rs needs the sidecar
  binary; a bare workspace `cargo test` fails in a fresh worktree). In a fresh worktree the daemon
  also needs `ui/dist/` to exist (rust-embed): `mkdir -p ui/dist && touch ui/dist/index.html` or
  copy the main checkout's `ui/dist` before `cargo test`. Then `cd ui && npx vitest run`.
  Print `rg -c 'test result: ok'` AND `rg -c 'test result: FAILED'` in the foreground before releasing.
- Before any push/release: `git branch --show-current` must be `main` (another session may have
  switched this shared checkout); `release.sh` enforces this and tags the exact HEAD.
- Big builds: a fresh general-purpose agent (forks inherit the whole conversation and die on
  context), in an isolated worktree if a release may build concurrently; commit as you go.
- The server target must keep building: `cargo zigbuild --release -p taisce --target
  aarch64-unknown-linux-gnu`; ship it with the SSM deploy above. Never point a test
  daemon at the server or at `~/.grimoire`.

## Where the truth is
- **System docs**: the `[[Taisce]]` doc tree in Taisce itself (MCP server registered as `taisce`); design records and work logs live under its `History` folder. Write them to the `Technical Style Guide` (the `style_guide` MCP tool serves it):
  Architecture, Review Gate, Gardeners, Agent Guide, Using the App, Development, Roadmap
  (History holds the removed features: Federation, Hot Docs, Doc freshness). `PROJECT.md` is the founding
  design record and its status section says what has since been built and cut.
- **Decisions**: `docs/adr/` (0001 storage; 0002 federation and 0003 hot docs, both superseded).
- **Backlog**: GitHub issues; milestones M1–M9 are complete.

## Build / test / ship
- Latest version of everything, always — verify on crates.io / npm, never from memory.
  Lockstep families (Tiptap) move together. Toolchains count (rustup update).
- `cargo test` (daemon + store) and `cd ui && npx vitest run` before committing.
- **Before any live smoke test: `cd <repo> && cargo build --release`** — `cargo test`/debug
  builds do not refresh `target/release/taisce`; a stale scratch daemon has burned hours.
  The shell cwd drifts into `ui/` after npm commands; always `cd` to the repo root first.
- Scratch daemons (LOCAL mode): `HOME=<dir> ./target/release/taisce --db <dir>/ks.db --port 751x serve`
  (`--port` is global; the UI is embedded, no `TAISCE_UI_DIST` needed). Admin routes need
  `-H "Taisce-Admin: $(cat <dir>/admin.token)"`. Add `--public-url https://…` for SERVER
  mode. Never point one at `~/.grimoire` (still the default `--db` dir; the rename moved no data).
- Renamed from Grimoire in 0.9: `GRIMOIRE_*` env vars and `X-Grimoire-Admin` /
  `X-Grimoire-Principal` are still read for one release (`crates/daemon/src/legacy.rs`, with a
  log warning for env); delete that file in 0.10. OAuth scope stays `grimoire` (issued tokens).
  The legacy Tauri app keeps `Grimoire.app`, `ie.null.grimoire`, `grimoire-notary` and
  `~/.grimoire-release/updater.key` so installs update in place.
- `scripts/mcp-smoke.py` drives a scratch daemon over MCP end to end.
- `./deploy.sh` = fast unsigned local deploy of the Mac app (restarts the daemon on 7425 —
  in-flight gardener runs get orphaned). `./release.sh` = signed + notarized dmg.

## Writing to Taisce over MCP (this repo's own docs live in it)
- 17 tools (AX 2): `find_doc`, `orient`, `read_doc`, `edit_doc`, `append`, `propose_markdown`,
  `propose`, `diff_since`, `search`, `grep`, `related`, `create_doc`, `doc_op`, `add_comment`,
  `resolve`, `proposals`, `style_guide` (the caller's own doc titled "Technical Style Guide"). `edit_doc(old, new)` and `append(markdown, to, create_missing)` need no
  epoch; `read_doc` is text (`doc <id> · epoch N · path` + markdown; `refs: true` for `^abc123`).
- Attribution: pass `as: "claude:grimoire-<task>"` on every attributing call (`edit_doc`, `append`,
  `propose`, `propose_markdown`, `create_doc`, `doc_op`, `add_comment`, `resolve`, `proposals`), or
  register the server as `/mcp?as=<name>` / `/mcp?cwd=${PWD}` (→ `claude:<dirname>`) or send
  `Taisce-Principal`. Precedence: tool `as` > header > `?as=` > `?cwd=` > shared `claude`.
  MCP 2026-07-28 has no sessions (`identify` is gone). A running Claude Code session needs `/mcp`
  reconnect to see tools or parameters added by a daemon you just deployed.

## MCP from Claude Code: one PAT via headersHelper
- Every Claude account (the default one and each workbox account under `~/.claude-accounts/`) uses
  ONE personal access token, named `claude-code`, instead of an OAuth sign-in per account. Each account's
  `.claude.json` has `"taisce": {"type": "http", "url": "https://taisce.null.ie/mcp",
  "headersHelper": "/Users/tmeaney/.claude/taisce-mcp-headers.sh"}` (workbox seeds new accounts
  from `~/.claude.json`). The helper reads the Keychain item `taisce-mcp-token`; the token is
  never in a file.
- Minted on the laptop, registered by hash so the secret never reaches the box or SSM history:
  `taisce auth token create --name claude-code --hash <sha256 of tsk_…>` (run as `taisce` on the
  box). Rotate: mint a new one into the Keychain with `security add-generic-password -U`, register
  it, then `taisce auth token revoke <old id>`. `auth token list` shows last use.
- PATs open `/mcp` only (401 on `/api`, the web UI and the phone keep OAuth/passkeys). Writes
  default to `claude:claude-code`; `as: "claude:<project>-<task>"` still labels each call.

## Traps
- `window.alert`/`confirm` are silent no-ops in Tauri's WKWebView — use inline UI.
- Mac Catalyst answers every text-style size at `.large`: `.dynamicTypeSize(…)` and
  `UIFont.preferredFont(…, compatibleWith:)` change nothing there. Doc text scales by hand
  (`DocTextSize`, `docScale`, `DocScaleTrait` in `apple/App/Taisce/Presentation/TextSize.swift`).
- The Mac app is unsandboxed: its hosted unit tests run as `ie.null.taisce` with the real
  Keychain and `UserDefaults` domain. `AppPaths.isTestHost` keeps them off the real caches and
  stops the host booting or migrating; keep it that way. The old sandbox container is
  readable only when the app is launched by Launch Services (`open`, Finder), never from a
  shell (macOS app-data protection), so a migration run from a shell logs EPERM and retries.
- Markdown-it's commonmark preset has no tables.
- Old databases still hold `canvas_scene` blocks and `remote` principals: both stay valid in
  the schema (the editor, export and retrieval skip canvas blocks; new ones are refused).
- Anything settable that weakens the gate (review policy, gardeners) is a human surface:
  never expose it over MCP.
