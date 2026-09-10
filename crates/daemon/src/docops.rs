//! Tree ops through the gate (AX slice B): rename / move / set_status /
//! delete / merge as an AGENT. Every call lands one ledger op with a fixed
//! verdict — yellow (applied + flagged, revertible by decline) or, for
//! anything that trashes a doc, red (parked until a human accepts in the
//! queue). The refusal rules are the human API's own (`refuse_if_mirror`,
//! `refuse_move`, the live-session freeze), so an agent can never do what
//! the app would refuse the user.
//!
//! `is_hot` is passed as a closure so this module is testable without a
//! `HotState`; `mcp.rs` wires `HotState::is_hot`.

use crate::api::{refuse_if_mirror, refuse_move};
use grimoire_store::{BlockStore, DocStatus, OpKind, ProposeOutcome, SqliteStore, order_key};
use serde_json::{Value, json};
use uuid::Uuid;

pub type IsHot<'a> = &'a dyn Fn(Uuid) -> bool;

fn src(tool: &str) -> Vec<String> {
    vec![format!("mcp:{tool}")]
}

/// Yellow: applied now, inbound [[wikilinks]] rewritten, flagged.
pub fn rename(
    store: &mut SqliteStore,
    doc_id: Uuid,
    title: &str,
    principal: Uuid,
) -> Result<ProposeOutcome, String> {
    rename_with_refs(store, doc_id, title, principal, src("rename_doc"))
}

/// `rename` with caller-chosen provenance (the filer's `filer: <reason>`).
pub fn rename_with_refs(
    store: &mut SqliteStore,
    doc_id: Uuid,
    title: &str,
    principal: Uuid,
    source_refs: Vec<String>,
) -> Result<ProposeOutcome, String> {
    if let Some(e) = refuse_if_mirror(store, doc_id, "renaming") {
        return Err(e);
    }
    store
        .propose_doc_op(
            doc_id,
            principal,
            OpKind::RenameDoc {
                title: title.to_string(),
                from_title: String::new(),
            },
            source_refs,
        )
        .map_err(|e| e.to_string())
}

/// Yellow. `after` = the sibling to land after (must share `new_parent`);
/// None = append. The sort key is computed here from the existing siblings.
pub fn move_doc(
    store: &mut SqliteStore,
    doc_id: Uuid,
    new_parent: Option<Uuid>,
    after: Option<Uuid>,
    principal: Uuid,
) -> Result<ProposeOutcome, String> {
    move_doc_with_refs(store, doc_id, new_parent, after, principal, src("move_doc"))
}

/// `move_doc` with caller-chosen provenance (the filer's `filer: <reason>`).
pub fn move_doc_with_refs(
    store: &mut SqliteStore,
    doc_id: Uuid,
    new_parent: Option<Uuid>,
    after: Option<Uuid>,
    principal: Uuid,
    source_refs: Vec<String>,
) -> Result<ProposeOutcome, String> {
    if let Some(e) = refuse_move(store, doc_id, new_parent) {
        return Err(e);
    }
    let sort_key = match after {
        None => None,
        Some(a) => {
            if a == doc_id {
                return Err("after_doc_id cannot be the doc itself".into());
            }
            let after_doc = store.get_doc(a).map_err(|e| e.to_string())?;
            if after_doc.parent_id != new_parent {
                return Err(format!(
                    "after_doc_id {a} is not a child of the destination parent {}",
                    new_parent.map(|p| p.to_string()).unwrap_or_else(|| "(root)".into())
                ));
            }
            let mut siblings: Vec<_> = store
                .list_docs()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|d| d.parent_id == new_parent && d.id != doc_id)
                .collect();
            siblings.sort_by(|x, y| x.sort_key.cmp(&y.sort_key));
            let idx = siblings.iter().position(|d| d.id == a).unwrap_or(siblings.len() - 1);
            let next = siblings.get(idx + 1).and_then(|d| d.sort_key.as_deref());
            Some(order_key::between(after_doc.sort_key.as_deref(), next))
        }
    };
    store
        .propose_doc_op(
            doc_id,
            principal,
            OpKind::MoveDoc {
                new_parent,
                sort_key,
                new_parent_title: None,
                from_parent: None,
                from_sort_key: None,
                from_parent_title: None,
            },
            source_refs,
        )
        .map_err(|e| e.to_string())
}

/// Parse the tool's status argument: draft | in-review | decided |
/// superseded, or None / "null" / "" to clear.
pub fn parse_status(s: Option<&str>) -> Result<Option<DocStatus>, String> {
    match s.map(str::trim) {
        None | Some("") | Some("null") | Some("none") => Ok(None),
        Some(v) => DocStatus::parse(v)
            .map(Some)
            .ok_or_else(|| format!("bad status {v:?}: draft | in-review | decided | superseded | null")),
    }
}

