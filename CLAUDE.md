# Grimoire — working rules for Claude sessions

Repo: github.com/Nodstuff/grimoire (personal account `Nodstuff`; `gh`'s active
account may be flipped to the work account by other sessions — push with
`git -c credential.helper= -c 'credential.helper=!f() { echo username=Nodstuff; echo password=$(gh auth token --user Nodstuff); }; f' push origin main`).

## Where the truth is
- **System docs**: the `[[Grimoire]]` doc tree in Grimoire itself (MCP server `grimoire`):
  Architecture, Review Gate, Gardeners, Federation, Hot Docs, Agent Guide, Using the App,
  Development. These are current; `PROJECT.md` is the founding design record and its
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
- Scratch daemons: `GRIMOIRE_IDENTITY_FILE=<dir>/identity.key ./target/release/grimoire --db <dir>/ks.db serve --port 751x`
  with `GRIMOIRE_UI_DIST=<repo>/ui/dist`. Never point one at `~/.grimoire`.
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
