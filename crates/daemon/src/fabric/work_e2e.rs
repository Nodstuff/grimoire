//! Slices 2-5 end to end (ADR 0008): the real runtime channel, the fake
//! runtime answering as a runtime does, a test phone signing what it sends,
//! and the viewer routes under `require_auth` as in SERVER mode.

use super::phone::{Phone, unb64};
use super::tests::{S1, S2, T, session, setup};
use super::view::AlertKind;
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;
use taisce_runtime_proto::fake::work::Handled;
use taisce_runtime_proto::fake::{Fake, ScriptAttention, ScriptSend, ScriptSession, ScriptTurn, Side};
use taisce_runtime_proto::prost::Message as _;
use taisce_runtime_proto::signing;
use taisce_runtime_proto::v1 as pb;

const S3: &str = "0196f3a0-0000-7000-8000-000000000003";
const S4: &str = "0196f3a0-0000-7000-8000-000000000004";
const WAIT: Duration = Duration::from_secs(5);

/// GET `path` until `ready` says so.
async fn until(t: &T, path: &str, ready: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..500 {
        let v = t.get(path).await;
        if ready(&v) {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{path} never got there: {}", t.get(path).await);
}

fn in_project(id: &str, address: &str, state: &str, project: &str) -> ScriptSession {
    ScriptSession { project: project.into(), ..session(id, address, state) }
}

/// A runtime online with these sessions, and `phone` paired with it.
async fn online(t: &T, name: &str, instance: &str, sessions: &[ScriptSession], phone: Option<&Phone>) -> (String, Fake) {
    let (rt, cred) = t.enrol(name);
    let mut f = Fake::connect(&t.url, &cred, instance, 0).await.unwrap();
    f.snapshot(sessions).await.unwrap();
    if let Some(p) = phone {
        f.side.pair(&p.public_key, "Test iPhone");
    }
    f.status().await.unwrap();
    f.drained().await.unwrap();
    (rt.to_string(), f)
}

/// The first alert of `kind`, once it has been raised.
async fn alert_of(t: &T, kind: AlertKind) -> super::view::SessionAlert {
    for _ in 0..500 {
        if let Some(a) = t.alerts.lock().unwrap().iter().find(|a| a.kind == kind) {
            return a.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never saw a {kind:?} alert: {:?}", t.alerts.lock().unwrap());
}

async fn next(f: &mut Fake) -> Handled {
    let d = f.next_down(WAIT).await.expect("a Down event within 5 seconds");
    f.handle(d).await.unwrap()
}

fn sent(reply: &pb::SendReply) -> (pb::Delivery, Option<&pb::Peer>) {
    let s = &reply.sent[0];
    (pb::Delivery::try_from(s.delivery).unwrap(), s.recipient.as_ref())
}

#[tokio::test]
async fn turns_are_read_back_and_text_off_keeps_them_live_only() {
    let t = setup().await;
    let (rt, mut f) = online(&t, "mbp", "l1", &[in_project(S1, "taisce/main", "working", "taisce")], None).await;
    f.turn(&ScriptTurn {
        session: S1.into(),
        turn_id: Some("t1".into()),
        prompt: "zebra prompt".into(),
        reply: "done it".into(),
        files_changed: 3,
        lines_added: 84,
        lines_removed: 12,
        pull_request_number: 214,
        tool_count: 7,
        ..Default::default()
    })
    .await
    .unwrap();
    let path = format!("/api/fabric/sessions/{rt}/{S1}/turns");
    let v = until(&t, &path, |v| v["turns"].as_array().is_some_and(|a| a.len() == 1)).await;
    let turn = &v["turns"][0];
    assert_eq!((turn["prompt"].as_str(), turn["end"].as_str(), turn["source"].as_str()), (Some("zebra prompt"), Some("clean"), Some("mac")));
    assert_eq!((turn["outcome"]["files_changed"].as_u64(), turn["outcome"]["pull_request_number"].as_u64(), turn["tool_count"].as_u64()), (Some(3), Some(214), Some(7)));
    assert_eq!(v["more"], false);
    // the fleet says the session's latest turn, and that it finished
    until(&t, "/api/fabric/sessions", |v| v["sessions"][0]["turn_id"] == "t1" && v["sessions"][0]["state"] == "finished").await;

    // Text off: what is stored loses its text at once; the live stream still shows it
    let (st, p) = t.send("PUT", "/api/fabric/projects/taisce", Some(json!({"text_off": true}))).await;
    assert_eq!((st, p["text_off"].as_bool()), (StatusCode::OK, Some(true)), "{p}");
    let v = t.get(&path).await;
    assert_eq!((v["turns"][0]["prompt"].as_str(), v["turns"][0]["text_off"].as_bool()), (Some(""), Some(true)));
    let mut sse = t.sse().await;
    assert_eq!(sse.next().await.0, "fleet");
    f.turn(&ScriptTurn { session: S1.into(), turn_id: Some("t2".into()), prompt: "zebra two".into(), reply: "ok".into(), ..Default::default() }).await.unwrap();
    loop {
        let (event, _, v) = sse.next().await;
        if event == "turn" {
            assert_eq!(v["prompt"], "zebra two", "the live view carries it");
            break;
        }
    }
    let v = until(&t, &path, |v| v["turns"].as_array().is_some_and(|a| a.len() == 2)).await;
    assert_eq!(v["turns"][0]["prompt"], "", "the store does not: {v}");
    let since = t.get("/api/fabric/turns?since=0").await;
    assert_eq!(since["turns"].as_array().unwrap().len(), 2);
    let projects = t.get("/api/fabric/projects").await;
    assert_eq!(projects["projects"][0], json!({"name": "taisce", "text_off": true, "private_notifications": false, "sessions": 1}));
}

#[tokio::test]
async fn an_order_is_checked_kept_sent_and_answered() {
    let t = setup().await;
    let mut phone = Phone::new("a");
    let s1 = ScriptSession { turn_id: "t0".into(), ..in_project(S1, "taisce/main", "idle", "taisce") };
    let (rt, mut f) = online(&t, "mbp", "l1", &[s1], Some(&phone)).await;
    let v = until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["devices"].as_array().is_some_and(|d| d.len() == 1)).await;
    assert_eq!(v["runtimes"][0]["devices"][0]["fingerprint"], signing::fingerprint(&phone.device_id));

    let post = |body: Value| t.send("POST", "/api/fabric/orders", Some(body));
    // refused before anything is kept, each with its code
    let (body, _) = Phone::new("stranger").send(&rt, S1, "hi", "t0");
    let (st, v) = post(body).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("unknown_device")), "{v}");
    let (mut body, _) = phone.send(&rt, S1, "hi", "t0");
    body["signature"] = json!(super::phone::b64(&Phone::new("stranger").sign(signing::ORDER, &unb64(body["payload"].as_str().unwrap()))));
    let (st, v) = post(body).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_signature")), "{v}");
    let (st, v) = post(phone.send(&rt, S1, "!rm -rf /", "t0").0).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("unsafe_text")), "{v}");
    let (st, v) = post(phone.send(&rt, S2, "hi", "").0).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::NOT_FOUND, Some("no_session")), "{v}");
    let (body, _) = phone.send(&rt, S1, "hi", "t0");
    let mut payload = unb64(body["payload"].as_str().unwrap());
    payload.extend([0x98, 0x06, 0x01]); // field 99, a varint: unknown
    let (st, v) = post(phone.envelope(signing::ORDER, &payload)).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::BAD_REQUEST, Some("non_canonical")), "{v}");

    // taken: kept, and sent down
    let (body, id) = phone.send(&rt, S1, "good, now write the isolation test", "t0");
    let (st, v) = post(body.clone()).await;
    assert_eq!((st, v["order"]["id"].as_str()), (StatusCode::ACCEPTED, Some(id.as_str())), "{v}");
    let (st, _) = post(body).await;
    assert_eq!(st, StatusCode::OK, "the same order again is what is held");
    phone.counter -= 10;
    let (st, v) = post(phone.send(&rt, S1, "again", "t0").0).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::CONFLICT, Some("replayed")), "{v}");
    phone.counter += 20;

    let h = next(&mut f).await;
    assert_eq!(h, Handled::Order { order_id: id.clone(), outcome: pb::Outcome::Done, reason: String::new() });
    let v = until(&t, &format!("/api/fabric/orders/{id}"), |v| v["order"]["state"] == "done").await;
    assert_eq!((v["order"]["verb"].as_str(), v["order"]["text"].as_str()), (Some("send"), Some("good, now write the isolation test")));
    // the turn it started comes back, naming it
    let turns = until(&t, &format!("/api/fabric/sessions/{rt}/{S1}/turns"), |v| v["turns"].as_array().is_some_and(|a| !a.is_empty())).await;
    assert_eq!((turns["turns"][0]["source"].as_str(), turns["turns"][0]["order_id"].as_str()), (Some("order"), Some(id.as_str())));
    // a send naming a turn the session has moved past is refused by the runtime
    let (st, _) = post(phone.send(&rt, S1, "yes", "t0").0).await;
    assert_eq!(st, StatusCode::ACCEPTED);
    match next(&mut f).await {
        Handled::Order { outcome, reason, .. } => assert_eq!((outcome, reason.as_str()), (pb::Outcome::Refused, "wrong_turn")),
        other => panic!("{other:?}"),
    }
    let audit = until(&t, &format!("/api/fabric/orders?session_id={S1}"), |v| v["orders"][0]["state"] == "refused").await;
    assert_eq!(audit["orders"].as_array().unwrap().len(), 2);
    alert_of(&t, AlertKind::OrderRefused).await;
}

