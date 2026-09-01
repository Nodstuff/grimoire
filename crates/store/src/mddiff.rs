//! Markdown → minimal block ops (the agent-facing twin of the UI editor's
//! diff engine). An agent hands over a doc's full new markdown; this compares
//! it against the current blocks and emits insert/replace/delete/move ops —
//! unchanged blocks keep their ids, so provenance and comments survive.
//!
//! Structure rules mirror the importer: heading levels derive nesting;
//! sibling order uses fractional keys, with a greedy increasing run keeping
//! untouched siblings' keys stable. Matching is LCS on exact content.

use crate::import::{Segment, segment};
use crate::{Block, BlockNode, BlockType, OpInput, OpKind, order_key};
use std::collections::HashMap;
use uuid::Uuid;

struct Existing {
    id: Uuid,
    content: String,
    parent: Option<Uuid>,
    order_key: String,
}

/// Flatten the current tree in order — comments and canvases are not content
/// flow and are never touched by a markdown diff.
fn flatten(nodes: &[BlockNode], out: &mut Vec<Existing>) {
    for n in nodes {
        if n.block.block_type != BlockType::Comment && n.block.block_type != BlockType::CanvasScene
        {
            out.push(Existing {
                id: n.block.id,
                content: n.block.content.clone(),
                parent: n.block.parent_id,
                order_key: n.block.order_key.clone(),
            });
            flatten(&n.children, out);
        }
    }
}

