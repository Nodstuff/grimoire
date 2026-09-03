//! Store hygiene regressions (0.7.2): docs always keyed, trashed docs stay
//! out of every read surface, imports and redeems are atomic, the change
//! stamp sees every docs mutation.

use grimoire_store::*;
use uuid::Uuid;

fn seed() -> (SqliteStore, Principal) {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let tom = s
        .create_principal(PrincipalKind::Human, "Tom", None)
        .unwrap();
    (s, tom)
}

fn add_doc(s: &mut SqliteStore, tom: &Principal, title: &str, md: &str) -> Uuid {
    let (id, _) = import::import_markdown(s, title, None, tom.id, md).unwrap();
    id
}

fn insert(parent: Option<Uuid>, key: &str, content: &str) -> OpInput {
    OpInput {
        kind: OpKind::Insert {
            block_id: Uuid::now_v7(),
            parent_id: parent,
            order_key: key.into(),
            block_type: BlockType::Paragraph,
            content: content.into(),
            refers_to: None,
        },
        source_refs: vec![],
    }
}

/// Item 3: create_doc / create_doc_with_id / import never leave sort_key
/// NULL; siblings get distinct ascending keys in creation order.
#[test]
fn new_docs_get_distinct_sort_keys_in_creation_order() {
    let (mut s, tom) = seed();
    let folder = s.create_doc("folder", None, tom.id).unwrap();
    assert!(folder.sort_key.is_some(), "root doc keyed");

    let z = s.create_doc("zeta", Some(folder.id), tom.id).unwrap();
    let a = s
        .create_doc_with_id(Uuid::now_v7(), "alpha", Some(folder.id), tom.id)
        .unwrap();
    let m = add_doc(&mut s, &tom, "mid", "# x\n");
    let (zk, ak) = (z.sort_key.clone().unwrap(), a.sort_key.clone().unwrap());
    assert!(zk < ak, "creation order, not title order: {zk} < {ak}");
    assert!(
        s.get_doc(m).unwrap().sort_key.is_some(),
        "imported doc keyed"
    );

    // list_docs: folder children come back in creation order
    let kids: Vec<String> = s
        .list_docs()
        .unwrap()
        .into_iter()
        .filter(|d| d.parent_id == Some(folder.id))
        .map(|d| d.title)
        .collect();
    assert_eq!(kids, ["zeta", "alpha"]);
}

/// Item 4: a doc in Trash disappears from search (FTS and LIKE), backlinks,
/// tags, the vector index and vector hits — its blocks are only tombstoned
/// by proxy, so every read must join docs.deleted.
#[test]
fn trashed_doc_leaves_every_read_surface() {
    let (mut s, tom) = seed();
    let target = add_doc(&mut s, &tom, "Target", "# Target\n");
    let doc = add_doc(
        &mut s,
        &tom,
        "linker",
        "---\ntags:\n  - trashme\n---\n\nsee [[Target]] about gardeners\n",
    );
    let block = s
        .read_doc(doc)
        .unwrap()
        .roots
        .iter()
        .find(|n| n.block.content.contains("gardeners"))
        .unwrap()
        .block
        .id;
    s.set_block_vec(block, 1, &[1.0, 0.0]).unwrap();

    // sanity: live doc is visible everywhere
    assert_eq!(s.search_blocks("gardeners", 10).unwrap().len(), 1);
    assert_eq!(s.search_blocks("ga", 10).unwrap().len(), 1, "LIKE path");
    assert_eq!(s.backlinks(target).unwrap().len(), 1);
    assert_eq!(s.docs_by_tag("trashme").unwrap().len(), 1);
    assert!(s.list_tags().unwrap().iter().any(|(t, _)| t == "trashme"));
    assert_eq!(s.block_vecs().unwrap().len(), 1);
    assert_eq!(s.blocks_as_hits(&[block]).unwrap().len(), 1);
    assert_eq!(s.linking_blocks("Target").unwrap().len(), 1);
    assert_eq!(s.raw_links().unwrap().len(), 1);
    assert_eq!(s.raw_doc_tags().unwrap().len(), 1);

    s.delete_doc(doc).unwrap();

    assert!(s.search_blocks("gardeners", 10).unwrap().is_empty(), "fts");
    assert!(s.search_blocks("ga", 10).unwrap().is_empty(), "like");
    assert!(s.backlinks(target).unwrap().is_empty(), "backlinks");
    assert!(s.docs_by_tag("trashme").unwrap().is_empty(), "docs_by_tag");
    assert!(
        !s.list_tags().unwrap().iter().any(|(t, _)| t == "trashme"),
        "list_tags"
    );
    assert!(s.block_vecs().unwrap().is_empty(), "block_vecs");
    assert!(
        s.blocks_as_hits(&[block]).unwrap().is_empty(),
        "blocks_as_hits"
    );
    assert!(
        s.linking_blocks("Target").unwrap().is_empty(),
        "linking_blocks"
    );
    assert!(s.raw_links().unwrap().is_empty(), "raw_links");
    assert!(s.raw_doc_tags().unwrap().is_empty(), "raw_doc_tags");
    assert!(
        s.untagged_docs(10).unwrap().iter().all(|d| d.id != doc),
        "untagged_docs"
    );

    // restore brings it all back
    s.restore_doc(doc).unwrap();
    assert_eq!(s.search_blocks("gardeners", 10).unwrap().len(), 1);
    assert_eq!(s.backlinks(target).unwrap().len(), 1);
    assert_eq!(s.block_vecs().unwrap().len(), 1);
}

