//! ADR 0008's tables: what each call keeps, and that a person's rows are
//! theirs alone (anyone else's are NotFound or absent, never a 403).

use super::*;
use crate::{BlockStore, PrincipalKind};

struct W {
    s: SqliteStore,
    tom: Uuid,
    ann: Uuid,
    rt: Uuid,
    ann_rt: Uuid,
}

fn setup() -> W {
    let mut s = SqliteStore::open_in_memory().unwrap();
    let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
    let tom = s.auth_ensure_owner(p, "tom", 1).unwrap().id;
    let ann = s.auth_add_user("ann", 1).unwrap().id;
    let rt = s.fabric_runtime_create(tom, "mbp", "h1", 1).unwrap().id;
    let ann_rt = s.fabric_runtime_create(ann, "air", "h2", 1).unwrap().id;
    W { s, tom, ann, rt, ann_rt }
}

fn nf<T: std::fmt::Debug>(r: Result<T>) -> bool {
    matches!(r, Err(StoreError::NotFound(_)))
}

fn turn(session: Uuid, id: &str, ended: i64) -> FabricTurn {
    FabricTurn {
        runtime_id: Uuid::nil(),
        session_id: session,
        user_id: Uuid::nil(),
        turn_id: id.into(),
        address: "taisce/main".into(),
        project: "taisce".into(),
        prompt: "zebrasecret prompt".into(),
        reply: "done".into(),
        text_off: false,
        tool_count: 3,
        outcome: "{}".into(),
        started: ended - 10,
        ended,
        truncated: false,
        context_used_pct: Some(40),
        end_kind: "clean".into(),
        source: "mac".into(),
        order_id: None,
        created_at: ended,
    }
}

fn attention(session: Uuid, id: &str, at: i64) -> FabricAttention {
    FabricAttention {
        runtime_id: Uuid::nil(),
        id: id.into(),
        user_id: Uuid::nil(),
        session_id: session,
        address: "taisce/main".into(),
        project: "taisce".into(),
        source: "runtime".into(),
        kind: "approval".into(),
        text: "./release.sh".into(),
        choices: "[]".into(),
        tool: "Bash".into(),
        guardrail_id: "release".into(),
        guardrail_label: "Release".into(),
        turn_id: String::new(),
        raised_at: at,
        expires_at: None,
        request: Some(vec![1, 2, 3]),
        signature: Some(vec![4; 64]),
        conversation_id: None,
        message_id: None,
        from_address: None,
        cleared_at: None,
        resolution: None,
        updated_at: at,
    }
}

fn order(runtime: Uuid, id: &str, counter: u64, at: i64) -> FabricOrder {
    FabricOrder {
        runtime_id: runtime,
        id: id.into(),
        user_id: Uuid::nil(),
        session_id: Some(Uuid::nil()),
        device_id: "dev".into(),
        counter,
        kind: "command".into(),
        verb: Some("send".into()),
        hold: None,
        text: Some("zebrasecret words".into()),
        after_turn: String::new(),
        queued: false,
        request_id: String::new(),
        choice_id: String::new(),
        order_ids: "[]".into(),
        issued_at: at,
        expires_at: at + 3600,
        created_at: at,
        updated_at: at,
        payload: Some(vec![9; 10]),
        signature: Some(vec![8; 64]),
        state: "pending".into(),
        reason: None,
        detail: None,
        down_instance: None,
        down_seq: None,
        received_at: None,
        project: "taisce".into(),
    }
}

