//! Owner side of federation: the accept loop, per-request authentication,
//! the request handlers, and the hot-session bridge.
//!
//! Every request is authenticated independently (contact looked up per
//! request, never per connection) so a revoke bites on the next frame. The
//! hot bridge is a long-lived stream, so it re-checks authorization on a
//! timer (`BRIDGE_REAUTH`) instead.
//!
//! Refusals carry a typed `RefusalCode` beside the human reason — the
//! grantee's loops branch on the code, never on the text.

use super::hub;
use super::runtime::Runtime;
use super::transfer;
use super::wire::{
    ALPN, Frame, HOT_ALPN, HubAction, MAX_FRAME, OnBehalfOf, PROTOCOL_VERSION, PULL_BUDGET,
    Refusal, RefusalCode, Request, Response, WireDoc, WireDocMeta, hash_secret,
};
use crate::hot::HotState;
use anyhow::{Context, Result};
use grimoire_store::{BlockStore, Contact, OpStatus, SqliteStore};
use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// Bind the production endpoint: n0 relays + DNS/pkarr discovery, PLUS local
/// mDNS discovery so two instances on the same LAN find each other directly
/// — no relay, no public DNS in the path. That matters on office networks
/// where the relay connection flaps (observed: 80+ resets in a day) and
/// would otherwise make a colleague across the desk "unreachable".
pub async fn bind(secret: [u8; 32]) -> Result<(Endpoint, iroh_mdns_address_lookup::MdnsAddressLookup)> {
    let key = SecretKey::from_bytes(&secret);
    // built by hand (not via the builder trait) so we keep a handle: its
    // discovery stream is what the Shares page's "nearby" list reads
    let mdns = iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .build(key.public())
        .context("starting local discovery")?;
    let ep = Endpoint::builder(presets::N0)
        .secret_key(key)
        .alpns(vec![ALPN.to_vec(), HOT_ALPN.to_vec()])
        .address_lookup(mdns.clone())
        .bind()
        .await
        .context("binding federation endpoint")?;
    Ok((ep, mdns))
}

/// Feed mDNS discovery events into the runtime's neighbour list. Presence
/// only: a neighbour still needs an invite or an offer to become a contact.
pub async fn neighbour_loop(mdns: iroh_mdns_address_lookup::MdnsAddressLookup, runtime: Runtime) {
    use futures_util::StreamExt;
    let mut events = mdns.subscribe().await;
    while let Some(ev) = events.next().await {
        match ev {
            iroh_mdns_address_lookup::DiscoveryEvent::Discovered { endpoint_info, .. } => {
                let name = endpoint_info
                    .user_data()
                    .map(|u| u.to_string())
                    .filter(|n| !n.trim().is_empty());
                runtime.neighbour_seen(endpoint_info.endpoint_id.to_string(), name);
            }
            iroh_mdns_address_lookup::DiscoveryEvent::Expired { endpoint_id } => {
                runtime.neighbour_gone(&endpoint_id.to_string());
            }
            _ => {}
        }
    }
}

/// Bounded propose dedupe: (peer, request_id) → original outcome (retry
/// safety). Insertion-ordered eviction: the oldest entry goes when full, so
/// a retry straddling the cap still finds its original (the clear-everything
/// it replaces made exactly that retry park twice).
pub(super) struct DedupeCache {
    map: std::collections::HashMap<String, Response>,
    order: std::collections::VecDeque<String>,
    cap: usize,
}