/// LCS over exact content equality: which new segments are which old blocks.
fn lcs_match(old: &[Existing], new: &[Segment]) -> Vec<Option<usize>> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i].content == new[j].content {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut assign = vec![None; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i].content == new[j].content {
            assign[j] = Some(i);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    assign
}

/// Compute the minimal op set turning the doc's current content into `markdown`.
pub fn markdown_to_ops(roots: &[BlockNode], markdown: &str) -> Vec<OpInput> {
    let mut old = Vec::new();
    flatten(roots, &mut old);
    let new = segment(markdown);
    let matched = lcs_match(&old, &new);

    let mut ops: Vec<OpInput> = Vec::new();
    let src = || vec!["propose_markdown".to_string()];

    // 1. deletes: old blocks not matched by any new segment
    let matched_old: std::collections::HashSet<usize> = matched.iter().flatten().copied().collect();
    // pair leftovers in order as replacements instead of delete+insert where
    // both sides have an unmatched item (keeps ids + comment anchors alive)
    let unmatched_old: Vec<usize> = (0..old.len())
        .filter(|i| !matched_old.contains(i))
        .collect();
    let unmatched_new: Vec<usize> = (0..new.len()).filter(|j| matched[*j].is_none()).collect();
    let mut matched = matched;
    let pairs = unmatched_old.len().min(unmatched_new.len());
    let mut replaced: Vec<(usize, usize)> = Vec::new();
    for k in 0..pairs {
        matched[unmatched_new[k]] = Some(unmatched_old[k]);
        replaced.push((unmatched_new[k], unmatched_old[k]));
    }
    for &i in unmatched_old.iter().skip(pairs) {
        ops.push(OpInput {
            kind: OpKind::Delete { target: old[i].id },
            source_refs: src(),
        });
    }
    let replaced: std::collections::HashSet<(usize, usize)> = replaced.into_iter().collect();

    // 2. heading-stack parents over the new sequence
    struct Placed {
        new_idx: usize,
        parent: Option<Uuid>,
        temp_id: Uuid,
        old_idx: Option<usize>,
    }
    let mut placed: Vec<Placed> = Vec::new();
    let mut stack: Vec<(u8, Uuid)> = Vec::new();
    for (j, seg) in new.iter().enumerate() {
        if seg.level > 0 {
            while stack.last().is_some_and(|(l, _)| *l >= seg.level) {
                stack.pop();
            }
        }
        let parent = stack.last().map(|(_, id)| *id);
        let temp_id = matched[j].map(|i| old[i].id).unwrap_or_else(Uuid::now_v7);
        placed.push(Placed {
            new_idx: j,
            parent,
            temp_id,
            old_idx: matched[j],
        });
        if seg.level > 0 {
            stack.push((seg.level, temp_id));
        }
    }

    // 3. group by parent; stable increasing runs keep their keys
    let mut groups: HashMap<Option<Uuid>, Vec<usize>> = HashMap::new();
    let mut group_order: Vec<Option<Uuid>> = Vec::new();
    for (idx, p) in placed.iter().enumerate() {
        groups.entry(p.parent).or_insert_with(|| {
            group_order.push(p.parent);
            Vec::new()
        });
        groups.get_mut(&p.parent).unwrap().push(idx);
    }

    for parent in group_order {
        let idxs = &groups[&parent];
        let mut stable = vec![false; idxs.len()];
        let mut last_key = String::new();
        for (gi, &pi) in idxs.iter().enumerate() {
            let p = &placed[pi];
            if let Some(oi) = p.old_idx
                && old[oi].parent == p.parent
                && old[oi].order_key > last_key
            {
                stable[gi] = true;
                last_key = old[oi].order_key.clone();
            }
        }
        let mut keys: Vec<Option<String>> = idxs
            .iter()
            .enumerate()
            .map(|(gi, &pi)| stable[gi].then(|| old[placed[pi].old_idx.unwrap()].order_key.clone()))
            .collect();
        for gi in 0..idxs.len() {
            if keys[gi].is_none() {
                let prev = if gi > 0 { keys[gi - 1].clone() } else { None };
                let next = keys[gi + 1..].iter().flatten().next().cloned();
                keys[gi] = Some(order_key::between(prev.as_deref(), next.as_deref()));
            }
            let pi = idxs[gi];
            let p = &placed[pi];
            let seg = &new[p.new_idx];
            match p.old_idx {
                None => ops.push(OpInput {
                    kind: OpKind::Insert {
                        block_id: p.temp_id,
                        parent_id: p.parent,
                        order_key: keys[gi].clone().unwrap(),
                        block_type: seg.block_type,
                        content: seg.content.clone(),
                        refers_to: None,
                    },
                    source_refs: src(),
                }),
                Some(oi) => {
                    let moved = !stable[gi] || old[oi].parent != p.parent;
                    if moved {
                        ops.push(OpInput {
                            kind: OpKind::Move {
                                target: old[oi].id,
                                new_parent: p.parent,
                                new_order_key: keys[gi].clone().unwrap(),
                            },
                            source_refs: src(),
                        });
                    }
                    if replaced.contains(&(p.new_idx, oi)) || old[oi].content != seg.content {
                        ops.push(OpInput {
                            kind: OpKind::Replace {
                                target: old[oi].id,
                                content: seg.content.clone(),
                            },
                            source_refs: src(),
                        });
                    }
                }
            }
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockStore, PrincipalKind, SqliteStore, import::import_markdown};

    const MD: &str = "# Title\n\nfirst para\n\n## Sub\n\nsub para\n\ntail para";

    fn setup() -> (SqliteStore, Uuid, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) = import_markdown(&mut s, "d", None, tom.id, MD).unwrap();
        (s, doc, tom.id)
    }

    fn apply_md(s: &mut SqliteStore, doc: Uuid, who: Uuid, md: &str) -> Vec<OpInput> {
        let tree = s.read_doc(doc).unwrap();
        let ops = markdown_to_ops(&tree.roots, md);
        if !ops.is_empty() {
            s.propose(doc, tree.doc.current_epoch, who, ops.clone())
                .unwrap();
        }
        ops
    }

    #[test]
    fn identical_markdown_is_a_noop() {
        let (s, doc, _) = setup();
        let tree = s.read_doc(doc).unwrap();
        assert!(markdown_to_ops(&tree.roots, MD).is_empty());
    }

    #[test]
    fn single_paragraph_edit_is_one_replace_preserving_ids() {
        let (mut s, doc, tom) = setup();
        let before: Vec<Uuid> = {
            let t = s.read_doc(doc).unwrap();
            let mut v = Vec::new();
            fn rec(ns: &[crate::BlockNode], v: &mut Vec<Uuid>) {
                for n in ns {
                    v.push(n.block.id);
                    rec(&n.children, v);
                }
            }
            rec(&t.roots, &mut v);
            v
        };
        let ops = apply_md(
            &mut s,
            doc,
            tom,
            &MD.replace("first para", "first para, edited"),
        );
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].kind, OpKind::Replace { .. }));
        let after = crate::export::export_doc(&s, doc).unwrap();
        assert!(after.contains("first para, edited"));
        // every other block kept its id
        let t = s.read_doc(doc).unwrap();
        let mut now = Vec::new();
        fn rec2(ns: &[crate::BlockNode], v: &mut Vec<Uuid>) {
            for n in ns {
                v.push(n.block.id);
                rec2(&n.children, v);
            }
        }
        rec2(&t.roots, &mut now);
        assert_eq!(before, now);
    }

    #[test]
    fn appended_section_is_inserts_only() {
        let (mut s, doc, tom) = setup();
        let ops = apply_md(
            &mut s,
            doc,
            tom,
            &format!("{MD}\n\n## New Section\n\nnew body"),
        );
        assert!(ops.iter().all(|o| matches!(o.kind, OpKind::Insert { .. })));
        assert_eq!(ops.len(), 2);
        let out = crate::export::export_doc(&s, doc).unwrap();
        assert!(out.contains("## New Section"));
    }

    #[test]
    fn removed_paragraph_is_one_delete() {
        let (mut s, doc, tom) = setup();
        let ops = apply_md(&mut s, doc, tom, &MD.replace("\n\ntail para", ""));
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].kind, OpKind::Delete { .. }));
    }

    #[test]
    fn full_round_trip_matches_export() {
        let (mut s, doc, tom) = setup();
        let new_md = "# Retitled\n\nfirst para\n\ncompletely new para\n\n## Sub\n\nsub para";
        apply_md(&mut s, doc, tom, new_md);
        let out = crate::export::export_doc(&s, doc).unwrap();
        assert_eq!(out.trim_end(), new_md);
    }
}
