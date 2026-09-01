//! ks-store: the substrate (PROJECT.md §3.1–3.2).
//!
//! Ledger (`ops`) is the primary write record; `blocks` is the projection,
//! written in the same transaction. One committed `apply` = one epoch.

pub mod export;
pub mod gate;
pub mod import;
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

/// Policy when a doc and all its ancestors leave review_policy null.
/// Human-review until the reviewer agent (4.8) exists; flip to AgentReview then.
pub const DEFAULT_REVIEW_POLICY: ReviewPolicy = ReviewPolicy::HumanReview;

/// Storage boundary (ADR 0001). No SQL above this trait.
pub trait BlockStore {
    fn create_principal(
        &mut self,
        kind: PrincipalKind,
        display_name: &str,
        pubkey: Option<&str>,
    ) -> Result<Principal>;

    fn get_principal(&self, id: Uuid) -> Result<Principal>;

    fn list_principals(&self) -> Result<Vec<Principal>>;

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

    /// The doc's effective review policy: its own column, else the nearest
    /// ancestor's (one recursive lookup, ticket 2.10), else DEFAULT_REVIEW_POLICY.
    /// Consulted by propose: under Auto, yellows at/above gate::HIGH_CONFIDENCE
    /// self-apply without an annotation; reds always park regardless.
    fn effective_policy(&self, doc_id: Uuid) -> Result<ReviewPolicy>;

    /// Set or clear (None = inherit) a doc's review policy. Deliberately NOT
    /// exposed over MCP: an agent that can flip a doc to `auto` weakens the
    /// gate — policy changes are a human/UI surface.
    fn set_review_policy(&mut self, doc_id: Uuid, policy: Option<ReviewPolicy>) -> Result<()>;

    /// Doc lifecycle status (5.6): draft | in-review | decided | superseded;
    /// None clears. "All decided docs touching X" = search + status filter.
    fn set_doc_status(&mut self, doc_id: Uuid, status: Option<DocStatus>) -> Result<()>;

    /// Reparent/reorder a doc in the tree (cycle-checked; fractional sort_key).
    fn move_doc(&mut self, doc_id: Uuid, new_parent: Option<Uuid>, sort_key: Option<&str>)
    -> Result<()>;

    /// Soft-delete a doc and its descendants; returns count. Reversible.
    fn delete_doc(&mut self, doc_id: Uuid) -> Result<usize>;

    /// Rename a doc. NOTE: inbound [[wikilinks]] resolve by title and are not
    /// rewritten — they dangle until edited (or an agent fixes them).
    fn rename_doc(&mut self, doc_id: Uuid, title: &str) -> Result<()>;

    /// Substring search over live block content (ticket 3.4's v0: LIKE;
    /// FTS5+trigram replaces the internals without changing the signature).
    /// Results are blocks, not docs — the editable unit (§3.3).
    fn search_blocks(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>>;

    /// Blocks anywhere that [[wikilink]] to this doc (ticket 2.11) — matched
    /// by title against raw link targets (Octarine links are workspace paths).
    fn backlinks(&self, doc_id: Uuid) -> Result<Vec<SearchHit>>;

    /// Comments are blocks (ticket 3.5): block_type=comment, refers_to = the
    /// anchored content block; threads are trees via parent_id.
    fn add_comment(
        &mut self,
        target_block: Uuid,
        principal: Uuid,
        text: &str,
        reply_to: Option<Uuid>,
    ) -> Result<Block>;

    fn list_comments(&self, target_block: Uuid) -> Result<Vec<Block>>;

    /// The propose gate (ticket 2.5): the write path for agents and for any
    /// stale base. Current base → all green, applied. Stale base → per-op
    /// verdicts via `gate::score_stale_op`: greens apply; yellows apply with
    /// an open `review` annotation; reds park unapplied with a `parked`
    /// annotation, payload preserving the proposed text verbatim. One epoch
    /// bump iff anything applied. Never errors on content — a projection
    /// failure parks the op red instead.
    fn propose(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ProposeOutcome>;

    /// Park ops as unapplied reds with `parked` annotations — a drafted
    /// change awaiting judgment (the auditor's unverified-fix path). Nothing
    /// touches the projection; accepting later applies at the then-current
    /// epoch via resolve().
    fn park(
        &mut self,
        doc_id: Uuid,
        principal: Uuid,
        ops: Vec<OpInput>,
        note: &str,
    ) -> Result<Vec<Uuid>>;

    /// Open annotations (yellows + parked reds), oldest first — sorted by
    /// date it *is* the daily digest (§3.5). `None` = across all docs.
    fn review_queue(&self, doc_id: Option<Uuid>) -> Result<Vec<ReviewItem>>;

    /// Propose with every verdict capped at yellow: greens become applied,
    /// flagged yellows (confidence kept). "Auto-tagging that lands as
    /// reviewable yellows, declinable as a batch" — gardener confidence_policy
    /// 'review' (§5).
    fn propose_reviewed(
        &mut self,
        doc_id: Uuid,
        base_epoch: i64,
        principal: Uuid,
        ops: Vec<OpInput>,
    ) -> Result<ProposeOutcome>;

    // --- gardener registry (4.1) + run log (4.5) ---

    fn create_gardener(
        &mut self,
        name: &str,
        kind: GardenerKind,
        task_prompt: &str,
        scope_doc: Option<Uuid>,
        confidence_policy: ConfidencePolicy,
    ) -> Result<Gardener>;

    fn list_gardeners(&self) -> Result<Vec<Gardener>>;

    fn set_gardener_enabled(&mut self, id: Uuid, enabled: bool) -> Result<()>;

    /// Update a gardener's config (4.1: create/edit/disable without code changes).
    fn update_gardener(
        &mut self,
        id: Uuid,
        task_prompt: &str,
        schedule: &str,
        confidence_policy: ConfidencePolicy,
        scope_doc: Option<Uuid>,
        enabled: bool,
        bindings: serde_json::Value,
    ) -> Result<()>;

    fn start_run(&mut self, gardener: Uuid) -> Result<Uuid>;

    fn finish_run(
        &mut self,
        run: Uuid,
        status: &str,
        summary: &str,
        tokens_used: Option<i64>,
        tool_calls: Option<i64>,
    ) -> Result<()>;

    fn list_runs(&self, limit: usize) -> Result<Vec<GardenerRun>>;

    // --- tags (2.12): extracted from frontmatter, queryable ---

    fn list_tags(&self) -> Result<Vec<(String, i64)>>;

    fn docs_by_tag(&self, tag: &str) -> Result<Vec<Doc>>;

    /// Leaf docs (≥1 block) with no tags — the tagging gardener's worklist.
    fn untagged_docs(&self, limit: usize) -> Result<Vec<Doc>>;

    /// Resolve one annotation. Invariant enforced here: proposer ≠ approver.
    /// - accept yellow: clear the annotation (the edit is already live)
    /// - decline yellow: revert via the op's pre-image, as a new green op by
    ///   the reviewer (a receipt is returned)
    /// - accept red: apply the parked op now, at the current epoch (receipt)
    /// - decline red: park closed, never applied
    fn resolve(
        &mut self,
        annotation_id: Uuid,
        reviewer: Uuid,
        decision: ReviewDecision,
    ) -> Result<Option<ApplyReceipt>>;
}
