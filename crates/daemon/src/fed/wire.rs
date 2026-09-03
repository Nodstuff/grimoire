//! Wire types: frames, requests, responses, typed refusals, invite tickets.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ALPN: &[u8] = b"grimoire/fed/0";
/// Long-lived hot-session bridge streams (#66).
pub const HOT_ALPN: &[u8] = b"grimoire/hot/0";
pub const PROTOCOL_VERSION: u32 = 0;
/// Frame cap: a single frame is never larger than this, in either direction.
pub const MAX_FRAME: usize = 32 * 1024 * 1024;
/// Pull paging: the owner stops adding `changed` docs once their serialized
/// blocks pass this (at least one doc always ships), sets `more`, and the
/// grantee pulls again. Well under MAX_FRAME so metas + JSON overhead fit.
pub const PULL_BUDGET: usize = 4 * 1024 * 1024;

/// Invite secrets are stored hashed (share_invites.secret_hash); the secret
/// itself only ever exists inside the grimoire:// link. Mint (#57) and
/// redeem must agree on this.
pub fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame<T> {
    pub v: u32,
    #[serde(flatten)]
    pub msg: T,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The only request an unknown peer may make.
    Redeem { secret: String, petname: String },
    /// Authenticated no-op: proves the allowlist path end to end.
    Ping,
    /// The read protocol (#58). Empty cursors = initial snapshot. The owner
    /// answers with metas for every in-share doc (renames/moves don't bump
    /// epochs, so metas always ship), full blocks for docs the cursor has
    /// never seen or that changed, and removals for cursor docs that left
    /// the share.
    Pull {
        share: String,
        /// (doc_id, synced_epoch) for every mirror doc we hold.
        cursors: Vec<(String, i64)>,
    },
    /// The write-back (#60). Requires a `propose` share. Ops PARK on the
    /// owner as unapplied reds — a remote edit is a proposal request, never
    /// a write; the owner's review queue is where it becomes real. Trust
    /// tiers (auto-applying yellows for trusted peers) are a later policy
    /// upgrade, not a protocol change.
    Propose {
        share: String,
        doc: String,
        ops: Vec<grimoire_store::OpInput>,
        note: String,
        /// The grantee's synced epoch for the doc — the base the gate scores
        /// against on trusted (yellow) shares. Absent = park regardless.
        #[serde(default)]
        base_epoch: Option<i64>,
        /// Retry-safety: the same id returns the original outcome instead of
        /// parking twice (mirrors the MCP propose contract).
        #[serde(default)]
        request_id: Option<String>,
        /// Hub (slice 2): a hub forwarding a MEMBER's proposal on a doc it
        /// relays. The owner accepts this only from a contact flagged as a
        /// hub, and files the proposal under the member's principal — the
        /// hub carries, never decides. A plain peer setting it is refused.
        #[serde(default)]
        on_behalf_of: Option<OnBehalfOf>,
    },
    /// Status of previously proposed ops (only your own are disclosed).
    ProposalStatus { op_ids: Vec<String> },
    /// Hot-session queries (#66): is the doc live / start one remotely.
    HotStatus {
        share: String,
        doc: String,
    },
    /// Requires propose permission. Starting remotely seeds from the
    /// GRANTEE's mirror, so the owner only CREATES a session when the
    /// grantee's copy is at the owner's current epoch (`base_epoch`); a
    /// behind copy is refused with `RefusalCode::StaleBase` — otherwise the
    /// flatten would land the stale text over the owner's newer edits.
    /// Joining an already-live session needs no epoch.
    HotStart {
        share: String,
        doc: String,
        #[serde(default)]
        base_epoch: Option<i64>,
    },
    /// Cold-editor heartbeat from a grantee (auto-hot). Requires propose —
    /// only someone who could edit can escalate.
    EditPing {
        share: String,
        doc: String,
        key: String,
    },
    /// End + daemon-flatten a hot session (#67). Requires propose.
    HotEnd {
        share: String,
        doc: String,
    },
    /// The comment channel (#64, ADR 0003 §1). Applies DIRECTLY — comments
    /// are conversation, not content; block-type is restricted owner-side.
    /// `view` permission suffices: commenting is not editing.
    Comment {
        share: String,
        target_block: String,
        text: String,
        #[serde(default)]
        reply_to: Option<String>,
    },
    /// The nudge (OWNER → grantee): something you hold a mirror of just
    /// changed / appeared / went live — pull now instead of waiting for the
    /// sweep. Best-effort; a missed nudge is caught by the poll. The grantee
    /// only accepts it from a contact it holds that share from.
    Notify {
        share: String,
        doc: String,
        title: String,
        kind: NotifyKind,
    },
    /// Invites v2 (OWNER → contact): "I'd like to share this with you" — the
    /// invite delivered over the wire instead of as a pasted link. The
    /// recipient stores it as a durable share offer and accepts or declines
    /// in its app; accepting redeems `secret` exactly like a link would. Only
    /// accepted from a known, non-revoked contact.
    Offer {
        share: String,
        root_title: String,
        permission: String,
        secret: String,
        expires_at: String,
    },
    /// The coalesced nudge: everything that changed in ONE share during one
    /// detector tick, in one dial. Replaces N `Notify` dials with one; old
    /// peers still receive `Notify` (see `loops::send_nudges`).
    NotifyBatch {
        share: String,
        items: Vec<NotifyItem>,
    },
    /// Hub (slice 1): an admin acting on the hub's membership from their OWN
    /// Grimoire. Authorized iff the caller's contact row on the hub has role
    /// admin. Refused with `Unsupported` by a peer that is not a hub.
    HubAdmin { action: HubAction },
    /// Hub (slice 1): "what am I to you?" — any contact (even pending) may
    /// ask, so a member waiting for approval can see that they are waiting.
    HubStatus,
    /// Hub (slice 2, MEMBER → hub): "take ownership of this subtree of
    /// mine". Recorded for an admin to accept or decline; nothing moves yet.
    TransferOffer {
        root_doc: String,
        title: String,
        doc_count: usize,
    },
    /// Hub (slice 2, hub → member): an admin accepted the transfer. The
    /// member flips the subtree to mirrors of the hub and answers
    /// `TransferReady` with the share the hub pulls it through — or refuses
    /// `Busy` while any doc in it is live or has edits waiting for review.
    TransferAccepted { root_doc: String },
    /// Hub (slice 2): reversal seam — an admin handing a subtree back.
    /// Typed so peers can answer `Unsupported` today.
    TransferBack { root_doc: String },
    /// 0.7.2 (member → hub): "I flipped this folder to your mirrors and my
    /// `TransferReady` reply may not have reached you — pull it through
    /// `share_id` and take it over." Sent by the member's sweep while a
    /// flipped transfer is unacknowledged; the hub answers `Noted` once the
    /// take-over is done (the member then stops asking), `Busy` while it is
    /// in progress. A 0.7.1 hub answers `BadRequest` (unknown request):
    /// the member simply asks again next sweep.
    TransferReady { root_doc: String, share_id: String },
}