#[tokio::test]
async fn orders_wait_for_a_sleeping_mac_and_come_again_until_acknowledged() {
    let t = setup().await;
    let mut phone = Phone::new("a");
    let s1 = ScriptSession { turn_id: "t0".into(), ..session(S1, "taisce/main", "idle") };
    let (rt, cred) = t.enrol("mbp");
    let mut f = Fake::connect(&t.url, &cred, "l1", 0).await.unwrap();
    f.snapshot(std::slice::from_ref(&s1)).await.unwrap();
    f.side.pair(&phone.public_key, "Test iPhone");
    f.status().await.unwrap();
    f.drained().await.unwrap();
    let rt = rt.to_string();
    until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["devices"].as_array().is_some_and(|d| d.len() == 1)).await;
    let seq = f.seq();
    let side = std::mem::replace(&mut f.side, Side::new("spare"));
    f.disconnect();
    until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["online"] == false).await;

    // the Mac sleeps: the orders wait at the hub; one is withdrawn before it goes
    let (body, kept) = phone.send(&rt, S1, "when you are back, run the tests", "t0");
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    let (body, dropped) = phone.send(&rt, S1, "never mind", "t0");
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    let (st, v) = t.send("DELETE", &format!("/api/fabric/orders/{dropped}"), None).await;
    assert_eq!((st, v["order"]["state"].as_str()), (StatusCode::OK, Some("withdrawn")), "{v}");
    assert_eq!(t.get(&format!("/api/fabric/orders/{kept}")).await["order"]["state"], "pending");

    // it wakes; the order comes down, but the stream breaks before it is handled
    let mut f = Fake::connect_as(&t.url, &cred, "l1", seq, 0, side).await.unwrap();
    f.snapshot(std::slice::from_ref(&s1)).await.unwrap();
    let d = f.next_down(WAIT).await.expect("the order");
    assert!(matches!(d.body, Some(pb::down::Body::Order(_))));
    until(&t, &format!("/api/fabric/orders/{kept}"), |v| v["order"]["state"] == "sent").await;
    let (seq, side) = (f.seq(), std::mem::replace(&mut f.side, Side::new("spare")));
    f.disconnect();
    // the same launch again: it comes again, under a fresh number
    let mut f = Fake::connect_as(&t.url, &cred, "l1", seq, 0, side).await.unwrap();
    f.snapshot(std::slice::from_ref(&s1)).await.unwrap();
    let again = f.next_down(WAIT).await.expect("the order, again");
    assert!(again.seq > d.seq, "{} after {}", again.seq, d.seq);
    let h = f.handle(again).await.unwrap();
    assert_eq!(h, Handled::Order { order_id: kept.clone(), outcome: pb::Outcome::Done, reason: String::new() });
    until(&t, &format!("/api/fabric/orders/{kept}"), |v| v["order"]["state"] == "done").await;
    assert!(f.next_down(Duration::from_millis(300)).await.is_none(), "the withdrawn one never went");
    // a reconnect that says what it handled gets nothing again
    let (seq, down, side) = (f.seq(), f.down_seq, std::mem::replace(&mut f.side, Side::new("spare")));
    f.disconnect();
    let mut f = Fake::connect_as(&t.url, &cred, "l1", seq, down, side).await.unwrap();
    f.snapshot(&[s1]).await.unwrap();
    assert!(f.next_down(Duration::from_millis(500)).await.is_none());
}

