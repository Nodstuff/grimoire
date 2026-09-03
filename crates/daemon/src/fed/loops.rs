//! Federation background loops.
//!
//! Grantee side: the adaptive pull (5s for shares the UI is looking at,
//! 120s sweep for the rest, instant when nudged), the outbound-proposal
//! status refresh, and the async join retry (ADR 0002 decision 6).
//!
//! Owner side: the change detector that NUDGES grantees — one 1s tick names
//! the docs that moved and diffs only the shares containing them (epoch
//! moved → `doc_changed`, new id → `doc_added`, cold→hot → `live_started`),
//! then dials the contact. Nudges are best-effort; the grantee's poll is the
//! safety net. Every loop runs under `supervise` (restart on panic/exit).

use super::client::{join_once, pull_share, request};
use super::runtime::Runtime;
use super::server::served_docs_for;
use super::wire::{NotifyItem, NotifyKind, Refusal, RefusalCode, Request, Response, Ticket};
use crate::hot::HotState;
use crate::store_ext::{blocking, with_store};
use anyhow::Result;
use futures_util::StreamExt;
use grimoire_store::{BlockStore, SqliteStore};
use iroh::{Endpoint, EndpointAddr};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::client::PullSummary;

/// Fast tier: shares the UI has open (focus heartbeats).
pub const FOCUSED_PULL_EVERY: Duration = Duration::from_secs(5);
/// Cold tier: everything else.
pub const SWEEP_EVERY: Duration = Duration::from_secs(120);
/// Owner-side change detector tick (also the nudge coalescing window).
pub const NOTIFY_TICK: Duration = Duration::from_secs(1);
/// How many owners a sweep dials at once. One unreachable owner costs its
/// own 10s dial timeout, not everyone else's.
const PULL_CONCURRENCY: usize = 4;

/// Pull one share because its owner nudged us. At most one pull per share
/// runs at a time; nudges arriving meanwhile collapse into a single
/// follow-up pull (`Runtime::begin_pull` / `finish_pull`).
pub fn spawn_nudged_pull(
    endpoint: Endpoint,
    store: Arc<Mutex<SqliteStore>>,
    runtime: Runtime,
    owner: grimoire_store::Contact,
    share_id: Uuid,
) {
    if !runtime.begin_pull(share_id) {
        tracing::debug!(%share_id, "nudge while pulling; will pull again after");
        return;
    }
    tokio::spawn(async move {
        let Ok(id) = owner.pubkey.parse::<iroh::EndpointId>() else {
            runtime.finish_pull(share_id);
            return;
        };
        loop {
            match pull_share(&endpoint, &store, EndpointAddr::from(id), &owner, share_id).await {
                Ok(s) => tracing::debug!(%share_id, changed = s.changed, "nudged pull"),
                // an explicit revoke arrives as a nudge too (the owner tells us
                // so we don't keep stale docs until the sweep): drop at once
                Err(e) if refusal_code_of(&e) == Some(RefusalCode::ShareRevoked) => {
                    let dropped = with_store(&store, move |s| drop_dead_share(s, share_id)).await;
                    tracing::info!(%share_id, dropped = dropped.len(), "share revoked upstream; mirrors dropped");
                }
                Err(e) => tracing::warn!(%share_id, "nudged pull failed: {e:#}"),
            }
            if !runtime.finish_pull(share_id) {
                break;
            }
        }
    });
}