#[test]
fn turns_are_kept_once_and_read_newest_first() {
    let mut w = setup();
    let sid = Uuid::now_v7();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        assert!(s.fabric_turn_put(w.rt, &turn(sid, "t1", 100)).unwrap());
        assert!(!s.fabric_turn_put(w.rt, &turn(sid, "t1", 999)).unwrap(), "a replay changes nothing");
        assert!(s.fabric_turn_put(w.rt, &turn(sid, "t2", 200)).unwrap());
        let got = s.fabric_turns(w.rt, sid, None, 10).unwrap();
        assert_eq!(got.iter().map(|t| t.turn_id.as_str()).collect::<Vec<_>>(), ["t2", "t1"]);
        assert_eq!((got[1].ended, got[1].user_id, got[1].runtime_id), (100, w.tom, w.rt));
        assert_eq!(s.fabric_turns(w.rt, sid, Some(200), 10).unwrap().len(), 1);
        assert_eq!(s.fabric_turns_since(150, 10).unwrap().len(), 1);
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| {
        assert!(nf(s.fabric_turns(w.rt, sid, None, 10)));
        assert!(nf(s.fabric_turn_put(w.rt, &turn(sid, "planted", 1))));
        assert!(s.fabric_turns_since(0, 10).unwrap().is_empty());
    });
    for scope in [Scope::Local, Scope::Within { user: w.tom, workspace: Uuid::now_v7() }] {
        w.s.with_scope_for_test(scope, |s| assert!(s.fabric_turns_since(0, 10).unwrap().is_empty()));
    }
    w.s.with_scope_for_test(Scope::Public, |s| assert!(nf(s.fabric_turns_since(0, 10))));
}

#[test]
fn attention_is_raised_once_trimmed_and_cleared() {
    let mut w = setup();
    let sid = Uuid::now_v7();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        let (row, trimmed) = s.fabric_attention_raise(w.rt, &attention(sid, "r1", 10)).unwrap().unwrap();
        assert_eq!((row.user_id, row.runtime_id, trimmed.len()), (w.tom, w.rt, 0));
        assert!(s.fabric_attention_raise(w.rt, &attention(sid, "r1", 11)).unwrap().is_none(), "a replay");
        for i in 0..MAX_OPEN_ATTENTION {
            s.fabric_attention_raise(w.rt, &attention(sid, &format!("x{i}"), 20 + i as i64)).unwrap();
        }
        assert_eq!(s.fabric_attention_open().unwrap().len(), MAX_OPEN_ATTENTION);
        assert_eq!(s.fabric_attention_get("r1").unwrap().resolution.as_deref(), Some("withdrawn"), "the oldest made room");
        assert_eq!(s.fabric_attention_clear(w.rt, "x0", "approved", 99, false).unwrap().unwrap().resolution.as_deref(), Some("approved"));
        assert!(s.fabric_attention_clear(w.rt, "x0", "denied", 99, false).unwrap().is_none(), "already cleared");
        let keep: Vec<String> = vec!["x1".into()];
        let cleared = s.fabric_attention_clear_others(w.rt, &keep, 100).unwrap();
        assert_eq!(cleared.len(), MAX_OPEN_ATTENTION - 2);
        assert_eq!(s.fabric_attention_open().unwrap().iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["x1"]);
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| {
        assert!(s.fabric_attention_open().unwrap().is_empty());
        assert!(nf(s.fabric_attention_get("x1")));
        assert!(nf(s.fabric_attention_clear(w.rt, "x1", "denied", 1, false)));
        assert!(nf(s.fabric_attention_raise(w.rt, &attention(sid, "planted", 1))));
    });
}