#[tokio::test]
async fn an_approval_binds_the_request_the_phone_verified() {
    let t = setup().await;
    let mut phone = Phone::new("a");
    let (rt, mut f) = online(&t, "mbp", "l1", &[in_project(S1, "taisce/deploy", "working", "taisce")], Some(&phone)).await;
    let request_id = f
        .ask(&ScriptAttention {
            session: S1.into(),
            kind: "approval".into(),
            text: "./release.sh --notarize".into(),
            tool: "Bash".into(),
            guardrail_id: "release-deploy".into(),
            guardrail_label: "Release and deploy scripts".into(),
            expires_in: 540,
            ..Default::default()
        })
        .await
        .unwrap();
    let v = until(&t, "/api/fabric/attention", |v| v["items"].as_array().is_some_and(|a| a.len() == 1)).await;
    let item = &v["items"][0];
    assert_eq!((item["kind"].as_str(), item["text"].as_str(), item["id"].as_str()), (Some("approval"), Some("./release.sh --notarize"), Some(request_id.as_str())));
    // the phone checks the runtime's signature on the exact bytes it shows
    let request = unb64(item["signed"]["request"].as_str().unwrap());
    let sig = unb64(item["signed"]["signature"].as_str().unwrap());
    signing::verify(signing::ATTENTION, &f.side.public_key(), &request, &sig).unwrap();
    assert_eq!(pb::AttentionRequest::decode(&request[..]).unwrap().text, "./release.sh --notarize");
    let a = alert_of(&t, AlertKind::Approval).await;
    assert_eq!(a.preview.as_deref(), Some("Wants to run: ./release.sh --notarize"));
    // the fleet carries it too
    assert_eq!(t.get("/api/fabric/sessions").await["attention"][0]["id"], request_id.as_str());

    let approve = |hash: Vec<u8>| {
        pb::order::What::Command(pb::Command {
            session_id: S1.into(),
            verb: pb::Verb::Approve as i32,
            request_id: request_id.clone(),
            request_hash: hash,
            ..Default::default()
        })
    };
    // approving other bytes than were shown: the runtime refuses
    let (body, _) = phone.order(&rt, approve(signing::request_hash(b"a release that is really something else").to_vec()));
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Order { outcome: pb::Outcome::Refused, ref reason, .. } if reason == "stale_request"));
    // the bytes shown: allowed, and the item is over
    let (body, _) = phone.order(&rt, approve(signing::request_hash(&request).to_vec()));
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Order { outcome: pb::Outcome::Done, .. }));
    until(&t, "/api/fabric/attention", |v| v["items"].as_array().is_some_and(|a| a.is_empty())).await;
}

