//! Ownership transfer (hub slice 2): a member hands a subtree they own to a
//! hub. Two-sided and ledgered, UUIDs preserved:
//!
//! 1. Member → hub `TransferOffer` (recorded; an admin accepts or declines).
//! 2. Hub → member `TransferAccepted`. The member refuses `Busy` while any
//!    doc in the subtree is live or has edits waiting for review; otherwise
//!    it flips every doc to a MIRROR of the hub (its own copy becomes the
//!    read-only replica), makes sure a `propose` share to the hub covers the
//!    root, and answers `TransferReady { share_id }`.
//! 3. The hub pulls the subtree through that share (landing or refreshing
//!    its mirrors under `<hub root>/<member>`), then flips them to OWNED:
//!    mirror rows and the publication record go, `doc_transfers` records the
//!    hand-over. From then on the hub serves the docs as its own and the
//!    member's copy follows the hub like any mirror.
//!
//! Idempotent both ways: a second `TransferAccepted` for a root that is
//! already a mirror of the hub answers `TransferReady` again; a second flip
//! on the hub finds nothing to flip. Block `created_by` is untouched — the
//! provenance of who wrote what survives the change of home.

use super::client::{pull_share, request};
use super::hub;
use super::wire::{Refusal, RefusalCode, Request, Response};
use crate::hot::HotState;
use anyhow::{Context, Result};
use grimoire_store::{
    BlockStore, Contact, HubTransferState, SharePermission, ShareState, SqliteStore,
    TransferDirection,
};
use iroh::{Endpoint, EndpointAddr};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// The share a member holds the hub root through (their membership).
pub fn membership_share(store: &SqliteStore, hub_contact: Uuid) -> Option<Uuid> {
    let mirrors: Vec<_> = store
        .list_mirrors()
        .ok()?
        .into_iter()
        .filter(|m| m.owner == hub_contact)
        .collect();
    let ids: std::collections::HashSet<Uuid> = mirrors.iter().map(|m| m.doc_id).collect();
    // the hub root is the mirror whose parent is not itself one of the hub's mirrors
    mirrors
        .iter()
        .find(|m| {
            store
                .get_doc(m.doc_id)
                .ok()
                .map(|d| d.parent_id.map(|p| !ids.contains(&p)).unwrap_or(true))
                .unwrap_or(false)
        })
        .or(mirrors.first())
        .map(|m| m.share_id)
}

/// Member side: refuse unless every doc in the subtree is idle. Names the
/// offending doc so the person knows what to close.
fn require_idle(store: &SqliteStore, hot: &HotState, subtree: &[Uuid]) -> Result<()> {
    for id in subtree {
        let title = store.get_doc(*id).map(|d| d.title).unwrap_or_default();
        if hot.is_hot(*id) {
            return Err(Refusal::new(
                RefusalCode::Busy,
                format!("“{title}” is in a live session — end it first"),
            )
            .into());
        }
        if !store.review_queue(Some(*id))?.is_empty() {
            return Err(Refusal::new(
                RefusalCode::Busy,
                format!("“{title}” has edits waiting for review — resolve them first"),
            )
            .into());
        }
    }
    Ok(())
}

