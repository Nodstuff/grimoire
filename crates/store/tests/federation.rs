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

    // re-pair: same contact and principal; the owner's petname wins — a
    // peer's self-description never overwrites it
    let again = s.pair_contact(ALICE_KEY, "alice-work").unwrap();
    assert_eq!(again.id, alice.id);
    assert_eq!(again.principal, alice.principal);
    assert_eq!(again.petname, "alice");
    assert_eq!(
        s.contact_by_pubkey(ALICE_KEY).unwrap().unwrap().petname,
        "alice"
    );
    assert_eq!(s.list_contacts().unwrap().len(), 1);
    // rename_contact is the way to change it
    s.rename_contact(alice.id, "alice-work").unwrap();
    assert_eq!(
        s.pair_contact(ALICE_KEY, "whatever").unwrap().petname,
        "alice-work"
    );
}

#[test]
fn revoked_contact_cannot_redeem_until_unrevoked() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("runbook", None, tom.id).unwrap();
    let share = s
        .create_share(doc.id, None, SharePermission::View, None)
        .unwrap();
    s.create_invite(share.id, "h1", "2099-01-01T00:00:00.000Z")
        .unwrap();
    let (alice, _) = s.redeem_invite("h1", ALICE_KEY, "alice").unwrap();
    s.revoke_contact(alice.id).unwrap();

    // owner mints a fresh invite; the revoked peer tries to redeem it
    let share2 = s
        .create_share(doc.id, None, SharePermission::Propose, None)
        .unwrap();
    s.create_invite(share2.id, "h2", "2099-01-01T00:00:00.000Z")
        .unwrap();
    let err = s.redeem_invite("h2", ALICE_KEY, "alice-again");
    match err {
        Err(StoreError::InvalidOp(msg)) => assert!(msg.contains("revoked"), "{msg}"),
        other => panic!("expected InvalidOp, got {other:?}"),
    }
    // nothing changed: still revoked, petname intact, share still offered
    let alice = s.contact_by_pubkey(ALICE_KEY).unwrap().unwrap();
    assert!(alice.revoked);
    assert_eq!(alice.petname, "alice");
    let share2_now = s.get_share(share2.id).unwrap();
    assert_eq!(share2_now.state, ShareState::Offered);
    assert!(share2_now.contact.is_none());
    // and the invite was NOT burned: it redeems fine once un-revoked
    s.unrevoke_contact(alice.id).unwrap();
    assert!(!s.contact_by_pubkey(ALICE_KEY).unwrap().unwrap().revoked);
    // un-revoke does not touch the shares revoked alongside the contact
    assert_eq!(s.get_share(share.id).unwrap().state, ShareState::Revoked);
    let (alice2, share2_now) = s.redeem_invite("h2", ALICE_KEY, "alice-again").unwrap();
    assert_eq!(alice2.id, alice.id);
    assert_eq!(
        alice2.petname, "alice",
        "redeem never renames an existing contact"
    );
    assert_eq!(share2_now.state, ShareState::Active);
    assert_eq!(share2_now.contact, Some(alice.id));
}

#[test]
fn unrevoke_unknown_contact_is_not_found() {
    let (mut s, _) = store_with_tom();
    assert!(matches!(
        s.unrevoke_contact(uuid::Uuid::now_v7()),
        Err(StoreError::NotFound(_))
    ));
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
    s.upsert_mirror(
        mirror_doc.id,
        owner.id,
        share_id,
        42,
        SharePermission::Propose,
    )
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
    s.upsert_mirror(
        mirror_doc.id,
        owner.id,
        uuid::Uuid::now_v7(),
        3,
        SharePermission::View,
    )
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
    assert!(
        s.create_share(own.id, None, SharePermission::View, None)
            .is_ok()
    );
}

#[test]
fn doc_is_tended_walks_ancestors_and_ignores_disabled() {
    let (mut s, tom) = store_with_tom();
    let root = s.create_doc("Runbook", None, tom.id).unwrap();
    let child = s.create_doc("Deploys", Some(root.id), tom.id).unwrap();
    let grandchild = s.create_doc("Rollback", Some(child.id), tom.id).unwrap();
    let other = s.create_doc("Notes", None, tom.id).unwrap();
    // no gardeners yet
    assert!(!s.doc_is_tended(root.id).unwrap());
    // a gardener scoped to root tends the whole subtree
    let g = s
        .create_gardener("keeper", GardenerKind::Keeper, "keep", Some(root.id), ConfidencePolicy::Review)
        .unwrap();
    assert!(s.doc_is_tended(root.id).unwrap());
    assert!(s.doc_is_tended(child.id).unwrap());
    assert!(s.doc_is_tended(grandchild.id).unwrap());
    assert!(!s.doc_is_tended(other.id).unwrap(), "sibling subtree untended");
    // disabling the gardener untends the subtree
    s.set_gardener_enabled(g.id, false).unwrap();
    assert!(!s.doc_is_tended(grandchild.id).unwrap());
}

