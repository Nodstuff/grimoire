//! Wire types: frames, requests, responses, typed refusals, invite tickets.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ALPN: &[u8] = b"grimoire/fed/0";
/// Long-lived hot-session bridge streams (#66).
pub const HOT_ALPN: &[u8] = b"grimoire/hot/0";
pub const PROTOCOL_VERSION: u32 = 0;
/// Frame cap. Snapshots (#58) will stream doc-by-doc, not grow this.
pub const MAX_FRAME: usize = 32 * 1024 * 1024;

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
    /// GRANTEE's mirror (same content — the epoch is the owner's current).
    HotStart {
        share: String,
        doc: String,
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
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct WireDocMeta {
    pub id: String,
    /// Parent doc id if the parent is inside the shared subtree; None for
    /// the share root (its real parent is private to the owner).
    pub parent: Option<String>,
    pub title: String,
    pub epoch: i64,
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
    Pulled {
        metas: Vec<WireDocMeta>,
        changed: Vec<WireDoc>,
        removed: Vec<String>,
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

/// The invite ticket: everything a grantee needs to join, serialized as a
/// `grimoire://join/<base64url(json)>` link. The node id is enough to dial —
/// iroh discovery resolves it — so no relay hint in v0. The secret is the
/// trust anchor and only ever exists here and in flight.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Ticket {
    pub v: u32,
    /// Owner's endpoint id (hex ed25519 pubkey).
    pub node: String,
    pub share: String,
    pub secret: String,
}

const LINK_PREFIX: &str = "grimoire://join/";

impl Ticket {
    pub fn new(node: String, share: String, secret: String) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            node,
            share,
            secret,
        }
    }

    pub fn to_link(&self) -> String {
        let json = serde_json::to_vec(self).expect("ticket serializes");
        format!(
            "{LINK_PREFIX}{}",
            data_encoding::BASE64URL_NOPAD.encode(&json)
        )
    }

    pub fn parse(link: &str) -> Result<Self> {
        let encoded = link
            .trim()
            .strip_prefix(LINK_PREFIX)
            .context("not a grimoire://join/ link")?;
        let json = data_encoding::BASE64URL_NOPAD
            .decode(encoded.as_bytes())
            .context("ticket is not valid base64url")?;
        let ticket: Ticket = serde_json::from_slice(&json).context("ticket is not valid JSON")?;
        if ticket.v != PROTOCOL_VERSION {
            anyhow::bail!(
                "ticket is protocol version {} (this instance speaks {})",
                ticket.v,
                PROTOCOL_VERSION
            );
        }
        Ok(ticket)
    }
}