impl DedupeCache {
    pub(super) fn new(cap: usize) -> Self {
        Self {
            map: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<&Response> {
        self.map.get(key)
    }

    pub(super) fn insert(&mut self, key: String, value: Response) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.map.len()
    }
}

type Dedupe = Arc<Mutex<DedupeCache>>;
const DEDUPE_CAP: usize = 512;

/// 0.7.2 frame limits. A peer that is not (yet) a contact may only ever be
/// redeeming an invite — a few hundred bytes — so its frame is read under
/// this cap; `MAX_FRAME` applies once the peer is a known, live contact.
/// Before this an unknown node could make the daemon buffer and parse 32 MB
/// per connection, unbounded in connections and in time.
pub const PRE_AUTH_FRAME: usize = 64 * 1024;
/// How long one request frame (handshake, or bytes after `accept_bi`) may
/// take to arrive; also the idle wait for the next stream on a connection.
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Concurrent federation connections (requests + hot bridges) this daemon
/// services; the rest are dropped at accept with a warning.
pub const MAX_CONNECTIONS: usize = 256;

/// Accept loop. Spawned once per daemon; lives until the endpoint closes.
pub async fn serve(
    endpoint: Endpoint,
    store: Arc<Mutex<SqliteStore>>,
    hot: HotState,
    runtime: Runtime,
) {
    tracing::info!("federation endpoint listening (node id {})", endpoint.id());
    let dedupe: Dedupe = Arc::new(Mutex::new(DedupeCache::new(DEDUPE_CAP)));
    let gate = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            tracing::warn!(
                cap = MAX_CONNECTIONS,
                "federation: connection cap reached; dropping an incoming connection"
            );
            drop(incoming);
            continue;
        };
        let store = store.clone();
        let dedupe = dedupe.clone();
        let hot = hot.clone();
        let runtime = runtime.clone();
        let ep = endpoint.clone();
        tokio::spawn(async move {
            let _permit = permit; // held for the life of the connection
            let conn = match incoming.accept() {
                Ok(accepting) => match tokio::time::timeout(READ_TIMEOUT, accepting).await {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(e)) => {
                        tracing::debug!("federation handshake failed: {e:#}");
                        return;
                    }
                    Err(_) => {
                        tracing::debug!("federation handshake timed out");
                        return;
                    }
                },
                Err(e) => {
                    tracing::debug!("federation accept failed: {e:#}");
                    return;
                }
            };
            let peer = conn.remote_id().to_string();
            if conn.alpn() == HOT_ALPN {
                // a failed/refused bridge is the #1 "text never shows up"
                // symptom for grantees — always visible in the log
                if let Err(e) = handle_hot_bridge(conn, &peer, store, hot).await {
                    tracing::warn!(peer, "hot bridge ended/refused: {e:#}");
                }
                return;
            }
            if let Err(e) = handle_conn(conn, &peer, store, dedupe, hot, ep, runtime).await {
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
    hot: HotState,
    endpoint: Endpoint,
    runtime: Runtime,
) -> Result<()> {
    // authed = peer is a non-revoked contact. Checked once per request, not
    // once per connection: a successful redeem upgrades the session, and a
    // mid-session revoke takes effect on the next request.
    loop {
        let (mut send, mut recv) = match tokio::time::timeout(READ_TIMEOUT, conn.accept_bi()).await {
            Ok(Ok(s)) => s,
            // peer closed: normal end of conversation; idle too long: ours
            Ok(Err(_)) | Err(_) => return Ok(()),
        };
        // the cap is decided per request from the store, never from what the
        // peer says about itself: a stranger gets `PRE_AUTH_FRAME` until a
        // redeem has paired it, and a revoke shrinks it again
        let authed = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            s.contact_by_pubkey(peer).ok().flatten().is_some_and(|c| !c.revoked)
        };
        let limit = if authed { MAX_FRAME } else { PRE_AUTH_FRAME };
        let raw = match tokio::time::timeout(READ_TIMEOUT, recv.read_to_end(limit)).await {
            Ok(Ok(raw)) => raw,
            Ok(Err(iroh::endpoint::ReadToEndError::TooLong)) => {
                // refused without parsing a byte of it (the reply is flushed
                // by the normal close: the peer hangs up after reading it)
                tracing::warn!(peer, authed, limit, "federation: frame over the cap; refused");
                let out = serde_json::to_vec(&Frame {
                    v: PROTOCOL_VERSION,
                    msg: Response::refused(
                        RefusalCode::BadRequest,
                        format!("frame too large ({limit} bytes max{})", if authed { "" } else { " before authentication" }),
                    ),
                })?;
                send.write_all(&out).await?;
                send.finish()?;
                continue;
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("request frame timed out after {}s", READ_TIMEOUT.as_secs()),
        };
        let response = match serde_json::from_slice::<Frame<Request>>(&raw) {
            Err(e) => Response::refused(RefusalCode::BadRequest, format!("bad frame: {e}")),
            Ok(frame) if frame.v != PROTOCOL_VERSION => Response::refused(
                RefusalCode::Version,
                format!(
                    "protocol version {} not supported (this instance speaks {})",
                    frame.v, PROTOCOL_VERSION
                ),
            ),
            Ok(frame) => dispatch(frame.msg, peer, &store, &dedupe, &hot, &endpoint, &runtime).await,
        };
        let out = serde_json::to_vec(&Frame {
            v: PROTOCOL_VERSION,
            msg: response,
        })?;
        send.write_all(&out).await?;
        send.finish()?;
    }
}

/// Convert a handler error into the wire refusal, preserving a typed code
/// when the handler raised one.
fn refuse(e: anyhow::Error) -> Response {
    match e.downcast_ref::<Refusal>() {
        Some(r) => Response::refused(r.code, r.reason.clone()),
        None => Response::refused(RefusalCode::Other, format!("{e:#}")),
    }
}

fn unknown_peer() -> Response {
    Response::refused(
        RefusalCode::UnknownPeer,
        "unknown peer: redeem an invite first".to_string(),
    )
}

/// What the synchronous planner decided: either a reply, or work that needs
/// the network (and therefore must run WITHOUT the store lock).
enum Step {
    Reply(Response),
    /// Hub (slice 2): a member proposed on a relayed doc — carry it to the
    /// owner as the member's proposal.
    ForwardPropose(Box<ForwardPropose>),
    /// Hub (slice 2): some of the asked-about ops live on other owners.
    ForwardStatus {
        local: Vec<OpStatus>,
        remote: Vec<(Contact, Vec<uuid::Uuid>)>,
    },
}

struct ForwardPropose {
    owner: Contact,
    owner_share: uuid::Uuid,
    doc: uuid::Uuid,
    ops: Vec<grimoire_store::OpInput>,
    note: String,
    base_epoch: i64,
    member: Contact,
    member_name: String,
    hub_name: String,
    dedupe_key: Option<String>,
}

async fn dispatch(
    req: Request,
    peer: &str,
    store_arc: &Arc<Mutex<SqliteStore>>,
    dedupe: &Dedupe,
    hot: &HotState,
    endpoint: &Endpoint,
    runtime: &Runtime,
) -> Response {
    match dispatch_sync(req, peer, store_arc, dedupe, hot, endpoint, runtime) {
        Step::Reply(r) => r,
        Step::ForwardPropose(f) => forward_propose(endpoint, store_arc, dedupe, *f).await,
        Step::ForwardStatus { local, remote } => forward_status(endpoint, local, remote).await,
    }
}

/// Hub (slice 2): carry a member's proposal to the doc's true owner. The
/// owner sees it as the MEMBER's proposal (`on_behalf_of`); the hub records
/// the owner's op ids so the member's status checks can be answered later.
async fn forward_propose(
    endpoint: &Endpoint,
    store_arc: &Arc<Mutex<SqliteStore>>,
    dedupe: &Dedupe,
    f: ForwardPropose,
) -> Response {
    let Ok(owner_id) = f.owner.pubkey.parse::<iroh::EndpointId>() else {
        return Response::refused(RefusalCode::Other, "the owner's address is malformed");
    };
    let mut ops = f.ops;
    for op in &mut ops {
        op.source_refs.push(format!("via hub: {}", f.hub_name));
    }
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        super::client::request(
            endpoint,
            iroh::EndpointAddr::from(owner_id),
            Request::Propose {
                share: f.owner_share.to_string(),
                doc: f.doc.to_string(),
                ops,
                note: f.note,
                base_epoch: Some(f.base_epoch),
                request_id: Some(uuid::Uuid::now_v7().to_string()),
                on_behalf_of: Some(OnBehalfOf {
                    pubkey: f.member.pubkey.clone(),
                    name: f.member_name.clone(),
                }),
            },
        ),
    )
    .await;
    let owner_name = f.owner.petname.clone();
    let response = match res {
        Ok(Ok(Response::Proposed { op_ids })) => {
            let ids: Vec<uuid::Uuid> = op_ids.iter().filter_map(|s| s.parse().ok()).collect();
            let mut s = store_arc.lock().unwrap_or_else(|p| p.into_inner());
            for id in &ids {
                if let Err(e) = s.add_hub_forward(*id, f.owner.id, f.member.id, f.owner_share, f.doc) {
                    tracing::warn!(op = %id, "hub: forward record failed: {e}");
                }
            }
            tracing::info!(
                member = f.member_name,
                owner = owner_name,
                doc = %f.doc,
                ops = ids.len(),
                "hub: proposal forwarded to the owner"
            );
            Response::Proposed { op_ids }
        }
        Ok(Ok(Response::Refused { reason, code })) => {
            tracing::warn!(member = f.member_name, owner = owner_name, code = ?code, "hub: owner refused a forwarded proposal: {reason}");
            Response::refused(code, format!("{owner_name} did not take the edit: {reason}"))
        }
        Ok(Ok(other)) => Response::refused(RefusalCode::Other, format!("unexpected reply from {owner_name}: {other:?}")),
        Ok(Err(e)) => {
            tracing::warn!(owner = owner_name, "hub: forward failed: {e:#}");
            Response::refused(RefusalCode::Other, format!("{owner_name} is unreachable right now — try again later"))
        }
        Err(_) => Response::refused(RefusalCode::Other, format!("{owner_name} is offline or unreachable right now — try again later")),
    };
    if let (Some(key), Response::Proposed { .. }) = (&f.dedupe_key, &response) {
        let mut cache = dedupe.lock().unwrap_or_else(|p| p.into_inner());
        cache.insert(key.clone(), response.clone());
    }
    response
}

/// Hub (slice 2): ask each owner about the ops the hub forwarded to them
/// and merge with what the hub knows locally. An unreachable owner's ops are
/// simply absent (the member's loop treats that as "still pending").
async fn forward_status(
    endpoint: &Endpoint,
    mut local: Vec<OpStatus>,
    remote: Vec<(Contact, Vec<uuid::Uuid>)>,
) -> Response {
    for (owner, ids) in remote {
        let Ok(owner_id) = owner.pubkey.parse::<iroh::EndpointId>() else {
            continue;
        };
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            super::client::request(
                endpoint,
                iroh::EndpointAddr::from(owner_id),
                Request::ProposalStatus {
                    op_ids: ids.iter().map(|u| u.to_string()).collect(),
                },
            ),
        )
        .await;
        match res {
            Ok(Ok(Response::ProposalStatuses { statuses })) => local.extend(statuses),
            Ok(Ok(other)) => tracing::debug!(owner = owner.petname, "hub: status forward not answered: {other:?}"),
            Ok(Err(e)) => tracing::debug!(owner = owner.petname, "hub: status forward failed: {e:#}"),
            Err(_) => tracing::debug!(owner = owner.petname, "hub: status forward timed out"),
        }
    }
    Response::ProposalStatuses { statuses: local }
}

fn dispatch_sync(
    req: Request,
    peer: &str,
    store_arc: &Arc<Mutex<SqliteStore>>,
    dedupe: &Dedupe,
    hot: &HotState,
    endpoint: &Endpoint,
    runtime: &Runtime,
) -> Step {
    dispatch_inner(req, peer, store_arc, dedupe, hot, endpoint, runtime).unwrap_or_else(Step::Reply)
}