#[test]
fn mirror_tended_flag_round_trips() {
    let (mut s, _tom) = store_with_tom();
    let owner = s.pair_contact(&"cd".repeat(32), "owner").unwrap();
    let doc = uuid::Uuid::now_v7();
    s.create_doc_with_id(doc, "Shared", None, owner.principal).unwrap();
    let share = uuid::Uuid::now_v7();
    s.upsert_mirror(doc, owner.id, share, 1, SharePermission::View).unwrap();
    assert!(!s.get_mirror(doc).unwrap().unwrap().owner_tended);
    s.set_mirror_tended(doc, true).unwrap();
    assert!(s.get_mirror(doc).unwrap().unwrap().owner_tended);
    // upsert (a re-pull) must not clobber the tended flag
    s.upsert_mirror(doc, owner.id, share, 2, SharePermission::View).unwrap();
    assert!(s.get_mirror(doc).unwrap().unwrap().owner_tended, "tended survives re-upsert");
}

#[test]
fn maintainer_trust_round_trips_and_parses_alias() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("Shared", None, tom.id).unwrap();
    let share = s.create_share(doc.id, None, SharePermission::Propose, None).unwrap();
    assert_eq!(share.trust, ShareTrust::Review);
    s.set_share_trust(share.id, ShareTrust::Green).unwrap();
    assert_eq!(s.get_share(share.id).unwrap().trust, ShareTrust::Green);
    assert_eq!(ShareTrust::parse("maintainer"), Some(ShareTrust::Green));
    assert_eq!(ShareTrust::parse("green"), Some(ShareTrust::Green));
    assert_eq!(ShareTrust::Green.as_str(), "green");
}

#[test]
fn recent_remote_ops_lists_only_applied_remote_edits() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("Runbook", None, tom.id).unwrap();
    let alice = s.pair_contact(&"ef".repeat(32), "alice").unwrap();
    let mk = |content: &str| OpInput {
        kind: OpKind::Insert {
            block_id: uuid::Uuid::now_v7(),
            parent_id: None,
            order_key: "".into(),
            block_type: BlockType::Paragraph,
            content: content.into(),
            refers_to: None,
        },
        source_refs: vec![],
    };
    // a human edit: not remote, excluded
    s.apply(doc.id, 0, tom.id, vec![mk("tom wrote this")]).unwrap();
    // a maintainer (remote) edit that APPLIED: included
    let e = s.get_doc(doc.id).unwrap().current_epoch;
    s.propose(doc.id, e, alice.principal, vec![mk("alice wrote this")]).unwrap();
    // a parked (unapplied) remote proposal: excluded
    s.park(doc.id, alice.principal, vec![mk("alice proposed this")], "").unwrap();

    let feed = s.recent_remote_ops(10).unwrap();
    assert_eq!(feed.len(), 1, "only the applied remote edit: {feed:?}");
    assert_eq!(feed[0].principal_name, "alice");
    assert_eq!(feed[0].doc_title, "Runbook");
    assert_eq!(feed[0].op_type, "insert");
    assert_eq!(feed[0].principal, alice.principal);
}

