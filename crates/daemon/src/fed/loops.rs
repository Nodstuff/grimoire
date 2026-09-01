//! Grantee-side background loops: periodic pull, outbound-proposal status
//! refresh, and the async join retry (ADR 0002 decision 6).

use super::client::{join_once, pull_share, request};
use super::wire::{Refusal, RefusalCode, Request, Response, Ticket};
use anyhow::Result;
use grimoire_store::{BlockStore, SqliteStore};
use iroh::{Endpoint, EndpointAddr};
use std::sync::{Arc, Mutex};

use super::client::PullSummary;

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
        // resolved = the annotation is no longer open. Key acceptance off the
        // annotation status, NOT the applied flag — a declined yellow was
        // applied and then reverted, and must read as declined.
        let resolved: Vec<_> = statuses
            .iter()
            .filter(|s| s.review.as_deref() != Some("open"))
            .collect();
        if resolved.len() < prop.op_ids.len() {
            continue; // partially reviewed: wait for the rest
        }
        let accepted = resolved
            .iter()
            .filter(|s| match s.review.as_deref() {
                Some("accepted") => true,
                Some("declined") => false,
                // no annotation ever existed (a green under auto policy):
                // applied is the truth
                _ => s.applied,
            })
            .count();
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

/// The owner told us (by CODE, never by text) that this share no longer
/// exists for us: drop every mirror row it still claims and soft-delete
/// those docs locally. Active shares reclaim their docs during their own
/// pulls, so only truly orphaned mirrors go. Returns the docs dropped.
pub fn drop_dead_share(store: &mut SqliteStore, share_id: uuid::Uuid) -> Vec<uuid::Uuid> {
    let orphans: Vec<uuid::Uuid> = store
        .list_mirrors()
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.share_id == share_id)
        .map(|m| m.doc_id)
        .collect();
    for doc in &orphans {
        tracing::info!(%doc, %share_id, "share gone upstream; dropping mirror");
        store.remove_mirror(*doc).ok();
        store.delete_doc(*doc).ok();
    }
    orphans
}

/// Does this pull failure mean "this share is gone for us"? Only typed
/// refusals count: `ShareRevoked` (the owner revoked the share) and
/// `UnknownPeer` (the owner revoked or dropped US as a contact).
fn share_is_dead(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<Refusal>().map(|r| r.code),
        Some(RefusalCode::ShareRevoked | RefusalCode::UnknownPeer)
    )
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
        let contacts = s.list_contacts().unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        mirrors
            .into_iter()
            .filter(|m| seen.insert((m.owner, m.share_id)))
            .filter_map(|m| {
                let c = contacts.iter().find(|c| c.id == m.owner)?.clone();
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
            && share_is_dead(e)
        {
            dead_shares.push(share_id);
        }
        out.push((share_id, res));
    }
    // cleanup: docs still claimed only by a dead share (active shares
    // reclaimed theirs during the pulls above) are gone from our view
    if !dead_shares.is_empty() {
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        for share_id in dead_shares {
            drop_dead_share(&mut s, share_id);
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
