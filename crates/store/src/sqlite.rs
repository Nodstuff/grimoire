use crate::gate::{Scored, score_stale_op};
use crate::types::*;
use crate::{BlockStore, Result, StoreError};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;
use uuid::Uuid;

const SCHEMA: &str = include_str!("schema.sql");

pub struct SqliteStore {
    conn: Connection,
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
        backfill(&conn)?;
        Ok(Self { conn })
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
    // Fresh installs from before the maintainer tier created `shares` with
    // a CHECK that only allows review|yellow; SQLite can't widen a CHECK in
    // place, so rebuild the table once when we see the old constraint.
    // (DBs that got `trust` via ALTER have no CHECK and need nothing.)
    let shares_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'shares'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(sql) = shares_sql
        && sql.contains("trust IN ('review', 'yellow'))")
        && !sql.contains("'green'")
    {
        widen_shares_trust_check(conn)?;
    }
    // block_vec: the first cut referenced blocks(id) without ON DELETE
    // CASCADE, so a mirror pull (hard-delete + reinsert) failed the FK once
    // a mirror block had been embedded. Rebuild once with the cascade.
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
    }
    let has_invites: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'share_invites'",
        [],
        |r| r.get(0),
    )?;
    if has_invites > 0 {
        let has_offered: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('share_invites') WHERE name = 'offered_to'",
            [],
            |r| r.get(0),
        )?;
        if has_offered == 0 {
            conn.execute(
                "ALTER TABLE share_invites ADD COLUMN offered_to TEXT REFERENCES contacts (id)",
                [],
            )?;
        }
    }
    let has_shares: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'shares'",
        [],
        |r| r.get(0),
    )?;
    if has_shares > 0 {
        let has_trust: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('shares') WHERE name = 'trust'",
            [],
            |r| r.get(0),
        )?;
        if has_trust == 0 {
            conn.execute(
                "ALTER TABLE shares ADD COLUMN trust TEXT NOT NULL DEFAULT 'review'",
                [],
            )?;
        }
    }
    let has_mirrors: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'mirrors'",
        [],
        |r| r.get(0),
    )?;
    if has_mirrors > 0 {
        let has_perm: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('mirrors') WHERE name = 'permission'",
            [],
            |r| r.get(0),
        )?;
        if has_perm == 0 {
            conn.execute(
                "ALTER TABLE mirrors ADD COLUMN permission TEXT NOT NULL DEFAULT 'view'",
                [],
            )?;
        }
        let has_tended: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('mirrors') WHERE name = 'owner_tended'",
            [],
            |r| r.get(0),
        )?;
        if has_tended == 0 {
            conn.execute(
                "ALTER TABLE mirrors ADD COLUMN owner_tended INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        for (col, ddl) in [
            ("last_pulled_at", "ALTER TABLE mirrors ADD COLUMN last_pulled_at TEXT"),
            ("last_error", "ALTER TABLE mirrors ADD COLUMN last_error TEXT"),
            ("owner_epoch", "ALTER TABLE mirrors ADD COLUMN owner_epoch INTEGER NOT NULL DEFAULT 0"),
            // hub relay provenance (slice 1)
            ("origin_owner", "ALTER TABLE mirrors ADD COLUMN origin_owner TEXT"),
            ("origin_owner_name", "ALTER TABLE mirrors ADD COLUMN origin_owner_name TEXT"),
        ] {
            let has: i64 = conn.query_row(
                "SELECT count(*) FROM pragma_table_info('mirrors') WHERE name = ?1",
                params![col],
                |r| r.get(0),
            )?;
            if has == 0 {
                conn.execute(ddl, [])?;
            }
        }
    }
    // hub membership columns on contacts (slice 1)
    let has_contacts: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'contacts'",
        [],
        |r| r.get(0),
    )?;
    if has_contacts > 0 {
        for (col, ddl) in [
            ("role", "ALTER TABLE contacts ADD COLUMN role TEXT NOT NULL DEFAULT 'member'"),
            ("membership", "ALTER TABLE contacts ADD COLUMN membership TEXT NOT NULL DEFAULT 'active'"),
            ("is_hub", "ALTER TABLE contacts ADD COLUMN is_hub INTEGER NOT NULL DEFAULT 0"),
        ] {
            let has: i64 = conn.query_row(
                "SELECT count(*) FROM pragma_table_info('contacts') WHERE name = ?1",
                params![col],
                |r| r.get(0),
            )?;
            if has == 0 {
                conn.execute(ddl, [])?;
            }
        }
    }
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
    }
    Ok(())
}

