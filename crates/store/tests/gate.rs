//! Acceptance tests for #13/#14: propose gate verdicts, stale-op confidence,
//! yellow/red annotation lifecycle, proposer ≠ approver, resolved-red provenance.

use taisce_store::*;
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

/// The invariant bounds AGENTS. The human owner's own stale edit that the
/// gate parked red (an autosave racing a second window) must be
/// resolvable by that human — otherwise it is stuck in the queue forever.
#[test]
fn human_owner_can_resolve_their_own_parked_edit() {
    let mut f = fixture();
    tom_edits(&mut f); // epoch 2: para changed since epoch 1
    // tom's stale autosave against base 1 targets the changed block → red
    let out = f
        .s
        .propose(f.doc.id, 1, f.tom.id, vec![replace(f.para, "tom's stale edit")])
        .unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Red);
    let ann = f.s.review_queue(Some(f.doc.id)).unwrap()[0].annotation.id;
    // tom declines his own red: allowed, queue drains, doc untouched
    f.s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap();
    assert!(f.s.review_queue(Some(f.doc.id)).unwrap().is_empty());
    // and accepting his own works too
    f.s.propose(f.doc.id, 1, f.tom.id, vec![replace(f.para, "tom accepts this")])
        .unwrap();
    let ann = f.s.review_queue(Some(f.doc.id)).unwrap()[0].annotation.id;
    f.s.resolve(ann, f.tom.id, ReviewDecision::Accept).unwrap();
    assert_eq!(
        f.s.read_block(f.para).unwrap().content,
        "tom accepts this"
    );
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
fn auto_policy_self_applies_position_yellows_flags_reviewed_and_parks_reds() {
    let mut f = fixture();
    f.s.set_review_policy(f.doc.id, Some(ReviewPolicy::Auto))
        .unwrap();
    // position-only yellow: move-on-changed target. Exact id, no content
    // overwritten → HIGH_CONFIDENCE → applies WITHOUT an annotation under auto.
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
    assert_eq!(out.verdicts[0].verdict, Verdict::Yellow);
    assert!(out.verdicts[0].confidence >= gate::HIGH_CONFIDENCE);
    assert!(out.verdicts[0].applied);
    assert_eq!(f.s.read_block(other).unwrap().parent_id, Some(f.para));
    assert!(
        f.s.review_queue(None).unwrap().is_empty(),
        "position-only yellow self-applies under auto: no annotation"
    );

    // review-capped yellow (propose_reviewed) keeps its flag even under auto
    let out =
        f.s.propose_reviewed(f.doc.id, 4, f.agent.id, vec![replace(other, "tagged")])
            .unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Yellow);
    assert!(out.verdicts[0].applied);
    assert_eq!(
        f.s.review_queue(None)
            .unwrap()
            .iter()
            .filter(|i| i.annotation.kind == AnnotationKind::Review)
            .count(),
        1,
        "review requested by the proposer is honoured regardless of policy"
    );

    // red still parks under auto
    f.s.apply(f.doc.id, 5, f.tom.id, vec![replace(f.para, "tom again")])
        .unwrap();
    f.s.propose(
        f.doc.id,
        4,
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

#[test]
fn declining_a_reviewed_delete_resurrects_the_block_in_place() {
    let mut f = fixture();
    let before = f.s.read_block(f.para).unwrap();
    // a review-policy gardener deletes: green capped to yellow, applied
    let out =
        f.s.propose_reviewed(
            f.doc.id,
            1,
            f.agent.id,
            vec![OpInput {
                kind: OpKind::Delete { target: f.para },
                source_refs: vec![],
            }],
        )
        .unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Yellow);
    assert!(out.verdicts[0].applied);
    assert_eq!(out.epoch, 2);
    assert!(f.s.read_block(f.para).unwrap().deleted);

    let ann = f.s.review_queue(None).unwrap()[0].annotation.id;
    let receipt =
        f.s.resolve(ann, f.tom.id, ReviewDecision::Decline)
            .unwrap()
            .expect("decline of an applied yellow yields a revert receipt");
    assert_eq!(receipt.epoch, 3, "epoch bumped exactly once by the revert");

    let after = f.s.read_block(f.para).unwrap();
    assert!(!after.deleted, "block is live again");
    assert_eq!(after.id, before.id);
    assert_eq!(after.content, before.content);
    assert_eq!(after.parent_id, before.parent_id);
    assert_eq!(after.order_key, before.order_key);
    assert_eq!(after.block_type, before.block_type);
    assert_eq!(f.s.get_doc(f.doc.id).unwrap().current_epoch, 3);
    assert!(f.s.review_queue(None).unwrap().is_empty());
    // the doc tree shows it in its original position
    let tree = f.s.read_doc(f.doc.id).unwrap();
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(tree.roots[0].block.id, f.para);
}

