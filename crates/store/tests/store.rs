//! Acceptance tests for #9/#10: CRUD a doc tree; round-trips; every mutation
//! lands as op + projection in one transaction; batch bumps epoch exactly once;
//! zero integer IDs anywhere.

use ks_store::order_key::between;
use ks_store::*;
use uuid::Uuid;

fn store_with_tom() -> (SqliteStore, Principal) {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let tom = s
        .create_principal(PrincipalKind::Human, "Tom", None)
        .unwrap();
    (s, tom)
}

fn insert(parent: Option<Uuid>, key: &str, ty: BlockType, content: &str) -> (Uuid, OpInput) {
    let id = Uuid::now_v7();
    let op = OpInput {
        kind: OpKind::Insert {
            block_id: id,
            parent_id: parent,
            order_key: key.into(),
            block_type: ty,
            content: content.into(),
            refers_to: None,
        },
        source_refs: vec![],
    };
    (id, op)
}

#[test]
fn doc_tree_round_trips() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("runbook", None, tom.id).unwrap();
    assert_eq!(doc.current_epoch, 0);

    let k1 = between(None, None);
    let k2 = between(Some(&k1), None);
    let (h, op_h) = insert(None, &k1, BlockType::Heading, "Deploy");
    let (p1, op_p1) = insert(Some(h), &k1, BlockType::Paragraph, "step one");
    let (p2, op_p2) = insert(Some(h), &k2, BlockType::Paragraph, "step two");
    let (tail, op_tail) = insert(None, &k2, BlockType::Paragraph, "footer");

    let r = s
        .apply(doc.id, 0, tom.id, vec![op_h, op_p1, op_p2, op_tail])
        .unwrap();
    assert_eq!(r.epoch, 1, "batch of 4 ops bumps epoch exactly once");
    assert_eq!(r.op_ids.len(), 4);

    let tree = s.read_doc(doc.id).unwrap();
    assert_eq!(tree.doc.current_epoch, 1);
    assert_eq!(tree.roots.len(), 2);
    assert_eq!(tree.roots[0].block.id, h);
    assert_eq!(tree.roots[0].children.len(), 2);
    assert_eq!(
        (
            tree.roots[0].children[0].block.id,
            tree.roots[0].children[1].block.id
        ),
        (p1, p2),
        "children ordered by order_key"
    );
    assert_eq!(tree.roots[1].block.id, tail);
}