/// Yellow.
pub fn set_status(
    store: &mut SqliteStore,
    doc_id: Uuid,
    status: Option<DocStatus>,
    principal: Uuid,
) -> Result<ProposeOutcome, String> {
    if let Some(e) = refuse_if_mirror(store, doc_id, "status") {
        return Err(e);
    }
    store
        .propose_doc_op(
            doc_id,
            principal,
            OpKind::SetStatus {
                status,
                from_status: None,
            },
            src("set_status"),
        )
        .map_err(|e| e.to_string())
}

/// Red, always: parked until a human accepts, then the subtree goes to the
/// Trash. Refused when any doc of the subtree is a mirror or in a live session.
pub fn delete(
    store: &mut SqliteStore,
    is_hot: IsHot,
    doc_id: Uuid,
    principal: Uuid,
) -> Result<ProposeOutcome, String> {
    refuse_subtree_locked(store, is_hot, doc_id, "deleting")?;
    store
        .propose_doc_op(
            doc_id,
            principal,
            OpKind::DeleteDoc {
                title: String::new(),
                doc_count: 0,
            },
            src("delete_doc"),
        )
        .map_err(|e| e.to_string())
}

/// The delete/merge universe check: no mirror and no live session anywhere
/// in the subtree (the app's own rule, `api::delete_doc`).
fn refuse_subtree_locked(
    store: &SqliteStore,
    is_hot: IsHot,
    doc_id: Uuid,
    what: &str,
) -> Result<(), String> {
    let ids = store.doc_subtree_ids(doc_id).map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Err(format!("not found: doc {doc_id}"));
    }
    for d in &ids {
        if let Some(e) = refuse_if_mirror(store, *d, what) {
            return Err(e);
        }
        if is_hot(*d) {
            let title = store.get_doc(*d).map(|d| d.title).unwrap_or_default();
            return Err(format!("“{title}” is in a live session — end it before {what}"));
        }
    }
    Ok(())
}

