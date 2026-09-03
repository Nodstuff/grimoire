# Grimoire

A local-first knowledge system for people who work with AI agents. Your notes are
markdown blocks in a SQLite file on your Mac; humans and agents write through the
same **review gate**; you can share a folder with another Grimoire over the open
internet and edit it live together.

**Download:** [latest release](https://github.com/Nodstuff/grimoire/releases/latest)
(macOS, Apple Silicon, signed + notarized; SHA-256 in each release’s notes).
Open the dmg, drag Grimoire to Applications. Data lives in `~/.grimoire`.
The app checks for updates daily (◈ menu → *Check for updates…*); updates are
minisign-verified against the key in `crates/shell/tauri.conf.json`.

## What it is

- **Blocks, not files.** Every doc is a tree of markdown blocks with stable ids; every
  change is an append-only op with a principal and a verdict. History is a query.
- **One gate for everyone.** Your autosave, an agent's proposal, a colleague's edit — all
  land through the same gate: current base → applied; stale base → scored; unclear →
  parked red for review. Nothing is ever hard-deleted; the Trash restores.
- **Agents as gardeners.** Scheduled or on-demand Claude Code runs (tagging, review,
  fact-audit) propose edits under their own principal; you accept or decline in-editor.
  MCP server at `localhost:7425/mcp` for any agent session.
- **Federation.** Share a subtree with another instance over [iroh](https://iroh.computer)
  — owner-authoritative mirrors, per-share `view`/`propose` grants and trust tiers,
  deny-by-default, mDNS on the LAN, relays anywhere else.
- **Hot docs.** Go live on any doc: a Yjs session at the edge, one clean commit through
  the gate when it ends. Works across instances.

## Layout

| Path | What |
|---|---|
| `crates/store` | SQLite ledger + projection, the gate, import/export, markdown diff |
| `crates/daemon` | The `grimoire` binary: MCP, JSON API, federation, hot sessions, gardeners, backups |
| `crates/shell` | Tauri app — a window and tray around the daemon, which it bundles as a sidecar |
| `ui/` | React + Tiptap + React Flow frontend, embedded into the daemon binary |
| `docs/adr/` | Decisions: 0001 storage, 0002 federation, 0003 hot docs |
| `PROJECT.md` | The founding design record |

## Build

```sh
cd ui && npm install && npx vite build && cd ..
cargo build --release              # target/release/grimoire
cargo test && (cd ui && npx vitest run)
./target/release/grimoire serve    # http://127.0.0.1:7425
```

The UI accepts a few query params on load, all scrubbed off the URL once read:
`?admin_token=<token>` (the per-boot token beside the db, kept in sessionStorage for
`/admin/*` calls), `?doc=<uuid>[&block=<uuid>]` (open that doc, scroll to the block),
`?tab=review|runs|graph|sharing|profile|trash` (open a top-level view; `doc` wins if
both are given) and `?join=<payload>` (a `grimoire://join/…` link routed by the shell).

## HTTP API for other local clients (0.6.2+)

Everything the UI uses is plain JSON under `/api/*` on `127.0.0.1:7425` (errors are
`{"error": …}` with HTTP 200; only `/admin/*` needs the `X-Grimoire-Admin` token). Three
additions give an HTTP client the same contract MCP agents get:

- `POST /api/propose_markdown` `{doc_id, base_epoch, markdown, request_id?}` — the whole doc's
  new markdown is diffed against the current blocks and the minimal ops go through the gate
  (unchanged blocks keep their ids). A stale `base_epoch` returns
  `{"error":"stale_base", base_epoch, current_epoch, missed_ops, recover}`; identical markdown
  returns `{…, "verdicts": [], "note": "no changes"}`.
- `X-Grimoire-Principal: <name>` (1–60 chars) on `POST /api/propose`, `/api/propose_markdown`,
  `/api/docs` and `/api/comment` attributes the write to that Agent principal (created on first
  use, the MCP `identify` rule) instead of you, so it goes through review as an agent's. Absent
  → the human, as before.
- `request_id` (any UUID) on `/api/propose` and `/api/propose_markdown`: a retry with the same
  id returns the first outcome instead of double-applying. Per principal, in-memory.

The daemon's version is `GET /api/buildinfo` → `{"version": "0.6.2", "build": <stamp>}`.

`./release.sh` builds the signed, notarized dmg (needs a Developer ID certificate and a
`notarytool` keychain profile). Gardeners need [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
on the machine.

## License

MIT — see [LICENSE](LICENSE).
