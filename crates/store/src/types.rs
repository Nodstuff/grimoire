use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrincipalKind {
    Human,
    Agent,
    Remote,
}

impl PrincipalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrincipalKind::Human => "human",
            PrincipalKind::Agent => "agent",
            PrincipalKind::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(PrincipalKind::Human),
            "agent" => Some(PrincipalKind::Agent),
            "remote" => Some(PrincipalKind::Remote),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub id: Uuid,
    pub kind: PrincipalKind,
    pub display_name: String,
    pub pubkey: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewPolicy {
    HumanReview,
    AgentReview,
    Auto,
}

impl ReviewPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewPolicy::HumanReview => "human-review",
            ReviewPolicy::AgentReview => "agent-review",
            ReviewPolicy::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human-review" => Some(ReviewPolicy::HumanReview),
            "agent-review" => Some(ReviewPolicy::AgentReview),
            "auto" => Some(ReviewPolicy::Auto),
            _ => None,
        }
    }
}

/// Doc lifecycle (ticket 5.6): dogfooding the decision-doc format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocStatus {
    Draft,
    InReview,
    Decided,
    Superseded,
}

impl DocStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocStatus::Draft => "draft",
            DocStatus::InReview => "in-review",
            DocStatus::Decided => "decided",
            DocStatus::Superseded => "superseded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(DocStatus::Draft),
            "in-review" => Some(DocStatus::InReview),
            "decided" => Some(DocStatus::Decided),
            "superseded" => Some(DocStatus::Superseded),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub review_policy: Option<ReviewPolicy>,
    pub current_epoch: i64,
    pub created_by: Uuid,
    pub status: Option<DocStatus>,
    pub sort_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    Paragraph,
    Heading,
    Code,
    DiagramD2,
    DiagramMermaid,
    CanvasScene,
    Comment,
    Decision,
}

impl BlockType {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockType::Paragraph => "paragraph",
            BlockType::Heading => "heading",
            BlockType::Code => "code",
            BlockType::DiagramD2 => "diagram_d2",
            BlockType::DiagramMermaid => "diagram_mermaid",
            BlockType::CanvasScene => "canvas_scene",
            BlockType::Comment => "comment",
            BlockType::Decision => "decision",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "paragraph" => Some(BlockType::Paragraph),
            "heading" => Some(BlockType::Heading),
            "code" => Some(BlockType::Code),
            "diagram_d2" => Some(BlockType::DiagramD2),
            "diagram_mermaid" => Some(BlockType::DiagramMermaid),
            "canvas_scene" => Some(BlockType::CanvasScene),
            "comment" => Some(BlockType::Comment),
            "decision" => Some(BlockType::Decision),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub id: Uuid,
    pub doc_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub order_key: String,
    pub block_type: BlockType,
    pub content: String,
    pub created_by: Uuid,
    pub epoch: i64,
    pub deleted: bool,
    /// Comment blocks: the content block this thread anchors to.
    #[serde(default)]
    pub refers_to: Option<Uuid>,
}

/// A doc's blocks as a tree, children ordered by `order_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockNode {
    pub block: Block,
    pub children: Vec<BlockNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocTree {
    pub doc: Doc,
    pub roots: Vec<BlockNode>,
}

/// The block-level operations — exactly these, never finer (PROJECT.md §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OpKind {
    Insert {
        block_id: Uuid,
        parent_id: Option<Uuid>,
        order_key: String,
        block_type: BlockType,
        content: String,
        /// Comment anchor (comment blocks only).
        #[serde(default)]
        refers_to: Option<Uuid>,
    },
    Replace {
        target: Uuid,
        content: String,
    },
    Delete {
        target: Uuid,
    },
    Move {
        target: Uuid,
        new_parent: Option<Uuid>,
        new_order_key: String,
    },
}

impl OpKind {
    pub fn op_type(&self) -> &'static str {
        match self {
            OpKind::Insert { .. } => "insert",
            OpKind::Replace { .. } => "replace",
            OpKind::Delete { .. } => "delete",
            OpKind::Move { .. } => "move",
        }
    }

    pub fn target_block(&self) -> Option<Uuid> {
        match self {
            OpKind::Insert { block_id, .. } => Some(*block_id),
            OpKind::Replace { target, .. }
            | OpKind::Delete { target }
            | OpKind::Move { target, .. } => Some(*target),
        }
    }
}

/// One proposed/applied operation with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpInput {
    pub kind: OpKind,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Green,
    Yellow,
    Red,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Green => "green",
            Verdict::Yellow => "yellow",
            Verdict::Red => "red",
        }
    }
}

/// A ledger row: the primary write record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LedgerOp {
    pub id: Uuid,
    pub doc_id: Uuid,
    pub kind: OpKind,
    pub principal: Uuid,
    pub base_epoch: i64,
    pub epoch_applied: Option<i64>,
    pub verdict: Option<Verdict>,
    pub confidence: Option<f64>,
    /// Pre-image of the affected block (None for inserts): powers
    /// decline-revert, verbatim red parking, and before/after diffs.
    pub prior: Option<Block>,
    pub source_refs: Vec<String>,
}

/// Result of a committed `apply`: one transaction, one epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyReceipt {
    pub doc_id: Uuid,
    pub epoch: i64,
    pub op_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    /// Applied yellow awaiting review.
    Review,
    /// Red: parked, never applied.
    Parked,
}

impl AnnotationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnnotationKind::Review => "review",
            AnnotationKind::Parked => "parked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationStatus {
    Open,
    Accepted,
    Declined,
}