/// Append `from`'s content after `into`'s last block as reviewable yellows,
/// then park a red delete of `from`. `from`'s frontmatter is not carried
/// over (`into` keeps its own tags). Returns both outcomes.
pub fn merge(
    store: &mut SqliteStore,
    is_hot: IsHot,
    from: Uuid,
    into: Uuid,
    principal: Uuid,
) -> Result<Value, String> {
    if from == into {
        return Err("from_doc_id and into_doc_id are the same doc".into());
    }
    refuse_subtree_locked(store, is_hot, from, "merging")?;
    if let Some(e) = refuse_if_mirror(store, into, "merging into") {
        return Err(e);
    }
    if is_hot(into) {
        return Err("the target doc is in a live session — retry after it ends".into());
    }
    if store
        .doc_subtree_ids(from)
        .map_err(|e| e.to_string())?
        .contains(&into)
    {
        return Err("into_doc_id is inside from_doc_id's subtree — trashing from would trash it too".into());
    }
    let from_tree = store.read_doc(from).map_err(|e| e.to_string())?;
    let into_tree = store.read_doc(into).map_err(|e| e.to_string())?;
    // from's blocks minus frontmatter, as markdown appended to into's own
    let from_roots: Vec<_> = from_tree
        .roots
        .iter()
        .filter(|n| !grimoire_store::import::is_frontmatter(&n.block.content))
        .cloned()
        .collect();
    let appended = grimoire_store::export::markdown_of(&from_roots, false);
    let content_outcome = if appended.trim().is_empty() {
        None
    } else {
        let mut combined = grimoire_store::export::markdown_of(&into_tree.roots, false);
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&appended);
        let ops = grimoire_store::mddiff::markdown_to_ops(&into_tree.roots, &combined);
        if ops.is_empty() {
            None
        } else {
            Some(
                store
                    .propose_reviewed(into, into_tree.doc.current_epoch, principal, ops)
                    .map_err(|e| e.to_string())?,
            )
        }
    };
    let delete_outcome = delete(store, is_hot, from, principal)?;
    Ok(json!({
        "into": content_outcome,
        "delete": delete_outcome,
        "note": format!(
            "{} block(s) from “{}” appended to “{}” as flagged yellows; trashing “{}” is parked red until a human accepts",
            content_outcome.as_ref().map(|o| o.verdicts.len()).unwrap_or(0),
            from_tree.doc.title, into_tree.doc.title, from_tree.doc.title
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{
        AnnotationKind, AnnotationStatus, BlockStore, PrincipalKind, ReviewDecision, Verdict,
        import::import_markdown,
    };

    fn setup() -> (SqliteStore, Uuid, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let bot = s.create_principal(PrincipalKind::Agent, "claude:test", None).unwrap().id;
        (s, tom, bot)
    }

    fn cold(_: Uuid) -> bool {
        false
    }

    #[test]
    fn rename_is_yellow_and_declining_reverts_it() {
        let (mut s, tom, bot) = setup();
        let d = s.create_doc("Old", None, tom).unwrap();
        let (linker, _) = import_markdown(&mut s, "L", None, tom, "see [[Old]] and [[Old|alias]]").unwrap();
        let out = rename(&mut s, d.id, "New", bot).unwrap();
        assert_eq!(out.verdicts[0].verdict, Verdict::Yellow);
        assert!(out.verdicts[0].applied);
        assert_eq!(s.get_doc(d.id).unwrap().title, "New");
        assert!(grimoire_store::export::export_doc(&s, linker).unwrap().contains("[[New]] and [[New|alias]]"));
        let q = s.review_queue(Some(d.id)).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].annotation.kind, AnnotationKind::Review);
        assert!(matches!(&q[0].op.kind, OpKind::RenameDoc { title, from_title } if title == "New" && from_title == "Old"));

        s.resolve(q[0].annotation.id, tom, ReviewDecision::Decline).unwrap();
        assert_eq!(s.get_doc(d.id).unwrap().title, "Old");
        assert!(grimoire_store::export::export_doc(&s, linker).unwrap().contains("[[Old]] and [[Old|alias]]"));
        assert!(s.review_queue(None).unwrap().is_empty());
        // same title again is a refusal, not a no-op yellow
        assert!(rename(&mut s, d.id, "Old", bot).is_err());
    }

    #[test]
    fn move_after_sibling_computes_key_and_decline_moves_back() {
        let (mut s, tom, bot) = setup();
        let folder = s.create_doc("F", None, tom).unwrap();
        let a = s.create_doc("a", Some(folder.id), tom).unwrap();
        let c = s.create_doc("c", Some(folder.id), tom).unwrap();
        let b = s.create_doc("b", None, tom).unwrap();
        let out = move_doc(&mut s, b.id, Some(folder.id), Some(a.id), bot).unwrap();
        assert_eq!(out.verdicts[0].verdict, Verdict::Yellow);
        let kids: Vec<String> = s
            .doc_subtree(folder.id)
            .unwrap()
            .into_iter()
            .filter(|d| d.parent_id == Some(folder.id))
            .map(|d| d.title)
            .collect();
        assert_eq!(kids, ["a", "b", "c"]);
        // wrong parent for after_doc_id
        assert!(move_doc(&mut s, c.id, None, Some(a.id), bot).is_err());
        // cycle is an error, not a parked red
        assert!(move_doc(&mut s, folder.id, Some(a.id), None, bot).is_err());

        let q = s.review_queue(Some(b.id)).unwrap();
        s.resolve(q[0].annotation.id, tom, ReviewDecision::Decline).unwrap();
        let back = s.get_doc(b.id).unwrap();
        assert_eq!(back.parent_id, None);
        assert_eq!(back.sort_key, b.sort_key);
    }

    #[test]
    fn set_status_yellow_and_parse() {
        let (mut s, tom, bot) = setup();
        let d = s.create_doc("D", None, tom).unwrap();
        assert_eq!(parse_status(Some("null")).unwrap(), None);
        assert!(parse_status(Some("bogus")).is_err());
        let st = parse_status(Some("decided")).unwrap();
        set_status(&mut s, d.id, st, bot).unwrap();
        assert_eq!(s.get_doc(d.id).unwrap().status, Some(DocStatus::Decided));
        let q = s.review_queue(Some(d.id)).unwrap();
        s.resolve(q[0].annotation.id, tom, ReviewDecision::Accept).unwrap();
        assert_eq!(s.get_doc(d.id).unwrap().status, Some(DocStatus::Decided));
        set_status(&mut s, d.id, None, bot).unwrap();
        assert_eq!(s.get_doc(d.id).unwrap().status, None);
        let q = s.review_queue(Some(d.id)).unwrap();
        s.resolve(q[0].annotation.id, tom, ReviewDecision::Decline).unwrap();
        assert_eq!(s.get_doc(d.id).unwrap().status, Some(DocStatus::Decided));
    }

    #[test]
    fn delete_parks_red_until_a_human_accepts_then_trashes() {
        let (mut s, tom, bot) = setup();
        let d = s.create_doc("Gone", None, tom).unwrap();
        let kid = s.create_doc("kid", Some(d.id), tom).unwrap();
        let out = delete(&mut s, &cold, d.id, bot).unwrap();
        assert_eq!(out.verdicts[0].verdict, Verdict::Red);
        assert!(!out.verdicts[0].applied);
        assert!(out.verdicts[0].note.contains("2 docs"));
        assert!(!s.doc_is_tombstoned(d.id).unwrap(), "nothing happened yet");
        let q = s.review_queue(None).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].annotation.kind, AnnotationKind::Parked);
        assert!(matches!(&q[0].op.kind, OpKind::DeleteDoc { title, doc_count } if title == "Gone" && *doc_count == 2));
        // the agent cannot accept its own
        assert!(s.resolve(q[0].annotation.id, bot, ReviewDecision::Accept).is_err());
        // a live session in the subtree blocks the accept
        s.set_frozen_probe(Box::new(move |id| id == kid.id));
        assert!(s.resolve(q[0].annotation.id, tom, ReviewDecision::Accept).is_err());
        s.set_frozen_probe(Box::new(|_| false));
        s.resolve(q[0].annotation.id, tom, ReviewDecision::Accept).unwrap();
        assert!(s.doc_is_tombstoned(d.id).unwrap() && s.doc_is_tombstoned(kid.id).unwrap());
        let trash = s.list_trash().unwrap();
        assert_eq!(trash.len(), 1);
        assert_eq!(trash[0].descendants, 1);
        assert_eq!(s.restore_doc(d.id).unwrap(), 2);
        // the resolved item shows in the agent's outcomes
        let mine = s.proposal_outcomes(bot, 10).unwrap();
        assert!(mine.iter().any(|(op, st, _)| matches!(op.kind, OpKind::DeleteDoc { .. }) && st.as_deref() == Some("accepted")));
    }

    #[test]
    fn delete_declined_is_never_applied_and_hot_docs_are_refused_up_front() {
        let (mut s, tom, bot) = setup();
        let d = s.create_doc("Keep", None, tom).unwrap();
        let kid = s.create_doc("kid", Some(d.id), tom).unwrap();
        let hot = move |id: Uuid| id == kid.id;
        let e = delete(&mut s, &hot, d.id, bot).unwrap_err();
        assert!(e.contains("live session"), "{e}");
        delete(&mut s, &cold, d.id, bot).unwrap();
        let q = s.review_queue(None).unwrap();
        s.resolve(q[0].annotation.id, tom, ReviewDecision::Decline).unwrap();
        assert!(!s.doc_is_tombstoned(d.id).unwrap());
        assert_eq!(
            s.proposal_outcomes(bot, 10).unwrap()[0].1.as_deref(),
            Some(AnnotationStatus::Declined.as_str())
        );
    }

    #[test]
    fn mirrors_are_refused_everywhere() {
        let (mut s, tom, bot) = setup();
        let m = s.create_doc("Mirror", None, tom).unwrap();
        let contact = s.pair_contact("ab".repeat(32).as_str(), "alice").unwrap();
        s.upsert_mirror(m.id, contact.id, Uuid::now_v7(), 0, grimoire_store::SharePermission::View)
            .unwrap();
        let mine = s.create_doc("Mine", None, tom).unwrap();
        assert!(rename(&mut s, m.id, "X", bot).is_err());
        assert!(set_status(&mut s, m.id, Some(DocStatus::Draft), bot).is_err());
        assert!(delete(&mut s, &cold, m.id, bot).is_err());
        assert!(move_doc(&mut s, mine.id, Some(m.id), None, bot).is_err(), "nothing lands inside a mirror");
        assert!(merge(&mut s, &cold, m.id, mine.id, bot).is_err());
        assert!(merge(&mut s, &cold, mine.id, m.id, bot).is_err());
    }

    #[test]
    fn merge_appends_as_yellows_and_parks_the_delete() {
        let (mut s, tom, bot) = setup();
        let (into, _) = import_markdown(&mut s, "2026-09-08", None, tom, "---\ntags:\n  - daily\n---\n\n# A\n\none").unwrap();
        let (from, _) = import_markdown(&mut s, "2026-09-08", None, tom, "---\ntags:\n  - other\n---\n\n# B\n\ntwo").unwrap();
        assert!(merge(&mut s, &cold, from, from, bot).is_err());
        let out = merge(&mut s, &cold, from, into, bot).unwrap();
        assert_eq!(out["delete"]["verdicts"][0]["verdict"], "red");
        let verdicts = out["into"]["verdicts"].as_array().unwrap();
        assert_eq!(verdicts.len(), 2, "heading + paragraph, no frontmatter: {out}");
        assert!(verdicts.iter().all(|v| v["verdict"] == "yellow"));
        let md = grimoire_store::export::export_doc(&s, into).unwrap();
        assert_eq!(md, "---\ntags:\n  - daily\n---\n\n# A\n\none\n\n# B\n\ntwo\n");
        assert!(!s.doc_is_tombstoned(from).unwrap());
        assert_eq!(s.review_queue(None).unwrap().len(), 3);
        // a doc cannot be merged into its own descendant
        let child = s.create_doc("child", Some(from), tom).unwrap();
        assert!(merge(&mut s, &cold, from, child.id, bot).is_err());
    }
}
