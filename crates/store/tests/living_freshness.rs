//! Living answers (answer_sources) and doc freshness (docs.verified_at):
//! table round-trips, the resolve hook, list ordering, migration idempotence.

use grimoire_store::*;
use uuid::Uuid;

fn seed() -> (SqliteStore, Uuid) {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
    (s, tom)
}

fn first_block(s: &SqliteStore, doc: Uuid) -> Block {
    s.read_doc(doc).unwrap().roots[0].block.clone()
}

#[test]
fn answer_sources_round_trip_and_track_change() {
    let (mut s, tom) = seed();
    let (src, _) = import::import_markdown(&mut s, "Src", None, tom, "alpha fact\n\nbeta fact\n").unwrap();
    let tree = s.read_doc(src).unwrap();
    let (a, b) = (tree.roots[0].block.clone(), tree.roots[1].block.clone());
    let answer = s.create_doc("what is alpha", None, tom).unwrap();
    s.record_answer_sources(answer.id, &[(a.id, a.epoch), (b.id, b.epoch)]).unwrap();

    let rows = s.answer_sources(answer.id).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| !r.changed && !r.gone));
    assert_eq!(rows[0].doc_title.as_deref(), Some("Src"));
    assert_eq!(s.answer_docs().unwrap(), vec![answer.id]);

    // edit block a: its epoch moves → that source is stale, b is not
    s.apply(
        src,
        tree.doc.current_epoch,
        tom,
        vec![OpInput {
            kind: OpKind::Replace { target: a.id, content: "alpha fact, revised".into() },
            source_refs: vec![],
        }],
    )
    .unwrap();
    let rows = s.answer_sources(answer.id).unwrap();
    let by_id = |id: Uuid| rows.iter().find(|r| r.block_id == id).unwrap();
    assert!(by_id(a.id).changed && !by_id(a.id).gone);
    assert!(!by_id(b.id).changed);

    // tombstone b → gone and changed
    let epoch = s.get_doc(src).unwrap().current_epoch;
    s.apply(src, epoch, tom, vec![OpInput { kind: OpKind::Delete { target: b.id }, source_refs: vec![] }])
        .unwrap();
    let rows = s.answer_sources(answer.id).unwrap();
    let gone_b = rows.iter().find(|r| r.block_id == b.id).unwrap();
    assert!(gone_b.gone && gone_b.changed);

    // re-record replaces the set
    let a_now = s.read_block(a.id).unwrap();
    s.record_answer_sources(answer.id, &[(a.id, a_now.epoch)]).unwrap();
    let rows = s.answer_sources(answer.id).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].changed);

    // a trashed answer doc drops out of the sweep list
    s.delete_doc(answer.id).unwrap();
    assert!(s.answer_docs().unwrap().is_empty());
}

#[test]
fn verified_at_is_null_until_set_and_a_plain_edit_never_sets_it() {
    let (mut s, tom) = seed();
    let (d, _) = import::import_markdown(&mut s, "D", None, tom, "one\n").unwrap();
    assert_eq!(s.doc_verified_at(d).unwrap(), None);
    let b = first_block(&s, d);
    s.apply(d, 1, tom, vec![OpInput { kind: OpKind::Replace { target: b.id, content: "two".into() }, source_refs: vec![] }])
        .unwrap();
    assert_eq!(s.doc_verified_at(d).unwrap(), None, "an ordinary edit is not a verification");
    s.set_doc_verified(d).unwrap();
    assert!(s.doc_verified_at(d).unwrap().is_some());
    assert!(matches!(s.set_doc_verified(Uuid::now_v7()), Err(StoreError::NotFound(_))));
}

#[test]
fn human_accepting_an_auditor_fix_verifies_the_doc_but_other_accepts_do_not() {
    let (mut s, tom) = seed();
    let auditor = s
        .create_gardener("aud", GardenerKind::Auditor, "audit", None, ConfidencePolicy::Review)
        .unwrap();
    let tagger = s
        .create_gardener("tags", GardenerKind::Tagging, "tag", None, ConfidencePolicy::Review)
        .unwrap();
    let (d, _) = import::import_markdown(&mut s, "D", None, tom, "stale claim\n\nother\n").unwrap();
    let tree = s.read_doc(d).unwrap();
    let (b0, b1) = (tree.roots[0].block.id, tree.roots[1].block.id);

    // a tagging gardener's yellow, accepted: no verification
    s.propose_reviewed(
        d,
        tree.doc.current_epoch,
        tagger.principal,
        vec![OpInput { kind: OpKind::Replace { target: b1, content: "other, tagged".into() }, source_refs: vec![] }],
    )
    .unwrap();
    let q = s.review_queue(Some(d)).unwrap();
    s.resolve(q[0].annotation.id, tom, ReviewDecision::Accept).unwrap();
    assert_eq!(s.doc_verified_at(d).unwrap(), None);

    // the auditor parks a fix; the human declines → still not verified
    s.park(
        d,
        auditor.principal,
        vec![OpInput { kind: OpKind::Replace { target: b0, content: "fresh claim".into() }, source_refs: vec![] }],
        "",
    )
    .unwrap();
    let q = s.review_queue(Some(d)).unwrap();
    s.resolve(q[0].annotation.id, tom, ReviewDecision::Decline).unwrap();
    assert_eq!(s.doc_verified_at(d).unwrap(), None);

    // the auditor parks again; the human accepts → verified
    s.park(
        d,
        auditor.principal,
        vec![OpInput { kind: OpKind::Replace { target: b0, content: "fresh claim".into() }, source_refs: vec![] }],
        "",
    )
    .unwrap();
    let q = s.review_queue(Some(d)).unwrap();
    s.resolve(q[0].annotation.id, tom, ReviewDecision::Accept).unwrap();
    assert!(s.doc_verified_at(d).unwrap().is_some());
}

