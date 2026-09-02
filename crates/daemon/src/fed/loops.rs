//! Federation background loops.
//!
//! Grantee side: the adaptive pull (5s for shares the UI is looking at,
//! 120s sweep for the rest, instant when nudged), the outbound-proposal
//! status refresh, and the async join retry (ADR 0002 decision 6).
//!
//! Owner side: the change detector that NUDGES grantees — one 1s tick diffs
//! every active share's docs (epoch moved → `doc_changed`, new id →
//! `doc_added`, cold→hot → `live_started`) and dials the contact. Nudges are
//! best-effort; the grantee's poll is the safety net.

use super::client::{join_once, pull_share, request};
use super::runtime::Runtime;
use super::server::served_docs;
use super::wire::{NotifyKind, Refusal, RefusalCode, Request, Response, Ticket};
use crate::hot::HotState;
use anyhow::Result;
use grimoire_store::{BlockStore, SqliteStore};
use iroh::{Endpoint, EndpointAddr};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

use super::client::PullSummary;

/// Fast tier: shares the UI has open (focus heartbeats).
pub const FOCUSED_PULL_EVERY: Duration = Duration::from_secs(5);
/// Cold tier: everything else.
pub const SWEEP_EVERY: Duration = Duration::from_secs(120);
/// Owner-side change detector tick (also the nudge coalescing window).
pub const NOTIFY_TICK: Duration = Duration::from_secs(1);

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
pub fn drop_dead_share(store: &mut SqliteStore, share_id: Uuid) -> Vec<Uuid> {
    let orphans: Vec<Uuid> = store
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

/// (owner contact, share) pairs we hold mirrors for, non-revoked owners only.
fn share_groups(store: &Arc<Mutex<SqliteStore>>) -> Vec<(grimoire_store::Contact, Uuid)> {
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
}

/// Pull a set of (owner, share) groups; drop shares the owner says are dead.
async fn pull_groups(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    groups: Vec<(grimoire_store::Contact, Uuid)>,
) -> Vec<(Uuid, Result<PullSummary>)> {
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

/// Pull every share we hold mirrors for (the full sweep).
pub async fn pull_all_once(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
) -> Vec<(Uuid, Result<PullSummary>)> {
    let groups = share_groups(store);
    pull_groups(endpoint, store, groups).await
}

/// Adaptive background sync (#59 + realtime): every 5s pull the shares the UI
/// is focused on; every 120s sweep everything (and refresh outbound
/// proposal statuses). Nudges from owners trigger pulls independently, so
/// this is the safety net, not the primary path.
pub async fn pull_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>, runtime: Runtime) {
    let mut last_sweep = std::time::Instant::now();
    loop {
        tokio::time::sleep(FOCUSED_PULL_EVERY).await;
        let sweep = last_sweep.elapsed() >= SWEEP_EVERY;
        let results = if sweep {
            last_sweep = std::time::Instant::now();
            refresh_outbound(&endpoint, &store).await;
            pull_all_once(&endpoint, &store).await
        } else {
            let focused = runtime.focused_shares();
            if focused.is_empty() {
                continue;
            }
            let groups: Vec<_> = share_groups(&store)
                .into_iter()
                .filter(|(_, sh)| focused.contains(sh))
                .collect();
            pull_groups(&endpoint, &store, groups).await
        };
        for (share, res) in results {
            match res {
                Ok(s) if s.changed > 0 || s.removed > 0 => {
                    tracing::info!(%share, changed = s.changed, removed = s.removed, sweep, "pulled");
                }
                Ok(_) => {}
                // WARN, not debug: a pull that keeps failing is how a grantee
                // ends up with titles but no content — it must be visible
                Err(e) => tracing::warn!(%share, "pull failed: {e:#}"),
            }
        }
    }
}

/// What the owner-side detector remembers per (share, doc).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DocMark {
    pub epoch: i64,
    pub hot: bool,
}

/// Pure diff for the change detector: given the last marks for one share and
/// the current view, which nudges are due? `seeded` false = first observation
/// of this share (nothing is "new" yet — just remember it).
pub fn diff_share(
    last: &HashMap<Uuid, DocMark>,
    now: &[(Uuid, DocMark)],
    seeded: bool,
) -> Vec<(Uuid, NotifyKind)> {
    let mut out = Vec::new();
    if !seeded {
        return out;
    }
    for (doc, mark) in now {
        match last.get(doc) {
            None => out.push((*doc, NotifyKind::DocAdded)),
            Some(prev) => {
                if mark.epoch > prev.epoch {
                    out.push((*doc, NotifyKind::DocChanged));
                }
                if mark.hot && !prev.hot {
                    out.push((*doc, NotifyKind::LiveStarted));
                }
            }
        }
    }
    out
}

