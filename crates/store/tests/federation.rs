//! Acceptance tests for #55 (ADR 0002 slice 1): contacts pair idempotently,
//! invites burn on redeem, share containment is recursive, mirrors keep
//! cursors, revoking a contact revokes its shares.

use grimoire_store::*;

fn store_with_tom() -> (SqliteStore, Principal) {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let tom = s
        .create_principal(PrincipalKind::Human, "Tom", None)
        .unwrap();
    (s, tom)
}

const ALICE_KEY: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

#[test]
fn pair_contact_is_idempotent_on_pubkey_and_creates_remote_principal() {
    let (mut s, _) = store_with_tom();
    let alice = s.pair_contact(ALICE_KEY, "alice").unwrap();
    assert!(!alice.verified);
    assert!(!alice.revoked);
    let principal = s.get_principal(alice.principal).unwrap();
    assert_eq!(principal.kind, PrincipalKind::Remote);
    assert_eq!(principal.pubkey.as_deref(), Some(ALICE_KEY));

    // re-pair: same contact and principal, petname updated
    let again = s.pair_contact(ALICE_KEY, "alice-work").unwrap();
    assert_eq!(again.id, alice.id);
    assert_eq!(again.principal, alice.principal);
    assert_eq!(again.petname, "alice-work");
    assert_eq!(s.list_contacts().unwrap().len(), 1);
}

#[test]
fn invite_redeem_binds_contact_activates_share_and_burns_secret() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("runbook", None, tom.id).unwrap();
    let share = s
        .create_share(doc.id, None, SharePermission::Propose, None)
        .unwrap();
    assert_eq!(share.state, ShareState::Offered);
    assert!(share.contact.is_none());

    s.create_invite(share.id, "hash-of-secret", "2099-01-01T00:00:00.000Z")
        .unwrap();

    let (alice, share) = s
        .redeem_invite("hash-of-secret", ALICE_KEY, "alice")
        .unwrap();
    assert_eq!(share.contact, Some(alice.id));
    assert_eq!(share.state, ShareState::Active);
    assert_eq!(share.permission, SharePermission::Propose);

    // burned: second redeem refused, even from the same key
    let err = s.redeem_invite("hash-of-secret", ALICE_KEY, "alice");
    assert!(matches!(err, Err(StoreError::InvalidOp(_))));
}

#[test]
fn expired_and_unknown_invites_are_refused() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("runbook", None, tom.id).unwrap();
    let share = s
        .create_share(doc.id, None, SharePermission::View, None)
        .unwrap();
    s.create_invite(share.id, "stale-hash", "2000-01-01T00:00:00.000Z")
        .unwrap();

    assert!(matches!(
        s.redeem_invite("stale-hash", ALICE_KEY, "alice"),
        Err(StoreError::InvalidOp(_))
    ));
    assert!(matches!(
        s.redeem_invite("never-minted", ALICE_KEY, "alice"),
        Err(StoreError::InvalidOp(_))
    ));
    // nothing paired along the way
    assert!(s.list_contacts().unwrap().is_empty());
}

#[test]
fn share_containment_is_recursive_and_skips_deleted() {
    let (mut s, tom) = store_with_tom();
    let root = s.create_doc("shared-root", None, tom.id).unwrap();
    let child = s.create_doc("child", Some(root.id), tom.id).unwrap();
    let grandchild = s.create_doc("grandchild", Some(child.id), tom.id).unwrap();
    let outside = s.create_doc("private", None, tom.id).unwrap();
    let deleted = s.create_doc("gone", Some(root.id), tom.id).unwrap();
    s.delete_doc(deleted.id).unwrap();

    let share = s
        .create_share(root.id, None, SharePermission::View, None)
        .unwrap();
    let ids: Vec<_> = s
        .docs_in_share(share.id)
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert!(ids.contains(&root.id));
    assert!(ids.contains(&child.id));
    assert!(ids.contains(&grandchild.id));
    assert!(!ids.contains(&outside.id));
    assert!(!ids.contains(&deleted.id));

    // reverse direction: a doc knows which shares expose it
    let containing = s.shares_containing(grandchild.id).unwrap();
    assert_eq!(containing.len(), 1);
    assert_eq!(containing[0].id, share.id);
    assert!(s.shares_containing(outside.id).unwrap().is_empty());
}

#[test]
fn doc_moved_into_shared_subtree_becomes_contained() {
    let (mut s, tom) = store_with_tom();
    let root = s.create_doc("shared-root", None, tom.id).unwrap();
    let wanderer = s.create_doc("wanderer", None, tom.id).unwrap();
    let share = s
        .create_share(root.id, None, SharePermission::View, None)
        .unwrap();

    assert!(s.shares_containing(wanderer.id).unwrap().is_empty());
    s.move_doc(wanderer.id, Some(root.id), None).unwrap();
    assert_eq!(s.shares_containing(wanderer.id).unwrap().len(), 1);
    assert!(
        s.docs_in_share(share.id)
            .unwrap()
            .iter()
            .any(|d| d.id == wanderer.id)
    );
}