#[test]
fn replace_delete_move_project_correctly() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let k1 = between(None, None);
    let k2 = between(Some(&k1), None);
    let (a, op_a) = insert(None, &k1, BlockType::Paragraph, "a");
    let (b, op_b) = insert(None, &k2, BlockType::Paragraph, "b");
    s.apply(doc.id, 0, tom.id, vec![op_a, op_b]).unwrap();

    // replace
    s.apply(
        doc.id,
        1,
        tom.id,
        vec![OpInput {
            kind: OpKind::Replace {
                target: a,
                content: "a2".into(),
            },
            source_refs: vec!["test:replace".into()],
        }],
    )
    .unwrap();
    assert_eq!(s.read_block(a).unwrap().content, "a2");
    assert_eq!(
        s.read_block(a).unwrap().epoch,
        2,
        "block carries epoch of last modification"
    );

    // move b under a
    s.apply(
        doc.id,
        2,
        tom.id,
        vec![OpInput {
            kind: OpKind::Move {
                target: b,
                new_parent: Some(a),
                new_order_key: between(None, None),
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    let tree = s.read_doc(doc.id).unwrap();
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(tree.roots[0].children[0].block.id, b);

    // delete b: tombstoned, not gone
    s.apply(
        doc.id,
        3,
        tom.id,
        vec![OpInput {
            kind: OpKind::Delete { target: b },
            source_refs: vec![],
        }],
    )
    .unwrap();
    assert!(s.read_block(b).unwrap().deleted, "delete is a tombstone");
    assert!(s.read_doc(doc.id).unwrap().roots[0].children.is_empty());

    // deleted block rejects further edits
    let err = s.apply(
        doc.id,
        4,
        tom.id,
        vec![OpInput {
            kind: OpKind::Replace {
                target: b,
                content: "zombie".into(),
            },
            source_refs: vec![],
        }],
    );
    assert!(matches!(err, Err(StoreError::NotFound(_))));
}

#[test]
fn stale_base_epoch_is_rejected() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let (_, op1) = insert(None, "i", BlockType::Paragraph, "x");
    s.apply(doc.id, 0, tom.id, vec![op1]).unwrap();

    let (_, op2) = insert(None, "j", BlockType::Paragraph, "y");
    let err = s.apply(doc.id, 0, tom.id, vec![op2]);
    match err {
        Err(StoreError::StaleBase {
            base: 0,
            current: 1,
        }) => {}
        other => panic!("expected StaleBase, got {other:?}"),
    }
}

#[test]
fn failed_op_rolls_back_whole_transaction() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let (a, op_a) = insert(None, "i", BlockType::Paragraph, "a");
    // second op targets a nonexistent parent → whole apply must roll back
    let (_, bad) = insert(Some(Uuid::now_v7()), "j", BlockType::Paragraph, "orphan");
    assert!(s.apply(doc.id, 0, tom.id, vec![op_a, bad]).is_err());

    assert_eq!(
        s.get_doc(doc.id).unwrap().current_epoch,
        0,
        "epoch not bumped"
    );
    assert!(
        matches!(s.read_block(a), Err(StoreError::NotFound(_))),
        "first op rolled back too"
    );
    assert!(
        s.ops_since(doc.id, 0).unwrap().is_empty(),
        "no ledger rows survive"
    );
}

#[test]
fn ledger_records_every_mutation_and_ops_since_reads_it() {
    let (mut s, tom) = store_with_tom();
    let agent = s
        .create_principal(PrincipalKind::Agent, "gardener", None)
        .unwrap();
    let doc = s.create_doc("d", None, tom.id).unwrap();

    let (a, op_a) = insert(None, "i", BlockType::Paragraph, "v1");
    s.apply(doc.id, 0, tom.id, vec![op_a]).unwrap();
    s.apply(
        doc.id,
        1,
        agent.id,
        vec![OpInput {
            kind: OpKind::Replace {
                target: a,
                content: "v2".into(),
            },
            source_refs: vec!["github:pr/341".into()],
        }],
    )
    .unwrap();

    let all = s.ops_since(doc.id, 0).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].principal, tom.id);
    assert_eq!(all[1].principal, agent.id);
    assert_eq!(all[1].source_refs, vec!["github:pr/341"]);
    assert_eq!(all[1].verdict, Some(Verdict::Green));

    // diff_since semantics: only what the stale reader missed
    let missed = s.ops_since(doc.id, 1).unwrap();
    assert_eq!(missed.len(), 1);
    assert_eq!(missed[0].epoch_applied, Some(2));
    assert!(matches!(missed[0].kind, OpKind::Replace { target, .. } if target == a));
}

#[test]
fn move_cycle_is_rejected() {
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let (a, op_a) = insert(None, "i", BlockType::Paragraph, "a");
    let (b, op_b) = insert(Some(a), "i", BlockType::Paragraph, "b");
    s.apply(doc.id, 0, tom.id, vec![op_a, op_b]).unwrap();

    let err = s.apply(
        doc.id,
        1,
        tom.id,
        vec![OpInput {
            kind: OpKind::Move {
                target: a,
                new_parent: Some(b),
                new_order_key: "i".into(),
            },
            source_refs: vec![],
        }],
    );
    assert!(matches!(err, Err(StoreError::InvalidOp(_))));
}

#[test]
fn zero_integer_ids_anywhere() {
    // every entity id round-trips as a UUID; epochs are the only integers by design
    let (mut s, tom) = store_with_tom();
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let (a, op_a) = insert(None, "i", BlockType::Paragraph, "a");
    let r = s.apply(doc.id, 0, tom.id, vec![op_a]).unwrap();
    for id in [tom.id, doc.id, a, r.op_ids[0]] {
        assert_eq!(
            id.get_version(),
            Some(uuid::Version::SortRand),
            "uuidv7: {id}"
        );
    }
}

#[test]
fn persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ks.db");
    let doc_id;
    {
        let mut s = SqliteStore::open(&path).unwrap();
        let tom = s
            .create_principal(PrincipalKind::Human, "Tom", None)
            .unwrap();
        let doc = s.create_doc("survives", None, tom.id).unwrap();
        let (_, op) = insert(None, "i", BlockType::Paragraph, "hello");
        s.apply(doc.id, 0, tom.id, vec![op]).unwrap();
        doc_id = doc.id;
    }
    let s = SqliteStore::open(&path).unwrap();
    let tree = s.read_doc(doc_id).unwrap();
    assert_eq!(tree.doc.title, "survives");
    assert_eq!(tree.roots[0].block.content, "hello");
    assert_eq!(tree.doc.current_epoch, 1);
}
