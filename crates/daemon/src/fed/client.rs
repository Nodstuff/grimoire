//! Grantee side of federation: dialing an owner, joining, pulling mirrors,
//! shipping proposals/comments upstream, hot-session relay one-shots — plus
//! `mint_invite` (the owner-side CLI/admin helper that creates a link).
//!
//! The pull is where someone else's data lands in MY store, so it is the
//! most defensive code here: a doc id the owner names is claimed as a mirror
//! ONLY if it is not already one of my own docs (`foreign_ids`).

use super::server::{read_frame, write_frame};
use super::wire::{
    ALPN, Frame, HOT_ALPN, MAX_FRAME, PROTOCOL_VERSION, Refusal, Request, Response, Ticket,
    WireDoc, hash_secret,
};
use anyhow::{Context, Result};
use grimoire_store::{BlockStore, SqliteStore};
use iroh::{Endpoint, EndpointAddr};
use serde::Serialize;
use std::sync::{Arc, Mutex};

/// One request against a remote instance (grantee-side; the pull loop and
/// the redeem flow both come through here).
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
/// Grantee side (#66): a raw duplex to the owner's session. The caller pumps
/// ws-binary ↔ these channels.
pub async fn open_hot_bridge(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
) -> Result<(
    tokio::sync::mpsc::Sender<Vec<u8>>,
    tokio::sync::mpsc::Receiver<Vec<u8>>,
)> {
    let (mirror, owner) = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let mirror = s.get_mirror(doc_id)?.context("doc is not a mirror")?;
        let owner = s
            .list_contacts()?
            .into_iter()
            .find(|c| c.id == mirror.owner)
            .context("mirror's owner contact is gone")?;
        (mirror, owner)
    };
    let owner_id: iroh::EndpointId = owner.pubkey.parse().context("owner pubkey malformed")?;
    // The bridge is a fresh QUIC connection on its own ALPN; across NATs it
    // may need a relay round or a hole-punch to settle, so retry the dial a
    // few times before declaring the session unreachable.
    let mut last_err = None;
    let mut conn = None;
    for attempt in 1..=4u32 {
        match tokio::time::timeout(
            std::time::Duration::from_secs(8),
            endpoint.connect(EndpointAddr::from(owner_id), HOT_ALPN),
        )
        .await
        {
            Ok(Ok(c)) => {
                conn = Some(c);
                break;
            }
            Ok(Err(e)) => {
                tracing::warn!(%doc_id, attempt, "hot bridge dial failed: {e:#}");
                last_err = Some(anyhow::Error::from(e));
            }
            Err(_) => {
                tracing::warn!(%doc_id, attempt, "hot bridge dial timed out");
                last_err = Some(anyhow::anyhow!("dial timed out after 8s"));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(750 * attempt as u64)).await;
    }
    let conn = conn.ok_or_else(|| {
        last_err
            .unwrap_or_else(|| anyhow::anyhow!("no attempts"))
            .context("dialing owner for hot bridge (4 attempts)")
    })?;
    let (mut send, mut recv) = conn.open_bi().await.context("opening bridge stream")?;
    let header = serde_json::json!({
        "share": mirror.share_id.to_string(),
        "doc": doc_id.to_string(),
    });
    write_frame(&mut send, &serde_json::to_vec(&header)?).await?;

    let (to_owner_tx, mut to_owner_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (from_owner_tx, from_owner_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
        while let Some(frame) = to_owner_rx.recv().await {
            if write_frame(&mut send, &frame).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        loop {
            match read_frame(&mut recv).await {
                Ok(Some(frame)) => {
                    if from_owner_tx.send(frame).await.is_err() {
                        break;
                    }
                }
                _ => break, // closed: dropping from_owner_tx ends the ws side
            }
        }
    });
    Ok((to_owner_tx, from_owner_rx))
}

/// Grantee-side one-shots for the UI surface (#66).
pub async fn hot_status_upstream(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
) -> Result<(bool, Option<i64>, usize, Option<bool>)> {
    let (mirror, owner) = mirror_owner(store, doc_id)?;
    let owner_id: iroh::EndpointId = owner.pubkey.parse().context("owner pubkey malformed")?;
    let res = request(
        endpoint,
        EndpointAddr::from(owner_id),
        Request::HotStatus {
            share: mirror.share_id.to_string(),
            doc: doc_id.to_string(),
        },
    )
    .await?;
    match res {
        Response::HotStatusIs {
            hot,
            frozen_epoch,
            editors,
            can_write,
        } => Ok((hot, frozen_epoch, editors, can_write)),
        other => anyhow::bail!("owner refused hot status: {other:?}"),
    }
}

pub async fn edit_ping_upstream(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
    key: uuid::Uuid,
) -> Result<usize> {
    let (mirror, owner) = mirror_owner(store, doc_id)?;
    let owner_id: iroh::EndpointId = owner.pubkey.parse().context("owner pubkey malformed")?;
    let res = request(
        endpoint,
        EndpointAddr::from(owner_id),
        Request::EditPing {
            share: mirror.share_id.to_string(),
            doc: doc_id.to_string(),
            key: key.to_string(),
        },
    )
    .await?;
    match res {
        Response::HotStatusIs { editors, .. } => Ok(editors),
        other => anyhow::bail!("owner refused edit ping: {other:?}"),
    }
}

pub async fn hot_end_upstream(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
) -> Result<usize> {
    let (mirror, owner) = mirror_owner(store, doc_id)?;
    let owner_id: iroh::EndpointId = owner.pubkey.parse().context("owner pubkey malformed")?;
    let res = request(
        endpoint,
        EndpointAddr::from(owner_id),
        Request::HotEnd {
            share: mirror.share_id.to_string(),
            doc: doc_id.to_string(),
        },
    )
    .await?;
    match res {
        Response::HotEnded { flattened_ops } => Ok(flattened_ops),
        other => anyhow::bail!("owner refused hot end: {other:?}"),
    }
}

pub async fn hot_start_upstream(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
) -> Result<(i64, bool)> {
    let (mirror, owner) = mirror_owner(store, doc_id)?;
    let owner_id: iroh::EndpointId = owner.pubkey.parse().context("owner pubkey malformed")?;
    let res = request(
        endpoint,
        EndpointAddr::from(owner_id),
        Request::HotStart {
            share: mirror.share_id.to_string(),
            doc: doc_id.to_string(),
        },
    )
    .await?;
    match res {
        Response::HotStarted { frozen_epoch, seed } => Ok((frozen_epoch, seed)),
        other => anyhow::bail!("owner refused hot start: {other:?}"),
    }
}

fn mirror_owner(
    store: &Arc<Mutex<SqliteStore>>,
    doc_id: uuid::Uuid,
) -> Result<(grimoire_store::Mirror, grimoire_store::Contact)> {
    let s = store.lock().unwrap_or_else(|p| p.into_inner());
    let mirror = s.get_mirror(doc_id)?.context("doc is not a mirror")?;
    let owner = s
        .list_contacts()?
        .into_iter()
        .find(|c| c.id == mirror.owner)
        .context("mirror's owner contact is gone")?;
    Ok((mirror, owner))
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
        // typed so the retry loop can tell a DEAD invite (already redeemed,
        // expired, unknown → stop retrying) from "owner offline, try later"
        if let Response::Refused { reason, code } = res {
            return Err(Refusal::new(code, format!("owner refused the invite: {reason}")).into());
        }
        anyhow::bail!("owner refused the invite: {res:?}");
    };

    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
    let owner_contact = s.pair_contact(&ticket.node, &owner_name)?;
    let root_uuid: uuid::Uuid = root_doc.parse().context("owner sent a bad doc id")?;
    let share_uuid: uuid::Uuid = share_id.parse().context("owner sent a bad share id")?;
    // same UUID = same doc. Three cases for a root id we already have:
    // - it is a mirror (re-join): keep it;
    // - it is a TOMBSTONE left by a dropped mirror (the owner revoked, we
    //   soft-deleted; now they share again): revive it — refusing here was
    //   the "revoking blocks future shares of that subtree" bug;
    // - it is a LIVE doc of my own: refuse — an owner naming my doc id (a bug
    //   or a hostile peer who learned it from a share I gave them) must never
    //   turn my doc into their mirror.
    if s.get_doc(root_uuid).is_ok() && s.get_mirror(root_uuid)?.is_none() {
        if s.doc_is_tombstoned(root_uuid)? {
            s.undelete_doc(root_uuid)?;
            if !root_title.is_empty() {
                s.rename_doc(root_uuid, &root_title).ok();
            }
            tracing::info!(root = %root_uuid, "revived tombstoned mirror root on re-join");
        } else {
            anyhow::bail!(
                "owner's share root {root_uuid} collides with a doc of your own — refusing to join"
            );
        }
    }
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
    let (metas, changed, removed) = match res {
        Response::Pulled {
            metas,
            changed,
            removed,
        } => (metas, changed, removed),
        Response::Refused { reason, code } => {
            return Err(Refusal::new(code, format!("owner refused pull: {reason}")).into());
        }
        other => anyhow::bail!("owner refused pull: {other:?}"),
    };

    let mut s = store.lock().unwrap_or_else(|p| p.into_inner());

    // 0. the hijack guard: any id the owner names that is one of MY LIVE docs
    // (exists locally, not a mirror, not a tombstone) is ignored in every
    // step below. Tombstones are leftovers of a mirror we dropped when a
    // share was revoked — those REVIVE (step 0b), they don't collide.
    let foreign_ids: std::collections::HashSet<String> = metas
        .iter()
        .map(|m| m.id.as_str())
        .chain(changed.iter().map(|wd| wd.meta.id.as_str()))
        .filter(|id| {
            id.parse::<uuid::Uuid>().ok().is_some_and(|u| {
                s.get_doc(u).is_ok()
                    && s.get_mirror(u).ok().flatten().is_none()
                    && !s.doc_is_tombstoned(u).unwrap_or(false)
            })
        })
        .map(str::to_string)
        .collect();
    // 0b. revive tombstoned docs the owner still shares (a re-granted share)
    for m in &metas {
        if let Ok(u) = m.id.parse::<uuid::Uuid>()
            && !foreign_ids.contains(&m.id)
            && s.get_doc(u).is_ok()
            && s.doc_is_tombstoned(u).unwrap_or(false)
        {
            s.undelete_doc(u)?;
            tracing::info!(doc = %u, "revived tombstoned mirror doc on pull");
        }
    }
    for id in &foreign_ids {
        tracing::warn!(doc = %id, owner = %owner.pubkey, "owner named one of OUR docs; ignored");
    }
    let changed: Vec<&WireDoc> = changed
        .iter()
        .filter(|wd| !foreign_ids.contains(&wd.meta.id))
        .collect();
    let metas: Vec<_> = metas
        .iter()
        .filter(|m| !foreign_ids.contains(&m.id))
        .collect();
    let summary = PullSummary {
        changed: changed.len(),
        removed: removed.len(),
    };

    // 1. create any docs we've never seen, parents before children
    let mut pending: Vec<&WireDoc> = changed.clone();
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

    // 2. metas: renames and moves don't bump epochs, so reconcile them always.
    // The share ROOT's parent is the owner's private business (wire parent =
    // None) — the grantee files the root wherever they like, so its local
    // parent is never touched; only in-share moves are mirrored.
    for m in &metas {
        let Ok(id) = m.id.parse::<uuid::Uuid>() else {
            continue;
        };
        let Ok(local) = s.get_doc(id) else { continue };
        if local.title != m.title {
            s.rename_doc(id, &m.title).ok();
        }
        let Some(parent) = m.parent.as_ref().and_then(|p| p.parse::<uuid::Uuid>().ok()) else {
            continue; // share root: keep the grantee's filing
        };
        if local.parent_id != Some(parent) {
            s.move_doc(id, Some(parent), None).ok();
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
        // reflect the owner's tend status so the grantee can show it and
        // refuse to tend the doc locally (one side's agents own it)
        s.set_mirror_tended(id, m.tended)?;
    }
    for wd in &changed {
        let Ok(id) = wd.meta.id.parse::<uuid::Uuid>() else {
            continue;
        };
        // belt and braces: only ever replace blocks of a doc that IS a mirror
        if s.get_mirror(id)?.is_none() {
            continue;
        }
        s.mirror_replace_blocks(id, wd.blocks.clone(), wd.meta.epoch, owner.principal)?;
    }

    // 4. docs that left the share: gone from our view, mirror row dropped.
    // Only mirrors of THIS share are ever deleted here.
    for id in &removed {
        if let Ok(u) = id.parse::<uuid::Uuid>()
            && s.get_mirror(u)?.is_some_and(|m| m.share_id == share_id)
        {
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
            base_epoch: Some(mirror.synced_epoch),
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

/// Post a comment on a mirror doc upstream (#64). Applied immediately on the
/// owner; arrives back (and reaches other grantees) via the pull loop.
pub async fn comment_upstream(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    target_block: uuid::Uuid,
    text: &str,
    reply_to: Option<uuid::Uuid>,
) -> Result<String> {
    let (mirror, owner) = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let block = s.read_block(target_block)?;
        let mirror = s
            .get_mirror(block.doc_id)?
            .context("doc is not a mirror — comment locally")?;
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
        Request::Comment {
            share: mirror.share_id.to_string(),
            target_block: target_block.to_string(),
            text: text.to_string(),
            reply_to: reply_to.map(|r| r.to_string()),
        },
    )
    .await?;
    let Response::Commented { block_id } = res else {
        anyhow::bail!("owner refused comment: {res:?}");
    };
    Ok(block_id)
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