#[test]
fn revoking_a_contact_revokes_its_shares() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("runbook", None, tom.id).unwrap();
    let share = s
        .create_share(doc.id, None, SharePermission::View, None)
        .unwrap();
    s.create_invite(share.id, "h", "2099-01-01T00:00:00.000Z")
        .unwrap();
    let (alice, share) = s.redeem_invite("h", ALICE_KEY, "alice").unwrap();

    s.revoke_contact(alice.id).unwrap();
    let share = s.get_share(share.id).unwrap();
    assert_eq!(share.state, ShareState::Revoked);
    let alice = s.contact_by_pubkey(ALICE_KEY).unwrap().unwrap();
    assert!(alice.revoked);
    // revoked shares no longer report containment
    assert!(s.shares_containing(doc.id).unwrap().is_empty());
    // provenance survives: principal row intact
    assert!(s.get_principal(alice.principal).is_ok());
}

#[test]
fn mirror_cursor_upserts() {
    let (mut s, tom) = store_with_tom();
    // grantee-side: the mirror doc exists locally under the same UUID
    let mirror_doc = s.create_doc("their-runbook", None, tom.id).unwrap();
    let owner = s.pair_contact(ALICE_KEY, "alice").unwrap();
    let share_id = uuid::Uuid::now_v7(); // owner-side id, foreign to us

    s.upsert_mirror(mirror_doc.id, owner.id, share_id, 0, SharePermission::View)
        .unwrap();
    s.upsert_mirror(mirror_doc.id, owner.id, share_id, 42, SharePermission::Propose)
        .unwrap();

    let m = s.get_mirror(mirror_doc.id).unwrap().unwrap();
    assert_eq!(m.synced_epoch, 42);
    assert_eq!(m.permission, SharePermission::Propose);
    assert_eq!(m.owner, owner.id);
    assert_eq!(m.share_id, share_id);
    assert_eq!(s.list_mirrors().unwrap().len(), 1);
    assert!(s.get_mirror(uuid::Uuid::now_v7()).unwrap().is_none());
}

#[test]
fn reinvite_supersedes_older_active_share_for_same_contact() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("runbook", None, tom.id).unwrap();
    let first = s
        .create_share(doc.id, None, SharePermission::View, None)
        .unwrap();
    s.create_invite(first.id, "h1", "2099-01-01T00:00:00.000Z")
        .unwrap();
    s.redeem_invite("h1", ALICE_KEY, "alice").unwrap();

    // second invite for the same subtree, upgraded permission
    let second = s
        .create_share(doc.id, None, SharePermission::Propose, None)
        .unwrap();
    s.create_invite(second.id, "h2", "2099-01-01T00:00:00.000Z")
        .unwrap();
    s.redeem_invite("h2", ALICE_KEY, "alice").unwrap();

    assert_eq!(s.get_share(first.id).unwrap().state, ShareState::Revoked);
    let second = s.get_share(second.id).unwrap();
    assert_eq!(second.state, ShareState::Active);
    assert_eq!(second.permission, SharePermission::Propose);
    // exactly one active share exposes the doc now
    assert_eq!(s.shares_containing(doc.id).unwrap().len(), 1);
}

#[test]
fn resharing_a_mirror_is_refused() {
    let (mut s, tom) = store_with_tom();
    let folder = s.create_doc("Shared with me", None, tom.id).unwrap();
    let mirror_doc = s
        .create_doc_with_id(uuid::Uuid::now_v7(), "their-doc", Some(folder.id), tom.id)
        .unwrap();
    let owner = s.pair_contact(ALICE_KEY, "alice").unwrap();
    s.upsert_mirror(mirror_doc.id, owner.id, uuid::Uuid::now_v7(), 3, SharePermission::View)
        .unwrap();

    // sharing the mirror itself: refused
    assert!(matches!(
        s.create_share(mirror_doc.id, None, SharePermission::View, None),
        Err(StoreError::InvalidOp(_))
    ));
    // sharing an ancestor folder that contains it: also refused
    assert!(matches!(
        s.create_share(folder.id, None, SharePermission::View, None),
        Err(StoreError::InvalidOp(_))
    ));
    // a sibling subtree with no mirrors is fine
    let own = s.create_doc("my own", None, tom.id).unwrap();
    assert!(s.create_share(own.id, None, SharePermission::View, None).is_ok());
}
