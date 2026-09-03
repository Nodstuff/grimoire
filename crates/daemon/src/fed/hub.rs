//! Hub mode (ADR 0002 § Hub, slice 1): a Grimoire run headless on an
//! always-on box that a team joins. Members PUBLISH subtrees to it (a plain
//! `propose` share offered to the hub, which auto-accepts), the hub RELAYS
//! everything published to every member under one folder, and the hub can
//! own docs of its own. Every doc keeps ONE home: a relayed doc's wire meta
//! names its true owner, and in this slice the hub refuses to take edits on
//! it (`RefusalCode::RelayedReadOnly`) — routing them to the owner is the
//! next slice.
//!
//! Membership: the first paired contact is the first admin; later redeemers
//! are `pending` (no shares) until an admin approves — over the wire, from
//! their own Grimoire (`Request::HubAdmin`), or via the CLI on the hub box.
//! Ejection revokes the member's hub-root share and every publication of
//! theirs, and blocks the contact.
//!
//! Persisted in `settings`: `hub.enabled`, `hub.name`, `hub.root_doc`, and
//! `hub.folder.<contact_id>` (the member's folder under the root).

use super::client::{MintedInvite, join_at, mint_invite_full, pull_share, ticket_for_offer};
use super::wire::{HubMember, HubPublicationInfo};
use anyhow::{Context, Result};
use grimoire_store::{BlockStore, Contact, ContactRole, Membership, PrincipalKind, SqliteStore};
use iroh::{Endpoint, EndpointAddr};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub const SETTING_ENABLED: &str = "hub.enabled";
pub const SETTING_NAME: &str = "hub.name";
pub const SETTING_ROOT: &str = "hub.root_doc";
const DEFAULT_NAME: &str = "Hub";

/// What the rest of the daemon needs to know about hub mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubConfig {
    pub name: String,
    pub root_doc: Uuid,
}

/// Hub mode, if enabled AND its root doc still exists.
pub fn config(store: &SqliteStore) -> Option<HubConfig> {
    if store.get_setting(SETTING_ENABLED).ok().flatten().as_deref() != Some("1") {
        return None;
    }
    let root_doc: Uuid = store.get_setting(SETTING_ROOT).ok().flatten()?.parse().ok()?;
    if store.get_doc(root_doc).is_err() || store.doc_is_tombstoned(root_doc).unwrap_or(true) {
        return None;
    }
    let name = store
        .get_setting(SETTING_NAME)
        .ok()
        .flatten()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_NAME.into());
    Some(HubConfig { name, root_doc })
}

/// Turn hub mode on (idempotent): persist the flag and name, create the root
/// doc named after the hub ONCE (re-creating only if it was deleted), and
/// make the hub's profile name the hub name — that is the petname members
/// see. `name: None` keeps the stored name (default "Hub").
pub fn enable(store: &mut SqliteStore, name: Option<&str>, human: Uuid) -> Result<HubConfig> {
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => store
            .get_setting(SETTING_NAME)?
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_NAME.into()),
    };
    store.set_setting(SETTING_ENABLED, "1")?;
    store.set_setting(SETTING_NAME, &name)?;
    let existing: Option<Uuid> = store
        .get_setting(SETTING_ROOT)?
        .and_then(|s| s.parse().ok())
        .filter(|id| store.get_doc(*id).is_ok() && !store.doc_is_tombstoned(*id).unwrap_or(true));
    let root_doc = match existing {
        Some(id) => {
            // the root follows the hub's name
            if store.get_doc(id)?.title != name {
                store.rename_doc(id, &name)?;
            }
            id
        }
        None => {
            let d = store.create_doc(&name, None, human)?;
            store.set_setting(SETTING_ROOT, &d.id.to_string())?;
            d.id
        }
    };
    // slice 2: members propose on hub-owned docs; the queue is resolved by
    // admins over the wire (there is no human at the hub's own review rail)
    if store.get_doc(root_doc)?.review_policy.is_none() {
        store.set_review_policy(root_doc, Some(grimoire_store::ReviewPolicy::AgentReview))?;
    }
    store.rename_principal(human, &name)?;
    store.set_setting("profile.confirmed", "1")?;
    Ok(HubConfig { name, root_doc })
}

