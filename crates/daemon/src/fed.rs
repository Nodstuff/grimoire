//! Federation endpoint (ADR 0002 decisions 5/6/7, ticket #56).
//!
//! An iroh endpoint on its own ALPN, alongside — never inside — the HTTP
//! surfaces. Deny-by-default: a connection from a pubkey that is not a
//! non-revoked contact may do exactly one thing, redeem an invite secret;
//! every other request from an unknown peer is refused. Known contacts get
//! the (for now tiny) authenticated protocol. /api and /admin are not
//! reachable here by construction — this module only ever touches the store
//! through the specific calls below.
//!
//! Wire format: one request per bi-stream. The opener writes one JSON frame
//! and finishes; the acceptor replies with one JSON frame. Every frame
//! carries `v` (protocol version) — refused loudly on mismatch, so the
//! version dance is cheap now instead of impossible later.

use anyhow::{Context, Result};
use grimoire_store::{BlockStore, SqliteStore};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

pub const ALPN: &[u8] = b"grimoire/fed/0";
pub const PROTOCOL_VERSION: u32 = 0;
/// Frame cap. Snapshots (#58) will stream doc-by-doc, not grow this.
const MAX_FRAME: usize = 32 * 1024 * 1024;

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
        /// Retry-safety: the same id returns the original outcome instead of
        /// parking twice (mirrors the MCP propose contract).
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Status of previously proposed ops (only your own are disclosed).
    ProposalStatus { op_ids: Vec<String> },
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
    Refused {
        reason: String,
    },
}

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

/// Bind the production endpoint: n0 relays + discovery, our identity.
pub async fn bind(secret: [u8; 32]) -> Result<Endpoint> {
    Endpoint::builder(presets::N0)
        .secret_key(SecretKey::from_bytes(&secret))
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .context("binding federation endpoint")
}

/// Accept loop. Spawned once per daemon; lives until the endpoint closes.
/// Bounded propose dedupe: request_id → original outcome (retry safety).
type Dedupe = Arc<Mutex<std::collections::HashMap<String, Response>>>;

pub async fn serve(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    tracing::info!("federation endpoint listening (node id {})", endpoint.id());
    let dedupe: Dedupe = Default::default();
    while let Some(incoming) = endpoint.accept().await {
        let store = store.clone();
        let dedupe = dedupe.clone();
        tokio::spawn(async move {
            let conn = match incoming.accept() {
                Ok(accepting) => match accepting.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::debug!("federation handshake failed: {e:#}");
                        return;
                    }
                },
                Err(e) => {
                    tracing::debug!("federation accept failed: {e:#}");
                    return;
                }
            };
            let peer = conn.remote_id().to_string();
            if let Err(e) = handle_conn(conn, &peer, store, dedupe).await {
                tracing::debug!(peer, "federation connection ended: {e:#}");
            }
        });
    }
}

async fn handle_conn(
    conn: iroh::endpoint::Connection,
    peer: &str,
    store: Arc<Mutex<SqliteStore>>,
    dedupe: Dedupe,
) -> Result<()> {
    // authed = peer is a non-revoked contact. Checked once per request, not
    // once per connection: a successful redeem upgrades the session, and a
    // mid-session revoke takes effect on the next request.
    loop {
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            // peer closed: normal end of conversation
            Err(_) => return Ok(()),
        };
        let raw = recv.read_to_end(MAX_FRAME).await?;
        let response = match serde_json::from_slice::<Frame<Request>>(&raw) {
            Err(e) => Response::Refused {
                reason: format!("bad frame: {e}"),
            },
            Ok(frame) if frame.v != PROTOCOL_VERSION => Response::Refused {
                reason: format!(
                    "protocol version {} not supported (this instance speaks {})",
                    frame.v, PROTOCOL_VERSION
                ),
            },
            Ok(frame) => dispatch(frame.msg, peer, &store, &dedupe),
        };
        let out = serde_json::to_vec(&Frame {
            v: PROTOCOL_VERSION,
            msg: response,
        })?;
        send.write_all(&out).await?;
        send.finish()?;
    }
}

