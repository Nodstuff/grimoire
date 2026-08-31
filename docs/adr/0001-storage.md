# ADR 0001 — Storage: SQLite ledger + projection behind a `BlockStore` trait

- **Status**: decided
- **Who/when**: Tom + Claude, 2026-08-31 (design session; supersedes nothing)
- **Tickets**: #9, #10

## Decision

1. **SQLite** (WAL, busy_timeout, foreign_keys) is the v1 store, owned by **one daemon
   process** (PROJECT.md §3.2a). No other process ever opens the file — MCP is served
   over streamable HTTP from the daemon, never stdio.
2. **The ledger is the primary write record**: an append-only `ops` table. The `blocks`
   table is the projection of applied ops, written in the same transaction, and
   authoritative for reads. No replay machinery — if the ops table burned down, blocks
   still works as a plain wiki.
3. **One committed transaction = one epoch** (per-document, never global). Writes declare
   `base_epoch`; a stale base is an error on the direct path — the propose gate (2.5) is
   the only path for stale writes.
4. All access goes through the **`BlockStore` trait**; no SQL leaks above it. The trait
   bounds the swap cost at "reimplement the trait."
5. **All IDs are UUIDv7** (time-ordered, index-friendly), never autoincrement — federation
   tax (§6).
6. **Block ordering: fractional index** (`order_key`, base-36 fraction strings via
   `order_key::between`), not a sibling linked list. Plain `ORDER BY order_key` yields
   sibling order; concurrent inserts need no repair pass. Resolves the 2.2 in-ticket
   decision.
7. **Deletes are tombstones** (`deleted` flag), never row deletion — ops reference blocks
   forever, and provenance/history must survive.

## Alternatives

- **Postgres**: LISTEN/NOTIFY, pgvector, multi-process are real advantages — none needed
  in v1. Single-writer fear is engineered away twice over (epochs serialise writes
  architecturally; one daemon owns the file). Revisit only if a v2 need arrives.
- **libSQL**: the credible upgrade path if multi-writer ever becomes real — server-mode
  SQLite, same SQL, smallest migration. Named here so the future decision is a lookup,
  not a research project.
- **DuckDB**: OLAP-shaped, wrong for point writes and small transactions.
- **Embedded KV (redb/RocksDB)**: loses SQL, FTS5 (the 3.4 search plan), and the
  plain-queryable-tables honesty.
- **Sibling linked list** (for ordering): every insert rewrites neighbours, merge repair
  needed on concurrent inserts, and `ORDER BY` requires reconstruction. Fractional keys
  grow slowly in pathological insert patterns; acceptable, and re-keying is a compaction
  problem for later.

## Consequences

- FTS5 rides along for 3.4; sqlite-vec if embeddings ever happen.
- Time-travel/history and `diff_since` (3.6) are single SELECTs off the ledger.
- The gate (2.5) and confidence scoring (2.6) plug in at `apply`'s stale-base rejection
  point: stale writes stop being errors and become proposals.
- One daemon process is now a hard constraint the MCP transport must respect (never
  stdio) — see #21.