#[test]
fn orders_keep_their_bytes_until_received_and_go_down_by_number() {
    let mut w = setup();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        let o = s.fabric_order_insert(&order(w.rt, "o1", 5, 10)).unwrap();
        assert_eq!((o.user_id, o.state.as_str()), (w.tom, "pending"));
        assert!(matches!(s.fabric_order_insert(&order(w.rt, "o1", 6, 10)), Err(StoreError::InvalidOp(_))));
        s.fabric_order_insert(&order(w.rt, "o2", 7, 11)).unwrap();
        assert_eq!(s.fabric_order_max_counter(w.rt, "dev").unwrap(), 7);
        assert_eq!(s.fabric_order_max_counter(w.rt, "other").unwrap(), 0);
        assert_eq!(s.fabric_orders_undelivered(w.rt).unwrap().len(), 2);
        assert!(s.fabric_down_sent(w.rt, DownKind::Order, "o1", "inst", 3, 12).unwrap());
        assert!(s.fabric_down_sent(w.rt, DownKind::Order, "o2", "inst", 4, 12).unwrap());
        assert_eq!(s.fabric_order(w.rt, "o1").unwrap().state, "sent");
        assert_eq!(s.fabric_down_max_seq(w.rt, "inst").unwrap(), 4);
        assert_eq!(s.fabric_down_received(w.rt, "inst", 3, 13).unwrap(), vec![(DownKind::Order, "o1".to_string())]);
        assert!(s.fabric_down_received(w.rt, "other-launch", 9, 13).unwrap().is_empty());
        assert_eq!(s.fabric_order(w.rt, "o1").unwrap().state, "received");
        assert_eq!(s.fabric_orders_undelivered(w.rt).unwrap().iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["o2"]);
        let done = s.fabric_order_set(w.rt, "o1", "done", None, None, None, true, 14).unwrap().unwrap();
        assert_eq!((done.state.as_str(), done.text, done.payload), ("done", None, None));
        assert!(s.fabric_order_set(w.rt, "o1", "withdrawn", None, None, Some(&["pending"]), false, 15).unwrap().is_none());
        assert_eq!(s.fabric_orders(None, None, None, 10).unwrap().len(), 2);
        assert_eq!(s.fabric_order_find("o2").unwrap().counter, 7);
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| {
        assert!(nf(s.fabric_order_find("o2")));
        assert!(nf(s.fabric_order(w.rt, "o2")));
        assert!(nf(s.fabric_orders_undelivered(w.rt)));
        assert!(nf(s.fabric_order_insert(&order(w.rt, "planted", 1, 1))));
        assert!(nf(s.fabric_down_received(w.rt, "inst", 99, 1)));
        assert!(s.fabric_orders(None, None, None, 10).unwrap().is_empty());
        // her own runtime may hold the same id: her own row
        s.fabric_order_insert(&order(w.ann_rt, "o2", 1, 1)).unwrap();
    });
}

#[test]
fn settings_move_one_version_at_a_time_per_person() {
    let mut w = setup();
    let put = |s: &mut SqliteStore, v: u64| {
        s.fabric_settings_put(&FabricSettings {
            user_id: Uuid::nil(),
            version: v,
            issued_at: 1,
            payload: vec![1],
            device_id: "dev".into(),
            signature: vec![2; 64],
            json: "{}".into(),
            updated_at: 1,
        })
    };
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        assert!(s.fabric_settings().unwrap().is_none());
        assert!(matches!(put(s, 2), Err(StoreError::InvalidOp(_))));
        put(s, 1).unwrap();
        assert!(matches!(put(s, 1), Err(StoreError::InvalidOp(_))), "stale");
        put(s, 2).unwrap();
        assert_eq!(s.fabric_settings().unwrap().unwrap().version, 2);
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| {
        assert!(s.fabric_settings().unwrap().is_none());
        put(s, 1).unwrap();
    });
    w.s.with_scope_for_test(Scope::System, |s| assert!(nf(put(s, 3))));
    w.s.with_scope_for_test(Scope::Local, |s| assert!(s.fabric_settings().unwrap().is_none()));
}

#[test]
fn text_off_deletes_a_projects_stored_text() {
    let mut w = setup();
    let sid = Uuid::now_v7();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        let mut sess = FabricSession::blank(w.rt, sid, w.tom, "taisce/main", SessionStatus::Idle, 1);
        sess.project = "taisce".into();
        sess.last_line = "zebrasecret".into();
        s.fabric_sessions_put(w.rt, &[sess]).unwrap();
        s.fabric_turn_put(w.rt, &turn(sid, "t1", 10)).unwrap();
        let mut o = order(w.rt, "o1", 1, 1);
        o.state = "done".into();
        s.fabric_order_insert(&o).unwrap();
        s.fabric_order_insert(&order(w.rt, "o2", 2, 1)).unwrap();
        let p = s.fabric_project_put("taisce", Some(true), None, 5).unwrap();
        assert!(p.text_off && !p.private_notifications);
        let t = &s.fabric_turns(w.rt, sid, None, 1).unwrap()[0];
        assert_eq!((t.prompt.as_str(), t.text_off), ("", true));
        assert_eq!(s.fabric_sessions().unwrap()[0].last_line, "");
        assert_eq!(s.fabric_order(w.rt, "o1").unwrap().text, None);
        assert!(s.fabric_order(w.rt, "o2").unwrap().payload.is_some(), "undelivered: it must still reach the Mac");
        assert!(s.fabric_project_put("taisce", None, Some(true), 6).unwrap().text_off, "None leaves it");
        assert_eq!(s.fabric_projects().unwrap().len(), 1);
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| assert!(s.fabric_projects().unwrap().is_empty()));
}