/// Slice 2: transfer offers as the wire and the local routes report them.
pub fn transfers(store: &SqliteStore) -> Vec<super::wire::HubTransferInfo> {
    let contacts = store.list_contacts().unwrap_or_default();
    store
        .list_hub_transfers()
        .unwrap_or_default()
        .into_iter()
        .map(|t| super::wire::HubTransferInfo {
            id: t.id.to_string(),
            member_contact: t.member_contact.to_string(),
            member: contacts
                .iter()
                .find(|c| c.id == t.member_contact)
                .map(|c| display_name(&contacts, c))
                .unwrap_or_else(|| "someone".into()),
            root_doc: t.root_doc.to_string(),
            title: t.title,
            doc_count: t.doc_count,
            state: t.state.as_str().into(),
            at: t.at,
        })
        .collect()
}

/// A contact's name for display: the peer-supplied petname carries a
/// " · 3f9a" fingerprint suffix until renamed; drop it when no other live
/// contact claims the same base name (the suffix exists only to tell two
/// "alice"s apart).
pub fn display_name(contacts: &[Contact], c: &Contact) -> String {
    let base = |p: &str| p.rsplit_once(" · ").map(|(b, _)| b.to_string()).unwrap_or_else(|| p.to_string());
    let mine = base(&c.petname);
    let clash = contacts
        .iter()
        .any(|o| o.id != c.id && !o.revoked && base(&o.petname) == mine);
    if clash { c.petname.clone() } else { mine }
}

/// Docs the hub relays: every mirror doc belonging to a publication's share,
/// mapped to (true owner pubkey, display name). Empty when not a hub.
pub fn relay_set(store: &SqliteStore) -> HashMap<Uuid, (String, String)> {
    let mut out = HashMap::new();
    if config(store).is_none() {
        return out;
    }
    let pubs = store.list_hub_publications().unwrap_or_default();
    if pubs.is_empty() {
        return out;
    }
    let contacts = store.list_contacts().unwrap_or_default();
    let by_share: HashMap<Uuid, &Contact> = pubs
        .iter()
        .filter_map(|p| contacts.iter().find(|c| c.id == p.member_contact).map(|c| (p.share_id, c)))
        .collect();
    for m in store.list_mirrors().unwrap_or_default() {
        if let Some(c) = by_share.get(&m.share_id) {
            out.insert(m.doc_id, (c.pubkey.clone(), display_name(&contacts, c)));
        }
    }
    out
}

/// What a redeem on a hub decided for the redeemer.
#[derive(Debug, PartialEq, Eq)]
pub struct RedeemDecision {
    pub membership: Membership,
    pub role: ContactRole,
}

/// Hub side of a redeem, right after `redeem_invite`. The very first contact
/// becomes the first admin (active); anyone else new is pending and the
/// share the invite activated is taken back (pending members hold no
/// shares). A returning contact keeps their standing; a still-pending one
/// again gets no share.
pub fn on_redeem(
    store: &mut SqliteStore,
    hub: &HubConfig,
    was_new: bool,
    contact: &Contact,
    share: &grimoire_store::Share,
) -> Result<RedeemDecision> {
    let live = store
        .list_contacts()?
        .into_iter()
        .filter(|c| !c.revoked)
        .count();
    let decision = if was_new && live == 1 {
        store.set_contact_role(contact.id, ContactRole::Admin)?;
        store.set_contact_membership(contact.id, Membership::Active)?;
        RedeemDecision {
            membership: Membership::Active,
            role: ContactRole::Admin,
        }
    } else if was_new {
        store.set_contact_membership(contact.id, Membership::Pending)?;
        RedeemDecision {
            membership: Membership::Pending,
            role: ContactRole::Member,
        }
    } else {
        RedeemDecision {
            membership: contact.membership,
            role: contact.role,
        }
    };
    match decision.membership {
        Membership::Active => {
            // membership = the hub root with propose, whatever the invite said
            if share.root_doc == hub.root_doc && share.permission != grimoire_store::SharePermission::Propose {
                store.set_share_permission(share.id, grimoire_store::SharePermission::Propose)?;
            }
        }
        _ => {
            // no shares until approved: the redeem activated one — take it back
            store.set_share_state(share.id, grimoire_store::ShareState::Revoked)?;
            store.delete_share(share.id).ok();
        }
    }
    Ok(decision)
}