fn dispatch(req: Request, peer: &str, store: &Arc<Mutex<SqliteStore>>, dedupe: &Dedupe) -> Response {
    let mut store = store.lock().unwrap_or_else(|p| p.into_inner());
    let contact = match store.contact_by_pubkey(peer) {
        Ok(c) => c.filter(|c| !c.revoked),
        Err(e) => {
            return Response::Refused {
                reason: format!("store error: {e}"),
            };
        }
    };
    match req {
        Request::Redeem { secret, petname } => {
            match store.redeem_invite(&hash_secret(&secret), peer, &petname) {
                Ok((contact, share)) => {
                    tracing::info!(
                        peer,
                        petname = contact.petname,
                        share = %share.id,
                        "invite redeemed; contact paired"
                    );
                    let root_title = store
                        .get_doc(share.root_doc)
                        .map(|d| d.title)
                        .unwrap_or_default();
                    let owner_name = store
                        .list_principals()
                        .ok()
                        .and_then(|ps| {
                            ps.into_iter()
                                .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
                        })
                        .map(|p| p.display_name)
                        .unwrap_or_else(|| "owner".into());
                    Response::Redeemed {
                        share_id: share.id.to_string(),
                        root_doc: share.root_doc.to_string(),
                        root_title,
                        permission: share.permission.as_str().to_string(),
                        owner_name,
                    }
                }
                Err(e) => {
                    tracing::warn!(peer, "invite redeem refused: {e}");
                    Response::Refused {
                        reason: e.to_string(),
                    }
                }
            }
        }
        Request::Ping => match contact {
            Some(c) => {
                tracing::debug!(peer, petname = c.petname, "ping");
                Response::Pong
            }
            None => {
                tracing::warn!(peer, "unauthenticated request refused");
                Response::Refused {
                    reason: "unknown peer: redeem an invite first".into(),
                }
            }
        },
        Request::Pull { share, cursors } => {
            let Some(contact) = contact else {
                tracing::warn!(peer, "unauthenticated pull refused");
                return Response::Refused {
                    reason: "unknown peer: redeem an invite first".into(),
                };
            };
            match handle_pull(&store, &contact, &share, &cursors) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(peer, share, "pull refused: {e}");
                    Response::Refused {
                        reason: e.to_string(),
                    }
                }
            }
        }
        Request::Propose {
            share,
            doc,
            ops,
            note,
            request_id,
        } => {
            let Some(contact) = contact else {
                tracing::warn!(peer, "unauthenticated propose refused");
                return Response::Refused {
                    reason: "unknown peer: redeem an invite first".into(),
                };
            };
            // retry safety: the same request_id returns the original outcome
            let dedupe_key = request_id.map(|r| format!("{peer}:{r}"));
            if let Some(key) = &dedupe_key {
                let cache = dedupe.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(prior) = cache.get(key) {
                    return prior.clone();
                }
            }
            let res = match handle_propose(&mut store, &contact, &share, &doc, ops, &note) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(peer, share, doc, "propose refused: {e}");
                    Response::Refused {
                        reason: e.to_string(),
                    }
                }
            };
            if let (Some(key), Response::Proposed { .. }) = (&dedupe_key, &res) {
                let mut cache = dedupe.lock().unwrap_or_else(|p| p.into_inner());
                if cache.len() >= 512 {
                    cache.clear(); // crude but bounded
                }
                cache.insert(key.clone(), res.clone());
            }
            res
        }
        Request::ProposalStatus { op_ids } => {
            let Some(contact) = contact else {
                return Response::Refused {
                    reason: "unknown peer: redeem an invite first".into(),
                };
            };
            let ids: Vec<uuid::Uuid> = op_ids.iter().filter_map(|s| s.parse().ok()).collect();
            match store.op_statuses(&ids) {
                // disclose only the asker's own ops
                Ok(statuses) => Response::ProposalStatuses {
                    statuses: statuses
                        .into_iter()
                        .filter(|s| s.principal == contact.principal)
                        .collect(),
                },
                Err(e) => Response::Refused {
                    reason: e.to_string(),
                },
            }
        }
    }
}

/// Owner side of the write-back (#60): authorize, then PARK. The gate's red
/// path preserves the payload verbatim; accepting applies at the then-current
/// epoch; declining never touches the doc. Proposer ≠ approver holds because
/// the parking principal is the remote contact.
fn handle_propose(
    store: &mut grimoire_store::SqliteStore,
    contact: &grimoire_store::Contact,
    share_id: &str,
    doc_id: &str,
    ops: Vec<grimoire_store::OpInput>,
    note: &str,
) -> Result<Response> {
    let share_uuid: uuid::Uuid = share_id.parse().context("bad share id")?;
    let doc_uuid: uuid::Uuid = doc_id.parse().context("bad doc id")?;
    let share = store.get_share(share_uuid)?;
    if share.contact != Some(contact.id) {
        anyhow::bail!("share is not bound to this contact");
    }
    if share.state != grimoire_store::ShareState::Active {
        anyhow::bail!("share is {}", share.state.as_str());
    }
    if share.permission != grimoire_store::SharePermission::Propose {
        anyhow::bail!("share is view-only");
    }
    if !store.docs_in_share(share_uuid)?.iter().any(|d| d.id == doc_uuid) {
        anyhow::bail!("doc is not in this share");
    }
    if ops.is_empty() {
        anyhow::bail!("empty proposal");
    }
    let note = format!("via share from {}: {note}", contact.petname);
    let op_ids = store.park(doc_uuid, contact.principal, ops, &note)?;
    tracing::info!(
        peer = contact.pubkey,
        doc = doc_id,
        ops = op_ids.len(),
        "remote proposal parked for review"
    );
    Ok(Response::Proposed {
        op_ids: op_ids.into_iter().map(|u| u.to_string()).collect(),
    })
}

