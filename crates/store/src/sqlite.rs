use crate::gate::{Scored, score_stale_op};
use crate::scope::Scope;
use crate::tenancy;
use crate::types::*;
use crate::{BlockStore, Result, StoreError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;
use uuid::Uuid;

const SCHEMA: &str = include_str!("schema.sql");
const WORKSPACES_SCHEMA: &str = include_str!("workspaces.sql");

pub struct SqliteStore {
    pub(crate) conn: Connection,
    /// Who every call runs for (ADR 0004). A store opened directly by a
    /// process (the CLI, tests) is that process's System context; the
    /// daemon only reaches one through `SharedStore::lock(scope)`.
    pub(crate) scope: Scope,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// A consistent single-file copy of the database at `path` (no -wal/-shm
    /// pair to keep together), via `VACUUM INTO`. Runs inside SQLite's own
    /// read transaction, so concurrent writers are neither blocked nor
    /// captured half-way. The target must not exist.
    pub fn backup_to(&self, path: &Path) -> Result<()> {
        if path.exists() {
            return Err(StoreError::InvalidOp(format!(
                "backup target exists: {}",
                path.display()
            )));
        }
        self.conn
            .execute("VACUUM INTO ?1", params![path.to_string_lossy()])?;
        Ok(())
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // WAL + NORMAL: durable across process crashes, one fsync per
        // checkpoint instead of per commit (a power loss can lose the last
        // few commits, never corrupt the file)
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate_pre_schema(&conn)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(WORKSPACES_SCHEMA)?;
        backfill(&conn)?;
        Ok(Self { conn, scope: Scope::System })
    }

    /// Set the scope (crate-private: only `SharedStore::lock` and tests in
    /// this crate change it).
    pub(crate) fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
    }

    /// Tests: run `f` with the store in `scope`, then restore it.
    #[doc(hidden)]
    pub fn with_scope_for_test<T>(&mut self, scope: Scope, f: impl FnOnce(&mut Self) -> T) -> T {
        let prev = self.scope;
        self.scope = scope;
        let out = f(self);
        self.scope = prev;
        out
    }
}

fn uuid_col(s: String, ctx: &str) -> Result<Uuid> {
    Uuid::parse_str(&s).map_err(|_| StoreError::InvalidOp(format!("bad uuid in {ctx}: {s}")))
}

/// Additive column migrations that must run before the IF-NOT-EXISTS schema.
/// The ALTERs land in one transaction (a crash mid-way used to leave a table
/// with half its new columns); the table rebuilds run after it, on their
/// own — one toggles `foreign_keys`, which is a no-op inside a transaction.
fn migrate_pre_schema(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    additive_column_migrations(&tx)?;
    tx.commit()?;
    // v7: the peer-to-peer federation tables (contacts, shares, invites,
    // offers, mirrors, hub publications/forwards/transfers, doc transfers,
    // pending joins, outbound proposals) held only federation metadata;
    // federation is gone. Children before parents: foreign keys are on and
    // a DROP is an implicit DELETE. Idempotent, so it runs on every open.
    drop_federation_tables(conn)?;
    // block_vec: the first cut referenced blocks(id) without ON DELETE
    // CASCADE, so a hard block delete failed the FK once the block had been
    // embedded. Rebuild once with the cascade.
    let block_vec_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'block_vec'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(sql) = block_vec_sql
        && !sql.to_ascii_uppercase().contains("ON DELETE CASCADE")
    {
        rebuild_block_vec_with_cascade(conn)?;
    }
    // ops: doc ops (AX slice B) need op_type values the original CHECK did
    // not allow. Rebuild once when the constraint predates them.
    let ops_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'ops'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(sql) = ops_sql
        && !sql.contains("'rename_doc'")
    {
        widen_ops_op_type_check(conn)?;
    }
    // gardeners: the `filer` kind postdates the kind CHECK on fresh installs
    // (DBs that got `kind` via ALTER carry no CHECK and need nothing).
    let gardeners_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'gardeners'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(sql) = gardeners_sql
        && sql.contains("kind IN (")
        && !sql.contains("'filer'")
    {
        widen_gardeners_kind_check(conn)?;
    }
    // workspaces (ADR 0004): names unique per owner, not server-wide
    let has_ws: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'workspaces'",
        [],
        |r| r.get(0),
    )?;
    if has_ws > 0 {
        let has_owner: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('workspaces') WHERE name = 'owner_id'",
            [],
            |r| r.get(0),
        )?;
        if has_owner == 0 {
            rebuild_workspaces_with_owner(conn)?;
        }
    }
    Ok(())
}

/// The tables federation (ADR 0002, superseded) kept, children first.
const FEDERATION_TABLES: [&str; 11] = [
    "share_invites",
    "share_offers",
    "outbound_proposals",
    "mirrors",
    "hub_publications",
    "hub_forwards",
    "hub_transfers",
    "doc_transfers",
    "pending_joins",
    "shares",
    "contacts",
];

fn drop_federation_tables(conn: &Connection) -> Result<()> {
    let present: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ({})",
            FEDERATION_TABLES.map(|t| format!("'{t}'")).join(", ")
        ),
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for t in FEDERATION_TABLES {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
    }
    // hub mode's settings went with it
    tx.execute("DELETE FROM settings WHERE key LIKE 'hub.%'", [])?;
    tx.commit()?;
    Ok(())
}

/// Rebuild `gardeners` with the kind CHECK widened to include `filer`
/// (same dance as `widen_ops_op_type_check`; ids are preserved so the
/// gardener_runs FK holds afterwards).
fn widen_gardeners_kind_check(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = (|| -> Result<()> {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE gardeners_new (
                 id            TEXT PRIMARY KEY,
                 name          TEXT NOT NULL UNIQUE,
                 kind          TEXT NOT NULL DEFAULT 'tagging'
                     CHECK (kind IN ('tagging', 'reviewer', 'auditor', 'scribe', 'keeper', 'filer')),
                 principal     TEXT NOT NULL REFERENCES principals (id),
                 scope_doc     TEXT REFERENCES docs (id),
                 task_prompt   TEXT NOT NULL,
                 bindings      TEXT NOT NULL DEFAULT '[]',
                 creds_ref     TEXT,
                 schedule      TEXT NOT NULL DEFAULT 'daily',
                 confidence_policy TEXT NOT NULL DEFAULT 'review' CHECK (confidence_policy IN ('review', 'gate')),
                 enabled       INTEGER NOT NULL DEFAULT 1,
                 created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                 owner_id      TEXT
             );
             INSERT INTO gardeners_new (id, name, kind, principal, scope_doc, task_prompt, bindings,
                                        creds_ref, schedule, confidence_policy, enabled, created_at, owner_id)
                 SELECT id, name, kind, principal, scope_doc, task_prompt, bindings,
                        creds_ref, schedule, confidence_policy, enabled, created_at, owner_id FROM gardeners;
             DROP TABLE gardeners;
             ALTER TABLE gardeners_new RENAME TO gardeners;
             COMMIT;",
        )?;
        Ok(())
    })();
    conn.pragma_update(None, "foreign_keys", true)?;
    result?;
    let has_runs: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'gardener_runs'",
        [],
        |r| r.get(0),
    )?;
    if has_runs > 0 {
        let violations: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_foreign_key_check('gardener_runs')",
            [],
            |r| r.get(0),
        )?;
        if violations > 0 {
            return Err(StoreError::InvalidOp(format!(
                "gardeners rebuild left {violations} dangling gardener_runs rows"
            )));
        }
    }
    Ok(())
}

fn additive_column_migrations(conn: &Connection) -> Result<()> {
    let has_blocks: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'blocks'",
        [],
        |r| r.get(0),
    )?;
    if has_blocks > 0 {
        let has_refers: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('blocks') WHERE name = 'refers_to'",
            [],
            |r| r.get(0),
        )?;
        if has_refers == 0 {
            conn.execute("ALTER TABLE blocks ADD COLUMN refers_to TEXT", [])?;
        }
    }
    let has_docs: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'docs'",
        [],
        |r| r.get(0),
    )?;
    if has_docs > 0 {
        let has_status: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('docs') WHERE name = 'status'",
            [],
            |r| r.get(0),
        )?;
        if has_status == 0 {
            conn.execute("ALTER TABLE docs ADD COLUMN status TEXT", [])?;
        }
        let has_sort: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('docs') WHERE name = 'sort_key'",
            [],
            |r| r.get(0),
        )?;
        if has_sort == 0 {
            conn.execute("ALTER TABLE docs ADD COLUMN sort_key TEXT", [])?;
            conn.execute(
                "ALTER TABLE docs ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let has_deleted_at: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('docs') WHERE name = 'deleted_at'",
            [],
            |r| r.get(0),
        )?;
        if has_deleted_at == 0 {
            conn.execute("ALTER TABLE docs ADD COLUMN deleted_at TEXT", [])?;
            // pre-Trash tombstones: give them a stamp so they show and restore
            conn.execute(
                "UPDATE docs SET deleted_at = created_at WHERE deleted = 1 AND deleted_at IS NULL",
                [],
            )?;
        }
        let has_verified: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('docs') WHERE name = 'verified_at'",
            [],
            |r| r.get(0),
        )?;
        if has_verified == 0 {
            conn.execute("ALTER TABLE docs ADD COLUMN verified_at TEXT", [])?;
        }
        add_column_if_missing(conn, "docs", "owner_id", "TEXT")?;
    }
    add_column_if_missing(conn, "changes", "user_id", "TEXT")?;
    let has_gardeners: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'gardeners'",
        [],
        |r| r.get(0),
    )?;
    if has_gardeners > 0 {
        let has_kind: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('gardeners') WHERE name = 'kind'",
            [],
            |r| r.get(0),
        )?;
        if has_kind == 0 {
            conn.execute(
                "ALTER TABLE gardeners ADD COLUMN kind TEXT NOT NULL DEFAULT 'tagging'",
                [],
            )?;
        }
        add_column_if_missing(conn, "gardeners", "owner_id", "TEXT")?;
    }
    Ok(())
}

/// `ALTER TABLE … ADD COLUMN` when the table exists and lacks the column.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let has_table: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if has_table == 0 {
        return Ok(());
    }
    let has: i64 = conn.query_row(
        &format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get(0),
    )?;
    if has == 0 {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

/// v8 (ADR 0004): rebuild `workspaces` without the server-wide UNIQUE on
/// `name`, with `owner_id`; names become unique per owner via the
/// `workspaces_owner_name` index (workspaces.sql). Same dance as the ops
/// rebuild: ids preserved, foreign keys off for the swap, checked after.
fn rebuild_workspaces_with_owner(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = (|| -> Result<()> {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE workspaces_new (
                 id         TEXT PRIMARY KEY,
                 owner_id   TEXT,
                 name       TEXT NOT NULL COLLATE NOCASE,
                 color      TEXT,
                 icon       TEXT,
                 sort_key   TEXT,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             );
             INSERT INTO workspaces_new (id, name, color, icon, sort_key, created_at)
                 SELECT id, name, color, icon, sort_key, created_at FROM workspaces;
             DROP TABLE workspaces;
             ALTER TABLE workspaces_new RENAME TO workspaces;
             COMMIT;",
        )?;
        Ok(())
    })();
    conn.pragma_update(None, "foreign_keys", true)?;
    result?;
    let violations: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_foreign_key_check('doc_workspace')",
        [],
        |r| r.get(0),
    )?;
    if violations > 0 {
        return Err(StoreError::InvalidOp(format!(
            "workspaces rebuild left {violations} dangling doc_workspace rows"
        )));
    }
    Ok(())
}

/// Rebuild `block_vec` with `ON DELETE CASCADE` on its blocks FK. Nothing
/// references block_vec, so the swap is copy → drop → rename, in one
/// transaction; rows whose block is already gone are dropped on the way.
fn rebuild_block_vec_with_cascade(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE block_vec_new (
             block_id TEXT PRIMARY KEY REFERENCES blocks (id) ON DELETE CASCADE,
             epoch    INTEGER NOT NULL,
             dim      INTEGER NOT NULL,
             vec      BLOB NOT NULL
         );
         INSERT INTO block_vec_new (block_id, epoch, dim, vec)
             SELECT v.block_id, v.epoch, v.dim, v.vec FROM block_vec v
             WHERE EXISTS (SELECT 1 FROM blocks b WHERE b.id = v.block_id);
         DROP TABLE block_vec;
         ALTER TABLE block_vec_new RENAME TO block_vec;
         COMMIT;",
    )?;
    Ok(())
}

/// Rebuild `ops` with the op_type CHECK widened to the doc ops (rename_doc,
/// move_doc, set_status, delete_doc). Same dance as `shares`: annotations
/// reference ops by id, ids are preserved, foreign keys are off for the swap
/// and checked afterwards; the two indexes are recreated.
fn widen_ops_op_type_check(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = (|| -> Result<()> {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE ops_new (
                 id            TEXT PRIMARY KEY,
                 doc_id        TEXT NOT NULL REFERENCES docs (id),
                 op_type       TEXT NOT NULL CHECK (op_type IN ('insert', 'replace', 'delete', 'move',
                                                                'rename_doc', 'move_doc', 'set_status', 'delete_doc')),
                 target_block  TEXT,
                 payload       TEXT NOT NULL,
                 principal     TEXT NOT NULL REFERENCES principals (id),
                 base_epoch    INTEGER NOT NULL,
                 epoch_applied INTEGER,
                 verdict       TEXT CHECK (verdict IN ('green', 'yellow', 'red')),
                 confidence    REAL,
                 prior         TEXT,
                 source_refs   TEXT NOT NULL DEFAULT '[]',
                 created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             );
             INSERT INTO ops_new (id, doc_id, op_type, target_block, payload, principal, base_epoch,
                                  epoch_applied, verdict, confidence, prior, source_refs, created_at)
                 SELECT id, doc_id, op_type, target_block, payload, principal, base_epoch,
                        epoch_applied, verdict, confidence, prior, source_refs, created_at FROM ops;
             DROP TABLE ops;
             ALTER TABLE ops_new RENAME TO ops;
             CREATE INDEX IF NOT EXISTS ops_by_doc_epoch ON ops (doc_id, epoch_applied);
             CREATE INDEX IF NOT EXISTS ops_by_principal ON ops (principal);
             COMMIT;",
        )?;
        Ok(())
    })();
    conn.pragma_update(None, "foreign_keys", true)?;
    result?;
    let violations: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_foreign_key_check('annotations')",
        [],
        |r| r.get(0),
    )?;
    if violations > 0 {
        return Err(StoreError::InvalidOp(format!(
            "ops rebuild left {violations} dangling annotations rows"
        )));
    }
    Ok(())
}

/// Populate FTS and edges for rows that predate their triggers/extraction.
/// Gated on user_version: count(*) on an external-content FTS table proxies
/// the content table, so emptiness is unobservable — version it instead.
const SCHEMA_VERSION: i64 = 8;

/// Every outstanding step and the version bump commit together: a crash
/// mid-backfill re-runs the whole thing next open instead of leaving a
/// half-filled index stamped as done.
fn backfill(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    if version < 3 {
        backfill_fts_edges_tags(&tx)?;
    }
    if version < 4 {
        backfill_block_types(&tx)?;
    }
    if version < 5 {
        backfill_doc_sort_keys(&tx)?;
    }
    if version < 6 {
        backfill_changes(&tx)?;
    }
    if version < 7 {
        retire_reviewer_gardeners(&tx)?;
    }
    if version < 8 {
        // ADR 0004: everything existing becomes the instance owner's (a
        // database with no users yet adopts when the owner is created)
        if let Some(owner) = tenancy::instance_owner_conn(&tx)? {
            tenancy::adopt_unowned_conn(&tx, owner)?;
        }
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// v7: the reviewer gardener kind is retired. Existing reviewer rows (and
/// their run log) are kept but disabled, so the daily cut never runs them;
/// the kind stays in the CHECK so those rows still read.
fn retire_reviewer_gardeners(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE gardeners SET enabled = 0 WHERE kind = 'reviewer'", [])?;
    Ok(())
}

/// v6: seed the change journal with one `doc` row per live doc, so a client
/// syncing from `since=0` sees everything that predates the triggers.
fn backfill_changes(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO changes (doc_id, kind, epoch)
         SELECT id, 'doc', current_epoch FROM docs WHERE deleted = 0 ORDER BY created_at, id",
        [],
    )?;
    Ok(())
}

/// v5: docs used to be created with a NULL sort_key (sorted last, by title).
/// Key every unkeyed doc after its parent's last keyed sibling, in title
/// order, so the tree order is explicit and stable from here on.
fn backfill_doc_sort_keys(conn: &Connection) -> Result<()> {
    let rows: Vec<(String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, parent_id FROM docs WHERE sort_key IS NULL
             ORDER BY parent_id, deleted, title, id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut last: std::collections::HashMap<Option<String>, Option<String>> = Default::default();
    for (id, parent_id) in rows {
        let prev = match last.get(&parent_id) {
            Some(k) => k.clone(),
            None => max_doc_sort_key(conn, parent_id.as_deref())?,
        };
        let key = crate::order_key::between(prev.as_deref(), None);
        conn.execute(
            "UPDATE docs SET sort_key = ?1 WHERE id = ?2",
            params![key, id],
        )?;
        last.insert(parent_id, Some(key));
    }
    Ok(())
}

/// The highest sort_key among ALL docs under `parent_id` (tombstoned ones
/// included), if any. Trashed siblings keep their key so a restore lands them
/// back where they were; a new doc keyed only past the live ones could
/// collide with a trashed key and then tie with it after the restore.
fn max_doc_sort_key(conn: &Connection, parent_id: Option<&str>) -> Result<Option<String>> {
    Ok(conn.query_row(
        "SELECT max(sort_key) FROM docs
         WHERE sort_key IS NOT NULL
           AND ((?1 IS NULL AND parent_id IS NULL) OR parent_id = ?1)",
        params![parent_id],
        |r| r.get(0),
    )?)
}

/// The sort_key for a doc appended under `parent`: just after the last keyed
/// live sibling, so a new doc never carries NULL and lands at the end.
fn next_doc_sort_key(conn: &Connection, parent: Option<Uuid>) -> Result<String> {
    let parent = parent.map(|p| p.to_string());
    let last = max_doc_sort_key(conn, parent.as_deref())?;
    Ok(crate::order_key::between(last.as_deref(), None))
}

/// v1→v3: FTS rows, wikilink edges and frontmatter tags for blocks that
/// predate their triggers/extraction.
fn backfill_fts_edges_tags(conn: &Connection) -> Result<()> {
    conn.execute("INSERT INTO blocks_fts (blocks_fts) VALUES ('rebuild')", [])?;
    let edges: i64 = conn.query_row("SELECT count(*) FROM edges", [], |r| r.get(0))?;
    if edges == 0 {
        let mut stmt = conn
            .prepare("SELECT id, content FROM blocks WHERE deleted = 0 AND content LIKE '%[[%'")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, content) in rows {
            for target in wikilinks(&content) {
                conn.execute(
                    "INSERT OR IGNORE INTO edges (from_block, to_target) VALUES (?1, ?2)",
                    params![id, target],
                )?;
            }
        }
    }
    // v3: tags for frontmatter blocks that predate extraction
    {
        let mut stmt = conn.prepare(
            "SELECT id, doc_id, content FROM blocks WHERE deleted = 0 AND content LIKE '---%'",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, doc_id, content) in rows {
            for tag in frontmatter_tags(&content) {
                conn.execute(
                    "INSERT OR IGNORE INTO doc_tags (doc_id, block_id, tag) VALUES (?1, ?2, ?3)",
                    params![doc_id, id, tag],
                )?;
            }
        }
    }
    Ok(())
}

/// v4: Replace used to leave `block_type` untouched, so a paragraph edited
/// into a heading (or a mermaid block edited into prose) kept its stale type.
/// Retype every live content block from its content, once. Comments and
/// canvases are not markdown and are left alone.
fn backfill_block_types(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, block_type, content FROM blocks
         WHERE deleted = 0 AND block_type NOT IN ('comment', 'canvas_scene')",
    )?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, stored, content) in rows {
        let want = crate::import::infer_block_type(&content).as_str();
        if want != stored {
            conn.execute(
                "UPDATE blocks SET block_type = ?1 WHERE id = ?2",
                params![want, id],
            )?;
        }
    }
    Ok(())
}