#[tokio::test]
async fn a_question_is_answered_with_a_tap_and_hold_all_stops_everything() {
    let t = setup().await;
    let mut phone = Phone::new("a");
    let (rt, mut f) = online(&t, "mbp", "l1", &[in_project(S1, "qompass/mcp-dx", "needs_you", "qompass")], Some(&phone)).await;
    let request_id = f
        .ask(&ScriptAttention {
            session: S1.into(),
            kind: "question".into(),
            text: "The retry test is flaky on CI. Fix the test, or skip it?".into(),
            choices: vec![
                taisce_runtime_proto::fake::work::ScriptChoice { id: "fix".into(), label: "Fix the test".into() },
                taisce_runtime_proto::fake::work::ScriptChoice { id: "skip".into(), label: "Skip it".into() },
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    let v = until(&t, "/api/fabric/attention", |v| v["items"].as_array().is_some_and(|a| a.len() == 1)).await;
    assert_eq!(v["items"][0]["choices"][0], json!({"id": "fix", "label": "Fix the test"}));
    let q = alert_of(&t, AlertKind::Question).await;
    assert_eq!(q.choices[1]["label"], "Skip it");
    let request = unb64(v["items"][0]["signed"]["request"].as_str().unwrap());
    let (body, _) = phone.order(
        &rt,
        pb::order::What::Command(pb::Command {
            session_id: S1.into(),
            verb: pb::Verb::Answer as i32,
            text: "Fix the test".into(),
            request_id,
            request_hash: signing::request_hash(&request).to_vec(),
            choice_id: "fix".into(),
            ..Default::default()
        }),
    );
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Order { outcome: pb::Outcome::Done, .. }));
    until(&t, "/api/fabric/attention", |v| v["items"].as_array().is_some_and(|a| a.is_empty())).await;
    let turns = until(&t, &format!("/api/fabric/sessions/{rt}/{S1}/turns"), |v| v["turns"].as_array().is_some_and(|a| !a.is_empty())).await;
    assert_eq!((turns["turns"][0]["prompt"].as_str(), turns["turns"][0]["source"].as_str()), (Some("Fix the test"), Some("answer")));

    // Hold all: the runtime refuses every command until released
    let (body, _) = phone.order(&rt, pb::order::What::Hold(pb::Hold { on: true }));
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Order { outcome: pb::Outcome::Done, .. }));
    until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["held"] == true).await;
    let turn = f.side.sessions[S1].turn_id.clone();
    let (body, _) = phone.send(&rt, S1, "carry on", &turn);
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Order { outcome: pb::Outcome::Refused, ref reason, .. } if reason == "held"));
    let (body, _) = phone.order(&rt, pb::order::What::Hold(pb::Hold { on: false }));
    assert_eq!(t.send("POST", "/api/fabric/orders", Some(body)).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Order { outcome: pb::Outcome::Done, .. }));
    until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["held"] == false).await;
}