#[test]
fn pairing_offers_are_their_runtimes_owners() {
    let mut w = setup();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        let p = s.fabric_pairing_create(w.rt, "p1", &[7; 40], 10, 610).unwrap();
        assert_eq!((p.state.as_str(), p.user_id), ("pending", w.tom));
        assert!(matches!(s.fabric_pairing_create(w.rt, "p1", &[7], 10, 610), Err(StoreError::InvalidOp(_))));
        assert_eq!(s.fabric_pairings_undelivered(w.rt, 11).unwrap().len(), 1);
        assert!(s.fabric_pairings_undelivered(w.rt, 611).unwrap().is_empty(), "expired");
        let done = s.fabric_pairing_set(w.rt, "p1", "paired", Some("dev"), None, 20).unwrap().unwrap();
        assert_eq!((done.state.as_str(), done.sealed), ("paired", None));
        assert_eq!(s.fabric_pairing_find("p1").unwrap().device_id.as_deref(), Some("dev"));
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| {
        assert!(nf(s.fabric_pairing_find("p1")));
        assert!(nf(s.fabric_pairing_create(w.rt, "p2", &[1], 1, 2)));
    });
}

fn party(rt: Uuid, address: &str) -> Party {
    Party { runtime_id: rt, session_id: Uuid::now_v7(), address: address.into() }
}

fn message(from: &Party, to: &Party, conv: &str, id: &str) -> FabricMessage {
    FabricMessage {
        id: id.into(),
        user_id: Uuid::nil(),
        conversation_id: Some(conv.into()),
        from: from.clone(),
        to: to.clone(),
        to_written: to.address.clone(),
        kind: "ask".into(),
        body: Some("zebrasecret question".into()),
        doc: String::new(),
        reply_to: String::new(),
        sent_at: 10,
        read_at: None,
        state: "unread".into(),
        bounce_reason: None,
        answered: false,
        down_instance: None,
        down_seq: None,
        received_at: None,
        nudge_outcome: None,
        nudge_reason: None,
        updated_at: 10,
    }
}

#[test]
fn mail_is_read_once_by_its_receiver_and_never_crosses_people() {
    let mut w = setup();
    let (a, b) = (party(w.rt, "taisce/main"), party(w.rt, "workbox/main"));
    let anns = party(w.ann_rt, "ann/main");
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        let c = s.fabric_conversation_for(&a, &b, 10).unwrap();
        assert_eq!(s.fabric_conversation_for(&b, &a, 11).unwrap().id, c.id, "one row per pair, either way round");
        assert!(nf(s.fabric_conversation_for(&a, &anns, 11)), "not across people");
        s.fabric_message_insert(&message(&a, &b, &c.id, "m1")).unwrap();
        assert!(nf(s.fabric_message_insert(&message(&a, &anns, &c.id, "m2"))));
        let (inbox, more) = s.fabric_inbox(w.rt, b.session_id, 10).unwrap();
        assert_eq!((inbox.len(), more, inbox[0].user_id), (1, 0, w.tom));
        assert!(s.fabric_inbox(w.rt, a.session_id, 10).unwrap().0.is_empty(), "the sender's inbox is its own");
        assert!(s.fabric_inbox_ack(w.rt, a.session_id, &["m1".into()], 12).unwrap().is_empty(), "only its receiver acks it");
        let read = s.fabric_inbox_ack(w.rt, b.session_id, &["m1".into()], 12).unwrap();
        assert_eq!((read.len(), read[0].body.clone(), read[0].state.as_str()), (1, None, "read"));
        assert!(s.fabric_inbox_ack(w.rt, b.session_id, &["m1".into()], 13).unwrap().is_empty());
        let c = s.fabric_conversation_set(&c.id, 12, 12, true, 13, 13).unwrap();
        assert!(c.paused);
        assert!(s.fabric_conversations_reset(w.rt, a.session_id, 14).unwrap().is_empty(), "a paused pair waits for its person");
        s.fabric_conversation_set(&c.id, 3, 12, false, 13, 13).unwrap();
        assert_eq!(s.fabric_conversations_reset(w.rt, a.session_id, 14).unwrap()[0].count, 0);
        // a held message keeps its text; a bounce drops it
        let mut held = message(&a, &b, &c.id, "m3");
        held.state = "held".into();
        s.fabric_message_insert(&held).unwrap();
        assert_eq!(s.fabric_conversation_held(&c.id).unwrap().len(), 1);
        let gone = s.fabric_message_set("m3", "bounced", Some("stopped"), &["held"], 15).unwrap().unwrap();
        assert_eq!((gone.body, gone.bounce_reason.as_deref()), (None, Some("stopped")));
        assert_eq!(s.fabric_conversation_messages(&c.id, 10).unwrap().len(), 2);
    });
    w.s.with_scope_for_test(Scope::User(w.ann), |s| {
        assert!(s.fabric_conversations(10).unwrap().is_empty());
        assert!(nf(s.fabric_message("m1")));
        assert!(nf(s.fabric_inbox(w.rt, b.session_id, 10)));
        assert!(nf(s.fabric_inbox_ack(w.rt, b.session_id, &["m1".into()], 1)));
        assert!(nf(s.fabric_nudges_undelivered(w.rt)));
        assert!(matches!(s.fabric_messages_stale(i64::MAX), Err(StoreError::InvalidOp(_))));
    });
    w.s.with_scope_for_test(Scope::Public, |s| assert!(nf(s.fabric_conversations(10))));
}