/// Member side of `TransferAccepted`: flip `root_doc`'s subtree to mirrors of
/// `hub` and answer with the share the hub pulls it through. Runs under the
/// store lock (no network).
pub fn member_flip(
    store: &mut SqliteStore,
    hot: &HotState,
    hub: &Contact,
    root_doc: Uuid,
) -> Result<Response> {
    if !hub.is_hub {
        return Err(Refusal::new(RefusalCode::NotAllowed, "only a hub can take a folder over").into());
    }
    let offered = store
        .list_doc_transfers()?
        .into_iter()
        .find(|t| t.root_doc == root_doc && t.direction == TransferDirection::Out && t.counterparty == hub.id);
    let Some(record) = offered else {
        return Err(Refusal::new(
            RefusalCode::NotAllowed,
            format!("you did not offer this folder to {}", hub.petname),
        )
        .into());
    };
    if store.get_doc(root_doc).is_err() || store.doc_is_tombstoned(root_doc)? {
        return Err(Refusal::new(RefusalCode::BadRequest, "that folder no longer exists").into());
    }
    let Some(membership) = membership_share(store, hub.id) else {
        return Err(Refusal::new(
            RefusalCode::NotAllowed,
            format!("you are not a member of {}", hub.petname),
        )
        .into());
    };
    let subtree = store.doc_subtree_ids(root_doc)?;
    // already ours-as-a-mirror: the hub is retrying — answer the same way.
    // 0.7.2: "already" is judged on the root only, but the flip below runs
    // over the WHOLE subtree regardless, so a flip that died half-way (root
    // done, children not) is completed by the retry instead of frozen.
    let already = store
        .get_mirror(root_doc)?
        .is_some_and(|m| m.owner == hub.id);
    if !already {
        // anything mirrored from someone ELSE inside the subtree cannot be given away
        for id in &subtree {
            if store.get_mirror(*id)?.is_some() {
                let title = store.get_doc(*id).map(|d| d.title).unwrap_or_default();
                return Err(Refusal::new(
                    RefusalCode::NotAllowed,
                    format!("“{title}” was shared to you — only its owner can transfer it"),
                )
                .into());
            }
        }
        require_idle(store, hot, &subtree)?;
    }
    // the share the hub pulls through: reuse the publication if there is one
    // (created BEFORE the mirror rows — the re-share guard refuses a share
    // over a subtree that already contains mirrors)
    let existing = store.list_shares()?.into_iter().find(|sh| {
        sh.root_doc == root_doc
            && sh.contact == Some(hub.id)
            && sh.permission == SharePermission::Propose
            && sh.state != ShareState::Revoked
    });
    let share_id = match existing {
        Some(sh) => {
            if sh.state != ShareState::Active {
                store.set_share_state(sh.id, ShareState::Active)?;
            }
            sh.id
        }
        None => {
            let sh = store.create_share(root_doc, Some(hub.id), SharePermission::Propose, None)?;
            store.set_share_state(sh.id, ShareState::Active)?;
            sh.id
        }
    };
    let mut flipped = 0usize;
    for id in &subtree {
        // idempotent: a doc already mirrored from the hub keeps its cursor
        if store.get_mirror(*id)?.is_some_and(|m| m.owner == hub.id) {
            continue;
        }
        let d = store.get_doc(*id)?;
        store.upsert_mirror(*id, hub.id, membership, d.current_epoch, SharePermission::Propose)?;
        store.set_mirror_owner_epoch(*id, d.current_epoch)?;
        store.set_mirror_origin(*id, None, None)?;
        flipped += 1;
    }
    if flipped > 0 {
        tracing::info!(
            hub = hub.petname,
            root = %root_doc,
            docs = flipped,
            completing = already,
            "transfer: folder handed to the hub; local copy is now a mirror"
        );
    }
    if record.state != "done" {
        store.set_doc_transfer_state(record.id, "done")?;
    }
    Ok(Response::TransferReady {
        share_id: share_id.to_string(),
    })
}

/// Settings key marking a member's out-transfer as acknowledged by the hub
/// (the hub answered `Noted` to our `TransferReady` re-announcement).
fn acked_key(transfer: Uuid) -> String {
    format!("transfer.acked.{transfer}")
}

/// Member sweep (0.7.2): a flipped transfer whose `TransferReady` reply the
/// hub never got is stuck — our copy is a mirror of a hub that does not know
/// it owns the folder. Re-announce every flipped, unacknowledged out-transfer
/// with `Request::TransferReady`; `Noted` marks it acknowledged. `addr`
/// overrides the dial-by-pubkey (tests). Returns how many were acknowledged.
pub async fn resend_ready(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    addr: Option<EndpointAddr>,
) -> usize {
    let due: Vec<(Uuid, Contact, Uuid, Uuid)> = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let contacts = s.list_contacts().unwrap_or_default();
        s.list_doc_transfers()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.direction == TransferDirection::Out && t.state == "done")
            .filter(|t| !matches!(s.get_setting(&acked_key(t.id)), Ok(Some(_))))
            .filter_map(|t| {
                let hub = contacts.iter().find(|c| c.id == t.counterparty && c.is_hub && !c.revoked)?.clone();
                // still flipped: the root is our mirror of that hub
                s.get_mirror(t.root_doc).ok().flatten().filter(|m| m.owner == hub.id)?;
                let share = s
                    .list_shares()
                    .ok()?
                    .into_iter()
                    .find(|sh| sh.root_doc == t.root_doc && sh.contact == Some(hub.id) && sh.state == ShareState::Active)?;
                Some((t.id, hub, t.root_doc, share.id))
            })
            .collect()
    };
    let mut acked = 0;
    for (transfer, hub, root, share) in due {
        let addr = match &addr {
            Some(a) => a.clone(),
            None => match hub.pubkey.parse::<iroh::EndpointId>() {
                Ok(id) => EndpointAddr::from(id),
                Err(_) => continue,
            },
        };
        let req = Request::TransferReady {
            root_doc: root.to_string(),
            share_id: share.to_string(),
        };
        match tokio::time::timeout(std::time::Duration::from_secs(15), request(endpoint, addr, req)).await {
            Ok(Ok(Response::Noted)) => {
                let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
                s.set_setting(&acked_key(transfer), "1").ok();
                tracing::info!(hub = hub.petname, %root, "transfer: hub confirmed it owns the folder");
                acked += 1;
            }
            Ok(Ok(Response::Refused { code: RefusalCode::Busy, .. })) => {
                tracing::info!(hub = hub.petname, %root, "transfer: hub is taking the folder over; will confirm next sweep");
            }
            Ok(Ok(other)) => tracing::warn!(hub = hub.petname, %root, "transfer: re-announce not taken: {other:?}"),
            Ok(Err(e)) => tracing::debug!(hub = hub.petname, %root, "transfer: re-announce failed: {e:#}"),
            Err(_) => tracing::debug!(hub = hub.petname, %root, "transfer: re-announce timed out"),
        }
    }
    acked
}