/// Owner side of the read protocol (#58). The share's recursive containment
/// is the entire universe this peer can see: docs outside it are never
/// enumerated, wikilinks pointing out of it simply won't resolve downstream.
fn handle_pull(
    store: &grimoire_store::SqliteStore,
    contact: &grimoire_store::Contact,
    share_id: &str,
    cursors: &[(String, i64)],
) -> Result<Response> {
    let share_uuid: uuid::Uuid = share_id.parse().context("bad share id")?;
    let share = store.get_share(share_uuid)?;
    if share.contact != Some(contact.id) {
        anyhow::bail!("share is not bound to this contact");
    }
    if share.state != grimoire_store::ShareState::Active {
        anyhow::bail!("share is {}", share.state.as_str());
    }
    let docs = store.docs_in_share(share_uuid)?;
    let in_share: std::collections::HashSet<uuid::Uuid> = docs.iter().map(|d| d.id).collect();
    let cursor_map: std::collections::HashMap<String, i64> = cursors.iter().cloned().collect();

    let mut metas = Vec::new();
    let mut changed = Vec::new();
    for d in &docs {
        let meta = WireDocMeta {
            id: d.id.to_string(),
            parent: d
                .parent_id
                .filter(|p| in_share.contains(p))
                .map(|p| p.to_string()),
            title: d.title.clone(),
            epoch: d.current_epoch,
        };
        let unchanged = cursor_map
            .get(&meta.id)
            .is_some_and(|c| *c >= d.current_epoch);
        if !unchanged {
            let blocks = store
                .doc_blocks_flat(d.id)?
                .into_iter()
                .map(|b| grimoire_store::MirrorBlock {
                    id: b.id,
                    parent_id: b.parent_id,
                    order_key: b.order_key,
                    block_type: b.block_type,
                    content: b.content,
                    refers_to: b.refers_to,
                })
                .collect();
            changed.push(WireDoc {
                meta: meta.clone(),
                blocks,
            });
        }
        metas.push(meta);
    }
    let removed = cursor_map
        .keys()
        .filter(|id| {
            uuid::Uuid::parse_str(id)
                .map(|u| !in_share.contains(&u))
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    Ok(Response::Pulled {
        metas,
        changed,
        removed,
    })
}

/// One request against a remote instance (grantee-side; the pull loop and
/// the redeem flow both come through here).
#[allow(dead_code)] // wired up by the pairing flow (#57) and pull loop (#59)
pub async fn request(
    endpoint: &Endpoint,
    addr: impl Into<EndpointAddr>,
    req: Request,
) -> Result<Response> {
    let conn = endpoint
        .connect(addr, ALPN)
        .await
        .context("dialing federation peer")?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let out = serde_json::to_vec(&Frame {
        v: PROTOCOL_VERSION,
        msg: req,
    })?;
    send.write_all(&out).await?;
    send.finish()?;
    let raw = recv.read_to_end(MAX_FRAME).await?;
    let frame: Frame<Response> = serde_json::from_slice(&raw).context("bad response frame")?;
    Ok(frame.msg)
}

/// The outcome of a completed join, for the UI/CLI.
#[derive(Debug, Serialize)]
pub struct JoinOutcome {
    pub owner: String,
    pub owner_name: String,
    pub root_doc: String,
    pub root_title: String,
    pub permission: String,
}

/// One join attempt (grantee-side): dial the ticket's node, redeem, pair the
/// owner as a contact, and materialize the mirror root — a placeholder doc
/// under the origin UUID that the pull loop (#59) fills with the subtree.
pub async fn join_once(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    ticket: &Ticket,
) -> Result<JoinOutcome> {
    // dial by node id alone; iroh discovery resolves it
    let owner_id: iroh::EndpointId = ticket
        .node
        .parse()
        .context("ticket has a malformed node id")?;
    join_at(endpoint, store, ticket, EndpointAddr::from(owner_id)).await
}

/// join_once with an explicit address (tests, LAN-known peers).
pub async fn join_at(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    ticket: &Ticket,
    addr: EndpointAddr,
) -> Result<JoinOutcome> {
    let my_name = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.list_principals()
            .ok()
            .and_then(|ps| {
                ps.into_iter()
                    .find(|p| p.kind == grimoire_store::PrincipalKind::Human)
            })
            .map(|p| p.display_name)
            .unwrap_or_else(|| "someone".into())
    };
    let res = request(
        endpoint,
        addr,
        Request::Redeem {
            secret: ticket.secret.clone(),
            petname: my_name,
        },
    )
    .await?;
    let Response::Redeemed {
        share_id,
        root_doc,
        root_title,
        permission,
        owner_name,
    } = res
    else {
        anyhow::bail!("owner refused the invite: {res:?}");
    };

    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    let owner_contact = s.pair_contact(&ticket.node, &owner_name)?;
    let root_uuid: uuid::Uuid = root_doc.parse().context("owner sent a bad doc id")?;
    let share_uuid: uuid::Uuid = share_id.parse().context("owner sent a bad share id")?;
    // same UUID = same doc; if a mirror root already exists (re-join), keep it
    if s.get_doc(root_uuid).is_err() {
        let title = if root_title.is_empty() {
            "(shared doc)".to_string()
        } else {
            root_title.clone()
        };
        s.create_doc_with_id(root_uuid, &title, None, owner_contact.principal)?;
    }
    let perm = grimoire_store::SharePermission::parse(&permission)
        .unwrap_or(grimoire_store::SharePermission::View);
    s.upsert_mirror(root_uuid, owner_contact.id, share_uuid, 0, perm)?;
    tracing::info!(
        owner = ticket.node,
        root = root_doc,
        permission,
        "joined share"
    );
    Ok(JoinOutcome {
        owner: ticket.node.clone(),
        owner_name,
        root_doc,
        root_title,
        permission,
    })
}

#[derive(Debug, Serialize, Default)]
pub struct PullSummary {
    pub changed: usize,
    pub removed: usize,
}

/// Pull one share from its owner and apply the response (#59).
pub async fn pull_share(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    addr: EndpointAddr,
    owner: &grimoire_store::Contact,
    share_id: uuid::Uuid,
) -> Result<PullSummary> {
    let cursors: Vec<(String, i64)> = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.list_mirrors()?
            .into_iter()
            .filter(|m| m.share_id == share_id)
            .map(|m| (m.doc_id.to_string(), m.synced_epoch))
            .collect()
    };
    let res = request(
        endpoint,
        addr,
        Request::Pull {
            share: share_id.to_string(),
            cursors,
        },
    )
    .await?;
    let Response::Pulled {
        metas,
        changed,
        removed,
    } = res
    else {
        anyhow::bail!("owner refused pull: {res:?}");
    };

    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    let summary = PullSummary {
        changed: changed.len(),
        removed: removed.len(),
    };

    // 1. create any docs we've never seen, parents before children
    let mut pending: Vec<&WireDoc> = changed.iter().collect();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|wd| {
            let id: uuid::Uuid = match wd.meta.id.parse() {
                Ok(u) => u,
                Err(_) => return false, // malformed: drop
            };
            if s.get_doc(id).is_ok() {
                return false; // exists
            }
            let parent = wd.meta.parent.as_ref().and_then(|p| p.parse().ok());
            // parent not materialized yet → retry next round
            if let Some(p) = parent
                && s.get_doc(p).is_err()
            {
                return true;
            }
            s.create_doc_with_id(id, &wd.meta.title, parent, owner.principal)
                .is_err()
        });
        if pending.len() == before {
            // cycle or missing parent that never arrives: root the rest
            for wd in pending.drain(..) {
                if let Ok(id) = wd.meta.id.parse::<uuid::Uuid>()
                    && s.get_doc(id).is_err()
                {
                    s.create_doc_with_id(id, &wd.meta.title, None, owner.principal)
                        .ok();
                }
            }
        }
    }

    // 2. metas: renames and moves don't bump epochs, so reconcile them always
    for m in &metas {
        let Ok(id) = m.id.parse::<uuid::Uuid>() else {
            continue;
        };
        let Ok(local) = s.get_doc(id) else { continue };
        if local.title != m.title {
            s.rename_doc(id, &m.title).ok();
        }
        let parent: Option<uuid::Uuid> = m.parent.as_ref().and_then(|p| p.parse().ok());
        if local.parent_id != parent {
            s.move_doc(id, parent, None).ok();
        }
    }

    // 3. claim a mirror row for EVERY in-share doc (permission from the
    // share's existing rows) — this is what lets an active share reclaim
    // docs from a superseded one — then land blocks for what changed
    let share_perm = s
        .list_mirrors()?
        .into_iter()
        .find(|m| m.share_id == share_id)
        .map(|m| m.permission)
        .unwrap_or(grimoire_store::SharePermission::View);
    let changed_ids: std::collections::HashSet<&str> =
        changed.iter().map(|wd| wd.meta.id.as_str()).collect();
    for m in &metas {
        let Ok(id) = m.id.parse::<uuid::Uuid>() else {
            continue;
        };
        let cursor = if changed_ids.contains(m.id.as_str()) {
            0 // block replace below sets the real epoch
        } else {
            m.epoch
        };
        s.upsert_mirror(id, owner.id, share_id, cursor, share_perm)?;
    }
    for wd in &changed {
        let Ok(id) = wd.meta.id.parse::<uuid::Uuid>() else {
            continue;
        };
        s.mirror_replace_blocks(id, wd.blocks.clone(), wd.meta.epoch, owner.principal)?;
    }

    // 4. docs that left the share: gone from our view, mirror row dropped
    for id in &removed {
        if let Ok(u) = id.parse::<uuid::Uuid>() {
            s.remove_mirror(u).ok();
            s.delete_doc(u).ok();
        }
    }
    Ok(summary)
}