#[tokio::test]
async fn settings_reach_each_runtime_by_version() {
    let t = setup().await;
    let phone = Phone::new("a");
    let (_, mut f) = online(&t, "mbp", "l1", &[session(S1, "taisce/main", "idle")], Some(&phone)).await;
    until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["devices"].as_array().is_some_and(|d| d.len() == 1)).await;
    let v = t.get("/api/fabric/settings").await;
    assert_eq!((v["version"].as_u64(), v["defaults"].as_bool()), (Some(0), Some(true)));
    assert_eq!(v["settings"]["guardrails"][0]["id"], "force-push");

    let rail = pb::Guardrail { id: "pr-merge".into(), label: "Merge".into(), enabled: true, tools: vec!["Bash".into()], pattern: r"\bgh\s+pr\s+merge\b".into(), builtin: String::new() };
    let fabric = pb::Fabric { id: uuid::Uuid::now_v7().to_string(), name: "runtime-channel".into(), purpose: "Both ends.".into(), ..Default::default() };
    let s = pb::Settings { guardrails: vec![rail.clone()], fabrics: vec![fabric], limits: Some(pb::Limits { pair_messages: 6, ..Default::default() }), ..Default::default() };
    let (st, v) = t.send("PUT", "/api/fabric/settings", Some(phone.settings(2, s.clone()))).await;
    assert_eq!((st, v["code"].as_str(), v["version"].as_u64()), (StatusCode::CONFLICT, Some("stale_settings"), Some(0)), "{v}");
    let bad = pb::Settings { guardrails: vec![pb::Guardrail { pattern: "(unclosed".into(), ..rail }], ..s.clone() };
    let (st, v) = t.send("PUT", "/api/fabric/settings", Some(phone.settings(1, bad))).await;
    assert_eq!((st, v["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid")), "{v}");
    let (st, v) = t.send("PUT", "/api/fabric/settings", Some(phone.settings(1, s.clone()))).await;
    assert_eq!((st, v["version"].as_u64(), v["settings"]["limits"]["pair_messages"].as_u64()), (StatusCode::OK, Some(1), Some(6)), "{v}");
    assert_eq!(v["settings"]["limits"]["agent_turns_per_hour"], 20, "a limit left at 0 is its default");
    assert_eq!(t.send("PUT", "/api/fabric/settings", Some(phone.settings(1, s))).await.0, StatusCode::CONFLICT);

    assert_eq!(next(&mut f).await, Handled::Settings { version: 1, accepted: true });
    until(&t, "/api/fabric/settings", |v| v["runtimes"][0]["settings_version"] == 1).await;
    let fabrics = t.get("/api/fabric/fabrics").await;
    assert_eq!(fabrics["fabrics"][0]["name"], "runtime-channel");
}

