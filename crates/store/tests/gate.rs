//! Acceptance tests for #13/#14: propose gate verdicts, stale-op confidence,
//! yellow/red annotation lifecycle, proposer ≠ approver, resolved-red provenance.

use ks_store::*;
use uuid::Uuid;

struct Fixture {
    s: SqliteStore,
    tom: Principal,
    agent: Principal,
    doc: Doc,
    para: Uuid,
}

/// Doc with one paragraph at epoch 1; agent proposals then run against it.
fn fixture() -> Fixture {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let tom = s
        .create_principal(PrincipalKind::Human, "Tom", None)
        .unwrap();
    let agent = s
        .create_principal(PrincipalKind::Agent, "gardener", None)
        .unwrap();
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let para = Uuid::now_v7();
    s.apply(
        doc.id,
        0,
        tom.id,
        vec![OpInput {
            kind: OpKind::Insert {
                block_id: para,
                parent_id: None,
                order_key: "i".into(),
                block_type: BlockType::Paragraph,
                content: "original".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    Fixture {
        s,
        tom,
        agent,
        doc,
        para,
    }
}

fn replace(target: Uuid, content: &str) -> OpInput {
    OpInput {
        kind: OpKind::Replace {
            target,
            content: content.into(),
        },
        source_refs: vec![],
    }
}

/// Tom edits the paragraph, moving the doc to epoch 2 (staleness generator).
fn tom_edits(f: &mut Fixture) {
    f.s.apply(f.doc.id, 1, f.tom.id, vec![replace(f.para, "tom's edit")])
        .unwrap();
}

#[test]
fn current_base_propose_greens_and_applies() {
    let mut f = fixture();
    let out =
        f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent edit")])
            .unwrap();
    assert_eq!(out.epoch, 2);
    assert_eq!(out.verdicts[0].verdict, Verdict::Green);
    assert!(out.verdicts[0].applied);
    assert_eq!(f.s.read_block(f.para).unwrap().content, "agent edit");
    assert!(f.s.review_queue(None).unwrap().is_empty());
}

#[test]
fn stale_replace_on_unchanged_block_greens() {
    let mut f = fixture();
    // Tom adds an unrelated block → epoch 2; agent proposes at base 1 against untouched para
    let other = Uuid::now_v7();
    f.s.apply(
        f.doc.id,
        1,
        f.tom.id,
        vec![OpInput {
            kind: OpKind::Insert {
                block_id: other,
                parent_id: None,
                order_key: "j".into(),
                block_type: BlockType::Paragraph,
                content: "unrelated".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }],
    )
    .unwrap();

    let out =
        f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent edit")])
            .unwrap();
    assert_eq!(
        out.verdicts[0].verdict,
        Verdict::Green,
        "exact fast path: no fuzzy work"
    );
    assert_eq!(f.s.read_block(f.para).unwrap().content, "agent edit");
}

#[test]
fn overlapping_replace_parks_red_with_payload_preserved() {
    let mut f = fixture();
    tom_edits(&mut f);

    let out =
        f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent edit")])
            .unwrap();
    let v = &out.verdicts[0];
    assert_eq!(v.verdict, Verdict::Red);
    assert!(!v.applied);
    assert_eq!(out.epoch, 2, "nothing applied → no epoch bump");
    assert_eq!(
        f.s.read_block(f.para).unwrap().content,
        "tom's edit",
        "content untouched"
    );

    let queue = f.s.review_queue(Some(f.doc.id)).unwrap();
    assert_eq!(queue.len(), 1);
    let item = &queue[0];
    assert_eq!(item.annotation.kind, AnnotationKind::Parked);
    assert!(
        matches!(&item.op.kind, OpKind::Replace { content, .. } if content == "agent edit"),
        "proposed text preserved verbatim in the parked op"
    );
    assert_eq!(
        item.op.prior.as_ref().unwrap().content,
        "tom's edit",
        "pre-image recorded"
    );
}

#[test]
fn stale_delete_is_biased_red_even_when_annoying() {
    let mut f = fixture();
    tom_edits(&mut f);
    let out =
        f.s.propose(
            f.doc.id,
            1,
            f.agent.id,
            vec![OpInput {
                kind: OpKind::Delete { target: f.para },
                source_refs: vec![],
            }],
        )
        .unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Red);
    assert!(!f.s.read_block(f.para).unwrap().deleted);
}

#[test]
fn stale_move_on_changed_target_yellows_and_applies() {
    let mut f = fixture();
    // second root block to move
    let other = Uuid::now_v7();
    f.s.apply(
        f.doc.id,
        1,
        f.tom.id,
        vec![OpInput {
            kind: OpKind::Insert {
                block_id: other,
                parent_id: None,
                order_key: "j".into(),
                block_type: BlockType::Paragraph,
                content: "movable".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    // Tom edits the movable block → epoch 3
    f.s.apply(f.doc.id, 2, f.tom.id, vec![replace(other, "movable v2")])
        .unwrap();

    // agent, stale at base 2, moves the (now changed) block under para
    let out =
        f.s.propose(
            f.doc.id,
            2,
            f.agent.id,
            vec![OpInput {
                kind: OpKind::Move {
                    target: other,
                    new_parent: Some(f.para),
                    new_order_key: "i".into(),
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
    let v = &out.verdicts[0];
    assert_eq!(v.verdict, Verdict::Yellow);
    assert!(v.applied, "yellow = applied, flagged for review");
    assert_eq!(out.epoch, 4);
    assert_eq!(f.s.read_block(other).unwrap().parent_id, Some(f.para));

    let queue = f.s.review_queue(Some(f.doc.id)).unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].annotation.kind, AnnotationKind::Review);
}

#[test]
fn proposer_cannot_resolve_own_proposal() {
    let mut f = fixture();
    tom_edits(&mut f);
    f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent edit")])
        .unwrap();
    let ann = f.s.review_queue(None).unwrap()[0].annotation.id;

    let err = f.s.resolve(ann, f.agent.id, ReviewDecision::Accept);
    assert!(
        matches!(err, Err(StoreError::InvalidOp(_))),
        "proposer ≠ approver"
    );
    assert_eq!(f.s.review_queue(None).unwrap().len(), 1, "still open");
}

#[test]
fn accept_yellow_clears_annotation_without_editing() {
    let mut f = fixture();
    // yellow via move-on-changed (fixture as in the yellow test, compressed)
    let other = Uuid::now_v7();
    f.s.apply(
        f.doc.id,
        1,
        f.tom.id,
        vec![OpInput {
            kind: OpKind::Insert {
                block_id: other,
                parent_id: None,
                order_key: "j".into(),
                block_type: BlockType::Paragraph,
                content: "movable".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    f.s.apply(f.doc.id, 2, f.tom.id, vec![replace(other, "movable v2")])
        .unwrap();
    f.s.propose(
        f.doc.id,
        2,
        f.agent.id,
        vec![OpInput {
            kind: OpKind::Move {
                target: other,
                new_parent: Some(f.para),
                new_order_key: "i".into(),
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    let ann = f.s.review_queue(None).unwrap()[0].annotation.id;
    let epoch_before = f.s.get_doc(f.doc.id).unwrap().current_epoch;

    let receipt = f.s.resolve(ann, f.tom.id, ReviewDecision::Accept).unwrap();
    assert!(
        receipt.is_none(),
        "accepting a yellow is clearing an annotation, not an edit"
    );
    assert_eq!(f.s.get_doc(f.doc.id).unwrap().current_epoch, epoch_before);
    assert!(f.s.review_queue(None).unwrap().is_empty());
}

#[test]
fn decline_yellow_reverts_via_pre_image() {
    let mut f = fixture();
    let other = Uuid::now_v7();
    f.s.apply(
        f.doc.id,
        1,
        f.tom.id,
        vec![OpInput {
            kind: OpKind::Insert {
                block_id: other,
                parent_id: None,
                order_key: "j".into(),
                block_type: BlockType::Paragraph,
                content: "movable".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    f.s.apply(f.doc.id, 2, f.tom.id, vec![replace(other, "movable v2")])
        .unwrap();
    f.s.propose(
        f.doc.id,
        2,
        f.agent.id,
        vec![OpInput {
            kind: OpKind::Move {
                target: other,
                new_parent: Some(f.para),
                new_order_key: "i".into(),
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    assert_eq!(f.s.read_block(other).unwrap().parent_id, Some(f.para));
    let ann = f.s.review_queue(None).unwrap()[0].annotation.id;

    let receipt =
        f.s.resolve(ann, f.tom.id, ReviewDecision::Decline)
            .unwrap()
            .unwrap();
    let b = f.s.read_block(other).unwrap();
    assert_eq!(b.parent_id, None, "moved back to root");
    assert_eq!(b.order_key, "j", "original position restored");
    // the revert is itself a ledger op by the reviewer
    let last = f.s.ops_since(f.doc.id, receipt.epoch - 1).unwrap();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].principal, f.tom.id);
    assert!(last[0].source_refs[0].starts_with("review:decline:"));
}

#[test]
fn accept_red_applies_parked_op_with_red_provenance() {
    let mut f = fixture();
    tom_edits(&mut f);
    f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent edit")])
        .unwrap();
    let ann = f.s.review_queue(None).unwrap()[0].annotation.id;

    let receipt =
        f.s.resolve(ann, f.tom.id, ReviewDecision::Accept)
            .unwrap()
            .unwrap();
    assert_eq!(f.s.read_block(f.para).unwrap().content, "agent edit");
    // the op keeps verdict red with epoch_applied set: distinct provenance for resolved reds
    let applied = f.s.ops_since(f.doc.id, receipt.epoch - 1).unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].verdict, Some(Verdict::Red));
    assert_eq!(
        applied[0].principal, f.agent.id,
        "authorship stays with the proposer"
    );
    assert!(f.s.review_queue(None).unwrap().is_empty());
}

#[test]
fn decline_red_leaves_doc_untouched() {
    let mut f = fixture();
    tom_edits(&mut f);
    f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent edit")])
        .unwrap();
    let ann = f.s.review_queue(None).unwrap()[0].annotation.id;
    let epoch_before = f.s.get_doc(f.doc.id).unwrap().current_epoch;

    let receipt = f.s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap();
    assert!(receipt.is_none());
    assert_eq!(f.s.read_block(f.para).unwrap().content, "tom's edit");
    assert_eq!(f.s.get_doc(f.doc.id).unwrap().current_epoch, epoch_before);
    assert!(f.s.review_queue(None).unwrap().is_empty());

    // double resolution rejected
    let err = f.s.resolve(ann, f.tom.id, ReviewDecision::Accept);
    assert!(matches!(err, Err(StoreError::InvalidOp(_))));
}

#[test]
fn mixed_batch_bumps_epoch_once_and_scores_per_op() {
    let mut f = fixture();
    tom_edits(&mut f); // doc at epoch 2, para changed at 2

    let fresh = Uuid::now_v7();
    let out =
        f.s.propose(
            f.doc.id,
            1,
            f.agent.id,
            vec![
                // red: overlaps tom's edit
                replace(f.para, "agent edit"),
                // green: root insert
                OpInput {
                    kind: OpKind::Insert {
                        block_id: fresh,
                        parent_id: None,
                        order_key: "j".into(),
                        block_type: BlockType::Paragraph,
                        content: "new section".into(),
                        refers_to: None,
                    },
                    source_refs: vec![],
                },
            ],
        )
        .unwrap();
    assert_eq!(out.epoch, 3, "one bump for the batch");
    assert_eq!(
        out.verdicts.iter().map(|v| v.verdict).collect::<Vec<_>>(),
        vec![Verdict::Red, Verdict::Green]
    );
    assert!(f.s.read_block(fresh).is_ok());
    assert_eq!(f.s.read_block(f.para).unwrap().content, "tom's edit");
}

#[test]
fn base_epoch_ahead_of_doc_is_rejected() {
    let mut f = fixture();
    let err =
        f.s.propose(f.doc.id, 99, f.agent.id, vec![replace(f.para, "x")]);
    assert!(matches!(err, Err(StoreError::InvalidOp(_))));
}

#[test]
fn review_policy_inherits_from_parent_and_defaults_human() {
    let mut f = fixture();
    assert_eq!(
        f.s.effective_policy(f.doc.id).unwrap(),
        DEFAULT_REVIEW_POLICY
    );
    let child = f.s.create_doc("child", Some(f.doc.id), f.tom.id).unwrap();
    f.s.set_review_policy(f.doc.id, Some(ReviewPolicy::Auto))
        .unwrap();
    assert_eq!(
        f.s.effective_policy(child.id).unwrap(),
        ReviewPolicy::Auto,
        "null inherits parent"
    );
    f.s.set_review_policy(child.id, Some(ReviewPolicy::HumanReview))
        .unwrap();
    assert_eq!(
        f.s.effective_policy(child.id).unwrap(),
        ReviewPolicy::HumanReview,
        "own column wins"
    );
}

#[test]
fn auto_policy_still_flags_low_confidence_yellows_and_parks_reds() {
    let mut f = fixture();
    f.s.set_review_policy(f.doc.id, Some(ReviewPolicy::Auto))
        .unwrap();
    // low-confidence yellow: move-on-changed (0.6 < HIGH_CONFIDENCE)
    let other = Uuid::now_v7();
    f.s.apply(
        f.doc.id,
        1,
        f.tom.id,
        vec![OpInput {
            kind: OpKind::Insert {
                block_id: other,
                parent_id: None,
                order_key: "j".into(),
                block_type: BlockType::Paragraph,
                content: "movable".into(),
                refers_to: None,
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    f.s.apply(f.doc.id, 2, f.tom.id, vec![replace(other, "movable v2")])
        .unwrap();
    f.s.propose(
        f.doc.id,
        2,
        f.agent.id,
        vec![OpInput {
            kind: OpKind::Move {
                target: other,
                new_parent: Some(f.para),
                new_order_key: "i".into(),
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    assert_eq!(
        f.s.review_queue(None).unwrap().len(),
        1,
        "low-confidence yellow flagged even under auto"
    );
    // red still parks under auto
    f.s.apply(f.doc.id, 4, f.tom.id, vec![replace(f.para, "tom again")])
        .unwrap();
    f.s.propose(f.doc.id, 4, f.tom.id, vec![]).err(); // no-op guard
    f.s.propose(
        f.doc.id,
        3,
        f.agent.id,
        vec![replace(f.para, "stale agent")],
    )
    .unwrap();
    assert_eq!(
        f.s.review_queue(None)
            .unwrap()
            .iter()
            .filter(|i| i.annotation.kind == AnnotationKind::Parked)
            .count(),
        1,
        "reds always park regardless of policy"
    );
}