/// The membership grant: a `propose` share of the hub root minted for this
/// member, recorded as offered to them. The caller delivers it (`Offer`).
pub fn mint_membership(
    store: &mut SqliteStore,
    node_id: &str,
    hub: &HubConfig,
    contact: &Contact,
) -> Result<(grimoire_store::Share, MintedInvite)> {
    let (share, minted) = mint_invite_full(store, node_id, hub.root_doc, grimoire_store::SharePermission::Propose)?;
    store.set_invite_offered_to(share.id, contact.id)?;
    Ok((share, minted))
}

/// Approve a pending member (hub side), synchronous half: active, then mint
/// the hub-root share recorded as offered to them. Approving an already-active
/// member re-mints (useful when the first offer was lost). The caller
/// delivers with `deliver_membership`.
pub fn approve(
    store: &mut SqliteStore,
    node_id: &str,
    contact_id: Uuid,
) -> Result<(HubConfig, Contact, grimoire_store::Share, MintedInvite)> {
    let hub = config(store).context("this Grimoire is not a hub")?;
    let contact = store
        .list_contacts()?
        .into_iter()
        .find(|c| c.id == contact_id)
        .context("no such member")?;
    if contact.revoked || contact.membership == Membership::Ejected {
        anyhow::bail!(
            "{} was removed from {} — unblock them first",
            display_name(&[], &contact),
            hub.name
        );
    }
    store.set_contact_membership(contact.id, Membership::Active)?;
    let (share, minted) = mint_membership(store, node_id, &hub, &contact)?;
    Ok((hub, contact, share, minted))
}

/// Deliver a minted membership share as an `Offer`. Best-effort: if they are
/// offline the invite waits (7 days) and the link comes back so an admin on
/// the box can pass it by hand.
pub async fn deliver_membership(
    endpoint: &Endpoint,
    hub: &HubConfig,
    contact: &Contact,
    share: &grimoire_store::Share,
    minted: &MintedInvite,
) -> serde_json::Value {
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        super::client::offer_share(
            endpoint,
            &contact.pubkey,
            share.id,
            &hub.name,
            grimoire_store::SharePermission::Propose,
            minted,
        ),
    )
    .await;
    let name = display_name(&[], contact);
    match delivered {
        Ok(Ok(())) => {
            tracing::info!(member = name, "hub: member approved; hub share offered");
            serde_json::json!({"ok": true, "member": name, "delivered": true})
        }
        Ok(Err(e)) => {
            tracing::warn!(member = name, "hub: member approved but the offer was not delivered: {e:#}");
            serde_json::json!({"ok": true, "member": name, "delivered": false, "link": minted.link, "reason": format!("{e:#}")})
        }
        Err(_) => serde_json::json!({"ok": true, "member": name, "delivered": false, "link": minted.link, "reason": "they are offline or unreachable right now"}),
    }
}

/// Eject a member (hub side): ejected + blocked (their shares revoke with the
/// block), every publication of theirs dropped (the relay loses those docs on
/// the other members' next pull), their folder removed. Returns how many docs
/// were dropped.
pub fn eject(store: &mut SqliteStore, contact_id: Uuid) -> Result<usize> {
    let hub = config(store).context("this Grimoire is not a hub")?;
    let contact = store
        .list_contacts()?
        .into_iter()
        .find(|c| c.id == contact_id)
        .context("no such member")?;
    store.set_contact_membership(contact.id, Membership::Ejected)?;
    store.set_contact_role(contact.id, ContactRole::Member)?;
    store.revoke_contact(contact.id)?;
    let mut dropped = 0;
    for p in store.list_hub_publications()? {
        if p.member_contact != contact.id {
            continue;
        }
        store.remove_hub_publication(p.share_id)?;
        dropped += super::loops::drop_dead_share(store, p.share_id).len();
    }
    if let Some(folder) = store
        .get_setting(&folder_key(contact.id))?
        .and_then(|s| s.parse::<Uuid>().ok())
    {
        store.delete_doc(folder).ok();
    }
    tracing::info!(member = contact.petname, hub = hub.name, dropped, "hub: member ejected");
    Ok(dropped)
}

