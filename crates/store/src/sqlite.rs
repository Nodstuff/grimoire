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
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }
}

fn uuid_col(s: String, ctx: &str) -> Result<Uuid> {
    Uuid::parse_str(&s).map_err(|_| StoreError::InvalidOp(format!("bad uuid in {ctx}: {s}")))
}

type RawDoc = (String, Option<String>, String, Option<String>, i64, String);

fn row_to_doc(row: &rusqlite::Row) -> rusqlite::Result<RawDoc> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn build_doc(raw: RawDoc) -> Result<Doc> {
    let (id, parent, title, policy, epoch, created_by) = raw;
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
    ))
}

fn build_block(raw: RawBlock) -> Result<Block> {
    let (id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, deleted) = raw;
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
    })
}

const BLOCK_COLS: &str =
    "id, doc_id, parent_id, order_key, block_type, content, created_by, epoch, deleted";

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
        } => {
            if let Some(p) = parent_id {
                live_block(tx, doc_id, *p, "parent")?;
            }
            let n = tx.execute(
                "INSERT INTO blocks (id, doc_id, parent_id, order_key, block_type, content, created_by, epoch)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (id) DO NOTHING",
                params![
                    block_id.to_string(),
                    doc_id.to_string(),
                    parent_id.map(|p| p.to_string()),
                    order_key,
                    block_type.as_str(),
                    content,
                    principal.to_string(),
                    epoch
                ],
            )?;
            if n == 0 {
                return Err(StoreError::InvalidOp(format!(
                    "insert: block {block_id} already exists"
                )));
            }
        }
        OpKind::Replace { target, content } => {
            live_block(tx, doc_id, *target, "replace target")?;
            tx.execute(
                "UPDATE blocks SET content = ?1, epoch = ?2 WHERE id = ?3",
                params![content, epoch, target.to_string()],
            )?;
        }
        OpKind::Delete { target } => {
            live_block(tx, doc_id, *target, "delete target")?;
            tx.execute(
                "UPDATE blocks SET deleted = 1, epoch = ?1 WHERE id = ?2",
                params![epoch, target.to_string()],
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
            "SELECT id, parent_id, title, review_policy, current_epoch, created_by
             FROM docs ORDER BY title",
        )?;
        let rows = stmt.query_map([], row_to_doc)?;
        rows.map(|r| build_doc(r?)).collect()
    }

    fn get_doc(&self, id: Uuid) -> Result<Doc> {
        let raw = self
            .conn
            .query_row(
                "SELECT id, parent_id, title, review_policy, current_epoch, created_by
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
            tx.execute(
                "INSERT INTO ops (id, doc_id, op_type, target_block, payload, principal,
                                  base_epoch, epoch_applied, verdict, source_refs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'green', ?9)",
                params![
                    op_id.to_string(),
                    doc_id.to_string(),
                    op.kind.op_type(),
                    op.kind.target_block().map(|t| t.to_string()),
                    serde_json::to_string(&op.kind)?,
                    principal.to_string(),
                    base_epoch,
                    epoch,
                    serde_json::to_string(&op.source_refs)?,
                ],
            )?;
            project(&tx, doc_id, epoch, principal, &op.kind)?;
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
        let mut stmt = self.conn.prepare(
            "SELECT id, doc_id, payload, principal, base_epoch, epoch_applied, verdict, confidence, source_refs
             FROM ops
             WHERE doc_id = ?1 AND epoch_applied IS NOT NULL AND epoch_applied > ?2
             ORDER BY epoch_applied, id",
        )?;
        let rows = stmt.query_map(params![doc_id.to_string(), since_epoch], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?;
        rows.map(|r| {
            let (
                id,
                doc_id,
                payload,
                principal,
                base_epoch,
                epoch_applied,
                verdict,
                confidence,
                source_refs,
            ) = r?;
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
                source_refs: serde_json::from_str(&source_refs)?,
            })
        })
        .collect()
    }
}
