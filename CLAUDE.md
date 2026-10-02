# Taisce (formerly Grimoire) — working rules for Claude sessions

What it is now: ONE personal server, **Taisce** (`https://taisce.null.ie`), plus clients.
- The `taisce` binary runs in two modes. **SERVER** (`--public-url` /
  `TAISCE_PUBLIC_URL` set): every data route needs an OAuth 2.1 bearer token, sign-in is
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

## Start here
- **[[Roadmap]]** in Taisce (under the `[[Grimoire]]` tree — doc titles are data, not renamed) is the outstanding list — read it first in any
  new session; the daily doc carries narrative, the roadmap carries the work.
- Test with `cargo test -p taisce -p taisce-store` (the shell crate's build.rs needs the sidecar
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
- **System docs**: the `[[Grimoire]]` doc tree in Taisce itself (MCP server registered as `taisce`):
  Architecture, Review Gate, Gardeners, Agent Guide, Using the App, Development, Roadmap
  (its Federation and Hot Docs pages describe removed features). `PROJECT.md` is the founding
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
- 16 tools (AX 2): `find_doc`, `orient`, `read_doc`, `edit_doc`, `append`, `propose_markdown`,
  `propose`, `diff_since`, `search`, `grep`, `related`, `create_doc`, `doc_op`, `add_comment`,
  `resolve`, `proposals`. `edit_doc(old, new)` and `append(markdown, to, create_missing)` need no
  epoch; `read_doc` is text (`doc <id> · epoch N · path` + markdown; `refs: true` for `^abc123`).
- Attribution: pass `as: "claude:grimoire-<task>"` on every attributing call (`edit_doc`, `append`,
  `propose`, `propose_markdown`, `create_doc`, `doc_op`, `add_comment`, `resolve`, `proposals`), or
  register the server as `/mcp?as=<name>` / `/mcp?cwd=${PWD}` (→ `claude:<dirname>`) or send
  `Taisce-Principal`. Precedence: tool `as` > header > `?as=` > `?cwd=` > shared `claude`.
  MCP 2026-07-28 has no sessions (`identify` is gone). A running Claude Code session needs `/mcp`
  reconnect to see tools or parameters added by a daemon you just deployed.

## MCP from Claude Code: one PAT via headersHelper
- Every Claude Code account can share one personal access token instead of an OAuth sign-in each.
  Mint it on the box over SSM: `taisce --db <server db> auth token create --name claude-code`
  (prints `tsk_…` once; `auth token list` / `auth token revoke <name|id>`). It opens `/mcp` only.
- Store it in a 0600 file (`~/.config/taisce/token`) and register the server with a
  `headersHelper` so the secret never sits in the MCP config:
  `"taisce": {"type": "http", "url": "https://taisce.null.ie/mcp", "headersHelper":
  "printf '{\"Authorization\":\"Bearer %s\"}' \"$(cat ~/.config/taisce/token)\""}`.
- Writes default to `claude:<token name>`; `as: "claude:<project>-<task>"` still labels each call.

## Traps
- `window.alert`/`confirm` are silent no-ops in Tauri's WKWebView — use inline UI.
- Markdown-it's commonmark preset has no tables.
- Old databases still hold `canvas_scene` blocks and `remote` principals: both stay valid in
  the schema (the editor, export and retrieval skip canvas blocks; new ones are refused).
- Anything settable that weakens the gate (review policy, gardeners) is a human surface:
  never expose it over MCP.
