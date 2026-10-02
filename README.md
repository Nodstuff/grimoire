# Taisce

A knowledge system for people who work with AI agents. Your notes are markdown
blocks in one SQLite file; humans and agents write through the same **review gate**.
It runs as a personal server (SERVER mode: OAuth + passkeys behind a TLS proxy, with
native Apple clients) or as the Mac app's local daemon (LOCAL mode). A server can host a
household: each person has their own docs and workspaces, and shares a workspace with
others as editor or viewer (`taisce workspace share`; see `docs/adr/0004-multi-user-workspaces.md`).

**Download:** [latest release](https://github.com/Nodstuff/grimoire/releases/latest)
(macOS, Apple Silicon, signed + notarized; SHA-256 in each release’s notes).
Open the dmg, drag Grimoire.app to Applications (the desktop app keeps its pre-rename
bundle name until the Mac Catalyst app replaces it). Data lives in `~/.grimoire`, the
daemon's default `--db` dir; it is not moved by the rename.
The app checks for updates daily (◈ menu → *Check for updates…*); updates are
minisign-verified against the key in `crates/shell/tauri.conf.json`.

## What it is

- **Blocks, not files.** Every doc is a tree of markdown blocks with stable ids; every
  change is an append-only op with a principal and a verdict. History is a query.
- **One gate for everyone.** Your autosave, an agent's proposal, an offline device's
  catch-up — all land through the same gate: current base → applied; stale base → scored; unclear →
  parked red for review. Nothing is ever hard-deleted; the Trash restores.
- **Agents as gardeners.** Scheduled or on-demand Claude Code runs (tagging, fact-audit,
  scribe, keeper, filer) propose edits under their own principal; you accept or decline
  in-editor. MCP server at `/mcp` for any agent session.
- **Ask the vault (⌘/).** A question becomes an answer doc whose every claim cites the
  exact block it came from. Retrieval is local (a static embedding model compiled in,
  one vector per block, kept current as you edit); a synthesis appears if Claude Code
  is installed. Your Claude Code memories are imported and askable too.
- **Never lose text.** Trash with restore, daily self-contained backups, unsaved edits
  that retry.

## Layout

| Path | What |
|---|---|
| `crates/store` | SQLite ledger + projection, the gate, import/export, markdown diff |
| `crates/daemon` | The `taisce` binary: MCP, JSON API, OAuth + passkeys, APNs push, change feed, gardeners, backups |
| `crates/shell` | Tauri app — a window and tray around the daemon, which it bundles as a sidecar |
| `ui/` | React + Tiptap frontend, embedded into the daemon binary |
| `docs/adr/` | Decisions: 0001 storage; 0002 federation and 0003 hot docs (both superseded) |
| `PROJECT.md` | The founding design record |

## Build

```sh
cd ui && npm install && npx vite build && cd ..
cargo build --release              # target/release/taisce
cargo test && (cd ui && npx vitest run)
./target/release/taisce serve    # http://127.0.0.1:7425
```

The UI accepts a few query params on load, all scrubbed off the URL once read:
`?admin_token=<token>` (the per-boot token beside the db, kept in sessionStorage for
`/admin/*` calls), `?doc=<uuid>[&block=<uuid>]` (open that doc, scroll to the block),
`?tab=review|runs|graph|profile|trash` (open a top-level view; `doc` wins if both are
given) and `?capture=1` (open quick capture).

## HTTP API for other local clients (0.6.2+)

Everything the UI uses is plain JSON under `/api/*` on `127.0.0.1:7425` (errors are
`{"error": …}` with HTTP 200; only `/admin/*` needs the `Taisce-Admin` token). Three
additions give an HTTP client the same contract MCP agents get:

- `POST /api/propose_markdown` `{doc_id, base_epoch, markdown, request_id?}` — the whole doc's
  new markdown is diffed against the current blocks and the minimal ops go through the gate
  (unchanged blocks keep their ids). A stale `base_epoch` returns
  `{"error":"stale_base", base_epoch, current_epoch, missed_ops, recover}`; identical markdown
  returns `{…, "verdicts": [], "note": "no changes"}`.
- `Taisce-Principal: <name>` (1–60 chars) on `POST /api/propose`, `/api/propose_markdown`,
  `/api/docs` and `/api/comment` attributes the write to that Agent principal (created on first
  use — the same rule as the MCP `as` argument; a human's name is refused) instead
  of you, so it goes through review as an agent's. Absent → the human, as before. Over MCP, pass
  `as: "<name>"` on every attributing call: MCP 2026-07-28 has no sessions, so nothing set by an
  earlier call survives to the next.
- `request_id` (any UUID) on `/api/propose` and `/api/propose_markdown`: a retry with the same
  id returns the first outcome instead of double-applying. Per principal, kept 7 days in the
  store (survives restarts).
- `GET /api/docs` answers with `Taisce-Seq: <change-journal head>` for that snapshot.
- SERVER mode (`--public-url`): identity is the bearer token's. `Taisce-Principal`, `?as=`
  and `?cwd=` are ignored; the owner's app (redirect `ie.null.taisce:`) writes as the human, a
  connector as `claude:<client>`, and MCP `as` may only name a `claude:<label>` sub-principal.
- SERVER mode personal access tokens: `taisce auth token create --name <n> [--user <id>]` (on the
  box) prints a `tsk_…` bearer once; only its SHA-256 is stored. It opens `/mcp` and nothing else
  (`/api`, `/ws` → 401 `invalid_token`), as the owner with the same pin as a connector: default
  principal `claude:<n>`, MCP `as` may name a `claude:<label>`. `taisce auth token list` shows
  name, id, created, last used (updated at most once a minute) and revoked, never the value;
  `taisce auth token revoke <name|id>` ends it. Create, revoke and first use go to the
  `taisce::audit` log.
- SERVER mode push (APNs, `push.rs`): the app registers with `POST /api/devices`
  `{token, platform: "ios", env: "sandbox"|"production", app_version}` and removes itself with
  `DELETE /api/devices/{token}` (owner's app token only). Each journal change sends every active
  device one silent nudge (`content-available`, collapse id `changes`, at most one per device per
  30 s); 410 / `BadDeviceToken` / `Unregistered` disable the token. Configure with
  `TAISCE_APNS_KEY_FILE` (or `TAISCE_APNS_KEY`, the PEM), `TAISCE_APNS_KEY_ID`,
  `TAISCE_APNS_TEAM_ID`, optional `TAISCE_APNS_TOPIC` (default `ie.null.taisce`) and
  `TAISCE_APNS_ENV` (default env for a registration naming none; `production`); unset = off.

Renamed from Grimoire in 0.9: the daemon still reads `GRIMOIRE_*` env vars (as `TAISCE_*`,
with a deprecation warning in the log) and the `X-Grimoire-Admin` / `X-Grimoire-Principal`
headers (as `Taisce-*`) for one release; both go in 0.10 (`crates/daemon/src/legacy.rs`).

The daemon's version is `GET /api/buildinfo` → `{"version": "0.6.2", "build": <stamp>}`.

`./release.sh` builds the signed, notarized dmg (needs a Developer ID certificate and a
`notarytool` keychain profile). Gardeners need [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
on the machine.

## License

MIT — see [LICENSE](LICENSE).