/// Field bug: real docs nest paragraphs under headings, and the wire order is
/// per-sibling order_key — so a child can arrive before its parent. With
/// blocks.parent_id a FK, the naive insert failed and left mirrors as titles
/// with no content. The replace must succeed whatever order blocks arrive in.
#[test]
fn mirror_replace_blocks_accepts_children_before_parents() {
    let (mut s, _tom) = store_with_tom();
    let owner = s.pair_contact(&"ab".repeat(32), "owner").unwrap();
    let doc = uuid::Uuid::now_v7();
    s.create_doc_with_id(doc, "Nested", None, owner.principal).unwrap();
    s.upsert_mirror(doc, owner.id, uuid::Uuid::now_v7(), 0, SharePermission::View).unwrap();
    let h1 = uuid::Uuid::now_v7();
    let h2 = uuid::Uuid::now_v7();
    let p_under_h2 = uuid::Uuid::now_v7();
    let p_under_h1 = uuid::Uuid::now_v7();
    let mk = |id, parent, key: &str, ty, content: &str| MirrorBlock {
        id,
        parent_id: parent,
        order_key: key.into(),
        block_type: ty,
        content: content.into(),
        refers_to: None,
    };
    // deliberately WORST order: deepest children first, parents last
    let blocks = vec![
        mk(p_under_h2, Some(h2), "i", BlockType::Paragraph, "deep paragraph"),
        mk(p_under_h1, Some(h1), "r", BlockType::Paragraph, "para under h1"),
        mk(h2, Some(h1), "i", BlockType::Heading, "## Sub"),
        mk(h1, None, "i", BlockType::Heading, "# Top"),
    ];
    s.mirror_replace_blocks(doc, blocks, 7, owner.principal)
        .expect("children-before-parents must not fail the FK");
    let tree = s.read_doc(doc).unwrap();
    assert_eq!(tree.doc.current_epoch, 7);
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(tree.roots[0].block.content, "# Top");
    let kids: Vec<_> = tree.roots[0].children.iter().map(|n| n.block.content.as_str()).collect();
    assert_eq!(kids, vec!["## Sub", "para under h1"]);
    assert_eq!(tree.roots[0].children[0].children[0].block.content, "deep paragraph");
    // a second replace (a later pull) works too — the DELETE + reinsert path
    s.mirror_replace_blocks(doc, vec![mk(h1, None, "i", BlockType::Heading, "# Top v2")], 8, owner.principal).unwrap();
    assert_eq!(s.read_doc(doc).unwrap().roots[0].block.content, "# Top v2");
}

#[test]
fn profile_rename_and_settings() {
    let (mut s, tom) = store_with_tom();
    s.rename_principal(tom.id, "  Tom M  ").unwrap();
    assert_eq!(s.get_principal(tom.id).unwrap().display_name, "Tom M", "trimmed");
    assert!(s.rename_principal(tom.id, "   ").is_err(), "empty refused");
    assert!(s.rename_principal(tom.id, &"x".repeat(65)).is_err(), "too long refused");
    assert!(s.rename_principal(uuid::Uuid::now_v7(), "ghost").is_err(), "unknown principal");
    assert_eq!(s.get_setting("profile.confirmed").unwrap(), None);
    s.set_setting("profile.confirmed", "1").unwrap();
    assert_eq!(s.get_setting("profile.confirmed").unwrap().as_deref(), Some("1"));
    s.set_setting("profile.confirmed", "0").unwrap(); // upsert
    assert_eq!(s.get_setting("profile.confirmed").unwrap().as_deref(), Some("0"));
}

#[test]
fn mirror_sync_result_records_errors_and_clears_on_success() {
    let (mut s, _tom) = store_with_tom();
    let owner = s.pair_contact(&"12".repeat(32), "owner").unwrap();
    let share = uuid::Uuid::now_v7();
    let (a, b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    for (d, t) in [(a, "A"), (b, "B")] {
        s.create_doc_with_id(d, t, None, owner.principal).unwrap();
        s.upsert_mirror(d, owner.id, share, 1, SharePermission::View).unwrap();
    }
    let m = s.get_mirror(a).unwrap().unwrap();
    assert!(m.last_pulled_at.is_none() && m.last_error.is_none(), "fresh mirror: never synced");
    s.set_mirror_sync_result(share, Some("FOREIGN KEY constraint failed")).unwrap();
    for d in [a, b] {
        let m = s.get_mirror(d).unwrap().unwrap();
        assert_eq!(m.last_error.as_deref(), Some("FOREIGN KEY constraint failed"));
        assert!(m.last_pulled_at.is_none(), "a failure is not a pull");
    }
    s.set_mirror_sync_result(share, None).unwrap();
    for d in [a, b] {
        let m = s.get_mirror(d).unwrap().unwrap();
        assert!(m.last_error.is_none(), "success clears the error");
        assert!(m.last_pulled_at.is_some(), "success stamps the time");
    }
    // upsert (a pull re-claiming the row) must not wipe sync health
    s.upsert_mirror(a, owner.id, share, 2, SharePermission::View).unwrap();
    assert!(s.get_mirror(a).unwrap().unwrap().last_pulled_at.is_some());
}

#[test]
fn delete_share_clears_only_revoked_shares_and_their_invites() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("D", None, tom.id).unwrap();
    let share = s.create_share(doc.id, None, SharePermission::View, None).unwrap();
    s.create_invite(share.id, "h1", "2099-01-01T00:00:00.000Z").unwrap();
    assert!(s.delete_share(share.id).is_err(), "offered share must be revoked first");
    s.set_share_state(share.id, ShareState::Revoked).unwrap();
    s.delete_share(share.id).unwrap();
    assert!(s.get_share(share.id).is_err(), "row gone");
    assert!(s.redeem_invite("h1", &"34".repeat(32), "x").is_err(), "its invite is gone too");
    assert!(s.list_shares().unwrap().is_empty());
}