/// The request planner. Holds the store lock for its whole body, so it must
/// never await; network work is returned as a non-`Reply` step. Returns
/// `Err(response)` for early refusals — the same thing as `Ok(Step::Reply)`,
/// kept separate only to let `?`-style early returns read naturally.
fn dispatch_inner(
    req: Request,
    peer: &str,
    store_arc: &Arc<Mutex<SqliteStore>>,
    dedupe: &Dedupe,
    hot: &HotState,
    endpoint: &Endpoint,
    runtime: &Runtime,
) -> std::result::Result<Step, Response> {
    let mut store = store_arc.lock().unwrap_or_else(|p| p.into_inner());
    let contact = match store.contact_by_pubkey(peer) {
        Ok(c) => c.filter(|c| !c.revoked),
        Err(e) => {
            return Err(Response::refused(RefusalCode::Other, format!("store error: {e}")));
        }
    };
    // the one request an unknown peer may make
    if let Request::Redeem { secret, petname } = req {
        let was_new = contact.is_none();
        return Err(match store.redeem_invite(&hash_secret(&secret), peer, &petname) {
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
                // hub: first contact = first admin; later ones wait for approval
                let hub_cfg = hub::config(&store);
                let decision = match &hub_cfg {
                    Some(h) => match hub::on_redeem(&mut store, h, was_new, &contact, &share) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            tracing::warn!(peer, "hub membership decision failed: {e:#}");
                            None
                        }
                    },
                    None => None,
                };
                if let Some(d) = &decision {
                    tracing::info!(
                        peer,
                        petname = contact.petname,
                        membership = d.membership.as_str(),
                        role = d.role.as_str(),
                        "hub: membership decided"
                    );
                }
                // a pending member gets the share taken back: say so
                let permission = match &decision {
                    Some(d) if d.membership == grimoire_store::Membership::Active => "propose".to_string(),
                    Some(_) => "none".to_string(),
                    None => share.permission.as_str().to_string(),
                };
                Response::Redeemed {
                    share_id: share.id.to_string(),
                    root_doc: share.root_doc.to_string(),
                    root_title,
                    permission,
                    owner_name,
                    is_hub: hub_cfg.is_some(),
                    membership: decision.as_ref().map(|d| d.membership.as_str().to_string()),
                    role: decision.as_ref().map(|d| d.role.as_str().to_string()),
                }
            }
            Err(e) => {
                tracing::warn!(peer, "invite redeem refused: {e}");
                Response::refused(RefusalCode::InviteInvalid, e.to_string())
            }
        });
    }
    // everything else needs a live contact
    let Some(contact) = contact else {
        tracing::warn!(peer, "unauthenticated request refused");
        return Err(unknown_peer());
    };
    // hub: a member who is not (yet) active may only ask where they stand
    let hub_cfg = hub::config(&store);
    if let Some(h) = &hub_cfg
        && contact.membership != grimoire_store::Membership::Active
    {
        return Err(match req {
            Request::HubStatus => hub_status(&store, h, &contact),
            _ => Response::refused(
                RefusalCode::NotAllowed,
                format!("your request to join {} is waiting for an admin", h.name),
            ),
        });
    }
    let response = match req {
        Request::Redeem { .. } => unreachable!("handled above"),
        Request::HubStatus => match &hub_cfg {
            Some(h) => hub_status(&store, h, &contact),
            None => Response::refused(RefusalCode::Unsupported, "this Grimoire is not a hub"),
        },
        Request::HubAdmin { action } => {
            let Some(h) = &hub_cfg else {
                return Err(Response::refused(RefusalCode::Unsupported, "this Grimoire is not a hub"));
            };
            if contact.role != grimoire_store::ContactRole::Admin {
                tracing::warn!(peer, petname = contact.petname, ?action, "hub admin action from a non-admin refused");
                return Err(Response::refused(
                    RefusalCode::NotAllowed,
                    format!("only an admin of {} can do that", h.name),
                ));
            }
            match handle_hub_admin(&mut store, store_arc, hot, h, &contact, action, endpoint) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(peer, "hub admin action refused: {e:#}");
                    refuse(e)
                }
            }
        }
        Request::TransferOffer { root_doc, title, doc_count } => {
            let Some(h) = &hub_cfg else {
                return Err(Response::refused(RefusalCode::Unsupported, "this Grimoire is not a hub"));
            };
            let Ok(root) = root_doc.parse::<uuid::Uuid>() else {
                return Err(Response::refused(RefusalCode::BadRequest, "bad doc id"));
            };
            if title.chars().count() > 300 {
                return Err(Response::refused(RefusalCode::BadRequest, "bad title"));
            }
            match store.add_hub_transfer(contact.id, root, &title, doc_count as i64) {
                Ok(t) => {
                    tracing::info!(peer, member = contact.petname, hub = h.name, title, doc_count, "hub: transfer offered");
                    Response::TransferOffered { id: t.id.to_string() }
                }
                Err(e) => Response::refused(RefusalCode::Other, e.to_string()),
            }
        }
        Request::TransferAccepted { root_doc } => {
            let Ok(root) = root_doc.parse::<uuid::Uuid>() else {
                return Err(Response::refused(RefusalCode::BadRequest, "bad doc id"));
            };
            match transfer::member_flip(&mut store, hot, &contact, root) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(peer, root = %root, "transfer refused: {e:#}");
                    refuse(e)
                }
            }
        }
        Request::TransferBack { .. } => Response::refused(
            RefusalCode::Unsupported,
            "handing a folder back is not supported yet",
        ),
        Request::Ping => {
            tracing::debug!(peer, petname = contact.petname, "ping");
            Response::Pong
        }
        Request::Pull { share, cursors } => match handle_pull(&store, &contact, &share, &cursors) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(peer, share, "pull refused: {e}");
                refuse(e)
            }
        },
        Request::Propose {
            share,
            doc,
            ops,
            note,
            base_epoch,
            request_id,
            on_behalf_of,
        } => {
            // retry safety: the same request_id returns the original outcome
            let dedupe_key = request_id.map(|r| format!("{peer}:{r}"));
            if let Some(key) = &dedupe_key {
                let cache = dedupe.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(prior) = cache.get(key) {
                    return Err(prior.clone());
                }
            }
            // hub (slice 2): a relayed doc's edits go to its owner, as the
            // member's proposal — the hub carries, never decides
            if let Some(h) = &hub_cfg {
                match plan_forward(&store, h, &contact, &share, &doc) {
                    Ok(Some((owner, owner_share, doc_id, base))) => {
                        if ops.is_empty() {
                            return Err(Response::refused(RefusalCode::BadRequest, "empty proposal"));
                        }
                        if on_behalf_of.is_some() {
                            return Err(Response::refused(RefusalCode::NotAllowed, "a hub does not relay for another hub"));
                        }
                        let contacts = store.list_contacts().unwrap_or_default();
                        return Ok(Step::ForwardPropose(Box::new(ForwardPropose {
                            owner,
                            owner_share,
                            doc: doc_id,
                            ops,
                            note,
                            base_epoch: base,
                            member_name: hub::display_name(&contacts, &contact),
                            member: contact,
                            hub_name: h.name.clone(),
                            dedupe_key,
                        })));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(peer, share, doc, "propose refused: {e}");
                        return Err(refuse(e));
                    }
                }
            }
            let res = match handle_propose(&mut store, hot, &contact, &share, &doc, ops, &note, base_epoch, on_behalf_of)
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(peer, share, doc, "propose refused: {e}");
                    refuse(e)
                }
            };
            if let (Some(key), Response::Proposed { .. }) = (&dedupe_key, &res) {
                let mut cache = dedupe.lock().unwrap_or_else(|p| p.into_inner());
                cache.insert(key.clone(), res.clone());
            }
            res
        }
        Request::HotStatus { share, doc } => match authorize_hot(&store, &contact, &share, &doc, false) {
            Ok(doc_id) => {
                let (is_hot, frozen_epoch) = hot.status(doc_id);
                // session = consent: propose always writes; a view grantee
                // writes too while the owner leaves the session open to all
                let has_propose = authorize_share(&store, &contact, &share)
                    .map(|s| s.permission == grimoire_store::SharePermission::Propose)
                    .unwrap_or(false);
                let can_write = has_propose || hot.viewers_write(doc_id).unwrap_or(false);
                Response::HotStatusIs {
                    hot: is_hot,
                    frozen_epoch,
                    editors: hot.editors(doc_id),
                    can_write: Some(can_write),
                }
            }
            Err(e) => refuse(e),
        },
        Request::HotStart {
            share,
            doc,
            base_epoch,
        } => match authorize_hot(&store, &contact, &share, &doc, true) {
            Ok(doc_id) => {
                let frozen_epoch = match store.get_doc(doc_id) {
                    Ok(d) => d.current_epoch,
                    Err(e) => return Err(Response::refused(RefusalCode::Other, e.to_string())),
                };
                // Joining a live session is always fine — the seed already
                // happened. CREATING one seeds from the grantee's mirror, so
                // that mirror must be at our epoch or the flatten would land
                // stale text over newer edits (with a green verdict).
                if !hot.is_hot(doc_id) && base_epoch != Some(frozen_epoch) {
                    tracing::warn!(
                        peer = contact.pubkey,
                        %doc_id,
                        ?base_epoch,
                        frozen_epoch,
                        "remote hot start refused: grantee copy is behind"
                    );
                    return Err(Response::refused(
                        RefusalCode::StaleBase,
                        format!(
                            "your copy is at epoch {}, the owner is at {frozen_epoch}: pull first",
                            base_epoch.map(|e| e.to_string()).unwrap_or_else(|| "?".into())
                        ),
                    ));
                }
                // the creator is recorded: only they (or the owner) may end it
                match hot.start_by(doc_id, frozen_epoch, Some(peer)) {
                    Ok(seed) => {
                        tracing::info!(peer = contact.pubkey, %doc_id, seed, "remote hot start");
                        Response::HotStarted { frozen_epoch, seed }
                    }
                    Err(e) => Response::refused(RefusalCode::Other, e.to_string()),
                }
            }
            Err(e) => refuse(e),
        },
        Request::EditPing { share, doc, key } => {
            match authorize_hot(&store, &contact, &share, &doc, true) {
                Ok(doc_id) => {
                    let editors = key
                        .parse::<uuid::Uuid>()
                        .ok()
                        .map(|k| hot.edit_ping(doc_id, k))
                        .unwrap_or(1);
                    Response::HotStatusIs {
                        hot: hot.is_hot(doc_id),
                        frozen_epoch: None,
                        editors,
                        can_write: Some(true), // EditPing already required propose
                    }
                }
                Err(e) => refuse(e),
            }
        }
        Request::HotEnd { share, doc } => match authorize_hot(&store, &contact, &share, &doc, true) {
            Ok(doc_id) => {
                // ending flattens for EVERYONE in the room: the owner (locally)
                // or the participant who started the session — not any peer
                if !hot.can_end(doc_id, peer) {
                    return Err(Response::refused(
                        RefusalCode::NotAllowed,
                        "only the owner or whoever started the session can end it",
                    ));
                }
                // flatten needs the store WITHOUT this dispatch holding it
                drop(store);
                match hot.flatten_and_close(store_arc, doc_id, "ended by peer") {
                    Ok(applied) => Response::HotEnded {
                        flattened_ops: applied,
                    },
                    Err(e) => Response::refused(RefusalCode::Other, format!("{e:#}")),
                }
            }
            Err(e) => refuse(e),
        },
        Request::Comment {
            share,
            target_block,
            text,
            reply_to,
        } => match handle_comment(&mut store, &contact, &share, &target_block, &text, reply_to) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(peer, share, "comment refused: {e}");
                refuse(e)
            }
        },
        Request::Notify {
            share,
            doc,
            title,
            kind,
        } => accept_nudges(
            &store,
            store_arc,
            endpoint,
            runtime,
            &contact,
            &share,
            vec![super::wire::NotifyItem { doc, title, kind }],
        ),
        Request::NotifyBatch { share, items } => {
            accept_nudges(&store, store_arc, endpoint, runtime, &contact, &share, items)
        }
        Request::Offer {
            share,
            root_title,
            permission,
            secret,
            expires_at,
        } => {
            // a known contact offering us a share: store it durably, tell the UI
            let Ok(share_uuid) = share.parse::<uuid::Uuid>() else {
                return Err(Response::refused(RefusalCode::BadRequest, "bad share id"));
            };
            let Some(perm) = grimoire_store::SharePermission::parse(&permission) else {
                return Err(Response::refused(RefusalCode::BadRequest, "bad permission"));
            };
            if secret.is_empty() || secret.len() > 128 || root_title.chars().count() > 300 {
                return Err(Response::refused(RefusalCode::BadRequest, "bad offer"));
            }
            if hub_cfg.is_some() && perm != grimoire_store::SharePermission::Propose {
                return Err(Response::refused(
                    RefusalCode::BadRequest,
                    "publishing to a hub needs a share that can propose edits",
                ));
            }
            match store.add_share_offer(
                contact.id,
                peer,
                share_uuid,
                &root_title,
                perm,
                &secret,
                &expires_at,
            ) {
                Ok(offer) => {
                    tracing::info!(peer, petname = contact.petname, share, "share offer received");
                    if hub_cfg.is_some() {
                        // hub: an active member publishing — accept without a human,
                        // off this thread (dispatch holds the store lock). The member
                        // just dialed us, so their address is known to the endpoint.
                        if let Ok(id) = peer.parse::<iroh::EndpointId>() {
                            let endpoint = endpoint.clone();
                            let store_arc = store_arc.clone();
                            let offer_id = offer.id;
                            tokio::spawn(async move {
                                match hub::accept_publication(&endpoint, &store_arc, offer_id, iroh::EndpointAddr::from(id)).await {
                                    Ok(root) => tracing::info!(%root, "hub: publication relayed"),
                                    Err(e) => tracing::warn!("hub: publication not accepted: {e:#}"),
                                }
                            });
                        }
                    } else {
                        runtime.push_event("share_offered", offer.id, root_title, contact.petname.clone());
                    }
                    Response::Noted
                }
                Err(e) => Response::refused(RefusalCode::Other, e.to_string()),
            }
        }
        Request::ProposalStatus { op_ids } => {
            let ids: Vec<uuid::Uuid> = op_ids.iter().filter_map(|s| s.parse().ok()).collect();
            // hub (slice 2): ops the hub forwarded for THIS member live on
            // their owners — ask them, off the lock
            let forwarded: Vec<grimoire_store::HubForward> = if hub_cfg.is_some() {
                store
                    .hub_forwards_for(&ids)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|f| f.member_contact == contact.id)
                    .collect()
            } else {
                Vec::new()
            };
            let forwarded_ids: std::collections::HashSet<uuid::Uuid> = forwarded.iter().map(|f| f.op_id).collect();
            let local_ids: Vec<uuid::Uuid> = ids.iter().copied().filter(|i| !forwarded_ids.contains(i)).collect();
            let hub_ref = format!("hub-pubkey:{}", contact.pubkey);
            let local = match store.op_statuses(&local_ids) {
                // disclose only the asker's own ops — or, to a hub, the ops
                // it forwarded to us on someone's behalf
                Ok(statuses) => statuses
                    .into_iter()
                    .filter(|s| {
                        s.principal == contact.principal
                            || (contact.is_hub && s.source_refs.iter().any(|r| r == &hub_ref))
                    })
                    .collect::<Vec<_>>(),
                Err(e) => return Err(Response::refused(RefusalCode::Other, e.to_string())),
            };
            if forwarded.is_empty() {
                Response::ProposalStatuses { statuses: local }
            } else {
                let contacts = store.list_contacts().unwrap_or_default();
                let mut remote: Vec<(Contact, Vec<uuid::Uuid>)> = Vec::new();
                for f in forwarded {
                    let Some(owner) = contacts.iter().find(|c| c.id == f.owner_contact && !c.revoked) else {
                        continue;
                    };
                    match remote.iter_mut().find(|(c, _)| c.id == owner.id) {
                        Some((_, ids)) => ids.push(f.op_id),
                        None => remote.push((owner.clone(), vec![f.op_id])),
                    }
                }
                return Ok(Step::ForwardStatus { local, remote });
            }
        }
    };
    Ok(Step::Reply(response))
}

