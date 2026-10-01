//! The change journal (`changes`): every write kind lands a row through the
//! triggers, the cursor pages with since/limit/more, the v6 migration seeds
//! one row per live doc, and the commit hook fires.

use grimoire_store::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

fn store_with_tom() -> (SqliteStore, Principal) {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let tom = s.create_principal(PrincipalKind::Human, "Tom", None).unwrap();
    (s, tom)
}

fn insert(content: &str) -> OpInput {
    OpInput {
        kind: OpKind::Insert {
            block_id: Uuid::now_v7(),
            parent_id: None,
            order_key: "i".into(),
            block_type: BlockType::Paragraph,
            content: content.into(),
            refers_to: None,
        },
        source_refs: vec![],
    }
}

/// (doc_id, kind, epoch) of every row after `since`.
fn rows(s: &SqliteStore, since: i64) -> Vec<(String, String, Option<i64>)> {
    s.changes_since(since, 2000)
        .unwrap()
        .changes
        .into_iter()
        .map(|c| (c.doc_id, c.kind, c.epoch))
        .collect()
}

#[test]
fn every_write_kind_lands_a_row() {
    let (mut s, tom) = store_with_tom();
    assert_eq!(s.latest_change_seq().unwrap(), 0);

    let doc = s.create_doc("d", None, tom.id).unwrap();
    let id = doc.id.to_string();
    assert_eq!(rows(&s, 0), vec![(id.clone(), "tree".into(), Some(0))], "create is a tree change");

    let mut at = s.latest_change_seq().unwrap();
    s.apply(doc.id, 0, tom.id, vec![insert("a"), insert("b")]).unwrap();
    assert_eq!(rows(&s, at), vec![(id.clone(), "doc".into(), Some(1))], "one batch, one row, new epoch");

    at = s.latest_change_seq().unwrap();
    s.rename_doc(doc.id, "renamed").unwrap();
    assert_eq!(rows(&s, at), vec![(id.clone(), "doc".into(), Some(1))]);

    at = s.latest_change_seq().unwrap();
    s.set_doc_status(doc.id, Some(DocStatus::Draft)).unwrap();
    assert_eq!(rows(&s, at), vec![(id.clone(), "doc".into(), Some(1))]);

    let parent = s.create_doc("p", None, tom.id).unwrap();
    at = s.latest_change_seq().unwrap();
    s.move_doc(doc.id, Some(parent.id), None).unwrap();
    assert_eq!(rows(&s, at), vec![(id.clone(), "tree".into(), Some(1))], "reparent is a tree change");

    // the delete takes the subtree: both docs tombstone
    at = s.latest_change_seq().unwrap();
    assert_eq!(s.delete_doc(parent.id).unwrap(), 2);
    let mut got = rows(&s, at);
    got.sort();
    let mut want = vec![
        (id.clone(), "deleted".to_string(), Some(1)),
        (parent.id.to_string(), "deleted".to_string(), Some(0)),
    ];
    want.sort();
    assert_eq!(got, want);

    at = s.latest_change_seq().unwrap();
    s.restore_doc(parent.id).unwrap();
    let kinds: Vec<String> = rows(&s, at).into_iter().map(|r| r.1).collect();
    assert_eq!(kinds, vec!["restored", "restored"]);

    // review state without an epoch move: a parked proposal is a doc change
    let agent = s.create_principal(PrincipalKind::Agent, "gardener", None).unwrap();
    at = s.latest_change_seq().unwrap();
    s.park(doc.id, agent.id, vec![insert("c")], "check this").unwrap();
    assert_eq!(rows(&s, at), vec![(id.clone(), "doc".into(), Some(1))]);

    // a no-op update (same title) writes nothing
    at = s.latest_change_seq().unwrap();
    s.rename_doc(doc.id, "renamed").unwrap();
    assert!(rows(&s, at).is_empty());
}

