# Grimoire — working rules for Claude sessions

Repo: github.com/Nodstuff/grimoire (personal account `Nodstuff`; `gh`'s active
account may be flipped to the work account by other sessions — push with
`git -c credential.helper= -c 'credential.helper=!f() { echo username=Nodstuff; echo password=$(gh auth token --user Nodstuff); }; f' push origin main`).

## Start here
- **[[Roadmap]]** in Grimoire (under the `[[Grimoire]]` tree) is the outstanding list — read it first in any
  new session; the daily doc carries narrative, the roadmap carries the work.
- Test with `cargo test -p grimoire -p grimoire-store` (the shell crate's build.rs needs the sidecar
  binary; a bare workspace `cargo test` fails in a fresh worktree) and `cd ui && npx vitest run`.
  Print `rg -c 'test result: ok'` AND `rg -c 'test result: FAILED'` in the foreground before releasing.
- Before any push/release: `git branch --show-current` must be `main` (another session may have
  switched this shared checkout); `release.sh` enforces this and tags the exact HEAD.
- Big builds: a fresh general-purpose agent (forks inherit the whole conversation and die on
  context), in an isolated worktree if a release may build concurrently; commit as you go.
- The real hub runs on EC2 (`i-01cfd84ace1e1e2a4`, Qompass-Dev, SSM only, zero ingress); update
  it with `cargo zigbuild --release -p grimoire --target aarch64-unknown-linux-gnu` shipped via a
  throwaway presigned S3 object. Never point a test daemon at it or at `~/.grimoire`.

## Where the truth is
- **System docs**: the `[[Grimoire]]` doc tree in Grimoire itself (MCP server `grimoire`):
  Architecture, Review Gate, Gardeners, Federation, Hot Docs, Agent Guide, Using the App,
  Development, Roadmap. These are current; `PROJECT.md` is the founding design record and its
  status section says what has since been built.
- **Decisions**: `docs/adr/` (0001 storage, 0002 federation, 0003 hot docs).
- **Backlog**: GitHub issues; milestones M1–M9 are complete.

## Build / test / ship
- Latest version of everything, always — verify on crates.io / npm, never from memory.
  Lockstep families (Tiptap) move together. Toolchains count (rustup update).
- `cargo test` (daemon + store) and `cd ui && npx vitest run` before committing.
- **Before any live smoke test: `cd <repo> && cargo build --release`** — `cargo test`/debug
  builds do not refresh `target/release/grimoire`; a stale scratch daemon has burned hours.
  The shell cwd drifts into `ui/` after npm commands; always `cd` to the repo root first.
- Scratch daemons: `GRIMOIRE_IDENTITY_FILE=<dir>/identity.key ./target/release/grimoire --db <dir>/ks.db --port 751x serve [--hub --name X]`
  (`--port` is global; the UI is embedded, no `GRIMOIRE_UI_DIST` needed). Admin routes need
  `-H "X-Grimoire-Admin: $(cat <dir>/admin.token)"`. Never point one at `~/.grimoire`.
- Hub changes: run `scripts/smoke-hub-slice2.sh` (3 daemons, 33 checks) before shipping.
- `./deploy.sh` = fast unsigned local deploy (restarts the production daemon on 7425 —
  in-flight gardener runs get orphaned). `./release.sh` = signed + notarized dmg.
- Federation smoke across two daemons is the canonical end-to-end test (join → pull →
  propose → accept → pull back). Two browser windows on one daemon test hot sessions.

## Traps
- `window.alert`/`confirm` are silent no-ops in Tauri's WKWebView — use inline UI.
- Markdown-it's commonmark preset has no tables; mirrors are read-only at the store layer;
  hot docs freeze the epoch (propose surfaces refuse — retry after the session).
- Anything settable that weakens the gate (review policy, shares, trust, gardeners) is a
  human surface: never expose it over MCP.