/// Hub (slice 2): who a forwarded proposal is really from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OnBehalfOf {
    pub pubkey: String,
    pub name: String,
}

/// What a hub admin can do over the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HubAction {
    ListMembers,
    /// Pending → active: the hub mints a `propose` share of its root to the
    /// member and offers it.
    Approve { contact_id: String },
    /// Active/pending → ejected: their hub-root share and every publication
    /// of theirs are revoked; the contact is blocked.
    Eject { contact_id: String },
    /// "member" | "admin".
    SetRole { contact_id: String, role: String },
    /// Mint a one-time invite (propose, hub root) and return the link — how
    /// an admin onboards someone without touching the hub box.
    Invite,
    /// Slice 2: open proposals on HUB-OWNED docs (members' edits waiting).
    ReviewQueue,
    /// Slice 2: resolve one of them. `decision` = "accept" | "decline".
    Resolve { annotation_id: String, decision: String },
    /// Slice 2: transfers members offered the hub, every state.
    ListTransfers,
    /// Slice 2: take the subtree over (dials the member; completes off-thread).
    AcceptTransfer { id: String },
    DeclineTransfer { id: String },
}

/// Slice 2: one transfer offer as the hub reports it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubTransferInfo {
    pub id: String,
    pub member_contact: String,
    pub member: String,
    pub root_doc: String,
    pub title: String,
    pub doc_count: i64,
    /// offered | accepted | declined | done
    pub state: String,
    pub at: String,
}