/// Owner-side change detector + nudger. One 1s tick over every active share:
/// diffs docs (epoch / presence / hotness) against the last tick and dials
/// the share's contact with `Notify` for each change. Best-effort, spawned
/// per contact with a timeout so an offline grantee never stalls the loop.
pub async fn notify_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>, hot: HotState) {
    // (share id) → (doc id → mark); a share's first observation seeds silently
    let mut marks: HashMap<Uuid, HashMap<Uuid, DocMark>> = HashMap::new();
    loop {
        tokio::time::sleep(NOTIFY_TICK).await;
        // snapshot under one lock: active shares with a contact, their docs
        let snapshot: Vec<(grimoire_store::Share, grimoire_store::Contact, Vec<(Uuid, String, DocMark)>)> = {
            let s = store.lock().unwrap_or_else(|p| p.into_inner());
            let contacts = s.list_contacts().unwrap_or_default();
            s.list_shares()
                .unwrap_or_default()
                .into_iter()
                .filter(|sh| sh.state == grimoire_store::ShareState::Active)
                .filter_map(|sh| {
                    let c = contacts.iter().find(|c| Some(c.id) == sh.contact && !c.revoked)?.clone();
                    let docs = served_docs(&s, sh.id).ok()?;
                    let view: Vec<(Uuid, String, DocMark)> = docs
                        .into_iter()
                        .map(|d| {
                            (
                                d.id,
                                d.title,
                                DocMark {
                                    epoch: d.current_epoch,
                                    hot: hot.is_hot(d.id),
                                },
                            )
                        })
                        .collect();
                    Some((sh, c, view))
                })
                .collect()
        };
        let live_share_ids: std::collections::HashSet<Uuid> = snapshot.iter().map(|(sh, _, _)| sh.id).collect();
        marks.retain(|id, _| live_share_ids.contains(id)); // forget revoked shares
        for (share, contact, view) in snapshot {
            let seeded = marks.contains_key(&share.id);
            let last = marks.entry(share.id).or_default();
            let now: Vec<(Uuid, DocMark)> = view.iter().map(|(d, _, m)| (*d, *m)).collect();
            let due = diff_share(last, &now, seeded);
            *last = now.iter().copied().collect();
            if due.is_empty() {
                continue;
            }
            let titles: HashMap<Uuid, String> = view.into_iter().map(|(d, t, _)| (d, t)).collect();
            let Ok(peer_id) = contact.pubkey.parse::<iroh::EndpointId>() else {
                continue;
            };
            for (doc, kind) in due {
                let ep = endpoint.clone();
                let req = Request::Notify {
                    share: share.id.to_string(),
                    doc: doc.to_string(),
                    title: titles.get(&doc).cloned().unwrap_or_default(),
                    kind,
                };
                let petname = contact.petname.clone();
                tokio::spawn(async move {
                    let r = tokio::time::timeout(
                        Duration::from_secs(5),
                        request(&ep, EndpointAddr::from(peer_id), req),
                    )
                    .await;
                    match r {
                        Ok(Ok(Response::Noted)) => tracing::debug!(%doc, ?kind, to = petname, "nudged"),
                        Ok(Ok(other)) => tracing::debug!(%doc, ?kind, to = petname, "nudge not accepted: {other:?}"),
                        Ok(Err(e)) => tracing::debug!(%doc, to = petname, "nudge failed (offline?): {e:#}"),
                        Err(_) => tracing::debug!(%doc, to = petname, "nudge timed out"),
                    }
                });
            }
        }
    }
}

/// Background retry for joins whose owner was offline (async redeem,
/// ADR 0002 decision 6). Every failure is recorded; success removes the row.
pub async fn join_retry_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    const RETRY_EVERY: Duration = Duration::from_secs(60);
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
                    // a DEAD invite (already redeemed — e.g. the same link
                    // pasted twice — expired, or unknown) can never succeed:
                    // drop it instead of hammering the owner every 60s forever
                    let dead = matches!(
                        e.downcast_ref::<Refusal>().map(|r| r.code),
                        Some(RefusalCode::InviteInvalid)
                    );
                    if dead {
                        tracing::warn!("dropping pending join: {e:#}");
                        s.remove_pending_join(join.id).ok();
                    } else {
                        s.record_join_attempt(join.id, &format!("{e:#}")).ok();
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_share_detects_added_changed_and_live_started_but_seeds_silently() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let cold = |e| DocMark { epoch: e, hot: false };
        // first observation: nothing is due, whatever is there
        let due = diff_share(&HashMap::new(), &[(a, cold(3))], false);
        assert!(due.is_empty(), "first tick seeds silently");
        let last: HashMap<Uuid, DocMark> = [(a, cold(3))].into_iter().collect();
        // unchanged: nothing
        assert!(diff_share(&last, &[(a, cold(3))], true).is_empty());
        // epoch moved: doc_changed
        assert_eq!(diff_share(&last, &[(a, cold(4))], true), vec![(a, NotifyKind::DocChanged)]);
        // new doc: doc_added
        assert_eq!(diff_share(&last, &[(a, cold(3)), (b, cold(1))], true), vec![(b, NotifyKind::DocAdded)]);
        // went hot (no epoch change): live_started
        assert_eq!(
            diff_share(&last, &[(a, DocMark { epoch: 3, hot: true })], true),
            vec![(a, NotifyKind::LiveStarted)]
        );
        // stayed hot: no repeat nudge
        let hot_last: HashMap<Uuid, DocMark> = [(a, DocMark { epoch: 3, hot: true })].into_iter().collect();
        assert!(diff_share(&hot_last, &[(a, DocMark { epoch: 3, hot: true })], true).is_empty());
        // hot AND edited in the same tick: both nudges
        let both = diff_share(&last, &[(a, DocMark { epoch: 9, hot: true })], true);
        assert!(both.contains(&(a, NotifyKind::DocChanged)) && both.contains(&(a, NotifyKind::LiveStarted)));
    }
}
