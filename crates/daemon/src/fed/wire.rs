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

/// A fresh invite secret: 16 random bytes, base32 lowercase, no padding.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS entropy");
    data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
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