#[tokio::test]
async fn a_phone_pairs_through_the_hub_without_the_hub_reading_it() {
    let t = setup().await;
    let phone = Phone::new("a");
    let (_, mut f) = online(&t, "mbp", "l1", &[session(S1, "taisce/main", "idle")], None).await;
    let code = f.pairing_code("taisce.example");
    let body = phone.seal(&code, "Test iPhone");
    let pairing_id = body["pairing_id"].as_str().unwrap().to_string();
    let (st, v) = t.send("POST", "/api/fabric/pairing", Some(body.clone())).await;
    assert_eq!((st, v["state"].as_str()), (StatusCode::ACCEPTED, Some("pending")), "{v}");
    assert_eq!(t.send("POST", "/api/fabric/pairing", Some(body)).await.0, StatusCode::CONFLICT);
    match next(&mut f).await {
        Handled::Pairing { outcome, device_id, .. } => assert_eq!((outcome, device_id), (pb::PairingOutcome::Paired, phone.device_id.clone())),
        other => panic!("{other:?}"),
    }
    until(&t, &format!("/api/fabric/pairing/{pairing_id}"), |v| v["state"] == "paired").await;
    until(&t, "/api/fabric/runtimes", |v| v["runtimes"][0]["devices"][0]["device_id"] == phone.device_id.as_str()).await;

    // a seal made with another secret does not open: refused, nothing paired
    let code = f.pairing_code("taisce.example");
    let forged = code.split('&').map(|p| if p.starts_with("s=") { "s=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" } else { p }).collect::<Vec<_>>().join("&");
    let body = Phone::new("mallory").seal(&forged, "Not yours");
    assert_eq!(t.send("POST", "/api/fabric/pairing", Some(body.clone())).await.0, StatusCode::ACCEPTED);
    assert!(matches!(next(&mut f).await, Handled::Pairing { outcome: pb::PairingOutcome::Invalid, .. }));
    until(&t, &format!("/api/fabric/pairing/{}", body["pairing_id"].as_str().unwrap()), |v| v["state"] == "invalid").await;
    assert_eq!(f.side.devices.len(), 1);
}

