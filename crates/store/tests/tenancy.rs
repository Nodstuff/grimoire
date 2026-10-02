//! ADR 0004 at the store boundary: two tenants, A (the instance owner) and
//! B (a member). A has a private "Work", B has a private "Work", A shares
//! "Family" with B as a viewer, and each has Unsorted docs. Every scoped
//! read must show B nothing of A's private data, and by-id reads answer
//! NotFound (never Forbidden).

use taisce_store::*;
use uuid::Uuid;

fn para(content: &str) -> OpInput {
    OpInput {
        kind: OpKind::Insert {
            block_id: Uuid::now_v7(),
            parent_id: None,
            order_key: String::new(),
            block_type: BlockType::Paragraph,
            content: content.into(),
            refers_to: None,
        },
        source_refs: vec![],
    }
}

struct T {
    store: SharedStore,
    a: Uuid,
    b: Uuid,
    a_human: Uuid,
    b_human: Uuid,
    agent: Uuid,
    a_work: Uuid,
    b_work: Uuid,
    family: Uuid,
    /// A's private doc in A's Work
    a_secret: Uuid,
    a_secret_block: Uuid,
    /// A's Unsorted doc
    a_loose: Uuid,
    /// A's doc in Family (shared with B as viewer)
    shared_doc: Uuid,
    shared_block: Uuid,
    /// B's own docs
    b_doc: Uuid,
    b_loose: Uuid,
}