/// Ship a local edit of a mirror doc upstream as a proposal (#60). The
/// pessimistic mirror never changes here — the edit becomes real locally
/// only when the owner accepts and a pull lands it.
pub async fn propose_upstream(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
    ops: Vec<grimoire_store::OpInput>,
    note: &str,
) -> Result<uuid::Uuid> {
    let (mirror, owner) = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let mirror = s
            .get_mirror(doc_id)?
            .context("doc is not a mirror — edit it directly")?;
        let owner = s
            .list_contacts()?
            .into_iter()
            .find(|c| c.id == mirror.owner)
            .context("mirror's owner contact is gone")?;
        (mirror, owner)
    };
    let owner_id: iroh::EndpointId = owner.pubkey.parse().context("owner pubkey malformed")?;
    let res = request(
        endpoint,
        EndpointAddr::from(owner_id),
        Request::Propose {
            share: mirror.share_id.to_string(),
            doc: doc_id.to_string(),
            ops,
            note: note.to_string(),
            request_id: Some(uuid::Uuid::now_v7().to_string()),
        },
    )
    .await?;
    let Response::Proposed { op_ids } = res else {
        anyhow::bail!("owner refused proposal: {res:?}");
    };
    let ids: Vec<uuid::Uuid> = op_ids.iter().filter_map(|s| s.parse().ok()).collect();
    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    Ok(s.record_outbound_proposal(doc_id, mirror.share_id, owner.id, &ids, note)?)
}