/// `[[target]]` / `[[target|alias]]` / `[[target#section]]`; `.md` optional.
fn wikilinks(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let target = after[..end].split(['|', '#']).next().unwrap_or("").trim();
        let target = target.strip_suffix(".md").unwrap_or(target);
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &after[end + 2..];
    }
    out
}

/// Tags from a frontmatter block: `tags:` followed by `- item` lines.
fn frontmatter_tags(content: &str) -> Vec<String> {
    if !content.starts_with("---") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut in_tags = false;
    for line in content.lines() {
        if in_tags {
            let t = line.trim_start();
            if let Some(item) = t.strip_prefix("- ") {
                out.push(item.trim().trim_matches(['"', '\'']).to_lowercase());
                continue;
            }
            in_tags = false;
        }
        if line.trim_end() == "tags:" {
            in_tags = true;
        }
    }
    out
}

fn set_tags(tx: &Transaction, doc_id: Uuid, block_id: Uuid, content: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM doc_tags WHERE block_id = ?1",
        params![block_id.to_string()],
    )?;
    for tag in frontmatter_tags(content) {
        tx.execute(
            "INSERT OR IGNORE INTO doc_tags (doc_id, block_id, tag) VALUES (?1, ?2, ?3)",
            params![doc_id.to_string(), block_id.to_string(), tag],
        )?;
    }
    Ok(())
}

fn set_edges(tx: &Transaction, block_id: Uuid, content: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM edges WHERE from_block = ?1",
        params![block_id.to_string()],
    )?;
    for target in wikilinks(content) {
        tx.execute(
            "INSERT OR IGNORE INTO edges (from_block, to_target) VALUES (?1, ?2)",
            params![block_id.to_string(), target],
        )?;
    }
    Ok(())
}

type RawDoc = (
    String,
    Option<String>,
    String,
    Option<String>,
    i64,
    String,
    Option<String>,
    Option<String>,
);

fn row_to_doc(row: &rusqlite::Row) -> rusqlite::Result<RawDoc> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
    ))
}

fn build_doc(raw: RawDoc) -> Result<Doc> {
    let (id, parent, title, policy, epoch, created_by, status, sort_key) = raw;
    Ok(Doc {
        id: uuid_col(id, "docs.id")?,
        parent_id: parent.map(|p| uuid_col(p, "docs.parent_id")).transpose()?,
        title,
        review_policy: policy
            .map(|p| {
                ReviewPolicy::parse(&p)
                    .ok_or_else(|| StoreError::InvalidOp(format!("bad review_policy: {p}")))
            })
            .transpose()?,
        current_epoch: epoch,
        created_by: uuid_col(created_by, "docs.created_by")?,
        status: status
            .map(|st| {
                DocStatus::parse(&st)
                    .ok_or_else(|| StoreError::InvalidOp(format!("bad status: {st}")))
            })
            .transpose()?,
        sort_key,
    })
}

type RawBlock = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    i64,
    bool,
    Option<String>,
);

fn row_to_block(row: &rusqlite::Row) -> rusqlite::Result<RawBlock> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn build_block(raw: RawBlock) -> Result<Block> {
    let (
        id,
        doc_id,
        parent_id,
        order_key,
        block_type,
        content,
        created_by,
        epoch,
        deleted,
        refers_to,
    ) = raw;
    Ok(Block {
        id: uuid_col(id, "blocks.id")?,
        doc_id: uuid_col(doc_id, "blocks.doc_id")?,
        parent_id: parent_id
            .map(|p| uuid_col(p, "blocks.parent_id"))
            .transpose()?,
        order_key,
        block_type: BlockType::parse(&block_type)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad block_type: {block_type}")))?,
        content,
        created_by: uuid_col(created_by, "blocks.created_by")?,
        epoch,
        deleted,
        refers_to: refers_to
            .map(|r| uuid_col(r, "blocks.refers_to"))
            .transpose()?,
    })
}

const BLOCK_COLS: &str =
    "id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, deleted, refers_to";

/// A new docs row, keyed after its last live sibling (never NULL).
fn insert_doc_row(
    conn: &Connection,
    id: Uuid,
    title: &str,
    parent: Option<Uuid>,
    created_by: Uuid,
    owner: Option<Uuid>,
) -> Result<()> {
    let sort_key = next_doc_sort_key(conn, parent)?;
    conn.execute(
        "INSERT INTO docs (id, parent_id, title, created_by, sort_key, owner_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id.to_string(),
            parent.map(|p| p.to_string()),
            title,
            created_by.to_string(),
            sort_key,
            owner.map(|o| o.to_string()),
        ],
    )?;
    Ok(())
}

/// The body of `apply` against an open transaction: epoch check, project +
/// ledger for every op, epoch bump. The caller commits.
fn apply_in_tx(
    tx: &Transaction,
    doc_id: Uuid,
    base_epoch: i64,
    principal: Uuid,
    ops: Vec<OpInput>,
) -> Result<ApplyReceipt> {
    let current = doc_epoch(tx, doc_id)?;
    if base_epoch != current {
        return Err(StoreError::StaleBase {
            base: base_epoch,
            current,
        });
    }

    // one committed transaction = one epoch (PROJECT.md §3.1)
    let epoch = current + 1;
    let mut op_ids = Vec::with_capacity(ops.len());
    for op in &ops {
        let op_id = Uuid::now_v7();
        let mut op = op.clone();
        resolve_order_keys(tx, doc_id, &mut op.kind)?;
        let op = &op;
        let prior = match op.kind.target_block() {
            Some(t) => block_by_id(tx, doc_id, t)?,
            None => None,
        };
        project(tx, doc_id, epoch, principal, &op.kind)?;
        insert_op_row(
            tx,
            op_id,
            doc_id,
            op,
            principal,
            base_epoch,
            Some(epoch),
            Verdict::Green,
            1.0,
            &prior,
        )?;
        op_ids.push(op_id);
    }
    tx.execute(
        "UPDATE docs SET current_epoch = ?1 WHERE id = ?2",
        params![epoch, doc_id.to_string()],
    )?;
    Ok(ApplyReceipt {
        doc_id,
        epoch,
        op_ids,
    })
}

/// Fetch a block by id within a doc, tombstoned ones included.
fn block_by_id(tx: &Transaction, doc_id: Uuid, id: Uuid) -> Result<Option<Block>> {
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1 AND doc_id = ?2"
    ))?;
    stmt.query_row(params![id.to_string(), doc_id.to_string()], row_to_block)
        .optional()?
        .map(build_block)
        .transpose()
}

fn doc_epoch(tx: &Connection, doc_id: Uuid) -> Result<i64> {
    tx.query_row(
        "SELECT current_epoch FROM docs WHERE id = ?1",
        params![doc_id.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("doc {doc_id}")))
}

#[expect(clippy::too_many_arguments)]
fn insert_op_row(
    tx: &Transaction,
    op_id: Uuid,
    doc_id: Uuid,
    op: &OpInput,
    principal: Uuid,
    base_epoch: i64,
    epoch_applied: Option<i64>,
    verdict: Verdict,
    confidence: f64,
    prior: &Option<Block>,
) -> Result<()> {
    tx.execute(
        "INSERT INTO ops (id, doc_id, op_type, target_block, payload, principal,
                          base_epoch, epoch_applied, verdict, confidence, prior, source_refs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            op_id.to_string(),
            doc_id.to_string(),
            op.kind.op_type(),
            op.kind.target_block().map(|t| t.to_string()),
            serde_json::to_string(&op.kind)?,
            principal.to_string(),
            base_epoch,
            epoch_applied,
            verdict.as_str(),
            confidence,
            prior.as_ref().map(serde_json::to_string).transpose()?,
            serde_json::to_string(&op.source_refs)?,
        ],
    )?;
    Ok(())
}

fn insert_annotation(
    tx: &Transaction,
    doc_id: Uuid,
    op_id: Uuid,
    kind: AnnotationKind,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    tx.execute(
        "INSERT INTO annotations (id, doc_id, op_id, kind) VALUES (?1, ?2, ?3, ?4)",
        params![
            id.to_string(),
            doc_id.to_string(),
            op_id.to_string(),
            kind.as_str()
        ],
    )?;
    Ok(id)
}

/// The inverse of an applied yellow, built from its pre-image (decline path).
fn inverse_of(kind: &OpKind, prior: Option<&Block>) -> Result<OpKind> {
    let need_prior = || {
        prior.ok_or_else(|| StoreError::InvalidOp("cannot invert: no pre-image recorded".into()))
    };
    match kind {
        OpKind::Insert { block_id, .. } => Ok(OpKind::Delete { target: *block_id }),
        OpKind::Replace { target, .. } => Ok(OpKind::Replace {
            target: *target,
            content: need_prior()?.content.clone(),
        }),
        OpKind::Move { target, .. } => {
            let p = need_prior()?;
            Ok(OpKind::Move {
                target: *target,
                new_parent: p.parent_id,
                new_order_key: p.order_key.clone(),
            })
        }
        // the gate never yellows a delete, but `propose_reviewed` caps a green
        // delete to yellow — declining it must resurrect the block in place
        OpKind::Delete { target } => {
            let p = need_prior()?;
            Ok(OpKind::Insert {
                block_id: *target,
                parent_id: p.parent_id,
                order_key: p.order_key.clone(),
                block_type: p.block_type,
                content: p.content.clone(),
                refers_to: p.refers_to,
            })
        }
        // doc ops carry their own pre-image in the payload
        OpKind::RenameDoc { title, from_title } => Ok(OpKind::RenameDoc {
            title: from_title.clone(),
            from_title: title.clone(),
        }),
        OpKind::MoveDoc {
            new_parent,
            sort_key,
            new_parent_title,
            from_parent,
            from_sort_key,
            from_parent_title,
        } => Ok(OpKind::MoveDoc {
            new_parent: *from_parent,
            sort_key: from_sort_key.clone(),
            new_parent_title: from_parent_title.clone(),
            from_parent: *new_parent,
            from_sort_key: sort_key.clone(),
            from_parent_title: new_parent_title.clone(),
        }),
        OpKind::SetStatus { status, from_status } => Ok(OpKind::SetStatus {
            status: *from_status,
            from_status: *status,
        }),
        // a delete is never yellow: it parks red, and a declined red is
        // simply never applied — there is nothing to invert
        OpKind::DeleteDoc { .. } => Err(StoreError::InvalidOp(
            "cannot invert delete_doc: it is never applied as a yellow".into(),
        )),
    }
}

/// Project a doc op (AX slice B) onto the `docs` table. Shared by the human
/// paths (`rename_doc` / `move_doc` / `delete_doc` in the trait impl) and the
/// gate (`propose_doc_op`, `resolve`), so the rules cannot drift.
fn project_doc_op(conn: &Connection, doc_id: Uuid, op: &OpKind) -> Result<()> {
    match op {
        OpKind::RenameDoc { title, .. } => rename_doc_conn(conn, doc_id, title),
        OpKind::MoveDoc {
            new_parent,
            sort_key,
            ..
        } => move_doc_conn(conn, doc_id, *new_parent, sort_key.as_deref()),
        OpKind::SetStatus { status, .. } => set_doc_status_conn(conn, doc_id, *status),
        OpKind::DeleteDoc { .. } => delete_subtree(conn, doc_id).map(|_| ()),
        _ => Err(StoreError::InvalidOp(format!(
            "{} is a block op, not a doc op",
            op.op_type()
        ))),
    }
}

fn rename_doc_conn(conn: &Connection, doc_id: Uuid, title: &str) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        return Err(StoreError::InvalidOp("rename: empty title".into()));
    }
    let n = conn.execute(
        "UPDATE docs SET title = ?1 WHERE id = ?2 AND deleted = 0",
        params![title, doc_id.to_string()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("doc {doc_id}")));
    }
    Ok(())
}

fn set_doc_status_conn(conn: &Connection, doc_id: Uuid, status: Option<DocStatus>) -> Result<()> {
    let n = conn.execute(
        "UPDATE docs SET status = ?1 WHERE id = ?2",
        params![status.map(|st| st.as_str()), doc_id.to_string()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("doc {doc_id}")));
    }
    Ok(())
}

/// The doc's parent chain, nearest first (live or not — a cycle check must
/// see every row).
fn doc_parent_of(conn: &Connection, id: Uuid) -> Result<Option<Uuid>> {
    let p: Option<Option<String>> = conn
        .query_row(
            "SELECT parent_id FROM docs WHERE id = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    match p {
        None => Err(StoreError::NotFound(format!("doc {id}"))),
        Some(p) => p.map(|p| uuid_col(p, "docs.parent_id")).transpose(),
    }
}

fn move_doc_conn(
    conn: &Connection,
    doc_id: Uuid,
    new_parent: Option<Uuid>,
    sort_key: Option<&str>,
) -> Result<()> {
    let mut cursor = new_parent;
    while let Some(p) = cursor {
        if p == doc_id {
            return Err(StoreError::InvalidOp(
                "move: doc cannot nest under itself".into(),
            ));
        }
        cursor = doc_parent_of(conn, p)?;
    }
    // no key from the client = append under the new parent; a move must
    // never re-NULL a key the v5 backfill assigned
    let sort_key = match sort_key {
        Some(k) if crate::order_key::is_valid(k) => k.to_string(),
        Some(k) => {
            return Err(StoreError::InvalidOp(format!(
                "move: invalid sort_key {k:?}"
            )))
        }
        None => next_doc_sort_key(conn, new_parent)?,
    };
    let ws_before = crate::workspaces::resolve_conn(conn, doc_id)?;
    let n = conn.execute(
        "UPDATE docs SET parent_id = ?1, sort_key = ?2 WHERE id = ?3 AND deleted = 0",
        params![
            new_parent.map(|p| p.to_string()),
            sort_key,
            doc_id.to_string()
        ],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(format!("doc {doc_id}")));
    }
    // the trigger journals the moved doc; its subtree re-resolves too
    if crate::workspaces::resolve_conn(conn, doc_id)? != ws_before {
        crate::workspaces::emit_tree_conn(conn, &crate::workspaces::descendants_conn(conn, doc_id)?)?;
    }
    Ok(())
}

/// Soft-delete `doc_id` and every live descendant under one `deleted_at`
/// stamp (so `restore_doc` revives exactly this subtree). Returns the count.
/// Runs on whatever connection/transaction the caller holds.
fn delete_subtree(conn: &Connection, doc_id: Uuid) -> Result<usize> {
    let mut to_delete = vec![doc_id];
    let mut i = 0;
    while i < to_delete.len() {
        let kids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM docs WHERE parent_id = ?1 AND deleted = 0")?;
            let rows = stmt.query_map(params![to_delete[i].to_string()], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for k in kids {
            to_delete.push(uuid_col(k, "docs.id")?);
        }
        i += 1;
    }
    // one stamp for the whole subtree: restore_doc revives exactly the
    // docs that fell together, not a child tombstoned earlier on its own
    let stamp: String = conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        [],
        |r| r.get(0),
    )?;
    let mut n = 0;
    for d in &to_delete {
        n += conn.execute(
            "UPDATE docs SET deleted = 1, deleted_at = ?2 WHERE id = ?1 AND deleted = 0",
            params![d.to_string(), stamp],
        )?;
    }
    if n == 0 {
        return Err(StoreError::NotFound(format!("doc {doc_id}")));
    }
    Ok(n)
}

/// Live docs in the subtree rooted at `doc_id` (root included).
fn subtree_ids_conn(conn: &Connection, doc_id: Uuid) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE sub(id) AS (
             SELECT id FROM docs WHERE id = ?1
             UNION ALL
             SELECT docs.id FROM docs JOIN sub ON docs.parent_id = sub.id
             WHERE docs.deleted = 0)
         SELECT id FROM sub",
    )?;
    let rows = stmt.query_map(params![doc_id.to_string()], |r| r.get::<_, String>(0))?;
    rows.map(|r| uuid_col(r?, "docs.id")).collect()
}

/// Resolve agent-friendly order_key specs before anything is persisted:
/// "" = append after the last sibling; "after:<uuid>" = between that block
/// and its next sibling. Real keys pass through untouched, and the ledger
/// stores the resolved op.
fn resolve_order_keys(tx: &Transaction, doc_id: Uuid, kind: &mut OpKind) -> Result<()> {
    let (parent_id, order_key) = match kind {
        OpKind::Insert {
            parent_id,
            order_key,
            ..
        } => (parent_id, order_key),
        // a move carries a real key: validate, never resolve
        OpKind::Move { new_order_key, .. } => {
            check_order_key(new_order_key)?;
            return Ok(());
        }
        _ => return Ok(()),
    };
    let spec = order_key.clone();
    if !spec.is_empty() && !spec.starts_with("after:") {
        return check_order_key(&spec);
    }
    let siblings = live_sibling_keys(tx, doc_id, *parent_id)?;
    let new_key = if let Some(after_id) = spec.strip_prefix("after:") {
        let after_id = after_id.trim();
        let idx = siblings
            .iter()
            .position(|(id, _)| id == after_id)
            .ok_or_else(|| {
                StoreError::InvalidOp(format!("after:{after_id} is not a sibling in this doc"))
            })?;
        let next = siblings.get(idx + 1).map(|(_, k)| k.as_str());
        crate::order_key::between(Some(&siblings[idx].1), next)
    } else {
        crate::order_key::between(siblings.last().map(|(_, k)| k.as_str()), None)
    };
    *order_key = new_key;
    Ok(())
}

/// Client-supplied keys must be real fractional keys (base36, non-empty, no
/// trailing 0): anything else is rejected as a normal error, never a panic.
fn check_order_key(key: &str) -> Result<()> {
    if crate::order_key::is_valid(key) {
        Ok(())
    } else {
        Err(StoreError::InvalidOp(format!(
            "order_key {key:?}: must be non-empty base36 digits (or \"\" / \"after:<id>\" on insert)"
        )))
    }
}