/// Hub (slice 2): is this proposal aimed at a doc the hub merely relays? If
/// so, return the owner to carry it to: (owner contact, the owner's share the
/// hub holds it through, the doc, the hub's synced epoch for it). `None` =
/// the hub's own doc; handle locally.
fn plan_forward(
    store: &SqliteStore,
    _hub: &hub::HubConfig,
    contact: &Contact,
    share_id: &str,
    doc_id: &str,
) -> Result<Option<(Contact, uuid::Uuid, uuid::Uuid, i64)>> {
    let share = authorize_share(store, contact, share_id)?;
    let doc_uuid: uuid::Uuid = doc_id
        .parse()
        .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad doc id"))?;
    let Some((owner_pubkey, owner_name)) = relay_origins(store, share.id).get(&doc_uuid).cloned() else {
        return Ok(None);
    };
    if share.permission != grimoire_store::SharePermission::Propose {
        return Err(Refusal::new(RefusalCode::ViewOnly, "share is view-only").into());
    }
    require_in_share(store, share.id, doc_uuid)?;
    let owner = store
        .contact_by_pubkey(&owner_pubkey)?
        .filter(|c| !c.revoked)
        .ok_or_else(|| Refusal::new(RefusalCode::Other, format!("{owner_name} is no longer a member")))?;
    let mirror = store
        .get_mirror(doc_uuid)?
        .ok_or_else(|| Refusal::new(RefusalCode::Other, "relay record is missing"))?;
    Ok(Some((owner, mirror.share_id, doc_uuid, mirror.synced_epoch)))
}