/// Item 5: create_doc_with_ops (import_markdown's path) is one transaction —
/// a failing op leaves no empty doc behind.
#[test]
fn create_doc_with_ops_is_atomic() {
    let (mut s, tom) = seed();
    let before = s.list_docs().unwrap().len();
    let bad_parent = Uuid::now_v7();
    let err = s.create_doc_with_ops(
        "ghost",
        None,
        tom.id,
        vec![
            insert(None, "", "ok"),
            insert(Some(bad_parent), "", "orphan"),
        ],
    );
    assert!(err.is_err(), "insert under a missing parent fails");
    assert_eq!(s.list_docs().unwrap().len(), before, "no doc created");

    let (doc, n) = s
        .create_doc_with_ops(
            "real",
            None,
            tom.id,
            vec![insert(None, "", "a"), insert(None, "", "b")],
        )
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        doc.current_epoch, 1,
        "one apply = one epoch, doc read back after commit"
    );
    let tree = s.read_doc(doc.id).unwrap();
    assert_eq!(tree.roots.len(), 2);
}

/// Item 5: redeem_invite's four writes commit as one and leave the
/// connection in autocommit (a stray open transaction would block every
/// later write with "cannot start a transaction within a transaction").
#[test]
fn redeem_invite_commits_and_leaves_no_open_transaction() {
    let (mut s, tom) = seed();
    let doc = s.create_doc("shared", None, tom.id).unwrap();
    let share = s
        .create_share(doc.id, None, SharePermission::View, None)
        .unwrap();
    s.create_invite(share.id, "h1", "2099-01-01T00:00:00.000Z")
        .unwrap();
    let (alice, bound) = s.redeem_invite("h1", "abcdef0123456789", "alice").unwrap();
    assert_eq!(bound.contact, Some(alice.id));
    // a second redeem of the burned secret fails cleanly...
    assert!(s.redeem_invite("h1", "abcdef0123456789", "alice").is_err());
    // ...and writes still work afterwards (nothing left open either way)
    s.create_doc("after", None, tom.id).unwrap();
    s.rename_doc(doc.id, "renamed").unwrap();
}

/// Item 6: change_stamp moves on every docs mutation, including the ones
/// the old aggregates were blind to (same-length rename, status, policy).
#[test]
fn change_stamp_sees_same_length_rename_status_and_policy() {
    let (mut s, tom) = seed();
    let doc = s.create_doc("aaa", None, tom.id).unwrap();
    let mut last = s.change_stamp().unwrap();
    let mut step = |s: &SqliteStore, what: &str| {
        let now = s.change_stamp().unwrap();
        assert_ne!(now, last, "{what} did not move the change stamp");
        last = now;
    };

    s.rename_doc(doc.id, "bbb").unwrap();
    step(&s, "same-length rename");
    s.set_doc_status(doc.id, Some(DocStatus::Draft)).unwrap();
    step(&s, "status");
    s.set_review_policy(doc.id, Some(ReviewPolicy::Auto))
        .unwrap();
    step(&s, "review policy");
    s.move_doc(doc.id, None, Some("m")).unwrap();
    step(&s, "move");
    s.delete_doc(doc.id).unwrap();
    step(&s, "delete");
    s.restore_doc(doc.id).unwrap();
    step(&s, "restore");
    // and it is stable when nothing happens
    assert_eq!(s.change_stamp().unwrap(), last);
}