/// (id, order_key) of the live blocks under `parent_id`, in key order.
fn live_sibling_keys(
    tx: &Transaction,
    doc_id: Uuid,
    parent_id: Option<Uuid>,
) -> Result<Vec<(String, String)>> {
    let mut stmt = tx.prepare_cached(
        "SELECT id, order_key FROM blocks
         WHERE doc_id = ?1 AND deleted = 0
           AND ((?2 IS NULL AND parent_id IS NULL) OR parent_id = ?2)
         ORDER BY order_key",
    )?;
    let rows = stmt.query_map(
        params![doc_id.to_string(), parent_id.map(|p| p.to_string())],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A parked insert resolved its key when it was parked; by the time it is
/// accepted a sibling may have taken that key. Re-resolve to a fresh key just
/// after the collision so live siblings never share a key. Returns whether
/// the key changed.
fn dedupe_insert_key(tx: &Transaction, doc_id: Uuid, kind: &mut OpKind) -> Result<bool> {
    let OpKind::Insert {
        block_id,
        parent_id,
        order_key,
        ..
    } = kind
    else {
        return Ok(false);
    };
    let siblings = live_sibling_keys(tx, doc_id, *parent_id)?;
    let me = block_id.to_string();
    let taken = siblings
        .iter()
        .any(|(id, k)| *id != me && k == order_key);
    if !taken {
        return Ok(false);
    }
    let next = siblings
        .iter()
        .map(|(_, k)| k.as_str())
        .find(|k| *k > order_key.as_str());
    *order_key = crate::order_key::between(Some(order_key), next);
    Ok(true)
}

/// Fetch a live (non-deleted) block within a doc, for projection checks.
fn live_block(tx: &Transaction, doc_id: Uuid, id: Uuid, role: &str) -> Result<Block> {
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1 AND doc_id = ?2 AND deleted = 0"
    ))?;
    let raw = stmt
        .query_row(params![id.to_string(), doc_id.to_string()], row_to_block)
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("{role} block {id} in doc {doc_id}")))?;
    build_block(raw)
}

/// Apply one op to the projection. Ledger insert happens beside this in `apply`.
fn project(tx: &Transaction, doc_id: Uuid, epoch: i64, principal: Uuid, op: &OpKind) -> Result<()> {
    match op {
        OpKind::Insert {
            block_id,
            parent_id,
            order_key,
            block_type,
            content,
            refers_to,
        } => {
            if let Some(p) = parent_id {
                live_block(tx, doc_id, *p, "parent")?;
            }
            // An insert under an id that already exists is a conflict — unless
            // the row is a tombstone, in which case this is a resurrection
            // (declining a reviewed delete) and the block comes back in place
            // under its original id, so deep links and comment anchors hold.
            let existing: Option<(String, bool)> = tx
                .query_row(
                    "SELECT doc_id, deleted FROM blocks WHERE id = ?1",
                    params![block_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match existing {
                Some((_, false)) => {
                    return Err(StoreError::InvalidOp(format!(
                        "insert: block {block_id} already exists"
                    )));
                }
                Some((other_doc, true)) if other_doc != doc_id.to_string() => {
                    return Err(StoreError::InvalidOp(format!(
                        "insert: block {block_id} is a tombstone in another doc"
                    )));
                }
                Some((_, true)) => {
                    tx.execute(
                        "UPDATE blocks SET deleted = 0, parent_id = ?1, order_key = ?2,
                                block_type = ?3, content = ?4, epoch = ?5, refers_to = ?6
                         WHERE id = ?7",
                        params![
                            parent_id.map(|p| p.to_string()),
                            order_key,
                            block_type.as_str(),
                            content,
                            epoch,
                            refers_to.map(|r| r.to_string()),
                            block_id.to_string(),
                        ],
                    )?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO blocks (id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, refers_to)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            block_id.to_string(),
                            doc_id.to_string(),
                            parent_id.map(|p| p.to_string()),
                            order_key,
                            block_type.as_str(),
                            content,
                            principal.to_string(),
                            epoch,
                            refers_to.map(|r| r.to_string()),
                        ],
                    )?;
                }
            }
            set_edges(tx, *block_id, content)?;
            set_tags(tx, doc_id, *block_id, content)?;
        }
        OpKind::Replace { target, content } => {
            let existing = live_block(tx, doc_id, *target, "replace target")?;
            // the type follows the content: a paragraph edited into `## x`
            // becomes a heading. Comments and canvases keep their type — they
            // are not markdown and the editor never retypes them.
            let block_type = if matches!(
                existing.block_type,
                BlockType::Comment | BlockType::CanvasScene
            ) {
                existing.block_type
            } else {
                crate::import::infer_block_type(content)
            };
            tx.execute(
                "UPDATE blocks SET content = ?1, epoch = ?2, block_type = ?3 WHERE id = ?4",
                params![content, epoch, block_type.as_str(), target.to_string()],
            )?;
            set_edges(tx, *target, content)?;
            set_tags(tx, doc_id, *target, content)?;
        }
        OpKind::Delete { target } => {
            live_block(tx, doc_id, *target, "delete target")?;
            tx.execute(
                "UPDATE blocks SET deleted = 1, epoch = ?1 WHERE id = ?2",
                params![epoch, target.to_string()],
            )?;
            tx.execute(
                "DELETE FROM edges WHERE from_block = ?1",
                params![target.to_string()],
            )?;
            tx.execute(
                "DELETE FROM doc_tags WHERE block_id = ?1",
                params![target.to_string()],
            )?;
        }
        OpKind::Move {
            target,
            new_parent,
            new_order_key,
        } => {
            live_block(tx, doc_id, *target, "move target")?;
            if let Some(p) = new_parent {
                if p == target {
                    return Err(StoreError::InvalidOp(
                        "move: block cannot parent itself".into(),
                    ));
                }
                // walk up from the new parent: moving under one's own descendant is a cycle
                let mut cursor = live_block(tx, doc_id, *p, "new parent")?;
                while let Some(anc) = cursor.parent_id {
                    if anc == *target {
                        return Err(StoreError::InvalidOp(format!(
                            "move: {p} is a descendant of {target}"
                        )));
                    }
                    cursor = live_block(tx, doc_id, anc, "ancestor")?;
                }
            }
            tx.execute(
                "UPDATE blocks SET parent_id = ?1, order_key = ?2, epoch = ?3 WHERE id = ?4",
                params![
                    new_parent.map(|p| p.to_string()),
                    new_order_key,
                    epoch,
                    target.to_string()
                ],
            )?;
        }
        OpKind::RenameDoc { .. }
        | OpKind::MoveDoc { .. }
        | OpKind::SetStatus { .. }
        | OpKind::DeleteDoc { .. } => project_doc_op(tx, doc_id, op)?,
    }
    Ok(())
}