pub fn set_role(store: &mut SqliteStore, contact_id: Uuid, role: ContactRole) -> Result<()> {
    config(store).context("this Grimoire is not a hub")?;
    let contact = store
        .list_contacts()?
        .into_iter()
        .find(|c| c.id == contact_id)
        .context("no such member")?;
    if contact.membership != Membership::Active {
        anyhow::bail!("only active members can be admins");
    }
    store.set_contact_role(contact.id, role)?;
    Ok(())
}

/// The member list as the wire and the local routes report it.
pub fn members(store: &SqliteStore) -> Result<Vec<HubMember>> {
    let contacts = store.list_contacts()?;
    let pubs = store.list_hub_publications()?;
    Ok(contacts
        .iter()
        .filter(|c| !c.is_hub)
        .map(|c| HubMember {
            contact_id: c.id.to_string(),
            petname: display_name(&contacts, c),
            pubkey: c.pubkey.clone(),
            role: c.role.as_str().into(),
            membership: c.membership.as_str().into(),
            paired_at: c.paired_at.clone(),
            publications: pubs
                .iter()
                .filter(|p| p.member_contact == c.id)
                .map(|p| HubPublicationInfo {
                    share_id: p.share_id.to_string(),
                    root_doc: p.root_doc.to_string(),
                    root_title: store.get_doc(p.root_doc).map(|d| d.title).unwrap_or_default(),
                    doc_count: store
                        .list_mirrors()
                        .map(|ms| ms.iter().filter(|m| m.share_id == p.share_id).count())
                        .unwrap_or(0),
                    published_at: p.published_at.clone(),
                })
                .collect(),
        })
        .collect())
}

fn folder_key(contact_id: Uuid) -> String {
    format!("hub.folder.{contact_id}")
}

/// `<hub root>/<member>`: created on the member's first publication, owned
/// by the hub (so it is served like any hub doc), remembered in settings.
pub fn member_folder(store: &mut SqliteStore, hub: &HubConfig, contact: &Contact) -> Result<Uuid> {
    if let Some(id) = store
        .get_setting(&folder_key(contact.id))?
        .and_then(|s| s.parse::<Uuid>().ok())
        .filter(|id| store.get_doc(*id).is_ok() && !store.doc_is_tombstoned(*id).unwrap_or(true))
    {
        return Ok(id);
    }
    let human = store
        .list_principals()?
        .into_iter()
        .find(|p| p.kind == PrincipalKind::Human)
        .map(|p| p.id)
        .context("hub has no human principal")?;
    let name = display_name(&store.list_contacts()?, contact);
    let folder = store.create_doc(&name, Some(hub.root_doc), human)?;
    store.set_setting(&folder_key(contact.id), &folder.id.to_string())?;
    Ok(folder.id)
}