/// The SQLite "rebuild a table to change a constraint" dance for `shares`:
/// copy into a table with the widened trust CHECK, swap, recreate the index.
/// Foreign keys are switched off for the swap (share_invites references
/// shares by id; the ids are preserved, so integrity holds afterwards).
fn widen_shares_trust_check(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", false)?;
    let result = (|| -> Result<()> {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE shares_new (
                 id         TEXT PRIMARY KEY,
                 root_doc   TEXT NOT NULL REFERENCES docs (id),
                 contact    TEXT REFERENCES contacts (id),
                 permission TEXT NOT NULL DEFAULT 'view' CHECK (permission IN ('view', 'propose')),
                 state      TEXT NOT NULL DEFAULT 'offered' CHECK (state IN ('offered', 'active', 'revoked')),
                 policy_override TEXT CHECK (policy_override IN ('human-review', 'agent-review', 'auto')),
                 trust      TEXT NOT NULL DEFAULT 'review' CHECK (trust IN ('review', 'yellow', 'green')),
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             );
             INSERT INTO shares_new (id, root_doc, contact, permission, state, policy_override, trust, created_at)
                 SELECT id, root_doc, contact, permission, state, policy_override, trust, created_at FROM shares;
             DROP TABLE shares;
             ALTER TABLE shares_new RENAME TO shares;
             CREATE INDEX IF NOT EXISTS shares_by_contact ON shares (contact);
             COMMIT;",
        )?;
        Ok(())
    })();
    conn.pragma_update(None, "foreign_keys", true)?;
    result?;
    // belt and braces: nothing dangling after the swap
    let violations: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_foreign_key_check('share_invites')",
        [],
        |r| r.get(0),
    )?;
    if violations > 0 {
        return Err(StoreError::InvalidOp(format!(
            "shares rebuild left {violations} dangling share_invites rows"
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

/// Populate FTS and edges for rows that predate their triggers/extraction.
/// Gated on user_version: count(*) on an external-content FTS table proxies
/// the content table, so emptiness is unobservable — version it instead.
const SCHEMA_VERSION: i64 = 5;

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
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
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

/// The highest sort_key among the live docs under `parent_id`, if any.
fn max_doc_sort_key(conn: &Connection, parent_id: Option<&str>) -> Result<Option<String>> {
    Ok(conn.query_row(
        "SELECT max(sort_key) FROM docs
         WHERE deleted = 0 AND sort_key IS NOT NULL
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

// --- federation row mapping (ADR 0002) ---

type RawContact = (String, String, String, String, bool, bool, String, String, String, bool);

const CONTACT_COLS: &str =
    "id, pubkey, petname, principal, verified, revoked, paired_at, role, membership, is_hub";

fn contact_row(row: &rusqlite::Row) -> rusqlite::Result<RawContact> {
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

fn finish_contact(raw: RawContact) -> Result<Contact> {
    let (id, pubkey, petname, principal, verified, revoked, paired_at, role, membership, is_hub) = raw;
    Ok(Contact {
        id: uuid_col(id, "contacts.id")?,
        pubkey,
        petname,
        principal: uuid_col(principal, "contacts.principal")?,
        verified,
        revoked,
        paired_at,
        role: ContactRole::parse(&role)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad contact role: {role}")))?,
        membership: Membership::parse(&membership)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad contact membership: {membership}")))?,
        is_hub,
    })
}

type RawShare = (
    String,
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    String,
);

fn share_row(row: &rusqlite::Row) -> rusqlite::Result<RawShare> {
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

type RawOffer = (String, String, String, String, String, String, String, String, String, String);

fn offer_row(row: &rusqlite::Row) -> rusqlite::Result<RawOffer> {
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

fn finish_offer(raw: RawOffer) -> Result<ShareOffer> {
    let (id, from_contact, owner_node, share_id, root_title, permission, secret, state, created_at, expires_at) = raw;
    Ok(ShareOffer {
        id: uuid_col(id, "share_offers.id")?,
        from_contact: uuid_col(from_contact, "share_offers.from_contact")?,
        owner_node,
        share_id: uuid_col(share_id, "share_offers.share_id")?,
        root_title,
        permission: SharePermission::parse(&permission)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad offer permission: {permission}")))?,
        secret,
        state: ShareOfferState::parse(&state)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad offer state: {state}")))?,
        created_at,
        expires_at,
    })
}

fn finish_share(raw: RawShare) -> Result<Share> {
    let (id, root_doc, contact, permission, state, policy_override, created_at, trust) = raw;
    Ok(Share {
        id: uuid_col(id, "shares.id")?,
        root_doc: uuid_col(root_doc, "shares.root_doc")?,
        contact: contact.map(|c| uuid_col(c, "shares.contact")).transpose()?,
        permission: SharePermission::parse(&permission)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad permission: {permission}")))?,
        state: ShareState::parse(&state)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad share state: {state}")))?,
        policy_override: policy_override
            .map(|p| {
                ReviewPolicy::parse(&p)
                    .ok_or_else(|| StoreError::InvalidOp(format!("bad policy_override: {p}")))
            })
            .transpose()?,
        created_at,
        trust: ShareTrust::parse(&trust)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad trust: {trust}")))?,
    })
}

type RawMirror = (
    String,
    String,
    String,
    i64,
    String,
    bool,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);

const MIRROR_COLS: &str = "doc_id, owner, share_id, synced_epoch, permission, owner_tended, last_pulled_at, last_error, owner_epoch, origin_owner, origin_owner_name";

type RawHubTransfer = (String, String, String, String, i64, String, String);

const HUB_TRANSFER_COLS: &str = "id, member_contact, root_doc, title, doc_count, state, at";

fn hub_transfer_row(row: &rusqlite::Row) -> rusqlite::Result<RawHubTransfer> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn finish_hub_transfer(raw: RawHubTransfer) -> Result<HubTransfer> {
    let (id, member_contact, root_doc, title, doc_count, state, at) = raw;
    Ok(HubTransfer {
        id: uuid_col(id, "hub_transfers.id")?,
        member_contact: uuid_col(member_contact, "hub_transfers.member_contact")?,
        root_doc: uuid_col(root_doc, "hub_transfers.root_doc")?,
        title,
        doc_count,
        state: HubTransferState::parse(&state)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad hub transfer state {state}")))?,
        at,
    })
}

fn mirror_row(row: &rusqlite::Row) -> rusqlite::Result<RawMirror> {
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
        row.get(10)?,
    ))
}

fn finish_mirror(raw: RawMirror) -> Result<Mirror> {
    let (
        doc_id,
        owner,
        share_id,
        synced_epoch,
        permission,
        owner_tended,
        last_pulled_at,
        last_error,
        owner_epoch,
        origin_owner,
        origin_owner_name,
    ) = raw;
    Ok(Mirror {
        doc_id: uuid_col(doc_id, "mirrors.doc_id")?,
        owner: uuid_col(owner, "mirrors.owner")?,
        share_id: uuid_col(share_id, "mirrors.share_id")?,
        synced_epoch,
        permission: SharePermission::parse(&permission)
            .ok_or_else(|| StoreError::InvalidOp(format!("bad mirror permission: {permission}")))?,
        owner_tended,
        last_pulled_at,
        last_error,
        owner_epoch,
        origin_owner,
        origin_owner_name,
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
) -> Result<()> {
    let sort_key = next_doc_sort_key(conn, parent)?;
    conn.execute(
        "INSERT INTO docs (id, parent_id, title, created_by, sort_key) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id.to_string(),
            parent.map(|p| p.to_string()),
            title,
            created_by.to_string(),
            sort_key
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

fn doc_epoch(tx: &Transaction, doc_id: Uuid) -> Result<i64> {
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
    }
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

    fn set_principal_pubkey(&mut self, id: Uuid, pubkey: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE principals SET pubkey = ?1 WHERE id = ?2",
            params![pubkey, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("principal {id}")));
        }
        Ok(())
    }

    fn rename_principal(&mut self, id: Uuid, display_name: &str) -> Result<()> {
        let name = display_name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(StoreError::InvalidOp("display name must be 1..64 characters".into()));
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
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn set_mirror_sync_result(&mut self, share_id: Uuid, error: Option<&str>) -> Result<()> {
        match error {
            None => self.conn.execute(
                "UPDATE mirrors SET last_pulled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), last_error = NULL
                 WHERE share_id = ?1",
                params![share_id.to_string()],
            )?,
            Some(e) => self.conn.execute(
                "UPDATE mirrors SET last_error = ?2 WHERE share_id = ?1",
                params![share_id.to_string(), e],
            )?,
        };
        Ok(())
    }

    fn delete_share(&mut self, id: Uuid) -> Result<()> {
        let share = self.get_share(id)?;
        if share.state != ShareState::Revoked {
            return Err(StoreError::InvalidOp(
                "only a revoked share can be cleared — revoke it first".into(),
            ));
        }
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM share_invites WHERE share_id = ?1", params![id.to_string()])?;
        tx.execute("DELETE FROM shares WHERE id = ?1", params![id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    fn list_principals(&self) -> Result<Vec<Principal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, display_name, pubkey FROM principals ORDER BY display_name",
        )?;
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
        insert_doc_row(&self.conn, id, title, parent, created_by)?;
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
        let tx = self.conn.transaction()?;
        insert_doc_row(&tx, id, title, parent, created_by)?;
        if n > 0 {
            apply_in_tx(&tx, id, 0, created_by, ops)?;
        }
        tx.commit()?;
        Ok((self.get_doc(id)?, n))
    }

    fn list_docs(&self) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, title, review_policy, current_epoch, created_by, status, sort_key
             FROM docs WHERE deleted = 0 ORDER BY sort_key IS NULL, sort_key, title",
        )?;
        let rows = stmt.query_map([], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    fn get_doc(&self, id: Uuid) -> Result<Doc> {
        let raw = self
            .conn
            .query_row(
                "SELECT id, parent_id, title, review_policy, current_epoch, created_by, status, sort_key
                 FROM docs WHERE id = ?1",
                params![id.to_string()],
                row_to_doc,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("doc {id}")))?;
        build_doc(raw)
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
        build_block(raw)
    }

    fn apply(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ApplyReceipt> {
        self.reject_if_mirror(doc_id)?;
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("apply: empty op list".into()));
        }
        let tx = self.conn.transaction()?;
        let receipt = apply_in_tx(&tx, doc_id, base_epoch, principal, ops)?;
        tx.commit()?;
        Ok(receipt)
    }

    fn ops_since(&self, doc_id: Uuid, since_epoch: i64) -> Result<Vec<LedgerOp>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {OP_COLS} FROM ops
             WHERE doc_id = ?1 AND epoch_applied IS NOT NULL AND epoch_applied > ?2
             ORDER BY epoch_applied, id"
        ))?;
        let rows = stmt.query_map(params![doc_id.to_string(), since_epoch], row_to_op)?;
        rows.map(|r| build_op(r?)).collect()
    }

    fn effective_policy(&self, doc_id: Uuid) -> Result<ReviewPolicy> {
        let mut cursor = doc_id;
        loop {
            let doc = self.get_doc(cursor)?;
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
        let mut cursor = new_parent;
        while let Some(p) = cursor {
            if p == doc_id {
                return Err(StoreError::InvalidOp(
                    "move: doc cannot nest under itself".into(),
                ));
            }
            cursor = self.get_doc(p)?.parent_id;
        }
        let n = self.conn.execute(
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
        Ok(())
    }

    fn delete_doc(&mut self, doc_id: Uuid) -> Result<usize> {
        let mut to_delete = vec![doc_id];
        let mut i = 0;
        while i < to_delete.len() {
            let kids: Vec<String> = {
                let mut stmt = self
                    .conn
                    .prepare("SELECT id FROM docs WHERE parent_id = ?1 AND deleted = 0")?;
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
        let stamp: String = self.conn.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            [],
            |r| r.get(0),
        )?;
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for d in &to_delete {
            n += tx.execute(
                "UPDATE docs SET deleted = 1, deleted_at = ?2 WHERE id = ?1 AND deleted = 0",
                params![d.to_string(), stamp],
            )?;
        }
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        tx.commit()?;
        Ok(n)
    }

    fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        // roots of tombstoned subtrees: deleted, and the parent is live or
        // absent. Docs a remote owner created are dropped mirrors of a revoked
        // share, not the user's own deletions — they revive via re-join.
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by,
                    d.status, d.sort_key, d.deleted_at,
                    (SELECT count(*) FROM docs c
                      WHERE c.deleted = 1 AND c.deleted_at = d.deleted_at AND c.id != d.id
                        AND c.id IN (WITH RECURSIVE sub(id) AS (
                              SELECT id FROM docs WHERE parent_id = d.id
                              UNION ALL
                              SELECT docs.id FROM docs JOIN sub ON docs.parent_id = sub.id)
                            SELECT id FROM sub)) AS descendants
             FROM docs d
             JOIN principals p ON p.id = d.created_by
             WHERE d.deleted = 1
               AND p.kind != 'remote'
               AND (d.parent_id IS NULL
                    OR NOT EXISTS (SELECT 1 FROM docs pd WHERE pd.id = d.parent_id AND pd.deleted = 1))
             ORDER BY d.deleted_at DESC, d.title",
        )?;
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
            Ok(TrashEntry {
                doc: build_doc(raw)?,
                deleted_at: deleted_at.unwrap_or_default(),
                descendants: descendants as usize,
            })
        })
        .collect()
    }

    fn restore_doc(&mut self, doc_id: Uuid) -> Result<usize> {
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
        tx.commit()?;
        Ok(n)
    }

    fn doc_subtree_ids(&self, doc_id: Uuid) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
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

    fn rename_doc(&mut self, doc_id: Uuid, title: &str) -> Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Err(StoreError::InvalidOp("rename: empty title".into()));
        }
        let n = self.conn.execute(
            "UPDATE docs SET title = ?1 WHERE id = ?2 AND deleted = 0",
            params![title, doc_id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        Ok(())
    }

    fn set_doc_status(&mut self, doc_id: Uuid, status: Option<DocStatus>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE docs SET status = ?1 WHERE id = ?2",
            params![status.map(|st| st.as_str()), doc_id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        Ok(())
    }

    fn set_review_policy(&mut self, doc_id: Uuid, policy: Option<ReviewPolicy>) -> Result<()> {
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
             WHERE b.deleted = 0 AND d.deleted = 0
               AND (e.to_target = ?1 OR e.to_target LIKE '%/' || ?1)
             ORDER BY d.title, b.order_key",
            b_cols()
        );
        let mut stmt = self.conn.prepare(&sql)?;
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
                 WHERE blocks_fts MATCH ?1 AND b.deleted = 0 AND d.deleted = 0
                 ORDER BY bm25(blocks_fts) LIMIT ?2",
                b_cols()
            );
            let mut stmt = self.conn.prepare(&sql)?;
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
             WHERE b.deleted = 0 AND d.deleted = 0 AND b.content LIKE ?1 ESCAPE '\\'
             ORDER BY d.title, b.order_key LIMIT ?2",
            b_cols()
        );
        let mut stmt = self.conn.prepare(&sql)?;
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
        self.propose_impl(doc_id, base_epoch, principal, ops, false)
    }

    fn propose_reviewed(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ProposeOutcome> {
        self.propose_impl(doc_id, base_epoch, principal, ops, true)
    }

    fn create_gardener(
        &mut self,
        name: &str,
        kind: GardenerKind,
        task_prompt: &str,
        scope_doc: Option<Uuid>,
        confidence_policy: ConfidencePolicy,
    ) -> Result<Gardener> {
        // each gardener is its own principal: provenance is per-gardener
        let principal = self.create_principal(PrincipalKind::Agent, name, None)?;
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO gardeners (id, name, kind, principal, scope_doc, task_prompt, confidence_policy)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.to_string(),
                name,
                kind.as_str(),
                principal.id.to_string(),
                scope_doc.map(|d| d.to_string()),
                task_prompt,
                confidence_policy.as_str(),
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
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, principal, scope_doc, task_prompt, bindings, creds_ref,
                    schedule, confidence_policy, enabled
             FROM gardeners ORDER BY name",
        )?;
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
            "UPDATE gardeners SET enabled = ?1 WHERE id = ?2",
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
        let n = self.conn.execute(
            "UPDATE gardeners SET task_prompt = ?1, schedule = ?2, confidence_policy = ?3,
                    scope_doc = ?4, enabled = ?5, bindings = ?6
             WHERE id = ?7",
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
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.gardener, g.name, r.started_at, r.status, r.summary, r.tokens_used, r.tool_calls
             FROM gardener_runs r JOIN gardeners g ON g.id = r.gardener
             ORDER BY r.started_at DESC LIMIT ?1",
        )?;
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
        let mut stmt = self.conn.prepare(
            "SELECT t.tag, count(DISTINCT t.doc_id) FROM doc_tags t
             JOIN docs d ON d.id = t.doc_id AND d.deleted = 0
             GROUP BY t.tag ORDER BY 2 DESC, t.tag",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    fn docs_by_tag(&self, tag: &str) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d JOIN doc_tags t ON t.doc_id = d.id
             WHERE t.tag = ?1 AND d.deleted = 0 GROUP BY d.id ORDER BY d.title",
        )?;
        let rows = stmt.query_map(params![tag.to_lowercase()], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    fn untagged_docs(&self, limit: usize) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d
             WHERE d.deleted = 0
               AND EXISTS (SELECT 1 FROM blocks b WHERE b.doc_id = d.id AND b.deleted = 0)
               AND NOT EXISTS (SELECT 1 FROM doc_tags t WHERE t.doc_id = d.id)
             ORDER BY d.title LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    fn park(
        &mut self,
        doc_id: Uuid,
        principal: Uuid,
        ops: Vec<OpInput>,
        note: &str,
    ) -> Result<Vec<Uuid>> {
        self.reject_if_mirror(doc_id)?;
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("park: empty op list".into()));
        }
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
        let sql = format!(
            "SELECT a.id, a.doc_id, a.op_id, a.kind, a.status, a.resolved_by,
                    {}
             FROM annotations a JOIN ops o ON o.id = a.op_id
             WHERE a.status = 'open' AND (?1 IS NULL OR a.doc_id = ?1)
             ORDER BY a.created_at, a.id",
            OP_COLS
                .split(", ")
                .map(|c| format!("o.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
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

    // --- federation (ADR 0002) ---

    fn pair_contact(&mut self, pubkey: &str, petname: &str) -> Result<Contact> {
        // Idempotent on pubkey. An existing contact is returned untouched:
        // the petname is the owner's chosen name (rename_contact is the way
        // to change it, never a peer's self-description), and revocation is
        // only lifted by the explicit unrevoke_contact — re-pairing must not
        // quietly restore trust.
        if let Some(existing) = self.contact_by_pubkey(pubkey)? {
            return Ok(existing);
        }
        let principal = self.create_principal(PrincipalKind::Remote, petname, Some(pubkey))?;
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO contacts (id, pubkey, petname, principal) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), pubkey, petname, principal.id.to_string()],
        )?;
        self.contact_by_pubkey(pubkey)?
            .ok_or_else(|| StoreError::NotFound(format!("contact {id}")))
    }

    fn list_contacts(&self) -> Result<Vec<Contact>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {CONTACT_COLS} FROM contacts ORDER BY paired_at"))?;
        let rows = stmt.query_map([], contact_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_contact)
            .collect()
    }

    fn contact_by_pubkey(&self, pubkey: &str) -> Result<Option<Contact>> {
        self.conn
            .query_row(
                &format!("SELECT {CONTACT_COLS} FROM contacts WHERE pubkey = ?1"),
                params![pubkey],
                contact_row,
            )
            .optional()?
            .map(finish_contact)
            .transpose()
    }

    fn set_contact_verified(&mut self, id: Uuid, verified: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE contacts SET verified = ?1 WHERE id = ?2",
            params![verified, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        Ok(())
    }

    fn revoke_contact(&mut self, id: Uuid) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE contacts SET revoked = 1 WHERE id = ?1",
            params![id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        self.conn.execute(
            "UPDATE shares SET state = 'revoked' WHERE contact = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    fn remove_contact(&mut self, id: Uuid) -> Result<()> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE shares SET state = 'revoked' WHERE contact = ?1 AND state != 'revoked'",
            params![id.to_string()],
        )?;
        let _ = n;
        // shares keep pointing at the contact row for history → null the FK
        // rather than cascade-deleting the audit trail
        tx.execute("UPDATE shares SET contact = NULL WHERE contact = ?1", params![id.to_string()])?;
        tx.execute(
            "UPDATE share_invites SET redeemed_by = NULL WHERE redeemed_by = ?1",
            params![id.to_string()],
        )?;
        // mirrors we hold FROM this contact reference the row: the user must
        // leave those shares first (that path drops the mirrors deliberately)
        let held: i64 = tx.query_row(
            "SELECT count(*) FROM mirrors WHERE owner = ?1",
            params![id.to_string()],
            |r| r.get(0),
        )?;
        if held > 0 {
            return Err(StoreError::InvalidOp(format!(
                "you still hold {held} doc{} shared by this contact — leave those shares first",
                if held == 1 { "" } else { "s" }
            )));
        }
        // proposals we sent THEM (grantee side) are history too
        tx.execute(
            "DELETE FROM outbound_proposals WHERE owner = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM share_offers WHERE from_contact = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "UPDATE share_invites SET offered_to = NULL WHERE offered_to = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM hub_publications WHERE member_contact = ?1",
            params![id.to_string()],
        )?;
        let deleted = tx.execute("DELETE FROM contacts WHERE id = ?1", params![id.to_string()])?;
        if deleted == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        tx.commit()?;
        Ok(())
    }

    fn unrevoke_contact(&mut self, id: Uuid) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE contacts SET revoked = 0 WHERE id = ?1",
            params![id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        Ok(())
    }

    fn create_share(
        &mut self,
        root_doc: Uuid,
        contact: Option<Uuid>,
        permission: SharePermission,
        policy_override: Option<ReviewPolicy>,
    ) -> Result<Share> {
        self.get_doc(root_doc)?; // must exist
        // re-share guard: a subtree containing mirrors is someone ELSE's
        // content — serving it onward would leak their docs to a third party
        // without their gate ever seeing it. Share your own docs only.
        // The one exception (hub, slice 1): a hub sharing its ROOT may contain
        // members' publications — mirrors the members asked it to relay.
        let relayed: std::collections::HashSet<Uuid> = {
            let hub_root: Option<Uuid> = match self.get_setting("hub.enabled")?.as_deref() {
                Some("1") => self.get_setting("hub.root_doc")?.and_then(|r| r.parse().ok()),
                _ => None,
            };
            if hub_root == Some(root_doc) {
                let pubs: std::collections::HashSet<Uuid> =
                    self.list_hub_publications()?.into_iter().map(|p| p.share_id).collect();
                self.list_mirrors()?
                    .into_iter()
                    .filter(|m| pubs.contains(&m.share_id))
                    .map(|m| m.doc_id)
                    .collect()
            } else {
                Default::default()
            }
        };
        let mirrors: std::collections::HashSet<Uuid> = self
            .list_mirrors()?
            .into_iter()
            .map(|m| m.doc_id)
            .filter(|id| !relayed.contains(id))
            .collect();
        if !mirrors.is_empty() {
            let mut stmt = self.conn.prepare(
                "WITH RECURSIVE subtree (id) AS (
                     SELECT id FROM docs WHERE id = ?1 AND deleted = 0
                     UNION ALL
                     SELECT d.id FROM docs d JOIN subtree s ON d.parent_id = s.id
                     WHERE d.deleted = 0
                 )
                 SELECT id FROM subtree",
            )?;
            let ids: Vec<String> = stmt
                .query_map(params![root_doc.to_string()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                let id = uuid_col(id, "docs.id")?;
                if mirrors.contains(&id) {
                    return Err(StoreError::InvalidOp(
                        "cannot share a subtree containing docs shared TO you —                          only the owner can share those"
                            .into(),
                    ));
                }
            }
        }
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO shares (id, root_doc, contact, permission, policy_override)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                root_doc.to_string(),
                contact.map(|c| c.to_string()),
                permission.as_str(),
                policy_override.map(|p| p.as_str()),
            ],
        )?;
        self.get_share(id)
    }

    fn list_shares(&self) -> Result<Vec<Share>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root_doc, contact, permission, state, policy_override, created_at, trust
             FROM shares ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], share_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_share)
            .collect()
    }

    fn get_share(&self, id: Uuid) -> Result<Share> {
        self.conn
            .query_row(
                "SELECT id, root_doc, contact, permission, state, policy_override, created_at, trust
                 FROM shares WHERE id = ?1",
                params![id.to_string()],
                share_row,
            )
            .optional()?
            .map(finish_share)
            .transpose()?
            .ok_or_else(|| StoreError::NotFound(format!("share {id}")))
    }

    fn set_share_state(&mut self, id: Uuid, state: ShareState) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE shares SET state = ?1 WHERE id = ?2",
            params![state.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("share {id}")));
        }
        Ok(())
    }

    fn set_share_permission(&mut self, id: Uuid, permission: SharePermission) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE shares SET permission = ?1 WHERE id = ?2",
            params![permission.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("share {id}")));
        }
        Ok(())
    }

    fn set_share_trust(&mut self, id: Uuid, trust: ShareTrust) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE shares SET trust = ?1 WHERE id = ?2",
            params![trust.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("share {id}")));
        }
        Ok(())
    }

    fn recent_remote_ops(&self, limit: usize) -> Result<Vec<ActivityItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT o.id, o.doc_id, d.title, o.principal, p.display_name, o.op_type,
                    o.epoch_applied, o.created_at
             FROM ops o
             JOIN docs d ON d.id = o.doc_id
             JOIN principals p ON p.id = o.principal
             WHERE p.kind = 'remote' AND o.epoch_applied IS NOT NULL
             ORDER BY o.created_at DESC, o.id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        rows.map(|r| {
            let (op_id, doc_id, doc_title, principal, principal_name, op_type, epoch, created_at) =
                r?;
            Ok(ActivityItem {
                op_id: uuid_col(op_id, "ops.id")?,
                doc_id: uuid_col(doc_id, "ops.doc_id")?,
                doc_title,
                principal: uuid_col(principal, "ops.principal")?,
                principal_name,
                op_type,
                epoch,
                created_at,
            })
        })
        .collect()
    }

    fn create_invite(
        &mut self,
        share_id: Uuid,
        secret_hash: &str,
        expires_at: &str,
    ) -> Result<Uuid> {
        self.get_share(share_id)?;
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO share_invites (id, share_id, secret_hash, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                share_id.to_string(),
                secret_hash,
                expires_at
            ],
        )?;
        Ok(id)
    }

    fn redeem_invite(
        &mut self,
        secret_hash: &str,
        pubkey: &str,
        petname: &str,
    ) -> Result<(Contact, Share)> {
        // ISO-8601 UTC strings compare lexicographically; 'now' matches the
        // strftime format used everywhere else in this schema.
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT id, share_id FROM share_invites
                 WHERE secret_hash = ?1
                   AND redeemed_at IS NULL
                   AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![secret_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((invite_id, share_id)) = row else {
            return Err(StoreError::InvalidOp(
                "invite invalid: unknown, already redeemed, or expired".into(),
            ));
        };
        let share_id = uuid_col(share_id, "share_invites.share_id")?;
        // a revoked peer cannot redeem their way back in: nothing is burned,
        // nothing is revived — the owner un-revokes first, deliberately
        if self.contact_by_pubkey(pubkey)?.is_some_and(|c| c.revoked) {
            return Err(StoreError::InvalidOp(
                "contact is revoked; un-revoke before re-inviting".into(),
            ));
        }
        // The petname is PEER-SUPPLIED. A first-seen contact carries a short
        // fingerprint suffix ("alice · 3f9a") so two peers claiming the same
        // name are distinguishable until the owner renames or verifies them.
        // An existing contact keeps the owner's chosen name (pair_contact).
        let suffix: String = pubkey.chars().take(4).collect();
        let shown = format!("{} · {suffix}", petname.trim());
        // pair + burn + bind + supersede land together: a failure half-way
        // must not leave a contact without a share, or a burned invite that
        // never bound. The helpers below run on self, so the transaction is
        // driven by hand rather than through a Transaction borrow.
        self.conn.execute_batch("BEGIN")?;
        let result = (|| -> Result<(Contact, Share)> {
            let contact = self.pair_contact(pubkey, &shown)?;
            self.conn.execute(
                "UPDATE share_invites SET redeemed_by = ?1,
                     redeemed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?2",
                params![contact.id.to_string(), invite_id],
            )?;
            self.conn.execute(
                "UPDATE shares SET contact = ?1, state = 'active' WHERE id = ?2",
                params![contact.id.to_string(), share_id.to_string()],
            )?;
            // supersede: a re-invite of the same subtree to the same person
            // replaces the old grant — one active share per (root_doc, contact),
            // so grantee mirror rows never tug-of-war over permission
            let share = self.get_share(share_id)?;
            self.conn.execute(
                "UPDATE shares SET state = 'revoked'
                 WHERE root_doc = ?1 AND contact = ?2 AND id != ?3 AND state = 'active'",
                params![
                    share.root_doc.to_string(),
                    contact.id.to_string(),
                    share_id.to_string()
                ],
            )?;
            Ok((contact, share))
        })();
        match result {
            Ok(out) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(out)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn docs_in_share(&self, share_id: Uuid) -> Result<Vec<Doc>> {
        let share = self.get_share(share_id)?;
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE subtree (id) AS (
                 SELECT id FROM docs WHERE id = ?1 AND deleted = 0
                 UNION ALL
                 SELECT d.id FROM docs d JOIN subtree s ON d.parent_id = s.id
                 WHERE d.deleted = 0
             )
             SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch,
                    d.created_by, d.status, d.sort_key
             FROM docs d JOIN subtree s ON d.id = s.id
             ORDER BY d.sort_key IS NULL, d.sort_key, d.title",
        )?;
        let rows = stmt.query_map(params![share.root_doc.to_string()], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    fn shares_containing(&self, doc_id: Uuid) -> Result<Vec<Share>> {
        // walk up from the doc; any non-revoked share rooted at an ancestor
        // (or the doc itself) contains it
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE ancestors (id) AS (
                 SELECT id FROM docs WHERE id = ?1
                 UNION ALL
                 SELECT d.parent_id FROM docs d JOIN ancestors a ON d.id = a.id
                 WHERE d.parent_id IS NOT NULL
             )
             SELECT s.id, s.root_doc, s.contact, s.permission, s.state,
                    s.policy_override, s.created_at, s.trust
             FROM shares s JOIN ancestors a ON s.root_doc = a.id
             WHERE s.state != 'revoked'
             ORDER BY s.created_at",
        )?;
        let rows = stmt.query_map(params![doc_id.to_string()], share_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_share)
            .collect()
    }

    fn create_doc_with_id(
        &mut self,
        id: Uuid,
        title: &str,
        parent: Option<Uuid>,
        created_by: Uuid,
    ) -> Result<Doc> {
        insert_doc_row(&self.conn, id, title, parent, created_by)?;
        self.get_doc(id)
    }

    fn record_outbound_proposal(
        &mut self,
        doc_id: Uuid,
        share_id: Uuid,
        owner: Uuid,
        op_ids: &[Uuid],
        note: &str,
    ) -> Result<Uuid> {
        let id = Uuid::now_v7();
        let ids_json =
            serde_json::to_string(&op_ids.iter().map(|u| u.to_string()).collect::<Vec<_>>())?;
        self.conn.execute(
            "INSERT INTO outbound_proposals (id, doc_id, share_id, owner, op_ids, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.to_string(),
                doc_id.to_string(),
                share_id.to_string(),
                owner.to_string(),
                ids_json,
                note
            ],
        )?;
        Ok(id)
    }

    fn list_outbound_proposals(&self, pending_only: bool) -> Result<Vec<OutboundProposal>> {
        let sql = if pending_only {
            "SELECT id, doc_id, share_id, owner, op_ids, note, state, created_at
             FROM outbound_proposals WHERE state = 'pending' ORDER BY created_at"
        } else {
            "SELECT id, doc_id, share_id, owner, op_ids, note, state, created_at
             FROM outbound_proposals ORDER BY created_at"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(
                |(id, doc_id, share_id, owner, op_ids, note, state, created_at)| {
                    let raw_ids: Vec<String> = serde_json::from_str(&op_ids)?;
                    Ok(OutboundProposal {
                        id: uuid_col(id, "outbound_proposals.id")?,
                        doc_id: uuid_col(doc_id, "outbound_proposals.doc_id")?,
                        share_id: uuid_col(share_id, "outbound_proposals.share_id")?,
                        owner: uuid_col(owner, "outbound_proposals.owner")?,
                        op_ids: raw_ids
                            .into_iter()
                            .map(|s| uuid_col(s, "outbound_proposals.op_ids"))
                            .collect::<Result<_>>()?,
                        note,
                        state,
                        created_at,
                    })
                },
            )
            .collect()
    }

    fn set_outbound_state(&mut self, id: Uuid, state: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE outbound_proposals SET state = ?1 WHERE id = ?2",
            params![state, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("outbound proposal {id}")));
        }
        Ok(())
    }

    fn op_statuses(&self, ids: &[Uuid]) -> Result<Vec<OpStatus>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let row = self
                .conn
                .query_row(
                    "SELECT o.principal, o.epoch_applied,
                            (SELECT a.status FROM annotations a
                             WHERE a.op_id = o.id ORDER BY a.created_at DESC LIMIT 1),
                            o.source_refs
                     FROM ops o WHERE o.id = ?1",
                    params![id.to_string()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((principal, epoch_applied, review, refs)) = row {
                out.push(OpStatus {
                    op_id: *id,
                    principal: uuid_col(principal, "ops.principal")?,
                    applied: epoch_applied.is_some(),
                    review,
                    source_refs: serde_json::from_str(&refs).unwrap_or_default(),
                });
            }
        }
        Ok(out)
    }

    // --- hub slice 2: forwarding + transfers ---

    fn remote_principal_for(&mut self, pubkey: &str, name: &str) -> Result<Uuid> {
        if let Some(c) = self.contact_by_pubkey(pubkey)? {
            return Ok(c.principal);
        }
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM principals WHERE kind = 'remote' AND pubkey = ?1 ORDER BY id LIMIT 1",
                params![pubkey],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return uuid_col(id, "principals.id");
        }
        let name = name.trim();
        let name = if name.is_empty() { "someone" } else { name };
        Ok(self
            .create_principal(PrincipalKind::Remote, name, Some(pubkey))?
            .id)
    }

    fn add_hub_forward(
        &mut self,
        op_id: Uuid,
        owner_contact: Uuid,
        member_contact: Uuid,
        owner_share: Uuid,
        doc_id: Uuid,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO hub_forwards (op_id, owner_contact, member_contact, owner_share, doc_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                op_id.to_string(),
                owner_contact.to_string(),
                member_contact.to_string(),
                owner_share.to_string(),
                doc_id.to_string()
            ],
        )?;
        Ok(())
    }

    fn hub_forwards_for(&self, op_ids: &[Uuid]) -> Result<Vec<HubForward>> {
        let mut out = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT op_id, owner_contact, member_contact, owner_share, doc_id
             FROM hub_forwards WHERE op_id = ?1",
        )?;
        for id in op_ids {
            let row: Option<(String, String, String, String, String)> = stmt
                .query_row(params![id.to_string()], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?;
            if let Some((op_id, owner_contact, member_contact, owner_share, doc_id)) = row {
                out.push(HubForward {
                    op_id: uuid_col(op_id, "hub_forwards.op_id")?,
                    owner_contact: uuid_col(owner_contact, "hub_forwards.owner_contact")?,
                    member_contact: uuid_col(member_contact, "hub_forwards.member_contact")?,
                    owner_share: uuid_col(owner_share, "hub_forwards.owner_share")?,
                    doc_id: uuid_col(doc_id, "hub_forwards.doc_id")?,
                });
            }
        }
        Ok(out)
    }

    fn add_hub_transfer(
        &mut self,
        member_contact: Uuid,
        root_doc: Uuid,
        title: &str,
        doc_count: i64,
    ) -> Result<HubTransfer> {
        // one open offer per (member, root): a re-offer replaces it
        self.conn.execute(
            "DELETE FROM hub_transfers WHERE member_contact = ?1 AND root_doc = ?2 AND state = 'offered'",
            params![member_contact.to_string(), root_doc.to_string()],
        )?;
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO hub_transfers (id, member_contact, root_doc, title, doc_count)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                member_contact.to_string(),
                root_doc.to_string(),
                title,
                doc_count
            ],
        )?;
        self.get_hub_transfer(id)
    }

    fn list_hub_transfers(&self) -> Result<Vec<HubTransfer>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {HUB_TRANSFER_COLS} FROM hub_transfers ORDER BY at"))?;
        let rows = stmt.query_map([], hub_transfer_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_hub_transfer)
            .collect()
    }

    fn get_hub_transfer(&self, id: Uuid) -> Result<HubTransfer> {
        self.conn
            .query_row(
                &format!("SELECT {HUB_TRANSFER_COLS} FROM hub_transfers WHERE id = ?1"),
                params![id.to_string()],
                hub_transfer_row,
            )
            .optional()?
            .map(finish_hub_transfer)
            .transpose()?
            .ok_or_else(|| StoreError::NotFound(format!("transfer {id}")))
    }

    fn set_hub_transfer_state(&mut self, id: Uuid, state: HubTransferState) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE hub_transfers SET state = ?1 WHERE id = ?2",
            params![state.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("transfer {id}")));
        }
        Ok(())
    }

    fn add_doc_transfer(
        &mut self,
        root_doc: Uuid,
        counterparty: Uuid,
        direction: TransferDirection,
        state: &str,
    ) -> Result<DocTransfer> {
        if !matches!(state, "offered" | "done") {
            return Err(StoreError::InvalidOp(format!("bad transfer state {state}")));
        }
        // one live record per (root, direction): a re-offer replaces an open one
        self.conn.execute(
            "DELETE FROM doc_transfers WHERE root_doc = ?1 AND direction = ?2 AND state = 'offered'",
            params![root_doc.to_string(), direction.as_str()],
        )?;
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO doc_transfers (id, root_doc, counterparty, direction, state)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                root_doc.to_string(),
                counterparty.to_string(),
                direction.as_str(),
                state
            ],
        )?;
        Ok(self
            .list_doc_transfers()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("transfer {id}")))?)
    }

    fn list_doc_transfers(&self) -> Result<Vec<DocTransfer>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root_doc, counterparty, direction, state, at FROM doc_transfers ORDER BY at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        rows.map(|r| {
            let (id, root_doc, counterparty, direction, state, at) = r?;
            Ok(DocTransfer {
                id: uuid_col(id, "doc_transfers.id")?,
                root_doc: uuid_col(root_doc, "doc_transfers.root_doc")?,
                counterparty: uuid_col(counterparty, "doc_transfers.counterparty")?,
                direction: TransferDirection::parse(&direction).ok_or_else(|| {
                    StoreError::InvalidOp(format!("bad transfer direction {direction}"))
                })?,
                state,
                at,
            })
        })
        .collect()
    }

    fn set_doc_transfer_state(&mut self, id: Uuid, state: &str) -> Result<()> {
        if !matches!(state, "offered" | "done") {
            return Err(StoreError::InvalidOp(format!("bad transfer state {state}")));
        }
        let n = self.conn.execute(
            "UPDATE doc_transfers SET state = ?1 WHERE id = ?2",
            params![state, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("transfer {id}")));
        }
        Ok(())
    }

    fn set_invite_offered_to(&mut self, share_id: Uuid, contact: Uuid) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE share_invites SET offered_to = ?1
             WHERE share_id = ?2 AND redeemed_at IS NULL",
            params![contact.to_string(), share_id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("open invite for share {share_id}")));
        }
        Ok(())
    }

    fn invite_offered_to(&self, share_id: Uuid) -> Result<Option<Uuid>> {
        let c: Option<String> = self
            .conn
            .query_row(
                "SELECT offered_to FROM share_invites
                 WHERE share_id = ?1 AND redeemed_at IS NULL AND offered_to IS NOT NULL
                 ORDER BY created_at DESC LIMIT 1",
                params![share_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        c.map(|c| uuid_col(c, "share_invites.offered_to")).transpose()
    }

    fn add_share_offer(
        &mut self,
        from_contact: Uuid,
        owner_node: &str,
        share_id: Uuid,
        root_title: &str,
        permission: SharePermission,
        secret: &str,
        expires_at: &str,
    ) -> Result<ShareOffer> {
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO share_offers (id, from_contact, owner_node, share_id, root_title, permission, secret, state, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8)
             ON CONFLICT(owner_node, share_id) DO UPDATE SET
                 id = excluded.id, from_contact = excluded.from_contact, root_title = excluded.root_title,
                 permission = excluded.permission, secret = excluded.secret, state = 'open',
                 expires_at = excluded.expires_at, created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![
                id.to_string(),
                from_contact.to_string(),
                owner_node,
                share_id.to_string(),
                root_title,
                permission.as_str(),
                secret,
                expires_at
            ],
        )?;
        self.get_share_offer(id)
    }

    fn list_share_offers(&self, open_only: bool) -> Result<Vec<ShareOffer>> {
        let sql = if open_only {
            "SELECT id, from_contact, owner_node, share_id, root_title, permission, secret, state, created_at, expires_at
             FROM share_offers WHERE state = 'open' ORDER BY created_at DESC"
        } else {
            "SELECT id, from_contact, owner_node, share_id, root_title, permission, secret, state, created_at, expires_at
             FROM share_offers ORDER BY created_at DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], offer_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_offer)
            .collect()
    }

    fn get_share_offer(&self, id: Uuid) -> Result<ShareOffer> {
        self.conn
            .query_row(
                "SELECT id, from_contact, owner_node, share_id, root_title, permission, secret, state, created_at, expires_at
                 FROM share_offers WHERE id = ?1",
                params![id.to_string()],
                offer_row,
            )
            .optional()?
            .map(finish_offer)
            .transpose()?
            .ok_or_else(|| StoreError::NotFound(format!("share offer {id}")))
    }

    fn set_share_offer_state(&mut self, id: Uuid, state: ShareOfferState) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE share_offers SET state = ?1 WHERE id = ?2",
            params![state.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("share offer {id}")));
        }
        Ok(())
    }

    fn expire_share_offers(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE share_offers SET state = 'expired'
             WHERE state = 'open' AND expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            [],
        )?)
    }

    fn clear_share_offers(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM share_offers WHERE state IN ('declined', 'expired')",
            [],
        )?)
    }

    fn queue_join(&mut self, ticket: &str) -> Result<Uuid> {
        if let Some(existing) = self
            .conn
            .query_row(
                "SELECT id FROM pending_joins WHERE ticket = ?1",
                params![ticket],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return uuid_col(existing, "pending_joins.id");
        }
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO pending_joins (id, ticket) VALUES (?1, ?2)",
            params![id.to_string(), ticket],
        )?;
        Ok(id)
    }

    fn list_pending_joins(&self) -> Result<Vec<PendingJoin>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ticket, attempts, last_error, created_at
             FROM pending_joins ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(id, ticket, attempts, last_error, created_at)| {
                Ok(PendingJoin {
                    id: uuid_col(id, "pending_joins.id")?,
                    ticket,
                    attempts,
                    last_error,
                    created_at,
                })
            })
            .collect()
    }

    fn record_join_attempt(&mut self, id: Uuid, error: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE pending_joins SET attempts = attempts + 1, last_error = ?1 WHERE id = ?2",
            params![error, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("pending join {id}")));
        }
        Ok(())
    }

    fn remove_pending_join(&mut self, id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM pending_joins WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    fn upsert_mirror(
        &mut self,
        doc_id: Uuid,
        owner: Uuid,
        share_id: Uuid,
        synced_epoch: i64,
        permission: SharePermission,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mirrors (doc_id, owner, share_id, synced_epoch, permission)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (doc_id) DO UPDATE SET
                 owner = excluded.owner,
                 share_id = excluded.share_id,
                 synced_epoch = excluded.synced_epoch,
                 permission = excluded.permission",
            params![
                doc_id.to_string(),
                owner.to_string(),
                share_id.to_string(),
                synced_epoch,
                permission.as_str()
            ],
        )?;
        Ok(())
    }

    fn rename_contact(&mut self, id: Uuid, petname: &str) -> Result<()> {
        let petname = petname.trim();
        if petname.is_empty() || petname.chars().count() > 64 {
            return Err(StoreError::InvalidOp("petname must be 1..64 characters".into()));
        }
        let n = self.conn.execute(
            "UPDATE contacts SET petname = ?1 WHERE id = ?2",
            params![petname, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        // the contact's Remote principal is how they appear in provenance and
        // the review queue; it follows the owner's chosen name
        self.conn.execute(
            "UPDATE principals SET display_name = ?1
             WHERE id = (SELECT principal FROM contacts WHERE id = ?2)",
            params![petname, id.to_string()],
        )?;
        Ok(())
    }

    fn stale_block_vectors(&self, limit: usize) -> Result<Vec<(Uuid, i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT b.id, b.epoch, b.content FROM blocks b
             JOIN docs d ON d.id = b.doc_id
             LEFT JOIN block_vec v ON v.block_id = b.id
             WHERE b.deleted = 0 AND b.block_type != 'comment'
               AND d.deleted = 0
               AND NOT EXISTS (SELECT 1 FROM mirrors m WHERE m.doc_id = b.doc_id)
               AND (v.block_id IS NULL OR v.epoch < b.epoch)
             ORDER BY b.epoch DESC LIMIT ?1",
        )?;
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
        let mut stmt = self.conn.prepare(
            "SELECT v.block_id, v.vec FROM block_vec v
             JOIN blocks b ON b.id = v.block_id
             JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0 AND v.dim > 0",
        )?;
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

    fn change_signature(&self) -> Result<ChangeSignature> {
        // one cheap aggregate over docs + shares: enough to say "nothing an
        // owner-side nudge could care about moved since last tick"
        let (max_epoch, doc_count): (i64, i64) = self.conn.query_row(
            "SELECT coalesce(max(current_epoch), 0), count(*) FROM docs WHERE deleted = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let active_shares: i64 = self.conn.query_row(
            "SELECT count(*) FROM shares WHERE state = 'active'",
            [],
            |r| r.get(0),
        )?;
        Ok(ChangeSignature {
            max_epoch,
            doc_count,
            active_shares,
        })
    }

    fn get_mirror(&self, doc_id: Uuid) -> Result<Option<Mirror>> {
        self.conn
            .query_row(
                &format!("SELECT {MIRROR_COLS} FROM mirrors WHERE doc_id = ?1"),
                params![doc_id.to_string()],
                mirror_row,
            )
            .optional()?
            .map(finish_mirror)
            .transpose()
    }

    fn doc_is_tombstoned(&self, id: Uuid) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT deleted FROM docs WHERE id = ?1",
                params![id.to_string()],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("doc {id}")))
    }

    fn undelete_doc(&mut self, id: Uuid) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE docs SET deleted = 0 WHERE id = ?1",
            params![id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {id}")));
        }
        Ok(())
    }

    fn set_mirror_owner_epoch(&mut self, doc_id: Uuid, owner_epoch: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE mirrors SET owner_epoch = ?1 WHERE doc_id = ?2",
            params![owner_epoch, doc_id.to_string()],
        )?;
        Ok(())
    }

    fn set_mirror_tended(&mut self, doc_id: Uuid, tended: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE mirrors SET owner_tended = ?1 WHERE doc_id = ?2",
            params![tended, doc_id.to_string()],
        )?;
        Ok(())
    }

    fn set_mirror_origin(
        &mut self,
        doc_id: Uuid,
        origin_owner: Option<&str>,
        origin_owner_name: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE mirrors SET origin_owner = ?1, origin_owner_name = ?2 WHERE doc_id = ?3",
            params![origin_owner, origin_owner_name, doc_id.to_string()],
        )?;
        Ok(())
    }

    // --- hub membership + publications (slice 1) ---

    fn set_contact_role(&mut self, id: Uuid, role: ContactRole) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE contacts SET role = ?1 WHERE id = ?2",
            params![role.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        Ok(())
    }

    fn set_contact_membership(&mut self, id: Uuid, membership: Membership) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE contacts SET membership = ?1 WHERE id = ?2",
            params![membership.as_str(), id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        Ok(())
    }

    fn set_contact_is_hub(&mut self, id: Uuid, is_hub: bool) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE contacts SET is_hub = ?1 WHERE id = ?2",
            params![is_hub, id.to_string()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("contact {id}")));
        }
        Ok(())
    }

    fn add_hub_publication(&mut self, share_id: Uuid, member_contact: Uuid, root_doc: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT INTO hub_publications (share_id, member_contact, root_doc) VALUES (?1, ?2, ?3)
             ON CONFLICT (share_id) DO UPDATE SET
                 member_contact = excluded.member_contact,
                 root_doc = excluded.root_doc",
            params![share_id.to_string(), member_contact.to_string(), root_doc.to_string()],
        )?;
        Ok(())
    }

    fn list_hub_publications(&self) -> Result<Vec<HubPublication>> {
        let mut stmt = self.conn.prepare(
            "SELECT share_id, member_contact, root_doc, published_at
             FROM hub_publications ORDER BY published_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|r| {
            let (share_id, member_contact, root_doc, published_at) = r?;
            Ok(HubPublication {
                share_id: uuid_col(share_id, "hub_publications.share_id")?,
                member_contact: uuid_col(member_contact, "hub_publications.member_contact")?,
                root_doc: uuid_col(root_doc, "hub_publications.root_doc")?,
                published_at,
            })
        })
        .collect()
    }

    fn remove_hub_publication(&mut self, share_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM hub_publications WHERE share_id = ?1",
            params![share_id.to_string()],
        )?;
        Ok(())
    }

    /// True if a gardener tends this doc or any ancestor (recursive
    /// containment, the same rule the tend panel uses). Disabled gardeners
    /// don't count.
    fn doc_is_tended(&self, doc_id: Uuid) -> Result<bool> {
        let scopes: std::collections::HashSet<Uuid> = self
            .list_gardeners()?
            .into_iter()
            .filter(|g| g.enabled)
            .filter_map(|g| g.scope_doc)
            .collect();
        if scopes.is_empty() {
            return Ok(false);
        }
        let mut cur = Some(doc_id);
        while let Some(id) = cur {
            if scopes.contains(&id) {
                return Ok(true);
            }
            cur = self.get_doc(id).ok().and_then(|d| d.parent_id);
        }
        Ok(false)
    }

    fn list_mirrors(&self) -> Result<Vec<Mirror>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {MIRROR_COLS} FROM mirrors ORDER BY doc_id"))?;
        let rows = stmt.query_map([], mirror_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_mirror)
            .collect()
    }

    fn remove_mirror(&mut self, doc_id: Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM mirrors WHERE doc_id = ?1",
            params![doc_id.to_string()],
        )?;
        Ok(())
    }

    fn doc_blocks_flat(&self, doc_id: Uuid) -> Result<Vec<Block>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {BLOCK_COLS} FROM blocks
             WHERE doc_id = ?1 AND deleted = 0 ORDER BY order_key"
        ))?;
        let rows = stmt.query_map(params![doc_id.to_string()], row_to_block)?;
        rows.map(|r| build_block(r?)).collect()
    }

    fn mirror_replace_blocks(
        &mut self,
        doc_id: Uuid,
        blocks: Vec<MirrorBlock>,
        owner_epoch: i64,
        principal: Uuid,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        // clear the old projection outright: a mirror is a replica, not a
        // ledger — tombstones and ops describe the OWNER's history, which
        // lives on the owner's instance
        {
            let mut stmt = tx.prepare("SELECT id FROM blocks WHERE doc_id = ?1")?;
            let old_ids: Vec<String> = stmt
                .query_map(params![doc_id.to_string()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in old_ids {
                tx.execute("DELETE FROM edges WHERE from_block = ?1", params![id])?;
                tx.execute("DELETE FROM doc_tags WHERE block_id = ?1", params![id])?;
            }
        }
        // embeddings go with their blocks (belt: the FK also cascades since
        // the block_vec rebuild, but a pre-rebuild table must not fail here)
        tx.execute(
            "DELETE FROM block_vec WHERE block_id IN (SELECT id FROM blocks WHERE doc_id = ?1)",
            params![doc_id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM blocks WHERE doc_id = ?1",
            params![doc_id.to_string()],
        )?;
        // blocks.parent_id REFERENCES blocks(id): parents must exist before
        // their children. The wire order is order_key (per-sibling), which
        // says nothing about depth — a paragraph under a heading can arrive
        // before the heading and fail the FK, leaving a doc with a title and
        // no content. Insert in topological order; defer FK checks to commit
        // so a genuinely dangling parent still fails loudly rather than by
        // accident of ordering.
        tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        let ids: std::collections::HashSet<Uuid> = blocks.iter().map(|b| b.id).collect();
        let mut placed: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let mut ordered: Vec<&MirrorBlock> = Vec::with_capacity(blocks.len());
        let mut remaining: Vec<&MirrorBlock> = blocks.iter().collect();
        while !remaining.is_empty() {
            let before = remaining.len();
            remaining.retain(|b| {
                let ready = match b.parent_id {
                    None => true,
                    // parent outside this doc's block set: treat as root-ready
                    Some(p) => placed.contains(&p) || !ids.contains(&p),
                };
                if ready {
                    placed.insert(b.id);
                    ordered.push(b);
                }
                !ready
            });
            if remaining.len() == before {
                // cycle in parent links: never valid; insert what's left as-is
                // and let the deferred FK check report it
                ordered.extend(remaining.drain(..));
            }
        }
        for b in ordered {
            tx.execute(
                "INSERT INTO blocks (id, doc_id, parent_id, order_key, block_type,
                                     content, created_by, epoch, refers_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    b.id.to_string(),
                    doc_id.to_string(),
                    b.parent_id.map(|p| p.to_string()),
                    b.order_key,
                    b.block_type.as_str(),
                    b.content,
                    principal.to_string(),
                    owner_epoch,
                    b.refers_to.map(|r| r.to_string()),
                ],
            )?;
            set_edges(&tx, b.id, &b.content)?;
            set_tags(&tx, doc_id, b.id, &b.content)?;
        }
        tx.execute(
            "UPDATE docs SET current_epoch = ?1 WHERE id = ?2",
            params![owner_epoch, doc_id.to_string()],
        )?;
        tx.execute(
            "UPDATE mirrors SET synced_epoch = ?1 WHERE doc_id = ?2",
            params![owner_epoch, doc_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
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
        // gate — for AGENT and REMOTE principals, whose autonomy it bounds.
        // The instance's human owner is exempt: their own stale edit that the
        // gate parked red (an autosave that raced a live session, a second
        // window) is theirs to accept or drop, and there is no one else to
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
        match (kind, decision) {
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
        tx.commit()?;
        Ok(receipt)
    }
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
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT b.id, b.doc_id, b.content
             FROM edges e JOIN blocks b ON b.id = e.from_block
             JOIN docs d ON d.id = b.doc_id
             WHERE b.deleted = 0 AND d.deleted = 0
               AND (e.to_target = ?1 OR e.to_target LIKE '%/' || ?1)",
        )?;
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
             WHERE o.principal = ?1
             ORDER BY o.id DESC LIMIT ?2",
            OP_COLS
                .split(", ")
                .map(|c| format!("o.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
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

    /// All live docs in a subtree, the scope root included — the opt-in
    /// boundary every scoped gardener works within.
    pub fn doc_subtree(&self, root: Uuid) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE sub(id) AS (
                 SELECT ?1
                 UNION
                 SELECT d.id FROM docs d JOIN sub ON d.parent_id = sub.id WHERE d.deleted = 0
             )
             SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d JOIN sub ON sub.id = d.id
             WHERE d.deleted = 0
             ORDER BY d.sort_key IS NULL, d.sort_key, d.title",
        )?;
        let rows = stmt.query_map(params![root.to_string()], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    /// Stalest docs first (oldest last-op) within a scope, excluding docs
    /// this gardener already covered — the scoped sweep worklist.
    pub fn audit_candidates(&self, auditor: Uuid, scope: Uuid, limit: usize) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
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
             ORDER BY o.last ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![auditor.to_string(), limit as i64, scope.to_string()],
            row_to_doc,
        )?;
        rows.map(|r| build_doc(r?)).collect()
    }

    /// Mark docs as covered by an auditor (re-audit = delete the rows).
    pub fn record_audits(&mut self, principal: Uuid, doc_ids: &[Uuid]) -> Result<()> {
        for d in doc_ids {
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
             WHERE b.block_type = 'comment' AND b.deleted = 0
             ORDER BY b.id DESC",
            b_cols()
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

    /// Docs whose content is a canvas scene (for tree/type badges).
    pub fn canvas_doc_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT doc_id FROM blocks WHERE block_type = 'canvas_scene' AND deleted = 0",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// (doc_id, principal) of each doc's last applied op — "who tends this".
    pub fn raw_tending(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT doc_id, principal, max(epoch_applied) FROM ops
             WHERE epoch_applied IS NOT NULL GROUP BY doc_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// Doc-to-doc edges: wikilinks resolved by title (graph view, 5.10).
    pub fn raw_links(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT b.doc_id, d2.id
             FROM edges e
             JOIN blocks b ON b.id = e.from_block AND b.deleted = 0
             JOIN docs d1 ON d1.id = b.doc_id AND d1.deleted = 0
             JOIN docs d2 ON (e.to_target = d2.title OR e.to_target LIKE '%/' || d2.title)
             WHERE b.doc_id != d2.id AND d2.deleted = 0",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// doc_id → tags (graph clustering).
    pub fn raw_doc_tags(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.doc_id, t.tag FROM doc_tags t
                 JOIN docs d ON d.id = t.doc_id AND d.deleted = 0
                 ORDER BY t.doc_id",
            )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
        for r in rows {
            let (d, t) = r?;
            out.entry(d).or_default().push(t);
        }
        Ok(out)
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
    /// Mirror docs are read-only replicas at the store layer (#59): every
    /// local content write is refused, whatever the surface. The propose
    /// permission (#60) ships edits UPSTREAM instead — it never writes the
    /// local projection directly. The sync path itself uses
    /// mirror_replace_blocks, which bypasses this by construction.
    fn reject_if_mirror(&self, doc_id: Uuid) -> Result<()> {
        if self.get_mirror(doc_id)?.is_some() {
            return Err(StoreError::InvalidOp(
                "mirror doc is read-only: it is synced from a remote owner".into(),
            ));
        }
        Ok(())
    }

    fn propose_impl(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
        cap_review: bool,
    ) -> Result<ProposeOutcome> {
        self.reject_if_mirror(doc_id)?;
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("propose: empty op list".into()));
        }
        let policy = self.effective_policy(doc_id)?;
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
                scored.note = format!("review requested by proposer; {}", scored.note);
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Hub slice 2 bookkeeping: on-behalf-of principals, forward records,
    /// hub-side transfer offers, and the two-sided transfer ledger.
    #[test]
    fn hub_slice_2_tables_round_trip() {
        use crate::PrincipalKind;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let hub = s.pair_contact(&"aa".repeat(32), "Team").unwrap();
        let bob_key = "bb".repeat(32);
        // a stranger's principal is created once, keyed by pubkey, no contact row
        let p1 = s.remote_principal_for(&bob_key, "bob").unwrap();
        let p2 = s.remote_principal_for(&bob_key, "robert").unwrap();
        assert_eq!(p1, p2, "same pubkey → same principal");
        assert_eq!(s.get_principal(p1).unwrap().display_name, "bob", "first name sticks");
        assert!(s.contact_by_pubkey(&bob_key).unwrap().is_none(), "no contact row");
        // a known contact's principal is reused
        assert_eq!(s.remote_principal_for(&hub.pubkey, "x").unwrap(), hub.principal);
        // empty name never yields an empty principal
        let anon = s.remote_principal_for(&"cc".repeat(32), "  ").unwrap();
        assert_eq!(s.get_principal(anon).unwrap().display_name, "someone");

        // op_statuses carries source_refs
        let doc = s.create_doc("d", None, tom.id).unwrap();
        let ids = s
            .park(
                doc.id,
                p1,
                vec![OpInput {
                    kind: OpKind::Insert {
                        block_id: Uuid::now_v7(),
                        parent_id: None,
                        order_key: "a".into(),
                        block_type: BlockType::Paragraph,
                        content: "hi".into(),
                        refers_to: None,
                    },
                    source_refs: vec!["via hub: Team".into()],
                }],
                "n",
            )
            .unwrap();
        let st = s.op_statuses(&ids).unwrap();
        assert_eq!(st.len(), 1);
        assert!(st[0].source_refs.contains(&"via hub: Team".to_string()));
        assert_eq!(st[0].review.as_deref(), Some("open"));

        // forwards: owner op id → (owner, member, share)
        let member = s.pair_contact(&bob_key, "bob").unwrap();
        let share = Uuid::now_v7();
        s.add_hub_forward(ids[0], hub.id, member.id, share, doc.id).unwrap();
        let f = s.hub_forwards_for(&[ids[0], Uuid::now_v7()]).unwrap();
        assert_eq!(f.len(), 1, "unknown ids skipped");
        assert_eq!((f[0].owner_contact, f[0].member_contact, f[0].owner_share, f[0].doc_id), (hub.id, member.id, share, doc.id));

        // hub transfers: one open offer per (member, root); state moves
        let t1 = s.add_hub_transfer(member.id, doc.id, "d", 3).unwrap();
        assert_eq!((t1.state, t1.doc_count, t1.title.as_str()), (HubTransferState::Offered, 3, "d"));
        let t2 = s.add_hub_transfer(member.id, doc.id, "d2", 4).unwrap();
        assert_ne!(t1.id, t2.id);
        assert_eq!(s.list_hub_transfers().unwrap().len(), 1, "re-offer replaced the open one");
        assert!(s.get_hub_transfer(t1.id).is_err());
        s.set_hub_transfer_state(t2.id, HubTransferState::Done).unwrap();
        assert_eq!(s.get_hub_transfer(t2.id).unwrap().state, HubTransferState::Done);
        let t3 = s.add_hub_transfer(member.id, doc.id, "d3", 1).unwrap();
        assert_eq!(s.list_hub_transfers().unwrap().len(), 2, "a done one is history, not replaced");
        assert!(matches!(s.set_hub_transfer_state(Uuid::now_v7(), HubTransferState::Declined), Err(StoreError::NotFound(_))));
        let _ = t3;

        // doc transfers ledger
        let out = s.add_doc_transfer(doc.id, hub.id, TransferDirection::Out, "offered").unwrap();
        assert_eq!((out.direction, out.state.as_str()), (TransferDirection::Out, "offered"));
        s.set_doc_transfer_state(out.id, "done").unwrap();
        let again = s.add_doc_transfer(doc.id, hub.id, TransferDirection::Out, "offered").unwrap();
        assert_ne!(again.id, out.id);
        let all = s.list_doc_transfers().unwrap();
        assert_eq!(all.len(), 2, "a done record is kept when a new offer arrives");
        assert!(s.add_doc_transfer(doc.id, hub.id, TransferDirection::In, "bogus").is_err());
        assert!(s.set_doc_transfer_state(again.id, "bogus").is_err());
    }

    /// Fresh installs from before the maintainer tier created `shares` with a
    /// CHECK allowing only review|yellow; opening such a DB must widen it so
    /// `green` is accepted, without losing rows or the share_invites FK.
    #[test]
    fn opening_a_db_with_the_old_trust_check_widens_it_for_green() {
        use crate::{PrincipalKind, SharePermission, ShareTrust};
        use rusqlite::params;
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("D", None, tom.id).unwrap();
        // recreate `shares` exactly as the OLD schema did (strict CHECK)
        s.conn.pragma_update(None, "foreign_keys", false).unwrap();
        s.conn
            .execute_batch(
                "DROP TABLE shares;
                 CREATE TABLE shares (
                     id TEXT PRIMARY KEY,
                     root_doc TEXT NOT NULL REFERENCES docs (id),
                     contact TEXT REFERENCES contacts (id),
                     permission TEXT NOT NULL DEFAULT 'view' CHECK (permission IN ('view', 'propose')),
                     state TEXT NOT NULL DEFAULT 'offered' CHECK (state IN ('offered', 'active', 'revoked')),
                     policy_override TEXT CHECK (policy_override IN ('human-review', 'agent-review', 'auto')),
                     trust TEXT NOT NULL DEFAULT 'review' CHECK (trust IN ('review', 'yellow')),
                     created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 );",
            )
            .unwrap();
        s.conn.pragma_update(None, "foreign_keys", true).unwrap();
        let share = s.create_share(doc.id, None, SharePermission::Propose, None).unwrap();
        s.create_invite(share.id, "hash", "2099-01-01T00:00:00.000Z").unwrap();
        // the old CHECK refuses green
        assert!(s.set_share_trust(share.id, ShareTrust::Green).is_err());
        // the migration open() runs widens it
        migrate_pre_schema(&s.conn).unwrap();
        s.set_share_trust(share.id, ShareTrust::Green).unwrap();
        assert_eq!(s.get_share(share.id).unwrap().trust, ShareTrust::Green);
        // row + invite FK survived the rebuild; idempotent on a second run
        assert_eq!(s.list_shares().unwrap().len(), 1);
        let n: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM share_invites WHERE share_id = ?1",
                params![share.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let fk: i64 = s
            .conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 0);
        migrate_pre_schema(&s.conn).unwrap();
        assert_eq!(s.list_shares().unwrap().len(), 1);
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

    /// Remove forgets a contact (shares revoked, row gone, a new redeem from
    /// the same key pairs afresh); block keeps the row with revoked=1 and the
    /// redeem is refused. Holding mirrors from the contact blocks removal.
    #[test]
    fn remove_contact_forgets_without_blocking_and_block_refuses_redeem() {
        use crate::{PrincipalKind, SharePermission};
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let doc = s.create_doc("D", None, tom.id).unwrap();
        let pk = "ab".repeat(32);
        // pair via redeem
        let share = s.create_share(doc.id, None, SharePermission::View, None).unwrap();
        s.create_invite(share.id, "h1", "2099-01-01T00:00:00.000Z").unwrap();
        let (c, _) = s.redeem_invite("h1", &pk, "alice").unwrap();
        assert_eq!(s.get_share(share.id).unwrap().state, crate::ShareState::Active);
        // remove: share revoked, contact gone, redeem works again
        s.remove_contact(c.id).unwrap();
        assert_eq!(s.get_share(share.id).unwrap().state, crate::ShareState::Revoked);
        assert!(s.contact_by_pubkey(&pk).unwrap().is_none());
        let share2 = s.create_share(doc.id, None, SharePermission::View, None).unwrap();
        s.create_invite(share2.id, "h2", "2099-01-01T00:00:00.000Z").unwrap();
        let (c2, _) = s.redeem_invite("h2", &pk, "alice").unwrap();
        // block: row stays, redeem refused, unrevoke lifts it
        s.revoke_contact(c2.id).unwrap();
        let share3 = s.create_share(doc.id, None, SharePermission::View, None).unwrap();
        s.create_invite(share3.id, "h3", "2099-01-01T00:00:00.000Z").unwrap();
        assert!(s.redeem_invite("h3", &pk, "alice").is_err());
        s.unrevoke_contact(c2.id).unwrap();
        s.redeem_invite("h3", &pk, "alice").unwrap();
        // a contact we hold mirrors FROM cannot be removed
        let owner = s.pair_contact(&"cd".repeat(32), "bob").unwrap();
        let root = s.create_doc_with_id(uuid::Uuid::now_v7(), "theirs", None, owner.principal).unwrap();
        s.upsert_mirror(root.id, owner.id, uuid::Uuid::now_v7(), 0, SharePermission::View).unwrap();
        assert!(matches!(s.remove_contact(owner.id), Err(StoreError::InvalidOp(_))));
    }

    /// Hub (slice 1): contacts carry role/membership/is_hub with safe defaults,
    /// mirrors carry relay provenance, publications upsert on share id and go
    /// with their member — and a pre-hub `contacts` table gains the columns.
    #[test]
    fn hub_columns_default_and_round_trip_and_migrate_onto_old_tables() {
        use crate::{ContactRole, Membership, PrincipalKind, SharePermission};
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        let alice = s.pair_contact(&"ab".repeat(32), "alice").unwrap();
        assert_eq!((alice.role, alice.membership, alice.is_hub), (ContactRole::Member, Membership::Active, false));
        s.set_contact_role(alice.id, ContactRole::Admin).unwrap();
        s.set_contact_membership(alice.id, Membership::Pending).unwrap();
        s.set_contact_is_hub(alice.id, true).unwrap();
        let a = s.contact_by_pubkey(&alice.pubkey).unwrap().unwrap();
        assert_eq!((a.role, a.membership, a.is_hub), (ContactRole::Admin, Membership::Pending, true));
        assert!(matches!(s.set_contact_role(Uuid::now_v7(), ContactRole::Admin), Err(StoreError::NotFound(_))));

        // mirror provenance
        let root = s.create_doc_with_id(Uuid::now_v7(), "theirs", None, alice.principal).unwrap();
        let share = Uuid::now_v7();
        s.upsert_mirror(root.id, alice.id, share, 0, SharePermission::Propose).unwrap();
        let m = s.get_mirror(root.id).unwrap().unwrap();
        assert_eq!((m.origin_owner, m.origin_owner_name), (None, None));
        s.set_mirror_origin(root.id, Some("cd"), Some("bob")).unwrap();
        let m = s.get_mirror(root.id).unwrap().unwrap();
        assert_eq!((m.origin_owner.as_deref(), m.origin_owner_name.as_deref()), (Some("cd"), Some("bob")));
        // a re-upsert (every pull) keeps the provenance until it is re-set
        s.upsert_mirror(root.id, alice.id, share, 3, SharePermission::Propose).unwrap();
        assert_eq!(s.get_mirror(root.id).unwrap().unwrap().origin_owner.as_deref(), Some("cd"));
        s.set_mirror_origin(root.id, None, None).unwrap();
        assert_eq!(s.list_mirrors().unwrap()[0].origin_owner, None);

        // publications: upsert on share id, listed, removed with the member
        s.add_hub_publication(share, alice.id, root.id).unwrap();
        s.add_hub_publication(share, alice.id, root.id).unwrap();
        let pubs = s.list_hub_publications().unwrap();
        assert_eq!(pubs.len(), 1);
        assert_eq!((pubs[0].share_id, pubs[0].member_contact, pubs[0].root_doc), (share, alice.id, root.id));
        s.remove_hub_publication(share).unwrap();
        assert!(s.list_hub_publications().unwrap().is_empty());
        s.add_hub_publication(share, alice.id, root.id).unwrap();
        s.remove_mirror(root.id).unwrap();
        s.remove_contact(alice.id).unwrap();
        assert!(s.list_hub_publications().unwrap().is_empty(), "publications go with the member");
        let _ = tom;

        // migration: a pre-hub contacts table gains the three columns with defaults
        let old = SqliteStore::open_in_memory().unwrap();
        old.conn.pragma_update(None, "foreign_keys", false).unwrap();
        old.conn
            .execute_batch(
                "DROP TABLE contacts;
                 CREATE TABLE contacts (
                     id TEXT PRIMARY KEY,
                     pubkey TEXT NOT NULL UNIQUE,
                     petname TEXT NOT NULL,
                     principal TEXT NOT NULL REFERENCES principals (id),
                     verified INTEGER NOT NULL DEFAULT 0,
                     revoked INTEGER NOT NULL DEFAULT 0,
                     paired_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 );
                 INSERT INTO principals (id, kind, display_name) VALUES ('p1', 'remote', 'x');
                 INSERT INTO contacts (id, pubkey, petname, principal) VALUES ('c1', 'k', 'x', 'p1');",
            )
            .unwrap();
        old.conn.pragma_update(None, "foreign_keys", true).unwrap();
        assert!(old.list_contacts().is_err(), "old table lacks the columns");
        migrate_pre_schema(&old.conn).unwrap();
        migrate_pre_schema(&old.conn).unwrap(); // idempotent
        let cs = old.list_contacts();
        // ids in this hand-made row are not uuids, so mapping fails on id — check columns directly
        let _ = cs;
        let (role, membership, is_hub): (String, String, bool) = old
            .conn
            .query_row("SELECT role, membership, is_hub FROM contacts WHERE id = 'c1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((role.as_str(), membership.as_str(), is_hub), ("member", "active", false));
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