/// Refresh the state of pending outbound proposals from their owners; any
/// accepted content arrives via the normal pull.
pub async fn refresh_outbound(endpoint: &Endpoint, store: &Arc<Mutex<SqliteStore>>) {
    let pending = with_store(store, |s| s.list_outbound_proposals(true).unwrap_or_default()).await;
    for prop in pending {
        let owner = {
            let owner = prop.owner;
            with_store(store, move |s| {
                s.list_contacts()
                    .ok()
                    .and_then(|cs| cs.into_iter().find(|c| c.id == owner))
            })
            .await
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
        let id = prop.id;
        with_store(store, move |s| s.set_outbound_state(id, state).ok()).await;
        tracing::info!(doc = %prop.doc_id, state, "outbound proposal resolved");
    }
}

/// The typed code behind a pull error, if it was a refusal.
fn refusal_code_of(e: &anyhow::Error) -> Option<RefusalCode> {
    e.downcast_ref::<super::wire::Refusal>().map(|r| r.code)
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
    // hub: a member unpublished (revoked their share) — the relay forgets it
    // too, so other members lose those docs on their next pull
    store.remove_hub_publication(share_id).ok();
    orphans
}

/// Decides when a pull refusal means "this share is gone for us".
///
/// `ShareRevoked` is explicit and immediate. `UnknownPeer` is ambiguous —
/// the owner may have revoked us, or restored a database that has not seen
/// us yet — so it must be seen on two pulls at least one sweep apart before
/// mirrors are dropped (they revive on re-join either way, but a transient
/// blip should not empty the tree).
#[derive(Default)]
pub struct DeadPeerTracker {
    first_unknown: HashMap<Uuid, Instant>,
}

impl DeadPeerTracker {
    /// Record the outcome of a pull; `true` = drop this share's mirrors now.
    pub fn observe(&mut self, share: Uuid, res: &Result<PullSummary>, now: Instant) -> bool {
        let code = res
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<Refusal>())
            .map(|r| r.code);
        match code {
            Some(RefusalCode::ShareRevoked) => {
                self.first_unknown.remove(&share);
                true
            }
            Some(RefusalCode::UnknownPeer) => {
                let first = *self.first_unknown.entry(share).or_insert(now);
                now.duration_since(first) >= SWEEP_EVERY
            }
            // anything else — success, offline, other refusals — resets
            _ => {
                self.first_unknown.remove(&share);
                false
            }
        }
    }
}

/// (owner contact, share) pairs we hold mirrors for, non-revoked owners only.
async fn share_groups(store: &Arc<Mutex<SqliteStore>>) -> Vec<(grimoire_store::Contact, Uuid)> {
    with_store(store, |s| {
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
    })
    .await
}

/// Pull a set of (owner, share) groups — `PULL_CONCURRENCY` owners at a time
/// — and drop shares the owner says are dead (per `DeadPeerTracker`).
async fn pull_groups(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    groups: Vec<(grimoire_store::Contact, Uuid)>,
    dead: &Mutex<DeadPeerTracker>,
) -> Vec<(Uuid, Result<PullSummary>)> {
    let out: Vec<(Uuid, Result<PullSummary>)> = futures_util::stream::iter(groups)
        .map(|(owner, share_id)| async move {
            let res = match owner.pubkey.parse::<iroh::EndpointId>() {
                Ok(id) => pull_share(endpoint, store, EndpointAddr::from(id), &owner, share_id).await,
                Err(_) => Err(anyhow::anyhow!("contact has a malformed pubkey")),
            };
            // sync health for the shares page: success clears, failure records
            {
                let err = res.as_ref().err().map(|e| format!("{e:#}"));
                with_store(store, move |s| s.set_mirror_sync_result(share_id, err.as_deref()).ok()).await;
            }
            (share_id, res)
        })
        .buffer_unordered(PULL_CONCURRENCY)
        .collect()
        .await;
    // cleanup: docs still claimed only by a dead share (active shares
    // reclaimed theirs during the pulls above) are gone from our view
    let now = Instant::now();
    let dead_shares: Vec<Uuid> = {
        let mut d = dead.lock().unwrap_or_else(|p| p.into_inner());
        out.iter()
            .filter(|(share, res)| d.observe(*share, res, now))
            .map(|(share, _)| *share)
            .collect()
    };
    if !dead_shares.is_empty() {
        with_store(store, move |s| {
            for share_id in dead_shares {
                drop_dead_share(s, share_id);
            }
        })
        .await;
    }
    out
}

/// Pull every share we hold mirrors for (the full sweep). One-shot callers
/// (admin "pull now") get a fresh tracker: a single `UnknownPeer` never drops
/// anything from here.
pub async fn pull_all_once(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
) -> Vec<(Uuid, Result<PullSummary>)> {
    let groups = share_groups(store).await;
    pull_groups(endpoint, store, groups, &Mutex::new(DeadPeerTracker::default())).await
}