/// Grantee side of a nudge (single or batched): accept only for a share we
/// hold FROM this contact, surface every item as a UI event, and pull that
/// share once — a burst of nudges while a pull is in flight collapses into
/// one follow-up pull (`Runtime::begin_pull`).
fn accept_nudges(
    store: &SqliteStore,
    store_arc: &Arc<Mutex<SqliteStore>>,
    endpoint: &Endpoint,
    runtime: &Runtime,
    contact: &grimoire_store::Contact,
    share: &str,
    items: Vec<super::wire::NotifyItem>,
) -> Response {
    let Ok(share_uuid) = share.parse::<uuid::Uuid>() else {
        return Response::refused(RefusalCode::BadRequest, "bad share id");
    };
    let holds = store
        .list_mirrors()
        .map(|ms| ms.iter().any(|m| m.share_id == share_uuid && m.owner == contact.id))
        .unwrap_or(false);
    if !holds {
        return Response::refused(RefusalCode::NotInShare, "no mirror of that share from you");
    }
    for item in items {
        let Ok(doc_uuid) = item.doc.parse::<uuid::Uuid>() else {
            return Response::refused(RefusalCode::BadRequest, "bad doc id");
        };
        runtime.push_event(item.kind.as_str(), doc_uuid, item.title, contact.petname.clone());
    }
    // pull that share NOW — off this thread (dispatch holds the store lock)
    super::loops::spawn_nudged_pull(
        endpoint.clone(),
        store_arc.clone(),
        runtime.clone(),
        contact.clone(),
        share_uuid,
    );
    Response::Noted
}

/// The share must exist, be bound to THIS contact, and be active. Every
/// authenticated request starts here.
fn authorize_share(
    store: &SqliteStore,
    contact: &grimoire_store::Contact,
    share_id: &str,
) -> Result<grimoire_store::Share> {
    let share_uuid: uuid::Uuid = share_id
        .parse()
        .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad share id"))?;
    let share = store
        .get_share(share_uuid)
        .map_err(|_| Refusal::new(RefusalCode::NotInShare, "no such share"))?;
    if share.contact != Some(contact.id) {
        return Err(Refusal::new(RefusalCode::NotInShare, "share is not bound to this contact").into());
    }
    match share.state {
        grimoire_store::ShareState::Active => Ok(share),
        grimoire_store::ShareState::Revoked => {
            Err(Refusal::new(RefusalCode::ShareRevoked, "share is revoked").into())
        }
        other => Err(Refusal::new(
            RefusalCode::ShareInactive,
            format!("share is {}", other.as_str()),
        )
        .into()),
    }
}

/// The docs a share exposes: its subtree MINUS any mirror (content shared TO
/// this instance is never served onward — belt and braces with the API-level
/// move refusal and the create-time re-share guard).
///
/// The ONE exception is a hub relaying publications (slice 1): for the share
/// of the hub root, mirrors that are hub publications ARE served — with their
/// true owner in the wire meta (`relay_origins`). Nothing else changes: a
/// hub's other shares, and every non-hub instance, still hide mirrors.
pub(super) fn served_docs(store: &SqliteStore, share_id: uuid::Uuid) -> Result<Vec<grimoire_store::Doc>> {
    served_docs_for(store, share_id, None)
}

/// `served_docs` for one viewer: a member never receives their OWN docs back
/// through the relay (they would only be ignored by the hijack guard).
pub(super) fn served_docs_for(
    store: &SqliteStore,
    share_id: uuid::Uuid,
    viewer_pubkey: Option<&str>,
) -> Result<Vec<grimoire_store::Doc>> {
    let relay = relay_origins(store, share_id);
    let viewer_contact: Option<uuid::Uuid> = viewer_pubkey
        .and_then(|pk| store.contact_by_pubkey(pk).ok().flatten())
        .map(|c| c.id);
    let mirrors: std::collections::HashSet<uuid::Uuid> = store
        .list_mirrors()?
        .into_iter()
        .filter(|m| match (relay.get(&m.doc_id), viewer_pubkey) {
            // relayed, and not the viewer's own: served
            (Some((owner, _)), Some(v)) => owner == v,
            (Some(_), None) => false,
            // a mirror is never served onward — except back to the very peer
            // it is mirrored FROM (slice 2: a subtree flipped to mirrors of a
            // hub in a transfer, which the hub pulls before taking it over)
            _ => Some(m.owner) != viewer_contact,
        })
        .map(|m| m.doc_id)
        .collect();
    let mut docs = store.docs_in_share(share_id)?;
    if !mirrors.is_empty() {
        // drop mirrors AND everything under them (a mirror's children are
        // the owner's too)
        let mut hidden = mirrors.clone();
        let mut changed = true;
        while changed {
            changed = false;
            for d in &docs {
                if !hidden.contains(&d.id)
                    && d.parent_id.is_some_and(|p| hidden.contains(&p))
                {
                    hidden.insert(d.id);
                    changed = true;
                }
            }
        }
        docs.retain(|d| !hidden.contains(&d.id));
    }
    Ok(docs)
}

/// Relay provenance for a share: doc → (true owner pubkey, display name),
/// non-empty only on a hub for the share of its root.
fn relay_origins(store: &SqliteStore, share_id: uuid::Uuid) -> std::collections::HashMap<uuid::Uuid, (String, String)> {
    match hub::config(store) {
        Some(h) if store.get_share(share_id).map(|sh| sh.root_doc == h.root_doc).unwrap_or(false) => {
            hub::relay_set(store)
        }
        _ => Default::default(),
    }
}

fn require_in_share(store: &SqliteStore, share_id: uuid::Uuid, doc_id: uuid::Uuid) -> Result<()> {
    if served_docs(store, share_id)?.iter().any(|d| d.id == doc_id) {
        Ok(())
    } else {
        Err(Refusal::new(RefusalCode::NotInShare, "doc is not in this share").into())
    }
}

/// Slice 1: a relayed doc is not the hub's to change. Writes (propose, hot
/// start/end, edit pings, comments) are refused with a typed code; routing
/// them to the owner is the next slice.
fn require_not_relayed(store: &SqliteStore, share_id: uuid::Uuid, doc_id: uuid::Uuid) -> Result<()> {
    if let Some((_, name)) = relay_origins(store, share_id).get(&doc_id) {
        return Err(Refusal::new(
            RefusalCode::RelayedReadOnly,
            format!("this doc is owned by {name} — edits go to them, not the hub (coming soon)"),
        )
        .into());
    }
    Ok(())
}

/// `HubStatus`: where the asking contact stands.
fn hub_status(store: &SqliteStore, h: &hub::HubConfig, contact: &grimoire_store::Contact) -> Response {
    let contacts = store.list_contacts().unwrap_or_default();
    let live: Vec<_> = contacts.iter().filter(|c| !c.revoked && !c.is_hub).collect();
    Response::HubStatusIs {
        name: h.name.clone(),
        role: contact.role.as_str().into(),
        membership: contact.membership.as_str().into(),
        members: live.iter().filter(|c| c.membership == grimoire_store::Membership::Active).count(),
        pending: live.iter().filter(|c| c.membership == grimoire_store::Membership::Pending).count(),
    }
}