/// Refresh the state of pending outbound proposals from their owners; any
/// accepted content arrives via the normal pull.
pub async fn refresh_outbound(endpoint: &Endpoint, store: &Arc<Mutex<SqliteStore>>) {
    let pending = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.list_outbound_proposals(true).unwrap_or_default()
    };
    for prop in pending {
        let owner = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            s.list_contacts()
                .ok()
                .and_then(|cs| cs.into_iter().find(|c| c.id == prop.owner))
        };
        let Some(owner) = owner.filter(|o| !o.revoked) else {
            continue;
        };
        let Ok(owner_id) = owner.pubkey.parse::<iroh::EndpointId>() else {
            continue;
        };
        let res = request(
            endpoint,
            EndpointAddr::from(owner_id),
            Request::ProposalStatus {
                op_ids: prop.op_ids.iter().map(|u| u.to_string()).collect(),
            },
        )
        .await;
        let Ok(Response::ProposalStatuses { statuses }) = res else {
            continue; // owner offline or refused: stays pending
        };
        // resolved = the annotation is no longer open
        let resolved: Vec<_> = statuses
            .iter()
            .filter(|s| s.review.as_deref() != Some("open"))
            .collect();
        if resolved.len() < prop.op_ids.len() {
            continue; // partially reviewed: wait for the rest
        }
        let accepted = resolved.iter().filter(|s| s.applied).count();
        let state = if accepted == resolved.len() {
            "accepted"
        } else if accepted == 0 {
            "declined"
        } else {
            "mixed"
        };
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.set_outbound_state(prop.id, state).ok();
        tracing::info!(doc = %prop.doc_id, state, "outbound proposal resolved");
    }
}

/// Pull every share we hold mirrors for. Groups mirrors by (owner, share),
/// skips revoked owners, dials by node id (discovery).
pub async fn pull_all_once(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
) -> Vec<(uuid::Uuid, Result<PullSummary>)> {
    let groups: Vec<(grimoire_store::Contact, uuid::Uuid)> = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let mirrors = s.list_mirrors().unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        mirrors
            .into_iter()
            .filter(|m| seen.insert((m.owner, m.share_id)))
            .filter_map(|m| {
                let contacts = s.list_contacts().ok()?;
                let c = contacts.into_iter().find(|c| c.id == m.owner)?;
                (!c.revoked).then_some((c, m.share_id))
            })
            .collect()
    };
    let mut out = Vec::new();
    let mut dead_shares = Vec::new();
    for (owner, share_id) in groups {
        let res = match owner.pubkey.parse::<iroh::EndpointId>() {
            Ok(id) => pull_share(endpoint, store, EndpointAddr::from(id), &owner, share_id).await,
            Err(_) => Err(anyhow::anyhow!("contact has a malformed pubkey")),
        };
        if let Err(e) = &res
            && format!("{e:#}").contains("share is revoked")
        {
            dead_shares.push(share_id);
        }
        out.push((share_id, res));
    }
    // cleanup: docs still claimed only by a revoked share (active shares
    // reclaimed theirs during the pulls above) are gone from our view
    if !dead_shares.is_empty() {
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        for share_id in dead_shares {
            let orphans: Vec<uuid::Uuid> = s
                .list_mirrors()
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.share_id == share_id)
                .map(|m| m.doc_id)
                .collect();
            for doc in orphans {
                tracing::info!(%doc, %share_id, "share revoked upstream; dropping mirror");
                s.remove_mirror(doc).ok();
                s.delete_doc(doc).ok();
            }
        }
    }
    out
}

/// Background sync: external owner edits appear within one interval (#59).
pub async fn pull_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    const PULL_EVERY: std::time::Duration = std::time::Duration::from_secs(120);
    loop {
        tokio::time::sleep(PULL_EVERY).await;
        refresh_outbound(&endpoint, &store).await;
        for (share, res) in pull_all_once(&endpoint, &store).await {
            match res {
                Ok(s) if s.changed > 0 || s.removed > 0 => {
                    tracing::info!(%share, changed = s.changed, removed = s.removed, "pulled");
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(%share, "pull failed (owner offline?): {e:#}"),
            }
        }
    }
}

/// Background retry for joins whose owner was offline (async redeem,
/// ADR 0002 decision 6). Every failure is recorded; success removes the row.
pub async fn join_retry_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
    loop {
        tokio::time::sleep(RETRY_EVERY).await;
        let pending = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            s.list_pending_joins().unwrap_or_default()
        };
        for join in pending {
            let ticket = match Ticket::parse(&join.ticket) {
                Ok(t) => t,
                Err(e) => {
                    // unparseable tickets can never succeed; drop them
                    tracing::warn!("dropping unparseable pending join: {e:#}");
                    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                    s.remove_pending_join(join.id).ok();
                    continue;
                }
            };
            match join_once(&endpoint, &store, &ticket).await {
                Ok(out) => {
                    tracing::info!(root = out.root_doc, "queued join completed");
                    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                    s.remove_pending_join(join.id).ok();
                }
                Err(e) => {
                    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                    s.record_join_attempt(join.id, &format!("{e:#}")).ok();
                }
            }
        }
    }
}

