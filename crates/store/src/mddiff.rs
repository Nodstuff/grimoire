//! Markdown → minimal block ops (the agent-facing twin of the UI editor's
//! diff engine). An agent hands over a doc's full new markdown; this compares
//! it against the current blocks and emits insert/replace/delete/move ops —
//! unchanged blocks keep their ids, so provenance and comments survive.
//!
//! Structure rules mirror the importer: heading levels derive nesting;
//! sibling order uses fractional keys, with a greedy increasing run keeping
//! untouched siblings' keys stable. Matching is LCS on exact content.

use crate::import::{Segment, infer_block_type, is_frontmatter, segment};
use crate::{Block, BlockNode, BlockType, OpInput, OpKind, is_editor_hidden, order_key};
use std::collections::HashMap;
use uuid::Uuid;

struct Existing {
    id: Uuid,
    content: String,
    parent: Option<Uuid>,
    order_key: String,
}

/// Flatten the current tree in order, skipping (with their subtrees) every
/// block `skip` rejects. Skipped blocks are invisible to the diff: never
/// deleted, replaced, moved, or paired.
fn flatten(nodes: &[BlockNode], skip: &dyn Fn(&Block) -> bool, out: &mut Vec<Existing>) {
    for n in nodes {
        if !skip(&n.block) {
            out.push(Existing {
                id: n.block.id,
                content: n.block.content.clone(),
                parent: n.block.parent_id,
                order_key: n.block.order_key.clone(),
            });
            flatten(&n.children, skip, out);
        }
    }
}

/// Comments and canvases are not content flow: the agent path (whole doc,
/// frontmatter included) skips only those.
fn skip_non_content(b: &Block) -> bool {
    matches!(b.block_type, BlockType::Comment | BlockType::CanvasScene)
}