/// `HubAdmin`: the caller is already known to be an admin. Delivery of a
/// membership offer happens off-thread (dispatch holds the store lock).
#[allow(clippy::too_many_arguments)]
fn handle_hub_admin(
    store: &mut SqliteStore,
    store_arc: &Arc<Mutex<SqliteStore>>,
    hot: &HotState,
    h: &hub::HubConfig,
    admin: &grimoire_store::Contact,
    action: HubAction,
    endpoint: &Endpoint,
) -> Result<Response> {
    let parse = |id: &str| -> Result<uuid::Uuid> {
        id.parse()
            .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad contact id").into())
    };
    match action {
        HubAction::ReviewQueue => {
            // every open item on a hub is on a hub-owned doc: mirrors take no
            // proposals (relayed edits are forwarded, never parked here)
            let mut items = crate::api::decorate_review_items(store, store.review_queue(None)?);
            // name members the way the member list does (no fingerprint
            // suffix unless two share a name)
            let contacts = store.list_contacts()?;
            for item in &mut items {
                let principal = item["item"]["op"]["principal"].as_str().and_then(|p| p.parse::<uuid::Uuid>().ok());
                if let Some(c) = principal.and_then(|p| contacts.iter().find(|c| c.principal == p)) {
                    item["proposer"] = serde_json::Value::String(hub::display_name(&contacts, c));
                }
            }
            Ok(Response::HubQueue { items })
        }
        HubAction::Resolve { annotation_id, decision } => {
            let ann: uuid::Uuid = annotation_id
                .parse()
                .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad annotation id"))?;
            let decision = match decision.as_str() {
                "accept" => grimoire_store::ReviewDecision::Accept,
                "decline" => grimoire_store::ReviewDecision::Decline,
                _ => return Err(Refusal::new(RefusalCode::BadRequest, "decision must be accept or decline").into()),
            };
            let Some(doc) = crate::hot::annotation_doc(store, ann) else {
                return Err(Refusal::new(RefusalCode::BadRequest, "that proposal is no longer open").into());
            };
            if let Err(m) = hot.assert_cold(doc) {
                return Err(Refusal::new(RefusalCode::DocHot, m).into());
            }
            store.resolve(ann, admin.principal, decision)?;
            tracing::info!(admin = admin.petname, %doc, ?decision, "hub: proposal resolved over the wire");
            Ok(Response::Noted)
        }
        HubAction::ListTransfers => Ok(Response::HubTransfers {
            transfers: hub::transfers(store),
        }),
        HubAction::AcceptTransfer { id } => {
            let id: uuid::Uuid = id.parse().map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad transfer id"))?;
            let t = store
                .get_hub_transfer(id)
                .map_err(|_| Refusal::new(RefusalCode::BadRequest, "no such transfer"))?;
            match t.state {
                grimoire_store::HubTransferState::Offered | grimoire_store::HubTransferState::Accepted => {}
                grimoire_store::HubTransferState::Done => return Ok(Response::Noted),
                grimoire_store::HubTransferState::Declined => {
                    return Err(Refusal::new(RefusalCode::NotAllowed, "that transfer was declined").into());
                }
            }
            store.set_hub_transfer_state(id, grimoire_store::HubTransferState::Accepted)?;
            tracing::info!(admin = admin.petname, title = t.title, "hub: transfer accepted; taking the folder over");
            let endpoint = endpoint.clone();
            let store_arc = store_arc.clone();
            tokio::spawn(async move {
                if let Err(e) = transfer::hub_complete(&endpoint, &store_arc, id, None).await {
                    tracing::warn!(transfer = %id, "hub: transfer did not complete: {e:#}");
                }
            });
            Ok(Response::Noted)
        }
        HubAction::DeclineTransfer { id } => {
            let id: uuid::Uuid = id.parse().map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad transfer id"))?;
            let t = store
                .get_hub_transfer(id)
                .map_err(|_| Refusal::new(RefusalCode::BadRequest, "no such transfer"))?;
            if t.state == grimoire_store::HubTransferState::Done {
                return Err(Refusal::new(RefusalCode::NotAllowed, "that folder is already the hub's").into());
            }
            store.set_hub_transfer_state(id, grimoire_store::HubTransferState::Declined)?;
            Ok(Response::Noted)
        }
        HubAction::ListMembers => Ok(Response::HubMembers {
            members: hub::members(store)?,
        }),
        HubAction::Approve { contact_id } => {
            let node_id = endpoint.id().to_string();
            let (hub_cfg, member, share, minted) = hub::approve(store, &node_id, parse(&contact_id)?)?;
            tracing::info!(admin = admin.petname, member = member.petname, "hub: approved over the wire");
            let endpoint = endpoint.clone();
            tokio::spawn(async move {
                hub::deliver_membership(&endpoint, &hub_cfg, &member, &share, &minted).await;
            });
            Ok(Response::Noted)
        }
        HubAction::Eject { contact_id } => {
            let id = parse(&contact_id)?;
            if id == admin.id {
                return Err(Refusal::new(RefusalCode::NotAllowed, "you cannot eject yourself").into());
            }
            hub::eject(store, id)?;
            tracing::info!(admin = admin.petname, member = %id, "hub: ejected over the wire");
            Ok(Response::Noted)
        }
        HubAction::SetRole { contact_id, role } => {
            let role = grimoire_store::ContactRole::parse(&role)
                .ok_or_else(|| Refusal::new(RefusalCode::BadRequest, "role must be member or admin"))?;
            let id = parse(&contact_id)?;
            if id == admin.id && role != grimoire_store::ContactRole::Admin {
                return Err(Refusal::new(RefusalCode::NotAllowed, "you cannot demote yourself").into());
            }
            hub::set_role(store, id, role)?;
            Ok(Response::Noted)
        }
        HubAction::Invite => {
            let node_id = endpoint.id().to_string();
            let (_share, link) = super::client::mint_invite(
                store,
                &node_id,
                h.root_doc,
                grimoire_store::SharePermission::Propose,
            )?;
            Ok(Response::HubInvite { link })
        }
    }
}

/// Shared hot-session authorization: active share bound to this contact,
/// doc inside it; `need_propose` for anything beyond looking.
fn authorize_hot(
    store: &SqliteStore,
    contact: &grimoire_store::Contact,
    share_id: &str,
    doc_id: &str,
    need_propose: bool,
) -> Result<uuid::Uuid> {
    let share = authorize_share(store, contact, share_id)?;
    let doc_uuid: uuid::Uuid = doc_id
        .parse()
        .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad doc id"))?;
    if need_propose && share.permission != grimoire_store::SharePermission::Propose {
        return Err(Refusal::new(RefusalCode::ViewOnly, "share is view-only").into());
    }
    require_in_share(store, share.id, doc_uuid)?;
    if need_propose {
        require_not_relayed(store, share.id, doc_uuid)?;
    }
    Ok(doc_uuid)
}

/// Owner side of the comment channel (#64): authorize, then apply directly.
/// The ONLY thing this can create is a comment block anchored to an in-share
/// block — add_comment enforces the block type and threading by construction.
/// Comments are allowed while the doc is hot (conversation, not content).
fn handle_comment(
    store: &mut SqliteStore,
    contact: &grimoire_store::Contact,
    share_id: &str,
    target_block: &str,
    text: &str,
    reply_to: Option<String>,
) -> Result<Response> {
    let share = authorize_share(store, contact, share_id)?;
    let target_uuid: uuid::Uuid = target_block
        .parse()
        .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad block id"))?;
    if text.trim().is_empty() || text.len() > 16 * 1024 {
        return Err(Refusal::new(RefusalCode::BadRequest, "comment must be 1..16k chars").into());
    }
    let block = store
        .read_block(target_uuid)
        .map_err(|_| Refusal::new(RefusalCode::NotInShare, "block is not in this share"))?;
    require_in_share(store, share.id, block.doc_id)
        .map_err(|_| Refusal::new(RefusalCode::NotInShare, "block is not in this share"))?;
    // a relayed doc's blocks are wiped on the next pull: a comment here would vanish
    require_not_relayed(store, share.id, block.doc_id)?;
    let reply_to = reply_to
        .map(|r| r.parse::<uuid::Uuid>())
        .transpose()
        .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad reply_to id"))?;
    let comment = store.add_comment(target_uuid, contact.principal, text, reply_to)?;
    tracing::info!(
        peer = contact.pubkey,
        doc = %block.doc_id,
        "remote comment applied"
    );
    Ok(Response::Commented {
        block_id: comment.id.to_string(),
    })
}