impl BlockStore for SqliteStore {
    fn create_principal(
        &mut self,
        kind: PrincipalKind,
        display_name: &str,
        pubkey: Option<&str>,
    ) -> Result<Principal> {
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO principals (id, kind, display_name, pubkey) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), kind.as_str(), display_name, pubkey],
        )?;
        Ok(Principal {
            id,
            kind,
            display_name: display_name.into(),
            pubkey: pubkey.map(Into::into),
        })
    }

    fn get_principal(&self, id: Uuid) -> Result<Principal> {
        let raw: Option<(String, String, String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, kind, display_name, pubkey FROM principals WHERE id = ?1",
                params![id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (id, kind, display_name, pubkey) =
            raw.ok_or_else(|| StoreError::NotFound(format!("principal {id}")))?;
        Ok(Principal {
            id: uuid_col(id, "principals.id")?,
            kind: PrincipalKind::parse(&kind)
                .ok_or_else(|| StoreError::InvalidOp(format!("bad principal kind: {kind}")))?,
            display_name,
            pubkey,
        })
    }

    fn rename_principal(&mut self, id: Uuid, display_name: &str) -> Result<()> {
        let name = display_name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(StoreError::InvalidOp("display name must be 1..64 characters".into()));
        }
        if let Some(u) = self.scope.user() {
            let mine: bool = self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM auth_users WHERE id = ?1 AND principal_id = ?2)",
                params![u.to_string(), id.to_string()],
                |r| r.get(0),
            )?;
            if !mine {
                return Err(StoreError::NotFound(format!("principal {id}")));
            }
            self.conn.execute(
                "UPDATE auth_users SET name = ?1 WHERE id = ?2",
                params![name, u.to_string()],
            )?;
        }
        let n = self.conn.execute(
            "UPDATE principals SET display_name = ?1 WHERE id = ?2",
            params![name, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("principal {id}")));
        }
        Ok(())
    }

    fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let get = |k: &str| -> Result<Option<String>> {
            Ok(self
                .conn
                .query_row("SELECT value FROM settings WHERE key = ?1", params![k], |r| r.get(0))
                .optional()?)
        };
        match self.scope.user() {
            None => get(key),
            // a user's settings are theirs (ADR 0004); the instance owner
            // still reads what the single-user install recorded
            Some(u) => match get(&user_setting_key(u, key))? {
                Some(v) => Ok(Some(v)),
                None if self.instance_owner()? == Some(u) => get(key),
                None => Ok(None),
            },
        }
    }

    fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        let key = &match self.scope.user() {
            Some(u) => user_setting_key(u, key),
            None => key.to_string(),
        };
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn list_principals(&self) -> Result<Vec<Principal>> {
        // a user sees agents, their own human principal, and the people they
        // share a workspace with — not everyone on the server
        let filter = match self.scope.user() {
            None => String::new(),
            Some(u) => format!(
                "WHERE kind != 'human'
                    OR id NOT IN (SELECT principal_id FROM auth_users)
                    OR id IN (SELECT a.principal_id FROM auth_users a
                              WHERE a.id = '{u}'
                                 OR a.id IN (SELECT m.user_id FROM workspace_members m
                                             WHERE m.workspace_id IN (SELECT workspace_id FROM workspace_members WHERE user_id = '{u}')))"
            ),
        };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, kind, display_name, pubkey FROM principals {filter} ORDER BY display_name",
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (id, kind, display_name, pubkey) = r?;
            Ok(Principal {
                id: uuid_col(id, "principals.id")?,
                kind: PrincipalKind::parse(&kind)
                    .ok_or_else(|| StoreError::InvalidOp(format!("bad principal kind: {kind}")))?,
                display_name,
                pubkey,
            })
        })
        .collect()
    }

    fn create_doc(&mut self, title: &str, parent: Option<Uuid>, created_by: Uuid) -> Result<Doc> {
        let id = Uuid::now_v7();
        let owner = self.new_doc_owner(parent)?;
        insert_doc_row(&self.conn, id, title, parent, created_by, owner)?;
        self.get_doc(id)
    }

    fn create_doc_with_ops(
        &mut self,
        title: &str,
        parent: Option<Uuid>,
        created_by: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<(Doc, usize)> {
        let id = Uuid::now_v7();
        let n = ops.len();
        let owner = self.new_doc_owner(parent)?;
        // the share gate: an agent's first content in a shared workspace
        // lands as flagged yellows, not greens
        let gated = n > 0
            && match parent {
                Some(p) => {
                    tenancy::is_agent_conn(&self.conn, created_by)?
                        && tenancy::space_is_shared(&self.conn, tenancy::space_conn(&self.conn, p)?)?
                }
                None => false,
            };
        let tx = self.conn.transaction()?;
        insert_doc_row(&tx, id, title, parent, created_by, owner)?;
        let ops = if n > 0 && !gated {
            apply_in_tx(&tx, id, 0, created_by, ops)?;
            Vec::new()
        } else {
            ops
        };
        tx.commit()?;
        if gated {
            self.propose_impl(id, 0, created_by, ops, true)?;
        }
        Ok((self.get_doc(id)?, n))
    }

    fn list_docs(&self) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT id, parent_id, title, review_policy, current_epoch, created_by, status, sort_key
             FROM docs WHERE deleted = 0 AND {} ORDER BY sort_key IS NULL, sort_key, title",
            self.vis("id")
        ))?;
        let rows = stmt.query_map([], row_to_doc)?;
        let mut docs: Vec<Doc> = rows.map(|r| build_doc(r?)).collect::<Result<_>>()?;
        if !self.scope.sees_all() {
            // a visible doc under a parent the viewer cannot see is a root
            // for them: the parent's id is not theirs to know
            let ids: std::collections::HashSet<Uuid> = docs.iter().map(|d| d.id).collect();
            for d in docs.iter_mut() {
                if d.parent_id.is_some_and(|p| !ids.contains(&p)) {
                    d.parent_id = None;
                }
            }
        }
        Ok(docs)
    }

    fn get_doc(&self, id: Uuid) -> Result<Doc> {
        self.see(id)?;
        let mut doc = get_doc_conn(&self.conn, id)?;
        self.mask_parent(&mut doc);
        Ok(doc)
    }

    fn read_doc(&self, id: Uuid) -> Result<DocTree> {
        let doc = self.get_doc(id)?;
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {BLOCK_COLS} FROM blocks
             WHERE doc_id = ?1 AND deleted = 0 ORDER BY order_key"
        ))?;
        let rows = stmt.query_map(params![id.to_string()], row_to_block)?;
        let blocks: Vec<Block> = rows.map(|r| build_block(r?)).collect::<Result<_>>()?;

        // assemble tree: children were already fetched in order_key order
        fn attach(parent: Option<Uuid>, pool: &mut Vec<Block>) -> Vec<BlockNode> {
            let (mine, rest): (Vec<Block>, Vec<Block>) = std::mem::take(pool)
                .into_iter()
                .partition(|b| b.parent_id == parent);
            *pool = rest;
            mine.into_iter()
                .map(|block| {
                    let children = attach(Some(block.id), pool);
                    BlockNode { block, children }
                })
                .collect()
        }
        let mut pool = blocks;
        let roots = attach(None, &mut pool);
        Ok(DocTree { doc, roots })
    }

    fn read_block(&self, id: Uuid) -> Result<Block> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1"))?;
        let raw = stmt
            .query_row(params![id.to_string()], row_to_block)
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("block {id}")))?;
        let block = build_block(raw)?;
        if tenancy::ensure_visible_conn(&self.conn, self.scope, block.doc_id).is_err() {
            return Err(StoreError::NotFound(format!("block {id}")));
        }
        Ok(block)
    }

    fn apply(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ApplyReceipt> {
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("apply: empty op list".into()));
        }
        self.may_write(doc_id)?;
        // the share gate (ADR 0004): an agent's direct write into a shared
        // workspace still applies, but flagged for review — never green
        if tenancy::agent_into_shared(&self.conn, doc_id, principal)? {
            let current = doc_epoch(&self.conn, doc_id)?;
            if base_epoch != current {
                return Err(StoreError::StaleBase { base: base_epoch, current });
            }
            let out = self.propose_impl(doc_id, base_epoch, principal, ops, true)?;
            if let Some(v) = out.verdicts.iter().find(|v| !v.applied) {
                return Err(StoreError::InvalidOp(format!("apply: {}", v.note)));
            }
            return Ok(ApplyReceipt {
                doc_id,
                epoch: out.epoch,
                op_ids: out.verdicts.iter().map(|v| v.op_id).collect(),
            });
        }
        let tx = self.conn.transaction()?;
        let receipt = apply_in_tx(&tx, doc_id, base_epoch, principal, ops)?;
        tx.commit()?;
        Ok(receipt)
    }

    fn ops_since(&self, doc_id: Uuid, since_epoch: i64) -> Result<Vec<LedgerOp>> {
        self.see(doc_id)?;
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {OP_COLS} FROM ops
             WHERE doc_id = ?1 AND epoch_applied IS NOT NULL AND epoch_applied > ?2
             ORDER BY epoch_applied, id"
        ))?;
        let rows = stmt.query_map(params![doc_id.to_string(), since_epoch], row_to_op)?;
        rows.map(|r| build_op(r?)).collect()
    }

    fn effective_policy(&self, doc_id: Uuid) -> Result<ReviewPolicy> {
        self.see(doc_id)?;
        let mut cursor = doc_id;
        loop {
            // the walk crosses ancestors the viewer may not see: unscoped
            let doc = get_doc_conn(&self.conn, cursor)?;
            if let Some(p) = doc.review_policy {
                return Ok(p);
            }
            match doc.parent_id {
                Some(parent) => cursor = parent,
                None => return Ok(crate::DEFAULT_REVIEW_POLICY),
            }
        }
    }

    fn move_doc(
        &mut self,
        doc_id: Uuid,
        new_parent: Option<Uuid>,
        sort_key: Option<&str>,
    ) -> Result<()> {
        let scope = self.scope;
        check_move(&self.conn, scope, doc_id, new_parent)?;
        let tx = self.conn.unchecked_transaction()?;
        let before = tenancy::access_snapshot(&tx)?;
        move_doc_conn(&tx, doc_id, new_parent, sort_key)?;
        reroot_owner(&tx, scope, doc_id, new_parent)?;
        tenancy::journal_access_diff(&tx, before)?;
        tx.commit()?;
        Ok(())
    }

    fn delete_doc(&mut self, doc_id: Uuid) -> Result<usize> {
        self.may_write(doc_id)?;
        check_subtree_writable(&self.conn, self.scope, doc_id)?;
        let tx = self.conn.transaction()?;
        let n = delete_subtree(&tx, doc_id)?;
        tx.commit()?;
        Ok(n)
    }

    fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        // roots of tombstoned subtrees: deleted, and the parent is live or
        // absent. Docs a remote principal created are tombstones federation
        // left behind (dropped mirrors of a revoked share), not the user's own
        // deletions, and stay out of the Trash.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by,
                    d.status, d.sort_key, d.deleted_at,
                    (SELECT count(*) FROM docs c
                      WHERE c.deleted = 1 AND c.deleted_at = d.deleted_at AND c.id != d.id AND {vis_c}
                        AND c.id IN (WITH RECURSIVE sub(id) AS (
                              SELECT id FROM docs WHERE parent_id = d.id
                              UNION ALL
                              SELECT docs.id FROM docs JOIN sub ON docs.parent_id = sub.id)
                            SELECT id FROM sub)) AS descendants
             FROM docs d
             JOIN principals p ON p.id = d.created_by
             WHERE d.deleted = 1
               AND p.kind != 'remote'
               AND {}
               AND (d.parent_id IS NULL
                    OR NOT EXISTS (SELECT 1 FROM docs pd WHERE pd.id = d.parent_id AND pd.deleted = 1))
             ORDER BY d.deleted_at DESC, d.title",
            self.vis("d.id"),
            vis_c = self.vis("c.id")
        ))?;
        let rows = stmt.query_map([], |r| {
            let raw: RawDoc = (
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            );
            let deleted_at: Option<String> = r.get(8)?;
            let descendants: i64 = r.get(9)?;
            Ok((raw, deleted_at, descendants))
        })?;
        rows.map(|r| {
            let (raw, deleted_at, descendants) = r?;
            let mut doc = build_doc(raw)?;
            self.mask_parent(&mut doc);
            Ok(TrashEntry {
                doc,
                deleted_at: deleted_at.unwrap_or_default(),
                descendants: descendants as usize,
            })
        })
        .collect()
    }

    fn restore_doc(&mut self, doc_id: Uuid) -> Result<usize> {
        self.may_write(doc_id)?;
        let (stamp, parent): (Option<String>, Option<String>) = self
            .conn
            .query_row(
                "SELECT deleted_at, parent_id FROM docs WHERE id = ?1 AND deleted = 1",
                params![doc_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("doc {doc_id} is not in the trash")))?;
        let tx = self.conn.transaction()?;
        let before = tenancy::access_snapshot(&tx)?;
        // the subtree that fell with it: descendants sharing the stamp
        let mut to_restore = vec![doc_id];
        let mut i = 0;
        while i < to_restore.len() {
            let kids: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM docs WHERE parent_id = ?1 AND deleted = 1
                       AND ((?2 IS NULL AND deleted_at IS NULL) OR deleted_at = ?2)",
                )?;
                let rows = stmt.query_map(params![to_restore[i].to_string(), stamp], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for k in kids {
                to_restore.push(uuid_col(k, "docs.id")?);
            }
            i += 1;
        }
        let mut n = 0;
        for d in &to_restore {
            n += tx.execute(
                "UPDATE docs SET deleted = 0, deleted_at = NULL WHERE id = ?1",
                params![d.to_string()],
            )?;
        }
        // a parent that is itself still in the trash would hide the restored
        // doc again: surface it at the root instead
        if let Some(p) = parent {
            let parent_deleted: i64 = tx.query_row(
                "SELECT coalesce((SELECT deleted FROM docs WHERE id = ?1), 1)",
                params![p],
                |r| r.get(0),
            )?;
            if parent_deleted == 1 {
                tx.execute(
                    "UPDATE docs SET parent_id = NULL WHERE id = ?1",
                    params![doc_id.to_string()],
                )?;
            }
        }
        tenancy::journal_access_diff(&tx, before)?;
        tx.commit()?;
        Ok(n)
    }

    fn doc_subtree_ids(&self, doc_id: Uuid) -> Result<Vec<Uuid>> {
        self.see(doc_id)?;
        let mut stmt = self.conn.prepare(&format!(
            "WITH RECURSIVE sub(id) AS (
                 SELECT id FROM docs WHERE id = ?1
                 UNION ALL
                 SELECT docs.id FROM docs JOIN sub ON docs.parent_id = sub.id
                 WHERE docs.deleted = 0)
             SELECT id FROM sub WHERE {}",
            self.vis("id")
        ))?;
        let rows = stmt.query_map(params![doc_id.to_string()], |r| r.get::<_, String>(0))?;
        rows.map(|r| uuid_col(r?, "docs.id")).collect()
    }

    fn rename_doc(&mut self, doc_id: Uuid, title: &str) -> Result<()> {
        self.may_write(doc_id)?;
        rename_doc_conn(&self.conn, doc_id, title)
    }

    fn set_doc_status(&mut self, doc_id: Uuid, status: Option<DocStatus>) -> Result<()> {
        self.may_write(doc_id)?;
        set_doc_status_conn(&self.conn, doc_id, status)
    }

    fn set_review_policy(&mut self, doc_id: Uuid, policy: Option<ReviewPolicy>) -> Result<()> {
        // weakening the gate is the space owner's call, not an editor's
        if !self.scope.sees_all() && self.doc_role(doc_id)? != crate::Role::Owner {
            return Err(StoreError::Forbidden("only the workspace owner sets review policy".into()));
        }
        let n = self.conn.execute(
            "UPDATE docs SET review_policy = ?1 WHERE id = ?2",
            params![policy.map(|p| p.as_str()), doc_id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        Ok(())
    }

    fn backlinks(&self, doc_id: Uuid) -> Result<Vec<SearchHit>> {
        let doc = self.get_doc(doc_id)?;
        let sql = format!(
            "SELECT DISTINCT {}, d.title FROM edges e
             JOIN blocks b ON b.id = e.from_block
             JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0 AND {}
               AND (e.to_target = ?1 OR e.to_target LIKE '%/' || ?1)
             ORDER BY d.title, b.order_key",
            b_cols(),
            self.vis("d.id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![doc.title], |r| {
            let raw = row_to_block(r)?;
            let title: String = r.get(10)?;
            Ok((raw, title))
        })?;
        rows.map(|r| {
            let (raw, doc_title) = r?;
            Ok(SearchHit {
                block: build_block(raw)?,
                doc_title,
            })
        })
        .collect()
    }

    fn add_comment(
        &mut self,
        target_block: Uuid,
        principal: Uuid,
        text: &str,
        reply_to: Option<Uuid>,
    ) -> Result<Block> {
        let target = self.read_block(target_block)?;
        // comments are writes: a viewer cannot leave one
        self.may_write(target.doc_id)?;
        if let Some(r) = reply_to {
            let parent = self.read_block(r)?;
            if parent.block_type != BlockType::Comment || parent.refers_to != Some(target_block) {
                return Err(StoreError::InvalidOp(
                    "reply_to must be a comment on the same block".into(),
                ));
            }
        }
        let epoch = self.get_doc(target.doc_id)?.current_epoch;
        let comment_id = Uuid::now_v7();
        self.apply(
            target.doc_id,
            epoch,
            principal,
            vec![OpInput {
                kind: OpKind::Insert {
                    block_id: comment_id,
                    parent_id: reply_to,
                    order_key: crate::order_key::between(None, None),
                    block_type: BlockType::Comment,
                    content: text.into(),
                    refers_to: Some(target_block),
                },
                source_refs: vec![],
            }],
        )?;
        self.read_block(comment_id)
    }

    fn list_comments(&self, target_block: Uuid) -> Result<Vec<Block>> {
        self.read_block(target_block)?;
        let sql = format!(
            "SELECT {BLOCK_COLS} FROM blocks
             WHERE refers_to = ?1 AND deleted = 0 ORDER BY id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![target_block.to_string()], row_to_block)?;
        rows.map(|r| build_block(r?)).collect()
    }

    fn search_blocks(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // FTS5 trigram with OR-of-trigrams: typo-tolerant ("gardnr" shares
        // trigrams with "gardener"), bm25-ranked. Sub-trigram queries fall
        // back to LIKE.
        if let Some(match_q) = fts_query(query) {
            let sql = format!(
                "SELECT {}, d.title FROM blocks_fts f
                 JOIN blocks b ON b.rowid = f.rowid
                 JOIN docs d ON d.id = b.doc_id
                 WHERE blocks_fts MATCH ?1 AND b.deleted = 0 AND d.deleted = 0 AND {}
                 ORDER BY bm25(blocks_fts) LIMIT ?2",
                b_cols(),
                self.vis("d.id")
            );
            let mut stmt = self.conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(params![match_q, limit as i64], |r| {
                let raw = row_to_block(r)?;
                let title: String = r.get(10)?;
                Ok((raw, title))
            })?;
            return rows
                .map(|r| {
                    let (raw, doc_title) = r?;
                    Ok(SearchHit {
                        block: build_block(raw)?,
                        doc_title,
                    })
                })
                .collect();
        }
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let sql = format!(
            "SELECT {}, d.title FROM blocks b JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0 AND {} AND b.content LIKE ?1 ESCAPE '\\'
             ORDER BY d.title, b.order_key LIMIT ?2",
            b_cols(),
            self.vis("d.id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![pattern, limit as i64], |r| {
            let raw = row_to_block(r)?;
            let title: String = r.get(10)?;
            Ok((raw, title))
        })?;
        rows.map(|r| {
            let (raw, doc_title) = r?;
            Ok(SearchHit {
                block: build_block(raw)?,
                doc_title,
            })
        })
        .collect()
    }

    fn propose(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ProposeOutcome> {
        self.may_write(doc_id)?;
        self.propose_impl(doc_id, base_epoch, principal, ops, false)
    }

    fn propose_reviewed(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ProposeOutcome> {
        self.may_write(doc_id)?;
        self.propose_impl(doc_id, base_epoch, principal, ops, true)
    }

    fn propose_doc_op(
        &mut self,
        doc_id: Uuid,
        principal: Uuid,
        mut kind: OpKind,
        source_refs: Vec<String>,
    ) -> Result<ProposeOutcome> {
        if !kind.is_doc_op() {
            return Err(StoreError::InvalidOp(format!(
                "propose_doc_op: {} is a block op — use propose",
                kind.op_type()
            )));
        }
        let scope = self.scope;
        self.may_write(doc_id)?;
        // a move that changes the doc's space is a share: authorized like a
        // human move, and an agent taking a doc into or out of a SHARED
        // workspace parks red for a human (ADR 0004 §5)
        let mut share_red = false;
        if let OpKind::MoveDoc { new_parent, .. } = &kind {
            let (from, to) = check_move(&self.conn, scope, doc_id, *new_parent)?;
            if from != to
                && tenancy::is_agent_conn(&self.conn, principal)?
                && (tenancy::space_is_shared(&self.conn, from)? || tenancy::space_is_shared(&self.conn, to)?)
            {
                share_red = true;
            }
        }
        if matches!(kind, OpKind::DeleteDoc { .. }) {
            check_subtree_writable(&self.conn, scope, doc_id)?;
        }
        let tx = self.conn.transaction()?;
        if !doc_is_live(&tx, doc_id)? {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        let doc = get_doc_conn(&tx, doc_id)?;
        let before = tenancy::access_snapshot(&tx)?;
        let current = doc.current_epoch;
        let parent_title = |p: Option<Uuid>| -> Result<Option<String>> {
            p.map(|p| get_doc_conn(&tx, p).map(|d| d.title)).transpose()
        };
        // the server owns the pre-image: whatever the caller put in from_* is replaced
        let note;
        match &mut kind {
            OpKind::RenameDoc { title, from_title } => {
                let new = title.trim().to_string();
                if new.is_empty() {
                    return Err(StoreError::InvalidOp("rename: empty title".into()));
                }
                if new == doc.title {
                    return Err(StoreError::InvalidOp(format!(
                        "rename: doc is already titled {new:?}"
                    )));
                }
                *title = new;
                *from_title = doc.title.clone();
            }
            OpKind::MoveDoc {
                new_parent,
                sort_key,
                new_parent_title,
                from_parent,
                from_sort_key,
                from_parent_title,
            } => {
                if let Some(p) = new_parent
                    && !doc_is_live(&tx, *p)?
                {
                    return Err(StoreError::NotFound(format!("new_parent doc {p}")));
                }
                *new_parent_title = parent_title(*new_parent)?;
                *from_parent = doc.parent_id;
                *from_sort_key = doc.sort_key.clone();
                *from_parent_title = parent_title(doc.parent_id)?;
                if sort_key.is_none() {
                    // resolve "append" now so the ledger holds the real key
                    *sort_key = Some(next_doc_sort_key(&tx, *new_parent)?);
                }
            }
            OpKind::SetStatus { from_status, .. } => {
                *from_status = doc.status;
            }
            OpKind::DeleteDoc { title, doc_count } => {
                *title = doc.title.clone();
                *doc_count = subtree_ids_conn(&tx, doc_id)?.len();
            }
            _ => unreachable!("is_doc_op checked above"),
        }
        let op_id = Uuid::now_v7();
        let (verdict, confidence, applied) = if let OpKind::DeleteDoc { title, doc_count } = &kind {
            note = format!(
                "parked: trashing “{title}” ({doc_count} doc{}) needs a human accept in the review queue",
                if *doc_count == 1 { "" } else { "s" }
            );
            (Verdict::Red, 0.5, false)
        } else if share_red {
            note = "parked: moving a doc into or out of a shared workspace needs a human accept".into();
            (Verdict::Red, 0.5, false)
        } else {
            // a cycle / bad key / missing parent is the caller's mistake: an
            // error, not a parked red that could never apply
            project_doc_op(&tx, doc_id, &kind)?;
            if let OpKind::MoveDoc { new_parent, .. } = &kind {
                reroot_owner(&tx, scope, doc_id, *new_parent)?;
            }
            let mut n = String::from("applied; flagged for review (declining reverts it)");
            if let OpKind::RenameDoc { title, from_title } = &kind {
                let rewritten = rewrite_inbound_links_tx(
                    &tx,
                    scope,
                    from_title,
                    title,
                    principal,
                    &format!("rename:{from_title} → {title}"),
                )?;
                n = format!("{n}; {rewritten} inbound link block(s) rewritten");
            }
            note = n;
            (Verdict::Yellow, 1.0, true)
        };
        let input = OpInput { kind, source_refs };
        insert_op_row(
            &tx,
            op_id,
            doc_id,
            &input,
            principal,
            current,
            applied.then_some(current),
            verdict,
            confidence,
            &None,
        )?;
        insert_annotation(
            &tx,
            doc_id,
            op_id,
            if applied {
                AnnotationKind::Review
            } else {
                AnnotationKind::Parked
            },
        )?;
        tenancy::journal_access_diff(&tx, before)?;
        tx.commit()?;
        Ok(ProposeOutcome {
            doc_id,
            epoch: current,
            verdicts: vec![ProposeVerdict {
                op_id,
                block_id: None,
                verdict,
                confidence,
                applied,
                note,
            }],
        })
    }

    fn create_gardener(
        &mut self,
        name: &str,
        kind: GardenerKind,
        task_prompt: &str,
        scope_doc: Option<Uuid>,
        confidence_policy: ConfidencePolicy,
    ) -> Result<Gardener> {
        if kind == GardenerKind::Reviewer {
            return Err(StoreError::InvalidOp("the reviewer gardener kind was retired".into()));
        }
        // each gardener is its own principal: provenance is per-gardener
        let principal = self.create_principal(PrincipalKind::Agent, name, None)?;
        if let Some(d) = scope_doc {
            self.see(d)?;
        }
        let id = Uuid::now_v7();
        // a gardener works for someone: the creating user, else (the admin
        // CLI) the instance owner
        let owner = match self.scope.user() {
            Some(u) => Some(u),
            None => self.instance_owner()?,
        };
        self.conn.execute(
            "INSERT INTO gardeners (id, name, kind, principal, scope_doc, task_prompt, confidence_policy, owner_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id.to_string(),
                name,
                kind.as_str(),
                principal.id.to_string(),
                scope_doc.map(|d| d.to_string()),
                task_prompt,
                confidence_policy.as_str(),
                owner.map(|o| o.to_string()),
            ],
        )?;
        Ok(Gardener {
            id,
            name: name.into(),
            kind,
            principal: principal.id,
            scope_doc,
            task_prompt: task_prompt.into(),
            bindings: serde_json::json!([]),
            creds_ref: None,
            schedule: "daily".into(),
            confidence_policy,
            enabled: true,
        })
    }

    fn list_gardeners(&self) -> Result<Vec<Gardener>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, name, kind, principal, scope_doc, task_prompt, bindings, creds_ref,
                    schedule, confidence_policy, enabled
             FROM gardeners WHERE {} ORDER BY name",
            self.gardener_pred("gardeners.owner_id")
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, bool>(10)?,
            ))
        })?;
        rows.map(|r| {
            let (
                id,
                name,
                kind,
                principal,
                scope,
                task_prompt,
                bindings,
                creds_ref,
                schedule,
                cp,
                enabled,
            ) = r?;
            Ok(Gardener {
                id: uuid_col(id, "gardeners.id")?,
                name,
                kind: GardenerKind::parse(&kind)
                    .ok_or_else(|| StoreError::InvalidOp(format!("bad gardener kind: {kind}")))?,
                principal: uuid_col(principal, "gardeners.principal")?,
                scope_doc: scope
                    .map(|d| uuid_col(d, "gardeners.scope_doc"))
                    .transpose()?,
                task_prompt,
                bindings: serde_json::from_str(&bindings)?,
                creds_ref,
                schedule,
                confidence_policy: ConfidencePolicy::parse(&cp)
                    .ok_or_else(|| StoreError::InvalidOp(format!("bad confidence_policy: {cp}")))?,
                enabled,
            })
        })
        .collect()
    }

    fn set_gardener_enabled(&mut self, id: Uuid, enabled: bool) -> Result<()> {
        let n = self.conn.execute(
            &format!("UPDATE gardeners SET enabled = ?1 WHERE id = ?2 AND {}", self.gardener_pred("owner_id")),
            params![enabled, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("gardener {id}")));
        }
        Ok(())
    }

    fn update_gardener(
        &mut self,
        id: Uuid,
        task_prompt: &str,
        schedule: &str,
        confidence_policy: ConfidencePolicy,
        scope_doc: Option<Uuid>,
        enabled: bool,
        bindings: serde_json::Value,
    ) -> Result<()> {
        if let Some(d) = scope_doc {
            self.see(d)?;
        }
        let n = self.conn.execute(
            &format!(
                "UPDATE gardeners SET task_prompt = ?1, schedule = ?2, confidence_policy = ?3,
                        scope_doc = ?4, enabled = ?5, bindings = ?6
                 WHERE id = ?7 AND {}",
                self.gardener_pred("owner_id")
            ),
            params![
                task_prompt,
                schedule,
                confidence_policy.as_str(),
                scope_doc.map(|d| d.to_string()),
                enabled,
                serde_json::to_string(&bindings)?,
                id.to_string(),
            ],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("gardener {id}")));
        }
        Ok(())
    }

    fn start_run(&mut self, gardener: Uuid) -> Result<Uuid> {
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO gardener_runs (id, gardener) VALUES (?1, ?2)",
            params![id.to_string(), gardener.to_string()],
        )?;
        Ok(id)
    }

    fn finish_run(
        &mut self,
        run: Uuid,
        status: &str,
        summary: &str,
        tokens_used: Option<i64>,
        tool_calls: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE gardener_runs
             SET status = ?1, summary = ?2, tokens_used = ?3, tool_calls = ?4,
                 finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?5",
            params![status, summary, tokens_used, tool_calls, run.to_string()],
        )?;
        Ok(())
    }

    fn list_runs(&self, limit: usize) -> Result<Vec<GardenerRun>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT r.id, r.gardener, g.name, r.started_at, r.status, r.summary, r.tokens_used, r.tool_calls
             FROM gardener_runs r JOIN gardeners g ON g.id = r.gardener
             WHERE {}
             ORDER BY r.started_at DESC LIMIT ?1",
            self.gardener_pred("g.owner_id")
        ))?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<i64>>(7)?,
            ))
        })?;
        rows.map(|r| {
            let (id, gardener, gardener_name, started_at, status, summary, tokens_used, tool_calls) = r?;
            Ok(GardenerRun {
                id: uuid_col(id, "runs.id")?,
                gardener: uuid_col(gardener, "runs.gardener")?,
                gardener_name,
                started_at,
                status,
                summary,
                tokens_used,
                tool_calls,
            })
        })
        .collect()
    }

    fn list_tags(&self) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT t.tag, count(DISTINCT t.doc_id) FROM doc_tags t
             JOIN docs d ON d.id = t.doc_id AND d.deleted = 0
             WHERE {}
             GROUP BY t.tag ORDER BY 2 DESC, t.tag",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    fn docs_by_tag(&self, tag: &str) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d JOIN doc_tags t ON t.doc_id = d.id
             WHERE t.tag = ?1 AND d.deleted = 0 AND {} GROUP BY d.id ORDER BY d.title",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map(params![tag.to_lowercase()], row_to_doc)?;
        self.masked(rows.map(|r| build_doc(r?)).collect())
    }

    fn untagged_docs(&self, limit: usize) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d
             WHERE d.deleted = 0 AND {}
               AND EXISTS (SELECT 1 FROM blocks b WHERE b.doc_id = d.id AND b.deleted = 0)
               AND NOT EXISTS (SELECT 1 FROM doc_tags t WHERE t.doc_id = d.id)
             ORDER BY d.title LIMIT ?1",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_doc)?;
        self.masked(rows.map(|r| build_doc(r?)).collect())
    }

    fn park(
        &mut self,
        doc_id: Uuid,
        principal: Uuid,
        ops: Vec<OpInput>,
        note: &str,
    ) -> Result<Vec<Uuid>> {
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("park: empty op list".into()));
        }
        self.may_write(doc_id)?;
        let tx = self.conn.transaction()?;
        let base = doc_epoch(&tx, doc_id)?;
        let mut op_ids = Vec::with_capacity(ops.len());
        for op in &ops {
            let op_id = Uuid::now_v7();
            let mut op = op.clone();
            resolve_order_keys(&tx, doc_id, &mut op.kind)?;
            if !note.is_empty() {
                op.source_refs.push(format!("note:{note}"));
            }
            let prior = match op.kind.target_block() {
                Some(t) => block_by_id(&tx, doc_id, t)?,
                None => None,
            };
            insert_op_row(
                &tx,
                op_id,
                doc_id,
                &op,
                principal,
                base,
                None,
                Verdict::Red,
                0.5,
                &prior,
            )?;
            insert_annotation(&tx, doc_id, op_id, AnnotationKind::Parked)?;
            op_ids.push(op_id);
        }
        tx.commit()?;
        Ok(op_ids)
    }

    fn review_queue(&self, doc_id: Option<Uuid>) -> Result<Vec<ReviewItem>> {
        if let Some(d) = doc_id {
            self.see(d)?;
        }
        let sql = format!(
            "SELECT a.id, a.doc_id, a.op_id, a.kind, a.status, a.resolved_by,
                    {}
             FROM annotations a JOIN ops o ON o.id = a.op_id
             WHERE a.status = 'open' AND (?1 IS NULL OR a.doc_id = ?1) AND {}
             ORDER BY a.created_at, a.id",
            OP_COLS
                .split(", ")
                .map(|c| format!("o.{c}"))
                .collect::<Vec<_>>()
                .join(", "),
            self.vis("a.doc_id")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![doc_id.map(|d| d.to_string())], |r| {
            let ann: (String, String, String, String, String, Option<String>) = (
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            );
            let op = row_to_op_offset(r, 6)?;
            Ok((ann, op))
        })?;
        rows.map(|r| {
            let ((id, a_doc, op_id, kind, status, resolved_by), raw_op) = r?;
            Ok(ReviewItem {
                annotation: Annotation {
                    id: uuid_col(id, "annotations.id")?,
                    doc_id: uuid_col(a_doc, "annotations.doc_id")?,
                    op_id: uuid_col(op_id, "annotations.op_id")?,
                    kind: parse_annotation_kind(&kind)?,
                    status: parse_annotation_status(&status)?,
                    resolved_by: resolved_by
                        .map(|p| uuid_col(p, "annotations.resolved_by"))
                        .transpose()?,
                },
                op: build_op(raw_op)?,
            })
        })
        .collect()
    }

    fn stale_block_vectors(&self, limit: usize) -> Result<Vec<(Uuid, i64, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT b.id, b.epoch, b.content FROM blocks b
             JOIN docs d ON d.id = b.doc_id
             LEFT JOIN block_vec v ON v.block_id = b.id
             WHERE b.deleted = 0 AND b.block_type != 'comment'
               AND d.deleted = 0 AND {}
               AND (v.block_id IS NULL OR v.epoch < b.epoch)
             ORDER BY b.epoch DESC LIMIT ?1",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })?;
        rows.map(|r| {
            let (id, epoch, content) = r?;
            Ok((uuid_col(id, "blocks.id")?, epoch, content))
        })
        .collect()
    }

    fn set_block_vec(&mut self, block_id: Uuid, epoch: i64, vec: &[f32]) -> Result<()> {
        self.read_block(block_id)?;
        let mut blob = Vec::with_capacity(vec.len() * 4);
        for f in vec {
            blob.extend_from_slice(&f.to_le_bytes());
        }
        self.conn.execute(
            "INSERT INTO block_vec (block_id, epoch, dim, vec) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(block_id) DO UPDATE SET epoch = excluded.epoch, dim = excluded.dim, vec = excluded.vec",
            params![block_id.to_string(), epoch, vec.len() as i64, blob],
        )?;
        Ok(())
    }

    fn block_vecs(&self) -> Result<Vec<(Uuid, Vec<f32>)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT v.block_id, v.vec FROM block_vec v
             JOIN blocks b ON b.id = v.block_id
             JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0 AND v.dim > 0 AND {}",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
        rows.map(|r| {
            let (id, blob) = r?;
            let vec = blob
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            Ok((uuid_col(id, "block_vec.block_id")?, vec))
        })
        .collect()
    }

    fn purge_block_vecs(&mut self) -> Result<usize> {
        if self.scope != Scope::System && self.scope != Scope::Local {
            return Err(StoreError::Forbidden("purge_block_vecs is System work".into()));
        }
        let n = self.conn.execute(
            "DELETE FROM block_vec WHERE block_id IN (
                 SELECT v.block_id FROM block_vec v LEFT JOIN blocks b ON b.id = v.block_id
                 WHERE b.id IS NULL OR b.deleted = 1)",
            [],
        )?;
        Ok(n)
    }

    fn blocks_as_hits(&self, ids: &[Uuid]) -> Result<Vec<SearchHit>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let Ok(block) = self.read_block(*id) else { continue };
            if block.deleted {
                continue;
            }
            // a trashed doc's blocks are not hits (the vector index lags the
            // tombstone by up to one embed pass)
            let doc: Option<(String, i64)> = self
                .conn
                .query_row(
                    "SELECT title, deleted FROM docs WHERE id = ?1",
                    params![block.doc_id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((doc_title, 0)) = doc else { continue };
            out.push(SearchHit { block, doc_title });
        }
        Ok(out)
    }

    fn doc_is_tombstoned(&self, id: Uuid) -> Result<bool> {
        self.see(id)?;
        self.conn
            .query_row(
                "SELECT deleted FROM docs WHERE id = ?1",
                params![id.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("doc {id}")))
    }

    fn resolve(
        &mut self,
        annotation_id: Uuid,
        reviewer: Uuid,
        decision: ReviewDecision,
    ) -> Result<Option<ApplyReceipt>> {
        let tx = self.conn.transaction()?;
        let raw: Option<(String, String, String, String)> = tx
            .query_row(
                "SELECT a.doc_id, a.kind, a.status, a.op_id
                 FROM annotations a WHERE a.id = ?1",
                params![annotation_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (doc_id_s, kind_s, status_s, op_id_s) =
            raw.ok_or_else(|| StoreError::NotFound(format!("annotation {annotation_id}")))?;
        let doc_id = uuid_col(doc_id_s, "annotations.doc_id")?;
        let scope = self.scope;
        if tenancy::ensure_visible_conn(&tx, scope, doc_id).is_err() {
            return Err(StoreError::NotFound(format!("annotation {annotation_id}")));
        }
        tenancy::ensure_write_conn(&tx, scope, doc_id)?;
        let before = tenancy::access_snapshot(&tx)?;
        let kind = parse_annotation_kind(&kind_s)?;
        if parse_annotation_status(&status_s)? != AnnotationStatus::Open {
            return Err(StoreError::InvalidOp(format!(
                "annotation {annotation_id} already {status_s}"
            )));
        }

        let raw_op = tx.query_row(
            &format!("SELECT {OP_COLS} FROM ops WHERE id = ?1"),
            params![op_id_s],
            row_to_op,
        )?;
        let op = build_op(raw_op)?;

        // the trust invariant (§3.4): proposer ≠ approver, enforced at the
        // gate — for AGENT principals, whose autonomy it bounds. The
        // instance's human owner is exempt: their own stale edit that the
        // gate parked red (an autosave that raced another window or device)
        // is theirs to accept or drop, and there is no one else to
        // do it — without this exemption those items are stuck forever.
        if op.principal == reviewer {
            let kind: String = tx.query_row(
                "SELECT kind FROM principals WHERE id = ?1",
                params![op.principal.to_string()],
                |r| r.get(0),
            )?;
            if kind != "human" {
                return Err(StoreError::InvalidOp(
                    "proposer cannot resolve their own proposal".into(),
                ));
            }
        }

        let mut receipt = None;
        // doc ops (AX slice B) never bump the epoch and carry their own
        // pre-image; they resolve on their own path
        let doc_op = op.kind.is_doc_op();
        if doc_op {
            if decision == ReviewDecision::Accept
                && let OpKind::MoveDoc { new_parent, .. } = &op.kind
            {
                // a parked (share) move applies now: authorize it as the reviewer's own
                check_move(&tx, scope, doc_id, *new_parent)?;
            }
            if decision == ReviewDecision::Accept && matches!(op.kind, OpKind::DeleteDoc { .. }) {
                check_subtree_writable(&tx, scope, doc_id)?;
            }
            receipt = resolve_doc_op(
                &tx,
                scope,
                doc_id,
                annotation_id,
                &op,
                reviewer,
                kind,
                decision,
            )?;
        }
        match (kind, decision) {
            _ if doc_op => {}
            // yellow accepted: the edit is already live — just clear the flag
            (AnnotationKind::Review, ReviewDecision::Accept) => {}
            // yellow declined: revert via pre-image, as a green op by the reviewer
            (AnnotationKind::Review, ReviewDecision::Decline) => {
                let inverse = inverse_of(&op.kind, op.prior.as_ref())?;
                let current = doc_epoch(&tx, doc_id)?;
                let epoch = current + 1;
                let inv_id = Uuid::now_v7();
                let inv_input = OpInput {
                    kind: inverse,
                    source_refs: vec![format!("review:decline:{annotation_id}")],
                };
                let prior = match inv_input.kind.target_block() {
                    Some(t) => block_by_id(&tx, doc_id, t)?,
                    None => None,
                };
                project(&tx, doc_id, epoch, reviewer, &inv_input.kind)?;
                insert_op_row(
                    &tx,
                    inv_id,
                    doc_id,
                    &inv_input,
                    reviewer,
                    current,
                    Some(epoch),
                    Verdict::Green,
                    1.0,
                    &prior,
                )?;
                tx.execute(
                    "UPDATE docs SET current_epoch = ?1 WHERE id = ?2",
                    params![epoch, doc_id.to_string()],
                )?;
                receipt = Some(ApplyReceipt {
                    doc_id,
                    epoch,
                    op_ids: vec![inv_id],
                });
            }
            // red accepted: apply the parked op now, at the current epoch;
            // verdict stays red — distinct provenance for resolved reds (§3.4)
            (AnnotationKind::Parked, ReviewDecision::Accept) => {
                let current = doc_epoch(&tx, doc_id)?;
                let epoch = current + 1;
                // the key was resolved at park time; a sibling may hold it now
                let mut kind = op.kind.clone();
                if dedupe_insert_key(&tx, doc_id, &mut kind)? {
                    tx.execute(
                        "UPDATE ops SET payload = ?1 WHERE id = ?2",
                        params![serde_json::to_string(&kind)?, op.id.to_string()],
                    )?;
                }
                project(&tx, doc_id, epoch, op.principal, &kind)?;
                tx.execute(
                    "UPDATE ops SET epoch_applied = ?1 WHERE id = ?2",
                    params![epoch, op.id.to_string()],
                )?;
                tx.execute(
                    "UPDATE docs SET current_epoch = ?1 WHERE id = ?2",
                    params![epoch, doc_id.to_string()],
                )?;
                receipt = Some(ApplyReceipt {
                    doc_id,
                    epoch,
                    op_ids: vec![op.id],
                });
            }
            // red declined: parked closed, never applied
            (AnnotationKind::Parked, ReviewDecision::Decline) => {}
        }

        let status = match decision {
            ReviewDecision::Accept => AnnotationStatus::Accepted,
            ReviewDecision::Decline => AnnotationStatus::Declined,
        };
        tx.execute(
            "UPDATE annotations SET status = ?1, resolved_by = ?2,
                    resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?3",
            params![
                status.as_str(),
                reviewer.to_string(),
                annotation_id.to_string()
            ],
        )?;
        // doc freshness: a HUMAN accepting an auditor's/keeper's fix means
        // the doc was just checked — stamp it. Nothing else here verifies.
        if decision == ReviewDecision::Accept && !doc_op {
            let reviewer_is_human: bool = tx.query_row(
                "SELECT kind = 'human' FROM principals WHERE id = ?1",
                params![reviewer.to_string()],
                |r| r.get(0),
            )?;
            let by_auditor: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM gardeners g
                                WHERE g.principal = ?1 AND g.kind IN ('auditor', 'keeper'))",
                params![op.principal.to_string()],
                |r| r.get(0),
            )?;
            if reviewer_is_human && by_auditor {
                tx.execute(
                    "UPDATE docs SET verified_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                    params![doc_id.to_string()],
                )?;
            }
        }
        tenancy::journal_access_diff(&tx, before)?;
        tx.commit()?;
        Ok(receipt)
    }
}

