//! ks-store: the substrate (PROJECT.md §3.1–3.2).
//!
//! Ledger (`ops`) is the primary write record; `blocks` is the projection,
//! written in the same transaction. One committed `apply` = one epoch.

pub mod order_key;
mod sqlite;
mod types;

pub use sqlite::SqliteStore;
pub use types::*;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The write declared a base epoch behind the doc's current epoch.
    /// v1 direct-write path rejects; the propose gate (ticket 2.5) will
    /// route these through confidence scoring instead.
    #[error("stale base epoch {base} (doc is at {current})")]
    StaleBase { base: i64, current: i64 },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid op: {0}")]
    InvalidOp(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Storage boundary (ADR 0001). No SQL above this trait.
pub trait BlockStore {
    fn create_principal(
        &mut self,
        kind: PrincipalKind,
        display_name: &str,
        pubkey: Option<&str>,
    ) -> Result<Principal>;

    fn get_principal(&self, id: Uuid) -> Result<Principal>;

    fn create_doc(&mut self, title: &str, parent: Option<Uuid>, created_by: Uuid) -> Result<Doc>;

    fn list_docs(&self) -> Result<Vec<Doc>>;

    fn get_doc(&self, id: Uuid) -> Result<Doc>;

    /// Full tree of live (non-deleted) blocks, children ordered by order_key.
    fn read_doc(&self, id: Uuid) -> Result<DocTree>;

    fn read_block(&self, id: Uuid) -> Result<Block>;

    /// Apply ops at the doc's current epoch: one transaction, one epoch bump,
    /// every op landing in the ledger with verdict green + its projection.
    /// A stale `base_epoch` is an error here — the propose gate (2.5) is the
    /// path for stale writes, not this one.
    fn apply(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ApplyReceipt>;

    /// Ledger ops applied after `since_epoch`, oldest first (tool 3.6's SELECT).
    fn ops_since(&self, doc_id: Uuid, since_epoch: i64) -> Result<Vec<LedgerOp>>;
}
