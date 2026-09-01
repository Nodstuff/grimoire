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

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
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
fn migrate_pre_schema(conn: &Connection) -> Result<()> {
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

/// Populate FTS and edges for rows that predate their triggers/extraction.
/// Gated on user_version: count(*) on an external-content FTS table proxies
/// the content table, so emptiness is unobservable — version it instead.
const SCHEMA_VERSION: i64 = 3;

fn backfill(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
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
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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

type RawContact = (String, String, String, String, bool, bool, String);

fn contact_row(row: &rusqlite::Row) -> rusqlite::Result<RawContact> {
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

fn finish_contact(raw: RawContact) -> Result<Contact> {
    let (id, pubkey, petname, principal, verified, revoked, paired_at) = raw;
    Ok(Contact {
        id: uuid_col(id, "contacts.id")?,
        pubkey,
        petname,
        principal: uuid_col(principal, "contacts.principal")?,
        verified,
        revoked,
        paired_at,
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
    ))
}

fn finish_share(raw: RawShare) -> Result<Share> {
    let (id, root_doc, contact, permission, state, policy_override, created_at) = raw;
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
    })
}

type RawMirror = (String, String, String, i64);

fn mirror_row(row: &rusqlite::Row) -> rusqlite::Result<RawMirror> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn finish_mirror(raw: RawMirror) -> Result<Mirror> {
    let (doc_id, owner, share_id, synced_epoch) = raw;
    Ok(Mirror {
        doc_id: uuid_col(doc_id, "mirrors.doc_id")?,
        owner: uuid_col(owner, "mirrors.owner")?,
        share_id: uuid_col(share_id, "mirrors.share_id")?,
        synced_epoch,
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

/// Fetch a block by id within a doc, tombstoned ones included.
fn block_by_id(tx: &Transaction, doc_id: Uuid, id: Uuid) -> Result<Option<Block>> {
    tx.query_row(
        &format!("SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1 AND doc_id = ?2"),
        params![id.to_string(), doc_id.to_string()],
        row_to_block,
    )
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
        OpKind::Delete { .. } => Err(StoreError::InvalidOp(
            "cannot invert a delete (deletes never yellow — gate bias)".into(),
        )),
    }
}

/// Resolve agent-friendly order_key specs before anything is persisted:
/// "" = append after the last sibling; "after:<uuid>" = between that block
/// and its next sibling. Real keys pass through untouched, and the ledger
/// stores the resolved op.
fn resolve_order_keys(tx: &Transaction, doc_id: Uuid, kind: &mut OpKind) -> Result<()> {
    let OpKind::Insert {
        parent_id,
        order_key,
        ..
    } = kind
    else {
        return Ok(());
    };
    let spec = order_key.clone();
    if !spec.is_empty() && !spec.starts_with("after:") {
        return Ok(());
    }
    let siblings: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, order_key FROM blocks
             WHERE doc_id = ?1 AND deleted = 0
               AND ((?2 IS NULL AND parent_id IS NULL) OR parent_id = ?2)
             ORDER BY order_key",
        )?;
        let rows = stmt.query_map(
            params![doc_id.to_string(), parent_id.map(|p| p.to_string())],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?;
        rows.collect::<rusqlite::Result<_>>()?
    };
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

/// Fetch a live (non-deleted) block within a doc, for projection checks.
fn live_block(tx: &Transaction, doc_id: Uuid, id: Uuid, role: &str) -> Result<Block> {
    let raw = tx
        .query_row(
            &format!(
                "SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1 AND doc_id = ?2 AND deleted = 0"
            ),
            params![id.to_string(), doc_id.to_string()],
            row_to_block,
        )
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
            let n = tx.execute(
                "INSERT INTO blocks (id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, refers_to)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (id) DO NOTHING",
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
            if n == 0 {
                return Err(StoreError::InvalidOp(format!(
                    "insert: block {block_id} already exists"
                )));
            }
            set_edges(tx, *block_id, content)?;
            set_tags(tx, doc_id, *block_id, content)?;
        }
        OpKind::Replace { target, content } => {
            live_block(tx, doc_id, *target, "replace target")?;
            tx.execute(
                "UPDATE blocks SET content = ?1, epoch = ?2 WHERE id = ?3",
                params![content, epoch, target.to_string()],
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
        self.conn.execute(
            "INSERT INTO docs (id, parent_id, title, created_by) VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                parent.map(|p| p.to_string()),
                title,
                created_by.to_string()
            ],
        )?;
        self.get_doc(id)
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
        let mut stmt = self.conn.prepare(&format!(
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
        let raw = self
            .conn
            .query_row(
                &format!("SELECT {BLOCK_COLS} FROM blocks WHERE id = ?1"),
                params![id.to_string()],
                row_to_block,
            )
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
        if ops.is_empty() {
            return Err(StoreError::InvalidOp("apply: empty op list".into()));
        }
        let tx = self.conn.transaction()?;

        let current: i64 = tx
            .query_row(
                "SELECT current_epoch FROM docs WHERE id = ?1",
                params![doc_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("doc {doc_id}")))?;
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
            resolve_order_keys(&tx, doc_id, &mut op.kind)?;
            let op = &op;
            let prior = match op.kind.target_block() {
                Some(t) => block_by_id(&tx, doc_id, t)?,
                None => None,
            };
            project(&tx, doc_id, epoch, principal, &op.kind)?;
            insert_op_row(
                &tx,
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
        tx.commit()?;
        Ok(ApplyReceipt {
            doc_id,
            epoch,
            op_ids,
        })
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
        let mut n = 0;
        for d in &to_delete {
            n += self.conn.execute(
                "UPDATE docs SET deleted = 1 WHERE id = ?1",
                params![d.to_string()],
            )?;
        }
        if n == 0 {
            return Err(StoreError::NotFound(format!("doc {doc_id}")));
        }
        Ok(n)
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
             WHERE b.deleted = 0 AND (e.to_target = ?1 OR e.to_target LIKE '%/' || ?1)
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
                 WHERE blocks_fts MATCH ?1 AND b.deleted = 0
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
             WHERE b.deleted = 0 AND b.content LIKE ?1 ESCAPE '\\'
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
            "SELECT tag, count(DISTINCT doc_id) FROM doc_tags GROUP BY tag ORDER BY 2 DESC, tag",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    fn docs_by_tag(&self, tag: &str) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d JOIN doc_tags t ON t.doc_id = d.id
             WHERE t.tag = ?1 GROUP BY d.id ORDER BY d.title",
        )?;
        let rows = stmt.query_map(params![tag.to_lowercase()], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    fn untagged_docs(&self, limit: usize) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.parent_id, d.title, d.review_policy, d.current_epoch, d.created_by, d.status, d.sort_key
             FROM docs d
             WHERE EXISTS (SELECT 1 FROM blocks b WHERE b.doc_id = d.id AND b.deleted = 0)
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
        if let Some(existing) = self.contact_by_pubkey(pubkey)? {
            self.conn.execute(
                "UPDATE contacts SET petname = ?1, revoked = 0 WHERE id = ?2",
                params![petname, existing.id.to_string()],
            )?;
            return Ok(Contact {
                petname: petname.into(),
                revoked: false,
                ..existing
            });
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
        let mut stmt = self.conn.prepare(
            "SELECT id, pubkey, petname, principal, verified, revoked, paired_at
             FROM contacts ORDER BY paired_at",
        )?;
        let rows = stmt.query_map([], contact_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_contact)
            .collect()
    }

    fn contact_by_pubkey(&self, pubkey: &str) -> Result<Option<Contact>> {
        self.conn
            .query_row(
                "SELECT id, pubkey, petname, principal, verified, revoked, paired_at
                 FROM contacts WHERE pubkey = ?1",
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

    fn create_share(
        &mut self,
        root_doc: Uuid,
        contact: Option<Uuid>,
        permission: SharePermission,
        policy_override: Option<ReviewPolicy>,
    ) -> Result<Share> {
        self.get_doc(root_doc)?; // must exist
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
            "SELECT id, root_doc, contact, permission, state, policy_override, created_at
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
                "SELECT id, root_doc, contact, permission, state, policy_override, created_at
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

    fn create_invite(&mut self, share_id: Uuid, secret_hash: &str, expires_at: &str) -> Result<Uuid> {
        self.get_share(share_id)?;
        let id = Uuid::now_v7();
        self.conn.execute(
            "INSERT INTO share_invites (id, share_id, secret_hash, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), share_id.to_string(), secret_hash, expires_at],
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
        let contact = self.pair_contact(pubkey, petname)?;
        // burn first: even if binding fails, the secret is single-use
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
        Ok((contact, self.get_share(share_id)?))
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
                    s.policy_override, s.created_at
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
        self.conn.execute(
            "INSERT INTO docs (id, parent_id, title, created_by) VALUES (?1, ?2, ?3, ?4)",
            params![
                id.to_string(),
                parent.map(|p| p.to_string()),
                title,
                created_by.to_string()
            ],
        )?;
        self.get_doc(id)
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
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mirrors (doc_id, owner, share_id, synced_epoch)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (doc_id) DO UPDATE SET
                 owner = excluded.owner,
                 share_id = excluded.share_id,
                 synced_epoch = excluded.synced_epoch",
            params![
                doc_id.to_string(),
                owner.to_string(),
                share_id.to_string(),
                synced_epoch
            ],
        )?;
        Ok(())
    }

    fn get_mirror(&self, doc_id: Uuid) -> Result<Option<Mirror>> {
        self.conn
            .query_row(
                "SELECT doc_id, owner, share_id, synced_epoch FROM mirrors WHERE doc_id = ?1",
                params![doc_id.to_string()],
                mirror_row,
            )
            .optional()?
            .map(finish_mirror)
            .transpose()
    }

    fn list_mirrors(&self) -> Result<Vec<Mirror>> {
        let mut stmt = self
            .conn
            .prepare("SELECT doc_id, owner, share_id, synced_epoch FROM mirrors ORDER BY doc_id")?;
        let rows = stmt.query_map([], mirror_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(finish_mirror)
            .collect()
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

        // the trust invariant (§3.4): proposer ≠ approver, enforced at the gate
        if op.principal == reviewer {
            return Err(StoreError::InvalidOp(
                "proposer cannot resolve their own proposal".into(),
            ));
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
                project(&tx, doc_id, epoch, op.principal, &op.kind)?;
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
             WHERE b.deleted = 0 AND (e.to_target = ?1 OR e.to_target LIKE '%/' || ?1)",
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
                      + (SELECT COALESCE(sum(length(coalesce(sort_key,'')) + length(coalesce(parent_id,'')) + length(title)), 0) FROM docs WHERE deleted = 0)",
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
             JOIN docs d2 ON (e.to_target = d2.title OR e.to_target LIKE '%/' || d2.title)
             WHERE b.doc_id != d2.id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| Ok(r?)).collect()
    }

    /// doc_id → tags (graph clustering).
    pub fn raw_doc_tags(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT doc_id, tag FROM doc_tags ORDER BY doc_id")?;
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