/// One row of a hub's member list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubMember {
    pub contact_id: String,
    pub petname: String,
    pub pubkey: String,
    /// "member" | "admin"
    pub role: String,
    /// "pending" | "active" | "ejected"
    pub membership: String,
    pub paired_at: String,
    /// Subtrees this member has published to the hub.
    pub publications: Vec<HubPublicationInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubPublicationInfo {
    pub share_id: String,
    pub root_doc: String,
    pub root_title: String,
    pub doc_count: usize,
    pub published_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotifyItem {
    pub doc: String,
    pub title: String,
    pub kind: NotifyKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotifyKind {
    LiveStarted,
    DocAdded,
    DocChanged,
}

impl NotifyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NotifyKind::LiveStarted => "live_started",
            NotifyKind::DocAdded => "doc_added",
            NotifyKind::DocChanged => "doc_changed",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct WireDocMeta {
    pub id: String,
    /// Parent doc id if the parent is inside the shared subtree; None for
    /// the share root (its real parent is private to the owner).
    pub parent: Option<String>,
    pub title: String,
    pub epoch: i64,
    /// The owner tends this doc (a gardener over it or an ancestor). The
    /// grantee shows it and refuses local tending. Default false so pre-field
    /// peers still deserialize.
    #[serde(default)]
    pub tended: bool,
    /// Hub relay provenance (slice 1): set ONLY by a hub, ONLY on docs it
    /// relays for a member — the true owner's pubkey (hex) and the name the
    /// hub knows them by. None for the serving peer's own docs.
    #[serde(default)]
    pub origin_owner: Option<String>,
    #[serde(default)]
    pub origin_owner_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WireDoc {
    pub meta: WireDocMeta,
    pub blocks: Vec<grimoire_store::MirrorBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Redeemed {
        share_id: String,
        root_doc: String,
        root_title: String,
        permission: String,
        /// The owner's self-chosen display name — the grantee's default
        /// petname for them (renameable, like any petname).
        owner_name: String,
        /// Hub (slice 1): the owner is a hub. Absent from pre-hub peers.
        #[serde(default)]
        is_hub: bool,
        /// Hub: the redeemer's standing — "pending" (no share yet; wait for
        /// an admin) or "active". None from a plain owner.
        #[serde(default)]
        membership: Option<String>,
        /// Hub: "member" | "admin". None from a plain owner.
        #[serde(default)]
        role: Option<String>,
    },
    Pong,
    /// Nudge acknowledged.
    Noted,
    Pulled {
        metas: Vec<WireDocMeta>,
        changed: Vec<WireDoc>,
        removed: Vec<String>,
        /// The owner capped `changed` to stay under the frame budget; docs
        /// still behind were left out of `changed` (their metas still ship).
        /// The grantee pulls again with updated cursors until this is false.
        #[serde(default)]
        more: bool,
    },
    /// Ops parked on the owner; these ids are the status handle.
    Proposed {
        op_ids: Vec<String>,
    },
    ProposalStatuses {
        statuses: Vec<grimoire_store::OpStatus>,
    },
    Commented {
        block_id: String,
    },
    HotStatusIs {
        hot: bool,
        frozen_epoch: Option<i64>,
        #[serde(default)]
        editors: usize,
        /// Whether THIS peer may write in the session (today: has `propose`).
        /// None from pre-field owners → the grantee UI falls back to the share
        /// permission.
        #[serde(default)]
        can_write: Option<bool>,
    },
    HotStarted {
        frozen_epoch: i64,
        seed: bool,
    },
    HotEnded {
        flattened_ops: usize,
    },
    Refused {
        reason: String,
        /// Machine-readable cause; the grantee's loops branch on THIS, never
        /// on `reason` text. Absent from pre-code peers → `Other`.
        #[serde(default)]
        code: RefusalCode,
    },
    /// Hub (slice 1): the member list, admins only.
    HubMembers { members: Vec<HubMember> },
    /// Hub (slice 1): a minted invite link (admin `Invite`).
    HubInvite { link: String },
    /// Hub (slice 1): the caller's standing at this hub.
    HubStatusIs {
        name: String,
        role: String,
        membership: String,
        members: usize,
        pending: usize,
    },
    /// Hub (slice 2): open proposals on hub-owned docs, in the same shape
    /// as the local `/api/queue` items (admins only).
    HubQueue { items: Vec<serde_json::Value> },
    /// Hub (slice 2): transfer offers (admins only).
    HubTransfers { transfers: Vec<HubTransferInfo> },
    /// Hub (slice 2): the hub recorded a member's transfer offer.
    TransferOffered { id: String },
    /// Hub (slice 2, member → hub): the subtree is flipped to mirrors of the
    /// hub; pull it through this share (the member's `propose` share to the
    /// hub) and take it over.
    TransferReady { share_id: String },
}

impl Response {
    pub fn refused(code: RefusalCode, reason: impl Into<String>) -> Self {
        Response::Refused {
            reason: reason.into(),
            code,
        }
    }
}

/// Why the owner said no. Stable across versions: add variants, never rename.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// Pubkey is not a (non-revoked) contact.
    UnknownPeer,
    /// Invite secret unknown, already redeemed, expired, or from a revoked contact.
    InviteInvalid,
    /// The share row is revoked: the grantee should drop its mirrors.
    ShareRevoked,
    /// The share exists but is not active (e.g. offered, never redeemed).
    ShareInactive,
    /// Share does not exist, is bound to someone else, or the doc is outside it.
    NotInShare,
    /// Write requested on a `view` share.
    ViewOnly,
    /// The doc is in a live session (P2.3); retry after it ends.
    DocHot,
    /// The caller's copy of the doc is behind the owner's epoch: pull first,
    /// then retry (a seed from a stale mirror would overwrite newer edits).
    StaleBase,
    /// Authenticated and in the share, but this action is not theirs to take
    /// (e.g. ending a live session someone else started).
    NotAllowed,
    /// The owner does not understand this request variant (older peer).
    Unsupported,
    /// Malformed ids / payload.
    BadRequest,
    /// Protocol version mismatch.
    Version,
    /// Hub (slice 1): the doc is relayed for another member; the hub is not
    /// its home. Slice 2 forwards proposals to the owner; live sessions and
    /// comments on relayed docs still answer this.
    RelayedReadOnly,
    /// Hub (slice 2): a transfer was refused because a doc in the subtree is
    /// in a live session or has edits waiting for review. The reason names
    /// the doc; retry once it is idle.
    Busy,
    /// 0.7.2: the share's root is already a mirror held from a DIFFERENT
    /// contact and this join may not rebind it (raised grantee-side by
    /// `client::join_at_from`; never retried — the invite cannot succeed
    /// until the person resolves the conflict).
    RootConflict,
    #[default]
    #[serde(other)]
    Other,
}

/// A handler-side refusal carrying its code; `server::refuse` downcasts it.
#[derive(Debug)]
pub struct Refusal {
    pub code: RefusalCode,
    pub reason: String,
}

impl Refusal {
    pub fn new(code: RefusalCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Refusal {}

/// The invite ticket: everything a grantee needs to join. The node id is
/// enough to dial — iroh discovery resolves it — so no relay hint in v0. The
/// secret is the trust anchor and only ever exists here and in flight.
///
/// Link format (v2): `grimoire://join/<node-id-hex>/<secret>` — ~110 chars,
/// readable aloud. The share id is not in the link: the owner looks the
/// invite up by the secret's hash alone. Secrets are 16 random bytes in
/// lowercase RFC 4648 base32 without padding (26 chars, no ambiguous
/// case). The v1 form `grimoire://join/<base64url(json)>` still parses so
/// links minted before this change keep working for their 7-day life.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct Ticket {
    pub v: u32,
    /// Owner's endpoint id (hex ed25519 pubkey).
    pub node: String,
    /// Owner's share id — carried by v1 links only; empty for v2 (the owner
    /// resolves the share from the secret).
    #[serde(default)]
    pub share: String,
    pub secret: String,
}

const LINK_PREFIX: &str = "grimoire://join/";

/// 0.7.2 hub marker: an invite minted BY A HUB for its root carries this
/// prefix on the secret. The grantee sets `is_hub` on the new contact only
/// when the ticket it redeemed carries the marker — the owner's `Redeemed`
/// reply alone never does (a plain owner could otherwise claim to be a hub
/// and have its `on_behalf_of` proposals honoured). Living inside the secret
/// keeps the link format unchanged: a 0.7.1 peer parses and redeems it as
/// any other secret (the hash covers the whole string).
pub const HUB_SECRET_PREFIX: &str = "hub-";

/// A fresh invite secret: 16 random bytes, base32 lowercase, no padding.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS entropy");
    data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
}

/// A secret for a hub-root invite (`HUB_SECRET_PREFIX` + `new_secret`).
pub fn new_hub_secret() -> String {
    format!("{HUB_SECRET_PREFIX}{}", new_secret())
}

impl Ticket {
    pub fn new(node: String, share: String, secret: String) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            node,
            share,
            secret,
        }
    }

    /// Whether this invite was minted by a hub for its root (see
    /// `HUB_SECRET_PREFIX`). Only such a ticket may flag the owner `is_hub`.
    pub fn is_hub_invite(&self) -> bool {
        self.secret.starts_with(HUB_SECRET_PREFIX)
    }

    /// The v2 short link.
    pub fn to_link(&self) -> String {
        format!("{LINK_PREFIX}{}/{}", self.node, self.secret)
    }

    pub fn parse(link: &str) -> Result<Self> {
        let encoded = link
            .trim()
            .trim_end_matches('/')
            .strip_prefix(LINK_PREFIX)
            .context("not a grimoire://join/ link")?;
        // v2: <64 hex>/<secret>
        if let Some((node, secret)) = encoded.split_once('/') {
            let node_ok = node.len() == 64 && node.chars().all(|c| c.is_ascii_hexdigit());
            let secret_ok = !secret.is_empty()
                && secret.len() <= 128
                && secret.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !node_ok || !secret_ok {
                anyhow::bail!("link is malformed (expected grimoire://join/<id>/<secret>)");
            }
            return Ok(Ticket::new(node.to_lowercase(), String::new(), secret.to_string()));
        }
        // v1: base64url(json)
        let json = data_encoding::BASE64URL_NOPAD
            .decode(encoded.as_bytes())
            .context("link is not valid base64url")?;
        let ticket: Ticket = serde_json::from_slice(&json).context("link is not a valid ticket")?;
        if ticket.v != PROTOCOL_VERSION {
            anyhow::bail!(
                "link is protocol version {} (this instance speaks {})",
                ticket.v,
                PROTOCOL_VERSION
            );
        }
        Ok(ticket)
    }
}
