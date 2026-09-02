# Grimoire

A local-first knowledge system for people who work with AI agents. Your notes are
markdown blocks in a SQLite file on your Mac; humans and agents write through the
same **review gate**; you can share a folder with another Grimoire over the open
internet and edit it live together.

**Download:** [v0.5.0 dmg](https://github.com/Nodstuff/grimoire/releases/tag/v0.5.0)
(macOS, Apple Silicon, signed + notarized) — SHA-256 `12551dc09364a3c3b9d7a9470155935fe75fa19d7305cf8cc1fa195ebf709809`.
Open the dmg, drag Grimoire to Applications. Data lives in `~/.grimoire`.

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

`./release.sh` builds the signed, notarized dmg (needs a Developer ID certificate and a
`notarytool` keychain profile). Gardeners need [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
on the machine.

## License

MIT — see [LICENSE](LICENSE).