#[test]
fn insert_over_a_live_id_still_fails() {
    let mut f = fixture();
    let out =
        f.s.propose(
            f.doc.id,
            1,
            f.agent.id,
            vec![OpInput {
                kind: OpKind::Insert {
                    block_id: f.para,
                    parent_id: None,
                    order_key: "j".into(),
                    block_type: BlockType::Paragraph,
                    content: "dup".into(),
                    refers_to: None,
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
    assert_eq!(
        out.verdicts[0].verdict,
        Verdict::Red,
        "projection failure parks red"
    );
    assert!(!out.verdicts[0].applied);
    assert_eq!(f.s.read_block(f.para).unwrap().content, "original");
}

#[test]
fn replace_retypes_content_blocks_but_not_comments() {
    let mut f = fixture();
    // paragraph → heading
    f.s.propose(f.doc.id, 1, f.agent.id, vec![replace(f.para, "## Heading")])
        .unwrap();
    assert_eq!(
        f.s.read_block(f.para).unwrap().block_type,
        BlockType::Heading
    );
    // heading → mermaid → bullet list (paragraph)
    f.s.propose(
        f.doc.id,
        2,
        f.agent.id,
        vec![replace(
            f.para,
            "```mermaid
graph TD
```",
        )],
    )
    .unwrap();
    assert_eq!(
        f.s.read_block(f.para).unwrap().block_type,
        BlockType::DiagramMermaid
    );
    f.s.propose(
        f.doc.id,
        3,
        f.agent.id,
        vec![replace(
            f.para, "- a
- b",
        )],
    )
    .unwrap();
    assert_eq!(
        f.s.read_block(f.para).unwrap().block_type,
        BlockType::Paragraph
    );
    // decision
    f.s.propose(
        f.doc.id,
        4,
        f.agent.id,
        vec![replace(f.para, "DECISION: go")],
    )
    .unwrap();
    assert_eq!(
        f.s.read_block(f.para).unwrap().block_type,
        BlockType::Decision
    );
    // comments keep their type whatever the content
    let c = f.s.add_comment(f.para, f.tom.id, "note", None).unwrap();
    let epoch = f.s.get_doc(f.doc.id).unwrap().current_epoch;
    f.s.propose(
        f.doc.id,
        epoch,
        f.agent.id,
        vec![replace(c.id, "## still a comment")],
    )
    .unwrap();
    assert_eq!(f.s.read_block(c.id).unwrap().block_type, BlockType::Comment);
}

// --- a decline never clobbers a later change to the same field ---------

fn review_item(s: &SqliteStore, doc: Uuid, op_id: Uuid) -> ReviewItem {
    s.review_queue(Some(doc))
        .unwrap()
        .into_iter()
        .find(|r| r.op.id == op_id)
        .expect("op still in the review queue")
}

fn doc_op(s: &mut SqliteStore, doc: Uuid, agent: Uuid, kind: OpKind) -> Uuid {
    let out = s.propose_doc_op(doc, agent, kind, vec![]).unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Yellow);
    out.verdicts[0].op_id
}

fn move_kind(new_parent: Option<Uuid>, sort_key: Option<String>) -> OpKind {
    OpKind::MoveDoc {
        new_parent,
        sort_key,
        new_parent_title: None,
        from_parent: None,
        from_sort_key: None,
        from_parent_title: None,
    }
}

fn rename_kind(title: &str) -> OpKind {
    OpKind::RenameDoc { title: title.into(), from_title: String::new() }
}

#[test]
fn declining_an_older_reorder_keeps_a_later_move_under_its_new_parent() {
    let f = fixture();
    let mut s = f.s;
    let a = s.create_doc("a", None, f.tom.id).unwrap();
    let doc = s.create_doc("Note", None, f.tom.id).unwrap();
    let roadmap = s.create_doc("Roadmap", None, f.tom.id).unwrap();
    let original_key = s.get_doc(doc.id).unwrap().sort_key;
    // 1. reorder among siblings: same (top-level) parent, before `a`
    let before_a = order_key::between(None, a.sort_key.as_deref());
    let reorder = doc_op(&mut s, doc.id, f.agent.id, move_kind(None, Some(before_a.clone())));
    assert_eq!(s.get_doc(doc.id).unwrap().sort_key.as_deref(), Some(before_a.as_str()));
    // 2. later: under Roadmap
    let mv = doc_op(&mut s, doc.id, f.agent.id, move_kind(Some(roadmap.id), None));
    assert_eq!(s.get_doc(doc.id).unwrap().parent_id, Some(roadmap.id));

    // decline the OLDER reorder: moot, the doc stays under Roadmap
    let ann = review_item(&s, doc.id, reorder).annotation.id;
    let receipt = s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap().unwrap();
    assert!(receipt.op_ids.is_empty(), "superseded: nothing reverted");
    assert_eq!(s.get_doc(doc.id).unwrap().parent_id, Some(roadmap.id), "the later move survives");
    // the reorder's item is closed; the move's item is still open and true
    assert!(s.review_queue(Some(doc.id)).unwrap().iter().all(|r| r.op.id != reorder));
    let item = review_item(&s, doc.id, mv);
    match &item.op.kind {
        OpKind::MoveDoc { new_parent, from_parent, from_sort_key, .. } => {
            assert_eq!(*new_parent, Some(roadmap.id));
            assert_eq!(*from_parent, None, "still says: moved from the top level");
            assert_eq!(*from_sort_key, original_key, "inherits the declined reorder's pre-image");
        }
        k => panic!("not a move: {k:?}"),
    }
    // declining the move too lands where the doc was before both
    s.resolve(item.annotation.id, f.tom.id, ReviewDecision::Decline).unwrap();
    let back = s.get_doc(doc.id).unwrap();
    assert_eq!(back.parent_id, None);
    assert_eq!(back.sort_key, original_key, "not the declined reorder's slot");
}

#[test]
fn declining_the_newer_move_first_then_the_reorder_still_reverts_both() {
    let f = fixture();
    let mut s = f.s;
    let a = s.create_doc("a", None, f.tom.id).unwrap();
    let doc = s.create_doc("Note", None, f.tom.id).unwrap();
    let roadmap = s.create_doc("Roadmap", None, f.tom.id).unwrap();
    let original_key = s.get_doc(doc.id).unwrap().sort_key;
    let before_a = order_key::between(None, a.sort_key.as_deref());
    let reorder = doc_op(&mut s, doc.id, f.agent.id, move_kind(None, Some(before_a.clone())));
    let mv = doc_op(&mut s, doc.id, f.agent.id, move_kind(Some(roadmap.id), None));
    let ann = review_item(&s, doc.id, mv).annotation.id;
    s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(s.get_doc(doc.id).unwrap().sort_key.as_deref(), Some(before_a.as_str()));
    // the move and its inverse cancel: the reorder is its own again
    let ann = review_item(&s, doc.id, reorder).annotation.id;
    let receipt = s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap().unwrap();
    assert_eq!(receipt.op_ids.len(), 1, "a real revert");
    let back = s.get_doc(doc.id).unwrap();
    assert_eq!((back.parent_id, back.sort_key), (None, original_key));
}

#[test]
fn declining_an_older_reorder_after_a_human_move_reverts_nothing() {
    let f = fixture();
    let mut s = f.s;
    let a = s.create_doc("a", None, f.tom.id).unwrap();
    let doc = s.create_doc("Note", None, f.tom.id).unwrap();
    let folder = s.create_doc("Folder", None, f.tom.id).unwrap();
    let before_a = order_key::between(None, a.sort_key.as_deref());
    let reorder = doc_op(&mut s, doc.id, f.agent.id, move_kind(None, Some(before_a)));
    // the app's drag: not ledgered, seen by value
    s.move_doc(doc.id, Some(folder.id), None).unwrap();
    let ann = review_item(&s, doc.id, reorder).annotation.id;
    let receipt = s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap().unwrap();
    assert!(receipt.op_ids.is_empty());
    assert_eq!(s.get_doc(doc.id).unwrap().parent_id, Some(folder.id));
}

#[test]
fn rename_then_rename_declining_the_older_keeps_the_newer_title() {
    let f = fixture();
    let mut s = f.s;
    let doc = s.create_doc("A", None, f.tom.id).unwrap();
    let r1 = doc_op(&mut s, doc.id, f.agent.id, rename_kind("B"));
    let r2 = doc_op(&mut s, doc.id, f.agent.id, rename_kind("C"));
    let ann = review_item(&s, doc.id, r1).annotation.id;
    let receipt = s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap().unwrap();
    assert!(receipt.op_ids.is_empty());
    assert_eq!(s.get_doc(doc.id).unwrap().title, "C");
    let item = review_item(&s, doc.id, r2);
    assert!(matches!(&item.op.kind, OpKind::RenameDoc { title, from_title } if title == "C" && from_title == "A"));
    // declining the newer one now goes back to A, never to the declined B
    s.resolve(item.annotation.id, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(s.get_doc(doc.id).unwrap().title, "A");
    assert!(s.review_queue(Some(doc.id)).unwrap().is_empty());
}

#[test]
fn rename_back_to_the_same_title_by_a_later_op_still_counts_as_later() {
    // A→B, B→C, C→B: the title equals the first op's post-image again, but
    // it is the third op's now — declining the first must not rename to A
    let f = fixture();
    let mut s = f.s;
    let doc = s.create_doc("A", None, f.tom.id).unwrap();
    let r1 = doc_op(&mut s, doc.id, f.agent.id, rename_kind("B"));
    doc_op(&mut s, doc.id, f.agent.id, rename_kind("C"));
    doc_op(&mut s, doc.id, f.agent.id, rename_kind("B"));
    let ann = review_item(&s, doc.id, r1).annotation.id;
    s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(s.get_doc(doc.id).unwrap().title, "B");
}

#[test]
fn status_then_status_declining_the_older_keeps_the_newer_status() {
    let f = fixture();
    let mut s = f.s;
    let doc = s.create_doc("D", None, f.tom.id).unwrap();
    let st = |status| OpKind::SetStatus { status: Some(status), from_status: None };
    let s1 = doc_op(&mut s, doc.id, f.agent.id, st(DocStatus::Draft));
    let s2 = doc_op(&mut s, doc.id, f.agent.id, st(DocStatus::Decided));
    let ann = review_item(&s, doc.id, s1).annotation.id;
    s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(s.get_doc(doc.id).unwrap().status, Some(DocStatus::Decided));
    let item = review_item(&s, doc.id, s2);
    assert!(matches!(&item.op.kind, OpKind::SetStatus { from_status: None, .. }));
    s.resolve(item.annotation.id, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(s.get_doc(doc.id).unwrap().status, None);
}

#[test]
fn replace_then_replace_declining_the_older_keeps_the_newer_content() {
    let mut f = fixture();
    let o1 = f.s.propose_reviewed(f.doc.id, 1, f.agent.id, vec![replace(f.para, "v2")]).unwrap();
    assert_eq!(o1.verdicts[0].verdict, Verdict::Yellow);
    let o2 = f.s.propose_reviewed(f.doc.id, 2, f.agent.id, vec![replace(f.para, "v3")]).unwrap();
    let (op1, op2) = (o1.verdicts[0].op_id, o2.verdicts[0].op_id);
    let ann = review_item(&f.s, f.doc.id, op1).annotation.id;
    let epoch = f.s.get_doc(f.doc.id).unwrap().current_epoch;
    let receipt = f.s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap().unwrap();
    assert!(receipt.op_ids.is_empty());
    assert_eq!(f.s.read_block(f.para).unwrap().content, "v3", "the later edit survives");
    assert_eq!(f.s.get_doc(f.doc.id).unwrap().current_epoch, epoch, "nothing applied");
    let item = review_item(&f.s, f.doc.id, op2);
    assert_eq!(item.op.prior.as_ref().unwrap().content, "original");
    f.s.resolve(item.annotation.id, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(f.s.read_block(f.para).unwrap().content, "original");
}

#[test]
fn replace_declined_after_a_human_edit_reverts_nothing() {
    let mut f = fixture();
    let o1 = f.s.propose_reviewed(f.doc.id, 1, f.agent.id, vec![replace(f.para, "agent")]).unwrap();
    f.s.apply(f.doc.id, 2, f.tom.id, vec![replace(f.para, "tom")]).unwrap();
    let ann = review_item(&f.s, f.doc.id, o1.verdicts[0].op_id).annotation.id;
    f.s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(f.s.read_block(f.para).unwrap().content, "tom");
}

#[test]
fn replace_declined_after_the_later_one_was_declined_reverts() {
    let mut f = fixture();
    let o1 = f.s.propose_reviewed(f.doc.id, 1, f.agent.id, vec![replace(f.para, "v2")]).unwrap();
    let o2 = f.s.propose_reviewed(f.doc.id, 2, f.agent.id, vec![replace(f.para, "v3")]).unwrap();
    let ann2 = review_item(&f.s, f.doc.id, o2.verdicts[0].op_id).annotation.id;
    f.s.resolve(ann2, f.tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(f.s.read_block(f.para).unwrap().content, "v2");
    let ann1 = review_item(&f.s, f.doc.id, o1.verdicts[0].op_id).annotation.id;
    let receipt = f.s.resolve(ann1, f.tom.id, ReviewDecision::Decline).unwrap().unwrap();
    assert_eq!(receipt.op_ids.len(), 1);
    assert_eq!(f.s.read_block(f.para).unwrap().content, "original");
}

#[test]
fn insert_edited_since_refuses_decline() {
    let mut f = fixture();
    let b = Uuid::now_v7();
    let o1 = f
        .s
        .propose_reviewed(
            f.doc.id,
            1,
            f.agent.id,
            vec![OpInput {
                kind: OpKind::Insert {
                    block_id: b,
                    parent_id: None,
                    order_key: "j".into(),
                    block_type: BlockType::Paragraph,
                    content: "new".into(),
                    refers_to: None,
                },
                source_refs: vec![],
            }],
        )
        .unwrap();
    f.s.apply(f.doc.id, 2, f.tom.id, vec![replace(b, "tom built on it")]).unwrap();
    let ann = review_item(&f.s, f.doc.id, o1.verdicts[0].op_id).annotation.id;
    let err = f.s.resolve(ann, f.tom.id, ReviewDecision::Decline).unwrap_err();
    assert!(err.to_string().contains("superseded"), "{err}");
    assert_eq!(f.s.read_block(b).unwrap().content, "tom built on it");
    assert!(f.s.review_queue(Some(f.doc.id)).unwrap().iter().any(|r| r.annotation.id == ann), "still open");
}