/// Owner side of the write-back (#60): authorize, then PARK (or, on a
/// trusted share, apply as flagged yellows). Proposer ≠ approver holds
/// because the parking principal is the remote contact. Refused while the
/// doc is hot — the live session is the only writer (P2.3).
///
/// Hub (slice 2): with `on_behalf_of`, the caller is a hub carrying a
/// member's proposal. Accepted ONLY from a contact flagged `is_hub` (a plain
/// peer cannot impersonate anyone); filed under the member's principal —
/// their contact's if they are one of ours, else a remote principal keyed by
/// their pubkey — and always parked for review, whatever the hub's trust
/// tier: trust in the hub is not trust in everyone behind it.
#[allow(clippy::too_many_arguments)]
fn handle_propose(
    store: &mut SqliteStore,
    hot: &HotState,
    contact: &grimoire_store::Contact,
    share_id: &str,
    doc_id: &str,
    mut ops: Vec<grimoire_store::OpInput>,
    note: &str,
    base_epoch: Option<i64>,
    on_behalf_of: Option<OnBehalfOf>,
) -> Result<Response> {
    let share = authorize_share(store, contact, share_id)?;
    let doc_uuid: uuid::Uuid = doc_id
        .parse()
        .map_err(|_| Refusal::new(RefusalCode::BadRequest, "bad doc id"))?;
    if share.permission != grimoire_store::SharePermission::Propose {
        return Err(Refusal::new(RefusalCode::ViewOnly, "share is view-only").into());
    }
    require_in_share(store, share.id, doc_uuid)?;
    if ops.is_empty() {
        return Err(Refusal::new(RefusalCode::BadRequest, "empty proposal").into());
    }
    if let Err(m) = hot.assert_cold(doc_uuid) {
        return Err(Refusal::new(RefusalCode::DocHot, m).into());
    }
    if let Some(who) = on_behalf_of {
        if !contact.is_hub {
            return Err(Refusal::new(
                RefusalCode::NotAllowed,
                "only a hub can pass on someone else's edit",
            )
            .into());
        }
        let pk_ok = who.pubkey.len() == 64 && who.pubkey.chars().all(|c| c.is_ascii_hexdigit());
        if !pk_ok || who.name.chars().count() > 64 {
            return Err(Refusal::new(RefusalCode::BadRequest, "bad on_behalf_of").into());
        }
        let name = who.name.trim();
        let principal = store.remote_principal_for(&who.pubkey, name)?;
        let hub_name = contact.petname.clone();
        for op in &mut ops {
            // the hub already added "via hub: <name>"; make sure it is there
            // and add the machine-readable ref the status filter keys on
            let human = format!("via hub: {hub_name}");
            if !op.source_refs.iter().any(|r| r.starts_with("via hub:")) {
                op.source_refs.push(human);
            }
            op.source_refs.push(format!("hub-pubkey:{}", contact.pubkey));
            op.source_refs.push(format!("proposer-pubkey:{}", who.pubkey));
        }
        let note = format!("from {name} via {hub_name}: {note}");
        let op_ids = store.park(doc_uuid, principal, ops, &note)?;
        tracing::info!(
            hub = hub_name,
            proposer = name,
            doc = doc_id,
            ops = op_ids.len(),
            "forwarded proposal parked for review"
        );
        return Ok(Response::Proposed {
            op_ids: op_ids.into_iter().map(|u| u.to_string()).collect(),
        });
    }
    let note = format!("via share from {}: {note}", contact.petname);
    // trust tiers (#62 + maintainer):
    // - green (maintainer): the gate's normal scoring — clean ops land GREEN
    //   with no review annotation; conflicts still yellow/red. Ledgered with
    //   pre-images and surfaced in the owner's activity feed.
    // - yellow (trusted): edits apply immediately as flagged yellows.
    // - review (default): everything parks red.
    if share.trust == grimoire_store::ShareTrust::Green
        && let Some(base) = base_epoch
    {
        let outcome = store.propose(doc_uuid, base, contact.principal, ops)?;
        tracing::info!(
            peer = contact.pubkey,
            doc = doc_id,
            ops = outcome.verdicts.len(),
            "maintainer remote edit applied through the gate"
        );
        return Ok(Response::Proposed {
            op_ids: outcome
                .verdicts
                .into_iter()
                .map(|v| v.op_id.to_string())
                .collect(),
        });
    }
    if share.trust == grimoire_store::ShareTrust::Yellow
        && let Some(base) = base_epoch
    {
        let outcome = store.propose_reviewed(doc_uuid, base, contact.principal, ops)?;
        tracing::info!(
            peer = contact.pubkey,
            doc = doc_id,
            ops = outcome.verdicts.len(),
            "trusted remote proposal applied as flagged yellows"
        );
        return Ok(Response::Proposed {
            op_ids: outcome
                .verdicts
                .into_iter()
                .map(|v| v.op_id.to_string())
                .collect(),
        });
    }
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
/// (minus mirrors) is the entire universe this peer can see: docs outside it
/// are never enumerated, wikilinks pointing out of it simply won't resolve
/// downstream.
fn handle_pull(
    store: &SqliteStore,
    contact: &grimoire_store::Contact,
    share_id: &str,
    cursors: &[(String, i64)],
) -> Result<Response> {
    let share = authorize_share(store, contact, share_id)?;
    let docs = served_docs_for(store, share.id, Some(&contact.pubkey))?;
    let relay = relay_origins(store, share.id);
    let in_share: std::collections::HashSet<uuid::Uuid> = docs.iter().map(|d| d.id).collect();
    let cursor_map: std::collections::HashMap<String, i64> = cursors.iter().cloned().collect();

    // tended-ness is a gardener-scope walk per doc; compute the scope set
    // once per pull rather than re-listing gardeners for every doc
    let tended_scopes: std::collections::HashSet<uuid::Uuid> = store
        .list_gardeners()?
        .into_iter()
        .filter(|g| g.enabled)
        .filter_map(|g| g.scope_doc)
        .collect();
    let parent_of: std::collections::HashMap<uuid::Uuid, Option<uuid::Uuid>> =
        docs.iter().map(|d| (d.id, d.parent_id)).collect();
    let is_tended = |mut cur: uuid::Uuid| -> bool {
        if tended_scopes.is_empty() {
            return false;
        }
        loop {
            if tended_scopes.contains(&cur) {
                return true;
            }
            // walk out of the share too: an ancestor above the root may be tended
            let next = match parent_of.get(&cur) {
                Some(p) => *p,
                None => store.get_doc(cur).ok().and_then(|d| d.parent_id),
            };
            match next {
                Some(p) => cur = p,
                None => return false,
            }
        }
    };

    let mut metas = Vec::new();
    let mut changed = Vec::new();
    let mut budget_used = 0usize;
    let mut more = false;
    for d in &docs {
        let meta = WireDocMeta {
            id: d.id.to_string(),
            parent: d
                .parent_id
                .filter(|p| in_share.contains(p))
                .map(|p| p.to_string()),
            title: d.title.clone(),
            epoch: d.current_epoch,
            tended: is_tended(d.id),
            origin_owner: relay.get(&d.id).map(|(pk, _)| pk.clone()),
            origin_owner_name: relay.get(&d.id).map(|(_, name)| name.clone()),
        };
        let unchanged = cursor_map
            .get(&meta.id)
            .is_some_and(|c| *c >= d.current_epoch);
        if !unchanged {
            // page: once the budget is spent, later changed docs wait for the
            // next pull (the grantee keeps their cursors, so they stay "changed")
            if budget_used >= PULL_BUDGET && !changed.is_empty() {
                more = true;
            } else {
                let blocks: Vec<grimoire_store::MirrorBlock> = store
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
                // content dominates the serialized size; ~120 bytes of ids,
                // keys and JSON punctuation per block
                budget_used += blocks
                    .iter()
                    .map(|b| b.content.len() + 120)
                    .sum::<usize>();
                changed.push(WireDoc {
                    meta: meta.clone(),
                    blocks,
                });
            }
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
        more,
    })
}

/// How often a live bridge re-checks that the peer is still a contact with
/// an active share on this doc (and re-reads its permission). A revoke does
/// not wait for this: `HotState::drop_bridges_*` cuts the stream at once.
#[cfg(not(test))]
pub(super) const BRIDGE_REAUTH: std::time::Duration = std::time::Duration::from_secs(10);
#[cfg(test)]
pub(super) const BRIDGE_REAUTH: std::time::Duration = std::time::Duration::from_millis(200);

/// Authorize a bridge participant. Returns the doc, whether the peer holds
/// only a `view` share, and the share id. Session = consent: a view
/// participant may still write while the session's `viewers_write` is on
/// (the owner opened the doc up); when the owner flips to "watch only",
/// their frames are filtered to presence + sync requests. `propose`
/// participates fully regardless.
pub(super) fn bridge_authorized(
    store: &Arc<Mutex<SqliteStore>>,
    peer: &str,
    share: &str,
    doc: &str,
) -> Result<(uuid::Uuid, bool, uuid::Uuid)> {
    let s = store.lock().unwrap_or_else(|p| p.into_inner());
    let contact = s
        .contact_by_pubkey(peer)?
        .filter(|c| !c.revoked)
        .ok_or_else(|| Refusal::new(RefusalCode::UnknownPeer, "unknown peer"))?;
    let sh = authorize_share(&s, &contact, share)?;
    let doc_id = authorize_hot(&s, &contact, share, doc, false)?;
    let read_only = sh.permission != grimoire_store::SharePermission::Propose;
    Ok((doc_id, read_only, sh.id))
}

/// Owner side of the hot bridge (#66): one bi-stream per remote participant.
/// Header frame (JSON {share, doc}) authorizes; then raw y-sync frames flow
/// both ways, length-prefixed (4-byte LE), through the SAME session paths as
/// local websockets. Authorization is re-checked every `BRIDGE_REAUTH` so a
/// revoke ends the stream instead of living on until it closes. A `view`
/// participant is read-only: inbound frames are filtered to presence and
/// sync requests only (`hot::readonly_filter`), so they can never inject
/// content.
async fn handle_hot_bridge(
    conn: iroh::endpoint::Connection,
    peer: &str,
    store: Arc<Mutex<SqliteStore>>,
    hot: HotState,
) -> Result<()> {
    // 0.7.2: the header is read under the pre-auth cap and a deadline — the
    // peer is not authorized until `bridge_authorized` says so
    let (mut send, mut recv) = tokio::time::timeout(READ_TIMEOUT, conn.accept_bi())
        .await
        .context("bridge: no stream opened in time")??;
    let header = tokio::time::timeout(READ_TIMEOUT, read_frame_capped(&mut recv, PRE_AUTH_FRAME))
        .await
        .context("bridge: header timed out")??
        .context("bridge closed before header")?;
    #[derive(Deserialize)]
    struct Header {
        share: String,
        doc: String,
    }
    let header: Header = serde_json::from_slice(&header).context("bad bridge header")?;
    let (doc_id, mut view_share, share_id) =
        bridge_authorized(&store, peer, &header.share, &header.doc)?;
    let (mut rx, hello) = hot
        .connect_as(doc_id, Some(peer))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    write_frame(&mut send, &hello).await?;
    tracing::info!(peer, %doc_id, view_share, "hot bridge joined");
    // registered so a revoke cuts this stream now, not at the next re-auth
    let (bridge_id, cancel) = hot.register_bridge(doc_id, peer, share_id);
    struct Unregister<'a>(&'a HotState, u64);
    impl Drop for Unregister<'_> {
        fn drop(&mut self) {
            self.0.unregister_bridge(self.1);
        }
    }
    let _unregister = Unregister(&hot, bridge_id);

    let fan_out = tokio::spawn(async move {
        while let Ok(frame) = rx.recv().await {
            if write_frame(&mut send, &frame).await.is_err() {
                break;
            }
        }
    });
    // re-auth re-reads the permission too: a propose→view downgrade applies
    // to the live bridge, not only to the next one
    let reauth = |view_share: &mut bool| -> Result<()> {
        let (_, ro, _) = bridge_authorized(&store, peer, &header.share, &header.doc)
            .context("bridge authorization lapsed")?;
        if ro != *view_share {
            tracing::info!(peer, %doc_id, read_only = ro, "bridge permission changed");
            *view_share = ro;
        }
        Ok(())
    };
    let mut last_auth = std::time::Instant::now();
    let result = loop {
        let frame = tokio::select! {
            f = read_frame(&mut recv) => f,
            _ = cancel.notified() => {
                break Err(anyhow::anyhow!("bridge cut: share or contact revoked"));
            }
            _ = tokio::time::sleep(BRIDGE_REAUTH) => {
                if let Err(e) = reauth(&mut view_share) {
                    break Err(e);
                }
                last_auth = std::time::Instant::now();
                continue;
            }
        };
        match frame {
            Ok(Some(frame)) => {
                if last_auth.elapsed() > BRIDGE_REAUTH {
                    if let Err(e) = reauth(&mut view_share) {
                        break Err(e);
                    }
                    last_auth = std::time::Instant::now();
                }
                // a view participant writes only while the owner leaves the
                // session open to everyone; under "watch only" their frames
                // are cut to presence + sync requests (evaluated per frame so
                // an owner's flip takes effect immediately)
                let read_only = view_share && !hot.viewers_write(doc_id).unwrap_or(false);
                let frame = if read_only {
                    match crate::hot::readonly_filter(&frame) {
                        Some(f) => f,
                        None => continue, // nothing allowed in this frame
                    }
                } else {
                    frame
                };
                if !hot.handle_frame(doc_id, &frame) {
                    break Ok(());
                }
            }
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    fan_out.abort();
    result
}

pub(super) async fn write_frame(send: &mut iroh::endpoint::SendStream, data: &[u8]) -> Result<()> {
    send.write_all(&(data.len() as u32).to_le_bytes()).await?;
    send.write_all(data).await?;
    Ok(())
}

/// Largest length-prefixed bridge frame (a full Yjs state of a big doc).
const BRIDGE_FRAME_MAX: usize = 8 * 1024 * 1024;

pub(super) async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Result<Option<Vec<u8>>> {
    read_frame_capped(recv, BRIDGE_FRAME_MAX).await
}

async fn read_frame_capped(recv: &mut iroh::endpoint::RecvStream, cap: usize) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(_) => return Ok(None), // stream closed
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > cap {
        anyhow::bail!("bridge frame too large ({len} > {cap})");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await.context("torn frame")?;
    Ok(Some(buf))
}

#[cfg(test)]
mod dedupe_tests {
    use super::*;

    #[test]
    fn dedupe_evicts_oldest_first_and_keeps_a_retry_findable() {
        let mut c = DedupeCache::new(3);
        for i in 0..3 {
            c.insert(format!("k{i}"), Response::Pong);
        }
        assert_eq!(c.len(), 3);
        c.insert("k3".into(), Response::Pong);
        assert_eq!(c.len(), 3, "bounded");
        assert!(c.get("k0").is_none(), "oldest evicted");
        assert!(c.get("k1").is_some() && c.get("k3").is_some(), "the rest survive — the clear-all did not");
        // re-inserting a live key does not grow the order queue
        c.insert("k1".into(), Response::Noted);
        assert!(matches!(c.get("k1"), Some(Response::Noted)));
        c.insert("k4".into(), Response::Pong);
        assert_eq!(c.len(), 3);
        assert_eq!(c.order.len(), 3);
    }
}