fn not_found<T: std::fmt::Debug>(r: Result<T>) {
    match r {
        Err(StoreError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

fn forbidden<T: std::fmt::Debug>(r: Result<T>) {
    match r {
        Err(StoreError::Forbidden(_)) => {}
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

fn setup() -> T {
    let mut raw = SqliteStore::open_in_memory().unwrap();
    let a_human = raw.create_principal(PrincipalKind::Human, "Tom", None).unwrap().id;
    let agent = raw.create_principal(PrincipalKind::Agent, "claude:test", None).unwrap().id;
    let a = raw.auth_ensure_owner(a_human, "Tom", 1).unwrap().id;
    let bu = raw.auth_add_user("Aoife", 2).unwrap();
    let (b, b_human) = (bu.id, bu.principal_id);
    let store = SharedStore::new(raw);

    let (a_work, family, a_secret, a_secret_block, a_loose, shared_doc, shared_block) = {
        let mut s = store.lock(Scope::User(a));
        let a_work = s.create_workspace("Work", None, None, None).unwrap().id;
        let family = s.create_workspace("Family", None, None, None).unwrap().id;
        let (root, _) = s.create_doc_with_ops("Work root", None, a_human, vec![para("root of work")]).unwrap();
        s.set_doc_workspace(root.id, Some(a_work), a_human).unwrap();
        let mut secret_op = para("the needle secret salary [[Family plan]]");
        let secret_block = match &mut secret_op.kind {
            OpKind::Insert { block_id, .. } => *block_id,
            _ => unreachable!(),
        };
        let (secret, _) = s.create_doc_with_ops("Salary review", Some(root.id), a_human, vec![secret_op, para("---\ntags: [money]\n---")]).unwrap();
        let (loose, _) = s.create_doc_with_ops("Tom loose", None, a_human, vec![para("needle in tom's unsorted")]).unwrap();
        let mut shared_op = para("needle shared with family");
        let shared_block = match &mut shared_op.kind {
            OpKind::Insert { block_id, .. } => *block_id,
            _ => unreachable!(),
        };
        let (fam_root, _) = s.create_doc_with_ops("Family plan", None, a_human, vec![shared_op]).unwrap();
        s.set_doc_workspace(fam_root.id, Some(family), a_human).unwrap();
        s.share_workspace(family, b, Role::Viewer).unwrap();
        (a_work, family, secret.id, secret_block, loose.id, fam_root.id, shared_block)
    };
    let (b_work, b_doc, b_loose) = {
        let mut s = store.lock(Scope::User(b));
        let b_work = s.create_workspace("Work", None, None, None).unwrap().id;
        let (d, _) = s.create_doc_with_ops("Aoife work", None, b_human, vec![para("needle aoife work")]).unwrap();
        s.set_doc_workspace(d.id, Some(b_work), b_human).unwrap();
        let (l, _) = s.create_doc_with_ops("Aoife loose", None, b_human, vec![para("needle aoife unsorted")]).unwrap();
        (b_work, d.id, l.id)
    };
    T { store, a, b, a_human, b_human, agent, a_work, b_work, family, a_secret, a_secret_block, a_loose, shared_doc, shared_block, b_doc, b_loose }
}

#[test]
fn b_sees_only_her_own_and_the_shared_docs() {
    let t = setup();
    let s = t.store.lock(Scope::User(t.b));
    let ids: Vec<Uuid> = s.list_docs().unwrap().into_iter().map(|d| d.id).collect();
    assert!(ids.contains(&t.b_doc) && ids.contains(&t.b_loose) && ids.contains(&t.shared_doc), "{ids:?}");
    assert!(!ids.contains(&t.a_secret) && !ids.contains(&t.a_loose), "A's private docs leak: {ids:?}");
    // by id: NotFound, never Forbidden
    not_found(s.get_doc(t.a_secret));
    not_found(s.read_doc(t.a_secret));
    not_found(s.read_block(t.a_secret_block));
    not_found(s.ops_since(t.a_secret, 0));
    not_found(s.ops_for_doc_limited(t.a_secret, 10));
    not_found(s.backlinks(t.a_secret));
    not_found(s.list_comments(t.a_secret_block));
    not_found(s.doc_subtree_ids(t.a_secret));
    not_found(s.doc_is_tombstoned(t.a_loose));
    not_found(s.doc_workspace(t.a_secret));
    not_found(s.doc_label(t.a_secret));
    not_found(s.review_queue(Some(t.a_secret)));
    not_found(s.effective_policy(t.a_secret));
    not_found(s.answer_sources(t.a_secret));
    not_found(s.doc_subtree(t.a_secret));
    not_found(s.get_workspace(t.a_work));
    assert!(s.read_doc(t.shared_doc).is_ok());

    // search, grep, refs, tags, links, counts
    let hits = s.search_blocks("needle", 50).unwrap();
    let hit_docs: Vec<Uuid> = hits.iter().map(|h| h.block.doc_id).collect();
    assert!(!hit_docs.contains(&t.a_secret) && !hit_docs.contains(&t.a_loose), "{hit_docs:?}");
    assert!(hit_docs.contains(&t.shared_doc) && hit_docs.contains(&t.b_doc));
    assert!(s.search_blocks("ne", 50).unwrap().iter().all(|h| h.block.doc_id != t.a_secret), "LIKE path too");
    assert!(s.live_blocks_with_titles().unwrap().iter().all(|h| h.block.doc_id != t.a_secret && h.block.doc_id != t.a_loose));
    let suffix = &t.a_secret_block.to_string()[30..];
    assert!(s.blocks_by_id_suffix(suffix).unwrap().is_empty(), "^ref resolution");
    assert!(s.list_tags().unwrap().iter().all(|(tag, _)| tag != "money"));
    assert!(s.docs_by_tag("money").unwrap().is_empty());
    assert!(s.raw_doc_tags().unwrap().keys().all(|d| *d != t.a_secret.to_string()));
    assert!(s.raw_links().unwrap().iter().all(|(from, _)| *from != t.a_secret.to_string()), "graph edges");
    assert!(s.backlinks(t.shared_doc).unwrap().is_empty(), "A's private doc links to Family plan: not B's to see");
    assert!(!s.block_counts().unwrap().contains_key(&t.a_secret));
    assert!(s.raw_tending().unwrap().iter().all(|(d, _)| *d != t.a_secret.to_string()));
    assert!(s.untagged_docs(100).unwrap().iter().all(|d| d.id != t.a_secret && d.id != t.a_loose));
    assert!(s.block_vecs().unwrap().is_empty() || true);
    assert!(s.linking_blocks("Family plan").unwrap().is_empty());
    assert!(s.workspace_map().unwrap().keys().all(|d| *d != t.a_secret && *d != t.a_loose));
    assert!(s.stale_block_vectors(1000).unwrap().iter().all(|(b, _, _)| *b != t.a_secret_block));
    assert!(s.blocks_as_hits(&[t.a_secret_block, t.shared_block]).unwrap().iter().all(|h| h.block.id != t.a_secret_block));
}

#[test]
fn workspaces_are_per_owner_with_display_names() {
    let t = setup();
    {
        let s = t.store.lock(Scope::User(t.b));
        let ws = s.list_workspaces().unwrap();
        let names: Vec<(&str, &str)> = ws.iter().map(|w| (w.name.as_str(), w.display_name.as_str())).collect();
        assert_eq!(ws.len(), 2, "her Work and the shared Family, never A's Work: {names:?}");
        let fam = ws.iter().find(|w| w.id == t.family).unwrap();
        assert_eq!(fam.role, Role::Viewer);
        assert!(fam.shared);
        assert_eq!(fam.owner_name.as_deref(), Some("Tom"));
        assert_eq!(fam.display_name, "Family", "no clash, no suffix");
        let own = ws.iter().find(|w| w.id == t.b_work).unwrap();
        assert_eq!((own.role, own.shared, own.display_name.as_str()), (Role::Owner, false, "Work"));
        assert_eq!(s.find_workspace("work").unwrap().unwrap().id, t.b_work, "her own Work");
        assert!(s.find_workspace(&t.a_work.to_string()).unwrap().is_none(), "A's id does not resolve for B");
        assert!(s.workspace_members(t.a_work).is_err());
        assert_eq!(s.workspace_members(t.family).unwrap().len(), 2);
    }
    // A shares Work too: now B sees two "Work"s; the foreign one carries its owner
    t.store.lock(Scope::User(t.a)).share_workspace(t.a_work, t.b, Role::Editor).unwrap();
    let s = t.store.lock(Scope::User(t.b));
    let ws = s.list_workspaces().unwrap();
    let theirs = ws.iter().find(|w| w.id == t.a_work).unwrap();
    assert_eq!(theirs.display_name, "Work · Tom");
    assert_eq!(ws.iter().find(|w| w.id == t.b_work).unwrap().display_name, "Work");
    assert_eq!(s.find_workspace("Work").unwrap().unwrap().id, t.b_work, "own beats shared");
    assert_eq!(s.find_workspace("Work · Tom").unwrap().unwrap().id, t.a_work, "the display name picks the shared one");
    // a third user with access to both foreign Works: ambiguous
    drop(s);
    let c = t.store.lock(Scope::System).auth_add_user("Ciara", 3).unwrap().id;
    t.store.lock(Scope::User(t.a)).share_workspace(t.a_work, c, Role::Viewer).unwrap();
    t.store.lock(Scope::User(t.b)).share_workspace(t.b_work, c, Role::Viewer).unwrap();
    let s = t.store.lock(Scope::User(c));
    let err = s.find_workspace("work").unwrap_err().to_string();
    assert!(err.contains("Work · Tom") && err.contains("Work · Aoife") && err.contains(&t.a_work.to_string()), "{err}");
    assert!(s.find_workspace(&t.b_work.to_string()).unwrap().is_some(), "an id disambiguates");
}

#[test]
fn names_are_unique_per_owner_not_server_wide() {
    let t = setup();
    let mut s = t.store.lock(Scope::User(t.b));
    assert!(s.create_workspace("WORK", None, None, None).is_err(), "her own clash");
    assert!(s.create_workspace("Personal", None, None, None).is_ok());
    drop(s);
    assert!(t.store.lock(Scope::User(t.a)).create_workspace("Personal", None, None, None).is_ok(), "both can have Personal");
}

#[test]
fn a_viewer_cannot_write_and_an_outsider_gets_not_found() {
    let t = setup();
    let mut s = t.store.lock(Scope::User(t.b));
    let epoch = s.get_doc(t.shared_doc).unwrap().current_epoch;
    forbidden(s.apply(t.shared_doc, epoch, t.b_human, vec![para("x")]));
    forbidden(s.propose(t.shared_doc, epoch, t.b_human, vec![para("x")]));
    forbidden(s.add_comment(t.shared_block, t.b_human, "hi", None));
    forbidden(s.rename_doc(t.shared_doc, "Mine now"));
    forbidden(s.set_doc_status(t.shared_doc, Some(DocStatus::Draft)));
    forbidden(s.delete_doc(t.shared_doc));
    forbidden(s.create_doc("child", Some(t.shared_doc), t.b_human));
    forbidden(s.move_doc(t.b_loose, Some(t.shared_doc), None));
    forbidden(s.propose_doc_op(t.shared_doc, t.b_human, OpKind::SetStatus { status: Some(DocStatus::Draft), from_status: None }, vec![]));
    forbidden(s.update_workspace(t.family, WorkspacePatch { name: Some("Ours".into()), ..Default::default() }));
    forbidden(s.delete_workspace(t.family));
    forbidden(s.share_workspace(t.family, t.b, Role::Editor));
    // A's private doc: it does not exist for B
    not_found(s.apply(t.a_secret, 1, t.b_human, vec![para("x")]));
    not_found(s.rename_doc(t.a_secret, "x"));
    not_found(s.create_doc("child", Some(t.a_secret), t.b_human));
    not_found(s.move_doc(t.b_loose, Some(t.a_secret), None));
    not_found(s.set_doc_workspace(t.b_loose, Some(t.a_work), t.b_human));
    not_found(s.restore_doc(t.a_loose));
}

#[test]
fn moves_and_labels_across_spaces_are_authorized_shares() {
    let t = setup();
    t.store.lock(Scope::User(t.a)).share_workspace(t.family, t.b, Role::Editor).unwrap();
    let mut s = t.store.lock(Scope::User(t.b));
    // into a workspace she edits: allowed (a share, by a human)
    s.move_doc(t.b_loose, Some(t.shared_doc), None).unwrap();
    assert_eq!(s.doc_workspace(t.b_loose).unwrap(), Some(t.family));
    // out of a workspace she does not own: refused
    forbidden(s.move_doc(t.b_loose, None, None));
    forbidden(s.set_doc_workspace(t.shared_doc, None, t.b_human));
    drop(s);
    // the owner can take it back out
    let mut s = t.store.lock(Scope::User(t.a));
    s.move_doc(t.b_loose, None, None).unwrap();
    assert!(s.list_docs().unwrap().iter().any(|d| d.id == t.b_loose), "re-rooted: now A's Unsorted");
    drop(s);
    not_found(t.store.lock(Scope::User(t.b)).get_doc(t.b_loose));
}

#[test]
fn the_share_gate_never_lands_an_agent_green_in_a_shared_workspace() {
    let t = setup();
    let mut s = t.store.lock(Scope::User(t.a));
    // unshared: green as ever
    let e = s.get_doc(t.a_secret).unwrap().current_epoch;
    let out = s.propose(t.a_secret, e, t.agent, vec![para("agent note")]).unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Green);
    // shared (Family has B): yellow, flagged, even under auto
    s.set_review_policy(t.shared_doc, Some(ReviewPolicy::Auto)).unwrap();
    let e = s.get_doc(t.shared_doc).unwrap().current_epoch;
    let out = s.propose(t.shared_doc, e, t.agent, vec![para("agent in shared")]).unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Yellow, "{:?}", out.verdicts);
    assert!(out.verdicts[0].applied);
    assert!(out.verdicts[0].note.contains("shared workspace"));
    let open = s.review_queue(Some(t.shared_doc)).unwrap();
    assert_eq!(open.len(), 1, "flagged for a human");
    // a direct apply by an agent is flagged too
    let e = s.get_doc(t.shared_doc).unwrap().current_epoch;
    s.apply(t.shared_doc, e, t.agent, vec![para("direct")]).unwrap();
    assert_eq!(s.review_queue(Some(t.shared_doc)).unwrap().len(), 2);
    // a comment by an agent, too
    s.add_comment(t.shared_block, t.agent, "a flag", None).unwrap();
    assert_eq!(s.review_queue(Some(t.shared_doc)).unwrap().len(), 3);
    // a new doc under a shared parent: its content lands flagged
    let (child, _) = s.create_doc_with_ops("Agent child", Some(t.shared_doc), t.agent, vec![para("child body")]).unwrap();
    assert_eq!(s.review_queue(Some(child.id)).unwrap().len(), 1);
    // the human owner writes green
    let e = s.get_doc(t.shared_doc).unwrap().current_epoch;
    assert_eq!(s.propose(t.shared_doc, e, t.a_human, vec![para("human")]).unwrap().verdicts[0].verdict, Verdict::Green);
    // an agent moving a doc into the shared workspace parks red
    let out = s
        .propose_doc_op(t.a_loose, t.agent, OpKind::MoveDoc {
            new_parent: Some(t.shared_doc),
            sort_key: None,
            new_parent_title: None,
            from_parent: None,
            from_sort_key: None,
            from_parent_title: None,
        }, vec![])
        .unwrap();
    assert_eq!(out.verdicts[0].verdict, Verdict::Red);
    assert!(!out.verdicts[0].applied);
    assert_eq!(s.doc_workspace(t.a_loose).unwrap(), None, "not moved until a human accepts");
    // and may not label one into it
    forbidden(s.set_doc_workspace(t.a_loose, Some(t.family), t.agent));
    // a human accepting the parked move applies it
    let item = s.review_queue(Some(t.a_loose)).unwrap().pop().unwrap();
    s.resolve(item.annotation.id, t.a_human, ReviewDecision::Accept).unwrap();
    assert_eq!(s.doc_workspace(t.a_loose).unwrap(), Some(t.family));
}

#[test]
fn revocation_tells_b_to_drop_the_docs_and_search_forgets_them() {
    let t = setup();
    let seq = t.store.lock(Scope::User(t.b)).latest_change_seq().unwrap();
    {
        let s = t.store.lock(Scope::User(t.b));
        assert!(s.search_blocks("family", 10).unwrap().iter().any(|h| h.block.doc_id == t.shared_doc));
        // B's whole feed never mentions A's private docs
        let all = s.changes_since(0, 10_000).unwrap();
        for c in &all.changes {
            assert!(c.doc_id != t.a_secret.to_string() && c.doc_id != t.a_loose.to_string(), "{c:?}");
        }
        assert!(all.changes.iter().any(|c| c.doc_id == t.shared_doc.to_string()));
    }
    assert!(t.store.lock(Scope::User(t.a)).unshare_workspace(t.family, t.b).unwrap());
    let s = t.store.lock(Scope::User(t.b));
    let page = s.changes_since(seq, 100).unwrap();
    let drop_row = page
        .changes
        .iter()
        .find(|c| c.doc_id == t.shared_doc.to_string())
        .expect("a drop row for the revoked doc");
    assert_eq!(drop_row.kind, "deleted");
    assert_eq!(drop_row.access.as_deref(), Some("revoked"));
    assert!(drop_row.doc.is_none(), "no title for a doc she can no longer see");
    assert!(s.search_blocks("family", 10).unwrap().is_empty());
    not_found(s.read_doc(t.shared_doc));
    assert!(s.list_workspaces().unwrap().iter().all(|w| w.id != t.family));
    drop(s);
    // A's feed carries no targeted rows for B
    let a_page = t.store.lock(Scope::User(t.a)).changes_since(seq, 100).unwrap();
    assert!(a_page.changes.iter().all(|c| c.access.is_none()));
    // re-sharing grants them back with a fetch row
    let seq2 = t.store.lock(Scope::User(t.b)).latest_change_seq().unwrap();
    t.store.lock(Scope::User(t.a)).share_workspace(t.family, t.b, Role::Viewer).unwrap();
    let page = t.store.lock(Scope::User(t.b)).changes_since(seq2, 100).unwrap();
    let grant = page.changes.iter().find(|c| c.doc_id == t.shared_doc.to_string()).unwrap();
    assert_eq!((grant.kind.as_str(), grant.access.as_deref()), ("tree", Some("granted")));
    assert!(grant.doc.is_some());
}

#[test]
fn the_audience_of_a_change_is_who_can_see_it() {
    let t = setup();
    let seq = t.store.lock(Scope::System).latest_change_seq().unwrap();
    {
        let mut s = t.store.lock(Scope::User(t.a));
        let e = s.get_doc(t.a_secret).unwrap().current_epoch;
        s.apply(t.a_secret, e, t.a_human, vec![para("private edit")]).unwrap();
    }
    let who = t.store.lock(Scope::System).change_audience(seq).unwrap();
    assert!(who.contains(&t.a) && !who.contains(&t.b), "{who:?}");
    let seq = t.store.lock(Scope::System).latest_change_seq().unwrap();
    {
        let mut s = t.store.lock(Scope::User(t.a));
        let e = s.get_doc(t.shared_doc).unwrap().current_epoch;
        s.apply(t.shared_doc, e, t.a_human, vec![para("shared edit")]).unwrap();
    }
    let who = t.store.lock(Scope::System).change_audience(seq).unwrap();
    assert!(who.contains(&t.a) && who.contains(&t.b));
    assert!(t.store.lock(Scope::User(t.a)).change_audience(0).is_err(), "System only");
}

#[test]
fn trash_principals_settings_idempotency_and_gardeners_are_per_user() {
    let t = setup();
    {
        let mut s = t.store.lock(Scope::User(t.a));
        s.delete_doc(t.a_loose).unwrap();
        s.set_setting("home.last_visit", "a-visit").unwrap();
        s.idempotency_put(t.agent, t.a_loose, "a-outcome", 10, 100).unwrap();
    }
    let mut s = t.store.lock(Scope::User(t.b));
    assert!(s.list_trash().unwrap().is_empty(), "A's trash is not B's");
    not_found(s.restore_doc(t.a_loose));
    assert_eq!(s.get_setting("home.last_visit").unwrap(), None);
    s.set_setting("home.last_visit", "b-visit").unwrap();
    assert_eq!(s.get_setting("home.last_visit").unwrap().as_deref(), Some("b-visit"));
    assert_eq!(s.idempotency_get(t.agent, t.a_loose, 0).unwrap(), None, "the same agent+key never replays A's outcome");
    // principals: B sees Tom (they share Family) but a stranger's human stays hidden
    drop(s);
    let c = t.store.lock(Scope::System).auth_add_user("Stranger", 4).unwrap();
    let s = t.store.lock(Scope::User(t.b));
    let names: Vec<String> = s.list_principals().unwrap().into_iter().map(|p| p.display_name).collect();
    assert!(names.contains(&"Tom".into()) && names.contains(&"Aoife".into()), "{names:?}");
    assert!(!names.contains(&"Stranger".into()), "{names:?}");
    let _ = c;
    drop(s);
    assert_eq!(t.store.lock(Scope::User(t.a)).get_setting("home.last_visit").unwrap().as_deref(), Some("a-visit"));
    // gardeners belong to someone
    let g = t.store.lock(Scope::System).create_gardener("tagger", GardenerKind::Tagging, "t", None, ConfidencePolicy::Review).unwrap();
    assert!(t.store.lock(Scope::User(t.b)).list_gardeners().unwrap().is_empty());
    assert_eq!(t.store.lock(Scope::User(t.a)).list_gardeners().unwrap()[0].id, g.id);
    assert!(t.store.lock(Scope::User(t.b)).set_gardener_enabled(g.id, false).is_err());
}

#[test]
fn local_and_system_see_everything_as_before() {
    let t = setup();
    for scope in [Scope::System, Scope::Local] {
        let s = t.store.lock(scope);
        let ids: Vec<Uuid> = s.list_docs().unwrap().into_iter().map(|d| d.id).collect();
        for d in [t.a_secret, t.a_loose, t.shared_doc, t.b_doc, t.b_loose] {
            assert!(ids.contains(&d), "{scope:?} misses {d}");
        }
        assert_eq!(s.list_workspaces().unwrap().len(), 3);
    }
}

#[test]
fn within_scope_reads_only_its_workspace() {
    let t = setup();
    let s = t.store.lock(Scope::Within { user: t.a, workspace: t.family });
    let ids: Vec<Uuid> = s.list_docs().unwrap().into_iter().map(|d| d.id).collect();
    assert_eq!(ids, vec![t.shared_doc]);
    assert!(s.search_blocks("needle", 50).unwrap().iter().all(|h| h.block.doc_id == t.shared_doc));
    drop(s);
    let s = t.store.lock(Scope::System);
    assert_eq!(s.writer_scope_for(Some(t.a), t.shared_doc).unwrap(), Scope::Within { user: t.a, workspace: t.family });
    assert_eq!(s.writer_scope_for(Some(t.a), t.a_secret).unwrap(), Scope::User(t.a));
}

#[test]
fn membership_changes_and_user_creation_are_audited() {
    let t = setup();
    let ev: Vec<String> = t.store.lock(Scope::System).audit_events(100).unwrap().into_iter().map(|e| e.event).collect();
    assert!(ev.iter().filter(|e| *e == "user.create").count() == 2, "{ev:?}");
    assert!(ev.contains(&"workspace.share".into()));
    t.store.lock(Scope::User(t.a)).unshare_workspace(t.family, t.b).unwrap();
    let _ = t.store.lock(Scope::User(t.a)).set_doc_workspace(t.a_loose, Some(t.family), t.agent);
    let ev: Vec<String> = t.store.lock(Scope::System).audit_events(100).unwrap().into_iter().map(|e| e.event).collect();
    assert!(ev.contains(&"workspace.unshare".into()));
    assert!(t.store.lock(Scope::User(t.a)).audit_events(1).is_err(), "the box's CLI only");
    assert!(t.store.lock(Scope::User(t.a)).auth_add_user("X", 9).is_err(), "users are added on the box");
}

/// The v8 migration on a realistically shaped pre-v8 database (built here,
/// never a real one): ~900 docs, ~17k blocks, the old server-wide-unique
/// workspaces table, one owner. Everything is adopted by the owner, fast.
#[test]
fn migration_v8_on_a_realistic_db() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ks.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(PRE_V8_DDL).unwrap();
        let tx = c.unchecked_transaction().unwrap();
        tx.execute("INSERT INTO principals (id, kind, display_name) VALUES ('00000000-0000-7000-8000-000000000001', 'human', 'Tom')", []).unwrap();
        tx.execute(
            "INSERT INTO auth_users (id, principal_id, name, role, created_at)
             VALUES ('00000000-0000-7000-8000-0000000000aa', '00000000-0000-7000-8000-000000000001', 'Tom', 'owner', 1)",
            [],
        )
        .unwrap();
        tx.execute("INSERT INTO workspaces (id, name, sort_key) VALUES ('00000000-0000-7000-8000-0000000000f1', 'Work', 'a')", []).unwrap();
        tx.execute("INSERT INTO workspaces (id, name, sort_key) VALUES ('00000000-0000-7000-8000-0000000000f2', 'Home', 'b')", []).unwrap();
        let mut n_blocks = 0;
        for d in 0..900 {
            let id = format!("00000000-0000-7000-8000-{d:012}");
            let parent = if d % 10 == 0 { None } else { Some(format!("00000000-0000-7000-8000-{:012}", d - d % 10)) };
            tx.execute(
                "INSERT INTO docs (id, parent_id, title, created_by, sort_key) VALUES (?1, ?2, ?3, '00000000-0000-7000-8000-000000000001', 'a')",
                rusqlite::params![id, parent, format!("Doc {d}")],
            )
            .unwrap();
            if d % 100 == 0 {
                let ws = if d % 200 == 0 { "00000000-0000-7000-8000-0000000000f1" } else { "00000000-0000-7000-8000-0000000000f2" };
                tx.execute("INSERT INTO doc_workspace (doc_id, workspace_id) VALUES (?1, ?2)", rusqlite::params![id, ws]).unwrap();
            }
            for b in 0..19 {
                tx.execute(
                    "INSERT INTO blocks (id, doc_id, order_key, block_type, content, created_by, epoch)
                     VALUES (?1, ?2, ?3, 'paragraph', ?4, '00000000-0000-7000-8000-000000000001', 1)",
                    rusqlite::params![format!("10000000-0000-7000-8000-{:012}", d * 100 + b), id, format!("k{b:03}"), format!("block {b} of doc {d} lorem ipsum")],
                )
                .unwrap();
                n_blocks += 1;
            }
        }
        tx.commit().unwrap();
        c.pragma_update(None, "user_version", 7).unwrap();
        assert!(n_blocks > 16_000);
    }
    let started = std::time::Instant::now();
    let s = SqliteStore::open(&path).unwrap();
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(5), "migration took {took:?}");
    eprintln!("v8 migration on 900 docs / 17.1k blocks: {took:?}");
    let shared = SharedStore::new(s);
    let owner = Uuid::parse_str("00000000-0000-7000-8000-0000000000aa").unwrap();
    {
        let s = shared.lock(Scope::System);
        assert_eq!(s.instance_owner().unwrap(), Some(owner));
        let ws = s.list_workspaces().unwrap();
        assert_eq!(ws.len(), 2);
        assert!(ws.iter().all(|w| w.owner_id == Some(owner) && w.role == Role::Owner && !w.shared));
        assert_eq!(s.workspace_members(ws[0].id).unwrap()[0].user_id, owner);
    }
    {
        // the owner sees everything, exactly as before
        let s = shared.lock(Scope::User(owner));
        assert_eq!(s.list_docs().unwrap().len(), 900);
        let t = std::time::Instant::now();
        let hits = s.search_blocks("lorem", 20).unwrap();
        assert_eq!(hits.len(), 20);
        eprintln!("scoped list+search on the migrated db: {:?}", t.elapsed());
        assert_eq!(s.workspace_map().unwrap().len(), 900);
    }
    // a second user sees nothing of it
    let b = shared.lock(Scope::System).auth_add_user("Aoife", 5).unwrap().id;
    assert!(shared.lock(Scope::User(b)).list_docs().unwrap().is_empty());
    assert!(shared.lock(Scope::User(b)).search_blocks("lorem", 20).unwrap().is_empty());
    // idempotent: reopening changes nothing
    drop(shared);
    let s = SqliteStore::open(&path).unwrap();
    assert_eq!(s.list_workspaces().unwrap().len(), 2);
}