/// Hub side of `Request::TransferReady` (0.7.2). `Ok(Some(id))` = an
/// accepted transfer from this member for this root that still needs the
/// take-over (the caller runs `hub_complete` off-thread and answers `Busy`);
/// `Ok(None)` = already done, answer `Noted`. Runs under the store lock.
pub fn hub_ready_ping(store: &SqliteStore, member: &Contact, root_doc: Uuid, _share_id: Uuid) -> Result<Option<Uuid>> {
    let mut mine: Vec<_> = store
        .list_hub_transfers()?
        .into_iter()
        .filter(|t| t.member_contact == member.id && t.root_doc == root_doc)
        .collect();
    if mine.iter().any(|t| t.state == HubTransferState::Done) {
        return Ok(None);
    }
    mine.sort_by(|a, b| b.at.cmp(&a.at));
    match mine.into_iter().find(|t| t.state == HubTransferState::Accepted) {
        Some(t) => Ok(Some(t.id)),
        None => Err(Refusal::new(
            RefusalCode::NotAllowed,
            "no accepted transfer of that folder from you",
        )
        .into()),
    }
}

/// Hub side, after an admin accepted: dial the member, and on `TransferReady`
/// pull the subtree and take it over. `addr` is how we reach the member —
/// by pubkey in production (they dialed us to offer), explicit in tests.
pub async fn hub_complete(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    transfer_id: Uuid,
    addr: Option<EndpointAddr>,
) -> Result<()> {
    let (hub_cfg, t, member) = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let hub_cfg = hub::config(&s).context("not a hub")?;
        let t = s.get_hub_transfer(transfer_id)?;
        let member = s
            .list_contacts()?
            .into_iter()
            .find(|c| c.id == t.member_contact)
            .context("member contact is gone")?;
        (hub_cfg, t, member)
    };
    if t.state == HubTransferState::Done {
        return Ok(());
    }
    if t.state != HubTransferState::Accepted {
        anyhow::bail!("transfer is {}", t.state.as_str());
    }
    if member.revoked {
        anyhow::bail!("member was removed");
    }
    let addr = match addr {
        Some(a) => a,
        None => EndpointAddr::from(member.pubkey.parse::<iroh::EndpointId>().context("member pubkey malformed")?),
    };
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        request(
            endpoint,
            addr.clone(),
            Request::TransferAccepted {
                root_doc: t.root_doc.to_string(),
            },
        ),
    )
    .await;
    let share_id: Uuid = match res {
        Ok(Ok(Response::TransferReady { share_id })) => share_id.parse().context("member sent a bad share id")?,
        Ok(Ok(Response::Refused { reason, code })) => {
            // back to offered so an admin can try again once it is idle
            let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
            s.set_hub_transfer_state(transfer_id, HubTransferState::Offered).ok();
            return Err(Refusal::new(code, format!("{} refused: {reason}", member.petname)).into());
        }
        Ok(Ok(other)) => anyhow::bail!("unexpected reply from {}: {other:?}", member.petname),
        Ok(Err(e)) => return Err(e.context(format!("dialing {}", member.petname))),
        Err(_) => anyhow::bail!("{} is offline or unreachable right now", member.petname),
    };
    let sum = pull_share(endpoint, store, addr, &member, share_id)
        .await
        .context("pulling the folder from the member")?;
    tracing::info!(root = %t.root_doc, docs = sum.changed, "transfer: folder pulled from the member");
    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    hub_flip(&mut s, &hub_cfg, &member, transfer_id, share_id)
}

/// Hub side: the pulled mirrors become the hub's own docs. Idempotent.
pub fn hub_flip(
    store: &mut SqliteStore,
    hub_cfg: &hub::HubConfig,
    member: &Contact,
    transfer_id: Uuid,
    share_id: Uuid,
) -> Result<()> {
    let t = store.get_hub_transfer(transfer_id)?;
    if store.get_doc(t.root_doc).is_err() {
        anyhow::bail!("the folder never arrived");
    }
    // file it where the member's publications live
    let folder = hub::member_folder(store, hub_cfg, member)?;
    if store.get_doc(t.root_doc)?.parent_id != Some(folder) {
        store.move_doc(t.root_doc, Some(folder), None)?;
    }
    let mine: Vec<Uuid> = store
        .list_mirrors()?
        .into_iter()
        .filter(|m| m.share_id == share_id)
        .map(|m| m.doc_id)
        .collect();
    for id in &mine {
        store.remove_mirror(*id)?;
    }
    store.remove_hub_publication(share_id)?;
    if t.state != HubTransferState::Done {
        store.add_doc_transfer(t.root_doc, member.id, TransferDirection::In, "done")?;
        store.set_hub_transfer_state(transfer_id, HubTransferState::Done)?;
        tracing::info!(
            member = member.petname,
            title = t.title,
            root = %t.root_doc,
            docs = mine.len(),
            "transfer: the hub now owns the folder"
        );
    }
    Ok(())
}
