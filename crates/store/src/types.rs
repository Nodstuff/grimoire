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
    /// Hub membership (slice 1). On a HUB these describe the contact as a
    /// member; on a member's own instance, for a contact that `is_hub`, they
    /// describe MY standing at that hub (copied from the hub's answers).
    /// Plain peer-to-peer contacts sit at the defaults (member, active).
    #[serde(default)]
    pub role: ContactRole,
    #[serde(default)]
    pub membership: Membership,
    /// This contact is a hub (`grimoire serve --hub`): its shares are team
    /// folders, relayed docs under them carry an origin owner.
    #[serde(default)]
    pub is_hub: bool,
}

/// A hub member's role. Admins approve joins, eject, and change roles — over
/// the wire, so an admin never needs the hub's own UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContactRole {
    #[default]
    Member,
    Admin,
}

impl ContactRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContactRole::Member => "member",
            ContactRole::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "member" => Some(ContactRole::Member),
            "admin" => Some(ContactRole::Admin),
            _ => None,
        }
    }
}

/// Where a contact stands with a hub: pending = redeemed an invite, waiting
/// for an admin; active = full member; ejected = removed by an admin (the
/// contact is also blocked, so a fresh invite is refused until unblocked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Membership {
    Pending,
    #[default]
    Active,
    Ejected,
}

impl Membership {
    pub fn as_str(&self) -> &'static str {
        match self {
            Membership::Pending => "pending",
            Membership::Active => "active",
            Membership::Ejected => "ejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Membership::Pending),
            "active" => Some(Membership::Active),
            "ejected" => Some(Membership::Ejected),
            _ => None,
        }
    }
}

/// Hub side: a member's subtree the hub accepted and relays to every member
/// (slice 1). `share_id` is the MEMBER's share id (the hub holds mirrors of
/// it); `root_doc` is the mirror root, filed under `<hub root>/<member>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubPublication {
    pub share_id: Uuid,
    pub member_contact: Uuid,
    pub root_doc: Uuid,
    pub published_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShareTrust {
    /// Remote proposals park red for review (default).
    Review,
    /// Trusted: remote edits apply immediately as flagged yellows; reds
    /// (overlaps, gone targets) still park.
    Yellow,
    /// Maintainer: remote edits go through the gate's normal scoring — clean
    /// ops land GREEN with no review annotation; conflicts still yellow/red.
    /// Every op is ledgered with a pre-image (revertible from history) and
    /// surfaces in the owner's activity feed. Trust the person, keep the
    /// receipt. Human-set only, never over MCP.
    Green,
}

impl ShareTrust {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShareTrust::Review => "review",
            ShareTrust::Yellow => "yellow",
            ShareTrust::Green => "green",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "review" => Some(ShareTrust::Review),
            "yellow" => Some(ShareTrust::Yellow),
            "green" | "maintainer" => Some(ShareTrust::Green),
            _ => None,
        }
    }
}

/// One entry in the owner's activity feed: a content edit applied directly
/// by a REMOTE principal (a maintainer-tier share). The notification that
/// replaces the review annotation for trusted peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityItem {
    pub op_id: Uuid,
    pub doc_id: Uuid,
    pub doc_title: String,
    pub principal: Uuid,
    pub principal_name: String,
    pub op_type: String,
    pub epoch: i64,
    pub created_at: String,
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
    pub trust: ShareTrust,
}

/// Owner-side change detector signal (see `BlockStore::change_signature`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangeSignature {
    pub max_epoch: i64,
    pub doc_count: i64,
    pub active_shares: i64,
}

/// One row of the Trash: a deleted subtree root, when it fell, and how many
/// descendants fell with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashEntry {
    pub doc: Doc,
    pub deleted_at: String,
    pub descendants: usize,
}

/// Grantee-side origin + pull cursor for a mirror doc (same UUID as upstream).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mirror {
    pub doc_id: Uuid,
    pub owner: Uuid,
    pub share_id: Uuid,
    pub synced_epoch: i64,
    pub permission: SharePermission,
    /// The owner tends this doc on their side; the grantee shows it and is
    /// refused local tending (avoids two agents editing both copies).
    pub owner_tended: bool,
    /// When the last successful pull touched this mirror (ISO), if ever.
    pub last_pulled_at: Option<String>,
    /// The last pull error for this mirror's share; None once a pull succeeds.
    pub last_error: Option<String>,
    /// The owner's epoch for this doc as of the last pull META (metas always
    /// ship). `> synced_epoch` = content we know exists but haven't landed
    /// (paged out, or the block replace failed): "behind".
    #[serde(default)]
    pub owner_epoch: i64,
    /// Hub relay provenance (slice 1): when the owner we pull from is a hub
    /// relaying someone else's doc, the TRUE owner's pubkey (hex) and the
    /// name the hub knows them by. None = the doc belongs to the contact we
    /// pull from. Relayed docs are read-only in this slice.
    #[serde(default)]
    pub origin_owner: Option<String>,
    #[serde(default)]
    pub origin_owner_name: Option<String>,
}