impl AnnotationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnnotationStatus::Open => "open",
            AnnotationStatus::Accepted => "accepted",
            AnnotationStatus::Declined => "declined",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Annotation {
    pub id: Uuid,
    pub doc_id: Uuid,
    pub op_id: Uuid,
    pub kind: AnnotationKind,
    pub status: AnnotationStatus,
    pub resolved_by: Option<Uuid>,
}

/// Per-op outcome of a `propose` call: structured, no prose parsing (§3.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProposeVerdict {
    pub op_id: Uuid,
    pub verdict: Verdict,
    pub confidence: f64,
    pub applied: bool,
    /// Placement context for the reviewer/agent, e.g. why an op went red.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProposeOutcome {
    pub doc_id: Uuid,
    /// Doc epoch after the call (unchanged when nothing applied).
    pub epoch: i64,
    pub verdicts: Vec<ProposeVerdict>,
}

/// A review-queue entry: the annotation plus the op it references.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewItem {
    pub annotation: Annotation,
    pub op: LedgerOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Accept,
    Decline,
}

/// A search hit: the block plus its doc breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub block: Block,
    pub doc_title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfidencePolicy {
    /// Every proposal lands as a reviewable yellow (declinable as a batch).
    Review,
    /// Normal gate verdicts (greens auto-apply).
    Gate,
}

impl ConfidencePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConfidencePolicy::Review => "review",
            ConfidencePolicy::Gate => "gate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "review" => Some(ConfidencePolicy::Review),
            "gate" => Some(ConfidencePolicy::Gate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GardenerKind {
    /// Sweeps docs, proposes content (tags, updates).
    Tagging,
    /// Reads the review queue on agent-review docs and resolves it (4.8).
    Reviewer,
    /// Veracity sweeps: reads the stalest docs and flags suspect claims
    /// as comments on the offending blocks. Flags, never edits.
    /// SCOPED ONLY: must be tied to a doc subtree.
    Auditor,
    /// Writes new docs from nothing: style exemplars + bound repos +
    /// instructions → a doc tree under its scope. SCOPED ONLY.
    Scribe,
    /// Keeps its scope true to its bound sources: proposes updates as
    /// reviewable yellows when the repo and the docs disagree. SCOPED ONLY.
    Keeper,
}

impl GardenerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GardenerKind::Tagging => "tagging",
            GardenerKind::Reviewer => "reviewer",
            GardenerKind::Auditor => "auditor",
            GardenerKind::Scribe => "scribe",
            GardenerKind::Keeper => "keeper",
        }
    }

    /// Kinds that require a scope doc — the opt-in boundary: docs outside
    /// every scoped tending are never touched by these.
    pub fn scoped_only(&self) -> bool {
        matches!(
            self,
            GardenerKind::Auditor | GardenerKind::Scribe | GardenerKind::Keeper
        )
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tagging" => Some(GardenerKind::Tagging),
            "reviewer" => Some(GardenerKind::Reviewer),
            "auditor" => Some(GardenerKind::Auditor),
            "scribe" => Some(GardenerKind::Scribe),
            "keeper" => Some(GardenerKind::Keeper),
            _ => None,
        }
    }
}

/// A gardener is config, not construction (PROJECT.md §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gardener {
    pub id: Uuid,
    pub name: String,
    pub kind: GardenerKind,
    pub principal: Uuid,
    pub scope_doc: Option<Uuid>,
    pub task_prompt: String,
    pub bindings: serde_json::Value,
    pub creds_ref: Option<String>,
    pub schedule: String,
    pub confidence_policy: ConfidencePolicy,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GardenerRun {
    pub id: Uuid,
    pub gardener: Uuid,
    pub gardener_name: String,
    pub started_at: String,
    pub status: String,
    pub summary: Option<String>,
    pub tokens_used: Option<i64>,
    pub tool_calls: Option<i64>,
}

// --- federation (ADR 0002) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SharePermission {
    View,
    Propose,
}

impl SharePermission {
    pub fn as_str(&self) -> &'static str {
        match self {
            SharePermission::View => "view",
            SharePermission::Propose => "propose",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "view" => Some(SharePermission::View),
            "propose" => Some(SharePermission::Propose),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShareState {
    Offered,
    Active,
    Revoked,
}

impl ShareState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShareState::Offered => "offered",
            ShareState::Active => "active",
            ShareState::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "offered" => Some(ShareState::Offered),
            "active" => Some(ShareState::Active),
            "revoked" => Some(ShareState::Revoked),
            _ => None,
        }
    }
}

/// A paired peer: pubkey identifies the actor, the linked remote principal
/// carries provenance for everything they propose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Contact {
    pub id: Uuid,
    pub pubkey: String,
    pub petname: String,
    pub principal: Uuid,
    pub verified: bool,
    pub revoked: bool,
    pub paired_at: String,
}

/// An owner-side grant: this subtree, this contact, this permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Share {
    pub id: Uuid,
    pub root_doc: Uuid,
    pub contact: Option<Uuid>,
    pub permission: SharePermission,
    pub state: ShareState,
    pub policy_override: Option<ReviewPolicy>,
    pub created_at: String,
}

/// Grantee-side origin + pull cursor for a mirror doc (same UUID as upstream).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mirror {
    pub doc_id: Uuid,
    pub owner: Uuid,
    pub share_id: Uuid,
    pub synced_epoch: i64,
}

/// A queued join (grantee-side): a redeem that waits for the owner to be up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingJoin {
    pub id: Uuid,
    pub ticket: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
}