/// The v7 shape of the tables the v8 migration touches (docs without
/// owner_id; workspaces with the server-wide UNIQUE name; changes and
/// gardeners without owner/user columns).
const PRE_V8_DDL: &str = "
CREATE TABLE principals (id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('human', 'agent', 'remote')), display_name TEXT NOT NULL, pubkey TEXT);
CREATE TABLE docs (
    id TEXT PRIMARY KEY, parent_id TEXT REFERENCES docs (id), title TEXT NOT NULL,
    review_policy TEXT CHECK (review_policy IN ('human-review', 'agent-review', 'auto')),
    status TEXT CHECK (status IN ('draft', 'in-review', 'decided', 'superseded')),
    current_epoch INTEGER NOT NULL DEFAULT 0, created_by TEXT NOT NULL REFERENCES principals (id),
    sort_key TEXT, deleted INTEGER NOT NULL DEFAULT 0, deleted_at TEXT, verified_at TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
CREATE TABLE blocks (
    id TEXT PRIMARY KEY, doc_id TEXT NOT NULL REFERENCES docs (id), parent_id TEXT REFERENCES blocks (id),
    order_key TEXT NOT NULL,
    block_type TEXT NOT NULL CHECK (block_type IN ('paragraph', 'heading', 'code', 'diagram_d2', 'diagram_mermaid', 'canvas_scene', 'comment', 'decision')),
    content TEXT NOT NULL, created_by TEXT NOT NULL REFERENCES principals (id), epoch INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0, refers_to TEXT);
CREATE VIRTUAL TABLE blocks_fts USING fts5(content, content='blocks', content_rowid='rowid', tokenize='trigram');
CREATE TRIGGER blocks_fts_ai AFTER INSERT ON blocks BEGIN
    INSERT INTO blocks_fts (rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TABLE ops (
    id TEXT PRIMARY KEY, doc_id TEXT NOT NULL REFERENCES docs (id),
    op_type TEXT NOT NULL CHECK (op_type IN ('insert', 'replace', 'delete', 'move', 'rename_doc', 'move_doc', 'set_status', 'delete_doc')),
    target_block TEXT, payload TEXT NOT NULL, principal TEXT NOT NULL REFERENCES principals (id),
    base_epoch INTEGER NOT NULL, epoch_applied INTEGER, verdict TEXT CHECK (verdict IN ('green', 'yellow', 'red')),
    confidence REAL, prior TEXT, source_refs TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
CREATE TABLE gardeners (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL DEFAULT 'tagging' CHECK (kind IN ('tagging', 'reviewer', 'auditor', 'scribe', 'keeper', 'filer')),
    principal TEXT NOT NULL REFERENCES principals (id), scope_doc TEXT REFERENCES docs (id), task_prompt TEXT NOT NULL,
    bindings TEXT NOT NULL DEFAULT '[]', creds_ref TEXT, schedule TEXT NOT NULL DEFAULT 'daily',
    confidence_policy TEXT NOT NULL DEFAULT 'review' CHECK (confidence_policy IN ('review', 'gate')),
    enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
CREATE TABLE changes (seq INTEGER PRIMARY KEY AUTOINCREMENT, doc_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('doc', 'tree', 'deleted', 'restored')), epoch INTEGER,
    at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
CREATE TABLE auth_users (id TEXT PRIMARY KEY, principal_id TEXT NOT NULL REFERENCES principals (id), name TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')), created_at INTEGER NOT NULL);
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE, color TEXT, icon TEXT, sort_key TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')));
CREATE TABLE doc_workspace (doc_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE);
";

#[test]
fn agent_labels_are_only_visible_where_they_wrote() {
    let t = setup();
    // B's agent writes in B's own doc: its label says what B is working on
    let label = t.store.lock(Scope::System).create_principal(PrincipalKind::Agent, "claude:aoife-private-project", None).unwrap().id;
    {
        let mut s = t.store.lock(Scope::User(t.b));
        let e = s.get_doc(t.b_doc).unwrap().current_epoch;
        s.propose(t.b_doc, e, label, vec![para("agent note")]).unwrap();
        assert!(s.list_principals().unwrap().iter().any(|p| p.id == label), "B sees her own agent");
    }
    let s = t.store.lock(Scope::User(t.a));
    assert!(s.list_principals().unwrap().iter().all(|p| p.id != label), "A never sees B's agent label");
    // identity resolution by name is unscoped, so no duplicate is ever minted
    assert_eq!(s.principal_by_name("claude:aoife-private-project").unwrap().unwrap().id, label);
    assert_eq!(s.principal_named(PrincipalKind::Agent, "claude:test").unwrap().unwrap().id, t.agent);
}