#[tokio::test]
async fn agents_message_by_exact_address_and_replies_go_back_by_id() {
    let t = setup().await;
    let (_, mut f) = online(
        &t,
        "mbp",
        "l1",
        &[in_project(S1, "taisce/main", "idle", "taisce"), in_project(S2, "workbox/main", "idle", "workbox")],
        None,
    )
    .await;
    let who = |all| pb::call_request::Tool::Who(pb::WhoRequest { all });
    let w = match f.call(S1, who(false)).await.unwrap().reply {
        Some(pb::call_reply::Reply::Who(w)) => w,
        other => panic!("{other:?}"),
    };
    assert_eq!((w.fabric.unwrap().name.as_str(), w.peers.len(), w.peers[0].me), ("taisce", 1, true));
    let w = match f.call(S1, who(true)).await.unwrap().reply {
        Some(pb::call_reply::Reply::Who(w)) => w,
        other => panic!("{other:?}"),
    };
    assert_eq!(w.peers.iter().map(|p| p.address.as_str()).collect::<Vec<_>>(), ["taisce/main", "workbox/main"], "the caller first");

    let mail = |from: &str, to: &str, kind: &str, body: &str, reply_to: &str| ScriptSend {
        from: from.into(),
        to: to.into(),
        kind: kind.into(),
        body: body.into(),
        reply_to: reply_to.into(),
        ..Default::default()
    };
    let r = f.send_mail(&mail(S1, "workbox/main", "ask", "What does a state event look like?", "")).await.unwrap();
    let (delivery, to) = sent(&r);
    assert_eq!(delivery, pb::Delivery::Queued);
    let to = to.unwrap();
    assert_eq!((to.session_id.as_str(), to.project.as_str(), to.address.as_str()), (S2, "workbox", "workbox/main"), "where it landed");
    let ask = match next(&mut f).await {
        Handled::Nudge { read, .. } => read,
        other => panic!("{other:?}"),
    };
    assert_eq!((ask.len(), ask[0].from_address.as_str(), ask[0].to_session_id.as_str(), ask[0].from_session_id.as_str()), (1, "taisce/main", S2, S1));
    // the answer goes back by id, whatever address is written
    let r = f.send_mail(&mail(S2, "somewhere/else", "reply", "Read Runtime Channel, Events.", &ask[0].message_id)).await.unwrap();
    assert_eq!((sent(&r).0, sent(&r).1.unwrap().session_id.as_str()), (pb::Delivery::Queued, S1));
    match next(&mut f).await {
        Handled::Nudge { read, .. } => assert_eq!((read[0].kind, read[0].reply_to.as_str()), (pb::MessageKind::Reply as i32, ask[0].message_id.as_str())),
        other => panic!("{other:?}"),
    }
    // read: the text is gone; who asked whom stays
    let c = until(&t, "/api/fabric/conversations", |v| v["conversations"][0]["total"] == 2).await;
    let id = c["conversations"][0]["id"].as_str().unwrap().to_string();
    let v = until(&t, &format!("/api/fabric/conversations/{id}"), |v| v["messages"].as_array().unwrap().iter().all(|m| m["state"] == "read")).await;
    assert!(v["messages"].as_array().unwrap().iter().all(|m| m["body"].is_null()), "{v}");
    assert_eq!(v["messages"][1]["answered"], true, "the ask was answered");

    // never a guess
    let r = f.send_mail(&mail(S1, "workbox/nope", "ask", "?", "")).await.unwrap();
    assert_eq!(sent(&r).0, pb::Delivery::NoSuchAddress);
    assert_eq!(r.candidates.iter().map(|p| p.address.as_str()).collect::<Vec<_>>(), ["workbox/main"]);
    let r = f.send_mail(&mail(S1, "workbox/main", "reply", "an answer to nothing", "")).await.unwrap();
    assert_eq!(sent(&r).0, pb::Delivery::NoSuchMessage);
    let r = f.send_mail(&mail(S1, "workbox", "ask", "anyone?", "")).await.unwrap();
    assert_eq!(sent(&r).0, pb::Delivery::NotANote);
    let r = f.send_mail(&mail(S1, "workbox", "note", "the contract changed: read Runtime Channel", "")).await.unwrap();
    assert_eq!((r.sent.len(), sent(&r).0), (1, pb::Delivery::Queued), "a note to the project's fabric");
    // a retried call posts one message
    let tool = pb::call_request::Tool::Send(pb::SendRequest { to: "workbox/main".into(), kind: pb::MessageKind::Note as i32, body: "once".into(), ..Default::default() });
    let a = f.call_as(S1, "retry-1", tool.clone()).await.unwrap();
    let b = f.call_as(S1, "retry-1", tool).await.unwrap();
    assert_eq!(a, b);
    let c = t.get(&format!("/api/fabric/conversations/{id}")).await;
    assert_eq!(c["conversation"]["total"], 4, "{c}");
}