/// Prose blocks retype freely as their content is edited (a paragraph may
/// become a heading), so an unmatched old block of one prose type may be
/// paired with a new segment of another. Everything else pairs only with its
/// own type, and frontmatter pairs with nothing — pairing it with an edited
/// paragraph is how the hot path used to destroy a doc's tags.
fn may_pair(old: &Existing, seg: &Segment) -> bool {
    if is_frontmatter(&old.content) || is_frontmatter(&seg.content) {
        return false;
    }
    let prose = |t: BlockType| {
        matches!(
            t,
            BlockType::Paragraph | BlockType::Heading | BlockType::Decision
        )
    };
    let old_t = infer_block_type(&old.content);
    old_t == seg.block_type || (prose(old_t) && prose(seg.block_type))
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

/// Compute the minimal op set turning the doc's current content into
/// `markdown` — the agent path (`propose_markdown`): `markdown` is the whole
/// doc, frontmatter included, so only comments and canvases are skipped.
pub fn markdown_to_ops(roots: &[BlockNode], markdown: &str) -> Vec<OpInput> {
    diff_with_filter(roots, markdown, &skip_non_content, "propose_markdown")
}

/// The editor path: `markdown` is what the editor showed, which never
/// includes frontmatter, horizontal rules, comments or canvases
/// (`is_editor_hidden`). Those blocks are left exactly as they are — a hot
/// session's flatten must not delete what the editor merely could not seed.
pub fn markdown_to_ops_editor(roots: &[BlockNode], markdown: &str) -> Vec<OpInput> {
    diff_with_filter(roots, markdown, &is_editor_hidden, "editor")
}

fn diff_with_filter(
    roots: &[BlockNode],
    markdown: &str,
    skip: &dyn Fn(&Block) -> bool,
    source: &str,
) -> Vec<OpInput> {
    let mut old = Vec::new();
    flatten(roots, skip, &mut old);
    let new = segment(markdown);
    let matched = lcs_match(&old, &new);

    let mut ops: Vec<OpInput> = Vec::new();
    let src = || vec![source.to_string()];

    // 1. deletes: old blocks not matched by any new segment
    let matched_old: std::collections::HashSet<usize> = matched.iter().flatten().copied().collect();
    // pair leftovers in order as replacements instead of delete+insert where
    // both sides have an unmatched, type-compatible item (keeps ids + comment
    // anchors alive). Order-preserving greedy: each unmatched new segment
    // takes the first compatible unmatched old block after the previous pair.
    let unmatched_old: Vec<usize> = (0..old.len())
        .filter(|i| !matched_old.contains(i))
        .collect();
    let unmatched_new: Vec<usize> = (0..new.len()).filter(|j| matched[*j].is_none()).collect();
    let mut matched = matched;
    let mut paired_old: std::collections::HashSet<usize> = Default::default();
    let mut cursor = 0;
    for &j in &unmatched_new {
        let Some(k) =
            (cursor..unmatched_old.len()).find(|&k| may_pair(&old[unmatched_old[k]], &new[j]))
        else {
            continue;
        };
        let i = unmatched_old[k];
        matched[j] = Some(i);
        paired_old.insert(i);
        cursor = k + 1;
    }
    for &i in unmatched_old.iter().filter(|i| !paired_old.contains(i)) {
        ops.push(OpInput {
            kind: OpKind::Delete { target: old[i].id },
            source_refs: src(),
        });
    }

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
                    // a paired leftover with identical content (LCS left it
                    // out for a reorder) needs only the Move above
                    if old[oi].content != seg.content {
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

    fn ids_in_order(ns: &[crate::BlockNode], v: &mut Vec<Uuid>) {
        for n in ns {
            v.push(n.block.id);
            ids_in_order(&n.children, v);
        }
    }

    fn find_by_content(s: &SqliteStore, doc: Uuid, content: &str) -> Block {
        let t = s.read_doc(doc).unwrap();
        let mut v = Vec::new();
        ids_in_order(&t.roots, &mut v);
        v.into_iter()
            .map(|id| s.read_block(id).unwrap())
            .find(|b| b.content == content)
            .unwrap_or_else(|| panic!("no block with content {content:?}"))
    }

    const FM_MD: &str = "---\ntags:\n  - keep\n---\n\nbody\n\n---\n\nafter rule";

    #[test]
    fn editor_diff_never_touches_frontmatter_or_rules() {
        let (mut s, doc, tom) = {
            let mut s = SqliteStore::open_in_memory().unwrap();
            let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
            let (doc, n) = import_markdown(&mut s, "d", None, tom.id, FM_MD).unwrap();
            assert_eq!(n, 4, "frontmatter, body, hr, after rule");
            (s, doc, tom.id)
        };
        let fm = find_by_content(&s, doc, "---\ntags:\n  - keep\n---");
        let hr = find_by_content(&s, doc, "---");
        let body = find_by_content(&s, doc, "body");
        assert!(is_editor_hidden(&fm) && is_editor_hidden(&hr) && !is_editor_hidden(&body));

        // the editor showed only "body" and "after rule"; the user edited body
        let tree = s.read_doc(doc).unwrap();
        let ops = markdown_to_ops_editor(&tree.roots, "body edited\n\nafter rule");
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(
            matches!(&ops[0].kind, OpKind::Replace { target, content } if *target == body.id && content == "body edited"),
            "{ops:?}"
        );
        s.propose(doc, tree.doc.current_epoch, tom, ops).unwrap();

        // frontmatter and the rule survive, tags intact, order intact
        assert_eq!(s.read_block(fm.id).unwrap().content, fm.content);
        assert!(!s.read_block(hr.id).unwrap().deleted);
        assert_eq!(s.docs_by_tag("keep").unwrap().len(), 1);
        let out = crate::export::export_doc(&s, doc).unwrap();
        assert_eq!(out.trim_end(), FM_MD.replace("body", "body edited"));
    }

    #[test]
    fn editor_diff_with_identical_visible_markdown_is_a_noop() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) = import_markdown(&mut s, "d", None, tom.id, FM_MD).unwrap();
        let tree = s.read_doc(doc).unwrap();
        assert!(markdown_to_ops_editor(&tree.roots, "body\n\nafter rule").is_empty());
    }

    #[test]
    fn agent_diff_never_pairs_frontmatter_with_prose() {
        // whole-doc path: an agent that drops the frontmatter and edits the
        // body gets Delete(frontmatter) + Replace(body) — never a Replace that
        // overwrites the frontmatter block with prose.
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) = import_markdown(
            &mut s,
            "d",
            None,
            tom.id,
            "---\ntags:\n  - keep\n---\n\nbody",
        )
        .unwrap();
        let fm = find_by_content(&s, doc, "---\ntags:\n  - keep\n---");
        let body = find_by_content(&s, doc, "body");
        let tree = s.read_doc(doc).unwrap();
        let ops = markdown_to_ops(&tree.roots, "body edited");
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(
            ops.iter()
                .any(|o| matches!(&o.kind, OpKind::Delete { target } if *target == fm.id))
        );
        assert!(ops.iter().any(
            |o| matches!(&o.kind, OpKind::Replace { target, content } if *target == body.id && content == "body edited")
        ));
    }

    #[test]
    fn leftover_pairing_respects_type_families() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) = import_markdown(
            &mut s,
            "d",
            None,
            tom.id,
            "```rust\nfn x() {}\n```\n\nsome prose",
        )
        .unwrap();
        let code = find_by_content(&s, doc, "```rust\nfn x() {}\n```");
        let prose = find_by_content(&s, doc, "some prose");
        let tree = s.read_doc(doc).unwrap();

        // code block gone, prose became a heading: heading pairs with the
        // prose (same family), the code block is deleted, not retyped
        let ops = markdown_to_ops(&tree.roots, "## some heading");
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(
            ops.iter()
                .any(|o| matches!(&o.kind, OpKind::Delete { target } if *target == code.id))
        );
        assert!(
            ops.iter()
                .any(|o| matches!(&o.kind, OpKind::Replace { target, .. } if *target == prose.id))
        );

        // a code fence edited into a mermaid fence is delete + insert (types differ)
        let ops = markdown_to_ops(&tree.roots, "```mermaid\ngraph TD\n```\n\nsome prose");
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(
            ops.iter()
                .any(|o| matches!(&o.kind, OpKind::Delete { target } if *target == code.id))
        );
        assert!(ops.iter().any(|o| matches!(
            &o.kind,
            OpKind::Insert {
                block_type: BlockType::DiagramMermaid,
                ..
            }
        )));

        // but a rust fence edited into a python fence keeps its id (both Code)
        let ops = markdown_to_ops(&tree.roots, "```python\nx = 1\n```\n\nsome prose");
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(matches!(&ops[0].kind, OpKind::Replace { target, .. } if *target == code.id));
    }

    #[test]
    fn reordering_siblings_is_exactly_one_move() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) =
            import_markdown(&mut s, "d", None, tom.id, "alpha\n\nbeta\n\ngamma").unwrap();
        let alpha = find_by_content(&s, doc, "alpha");
        let gamma = find_by_content(&s, doc, "gamma");
        let tree = s.read_doc(doc).unwrap();
        let ops = markdown_to_ops(&tree.roots, "beta\n\nalpha\n\ngamma");
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert!(
            matches!(&ops[0].kind, OpKind::Move { target, new_parent: None, .. } if *target == alpha.id),
            "{ops:?}"
        );
        s.propose(doc, tree.doc.current_epoch, tom.id, ops).unwrap();
        let tree = s.read_doc(doc).unwrap();
        let order: Vec<String> = tree.roots.iter().map(|n| n.block.content.clone()).collect();
        assert_eq!(order, ["beta", "alpha", "gamma"]);
        assert_eq!(
            tree.roots[2].block.id, gamma.id,
            "untouched siblings keep ids"
        );
        let out = crate::export::export_doc(&s, doc).unwrap();
        assert_eq!(out.trim_end(), "beta\n\nalpha\n\ngamma");
    }

    #[test]
    fn heading_level_change_reparents_following_blocks() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "t", None).unwrap();
        let (doc, _) =
            import_markdown(&mut s, "d", None, tom.id, "# A\n\npara a\n\n## B\n\npara b").unwrap();
        let a = find_by_content(&s, doc, "# A");
        let b = find_by_content(&s, doc, "## B");
        let para_b = find_by_content(&s, doc, "para b");
        assert_eq!(b.parent_id, Some(a.id));
        assert_eq!(para_b.parent_id, Some(b.id));

        // promote B to a top-level heading: B moves to root and is retyped by
        // content; para b follows its parent without an op of its own
        let tree = s.read_doc(doc).unwrap();
        let ops = markdown_to_ops(&tree.roots, "# A\n\npara a\n\n# B\n\npara b");
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(ops.iter().any(
            |o| matches!(&o.kind, OpKind::Move { target, new_parent: None, .. } if *target == b.id)
        ));
        assert!(ops.iter().any(
            |o| matches!(&o.kind, OpKind::Replace { target, content } if *target == b.id && content == "# B")
        ));
        s.propose(doc, tree.doc.current_epoch, tom.id, ops).unwrap();
        let tree = s.read_doc(doc).unwrap();
        assert_eq!(tree.roots.len(), 2);
        assert_eq!(tree.roots[1].block.id, b.id);
        assert_eq!(tree.roots[1].children[0].block.id, para_b.id);
        assert_eq!(s.read_block(b.id).unwrap().block_type, BlockType::Heading);

        // demote back: para b's parent is unchanged, so still no op for it
        let ops = markdown_to_ops(&tree.roots, "# A\n\npara a\n\n### B\n\npara b");
        assert_eq!(ops.len(), 2, "{ops:?}");
        s.propose(doc, tree.doc.current_epoch, tom.id, ops).unwrap();
        let tree = s.read_doc(doc).unwrap();
        assert_eq!(tree.roots.len(), 1);
        assert_eq!(tree.roots[0].children[1].block.id, b.id);
        assert_eq!(tree.roots[0].children[1].children[0].block.id, para_b.id);
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