/// Resolve an annotation whose op is a doc op. Yellow accept: nothing to do.
/// Yellow decline: apply the inverse (rename back + links back, move back,
/// status back) as a green op by the reviewer. Red accept (delete_doc): trash
/// the subtree now. Red decline: closed, never applied. No epoch bump in
/// any case.
#[expect(clippy::too_many_arguments)]
fn resolve_doc_op(
    tx: &Transaction,
    scope: Scope,
    doc_id: Uuid,
    annotation_id: Uuid,
    op: &LedgerOp,
    reviewer: Uuid,
    kind: AnnotationKind,
    decision: ReviewDecision,
) -> Result<Option<ApplyReceipt>> {
    let current = doc_epoch(tx, doc_id)?;
    match (kind, decision) {
        (AnnotationKind::Review, ReviewDecision::Accept)
        | (AnnotationKind::Parked, ReviewDecision::Decline) => Ok(None),
        (AnnotationKind::Review, ReviewDecision::Decline) => {
            let inverse = inverse_of(&op.kind, None)?;
            project_doc_op(tx, doc_id, &inverse)?;
            let mut note = String::new();
            if let OpKind::RenameDoc { title, from_title } = &inverse {
                // links were rewritten from_title → title on propose; put them back
                let n = rewrite_inbound_links_tx(
                    tx,
                    scope,
                    from_title,
                    title,
                    reviewer,
                    &format!("rename:{from_title} → {title}"),
                )?;
                note = format!("links_rewritten:{n}");
            }
            let inv_id = Uuid::now_v7();
            let mut source_refs = vec![format!("review:decline:{annotation_id}")];
            if !note.is_empty() {
                source_refs.push(note);
            }
            let inv_input = OpInput {
                kind: inverse,
                source_refs,
            };
            insert_op_row(
                tx,
                inv_id,
                doc_id,
                &inv_input,
                reviewer,
                current,
                Some(current),
                Verdict::Green,
                1.0,
                &None,
            )?;
            Ok(Some(ApplyReceipt {
                doc_id,
                epoch: current,
                op_ids: vec![inv_id],
            }))
        }
        (AnnotationKind::Parked, ReviewDecision::Accept) => {
            project_doc_op(tx, doc_id, &op.kind)?;
            if let OpKind::MoveDoc { new_parent, .. } = &op.kind {
                reroot_owner(tx, scope, doc_id, *new_parent)?;
            }
            tx.execute(
                "UPDATE ops SET epoch_applied = ?1 WHERE id = ?2",
                params![current, op.id.to_string()],
            )?;
            Ok(Some(ApplyReceipt {
                doc_id,
                epoch: current,
                op_ids: vec![op.id],
            }))
        }
    }
}

fn get_doc_conn(conn: &Connection, id: Uuid) -> Result<Doc> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, parent_id, title, review_policy, current_epoch, created_by, status, sort_key
         FROM docs WHERE id = ?1",
    )?;
    let raw = stmt
        .query_row(params![id.to_string()], row_to_doc)
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("doc {id}")))?;
    build_doc(raw)
}

fn doc_is_live(conn: &Connection, id: Uuid) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT deleted = 0 FROM docs WHERE id = ?1",
            params![id.to_string()],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// Blocks whose [[wikilinks]] point at `title` (exact or path form):
/// (block_id, doc_id, content).
fn linking_blocks_conn(conn: &Connection, scope: Scope, title: &str) -> Result<Vec<(Uuid, Uuid, String)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT b.id, b.doc_id, b.content
         FROM edges e JOIN blocks b ON b.id = e.from_block
         JOIN docs d ON d.id = b.doc_id
         WHERE b.deleted = 0 AND d.deleted = 0 AND {}
           AND (e.to_target = ?1 OR e.to_target LIKE '%/' || ?1)",
        tenancy::vis_pred(scope, "d.id")
    ))?;
    let rows = stmt.query_map(params![title], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    rows.map(|r| {
        let (b, d, c) = r?;
        Ok((
            uuid_col(b, "edges.from_block")?,
            uuid_col(d, "blocks.doc_id")?,
            c,
        ))
    })
    .collect()
}

/// Rewrite every inbound [[wikilink]] from `old` to `new` as green replaces
/// by `principal`, one epoch per linking doc (the same rule as the human
/// rename in the API). Returns how many blocks were rewritten.
fn rewrite_inbound_links_tx(
    tx: &Transaction,
    scope: Scope,
    old: &str,
    new: &str,
    principal: Uuid,
    source_ref: &str,
) -> Result<usize> {
    if old == new {
        return Ok(0);
    }
    let mut by_doc: std::collections::HashMap<Uuid, Vec<(Uuid, String)>> = Default::default();
    for (block, doc, content) in linking_blocks_conn(tx, scope, old)? {
        // a rename never writes into a doc the renamer cannot write
        if tenancy::ensure_write_conn(tx, scope, doc).is_err() {
            continue;
        }
        by_doc.entry(doc).or_default().push((block, content));
    }
    let mut rewritten = 0usize;
    for (doc, blocks) in by_doc {
        let ops: Vec<OpInput> = blocks
            .into_iter()
            .filter_map(|(block, content)| {
                let new_content = crate::rewrite_links(&content, old, new);
                (new_content != content).then(|| OpInput {
                    kind: OpKind::Replace {
                        target: block,
                        content: new_content,
                    },
                    source_refs: vec![source_ref.to_string()],
                })
            })
            .collect();
        if ops.is_empty() {
            continue;
        }
        rewritten += ops.len();
        let current = doc_epoch(tx, doc)?;
        apply_in_tx(tx, doc, current, principal, ops)?;
    }
    Ok(rewritten)
}

const OP_COLS: &str = "id, doc_id, payload, principal, base_epoch, epoch_applied, verdict, confidence, prior, source_refs";

type RawOp = (
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<String>,
    Option<f64>,
    Option<String>,
    String,
);

fn row_to_op(row: &rusqlite::Row) -> rusqlite::Result<RawOp> {
    row_to_op_offset(row, 0)
}

fn row_to_op_offset(row: &rusqlite::Row, o: usize) -> rusqlite::Result<RawOp> {
    Ok((
        row.get(o)?,
        row.get(o + 1)?,
        row.get(o + 2)?,
        row.get(o + 3)?,
        row.get(o + 4)?,
        row.get(o + 5)?,
        row.get(o + 6)?,
        row.get(o + 7)?,
        row.get(o + 8)?,
        row.get(o + 9)?,
    ))
}

fn build_op(raw: RawOp) -> Result<LedgerOp> {
    let (
        id,
        doc_id,
        payload,
        principal,
        base_epoch,
        epoch_applied,
        verdict,
        confidence,
        prior,
        source_refs,
    ) = raw;
    Ok(LedgerOp {
        id: uuid_col(id, "ops.id")?,
        doc_id: uuid_col(doc_id, "ops.doc_id")?,
        kind: serde_json::from_str(&payload)?,
        principal: uuid_col(principal, "ops.principal")?,
        base_epoch,
        epoch_applied,
        verdict: match verdict.as_deref() {
            None => None,
            Some("green") => Some(Verdict::Green),
            Some("yellow") => Some(Verdict::Yellow),
            Some("red") => Some(Verdict::Red),
            Some(v) => return Err(StoreError::InvalidOp(format!("bad verdict: {v}"))),
        },
        confidence,
        prior: prior.map(|p| serde_json::from_str(&p)).transpose()?,
        source_refs: serde_json::from_str(&source_refs)?,
    })
}

fn parse_annotation_kind(s: &str) -> Result<AnnotationKind> {
    match s {
        "review" => Ok(AnnotationKind::Review),
        "parked" => Ok(AnnotationKind::Parked),
        _ => Err(StoreError::InvalidOp(format!("bad annotation kind: {s}"))),
    }
}

fn parse_annotation_status(s: &str) -> Result<AnnotationStatus> {
    match s {
        "open" => Ok(AnnotationStatus::Open),
        "accepted" => Ok(AnnotationStatus::Accepted),
        "declined" => Ok(AnnotationStatus::Declined),
        _ => Err(StoreError::InvalidOp(format!("bad annotation status: {s}"))),
    }
}