#[tokio::test]
async fn two_macs_with_one_address_are_ambiguous_and_an_exited_one_is_said() {
    let t = setup().await;
    let (_, mut a) = online(&t, "mbp", "la", &[session(S1, "taisce/main", "idle")], None).await;
    let (_, mut b) = online(&t, "mini", "lb", &[session(S3, "taisce/main", "idle"), session(S4, "other/x", "idle")], None).await;
    let note = |to: &str| ScriptSend { from: S4.into(), to: to.into(), kind: "ask".into(), body: "which of you?".into(), ..Default::default() };
    let r = b.send_mail(&note("taisce/main")).await.unwrap();
    assert_eq!((sent(&r).0, r.candidates.len()), (pb::Delivery::Ambiguous, 2), "{r:?}");
    assert!(t.get("/api/fabric/conversations").await["conversations"].as_array().unwrap().is_empty(), "nothing stored");
    // one of them exits: the other is the only live one
    a.state(&session(S1, "taisce/main", "exited")).await.unwrap();
    a.drained().await.unwrap();
    until(&t, "/api/fabric/sessions", |v| v["sessions"].as_array().unwrap().iter().any(|s| s["state"] == "exited")).await;
    let r = b.send_mail(&note("taisce/main")).await.unwrap();
    assert_eq!((sent(&r).0, sent(&r).1.unwrap().session_id.as_str()), (pb::Delivery::Queued, S3));
    b.state(&session(S3, "taisce/main", "exited")).await.unwrap();
    b.drained().await.unwrap();
    until(&t, "/api/fabric/sessions", |v| v["sessions"].as_array().unwrap().iter().filter(|s| s["state"] == "exited").count() == 2).await;
    let r = b.send_mail(&note("taisce/main")).await.unwrap();
    assert_eq!(sent(&r).0, pb::Delivery::ReceiverExited);
}

#[tokio::test]
async fn a_pair_that_talks_too_long_pauses_until_its_person_says_so() {
    let t = setup().await;
    let (_, mut f) = online(&t, "mbp", "l1", &[session(S1, "taisce/main", "working"), session(S2, "workbox/main", "working")], None).await;
    let note = |n: usize| ScriptSend { from: S1.into(), to: "workbox/main".into(), kind: "note".into(), body: format!("note {n}"), ..Default::default() };
    for n in 0..12 {
        assert_eq!(sent(&f.send_mail(&note(n)).await.unwrap()).0, pb::Delivery::Queued, "note {n}");
    }
    let r = f.send_mail(&note(12)).await.unwrap();
    assert_eq!(sent(&r).0, pb::Delivery::Paused, "{r:?}");
    let v = until(&t, "/api/fabric/attention", |v| v["items"].as_array().is_some_and(|a| a.len() == 1)).await;
    assert_eq!(v["items"][0]["kind"], "pair_paused");
    let conv = v["items"][0]["conversation_id"].as_str().unwrap().to_string();
    alert_of(&t, AlertKind::PairPaused).await;
    assert_eq!(sent(&f.send_mail(&note(13)).await.unwrap()).0, pb::Delivery::Paused, "held while paused");
    // continue: the held two go, and the count starts again
    let (st, c) = t.send("POST", &format!("/api/fabric/conversations/{conv}/continue"), None).await;
    assert_eq!((st, c["paused"].as_bool(), c["count"].as_u64()), (StatusCode::OK, Some(false), Some(0)), "{c}");
    until(&t, "/api/fabric/attention", |v| v["items"].as_array().is_some_and(|a| a.is_empty())).await;
    let msgs = t.get(&format!("/api/fabric/conversations/{conv}")).await;
    assert!(msgs["messages"].as_array().unwrap().iter().all(|m| m["state"] == "unread"), "{msgs}");
    // and stop: what was held bounces to its sender
    for n in 0..12 {
        f.send_mail(&note(100 + n)).await.unwrap();
    }
    assert_eq!(sent(&f.send_mail(&note(200)).await.unwrap()).0, pb::Delivery::Paused);
    let (st, _) = t.send("POST", &format!("/api/fabric/conversations/{conv}/stop"), None).await;
    assert_eq!(st, StatusCode::OK);
    let inbox = match f.call(S1, pb::call_request::Tool::Inbox(pb::InboxRequest { limit: 50 })).await.unwrap().reply {
        Some(pb::call_reply::Reply::Inbox(i)) => i.messages,
        other => panic!("{other:?}"),
    };
    assert!(inbox.iter().any(|m| m.kind == pb::MessageKind::Bounce as i32 && m.body.contains("stopped")), "{inbox:?}");
}
