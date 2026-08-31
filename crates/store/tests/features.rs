//! Acceptance tests for #19 links, #24 FTS5 trigram, #25 comments.

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

#[test]
fn fuzzy_search_finds_typoed_terms() {
    let (mut s, tom) = seed();
    add_doc(
        &mut s,
        &tom,
        "budgets",
        "# Budgets\n\nGardener budget constants: tokens and tool calls.\n",
    );
    add_doc(&mut s, &tom, "other", "# Other\n\nNothing relevant here.\n");

    // the 5.9 acceptance phrase: typos in both words
    let hits = s.search_blocks("gardnr bugdet", 5).unwrap();
    assert!(!hits.is_empty(), "typo-tolerant search must hit");
    assert_eq!(hits[0].doc_title, "budgets");

    // exact substring still works
    let hits = s.search_blocks("tool calls", 5).unwrap();
    assert_eq!(hits[0].doc_title, "budgets");

    // sub-trigram query falls back to LIKE without erroring
    let hits = s.search_blocks("to", 5).unwrap();
    assert!(!hits.is_empty());
}

#[test]
fn wikilinks_become_edges_and_backlinks_resolve() {
    let (mut s, tom) = seed();
    let runbook = add_doc(&mut s, &tom, "deploy-runbook", "# Deploy\n\nsteps\n");
    let linker = add_doc(
        &mut s,
        &tom,
        "architecture",
        "# Arch\n\nSee [[Engineering/deploy-runbook]] and [[deploy-runbook|the runbook]].\n",
    );

    let back = s.backlinks(runbook).unwrap();
    assert_eq!(back.len(), 1, "one linking block (both links from it)");
    assert_eq!(back[0].doc_title, "architecture");

    // editing the block away removes the edge
    let block_id = back[0].block.id;
    let epoch = s.get_doc(linker).unwrap().current_epoch;
    s.apply(
        linker,
        epoch,
        tom.id,
        vec![OpInput {
            kind: OpKind::Replace {
                target: block_id,
                content: "no links now".into(),
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    assert!(s.backlinks(runbook).unwrap().is_empty());
}

#[test]
fn comments_anchor_thread_and_stay_out_of_export() {
    let (mut s, tom) = seed();
    let agent = s
        .create_principal(PrincipalKind::Agent, "gardener", None)
        .unwrap();
    let md = "# T\n\ncontent para\n";
    let doc = add_doc(&mut s, &tom, "d", md);
    let para = s.read_doc(doc).unwrap().roots[0].children[0].block.id;

    let c1 = s
        .add_comment(para, agent.id, "is this stale?", None)
        .unwrap();
    let c2 = s
        .add_comment(para, tom.id, "no, checked today", Some(c1.id))
        .unwrap();
    assert_eq!(c2.parent_id, Some(c1.id), "threads are trees");
    assert_eq!(c1.refers_to, Some(para));

    let thread = s.list_comments(para).unwrap();
    assert_eq!(thread.len(), 2);
    assert_eq!(
        thread[0].created_by, agent.id,
        "agent comments distinguishable by principal"
    );

    // comments survive an edit to the anchored block
    let epoch = s.get_doc(doc).unwrap().current_epoch;
    s.apply(
        doc,
        epoch,
        tom.id,
        vec![OpInput {
            kind: OpKind::Replace {
                target: para,
                content: "content para v2".into(),
            },
            source_refs: vec![],
        }],
    )
    .unwrap();
    assert_eq!(s.list_comments(para).unwrap().len(), 2);

    // export contains the content, never the comments
    let out = export::export_doc(&s, doc).unwrap();
    assert!(out.contains("content para v2"));
    assert!(!out.contains("is this stale"));

    // reply_to must belong to the same thread
    let other_doc = add_doc(&mut s, &tom, "d2", "# X\n\npara\n");
    let other_para = s.read_doc(other_doc).unwrap().roots[0].children[0].block.id;
    let err = s.add_comment(other_para, tom.id, "cross-thread", Some(c1.id));
    assert!(matches!(err, Err(StoreError::InvalidOp(_))));
}

#[test]
fn frontmatter_tags_extract_and_query() {
    let (mut s, tom) = seed();
    let d1 = add_doc(
        &mut s,
        &tom,
        "daily",
        "---\ntags:\n  - daily\n  - work\n---\n\n# Log\n",
    );
    add_doc(&mut s, &tom, "untagged", "# Plain\n\ncontent\n");

    let tags = s.list_tags().unwrap();
    assert!(tags.contains(&("daily".into(), 1)) && tags.contains(&("work".into(), 1)));
    assert_eq!(s.docs_by_tag("daily").unwrap()[0].id, d1);
    let untagged = s.untagged_docs(10).unwrap();
    assert_eq!(untagged.len(), 1);
    assert_eq!(untagged[0].title, "untagged");
}

#[test]
fn propose_reviewed_caps_greens_at_yellow_and_is_batch_declinable() {
    let (mut s, tom) = seed();
    let agent = s
        .create_principal(PrincipalKind::Agent, "tagger", None)
        .unwrap();
    let doc = add_doc(&mut s, &tom, "d", "# T\n\npara\n");
    let epoch = s.get_doc(doc).unwrap().current_epoch;
    let para = s.read_doc(doc).unwrap().roots[0].children[0].block.id;

    let out = s
        .propose_reviewed(
            doc,
            epoch,
            agent.id,
            vec![OpInput {
                kind: OpKind::Replace {
                    target: para,
                    content: "para v2".into(),
                },
                source_refs: vec!["gardener:test".into()],
            }],
        )
        .unwrap();
    assert_eq!(
        out.verdicts[0].verdict,
        Verdict::Yellow,
        "green capped to yellow"
    );
    assert!(out.verdicts[0].applied, "still applied — yellow semantics");
    assert_eq!(s.read_block(para).unwrap().content, "para v2");

    // declining reverts via pre-image: 'declined batch leaves no trace'
    let ann = s.review_queue(Some(doc)).unwrap()[0].annotation.id;
    s.resolve(ann, tom.id, ReviewDecision::Decline).unwrap();
    assert_eq!(s.read_block(para).unwrap().content, "para");
    assert!(s.review_queue(Some(doc)).unwrap().is_empty());
}

#[test]
fn gardener_registry_crud_and_runs() {
    let (mut s, _tom) = seed();
    let g = s
        .create_gardener(
            "tagging",
            GardenerKind::Tagging,
            "tag the docs",
            None,
            ConfidencePolicy::Review,
        )
        .unwrap();
    assert_eq!(s.list_gardeners().unwrap().len(), 1);
    // distinct principal per gardener
    let p = s.get_principal(g.principal).unwrap();
    assert_eq!(p.kind, PrincipalKind::Agent);
    assert_eq!(p.display_name, "tagging");

    let run = s.start_run(g.id).unwrap();
    s.finish_run(run, "ok", "verdicts: 3 yellow", Some(1200), Some(0))
        .unwrap();
    let runs = s.list_runs(5).unwrap();
    assert_eq!(runs[0].status, "ok");
    assert_eq!(runs[0].tokens_used, Some(1200));

    s.set_gardener_enabled(g.id, false).unwrap();
    assert!(!s.list_gardeners().unwrap()[0].enabled);
}