/// Mint a share invite: secret, hash, expiry, link (owner-side, #57).
pub fn mint_invite(
    store: &mut SqliteStore,
    node_id: &str,
    root_doc: uuid::Uuid,
    permission: grimoire_store::SharePermission,
) -> Result<(grimoire_store::Share, String)> {
    let share = store.create_share(root_doc, None, permission, None)?;
    let mut secret_bytes = [0u8; 32];
    getrandom::fill(&mut secret_bytes).expect("OS entropy");
    let secret = hex::encode(secret_bytes);
    let expires = (chrono::Utc::now() + chrono::Duration::days(7))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    store.create_invite(share.id, &hash_secret(&secret), &expires)?;
    let link = Ticket::new(node_id.to_string(), share.id.to_string(), secret).to_link();
    Ok((share, link))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{PrincipalKind, SharePermission};
    use iroh::TransportAddr;

    /// Local-only endpoint: no relays, no discovery, explicit addressing.
    async fn local_endpoint() -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    fn direct_addr(ep: &Endpoint) -> EndpointAddr {
        EndpointAddr::from_parts(
            ep.id(),
            ep.bound_sockets().into_iter().map(TransportAddr::Ip),
        )
    }

    /// Owner store with one doc, one share, one minted invite.
    fn owner_store(secret: &str) -> Arc<Mutex<SqliteStore>> {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = s.create_doc("shared-runbook", None, tom.id).unwrap();
        let share = s
            .create_share(doc.id, None, SharePermission::View, None)
            .unwrap();
        s.create_invite(share.id, &hash_secret(secret), "2099-01-01T00:00:00.000Z")
            .unwrap();
        Arc::new(Mutex::new(s))
    }

    #[tokio::test]
    async fn unknown_peer_is_refused_everything_but_redeem() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        let stranger = local_endpoint().await;
        let res = request(&stranger, addr, Request::Ping).await.unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn redeem_pairs_and_upgrades_the_session() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        let alice = local_endpoint().await;
        let alice_id = alice.id().to_string();

        // wrong secret first: refused, nothing paired
        let res = request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "wrong".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));

        let res = request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        let Response::Redeemed { permission, .. } = res else {
            panic!("expected Redeemed, got {res:?}");
        };
        assert_eq!(permission, "view");

        // paired under alice's real endpoint id
        {
            let s = store.lock().unwrap();
            let c = s.contact_by_pubkey(&alice_id).unwrap().unwrap();
            assert_eq!(c.petname, "alice");
        }

        // session upgraded: authenticated requests now work
        let res = request(&alice, addr.clone(), Request::Ping).await.unwrap();
        assert_eq!(res, Response::Pong);

        // burned: same secret from another peer is refused
        let mallory = local_endpoint().await;
        let res = request(
            &mallory,
            addr,
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "also-alice".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn revoked_contact_is_refused_on_next_request() {
        let store = owner_store("the-secret");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        let alice = local_endpoint().await;
        let alice_id = alice.id().to_string();
        request(
            &alice,
            addr.clone(),
            Request::Redeem {
                secret: "the-secret".into(),
                petname: "alice".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            request(&alice, addr.clone(), Request::Ping).await.unwrap(),
            Response::Pong
        );

        let contact_id = {
            let s = store.lock().unwrap();
            s.contact_by_pubkey(&alice_id).unwrap().unwrap().id
        };
        store.lock().unwrap().revoke_contact(contact_id).unwrap();

        let res = request(&alice, addr, Request::Ping).await.unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[test]
    fn ticket_link_round_trips() {
        let t = Ticket::new("ab".repeat(32), "share-id".into(), "s3cret".into());
        let link = t.to_link();
        assert!(link.starts_with("grimoire://join/"));
        assert_eq!(Ticket::parse(&link).unwrap(), t);
        assert_eq!(Ticket::parse(&format!("  {link}\n")).unwrap(), t); // pasted whitespace
        assert!(Ticket::parse("https://example.com/nope").is_err());
    }

    #[tokio::test]
    async fn join_materializes_mirror_root_and_pairs_both_sides() {
        // owner side: doc + minted invite via the real mint path
        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Team Runbook", None, tom.id).unwrap();
        let owner_ep = local_endpoint().await;
        let owner_id = owner_ep.id().to_string();
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_id,
            doc.id,
            SharePermission::Propose,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone()));

        // grantee side
        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;

        let ticket = Ticket::parse(&link).unwrap();
        let out = join_at(&alice_ep, &alice_store, &ticket, addr)
            .await
            .unwrap();
        assert_eq!(out.owner_name, "tom");
        assert_eq!(out.root_title, "Team Runbook");
        assert_eq!(out.permission, "propose");

        // grantee: owner paired, mirror root exists under the ORIGIN uuid
        {
            let s = alice_store.lock().unwrap();
            let owner_contact = s.contact_by_pubkey(&owner_id).unwrap().unwrap();
            assert_eq!(owner_contact.petname, "tom");
            let mirror = s.get_mirror(doc.id).unwrap().unwrap();
            assert_eq!(mirror.owner, owner_contact.id);
            assert_eq!(mirror.synced_epoch, 0);
            assert_eq!(s.get_doc(doc.id).unwrap().title, "Team Runbook");
        }
        // owner: grantee paired under her real key, share active
        {
            let s = owner_store.lock().unwrap();
            let alice_contact = s
                .contact_by_pubkey(&alice_ep.id().to_string())
                .unwrap()
                .unwrap();
            assert_eq!(alice_contact.petname, "alice");
            let share = s.get_share(share.id).unwrap();
            assert_eq!(share.contact, Some(alice_contact.id));
        }
    }

    #[tokio::test]
    async fn pull_syncs_subtree_edits_renames_moves_and_removals() {
        use grimoire_store::{BlockType, OpInput, OpKind};

        // owner: root with a child doc, each with a block
        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let root = owner_store.create_doc("Runbook", None, tom.id).unwrap();
        let child = owner_store
            .create_doc("Deploys", Some(root.id), tom.id)
            .unwrap();
        let block_op = |content: &str| OpInput {
            kind: OpKind::Insert {
                block_id: uuid::Uuid::now_v7(),
                parent_id: None,
                order_key: "i".into(),
                block_type: BlockType::Paragraph,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        };
        owner_store
            .apply(root.id, 0, tom.id, vec![block_op("root text")])
            .unwrap();
        owner_store
            .apply(child.id, 0, tom.id, vec![block_op("child text")])
            .unwrap();

        let owner_ep = local_endpoint().await;
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_ep.id().to_string(),
            root.id,
            SharePermission::View,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone()));

        // grantee joins, then pulls the snapshot
        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;
        let ticket = Ticket::parse(&link).unwrap();
        join_at(&alice_ep, &alice_store, &ticket, addr.clone())
            .await
            .unwrap();
        let owner_contact = {
            let s = alice_store.lock().unwrap();
            s.list_contacts().unwrap().into_iter().next().unwrap()
        };
        let sum = pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        assert_eq!(sum.changed, 2); // root + child

        {
            let s = alice_store.lock().unwrap();
            let tree = s.read_doc(child.id).unwrap();
            assert_eq!(tree.doc.title, "Deploys");
            assert_eq!(tree.doc.parent_id, Some(root.id));
            assert_eq!(tree.roots[0].block.content, "child text");
            // mirror is read-only at the store layer
            let mut s = s;
            let err = s.apply(child.id, tree.doc.current_epoch, owner_contact.principal,
                vec![block_op("local vandalism")]);
            assert!(matches!(err, Err(grimoire_store::StoreError::InvalidOp(_))));
        }

        // owner: edit root, rename child, add grandchild, then pull again
        {
            let mut s = owner_store.lock().unwrap();
            let epoch = s.get_doc(root.id).unwrap().current_epoch;
            s.apply(root.id, epoch, tom.id, vec![block_op("more root text")])
                .unwrap();
            s.rename_doc(child.id, "Deploy Runbook").unwrap();
            let gc = s.create_doc("Rollbacks", Some(child.id), tom.id).unwrap();
            s.apply(gc.id, 0, tom.id, vec![block_op("rollback text")])
                .unwrap();
        }
        let sum = pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        assert_eq!(sum.changed, 2); // root (edited) + grandchild (new)
        {
            let s = alice_store.lock().unwrap();
            assert_eq!(s.get_doc(child.id).unwrap().title, "Deploy Runbook");
            let gc = s
                .list_docs()
                .unwrap()
                .into_iter()
                .find(|d| d.title == "Rollbacks")
                .expect("grandchild mirrored");
            assert_eq!(gc.parent_id, Some(child.id));
            let root_tree = s.read_doc(root.id).unwrap();
            assert_eq!(root_tree.roots.len(), 2);
        }

        // owner moves child (and its subtree) out of the share
        {
            let mut s = owner_store.lock().unwrap();
            s.move_doc(child.id, None, None).unwrap();
        }
        let sum = pull_share(&alice_ep, &alice_store, addr, &owner_contact, share.id)
            .await
            .unwrap();
        assert_eq!(sum.removed, 2); // child + grandchild left the share
        {
            let s = alice_store.lock().unwrap();
            assert!(s.get_mirror(child.id).unwrap().is_none());
            // soft-deleted locally: no longer in the live listing
            assert!(!s.list_docs().unwrap().iter().any(|d| d.id == child.id));
        }
    }

    #[tokio::test]
    async fn propose_upstream_parks_then_accept_flows_back_via_pull() {
        use grimoire_store::{BlockType, OpInput, OpKind, ReviewDecision};

        let mut owner_store = SqliteStore::open_in_memory().unwrap();
        let tom = owner_store
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap();
        let doc = owner_store.create_doc("Notes", None, tom.id).unwrap();
        let owner_ep = local_endpoint().await;
        let (share, link) = mint_invite(
            &mut owner_store,
            &owner_ep.id().to_string(),
            doc.id,
            SharePermission::Propose,
        )
        .unwrap();
        let owner_store = Arc::new(Mutex::new(owner_store));
        let addr = direct_addr(&owner_ep);
        tokio::spawn(serve(owner_ep, owner_store.clone()));

        let mut alice_store = SqliteStore::open_in_memory().unwrap();
        alice_store
            .create_principal(PrincipalKind::Human, "alice", None)
            .unwrap();
        let alice_store = Arc::new(Mutex::new(alice_store));
        let alice_ep = local_endpoint().await;
        let ticket = Ticket::parse(&link).unwrap();
        join_at(&alice_ep, &alice_store, &ticket, addr.clone())
            .await
            .unwrap();
        let owner_contact = {
            let s = alice_store.lock().unwrap();
            s.list_contacts().unwrap().into_iter().next().unwrap()
        };
        // alice needs a direct addr (no discovery in tests): patch request
        // path by using propose via wire directly is what propose_upstream
        // does with discovery; here we test the protocol + bookkeeping by
        // sending the same messages at the known addr.
        let ops = vec![OpInput {
            kind: OpKind::Insert {
                block_id: uuid::Uuid::now_v7(),
                parent_id: None,
                order_key: "i".into(),
                block_type: BlockType::Paragraph,
                content: "alice's suggestion".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }];
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::Propose {
                share: share.id.to_string(),
                doc: doc.id.to_string(),
                ops: ops.clone(),
                note: "typo fix".into(),
                request_id: Some("retry-me".into()),
            },
        )
        .await
        .unwrap();
        let Response::Proposed { op_ids } = res else {
            panic!("expected Proposed, got {res:?}");
        };

        // retry with the same request_id: same outcome, nothing double-parked
        let retry = request(
            &alice_ep,
            addr.clone(),
            Request::Propose {
                share: share.id.to_string(),
                doc: doc.id.to_string(),
                ops: ops.clone(),
                note: "typo fix".into(),
                request_id: Some("retry-me".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(retry, Response::Proposed { op_ids: op_ids.clone() });
        let parked_ids: Vec<uuid::Uuid> = op_ids.iter().map(|s| s.parse().unwrap()).collect();
        {
            let s = alice_store.lock().unwrap();
            let mut s = s;
            s.record_outbound_proposal(doc.id, share.id, owner_contact.id, &parked_ids, "typo fix")
                .unwrap();
        }

        // owner: doc untouched (pessimistic!), one parked red in the queue
        let annotation_id = {
            let s = owner_store.lock().unwrap();
            assert!(s.read_doc(doc.id).unwrap().roots.is_empty());
            let queue = s.review_queue(Some(doc.id)).unwrap();
            assert_eq!(queue.len(), 1);
            // status reads as open for the proposer
            let statuses = s.op_statuses(&parked_ids).unwrap();
            assert!(!statuses[0].applied);
            assert_eq!(statuses[0].review.as_deref(), Some("open"));
            queue[0].annotation.id
        };

        // owner accepts → applied at current epoch
        {
            let mut s = owner_store.lock().unwrap();
            s.resolve(annotation_id, tom.id, ReviewDecision::Accept)
                .unwrap();
            assert_eq!(
                s.read_doc(doc.id).unwrap().roots[0].block.content,
                "alice's suggestion"
            );
        }

        // alice: status flips on refresh, content arrives on pull
        refresh_outbound(&alice_ep, &alice_store).await; // discovery-less: may no-op
        // discovery isn't available in tests, so check the status by wire:
        let res = request(
            &alice_ep,
            addr.clone(),
            Request::ProposalStatus {
                op_ids: op_ids.clone(),
            },
        )
        .await
        .unwrap();
        let Response::ProposalStatuses { statuses } = res else {
            panic!("expected statuses");
        };
        assert!(statuses[0].applied);
        assert_eq!(statuses[0].review.as_deref(), Some("accepted"));

        pull_share(&alice_ep, &alice_store, addr.clone(), &owner_contact, share.id)
            .await
            .unwrap();
        {
            let s = alice_store.lock().unwrap();
            let tree = s.read_doc(doc.id).unwrap();
            assert_eq!(tree.roots[0].block.content, "alice's suggestion");
        }

        // view-only share refuses proposes outright
        {
            let mut s = owner_store.lock().unwrap();
            s.set_share_permission(share.id, SharePermission::View)
                .unwrap();
        }
        let res = request(
            &alice_ep,
            addr,
            Request::Propose {
                share: share.id.to_string(),
                doc: doc.id.to_string(),
                ops,
                note: String::new(),
                request_id: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn pull_of_unbound_share_is_refused() {
        let store = owner_store("secret");
        let share_id = {
            let s = store.lock().unwrap();
            s.list_shares().unwrap()[0].id
        };
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store.clone()));

        // mallory redeems nothing but tries to pull the share
        let mallory = local_endpoint().await;
        let res = request(
            &mallory,
            addr,
            Request::Pull {
                share: share_id.to_string(),
                cursors: vec![],
            },
        )
        .await
        .unwrap();
        assert!(matches!(res, Response::Refused { .. }));
    }

    #[tokio::test]
    async fn version_mismatch_is_refused_loudly() {
        let store = owner_store("s");
        let owner = local_endpoint().await;
        let addr = direct_addr(&owner);
        tokio::spawn(serve(owner, store));

        let client = local_endpoint().await;
        let conn = client.connect(addr, ALPN).await.unwrap();
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(br#"{"v": 99, "type": "ping"}"#).await.unwrap();
        send.finish().unwrap();
        let raw = recv.read_to_end(MAX_FRAME).await.unwrap();
        let frame: Frame<Response> = serde_json::from_slice(&raw).unwrap();
        let Response::Refused { reason } = frame.msg else {
            panic!("expected Refused");
        };
        assert!(reason.contains("version"));
    }
}