impl SqliteStore {
    /// Blocks whose [[wikilinks]] point at this title (exact or path form),
    /// for rewrite-on-rename. Returns (block_id, doc_id, content).
    pub fn linking_blocks(&self, title: &str) -> Result<Vec<(Uuid, Uuid, String)>> {
        linking_blocks_conn(&self.conn, self.scope, title)
    }

    /// Outcome feedback for an agent: its recent ops with the annotation
    /// verdicts — (op, annotation_status, resolver_name). "What happened to
    /// my proposals, and who decided?"
    pub fn proposal_outcomes(
        &self,
        principal: Uuid,
        limit: usize,
    ) -> Result<Vec<(LedgerOp, Option<String>, Option<String>)>> {
        let sql = format!(
            "SELECT {}, a.status, p.display_name
             FROM ops o
             LEFT JOIN annotations a ON a.op_id = o.id
             LEFT JOIN principals p ON p.id = a.resolved_by
             WHERE o.principal = ?1 AND {}
             ORDER BY o.id DESC LIMIT ?2",
            OP_COLS
                .split(", ")
                .map(|c| format!("o.{c}"))
                .collect::<Vec<_>>()
                .join(", "),
            self.vis("o.doc_id")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![principal.to_string(), limit as i64], |r| {
            let raw = row_to_op(r)?;
            Ok((
                raw,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })?;
        rows.map(|r| {
            let (raw, status, resolver) = r?;
            Ok((build_op(raw)?, status, resolver))
        })
        .collect()
    }

    /// Live progress on a still-running gardener run (status untouched).
    pub fn update_run_progress(&mut self, run: Uuid, summary: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE gardener_runs SET summary = ?1 WHERE id = ?2 AND status = 'running'",
            params![summary, run.to_string()],
        )?;
        Ok(())
    }

    /// Runs left 'running' by a dead daemon (restarts kill in-flight work).
    /// Called at startup; returns how many were marked.
    pub fn mark_orphaned_runs(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE gardener_runs
             SET status = 'failed',
                 summary = COALESCE(summary, '') || char(10) || 'orphaned: daemon restarted mid-run',
                 finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE status = 'running'",
            [],
        )?)
    }

    /// Live blocks whose id ends with `suffix` (the `^abc123` short-ref
    /// resolver, `locate::short_ref`). A suffix scan over `blocks.id`; the
    /// caller turns 0 / >1 rows into the not-found / ambiguity errors.
    pub fn blocks_by_id_suffix(&self, suffix: &str) -> Result<Vec<Block>> {
        let suffix = suffix.to_ascii_lowercase();
        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {BLOCK_COLS} FROM blocks WHERE id LIKE ?1 AND deleted = 0 AND {} ORDER BY id",
            self.vis("doc_id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params![format!("%{suffix}")], row_to_block)?;
        rows.map(|r| build_block(r?)).collect()
    }

    /// All live docs in a subtree, the scope root included — the opt-in
    /// boundary every scoped gardener works within.
    pub fn doc_subtree(&self, root: Uuid) -> Result<Vec<Doc>> {
        self.see(root)?;
        let mut stmt = self.conn.prepare(&format!(
            "WITH RECURSIVE sub(id) AS (
                 SELECT ?1
                 UNION
                 SELECT d.id FROM docs d JOIN sub ON d.parent_id = sub.id WHERE d.deleted = 0
             )
             SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d JOIN sub ON sub.id = d.id
             WHERE d.deleted = 0 AND {}
             ORDER BY d.sort_key IS NULL, d.sort_key, d.title",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map(params![root.to_string()], row_to_doc)?;
        self.masked(rows.map(|r| build_doc(r?)).collect())
    }

    /// Stalest docs first (oldest last-op) within a scope, excluding docs
    /// this gardener already covered — the scoped sweep worklist.
    pub fn audit_candidates(&self, auditor: Uuid, scope: Uuid, limit: usize) -> Result<Vec<Doc>> {
        self.see(scope)?;
        let mut stmt = self.conn.prepare(&format!(
            "WITH RECURSIVE sub(id) AS (
                 SELECT ?3
                 UNION
                 SELECT d.id FROM docs d JOIN sub ON d.parent_id = sub.id WHERE d.deleted = 0
             )
             SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d
             JOIN sub ON sub.id = d.id
             JOIN (SELECT doc_id, max(created_at) AS last FROM ops
                   WHERE epoch_applied IS NOT NULL GROUP BY doc_id) o ON o.doc_id = d.id
             WHERE d.deleted = 0
               AND EXISTS (SELECT 1 FROM blocks b WHERE b.doc_id = d.id AND b.deleted = 0
                           AND b.block_type != 'comment')
               AND NOT EXISTS (SELECT 1 FROM audits a WHERE a.doc_id = d.id AND a.principal = ?1)
               AND {}
             ORDER BY o.last ASC LIMIT ?2",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map(
            params![auditor.to_string(), limit as i64, scope.to_string()],
            row_to_doc,
        )?;
        self.masked(rows.map(|r| build_doc(r?)).collect())
    }

    // --- living answers: cited blocks at the epoch they were read ---

    /// Replace an answer doc's cited-block set (called on create and after
    /// every refresh).
    pub fn record_answer_sources(&mut self, answer_doc: Uuid, sources: &[(Uuid, i64)]) -> Result<()> {
        self.may_write(answer_doc)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM answer_sources WHERE answer_doc_id = ?1",
            params![answer_doc.to_string()],
        )?;
        for (block, epoch) in sources {
            tx.execute(
                "INSERT INTO answer_sources (answer_doc_id, block_id, epoch_at_answer) VALUES (?1, ?2, ?3)",
                params![answer_doc.to_string(), block.to_string(), epoch],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The cited blocks of one answer doc, each with the block's CURRENT
    /// state (epoch now, tombstoned/gone) so a caller can tell stale from
    /// fresh without a second round-trip.
    pub fn answer_sources(&self, answer_doc: Uuid) -> Result<Vec<AnswerSource>> {
        self.see(answer_doc)?;
        // a cited block in a doc the viewer cannot see is reported without
        // its doc's title
        let mut stmt = self.conn.prepare(&format!(
            "SELECT a.block_id, a.epoch_at_answer, a.recorded_at, b.epoch, b.deleted,
                    CASE WHEN {} THEN d.title END
             FROM answer_sources a
             LEFT JOIN blocks b ON b.id = a.block_id
             LEFT JOIN docs d ON d.id = b.doc_id
             WHERE a.answer_doc_id = ?1
             ORDER BY a.recorded_at, a.block_id",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map(params![answer_doc.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<bool>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        rows.map(|r| {
            let (block, at, recorded, now, deleted, title) = r?;
            let gone = now.is_none() || deleted.unwrap_or(true);
            Ok(AnswerSource {
                block_id: uuid_col(block, "answer_sources.block_id")?,
                epoch_at_answer: at,
                recorded_at: recorded,
                current_epoch: now,
                gone,
                doc_title: title,
                changed: gone || now.is_some_and(|n| n > at),
            })
        })
        .collect()
    }

    /// Every live doc with recorded answer sources, oldest recorded first.
    pub fn answer_docs(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT a.answer_doc_id, min(a.recorded_at) AS first
             FROM answer_sources a JOIN docs d ON d.id = a.answer_doc_id
             WHERE d.deleted = 0 AND {}
             GROUP BY a.answer_doc_id ORDER BY first",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| uuid_col(r?, "answer_sources.answer_doc_id")).collect()
    }

    // --- verification stamps (docs.verified_at) ---

    /// Stamp a doc as verified now. Only the auditor/keeper paths call this
    /// (a no-finding evaluation, or a human accepting one of their fixes).
    /// Nothing reads the stamp since the freshness views went; the column and
    /// the bookkeeping stay so a future view has the history.
    pub fn set_doc_verified(&mut self, doc_id: Uuid) -> Result<()> {
        self.see(doc_id)?;
        let n = self.conn.execute(
            "UPDATE docs SET verified_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![doc_id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        Ok(())
    }

    pub fn doc_verified_at(&self, doc_id: Uuid) -> Result<Option<String>> {
        self.see(doc_id)?;
        self.conn
            .query_row(
                "SELECT verified_at FROM docs WHERE id = ?1",
                params![doc_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("doc {doc_id}")))
    }

    /// Mark docs as covered by an auditor (re-audit = delete the rows).
    pub fn record_audits(&mut self, principal: Uuid, doc_ids: &[Uuid]) -> Result<()> {
        for d in doc_ids {
            self.see(*d)?;
            self.conn.execute(
                "INSERT OR REPLACE INTO audits (doc_id, principal) VALUES (?1, ?2)",
                params![d.to_string(), principal.to_string()],
            )?;
        }
        Ok(())
    }

    /// Open agent flags: comment blocks authored by agent principals, with
    /// doc title, author name, and the anchored block's content.
    pub fn agent_flags(&self) -> Result<Vec<(Block, String, String, Option<String>)>> {
        let sql = format!(
            "SELECT {}, d.title, p.display_name, t.content
             FROM blocks b
             JOIN docs d ON d.id = b.doc_id
             JOIN principals p ON p.id = b.created_by AND p.kind = 'agent'
             LEFT JOIN blocks t ON t.id = b.refers_to
             WHERE b.block_type = 'comment' AND b.deleted = 0 AND {}
             ORDER BY b.id DESC",
            b_cols(),
            self.vis("d.id")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            let raw = row_to_block(r)?;
            Ok((
                raw,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, Option<String>>(12)?,
            ))
        })?;
        rows.map(|r| {
            let (raw, title, author, target) = r?;
            Ok((build_block(raw)?, title, author, target))
        })
        .collect()
    }

    /// Cheap fingerprint of everything the UI renders: changes whenever ops
    /// land, docs are created/moved/deleted/statused, annotations resolve, or
    /// gardener runs progress. The app polls this to live-refresh.
    pub fn change_stamp(&self) -> Result<i64> {
        if let Some(sub) = tenancy::vis_sub(self.scope) {
            return self
                .conn
                .query_row(
                    &format!(
                        "WITH v(id) AS ({sub})
                         SELECT (SELECT COALESCE(max(rowid), 0) FROM ops WHERE doc_id IN (SELECT id FROM v))
                              + (SELECT count(*) FROM docs WHERE deleted = 0 AND id IN (SELECT id FROM v)) * 1000003
                              + (SELECT COALESCE(sum(current_epoch), 0) FROM docs WHERE id IN (SELECT id FROM v))
                              + (SELECT count(*) FROM annotations WHERE status != 'open' AND doc_id IN (SELECT id FROM v)) * 7919
                              + (SELECT COALESCE(sum(length(coalesce(sort_key,'')) + length(coalesce(parent_id,'')) + length(title)
                                                     + length(coalesce(status,''))), 0)
                                 FROM docs WHERE deleted = 0 AND id IN (SELECT id FROM v))",
                    ),
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into);
        }
        self.conn
            .query_row(
                "SELECT (SELECT COALESCE(max(rowid), 0) FROM ops)
                      + (SELECT count(*) FROM docs WHERE deleted = 0) * 1000003
                      + (SELECT COALESCE(sum(current_epoch), 0) FROM docs)
                      + (SELECT count(*) FROM annotations WHERE status != 'open') * 7919
                      + (SELECT COALESCE(max(rowid), 0) FROM gardener_runs) * 104729
                      + (SELECT count(*) FROM gardener_runs WHERE status != 'running') * 31
                      + (SELECT COALESCE(sum(length(coalesce(summary,''))), 0) FROM gardener_runs)
                      + (SELECT COALESCE(sum(length(coalesce(sort_key,'')) + length(coalesce(parent_id,'')) + length(title)), 0) FROM docs WHERE deleted = 0)
                      + (SELECT COALESCE(rev, 0) FROM doc_revs WHERE id = 1) * 1299709",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
    }

    /// Run `f` inside one deferred read transaction: every read in it sees
    /// the same snapshot, even with another process writing the file (WAL).
    /// `f` must only read — a write path opening its own transaction inside
    /// would fail. The transaction is closed whatever `f` returns.
    pub fn read_snapshot<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Result<T> {
        self.conn.execute_batch("BEGIN DEFERRED")?;
        let out = f(self);
        self.conn.execute_batch("COMMIT")?;
        Ok(out)
    }

    /// The change journal's head: the highest `seq`, 0 when empty.
    pub fn latest_change_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("SELECT COALESCE(max(seq), 0) FROM changes")?
            .query_row([], |r| r.get(0))?)
    }

    /// Journal rows with `seq > since`, oldest first, at most `limit`;
    /// `more` says rows remain past the page.
    pub fn changes_since(&self, since: i64, limit: usize) -> Result<ChangePage> {
        // global rows the viewer can see now, plus the rows addressed to
        // them (access gained / revoked); System/Local read global rows only
        let filter = match self.scope.user() {
            None => "c.user_id IS NULL".to_string(),
            Some(u) => format!(
                "((c.user_id IS NULL AND {}) OR c.user_id = '{u}')",
                self.vis("c.doc_id")
            ),
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT c.seq, c.doc_id, c.kind, c.epoch, c.at,
                    d.id, d.title, d.parent_id, d.sort_key, d.status, d.current_epoch, d.deleted,
                    c.user_id IS NOT NULL
             FROM changes c LEFT JOIN docs d ON d.id = c.doc_id
             WHERE c.seq > ?1 AND {filter} ORDER BY c.seq LIMIT ?2"
        ))?;
        let mut changes: Vec<Change> = stmt
            .query_map(params![since, limit as i64 + 1], |r| {
                let targeted: bool = r.get(12)?;
                let kind: String = r.get(2)?;
                // a revoked doc is gone for this viewer: no summary at all
                let revoked = targeted && kind == "deleted";
                let access = targeted.then(|| if revoked { "revoked" } else { "granted" }.to_string());
                let doc = match r.get::<_, Option<String>>(5)? {
                    Some(_) if !revoked => Some(DocSummary {
                        title: r.get(6)?,
                        parent_id: r.get(7)?,
                        sort_key: r.get(8)?,
                        status: r.get(9)?,
                        current_epoch: r.get(10)?,
                        deleted: r.get::<_, i64>(11)? != 0,
                        workspace_id: None,
                    }),
                    _ => None,
                };
                Ok(Change {
                    seq: r.get(0)?,
                    doc_id: r.get(1)?,
                    kind,
                    epoch: r.get(3)?,
                    at: r.get(4)?,
                    doc,
                    access,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let more = changes.len() > limit;
        changes.truncate(limit);
        let mut resolved: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
        for c in changes.iter_mut() {
            let Some(doc) = c.doc.as_mut() else { continue };
            if !resolved.contains_key(&c.doc_id) {
                let ws = match Uuid::parse_str(&c.doc_id) {
                    Ok(id) => crate::workspaces::resolve_conn(&self.conn, id)?.map(|w| w.to_string()),
                    Err(_) => None,
                };
                resolved.insert(c.doc_id.clone(), ws);
            }
            doc.workspace_id = resolved[&c.doc_id].clone();
            // the parent's id only when the viewer can see the parent too
            if !self.scope.sees_all()
                && let Some(p) = doc.parent_id.as_deref().and_then(|p| Uuid::parse_str(p).ok())
                && tenancy::ensure_visible_conn(&self.conn, self.scope, p).is_err()
            {
                doc.parent_id = None;
            }
        }
        Ok(ChangePage { seq: self.latest_change_seq()?, changes, more })
    }

    /// Call `f` after every commit on this connection (autocommit statements
    /// included) — the change feed's wake-up. `f` runs inside SQLite's commit
    /// and must not touch the database; signal and return. Replaces any
    /// previous hook.
    pub fn on_commit(&self, f: impl Fn() + Send + 'static) {
        self.conn.commit_hook(Some(move || {
            f();
            false
        }));
    }

    /// The doc's most recent applied ops, newest first (epoch, then id,
    /// descending), capped at `limit` — the history panel's page, fetched
    /// in one bounded query instead of the whole ledger reversed and cut.
    pub fn ops_for_doc_limited(&self, doc_id: Uuid, limit: usize) -> Result<Vec<LedgerOp>> {
        self.see(doc_id)?;
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {OP_COLS} FROM ops
             WHERE doc_id = ?1 AND epoch_applied IS NOT NULL
             ORDER BY epoch_applied DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![doc_id.to_string(), limit as i64], row_to_op)?;
        rows.map(|r| build_op(r?)).collect()
    }

    /// Docs whose content is a canvas scene (for tree/type badges).
    pub fn canvas_doc_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT doc_id FROM blocks WHERE block_type = 'canvas_scene' AND deleted = 0 AND {}",
            self.vis("doc_id")
        ))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// (doc_id, principal) of each doc's last applied op — "who tends this".
    pub fn raw_tending(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT doc_id, principal, max(epoch_applied) FROM ops
             WHERE epoch_applied IS NOT NULL AND {} GROUP BY doc_id",
            self.vis("doc_id")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// Doc-to-doc edges: wikilinks resolved by title (graph view, 5.10).
    pub fn raw_links(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT b.doc_id, d2.id
             FROM edges e
             JOIN blocks b ON b.id = e.from_block AND b.deleted = 0
             JOIN docs d1 ON d1.id = b.doc_id AND d1.deleted = 0
             JOIN docs d2 ON (e.to_target = d2.title OR e.to_target LIKE '%/' || d2.title)
             WHERE b.doc_id != d2.id AND d2.deleted = 0 AND {} AND {}",
            self.vis("d1.id"),
            self.vis("d2.id")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// doc_id → tags (graph clustering).
    pub fn raw_doc_tags(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT t.doc_id, t.tag FROM doc_tags t
                 JOIN docs d ON d.id = t.doc_id AND d.deleted = 0
                 WHERE {}
                 ORDER BY t.doc_id",
                self.vis("d.id")
            ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
        for r in rows {
            let (d, t) = r?;
            out.entry(d).or_default().push(t);
        }
        Ok(out)
    }

    /// Every live block of every live doc with its doc title, in doc/tree
    /// order — one pass for the agent `grep` tool (daemon retrieval.rs).
    pub fn live_blocks_with_titles(&self) -> Result<Vec<SearchHit>> {
        let sql = format!(
            "SELECT {}, d.title FROM blocks b JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0 AND {}
             ORDER BY b.doc_id, b.parent_id IS NOT NULL, b.order_key",
            b_cols(),
            self.vis("d.id")
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map([], |r| {
            let raw = row_to_block(r)?;
            let title: String = r.get(10)?;
            Ok((raw, title))
        })?;
        rows.map(|r| {
            let (raw, doc_title) = r?;
            Ok(SearchHit {
                block: build_block(raw)?,
                doc_title,
            })
        })
        .collect()
    }

    /// doc id → live block count (size hints for the `orient` map).
    pub fn block_counts(&self) -> Result<std::collections::HashMap<Uuid, i64>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT b.doc_id, count(*) FROM blocks b JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0 AND {} GROUP BY b.doc_id",
            self.vis("d.id")
        ))?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for r in rows {
            let (d, n) = r?;
            out.insert(uuid_col(d, "blocks.doc_id")?, n);
        }
        Ok(out)
    }
}

/// A user's own copy of a setting key (ADR 0004).
fn user_setting_key(user: Uuid, key: &str) -> String {
    format!("user.{user}.{key}")
}

/// The root owner of `doc`'s tree (None = unowned: the instance owner).
fn root_owner_conn(conn: &Connection, doc: Uuid) -> Result<Option<Uuid>> {
    let o: Option<Option<String>> = conn
        .prepare_cached(
            "WITH RECURSIVE up(id, parent_id, owner, depth) AS (
                 SELECT id, parent_id, owner_id, 0 FROM docs WHERE id = ?1
                 UNION ALL
                 SELECT d.id, d.parent_id, d.owner_id, up.depth + 1 FROM docs d JOIN up ON d.id = up.parent_id
                 WHERE up.depth < 256)
             SELECT owner FROM up ORDER BY depth DESC LIMIT 1",
        )?
        .query_row(params![doc.to_string()], |r| r.get(0))
        .optional()?;
    o.flatten().map(|o| uuid_col(o, "docs.owner_id")).transpose()
}

/// Authorize a move of `doc` under `new_parent` (None = the root) for
/// `scope`: write on the doc and the new parent, and — when the doc's space
/// changes — ownership of the source and write on the destination. Returns
/// (from, to) spaces.
fn check_move(
    conn: &Connection,
    scope: Scope,
    doc: Uuid,
    new_parent: Option<Uuid>,
) -> Result<(tenancy::Space, tenancy::Space)> {
    tenancy::ensure_write_conn(conn, scope, doc)?;
    if let Some(p) = new_parent {
        tenancy::ensure_write_conn(conn, scope, p).map_err(|e| match e {
            StoreError::NotFound(_) => StoreError::NotFound(format!("new_parent doc {p}")),
            e => e,
        })?;
    }
    let from = tenancy::space_conn(conn, doc)?;
    // an own label travels with the doc; otherwise it takes the new
    // parent's space, or (at the root) the mover's Unsorted
    let to = match crate::workspaces::label_conn(conn, doc)? {
        Some(w) => tenancy::Space::Workspace(w),
        None => match new_parent {
            Some(p) => tenancy::space_conn(conn, p)?,
            None => match scope.user() {
                Some(_) => tenancy::own_unsorted(conn, scope)?,
                None => match root_owner_conn(conn, doc)? {
                    Some(o) => tenancy::Space::Unsorted(Some(o)),
                    None => tenancy::Space::Unsorted(tenancy::instance_owner_conn(conn)?),
                },
            },
        },
    };
    tenancy::check_space_change(conn, scope, from, to)?;
    Ok((from, to))
}

/// A doc moved to the root becomes a root of its own: in a user scope, the
/// mover's. (System/Local keep whatever owner it had.)
fn reroot_owner(conn: &Connection, scope: Scope, doc: Uuid, new_parent: Option<Uuid>) -> Result<()> {
    if new_parent.is_none()
        && let Some(u) = scope.user()
    {
        conn.execute(
            "UPDATE docs SET owner_id = ?1 WHERE id = ?2",
            params![u.to_string(), doc.to_string()],
        )?;
    }
    Ok(())
}

/// A subtree delete may not reach into docs the scope cannot write (a
/// descendant labelled into someone else's workspace).
fn check_subtree_writable(conn: &Connection, scope: Scope, doc: Uuid) -> Result<()> {
    if scope.sees_all() {
        return Ok(());
    }
    for d in subtree_ids_conn(conn, doc)? {
        if tenancy::ensure_write_conn(conn, scope, d).is_err() {
            return Err(StoreError::Forbidden(
                "this subtree holds docs you cannot write; move them out first".into(),
            ));
        }
    }
    Ok(())
}

impl SqliteStore {
    /// NotFound unless the scope can see `doc`.
    pub(crate) fn see(&self, doc: Uuid) -> Result<()> {
        tenancy::ensure_visible_conn(&self.conn, self.scope, doc)
    }

    /// NotFound when invisible, Forbidden when read-only.
    pub(crate) fn may_write(&self, doc: Uuid) -> Result<()> {
        tenancy::ensure_write_conn(&self.conn, self.scope, doc)
    }

    /// The visibility predicate on a doc-id column.
    pub(crate) fn vis(&self, col: &str) -> String {
        tenancy::vis_pred(self.scope, col)
    }

    /// Hide a parent the viewer cannot see (the doc is a root for them).
    pub(crate) fn mask_parent(&self, doc: &mut Doc) {
        if !self.scope.sees_all()
            && let Some(p) = doc.parent_id
            && tenancy::ensure_visible_conn(&self.conn, self.scope, p).is_err()
        {
            doc.parent_id = None;
        }
    }

    pub(crate) fn masked(&self, docs: Result<Vec<Doc>>) -> Result<Vec<Doc>> {
        let mut docs = docs?;
        for d in docs.iter_mut() {
            self.mask_parent(d);
        }
        Ok(docs)
    }

    /// Gardeners a scope may see: its user's own (NULL owner = the instance
    /// owner's); System/Local see all.
    pub(crate) fn gardener_pred(&self, col: &str) -> String {
        match self.scope.user() {
            None => "1".into(),
            Some(u) => format!("COALESCE({col}, {}) = '{u}'", tenancy::INSTANCE_OWNER_SQL),
        }
    }

    /// The owner a new doc gets: a child takes its root's owner (after a
    /// write check on the parent); a root, the creating user (None in
    /// System/Local = the instance owner).
    pub(crate) fn new_doc_owner(&self, parent: Option<Uuid>) -> Result<Option<Uuid>> {
        match parent {
            Some(p) => {
                self.may_write(p)?;
                root_owner_conn(&self.conn, p)
            }
            None => Ok(self.scope.user()),
        }
    }
}

fn b_cols() -> String {
    BLOCK_COLS
        .split(", ")
        .map(|c| format!("b.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// OR-of-trigrams FTS query for typo-tolerant matching; None below 3 chars.
fn fts_query(q: &str) -> Option<String> {
    let mut tris: Vec<String> = Vec::new();
    for token in q.split_whitespace() {
        let chars: Vec<char> = token
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        for w in chars.windows(3) {
            tris.push(format!("\"{}\"", w.iter().collect::<String>()));
        }
    }
    tris.dedup();
    tris.truncate(30);
    if tris.is_empty() {
        None
    } else {
        Some(tris.join(" OR "))
    }
}

impl SqliteStore {
    fn propose_impl(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
        cap_review: bool,
    ) -> Result<ProposeOutcome> {
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("propose: empty op list".into()));
        }
        if let Some(op) = ops.iter().find(|o| o.kind.is_doc_op()) {
            return Err(StoreError::InvalidOp(format!(
                "propose: {} is a doc op — use rename_doc/move_doc/set_status/delete_doc",
                op.kind.op_type()
            )));
        }
        let policy = self.effective_policy(doc_id)?;
        // the share gate (ADR 0004 §5): an agent writing into a shared
        // workspace never lands green, and `auto` cannot clean it
        let shared_cap = tenancy::agent_into_shared(&self.conn, doc_id, principal)?;
        let cap_review = cap_review || shared_cap;
        let tx = self.conn.transaction()?;
        let current = doc_epoch(&tx, doc_id)?;
        if base_epoch > current {
            return Err(StoreError::InvalidOp(format!(
                "propose: base epoch {base_epoch} is ahead of doc epoch {current}"
            )));
        }

        let candidate_epoch = current + 1;
        let mut verdicts = Vec::with_capacity(ops.len());
        let mut applied_any = false;
        for op in &ops {
            let op_id = Uuid::now_v7();
            let mut op = op.clone();
            resolve_order_keys(&tx, doc_id, &mut op.kind)?;
            let op = &op;
            let mut scored = if base_epoch == current {
                Scored {
                    verdict: Verdict::Green,
                    confidence: 1.0,
                    note: "current base".into(),
                }
            } else {
                let mut lookup = |id: Uuid| block_by_id(&tx, doc_id, id).ok().flatten();
                score_stale_op(&op.kind, base_epoch, &mut lookup)
            };
            // review cap: greens land as applied, flagged yellows (§5:
            // auto-tagging as reviewable yellows, declinable as a batch)
            if cap_review && scored.verdict == Verdict::Green {
                scored.verdict = Verdict::Yellow;
                scored.note = if shared_cap {
                    format!("shared workspace: an agent's edit is flagged for its members; {}", scored.note)
                } else {
                    format!("review requested by proposer; {}", scored.note)
                };
            }

            let prior = match op.kind.target_block() {
                Some(t) => block_by_id(&tx, doc_id, t)?,
                None => None,
            };

            let mut applied = false;
            if scored.verdict != Verdict::Red {
                match project(&tx, doc_id, candidate_epoch, principal, &op.kind) {
                    Ok(()) => applied = true,
                    // content-level failure downgrades to a parked red;
                    // the gate never errors on content
                    Err(e @ (StoreError::InvalidOp(_) | StoreError::NotFound(_))) => {
                        scored = Scored {
                            verdict: Verdict::Red,
                            confidence: 0.0,
                            note: format!("projection failed: {e}"),
                        };
                    }
                    Err(e) => return Err(e),
                }
            }
            insert_op_row(
                &tx,
                op_id,
                doc_id,
                op,
                principal,
                base_epoch,
                applied.then_some(candidate_epoch),
                scored.verdict,
                scored.confidence,
                &prior,
            )?;
            match scored.verdict {
                Verdict::Green => {}
                Verdict::Yellow => {
                    // auto policy self-applies high-confidence yellows: no flag
                    let auto_clean = !cap_review
                        && policy == ReviewPolicy::Auto
                        && scored.confidence >= crate::gate::HIGH_CONFIDENCE;
                    if !auto_clean {
                        insert_annotation(&tx, doc_id, op_id, AnnotationKind::Review)?;
                    }
                }
                Verdict::Red => {
                    insert_annotation(&tx, doc_id, op_id, AnnotationKind::Parked)?;
                }
            }
            applied_any |= applied;
            verdicts.push(ProposeVerdict {
                op_id,
                block_id: op.kind.target_block(),
                verdict: scored.verdict,
                confidence: scored.confidence,
                applied,
                note: scored.note,
            });
        }

        let epoch = if applied_any {
            candidate_epoch
        } else {
            current
        };
        if applied_any {
            tx.execute(
                "UPDATE docs SET current_epoch = ?1 WHERE id = ?2",
                params![epoch, doc_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(ProposeOutcome {
            doc_id,
            epoch,
            verdicts,
        })
    }
}

/// Doc ops (AX slice B) at the store boundary: ledger shape, gate rules,
/// link rewrites, the ops CHECK migration, and the optional insert id.
#[cfg(test)]
mod doc_op_tests {
    use super::*;
    use crate::{PrincipalKind, import::import_markdown};

    fn setup() -> (SqliteStore, Uuid, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let bot = s.create_principal(PrincipalKind::Agent, "claude:t", None).unwrap().id;
        (s, tom, bot)
    }

    #[test]
    fn rename_ledgers_pre_image_and_never_bumps_the_epoch() {
        let (mut s, tom, bot) = setup();
        let (d, _) = import_markdown(&mut s, "Old", None, tom, "body").unwrap();
        let epoch_before = s.get_doc(d).unwrap().current_epoch;
        let out = s
            .propose_doc_op(
                d,
                bot,
                OpKind::RenameDoc {
                    title: "  New  ".into(),
                    from_title: "caller lies".into(),
                },
                vec!["t".into()],
            )
            .unwrap();
        assert_eq!(out.epoch, epoch_before);
        assert_eq!(s.get_doc(d).unwrap().current_epoch, epoch_before, "doc ops never bump the epoch");
        assert_eq!(s.get_doc(d).unwrap().title, "New", "trimmed");
        let ops = s.ops_since(d, epoch_before - 1).unwrap();
        let op = ops.iter().find(|o| o.kind.is_doc_op()).expect("ledgered");
        assert_eq!(op.verdict, Some(Verdict::Yellow));
        assert_eq!(op.epoch_applied, Some(epoch_before));
        assert!(op.prior.is_none());
        assert!(matches!(&op.kind, OpKind::RenameDoc { from_title, .. } if from_title == "Old"), "server-owned pre-image");
        // accept keeps it; the outcome feed shows the resolver
        let q = s.review_queue(Some(d)).unwrap();
        s.resolve(q[0].annotation.id, tom, ReviewDecision::Accept).unwrap();
        assert_eq!(s.get_doc(d).unwrap().title, "New");
        let (_, status, who) = &s.proposal_outcomes(bot, 5).unwrap()[0];
        assert_eq!((status.as_deref(), who.as_deref()), (Some("accepted"), Some("tom")));
    }

    #[test]
    fn block_gate_refuses_doc_ops_and_doc_gate_refuses_block_ops() {
        let (mut s, tom, bot) = setup();
        let d = s.create_doc("D", None, tom).unwrap();
        let doc_op = OpInput {
            kind: OpKind::SetStatus { status: Some(DocStatus::Draft), from_status: None },
            source_refs: vec![],
        };
        assert!(s.propose(d.id, 0, bot, vec![doc_op]).is_err());
        assert!(s.propose_doc_op(d.id, bot, OpKind::Delete { target: Uuid::now_v7() }, vec![]).is_err());
        // a trashed doc takes no doc ops
        s.delete_doc(d.id).unwrap();
        assert!(matches!(
            s.propose_doc_op(d.id, bot, OpKind::SetStatus { status: None, from_status: None }, vec![]),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn insert_without_block_id_gets_one_minted_and_returned() {
        let (mut s, tom, _) = setup();
        let d = s.create_doc("D", None, tom).unwrap();
        let ops: Vec<OpInput> = serde_json::from_str(
            r#"[{"kind":{"op":"insert","parent_id":null,"order_key":"","block_type":"paragraph","content":"hi"}}]"#,
        )
        .unwrap();
        let out = s.propose(d.id, 0, tom, ops).unwrap();
        let id = out.verdicts[0].block_id.expect("minted id returned");
        assert_eq!(s.read_block(id).unwrap().content, "hi");
        assert_eq!(out.verdicts[0].op_id != id, true);
    }

    /// A db from before doc ops has `op_type IN ('insert','replace','delete','move')`;
    /// opening it rebuilds `ops` with the wider CHECK, keeping rows and the
    /// annotations FK, and is idempotent.
    #[test]
    fn opening_a_db_with_the_old_op_type_check_widens_it() {
        let (mut s, tom, bot) = setup();
        let (d, _) = import_markdown(&mut s, "D", None, tom, "para").unwrap();
        s.conn.pragma_update(None, "foreign_keys", false).unwrap();
        s.conn
            .execute_batch(
                "CREATE TABLE ops_old AS SELECT * FROM ops;
                 DROP TABLE ops;
                 CREATE TABLE ops (
                     id TEXT PRIMARY KEY,
                     doc_id TEXT NOT NULL REFERENCES docs (id),
                     op_type TEXT NOT NULL CHECK (op_type IN ('insert', 'replace', 'delete', 'move')),
                     target_block TEXT,
                     payload TEXT NOT NULL,
                     principal TEXT NOT NULL REFERENCES principals (id),
                     base_epoch INTEGER NOT NULL,
                     epoch_applied INTEGER,
                     verdict TEXT CHECK (verdict IN ('green', 'yellow', 'red')),
                     confidence REAL,
                     prior TEXT,
                     source_refs TEXT NOT NULL DEFAULT '[]',
                     created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 );
                 INSERT INTO ops SELECT * FROM ops_old;
                 DROP TABLE ops_old;",
            )
            .unwrap();
        s.conn.pragma_update(None, "foreign_keys", true).unwrap();
        // park a block op so an annotation references an op across the rebuild
        let target = s.read_doc(d).unwrap().roots[0].block.id;
        s.park(d, bot, vec![OpInput { kind: OpKind::Delete { target }, source_refs: vec![] }], "n").unwrap();
        let rows_before: i64 = s.conn.query_row("SELECT count(*) FROM ops", [], |r| r.get(0)).unwrap();
        // the old CHECK refuses a doc op
        assert!(
            s.propose_doc_op(d, bot, OpKind::SetStatus { status: Some(DocStatus::Draft), from_status: None }, vec![])
                .is_err()
        );
        migrate_pre_schema(&s.conn).unwrap();
        s.propose_doc_op(d, bot, OpKind::SetStatus { status: Some(DocStatus::Draft), from_status: None }, vec![])
            .unwrap();
        let rows_after: i64 = s.conn.query_row("SELECT count(*) FROM ops", [], |r| r.get(0)).unwrap();
        assert_eq!(rows_after, rows_before + 1);
        assert_eq!(s.review_queue(None).unwrap().len(), 2, "parked block op + status yellow");
        let fk: i64 = s
            .conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 0);
        migrate_pre_schema(&s.conn).unwrap();
        assert_eq!(s.review_queue(None).unwrap().len(), 2);
        let idx: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 'ops'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(idx >= 2, "both ops indexes recreated");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A move without a client key appends under the new parent instead of
    /// re-NULLing the key; a malformed key is refused.
    /// v7: the federation tables of an old database are dropped on open —
    /// rows and all, children before parents, with foreign keys on — and
    /// hub settings go with them; docs, blocks, ops and other settings stay.
    #[test]
    fn opening_a_db_with_federation_tables_drops_them() {
        use crate::{BlockStore, PrincipalKind};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ks.db");
        let (doc, tom) = {
            let mut s = SqliteStore::open(&path).unwrap();
            let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
            let (doc, _) = crate::import::import_markdown(&mut s, "Shared", None, tom, "# A\n\nbody\n").unwrap();
            s.conn.execute_batch(FEDERATION_DDL).unwrap();
            let peer = Uuid::now_v7();
            s.conn
                .execute_batch(&format!(
                    "INSERT INTO principals (id, kind, display_name, pubkey) VALUES ('{peer}', 'remote', 'alice', 'k1');
                     INSERT INTO contacts (id, pubkey, petname, principal) VALUES ('c1', 'k1', 'alice', '{peer}');
                     INSERT INTO shares (id, root_doc, contact, state) VALUES ('s1', '{doc}', 'c1', 'active');
                     INSERT INTO share_invites (id, share_id, secret_hash, expires_at, redeemed_by)
                         VALUES ('i1', 's1', 'h', '2099-01-01', 'c1');
                     INSERT INTO share_offers (id, from_contact, owner_node, share_id, root_title, permission, secret, expires_at)
                         VALUES ('o1', 'c1', 'k1', 's9', 'T', 'view', 'x', '2099-01-01');
                     INSERT INTO mirrors (doc_id, owner, share_id) VALUES ('{doc}', 'c1', 's9');
                     INSERT INTO hub_publications (share_id, member_contact, root_doc) VALUES ('s2', 'c1', '{doc}');
                     INSERT INTO hub_forwards (op_id, owner_contact, member_contact, owner_share, doc_id)
                         VALUES ('op1', 'c1', 'c1', 's1', '{doc}');
                     INSERT INTO hub_transfers (id, member_contact, root_doc, title) VALUES ('t1', 'c1', '{doc}', 'T');
                     INSERT INTO doc_transfers (id, root_doc, counterparty, direction) VALUES ('d1', '{doc}', 'c1', 'out');
                     INSERT INTO pending_joins (id, ticket) VALUES ('j1', 'ticket');
                     INSERT INTO outbound_proposals (id, doc_id, share_id, owner, op_ids)
                         VALUES ('ob1', '{doc}', 's9', 'c1', '[]');
                     INSERT INTO settings (key, value) VALUES ('hub.enabled', 'x'), ('hub.name', 'Team'), ('log.level', 'info');"
                ))
                .unwrap();
            (doc, tom)
        };
        let count = |s: &SqliteStore, sql: &str| -> i64 { s.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        let s = SqliteStore::open(&path).unwrap();
        for t in FEDERATION_TABLES {
            let n = count(&s, &format!("SELECT count(*) FROM sqlite_master WHERE name = '{t}'"));
            assert_eq!(n, 0, "{t} still there");
        }
        assert_eq!(count(&s, "SELECT count(*) FROM settings WHERE key LIKE 'hub.%'"), 0);
        assert_eq!(s.get_setting("log.level").unwrap().as_deref(), Some("info"));
        assert_eq!(count(&s, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
        // the content and its provenance are untouched, remote principal included
        assert_eq!(s.read_doc(doc).unwrap().roots.len(), 1, "the heading, its paragraph nested");
        assert_eq!(s.ops_since(doc, 0).unwrap().len(), 2);
        assert_eq!(s.list_principals().unwrap().len(), 2);
        let _ = tom;
        drop(s);
        // a second open is a no-op
        SqliteStore::open(&path).unwrap();
    }

    /// v7: reviewer gardeners are disabled once (rows and runs kept), and no
    /// new one can be created.
    #[test]
    fn v7_disables_reviewer_gardeners_and_refuses_new_ones() {
        use crate::{BlockStore, ConfidencePolicy, GardenerKind, PrincipalKind};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ks.db");
        {
            let mut s = SqliteStore::open(&path).unwrap();
            let p = s.create_principal(PrincipalKind::Agent, "rev", None).unwrap().id;
            s.conn
                .execute(
                    "INSERT INTO gardeners (id, name, kind, principal, task_prompt) VALUES (?1, 'rev', 'reviewer', ?2, 'r')",
                    params![Uuid::now_v7().to_string(), p.to_string()],
                )
                .unwrap();
            s.create_gardener("tags", GardenerKind::Tagging, "t", None, ConfidencePolicy::Review).unwrap();
            s.conn.pragma_update(None, "user_version", 6).unwrap();
        }
        let mut s = SqliteStore::open(&path).unwrap();
        let gs = s.list_gardeners().unwrap();
        let rev = gs.iter().find(|g| g.kind == GardenerKind::Reviewer).unwrap();
        assert!(!rev.enabled, "disabled by the migration");
        assert!(gs.iter().find(|g| g.kind == GardenerKind::Tagging).unwrap().enabled);
        assert!(s.create_gardener("r2", GardenerKind::Reviewer, "r", None, ConfidencePolicy::Review).is_err());
        let v: i64 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    const FEDERATION_DDL: &str = "
CREATE TABLE IF NOT EXISTS contacts (
    id        TEXT PRIMARY KEY,
    pubkey    TEXT NOT NULL UNIQUE,
    petname   TEXT NOT NULL,
    principal TEXT NOT NULL REFERENCES principals (id),
    verified  INTEGER NOT NULL DEFAULT 0,
    revoked   INTEGER NOT NULL DEFAULT 0,
    paired_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    role       TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('member', 'admin')),
    membership TEXT NOT NULL DEFAULT 'active' CHECK (membership IN ('pending', 'active', 'ejected')),
    is_hub     INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS shares (
    id         TEXT PRIMARY KEY,
    root_doc   TEXT NOT NULL REFERENCES docs (id),
    contact    TEXT REFERENCES contacts (id),
    permission TEXT NOT NULL DEFAULT 'view' CHECK (permission IN ('view', 'propose')),
    state      TEXT NOT NULL DEFAULT 'offered' CHECK (state IN ('offered', 'active', 'revoked')),
    policy_override TEXT CHECK (policy_override IN ('human-review', 'agent-review', 'auto')),
    trust      TEXT NOT NULL DEFAULT 'review' CHECK (trust IN ('review', 'yellow', 'green')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS shares_by_contact ON shares (contact);
CREATE TABLE IF NOT EXISTS share_invites (
    id          TEXT PRIMARY KEY,
    share_id    TEXT NOT NULL REFERENCES shares (id),
    secret_hash TEXT NOT NULL UNIQUE,
    expires_at  TEXT NOT NULL,
    redeemed_by TEXT REFERENCES contacts (id),
    redeemed_at TEXT,
    offered_to  TEXT REFERENCES contacts (id),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE IF NOT EXISTS share_offers (
    id           TEXT PRIMARY KEY,
    from_contact TEXT NOT NULL REFERENCES contacts (id),
    owner_node   TEXT NOT NULL,
    share_id     TEXT NOT NULL,
    root_title   TEXT NOT NULL,
    permission   TEXT NOT NULL CHECK (permission IN ('view', 'propose')),
    secret       TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'accepted', 'declined', 'expired')),
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at   TEXT NOT NULL,
    UNIQUE (owner_node, share_id)
);
CREATE TABLE IF NOT EXISTS mirrors (
    doc_id       TEXT PRIMARY KEY REFERENCES docs (id),
    owner        TEXT NOT NULL REFERENCES contacts (id),
    share_id     TEXT NOT NULL,
    synced_epoch INTEGER NOT NULL DEFAULT 0,
    permission   TEXT NOT NULL DEFAULT 'view' CHECK (permission IN ('view', 'propose')),
    owner_tended INTEGER NOT NULL DEFAULT 0,
    last_pulled_at TEXT,
    last_error     TEXT,
    owner_epoch  INTEGER NOT NULL DEFAULT 0,
    origin_owner      TEXT,
    origin_owner_name TEXT
);
CREATE TABLE IF NOT EXISTS hub_publications (
    share_id       TEXT PRIMARY KEY,
    member_contact TEXT NOT NULL REFERENCES contacts (id),
    root_doc       TEXT NOT NULL,
    published_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE IF NOT EXISTS hub_forwards (
    op_id          TEXT PRIMARY KEY,
    owner_contact  TEXT NOT NULL REFERENCES contacts (id),
    member_contact TEXT NOT NULL REFERENCES contacts (id),
    owner_share    TEXT NOT NULL,
    doc_id         TEXT NOT NULL,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS hub_forwards_by_created ON hub_forwards (created_at);
CREATE TABLE IF NOT EXISTS hub_transfers (
    id             TEXT PRIMARY KEY,
    member_contact TEXT NOT NULL REFERENCES contacts (id),
    root_doc       TEXT NOT NULL,
    title          TEXT NOT NULL,
    doc_count      INTEGER NOT NULL DEFAULT 0,
    state          TEXT NOT NULL DEFAULT 'offered'
        CHECK (state IN ('offered', 'accepted', 'declined', 'done')),
    at             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE IF NOT EXISTS doc_transfers (
    id           TEXT PRIMARY KEY,
    root_doc     TEXT NOT NULL,
    counterparty TEXT NOT NULL REFERENCES contacts (id),
    direction    TEXT NOT NULL CHECK (direction IN ('out', 'in')),
    state        TEXT NOT NULL DEFAULT 'offered' CHECK (state IN ('offered', 'done')),
    at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS doc_transfers_by_root ON doc_transfers (root_doc);
CREATE TABLE IF NOT EXISTS pending_joins (
    id         TEXT PRIMARY KEY,
    ticket     TEXT NOT NULL UNIQUE,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE IF NOT EXISTS outbound_proposals (
    id         TEXT PRIMARY KEY,
    doc_id     TEXT NOT NULL REFERENCES docs (id),
    share_id   TEXT NOT NULL,
    owner      TEXT NOT NULL REFERENCES contacts (id),
    op_ids     TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'accepted', 'declined', 'mixed')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
";

    #[test]
    fn move_doc_without_key_appends_and_rejects_invalid_keys() {
        use crate::PrincipalKind;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "Tom", None).unwrap();
        let folder = s.create_doc("folder", None, tom.id).unwrap();
        let first = s.create_doc("first", Some(folder.id), tom.id).unwrap();
        let mover = s.create_doc("mover", None, tom.id).unwrap();
        s.move_doc(mover.id, Some(folder.id), None).unwrap();
        let moved = s.get_doc(mover.id).unwrap();
        assert_eq!(moved.parent_id, Some(folder.id));
        let key = moved.sort_key.expect("move must not NULL the key");
        assert!(crate::order_key::is_valid(&key));
        assert!(key > first.sort_key.unwrap(), "appended after the last sibling");
        let err = s.move_doc(mover.id, None, Some("A0")).unwrap_err();
        assert!(matches!(err, StoreError::InvalidOp(_)), "{err}");
        assert_eq!(s.get_doc(mover.id).unwrap().parent_id, Some(folder.id));
    }

    /// A trashed sibling keeps its key; a new doc must key past it so the
    /// restore does not produce two siblings with the same key.
    #[test]
    fn new_doc_key_clears_a_trashed_siblings_key() {
        use crate::PrincipalKind;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "Tom", None).unwrap();
        let a = s.create_doc("a", None, tom.id).unwrap();
        s.delete_doc(a.id).unwrap();
        let b = s.create_doc("b", None, tom.id).unwrap();
        s.restore_doc(a.id).unwrap();
        let ka = s.get_doc(a.id).unwrap().sort_key.unwrap();
        let kb = s.get_doc(b.id).unwrap().sort_key.unwrap();
        assert_ne!(ka, kb);
        assert!(kb > ka);
    }

    /// v5 backfill: NULL sort_keys are assigned per parent, in title order,
    /// after any key the parent's siblings already hold; a second run is a
    /// no-op and the version lands at SCHEMA_VERSION.
    #[test]
    fn backfill_v5_keys_unkeyed_docs_per_parent_in_title_order() {
        use crate::PrincipalKind;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s
            .create_principal(PrincipalKind::Human, "Tom", None)
            .unwrap();
        let folder = s.create_doc("folder", None, tom.id).unwrap();
        let keyed = s.create_doc("keyed", Some(folder.id), tom.id).unwrap();
        let keyed_key = keyed.sort_key.clone().unwrap();
        let b = s.create_doc("b", Some(folder.id), tom.id).unwrap();
        let a = s.create_doc("a", Some(folder.id), tom.id).unwrap();
        let root = s.create_doc("root-null", None, tom.id).unwrap();
        for id in [b.id, a.id, root.id] {
            s.conn
                .execute(
                    "UPDATE docs SET sort_key = NULL WHERE id = ?1",
                    params![id.to_string()],
                )
                .unwrap();
        }
        s.conn.pragma_update(None, "user_version", 4).unwrap();
        backfill(&s.conn).unwrap();

        let key = |id: Uuid| s.get_doc(id).unwrap().sort_key.unwrap();
        let (ka, kb) = (key(a.id), key(b.id));
        assert!(keyed_key < ka && ka < kb, "{keyed_key} < {ka} < {kb}");
        assert!(key(folder.id) < key(root.id), "root-level null keyed last");
        let nulls: i64 = s
            .conn
            .query_row("SELECT count(*) FROM docs WHERE sort_key IS NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(nulls, 0);
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        backfill(&s.conn).unwrap();
        assert_eq!(key(a.id), ka, "idempotent");
    }

    /// ops_for_doc_limited equals "ops_since(doc, 0), reversed, truncated"
    /// — the history handler's derivation — for a multi-epoch, multi-op
    /// ledger, and skips parked (unapplied) ops.
    #[test]
    fn ops_for_doc_limited_is_newest_first_and_capped() {
        use crate::{BlockType, OpInput, OpKind, PrincipalKind};
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s
            .create_principal(PrincipalKind::Human, "Tom", None)
            .unwrap();
        let doc = s.create_doc("d", None, tom.id).unwrap();
        let ins = |content: &str| OpInput {
            kind: OpKind::Insert {
                block_id: Uuid::now_v7(),
                parent_id: None,
                order_key: "".into(),
                block_type: BlockType::Paragraph,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        };
        // epoch 1: two ops in one batch; epochs 2..=4: one op each
        s.apply(doc.id, 0, tom.id, vec![ins("a"), ins("b")]).unwrap();
        for (i, c) in ["c", "d", "e"].iter().enumerate() {
            s.apply(doc.id, 1 + i as i64, tom.id, vec![ins(c)]).unwrap();
        }
        // a parked op never shows in history
        s.park(doc.id, tom.id, vec![ins("parked")], "later").unwrap();

        let mut expected = s.ops_since(doc.id, 0).unwrap();
        expected.reverse();
        assert_eq!(expected.len(), 5);
        let all = s.ops_for_doc_limited(doc.id, 100).unwrap();
        assert_eq!(all, expected, "same order as reverse(ops_since)");
        assert!(all.iter().all(|o| o.epoch_applied.is_some()));

        expected.truncate(3);
        let capped = s.ops_for_doc_limited(doc.id, 3).unwrap();
        assert_eq!(capped, expected);
        assert_eq!(capped[0].epoch_applied, Some(4));
        assert!(s.ops_for_doc_limited(doc.id, 0).unwrap().is_empty());
        assert!(s.ops_for_doc_limited(Uuid::now_v7(), 5).unwrap().is_empty());
    }

    /// Items 8/9: the connection runs WAL + synchronous NORMAL with a busy
    /// timeout, and the query-backed indexes exist.
    #[test]
    fn pragmas_and_indexes_are_in_place() {
        let s = SqliteStore::open_in_memory().unwrap();
        let sync: i64 = s
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "NORMAL");
        let busy: i64 = s
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(busy >= 5000);
        for idx in [
            "blocks_by_refers_to",
            "docs_by_parent",
            "ops_by_principal",
            "annotations_by_status",
            "annotations_by_op",
        ] {
            let n: i64 = s
                .conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "index {idx} missing");
        }
        // the plan for list_comments walks the refers_to index
        let plan: String = s
            .conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM blocks WHERE refers_to = 'x' AND deleted = 0",
                [],
                |r| r.get::<_, String>(3),
            )
            .unwrap();
        assert!(plan.contains("blocks_by_refers_to"), "{plan}");
    }

    /// block_vec from before the cascade: opening the DB rebuilds it with
    /// ON DELETE CASCADE, keeps the rows, and a hard block delete then takes
    /// the vector with it.
    #[test]
    fn opening_a_db_with_a_non_cascading_block_vec_rebuilds_it() {
        use crate::{BlockType, OpInput, OpKind, PrincipalKind};
        use rusqlite::params;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("D", None, tom.id).unwrap();
        let bid = Uuid::now_v7();
        s.apply(
            doc.id,
            0,
            tom.id,
            vec![OpInput {
                kind: OpKind::Insert {
                    block_id: bid,
                    parent_id: None,
                    order_key: "i".into(),
                    block_type: BlockType::Paragraph,
                    content: "x".into(),
                    refers_to: None,
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
        s.conn
            .execute_batch(
                "DROP TABLE block_vec;
                 CREATE TABLE block_vec (
                     block_id TEXT PRIMARY KEY REFERENCES blocks (id),
                     epoch INTEGER NOT NULL, dim INTEGER NOT NULL, vec BLOB NOT NULL);",
            )
            .unwrap();
        s.set_block_vec(bid, 1, &[1.0]).unwrap();
        // old table: a hard delete of the block is an FK error
        assert!(
            s.conn
                .execute("DELETE FROM blocks WHERE id = ?1", params![bid.to_string()])
                .is_err()
        );
        migrate_pre_schema(&s.conn).unwrap();
        migrate_pre_schema(&s.conn).unwrap(); // idempotent
        let sql: String = s
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'block_vec'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sql.contains("ON DELETE CASCADE"));
        assert_eq!(s.block_vecs().unwrap().len(), 1, "rows survive the rebuild");
        s.conn
            .execute("DELETE FROM blocks WHERE id = ?1", params![bid.to_string()])
            .unwrap();
        let n: i64 = s
            .conn
            .query_row("SELECT count(*) FROM block_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "cascade took the vector");
    }

    /// v4 backfill: a row whose stored type disagrees with its content is
    /// retyped on migration; comments are never touched.
    #[test]
    fn backfill_v4_retypes_mistyped_blocks_once() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let doc = s.create_doc("d", None, tom.id).unwrap();
        let (heading, para, mermaid) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let ins = |id, key: &str, bt, content: &str| OpInput {
            kind: OpKind::Insert {
                block_id: id,
                parent_id: None,
                order_key: key.into(),
                block_type: bt,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        };
        s.apply(
            doc.id,
            0,
            tom.id,
            vec![
                ins(heading, "i", BlockType::Heading, "## H"),
                ins(para, "j", BlockType::Paragraph, "p"),
                ins(
                    mermaid,
                    "k",
                    BlockType::DiagramMermaid,
                    "```mermaid\ng\n```",
                ),
            ],
        )
        .unwrap();
        let comment = s.add_comment(para, tom.id, "note", None).unwrap();

        // simulate pre-v4 damage: stale types left behind by old Replace
        for (id, wrong) in [
            (heading, "paragraph"),
            (mermaid, "paragraph"),
            (para, "heading"),
        ] {
            s.conn
                .execute(
                    "UPDATE blocks SET block_type = ?1 WHERE id = ?2",
                    params![wrong, id.to_string()],
                )
                .unwrap();
        }
        s.conn.pragma_update(None, "user_version", 3).unwrap();
        backfill(&s.conn).unwrap();

        assert_eq!(
            s.read_block(heading).unwrap().block_type,
            BlockType::Heading
        );
        assert_eq!(s.read_block(para).unwrap().block_type, BlockType::Paragraph);
        assert_eq!(
            s.read_block(mermaid).unwrap().block_type,
            BlockType::DiagramMermaid
        );
        assert_eq!(
            s.read_block(comment.id).unwrap().block_type,
            BlockType::Comment
        );
        let v: i64 = s
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // idempotent: a second open-time backfill is a no-op
        backfill(&s.conn).unwrap();
    }

    /// Trash: a delete tombstones the subtree under one stamp; the trash lists
    /// the root only; restore revives exactly that subtree — a child that was
    /// deleted separately, earlier, stays deleted.
    #[test]
    fn delete_lists_in_trash_and_restore_revives_only_what_fell_together() {
        use crate::PrincipalKind;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let root = s.create_doc("root", None, tom.id).unwrap();
        let kid = s.create_doc("kid", Some(root.id), tom.id).unwrap();
        let grandkid = s.create_doc("grandkid", Some(kid.id), tom.id).unwrap();
        let old = s.create_doc("old", Some(root.id), tom.id).unwrap();

        // `old` deleted on its own first
        assert_eq!(s.delete_doc(old.id).unwrap(), 1);
        // distinct stamps need distinct millis
        std::thread::sleep(std::time::Duration::from_millis(3));
        assert_eq!(s.delete_doc(root.id).unwrap(), 3);

        let trash = s.list_trash().unwrap();
        // one root row: `old` is under a trashed parent, so it is not a root
        // while `root` is in the trash; its own stamp keeps it out of root's
        // descendant count
        let titles: Vec<&str> = trash.iter().map(|t| t.doc.title.as_str()).collect();
        assert_eq!(titles, vec!["root"]);
        assert_eq!(trash[0].descendants, 2);

        // restore root: kid + grandkid come back, `old` stays in the trash
        assert_eq!(s.restore_doc(root.id).unwrap(), 3);
        let live: Vec<String> = s.list_docs().unwrap().into_iter().map(|d| d.title).collect();
        assert!(live.contains(&"root".to_string()));
        assert!(live.contains(&"kid".to_string()));
        assert!(live.contains(&"grandkid".to_string()));
        assert!(!live.contains(&"old".to_string()));
        assert!(s.doc_is_tombstoned(old.id).unwrap());
        assert!(!s.doc_is_tombstoned(grandkid.id).unwrap());
        let trash = s.list_trash().unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].doc.title, "old");

        // restoring a child whose parent is still trashed surfaces it at the root
        s.delete_doc(root.id).unwrap();
        assert_eq!(s.restore_doc(kid.id).unwrap(), 2);
        assert_eq!(s.get_doc(kid.id).unwrap().parent_id, None);
        assert!(s.doc_is_tombstoned(root.id).unwrap());

        // not in the trash → NotFound
        assert!(matches!(s.restore_doc(kid.id), Err(StoreError::NotFound(_))));
    }
}