/// Adaptive background sync (#59 + realtime): every 5s pull the shares the UI
/// is focused on; every 120s sweep everything (and refresh outbound
/// proposal statuses). Nudges from owners trigger pulls independently, so
/// this is the safety net, not the primary path.
pub async fn pull_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>, runtime: Runtime) {
    supervise("pull", move || pull_loop_inner(endpoint.clone(), store.clone(), runtime.clone())).await
}

async fn pull_loop_inner(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>, runtime: Runtime) {
    let mut last_sweep = Instant::now();
    let dead = Mutex::new(DeadPeerTracker::default());
    loop {
        tokio::time::sleep(FOCUSED_PULL_EVERY).await;
        let sweep = last_sweep.elapsed() >= SWEEP_EVERY;
        let results = if sweep {
            last_sweep = Instant::now();
            {
                // invites v2: offers die with their invite (7 days)
                with_store(&store, |s| {
                    if let Ok(n) = s.expire_share_offers()
                        && n > 0
                    {
                        tracing::info!(expired = n, "share offers expired");
                    }
                })
                .await;
            }
            refresh_outbound(&endpoint, &store).await;
            // 0.7.2: a flipped transfer whose Ready reply was lost is re-announced
            super::transfer::resend_ready(&endpoint, &store, None).await;
            {
                let store = store.clone();
                blocking(move || prune_hub_forwards(&store)).await;
            }
            pull_groups(&endpoint, &store, share_groups(&store).await, &dead).await
        } else {
            let focused = runtime.focused_shares();
            if focused.is_empty() {
                continue;
            }
            let groups: Vec<_> = share_groups(&store)
                .await
                .into_iter()
                .filter(|(_, sh)| focused.contains(sh))
                .collect();
            pull_groups(&endpoint, &store, groups, &dead).await
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

/// Group one tick's due nudges into one batch per share, titles attached.
pub fn batch_nudges(
    due: &[(Uuid, NotifyKind)],
    titles: &HashMap<Uuid, String>,
) -> Vec<NotifyItem> {
    due.iter()
        .map(|(doc, kind)| NotifyItem {
            doc: doc.to_string(),
            title: titles.get(doc).cloned().unwrap_or_default(),
            kind: *kind,
        })
        .collect()
}

/// Deliver one share's nudges to its contact: ONE dial carrying the batch.
/// An owner from before batching answers `Unsupported`/`BadRequest` to the
/// new variant; then (and only then) fall back to one `Notify` per item.
pub async fn send_nudges(endpoint: Endpoint, peer_id: iroh::EndpointId, share: Uuid, items: Vec<NotifyItem>, petname: String) {
    let n = items.len();
    let batch = Request::NotifyBatch {
        share: share.to_string(),
        items: items.clone(),
    };
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        request(&endpoint, EndpointAddr::from(peer_id), batch),
    )
    .await;
    match r {
        Ok(Ok(Response::Noted)) => {
            tracing::debug!(%share, n, to = petname, "nudged");
            return;
        }
        Ok(Ok(Response::Refused { code, .. }))
            if matches!(code, RefusalCode::Unsupported | RefusalCode::BadRequest) =>
        {
            tracing::debug!(%share, to = petname, "peer predates batched nudges; sending singly");
        }
        Ok(Ok(other)) => {
            tracing::debug!(%share, to = petname, "nudge not accepted: {other:?}");
            return;
        }
        Ok(Err(e)) => {
            tracing::debug!(%share, to = petname, "nudge failed (offline?): {e:#}");
            return;
        }
        Err(_) => {
            tracing::debug!(%share, to = petname, "nudge timed out");
            return;
        }
    }
    for item in items {
        let req = Request::Notify {
            share: share.to_string(),
            doc: item.doc,
            title: item.title,
            kind: item.kind,
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            request(&endpoint, EndpointAddr::from(peer_id), req),
        )
        .await;
    }
}

/// Owner-side change detector state (0.7.2: incremental). Per share, the
/// last (doc → mark) view; across shares, the last epoch of every doc and the
/// set of live docs, so one tick can name exactly which docs moved and walk
/// only the shares containing them.
#[derive(Default)]
pub struct NotifyState {
    /// (share id) → (doc id → mark); a share's first observation seeds silently
    marks: HashMap<Uuid, HashMap<Uuid, DocMark>>,
    last_sig: Option<(grimoire_store::ChangeSignature, u64)>,
    doc_epochs: HashMap<Uuid, i64>,
    hot_docs: std::collections::HashSet<Uuid>,
    /// How many shares the last tick walked (`served_docs_for` + diff); a
    /// test seam and a log field.
    pub last_walked: usize,
}

/// One nudge batch the tick decided to send.
pub struct DueBatch {
    pub share: grimoire_store::Share,
    pub contact: grimoire_store::Contact,
    pub items: Vec<NotifyItem>,
}

/// One detector tick, pure over the store: what is due, and to whom. Idle =
/// the aggregate signature and the hot generation are unchanged → two trivial
/// queries, nothing walked. Otherwise: ONE `list_docs` names the docs whose
/// epoch rose (or are new) and whose hotness flipped; their containing
/// shares (walked up once per changed doc) plus any share never seen before
/// are the only ones diffed. Before 0.7.2 every active share was walked on
/// every change — on a hub, O(shares × mirrors × docs) per edit anywhere.
pub fn notify_tick(state: &mut NotifyState, store: &SqliteStore, hot: &HotState) -> Vec<DueBatch> {
    state.last_walked = 0;
    let sig = match store.change_signature() {
        Ok(sig) => (sig, hot.generation()),
        Err(e) => {
            tracing::debug!("change signature failed: {e}");
            return Vec::new();
        }
    };
    if state.last_sig == Some(sig) {
        return Vec::new();
    }
    state.last_sig = Some(sig);
    let contacts = store.list_contacts().unwrap_or_default();
    let shares: Vec<(grimoire_store::Share, grimoire_store::Contact)> = store
        .list_shares()
        .unwrap_or_default()
        .into_iter()
        .filter(|sh| sh.state == grimoire_store::ShareState::Active)
        .filter_map(|sh| {
            let c = contacts.iter().find(|c| Some(c.id) == sh.contact && !c.revoked)?.clone();
            Some((sh, c))
        })
        .collect();
    let live_share_ids: std::collections::HashSet<Uuid> = shares.iter().map(|(sh, _)| sh.id).collect();
    state.marks.retain(|id, _| live_share_ids.contains(id)); // forget revoked shares

    // the changed-doc set: epoch rose / new id / hotness flipped
    let docs = store.list_docs().unwrap_or_default();
    let hot_now: std::collections::HashSet<Uuid> = docs.iter().map(|d| d.id).filter(|d| hot.is_hot(*d)).collect();
    let mut changed: Vec<Uuid> = Vec::new();
    let mut epochs_now: HashMap<Uuid, i64> = HashMap::with_capacity(docs.len());
    for d in &docs {
        epochs_now.insert(d.id, d.current_epoch);
        let moved = match state.doc_epochs.get(&d.id) {
            Some(prev) => d.current_epoch > *prev,
            None => true,
        };
        if moved || state.hot_docs.contains(&d.id) != hot_now.contains(&d.id) {
            changed.push(d.id);
        }
    }
    let first_tick = state.doc_epochs.is_empty() && state.marks.is_empty();
    state.doc_epochs = epochs_now;
    state.hot_docs = hot_now;

    // shares to walk: those containing a changed doc, plus never-seen shares
    let mut walk: std::collections::HashSet<Uuid> = shares
        .iter()
        .filter(|(sh, _)| !state.marks.contains_key(&sh.id))
        .map(|(sh, _)| sh.id)
        .collect();
    if !first_tick {
        for d in &changed {
            for sh in store.shares_containing(*d).unwrap_or_default() {
                if live_share_ids.contains(&sh.id) {
                    walk.insert(sh.id);
                }
            }
        }
    }
    // never nudge about a mirror: its changes are the owner's, not ours (a
    // transferred subtree is served back to its new owner but is nothing to
    // announce to them) — a hub is the exception: relayed mirrors ARE what
    // it announces
    let mirror_ids: std::collections::HashSet<Uuid> = if super::hub::config(store).is_some() {
        Default::default()
    } else {
        store.list_mirrors().unwrap_or_default().into_iter().map(|m| m.doc_id).collect()
    };
    let mut out = Vec::new();
    for (share, contact) in shares {
        if !walk.contains(&share.id) {
            continue;
        }
        state.last_walked += 1;
        let Ok(docs) = served_docs_for(store, share.id, Some(&contact.pubkey)) else { continue };
        let view: Vec<(Uuid, String, DocMark)> = docs
            .into_iter()
            .filter(|d| !mirror_ids.contains(&d.id))
            .map(|d| (d.id, d.title, DocMark { epoch: d.current_epoch, hot: state.hot_docs.contains(&d.id) }))
            .collect();
        let seeded = state.marks.contains_key(&share.id);
        let last = state.marks.entry(share.id).or_default();
        let now: Vec<(Uuid, DocMark)> = view.iter().map(|(d, _, m)| (*d, *m)).collect();
        let due = diff_share(last, &now, seeded);
        *last = now.iter().copied().collect();
        if due.is_empty() {
            continue;
        }
        let titles: HashMap<Uuid, String> = view.into_iter().map(|(d, t, _)| (d, t)).collect();
        out.push(DueBatch { share, contact, items: batch_nudges(&due, &titles) });
    }
    if state.last_walked > 0 {
        tracing::debug!(changed = changed.len(), walked = state.last_walked, due = out.len(), "notify tick");
    }
    out
}

/// Owner-side change detector + nudger. One 1s tick; on an idle daemon the
/// tick is ONE aggregate query (`change_signature`) plus an atomic read of
/// the hot generation — the per-share walk runs only for shares whose docs
/// moved (`notify_tick`). Each share's changes go out as one `NotifyBatch`
/// dial to its contact (best-effort, timed out, never blocking the loop).
pub async fn notify_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>, hot: HotState) {
    supervise("notify", move || notify_loop_inner(endpoint.clone(), store.clone(), hot.clone())).await
}

async fn notify_loop_inner(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>, hot: HotState) {
    let mut state = NotifyState::default();
    loop {
        tokio::time::sleep(NOTIFY_TICK).await;
        let due = {
            let (hot, st) = (hot.clone(), std::mem::take(&mut state));
            let (due, st) = with_store(&store, move |s| {
                let mut st = st;
                let due = notify_tick(&mut st, s, &hot);
                (due, st)
            })
            .await;
            state = st;
            due
        };
        for batch in due {
            let Ok(peer_id) = batch.contact.pubkey.parse::<iroh::EndpointId>() else {
                continue;
            };
            tokio::spawn(send_nudges(
                endpoint.clone(),
                peer_id,
                batch.share.id,
                batch.items,
                batch.contact.petname.clone(),
            ));
        }
    }
}

/// Hub forward records older than this are pruned on the sweep.
pub const HUB_FORWARD_RETENTION_DAYS: u32 = 7;

/// 0.7.2: `hub_forwards` grew forever (one row per forwarded op, never
/// pruned). A hub drops rows older than `HUB_FORWARD_RETENTION_DAYS` on the
/// 120s sweep; the owner's op/annotation carries the verdict, so an old row
/// is only needed while a member may still ask `ProposalStatus` about it.
/// Non-hubs hold no rows and skip the query.
pub fn prune_hub_forwards(store: &Arc<Mutex<SqliteStore>>) -> usize {
    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    if super::hub::config(&s).is_none() {
        return 0;
    }
    match s.prune_hub_forwards(HUB_FORWARD_RETENTION_DAYS) {
        Ok(n) => {
            if n > 0 {
                tracing::info!(pruned = n, days = HUB_FORWARD_RETENTION_DAYS, "hub: old forward records pruned");
            }
            n
        }
        Err(e) => {
            tracing::warn!("hub: pruning forward records failed: {e}");
            0
        }
    }
}

/// 0.7.2: run a background loop forever. A loop task that panics (a poisoned
/// lock, an unwrap in a rarely-taken branch) or returns is logged at ERROR
/// and restarted with a backoff (1s, doubling to 60s; reset after a healthy
/// minute) instead of leaving the daemon silently without pulls, nudges or
/// join retries until the next restart.
pub async fn supervise<F, Fut>(name: &'static str, make: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    const MIN: Duration = Duration::from_secs(1);
    const MAX: Duration = Duration::from_secs(60);
    let mut backoff = MIN;
    loop {
        let started = Instant::now();
        match tokio::spawn(make()).await {
            Ok(()) => tracing::error!(name, "background loop exited; restarting"),
            Err(e) => tracing::error!(name, "background loop panicked: {e}; restarting"),
        }
        backoff = if started.elapsed() > MAX { MIN } else { (backoff * 2).min(MAX) };
        tokio::time::sleep(backoff).await;
    }
}

/// A DEAD invite (already redeemed — e.g. the same link pasted twice —
/// expired, or unknown) can never succeed: the retry loop drops it instead
/// of hammering the owner. Only the typed code counts; an offline owner is
/// "try later".
pub fn join_failure_is_dead(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<Refusal>().map(|r| r.code),
        // a root held from another contact does not free itself by waiting
        Some(RefusalCode::InviteInvalid | RefusalCode::RootConflict)
    )
}

