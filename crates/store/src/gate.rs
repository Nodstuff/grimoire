//! The propose gate's confidence scoring (tickets 2.5/2.6).
//!
//! Ops target block IDs, so the fast path is exact, not fuzzy: a target
//! untouched since `base_epoch` greens with no matching at all. Verdicts for
//! stale ops at block granularity:
//!
//! - target unchanged since base → green (clean)
//! - overlapping edit (target changed since base) → red for replace/delete
//!   (the destroyers), yellow for move (position-only conflict)
//! - target gone → red; the op's payload preserves the text verbatim
//! - deletes biased aggressively to red: a wrong insert annoys, a wrong
//!   delete destroys (PROJECT.md §2)

use crate::types::{Block, OpKind, Verdict};
use uuid::Uuid;

/// The one hardcoded threshold (§3.1): yellows at or above this are
/// auto-appliable under the `auto` review policy (ticket 2.10).
pub const HIGH_CONFIDENCE: f64 = 0.8;

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub verdict: Verdict,
    pub confidence: f64,
    pub note: String,
}

fn green(confidence: f64, note: &str) -> Scored {
    Scored {
        verdict: Verdict::Green,
        confidence,
        note: note.into(),
    }
}
fn yellow(confidence: f64, note: String) -> Scored {
    Scored {
        verdict: Verdict::Yellow,
        confidence,
        note,
    }
}
fn red(confidence: f64, note: String) -> Scored {
    Scored {
        verdict: Verdict::Red,
        confidence,
        note,
    }
}

enum TargetState {
    Unchanged,
    Changed(Block),
    Gone,
}

fn target_state(
    lookup: &mut impl FnMut(Uuid) -> Option<Block>,
    id: Uuid,
    base: i64,
) -> TargetState {
    match lookup(id) {
        None => TargetState::Gone,
        Some(b) if b.deleted => TargetState::Gone,
        Some(b) if b.epoch <= base => TargetState::Unchanged,
        Some(b) => TargetState::Changed(b),
    }
}

/// Score one stale op against the doc's current state.
/// `lookup` returns the block by id (including tombstoned ones).
pub fn score_stale_op(
    op: &OpKind,
    base_epoch: i64,
    lookup: &mut impl FnMut(Uuid) -> Option<Block>,
) -> Scored {
    match op {
        OpKind::Insert { parent_id, .. } => match parent_id {
            None => green(0.95, "insert at root: no anchor to invalidate"),
            Some(p) => match target_state(lookup, *p, base_epoch) {
                TargetState::Unchanged => green(0.9, "parent anchor unchanged since base"),
                TargetState::Changed(b) => yellow(
                    0.6,
                    format!(
                        "parent anchor edited at epoch {} since base {base_epoch}",
                        b.epoch
                    ),
                ),
                TargetState::Gone => red(0.1, format!("parent anchor {p} is gone")),
            },
        },
        OpKind::Replace { target, .. } => match target_state(lookup, *target, base_epoch) {
            TargetState::Unchanged => green(1.0, "target unchanged since base"),
            TargetState::Changed(b) => red(
                0.2,
                format!(
                    "overlapping edit: target changed at epoch {} since base {base_epoch}; \
                     proposed text preserved in op payload",
                    b.epoch
                ),
            ),
            TargetState::Gone => red(
                0.05,
                format!("target {target} is gone; proposed text preserved in op payload"),
            ),
        },
        OpKind::Delete { target } => match target_state(lookup, *target, base_epoch) {
            TargetState::Unchanged => green(0.9, "target unchanged since base"),
            TargetState::Changed(b) => red(
                0.1,
                format!(
                    "delete against a block edited at epoch {} since base {base_epoch}: \
                     a wrong delete destroys, biased red",
                    b.epoch
                ),
            ),
            TargetState::Gone => red(0.1, format!("target {target} already gone")),
        },
        OpKind::Move {
            target, new_parent, ..
        } => {
            let t = target_state(lookup, *target, base_epoch);
            let anchor_state = new_parent.map(|p| target_state(lookup, p, base_epoch));
            match (t, anchor_state) {
                (TargetState::Gone, _) => red(0.1, format!("move target {target} is gone")),
                (_, Some(TargetState::Gone)) => red(0.15, "move destination parent is gone".into()),
                (TargetState::Changed(b), _) => yellow(
                    0.6,
                    format!(
                        "position-only conflict: target edited at epoch {} since base {base_epoch}",
                        b.epoch
                    ),
                ),
                (_, Some(TargetState::Changed(b))) => yellow(
                    0.55,
                    format!(
                        "destination parent edited at epoch {} since base {base_epoch}",
                        b.epoch
                    ),
                ),
                (TargetState::Unchanged, _) => {
                    green(0.9, "target and destination unchanged since base")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BlockType;

    fn block(id: Uuid, epoch: i64, deleted: bool) -> Block {
        Block {
            id,
            doc_id: Uuid::now_v7(),
            parent_id: None,
            order_key: "i".into(),
            block_type: BlockType::Paragraph,
            content: "x".into(),
            created_by: Uuid::now_v7(),
            epoch,
            deleted,
        }
    }

    #[test]
    fn replace_verdict_matrix() {
        let t = Uuid::now_v7();
        let op = OpKind::Replace {
            target: t,
            content: "new".into(),
        };
        // unchanged → green
        let s = score_stale_op(&op, 5, &mut |id| (id == t).then(|| block(t, 3, false)));
        assert_eq!(s.verdict, Verdict::Green);
        // changed → red (overlap)
        let s = score_stale_op(&op, 5, &mut |id| (id == t).then(|| block(t, 7, false)));
        assert_eq!(s.verdict, Verdict::Red);
        // gone → red
        let s = score_stale_op(&op, 5, &mut |_| None);
        assert_eq!(s.verdict, Verdict::Red);
        // tombstoned counts as gone
        let s = score_stale_op(&op, 5, &mut |id| (id == t).then(|| block(t, 3, true)));
        assert_eq!(s.verdict, Verdict::Red);
    }

    #[test]
    fn deletes_never_yellow() {
        let t = Uuid::now_v7();
        let op = OpKind::Delete { target: t };
        for (epoch, deleted) in [(3, false), (7, false), (7, true)] {
            let s = score_stale_op(&op, 5, &mut |id| {
                (id == t).then(|| block(t, epoch, deleted))
            });
            assert_ne!(s.verdict, Verdict::Yellow);
        }
    }

    #[test]
    fn move_on_changed_target_is_yellow() {
        let t = Uuid::now_v7();
        let op = OpKind::Move {
            target: t,
            new_parent: None,
            new_order_key: "j".into(),
        };
        let s = score_stale_op(&op, 5, &mut |id| (id == t).then(|| block(t, 7, false)));
        assert_eq!(s.verdict, Verdict::Yellow);
        assert!(s.confidence < HIGH_CONFIDENCE);
    }

    #[test]
    fn root_insert_is_green() {
        let op = OpKind::Insert {
            block_id: Uuid::now_v7(),
            parent_id: None,
            order_key: "i".into(),
            block_type: BlockType::Paragraph,
            content: "x".into(),
        };
        let s = score_stale_op(&op, 5, &mut |_| None);
        assert_eq!(s.verdict, Verdict::Green);
    }
}
