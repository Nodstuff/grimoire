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

use super::runtime::Runtime;
use super::wire::{
    ALPN, Frame, HOT_ALPN, MAX_FRAME, PROTOCOL_VERSION, PULL_BUDGET, Refusal, RefusalCode,
    Request, Response, WireDoc, WireDocMeta, hash_secret,
};
use crate::hot::HotState;
use anyhow::{Context, Result};
use grimoire_store::{BlockStore, SqliteStore};
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

/// Accept loop. Spawned once per daemon; lives until the endpoint closes.
pub async fn serve(
    endpoint: Endpoint,
    store: Arc<Mutex<SqliteStore>>,
    hot: HotState,
    runtime: Runtime,
) {
    tracing::info!("federation endpoint listening (node id {})", endpoint.id());
    let dedupe: Dedupe = Arc::new(Mutex::new(DedupeCache::new(DEDUPE_CAP)));
    while let Some(incoming) = endpoint.accept().await {
        let store = store.clone();
        let dedupe = dedupe.clone();
        let hot = hot.clone();
        let runtime = runtime.clone();
        let ep = endpoint.clone();
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
        let (mut send, mut recv) = match conn.accept_bi().await {
            Ok(s) => s,
            // peer closed: normal end of conversation
            Err(_) => return Ok(()),
        };
        let raw = recv.read_to_end(MAX_FRAME).await?;
        let response = match serde_json::from_slice::<Frame<Request>>(&raw) {
            Err(e) => Response::refused(RefusalCode::BadRequest, format!("bad frame: {e}")),
            Ok(frame) if frame.v != PROTOCOL_VERSION => Response::refused(
                RefusalCode::Version,
                format!(
                    "protocol version {} not supported (this instance speaks {})",
                    frame.v, PROTOCOL_VERSION
                ),
            ),
            Ok(frame) => dispatch(frame.msg, peer, &store, &dedupe, &hot, &endpoint, &runtime),
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

fn dispatch(
    req: Request,
    peer: &str,
    store_arc: &Arc<Mutex<SqliteStore>>,
    dedupe: &Dedupe,
    hot: &HotState,
    endpoint: &Endpoint,
    runtime: &Runtime,
) -> Response {
    let mut store = store_arc.lock().unwrap_or_else(|p| p.into_inner());
    let contact = match store.contact_by_pubkey(peer) {
        Ok(c) => c.filter(|c| !c.revoked),
        Err(e) => {
            return Response::refused(RefusalCode::Other, format!("store error: {e}"));
        }
    };
    // the one request an unknown peer may make
    if let Request::Redeem { secret, petname } = req {
        return match store.redeem_invite(&hash_secret(&secret), peer, &petname) {
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
                Response::refused(RefusalCode::InviteInvalid, e.to_string())
            }
        };
    }
    // everything else needs a live contact
    let Some(contact) = contact else {
        tracing::warn!(peer, "unauthenticated request refused");
        return unknown_peer();
    };
    match req {
        Request::Redeem { .. } => unreachable!("handled above"),
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
        } => {
            // retry safety: the same request_id returns the original outcome
            let dedupe_key = request_id.map(|r| format!("{peer}:{r}"));
            if let Some(key) = &dedupe_key {
                let cache = dedupe.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(prior) = cache.get(key) {
                    return prior.clone();
                }
            }
            let res = match handle_propose(&mut store, hot, &contact, &share, &doc, ops, &note, base_epoch)
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
                    Err(e) => return Response::refused(RefusalCode::Other, e.to_string()),
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
                    return Response::refused(
                        RefusalCode::StaleBase,
                        format!(
                            "your copy is at epoch {}, the owner is at {frozen_epoch}: pull first",
                            base_epoch.map(|e| e.to_string()).unwrap_or_else(|| "?".into())
                        ),
                    );
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
                    return Response::refused(
                        RefusalCode::NotAllowed,
                        "only the owner or whoever started the session can end it",
                    );
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
                return Response::refused(RefusalCode::BadRequest, "bad share id");
            };
            let Some(perm) = grimoire_store::SharePermission::parse(&permission) else {
                return Response::refused(RefusalCode::BadRequest, "bad permission");
            };
            if secret.is_empty() || secret.len() > 128 || root_title.chars().count() > 300 {
                return Response::refused(RefusalCode::BadRequest, "bad offer");
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
                    runtime.push_event("share_offered", offer.id, root_title, contact.petname.clone());
                    Response::Noted
                }
                Err(e) => Response::refused(RefusalCode::Other, e.to_string()),
            }
        }
        Request::ProposalStatus { op_ids } => {
            let ids: Vec<uuid::Uuid> = op_ids.iter().filter_map(|s| s.parse().ok()).collect();
            match store.op_statuses(&ids) {
                // disclose only the asker's own ops
                Ok(statuses) => Response::ProposalStatuses {
                    statuses: statuses
                        .into_iter()
                        .filter(|s| s.principal == contact.principal)
                        .collect(),
                },
                Err(e) => Response::refused(RefusalCode::Other, e.to_string()),
            }
        }
    }
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
pub(super) fn served_docs(store: &SqliteStore, share_id: uuid::Uuid) -> Result<Vec<grimoire_store::Doc>> {
    let mirrors: std::collections::HashSet<uuid::Uuid> = store
        .list_mirrors()?
        .into_iter()
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

fn require_in_share(store: &SqliteStore, share_id: uuid::Uuid, doc_id: uuid::Uuid) -> Result<()> {
    if served_docs(store, share_id)?.iter().any(|d| d.id == doc_id) {
        Ok(())
    } else {
        Err(Refusal::new(RefusalCode::NotInShare, "doc is not in this share").into())
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
#[allow(clippy::too_many_arguments)]
fn handle_propose(
    store: &mut SqliteStore,
    hot: &HotState,
    contact: &grimoire_store::Contact,
    share_id: &str,
    doc_id: &str,
    ops: Vec<grimoire_store::OpInput>,
    note: &str,
    base_epoch: Option<i64>,
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
    let docs = served_docs(store, share.id)?;
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
    let (mut send, mut recv) = conn.accept_bi().await?;
    let header = read_frame(&mut recv).await?.context("bridge closed before header")?;
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

pub(super) async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(_) => return Ok(None), // stream closed
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > 8 * 1024 * 1024 {
        anyhow::bail!("bridge frame too large");
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