/// Retry schedule for a pending join: 60s, 2m, 4m, … capped at 30 minutes.
pub fn join_backoff(attempts: i64) -> Duration {
    const BASE: Duration = Duration::from_secs(60);
    const CAP: Duration = Duration::from_secs(30 * 60);
    let shift = attempts.clamp(0, 16) as u32;
    BASE.checked_mul(1u32 << shift).unwrap_or(CAP).min(CAP)
}

/// A pending join older than the invite window can never succeed (the
/// owner's invite has expired): give up rather than retry forever.
pub const JOIN_WINDOW: chrono::Duration = chrono::Duration::days(7);

pub fn join_expired(created_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    match chrono::DateTime::parse_from_rfc3339(created_at) {
        Ok(t) => now - t.with_timezone(&chrono::Utc) > JOIN_WINDOW,
        // unparseable stamp: treat as expired rather than retry forever
        Err(_) => true,
    }
}

/// Background retry for joins whose owner was offline (async redeem,
/// ADR 0002 decision 6). Exponential backoff per join (`join_backoff`),
/// give-up after the invite window (`join_expired`); every failure is
/// recorded; success removes the row.
pub async fn join_retry_loop(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    supervise("join-retry", move || join_retry_loop_inner(endpoint.clone(), store.clone())).await
}