/// Invites v2: a share OFFER received from a contact over the wire (recipient
/// side). Durable until accepted / declined / expired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareOffer {
    pub id: Uuid,
    pub from_contact: Uuid,
    /// Owner's pubkey (hex) — what we dial to redeem.
    pub owner_node: String,
    /// The owner's share id.
    pub share_id: Uuid,
    pub root_title: String,
    pub permission: SharePermission,
    /// The invite secret; redeeming uses it exactly like a pasted link.
    pub secret: String,
    pub state: ShareOfferState,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareOfferState {
    Open,
    Accepted,
    Declined,
    Expired,
}

impl ShareOfferState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShareOfferState::Open => "open",
            ShareOfferState::Accepted => "accepted",
            ShareOfferState::Declined => "declined",
            ShareOfferState::Expired => "expired",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(ShareOfferState::Open),
            "accepted" => Some(ShareOfferState::Accepted),
            "declined" => Some(ShareOfferState::Declined),
            "expired" => Some(ShareOfferState::Expired),
            _ => None,
        }
    }
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

/// A block on the federation wire / in a mirror replace: the projection
/// fields only — provenance is assigned by the receiving side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorBlock {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub order_key: String,
    pub block_type: BlockType,
    pub content: String,
    #[serde(default)]
    pub refers_to: Option<Uuid>,
}

/// A proposal shipped upstream (grantee-side bookkeeping, #60).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutboundProposal {
    pub id: Uuid,
    pub doc_id: Uuid,
    pub share_id: Uuid,
    pub owner: Uuid,
    pub op_ids: Vec<Uuid>,
    pub note: String,
    pub state: String,
    pub created_at: String,
}

/// Owner-side status of one ledger op, for federation status checks (#60).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpStatus {
    pub op_id: Uuid,
    pub principal: Uuid,
    pub applied: bool,
    /// Open annotation state: "open" | "accepted" | "declined"; None when
    /// the op never had one.
    pub review: Option<String>,
    /// The op's provenance refs (hub slice 2: a hub asking on a member's
    /// behalf is matched by the `via hub` refs, not by principal).
    #[serde(default)]
    pub source_refs: Vec<String>,
}

/// Hub side (slice 2): a proposal the hub forwarded to a doc's true owner on
/// a member's behalf. `op_id` is the OWNER's op id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubForward {
    pub op_id: Uuid,
    pub owner_contact: Uuid,
    pub member_contact: Uuid,
    pub owner_share: Uuid,
    pub doc_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HubTransferState {
    Offered,
    Accepted,
    Declined,
    Done,
}

impl HubTransferState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HubTransferState::Offered => "offered",
            HubTransferState::Accepted => "accepted",
            HubTransferState::Declined => "declined",
            HubTransferState::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "offered" => Some(HubTransferState::Offered),
            "accepted" => Some(HubTransferState::Accepted),
            "declined" => Some(HubTransferState::Declined),
            "done" => Some(HubTransferState::Done),
            _ => None,
        }
    }
}

/// Hub side (slice 2): a member's offer to hand a subtree over to the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubTransfer {
    pub id: Uuid,
    pub member_contact: Uuid,
    pub root_doc: Uuid,
    pub title: String,
    pub doc_count: i64,
    pub state: HubTransferState,
    pub at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferDirection {
    /// I gave the subtree away; my copy is a mirror of the counterparty now.
    Out,
    /// I took the subtree over.
    In,
}

impl TransferDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferDirection::Out => "out",
            TransferDirection::In => "in",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "out" => Some(TransferDirection::Out),
            "in" => Some(TransferDirection::In),
            _ => None,
        }
    }
}

/// Both sides (slice 2): one ownership transfer this instance took part in.
/// `state` is "offered" until the hand-over happened, then "done".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocTransfer {
    pub id: Uuid,
    pub root_doc: Uuid,
    pub counterparty: Uuid,
    pub direction: TransferDirection,
    pub state: String,
    pub at: String,
}