#[test]
fn expiry_is_maintenance_and_says_which_orders_never_arrived() {
    let mut w = setup();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        s.fabric_order_insert(&order(w.rt, "late", 1, 100)).unwrap();
        s.fabric_pairing_create(w.rt, "p", &[1], 100, 700).unwrap();
        assert!(matches!(s.fabric_work_expire(10_000), Err(StoreError::InvalidOp(_))));
    });
    let (orders, pairings) = w.s.fabric_work_expire(100 + 3600).unwrap();
    assert_eq!((orders.len(), orders[0].state.as_str(), orders[0].payload.clone()), (1, "expired", None));
    assert_eq!(pairings.len(), 1);
    w.s.fabric_work_expire(100 + WORK_RETENTION + 3601).unwrap();
    assert!(w.s.fabric_orders(None, None, None, 10).unwrap().is_empty());
}

/// Review 5: under Text off no attention text survives an item's end, by
/// any path (cleared, trimmed, withdrawn by a snapshot), and an order's text
/// goes once its runtime has it.
#[test]
fn review_5_text_off_text_goes_by_every_path() {
    let mut w = setup();
    let sid = Uuid::now_v7();
    w.s.with_scope_for_test(Scope::User(w.tom), |s| {
        s.fabric_project_put("taisce", Some(true), None, 1).unwrap();
        for i in 0..=MAX_OPEN_ATTENTION {
            s.fabric_attention_raise(w.rt, &attention(sid, &format!("x{i}"), 10 + i as i64)).unwrap();
        }
        let trimmed = s.fabric_attention_get("x0").unwrap();
        assert_eq!((trimmed.resolution.as_deref(), trimmed.text.as_str(), trimmed.request.is_none()), (Some("withdrawn"), "", true), "trimmed");
        s.fabric_attention_clear_others(w.rt, &[], 200).unwrap();
        let gone = s.fabric_attention_get("x5").unwrap();
        assert_eq!((gone.text.as_str(), gone.request.is_none()), ("", true), "withdrawn by a snapshot");
        s.fabric_order_insert(&order(w.rt, "o1", 1, 1)).unwrap();
        s.fabric_down_sent(w.rt, DownKind::Order, "o1", "i", 1, 2).unwrap();
        s.fabric_down_received(w.rt, "i", 1, 3).unwrap();
        let o = s.fabric_order(w.rt, "o1").unwrap();
        assert_eq!((o.text, o.payload), (None, None), "the runtime has it");
    });
}