async fn join_retry_loop_inner(endpoint: Endpoint, store: Arc<Mutex<SqliteStore>>) {
    const TICK: Duration = Duration::from_secs(60);
    // next attempt per pending join (in memory: a restart simply retries)
    let mut next_due: HashMap<Uuid, Instant> = HashMap::new();
    loop {
        tokio::time::sleep(TICK).await;
        let pending = with_store(&store, |s| s.list_pending_joins().unwrap_or_default()).await;
        let live: std::collections::HashSet<Uuid> = pending.iter().map(|j| j.id).collect();
        next_due.retain(|id, _| live.contains(id));
        let now = Instant::now();
        for join in pending {
            if join_expired(&join.created_at, chrono::Utc::now()) {
                tracing::warn!(attempts = join.attempts, "pending join older than the invite window; giving up");
                let id = join.id;
                with_store(&store, move |s| {
                    s.record_join_attempt(id, "gave up: the invite link is older than 7 days — ask for a new one").ok();
                    s.remove_pending_join(id).ok();
                })
                .await;
                continue;
            }
            if next_due.get(&join.id).is_some_and(|t| *t > now) {
                continue;
            }
            let ticket = match Ticket::parse(&join.ticket) {
                Ok(t) => t,
                Err(e) => {
                    // unparseable tickets can never succeed; drop them
                    tracing::warn!("dropping unparseable pending join: {e:#}");
                    let id = join.id;
                    with_store(&store, move |s| s.remove_pending_join(id).ok()).await;
                    continue;
                }
            };
            match join_once(&endpoint, &store, &ticket).await {
                Ok(out) => {
                    tracing::info!(root = out.root_doc, "queued join completed");
                    {
                        let id = join.id;
                        with_store(&store, move |s| s.remove_pending_join(id).ok()).await;
                    }
                    match super::client::pull_after_join(&endpoint, &store, &out.root_doc).await {
                        Ok(sum) => tracing::info!(root = out.root_doc, docs = sum.changed, "first pull after queued join"),
                        Err(e) => tracing::warn!(root = out.root_doc, "first pull after queued join failed: {e:#}"),
                    }
                }
                Err(e) => {
                    let id = join.id;
                    if join_failure_is_dead(&e) {
                        tracing::warn!("dropping pending join: {e:#}");
                        with_store(&store, move |s| s.remove_pending_join(id).ok()).await;
                    } else {
                        let msg = format!("{e:#}");
                        with_store(&store, move |s| s.record_join_attempt(id, &msg).ok()).await;
                        next_due.insert(join.id, now + join_backoff(join.attempts + 1));
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

    #[test]
    fn batch_nudges_carries_every_due_item_with_its_title() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let titles: HashMap<Uuid, String> = [(a, "A".to_string())].into_iter().collect();
        let items = batch_nudges(
            &[(a, NotifyKind::DocChanged), (a, NotifyKind::LiveStarted), (b, NotifyKind::DocAdded)],
            &titles,
        );
        assert_eq!(items.len(), 3);
        assert_eq!((items[0].doc.as_str(), items[0].title.as_str(), items[0].kind), (a.to_string().as_str(), "A", NotifyKind::DocChanged));
        assert_eq!(items[1].kind, NotifyKind::LiveStarted);
        assert_eq!((items[2].title.as_str(), items[2].kind), ("", NotifyKind::DocAdded), "unknown title → empty, never dropped");
    }

    fn refused(code: RefusalCode) -> Result<PullSummary> {
        Err(Refusal::new(code, "x").into())
    }

    #[test]
    fn dead_peer_tracker_drops_on_revoke_now_and_on_unknown_peer_only_after_a_sweep() {
        let mut t = DeadPeerTracker::default();
        let share = Uuid::now_v7();
        let t0 = Instant::now();
        // explicit revoke: immediate
        assert!(t.observe(share, &refused(RefusalCode::ShareRevoked), t0));
        // unknown peer: first sighting is not enough
        assert!(!t.observe(share, &refused(RefusalCode::UnknownPeer), t0));
        // a second sighting inside the same sweep: still not
        assert!(!t.observe(share, &refused(RefusalCode::UnknownPeer), t0 + Duration::from_secs(5)));
        // a sweep later, still unknown: drop
        assert!(t.observe(share, &refused(RefusalCode::UnknownPeer), t0 + SWEEP_EVERY));
        // a success (or any other error) in between resets the clock
        let mut t = DeadPeerTracker::default();
        assert!(!t.observe(share, &refused(RefusalCode::UnknownPeer), t0));
        assert!(!t.observe(share, &Ok(PullSummary::default()), t0 + Duration::from_secs(60)));
        assert!(!t.observe(share, &refused(RefusalCode::UnknownPeer), t0 + SWEEP_EVERY));
        assert!(!t.observe(share, &Err(anyhow::anyhow!("dial timed out")), t0 + SWEEP_EVERY * 2));
        assert!(!t.observe(share, &refused(RefusalCode::UnknownPeer), t0 + SWEEP_EVERY * 3));
        assert!(t.observe(share, &refused(RefusalCode::UnknownPeer), t0 + SWEEP_EVERY * 4));
        // other refusals never drop
        assert!(!t.observe(share, &refused(RefusalCode::NotInShare), t0));
        assert!(!t.observe(share, &refused(RefusalCode::ShareInactive), t0));
    }

    #[test]
    fn join_backoff_doubles_from_a_minute_and_caps_at_thirty() {
        assert_eq!(join_backoff(0), Duration::from_secs(60));
        assert_eq!(join_backoff(1), Duration::from_secs(120));
        assert_eq!(join_backoff(3), Duration::from_secs(480));
        assert_eq!(join_backoff(5), Duration::from_secs(1800), "2^5 min = 32 min → capped");
        assert_eq!(join_backoff(40), Duration::from_secs(1800), "no overflow");
        assert_eq!(join_backoff(-3), Duration::from_secs(60), "garbage in → base");
    }

    #[test]
    fn join_expires_after_the_invite_window() {
        let now = chrono::Utc::now();
        let fresh = (now - chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let old = (now - chrono::Duration::days(8)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        assert!(!join_expired(&fresh, now));
        assert!(join_expired(&old, now));
        assert!(join_expired("not a date", now));
    }
}