#[test]
fn since_limit_more_page_in_seq_order() {
    let (mut s, tom) = store_with_tom();
    for i in 0..5 {
        s.create_doc(&format!("d{i}"), None, tom.id).unwrap();
    }
    let head = s.latest_change_seq().unwrap();
    assert_eq!(head, 5);

    let p1 = s.changes_since(0, 2).unwrap();
    assert_eq!(p1.seq, 5);
    assert!(p1.more);
    assert_eq!(p1.changes.iter().map(|c| c.seq).collect::<Vec<_>>(), vec![1, 2]);
    let p2 = s.changes_since(2, 2).unwrap();
    assert!(p2.more);
    assert_eq!(p2.changes.iter().map(|c| c.seq).collect::<Vec<_>>(), vec![3, 4]);
    let p3 = s.changes_since(4, 2).unwrap();
    assert!(!p3.more, "exactly the last row: no more");
    assert_eq!(p3.changes.iter().map(|c| c.seq).collect::<Vec<_>>(), vec![5]);
    let p4 = s.changes_since(5, 2).unwrap();
    assert!(!p4.more && p4.changes.is_empty());
    assert_eq!(p4.seq, 5);
    // `at` is ISO-8601 UTC
    assert!(p1.changes[0].at.ends_with('Z') && p1.changes[0].at.contains('T'));
}

#[test]
fn migration_seeds_one_doc_row_per_live_doc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ks.db");
    let (live, gone) = {
        let mut s = SqliteStore::open(&path).unwrap();
        let tom = s.create_principal(PrincipalKind::Human, "Tom", None).unwrap();
        let live = s.create_doc("live", None, tom.id).unwrap();
        s.apply(live.id, 0, tom.id, vec![insert("x")]).unwrap();
        let gone = s.create_doc("gone", None, tom.id).unwrap();
        s.delete_doc(gone.id).unwrap();
        (live.id, gone.id)
    };
    // pretend this database predates the journal
    {
        let c = rusqlite_conn(&path);
        c.execute_batch("DROP TABLE changes; PRAGMA user_version = 5;").unwrap();
    }
    let s = SqliteStore::open(&path).unwrap();
    let got = rows(&s, 0);
    assert_eq!(got, vec![(live.to_string(), "doc".into(), Some(1))], "trashed {gone} is not seeded");
    // and the triggers are back for new writes
    let mut s = s;
    s.rename_doc(live, "again").unwrap();
    assert_eq!(s.latest_change_seq().unwrap(), 2);
}

fn rusqlite_conn(path: &std::path::Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).unwrap()
}

#[test]
fn commit_hook_fires_on_every_commit() {
    let (mut s, tom) = store_with_tom();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    s.on_commit(move || {
        n2.fetch_add(1, Ordering::SeqCst);
    });
    let doc = s.create_doc("d", None, tom.id).unwrap();
    let after_create = n.load(Ordering::SeqCst);
    assert!(after_create >= 1);
    s.apply(doc.id, 0, tom.id, vec![insert("a")]).unwrap();
    assert!(n.load(Ordering::SeqCst) > after_create);
}

#[test]
fn rows_carry_the_doc_summary_as_it_stands_now() {
    let (mut s, tom) = store_with_tom();
    let parent = s.create_doc("p", None, tom.id).unwrap();
    let doc = s.create_doc("d", Some(parent.id), tom.id).unwrap();
    s.set_doc_status(doc.id, Some(DocStatus::Draft)).unwrap();
    let page = s.changes_since(0, 100).unwrap();
    let row = page.changes.iter().find(|c| c.doc_id == doc.id.to_string()).unwrap();
    let sum = row.doc.as_ref().unwrap();
    assert_eq!(sum.title, "d");
    assert_eq!(sum.parent_id.as_deref(), Some(parent.id.to_string().as_str()));
    assert!(sum.sort_key.is_some());
    assert_eq!(sum.status.as_deref(), Some("draft"), "current state, even on the older create row");
    assert_eq!(sum.current_epoch, 0);
    assert!(!sum.deleted);
    s.delete_doc(doc.id).unwrap();
    let last = s.changes_since(0, 100).unwrap().changes.pop().unwrap();
    assert_eq!(last.kind, "deleted");
    assert!(last.doc.unwrap().deleted);
}