/// Hub side of a PUBLISH: an active member offered us a `propose` share of
/// their subtree. Accept it exactly like a person would (redeem the offer's
/// secret, materialize the mirror root), file the root under the member's
/// folder, record the publication, and pull the tree so the relay has
/// content at once. `addr` is how we reach the member — by pubkey in
/// production (discovery), explicit in tests.
pub async fn accept_publication(
    endpoint: &Endpoint,
    store: &Arc<Mutex<SqliteStore>>,
    offer_id: Uuid,
    addr: EndpointAddr,
) -> Result<Uuid> {
    let (hub, offer, member) = {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        let hub = config(&s).context("not a hub")?;
        let offer = s.get_share_offer(offer_id)?;
        let member = s
            .list_contacts()?
            .into_iter()
            .find(|c| c.id == offer.from_contact)
            .context("member contact is gone")?;
        (hub, offer, member)
    };
    if member.membership != Membership::Active || member.revoked {
        anyhow::bail!("only active members can publish");
    }
    if offer.permission != grimoire_store::SharePermission::Propose {
        anyhow::bail!("a publication must be a propose share");
    }
    let ticket = ticket_for_offer(&offer);
    let out = join_at(endpoint, store, &ticket, addr.clone()).await?;
    let root: Uuid = out.root_doc.parse().context("member sent a bad root id")?;
    {
        let mut s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.set_share_offer_state(offer.id, grimoire_store::ShareOfferState::Accepted)?;
        let folder = member_folder(&mut s, &hub, &member)?;
        if s.get_doc(root)?.parent_id != Some(folder) {
            s.move_doc(root, Some(folder), None)?;
        }
        s.add_hub_publication(offer.share_id, member.id, root)?;
        tracing::info!(
            member = member.petname,
            root = %root,
            title = out.root_title,
            "hub: publication accepted and filed"
        );
    }
    match pull_share(endpoint, store, addr, &member, offer.share_id).await {
        Ok(sum) => tracing::info!(root = %root, docs = sum.changed, "hub: first pull of publication"),
        Err(e) => tracing::warn!(root = %root, "hub: first pull of publication failed: {e:#}"),
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(id: Uuid, petname: &str) -> Contact {
        Contact {
            id,
            pubkey: id.to_string(),
            petname: petname.into(),
            principal: Uuid::now_v7(),
            verified: false,
            revoked: false,
            paired_at: String::new(),
            role: ContactRole::Member,
            membership: Membership::Active,
            is_hub: false,
        }
    }

    #[test]
    fn display_name_drops_the_fingerprint_suffix_unless_two_share_a_name() {
        let a = contact(Uuid::now_v7(), "alice · 3f9a");
        let b = contact(Uuid::now_v7(), "bob · 77aa");
        assert_eq!(display_name(&[a.clone(), b.clone()], &a), "alice");
        let a2 = contact(Uuid::now_v7(), "alice · c0de");
        assert_eq!(display_name(&[a.clone(), a2.clone(), b.clone()], &a), "alice · 3f9a");
        assert_eq!(display_name(&[a.clone(), a2.clone()], &a2), "alice · c0de");
        // a blocked namesake does not force the suffix
        let mut gone = a2.clone();
        gone.revoked = true;
        assert_eq!(display_name(&[a.clone(), gone], &a), "alice");
        // a renamed contact (no suffix) is shown as is
        let t = contact(Uuid::now_v7(), "Tom");
        assert_eq!(display_name(&[t.clone()], &t), "Tom");
    }

    #[test]
    fn enable_is_idempotent_and_the_root_follows_the_name() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        assert!(config(&s).is_none());
        let c1 = enable(&mut s, Some("Team"), tom.id).unwrap();
        assert_eq!(c1.name, "Team");
        assert_eq!(s.get_doc(c1.root_doc).unwrap().title, "Team");
        assert_eq!(config(&s), Some(c1.clone()));
        // profile name = hub name
        let human = s.list_principals().unwrap().into_iter().find(|p| p.kind == PrincipalKind::Human).unwrap();
        assert_eq!(human.display_name, "Team");
        // again with no name: same root, same name
        let c2 = enable(&mut s, None, tom.id).unwrap();
        assert_eq!(c2, c1);
        // rename: same root doc, new title
        let c3 = enable(&mut s, Some("Crew"), tom.id).unwrap();
        assert_eq!(c3.root_doc, c1.root_doc);
        assert_eq!(s.get_doc(c3.root_doc).unwrap().title, "Crew");
        // root deleted → config off until re-enabled, which recreates it
        s.delete_doc(c3.root_doc).unwrap();
        assert!(config(&s).is_none());
        let c4 = enable(&mut s, None, tom.id).unwrap();
        assert_ne!(c4.root_doc, c3.root_doc);
        assert_eq!(c4.name, "Crew");
    }
}