#[test]
fn freshness_lists_never_verified_first_then_oldest_and_skips_mirrors_and_empty_docs() {
    let (mut s, tom) = seed();
    let (old, _) = import::import_markdown(&mut s, "Old", None, tom, "x\n").unwrap();
    let (newer, _) = import::import_markdown(&mut s, "Newer", None, tom, "x\n").unwrap();
    let (never, _) = import::import_markdown(&mut s, "Never", None, tom, "x\n").unwrap();
    let _empty_folder = s.create_doc("Folder", None, tom).unwrap();
    let (mirror, _) = import::import_markdown(&mut s, "Mirror", None, tom, "x\n").unwrap();
    let contact = s.pair_contact(&"ab".repeat(32), "alice").unwrap();
    s.upsert_mirror(mirror, contact.id, Uuid::now_v7(), 0, SharePermission::View).unwrap();

    s.set_doc_verified(old).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3));
    s.set_doc_verified(newer).unwrap();

    let rows = s.freshness(50, false).unwrap();
    let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(titles, vec!["Never", "Old", "Newer"], "{rows:?}");
    assert!(rows.iter().all(|r| !r.tended));
    assert!(rows.iter().all(|r| !r.last_edited.is_empty()));
    assert_eq!(rows[0].id, never);
    assert_eq!(s.freshness(1, false).unwrap().len(), 1);

    // tended filter: only docs under an enabled gardener scope
    s.create_gardener("keep", GardenerKind::Keeper, "k", Some(newer), ConfidencePolicy::Review)
        .unwrap();
    let tended = s.freshness(50, true).unwrap();
    assert_eq!(tended.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), vec!["Newer"]);
    assert!(tended[0].tended);
}

/// A pre-freshness / pre-filer database opens, gains the column and the
/// widened kind CHECK, and opening it again changes nothing.
#[test]
fn migration_adds_verified_at_and_filer_kind_idempotently() {
    let dir = std::env::temp_dir().join(format!("grimoire-freshness-mig-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ks.db");
    let (p1, g1) = (Uuid::now_v7(), Uuid::now_v7());
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            &format!("CREATE TABLE principals (id TEXT PRIMARY KEY, kind TEXT NOT NULL, display_name TEXT NOT NULL, pubkey TEXT);
             CREATE TABLE docs (
                 id TEXT PRIMARY KEY, parent_id TEXT, title TEXT NOT NULL, review_policy TEXT, status TEXT,
                 current_epoch INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL, sort_key TEXT,
                 deleted INTEGER NOT NULL DEFAULT 0, deleted_at TEXT,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
             CREATE TABLE gardeners (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
                 kind TEXT NOT NULL DEFAULT 'tagging' CHECK (kind IN ('tagging', 'reviewer', 'auditor', 'scribe', 'keeper')),
                 principal TEXT NOT NULL REFERENCES principals (id), scope_doc TEXT, task_prompt TEXT NOT NULL,
                 bindings TEXT NOT NULL DEFAULT '[]', creds_ref TEXT, schedule TEXT NOT NULL DEFAULT 'daily',
                 confidence_policy TEXT NOT NULL DEFAULT 'review', enabled INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
             INSERT INTO principals VALUES ('{p1}', 'agent', 'old-tagger', NULL);
             INSERT INTO gardeners (id, name, kind, principal, task_prompt) VALUES ('{g1}', 'old', 'tagging', '{p1}', 'tag');"),
        )
        .unwrap();
    }
    for _ in 0..2 {
        let mut s = SqliteStore::open(&path).unwrap();
        let tom = match s.list_principals().unwrap().into_iter().find(|p| p.kind == PrincipalKind::Human) {
            Some(p) => p.id,
            None => s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id,
        };
        let d = s.create_doc("D", None, tom).unwrap();
        assert_eq!(s.doc_verified_at(d.id).unwrap(), None);
        s.set_doc_verified(d.id).unwrap();
        // the old row survived the rebuild and the new kind is accepted
        assert!(s.list_gardeners().unwrap().iter().any(|g| g.name == "old"));
        let name = format!("filer-{}", Uuid::now_v7());
        let f = s.create_gardener(&name, GardenerKind::Filer, "file", None, ConfidencePolicy::Review).unwrap();
        assert_eq!(f.kind, GardenerKind::Filer);
        assert!(s.list_gardeners().unwrap().iter().any(|g| g.id == f.id && g.kind == GardenerKind::Filer));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
