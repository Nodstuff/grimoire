//! Agent Fabric end to end: the real runtime channel on a loopback port,
//! the fake runtime (`taisce_runtime_proto::fake`) dialling it, and the
//! viewer routes under `require_auth` as in SERVER mode. The two-tenant
//! half is in `isolation_tests.rs`, which reuses the helpers here.

use super::grpc::{self, RuntimeService};
use super::hub::{Hub, SessionAlert};
use super::routes::FabricState;
use crate::auth::{AuthConfig, AuthState, now, require_auth};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt as _;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use taisce_runtime_proto::fake::{Fake, ScriptAccount, ScriptSession, Step};
use taisce_runtime_proto::v1 as pb;
use taisce_store::fabric::SessionStatus;
use taisce_store::{BlockStore, PrincipalKind, Scope, SharedStore, SqliteStore};
use tower::ServiceExt;
use uuid::Uuid;

const BASE: &str = "http://localhost:7513";

/// Serve the runtime channel for `hub` on a free loopback port; its URL.
pub(crate) async fn serve_channel(store: &SharedStore, hub: &Hub, heartbeat: Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let service =
        RuntimeService { hub: hub.clone(), store: store.clone(), limiter: Default::default(), trusted_proxy: false, heartbeat };
    tokio::spawn(async move {
        grpc::serve(listener, service, std::future::pending()).await.unwrap();
    });
    url
}

/// The alerts recorded once there are `n` of them. An alert is raised
/// after its change is published and written, so a test that has just seen
/// the change on the stream may be a moment early.
pub(crate) async fn alerts_when(alerts: &Arc<Mutex<Vec<SessionAlert>>>, n: usize) -> Vec<SessionAlert> {
    for _ in 0..500 {
        let got = alerts.lock().unwrap().clone();
        if got.len() >= n {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never saw {n} alerts: {:?}", alerts.lock().unwrap());
}

/// A hub whose alerts are recorded instead of pushed.
pub(crate) fn recording_hub(store: &SharedStore) -> (Hub, Arc<Mutex<Vec<SessionAlert>>>) {
    let alerts: Arc<Mutex<Vec<SessionAlert>>> = Default::default();
    let sink = alerts.clone();
    (Hub::new(store.clone(), Some(Arc::new(move |a| sink.lock().unwrap().push(a)))), alerts)
}

/// An open `GET /api/fabric/stream`, read one event at a time.
pub(crate) struct Sse {
    body: axum::body::BodyDataStream,
    buffer: String,
}

impl Sse {
    pub(crate) async fn open(app: &Router, request: Request<Body>) -> Sse {
        let res = app.clone().oneshot(request).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        Sse { body: res.into_body().into_data_stream(), buffer: String::new() }
    }

    /// The next named event: `(event, id, data)`. Comments and the opening
    /// `retry:` are skipped. None when nothing arrives within `wait`.
    pub(crate) async fn next_within(&mut self, wait: Duration) -> Option<(String, u64, Value)> {
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let field = |name: &str| block.lines().find_map(|l| l.strip_prefix(name)).map(|v| v.trim().to_string());
                if let (Some(event), Some(data)) = (field("event:"), field("data:")) {
                    let id = field("id:").and_then(|v| v.parse().ok()).unwrap_or(0);
                    return Some((event, id, serde_json::from_str(&data).unwrap()));
                }
            }
            let chunk = tokio::time::timeout(wait, self.body.next()).await.ok()??.unwrap();
            self.buffer.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }

    pub(crate) async fn next(&mut self) -> (String, u64, Value) {
        self.next_within(Duration::from_secs(5)).await.expect("an event within 5 seconds")
    }
}

struct T {
    app: Router,
    store: SharedStore,
    hub: Hub,
    user: Uuid,
    /// the person's own app
    token: String,
    alerts: Arc<Mutex<Vec<SessionAlert>>>,
    url: String,
}

async fn setup_with(heartbeat: Duration) -> T {
    let mut raw = SqliteStore::open_in_memory().unwrap();
    let human = raw.create_principal(PrincipalKind::Human, "Tom", None).unwrap().id;
    let user = raw.auth_ensure_owner(human, "Tom", now()).unwrap().id;
    let token = crate::isolation_tests::mint(&mut raw, user, "Taisce", true);
    let store = SharedStore::new(raw);
    let (hub, alerts) = recording_hub(&store);
    let st = AuthState::new(AuthConfig::from_public_url(BASE).unwrap(), store.clone()).unwrap();
    let app = super::router(FabricState { hub: Some(hub.clone()), local_human: human })
        .layer(axum::middleware::from_fn_with_state(st, require_auth));
    let url = serve_channel(&store, &hub, heartbeat).await;
    T { app, store, hub, user, token, alerts, url }
}

async fn setup() -> T {
    setup_with(grpc::HEARTBEAT).await
}

impl T {
    fn enrol(&self, name: &str) -> (Uuid, String) {
        let (r, secret) = super::enrol_runtime(&mut self.store.lock(Scope::System), None, name, None, now()).unwrap();
        (r.id, secret.unwrap())
    }

    fn request(&self, path: &str, token: &str) -> Request<Body> {
        Request::get(path).header("host", "localhost:7513").header("authorization", format!("Bearer {token}")).body(Body::empty()).unwrap()
    }

    async fn get(&self, path: &str) -> Value {
        let res = self.app.clone().oneshot(self.request(path, &self.token)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    async fn status(&self, path: &str, token: &str) -> StatusCode {
        self.app.clone().oneshot(self.request(path, token)).await.unwrap().status()
    }

    async fn sse(&self) -> Sse {
        Sse::open(&self.app, self.request("/api/fabric/stream", &self.token)).await
    }

    fn stored(&self, id: &str) -> taisce_store::fabric::FabricSession {
        let id = Uuid::parse_str(id).unwrap();
        self.store.lock(Scope::System).fabric_sessions().unwrap().into_iter().find(|s| s.id == id).expect("a stored session")
    }
}

fn session(id: &str, address: &str, state: &str) -> ScriptSession {
    ScriptSession { since: Some(1_800_000_000), ..ScriptSession::new(id, address, state) }
}

const S1: &str = "0196f3a0-0000-7000-8000-000000000001";
const S2: &str = "0196f3a0-0000-7000-8000-000000000002";

/// Slice 1's "done when", on the server's side: a runtime connects and its
/// sessions appear in the fleet sorted by attention; a change shows on the
/// stream; the one that needs its person is announced; a disconnect shows
/// the runtime offline with its sessions still there.
#[tokio::test]
async fn a_runtime_reports_its_sessions_and_the_viewer_watches_them() {
    let t = setup().await;
    let (runtime, credential) = t.enrol("mbp");
    // enrolled, never connected: listed, offline
    let listed = t.get("/api/fabric/runtimes").await;
    assert_eq!(listed["runtimes"].as_array().unwrap().len(), 1);
    let r = &listed["runtimes"][0];
    assert_eq!((r["id"].as_str(), r["name"].as_str(), r["online"].as_bool()), (Some(runtime.to_string().as_str()), Some("mbp"), Some(false)));
    assert!(r["last_seen_at"].is_null());

    let mut sse = t.sse().await;
    let (event, _, fleet) = sse.next().await;
    assert_eq!(event, "fleet");
    assert_eq!((fleet["runtimes"][0]["online"].as_bool(), fleet["sessions"].as_array().unwrap().len()), (Some(false), 0));

    // the runtime dials in
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    assert_eq!((fake.welcome.last_up_seq, fake.welcome.runtime_id.as_str()), (0, runtime.to_string().as_str()));
    assert_eq!(fake.welcome.heartbeat_secs, 20);
    let (event, _, r) = sse.next().await;
    assert_eq!((event.as_str(), r["online"].as_bool(), r["version"].as_str().unwrap().starts_with("fake/")), ("runtime", Some(true), true));

    // a scripted morning: two sessions, one of them comes to ask something
    let mut s1 = session(S1, "taisce/main", "working");
    s1.project = "taisce".into();
    s1.tool = "Bash".into();
    let ended = fake
        .play(&[
            Step::Snapshot(vec![s1.clone(), session(S2, "workbox/main", "idle")]),
            Step::Accounts(vec![ScriptAccount { name: "personal".into(), session_used_pct: Some(38), ..Default::default() }]),
        ])
        .await
        .unwrap();
    assert!(!ended);
    let mut seen = vec![];
    for _ in 0..3 {
        let (event, _, v) = sse.next().await;
        seen.push((event, v));
    }
    assert_eq!(seen.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), ["session", "session", "runtime"]);
    assert_eq!(seen[2].1["accounts"][0]["session_used_pct"], 38);
    let fleet = t.get("/api/fabric/sessions").await;
    let order: Vec<&str> = fleet["sessions"].as_array().unwrap().iter().map(|s| s["address"].as_str().unwrap()).collect();
    assert_eq!(order, ["taisce/main", "workbox/main"], "working before idle");
    assert_eq!(fleet["runtimes"][0]["online"], true);
    assert_eq!(fleet["runtimes"][0]["accounts"][0]["name"], "personal");
    let first = &fleet["sessions"][0];
    assert_eq!((first["id"].as_str(), first["runtime_id"].as_str()), (Some(S1), Some(runtime.to_string().as_str())));
    assert_eq!((first["state"].as_str(), first["tool"].as_str(), first["project"].as_str()), (Some("working"), Some("Bash"), Some("taisce")));

    // workbox/main starts a turn; taisce/main ends its turn with a question
    let mut asks = session(S1, "taisce/main", "needs_you");
    asks.last_line = "Shall I open the pull request?".into();
    fake.play(&[Step::State(session(S2, "workbox/main", "working").into()), Step::State(asks.clone().into())]).await.unwrap();
    let (_, _, v) = sse.next().await;
    assert_eq!((v["id"].as_str(), v["state"].as_str()), (Some(S2), Some("working")));
    let (event, rev, v) = sse.next().await;
    assert_eq!(event, "session");
    assert_eq!((v["id"].as_str(), v["state"].as_str(), v["last_line"].as_str()), (Some(S1), Some("needs_you"), Some("Shall I open the pull request?")));
    let fleet = t.get("/api/fabric/sessions").await;
    assert_eq!(fleet["sessions"][0]["id"], S1, "the one that needs you is at the top");
    assert_eq!(fleet["rev"], rev, "the stream's id is the fleet's rev");
    // announced once, by address and state; and already in SQLite
    let alerts = alerts_when(&t.alerts, 1).await;
    assert_eq!(alerts.len(), 1);
    assert_eq!((alerts[0].user, alerts[0].session_id.to_string().as_str(), alerts[0].address.as_str(), alerts[0].state), (t.user, S1, "taisce/main", SessionStatus::NeedsYou));
    assert_eq!(t.stored(S1).state, SessionStatus::NeedsYou);

    // a change that is not a change of state reaches the stream at once and
    // SQLite at the next flush
    asks.note = "waiting on Tom".into();
    fake.state(&asks).await.unwrap();
    assert_eq!(sse.next().await.2["note"], "waiting on Tom");
    assert_eq!(t.stored(S1).note, "");
    t.hub.flush().await;
    assert_eq!(t.stored(S1).note, "waiting on Tom");

    // the other finishes its turn: announced too
    fake.state(&session(S2, "workbox/main", "finished")).await.unwrap();
    assert_eq!(sse.next().await.2["state"], "finished");
    assert_eq!(alerts_when(&t.alerts, 2).await.iter().map(|a| a.state).collect::<Vec<_>>(), [SessionStatus::NeedsYou, SessionStatus::Finished]);

    // the Mac sleeps: the stream ends, the runtime is offline, the fleet stays
    let sent = fake.seq();
    assert_eq!(sent, 5, "accounts and four states");
    fake.disconnect();
    let (event, _, r) = sse.next().await;
    assert_eq!((event.as_str(), r["online"].as_bool()), ("runtime", Some(false)));
    assert!(r["last_seen_at"].as_i64().unwrap() >= now() - 5);
    let fleet = t.get("/api/fabric/sessions").await;
    assert_eq!((fleet["runtimes"][0]["online"].as_bool(), fleet["sessions"].as_array().unwrap().len()), (Some(false), 2));
    let stored = t.store.lock(Scope::System).fabric_runtime(runtime).unwrap();
    assert_eq!((stored.last_up_seq, stored.instance_id.as_deref()), (sent, Some("launch-1")));
    assert!(stored.last_seen_at.is_some());

    // it wakes and reconnects as the same launch: told where the hub got to
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", sent).await.unwrap();
    assert_eq!(fake.welcome.last_up_seq, sent);
    assert_eq!(sse.next().await.2["online"], true);
    // its snapshot no longer has workbox/main: shown as exited, kept
    fake.snapshot(&[asks.clone()]).await.unwrap();
    let (_, _, v) = sse.next().await;
    assert_eq!((v["id"].as_str(), v["state"].as_str()), (Some(S2), Some("exited")));
    // an event the hub already took is a replay and changes nothing
    let replay = pb::Up {
        seq: 2,
        body: Some(pb::up::Body::State(pb::SessionState {
            session_id: S1.into(),
            address: "taisce/main".into(),
            state: pb::Status::Working as i32,
            since: 1,
            ..Default::default()
        })),
    };
    fake.send(replay).await.unwrap();
    fake.accounts(&[ScriptAccount { name: "personal".into(), session_used_pct: Some(40), ..Default::default() }]).await.unwrap();
    let (event, _, v) = sse.next().await;
    assert_eq!((event.as_str(), v["accounts"][0]["session_used_pct"].as_i64()), ("runtime", Some(40)), "the replay published nothing");
    let fleet = t.get("/api/fabric/sessions").await;
    let states: Vec<(&str, &str)> =
        fleet["sessions"].as_array().unwrap().iter().map(|s| (s["address"].as_str().unwrap(), s["state"].as_str().unwrap())).collect();
    assert_eq!(states, [("taisce/main", "needs_you"), ("workbox/main", "exited")]);
    assert_eq!(t.alerts.lock().unwrap().len(), 2, "nothing new to announce");

    // a new launch numbers from 1 again
    fake.disconnect();
    assert_eq!(sse.next().await.2["online"], false);
    let fake = Fake::connect(&t.url, &credential, "launch-2", 0).await.unwrap();
    assert_eq!(fake.welcome.last_up_seq, 0);
}

/// Only a runtime credential opens the channel, and it opens nothing else.
#[tokio::test]
async fn the_channel_takes_only_a_runtime_credential_and_the_credential_opens_only_the_channel() {
    let t = setup().await;
    let (_, credential) = t.enrol("mbp");
    let pat = crate::auth::create_api_token(&mut t.store.lock(Scope::System), None, "claude-code", None, now()).unwrap().1.unwrap();
    let unauthenticated = |r: Result<_, taisce_runtime_proto::fake::Error>| match r {
        Err(e) => assert_eq!(e.code(), Some(tonic::Code::Unauthenticated), "{e}"),
        Ok(_) => panic!("the channel opened"),
    };
    unauthenticated(Fake::open(&t.url, None).await.map(|_| ()));
    unauthenticated(Fake::open(&t.url, Some(&pat)).await.map(|_| ()));
    unauthenticated(Fake::open(&t.url, Some(&t.token)).await.map(|_| ()));
    unauthenticated(Fake::open(&t.url, Some("trt_not-a-credential")).await.map(|_| ()));
    // the app's token, dressed as a runtime credential, is still unknown
    unauthenticated(Fake::open(&t.url, Some(&format!("trt_{}", t.token))).await.map(|_| ()));
    // the credential on the HTTP side is just an unknown token
    for path in ["/api/fabric/sessions", "/api/fabric/runtimes", "/api/fabric/stream", "/mcp", "/api/docs"] {
        assert_eq!(t.status(path, &credential).await, StatusCode::UNAUTHORIZED, "{path}");
    }
    // a removed runtime's credential stops working
    Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap().disconnect();
    super::revoke_runtime(&mut t.store.lock(Scope::System), "mbp", None, now()).unwrap();
    unauthenticated(Fake::connect(&t.url, &credential, "launch-1", 0).await.map(|_| ()));
}

/// More than 200 small updates sent as fast as the runtime can: the send
/// window holds (never more than 64 unacknowledged), the hub acknowledges
/// them all, and every one of them arrives, in order.
#[tokio::test]
async fn a_burst_of_small_updates_flows_through_the_window_and_nothing_is_lost() {
    use taisce_runtime_proto::window::{ACK_EVERY, WINDOW};
    const UPDATES: u64 = 300;
    let t = setup().await;
    let (runtime, credential) = t.enrol("mbp");
    let (_, mut changes) = t.hub.watch(t.user).await.unwrap();
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    let ids = [S1, S2, "0196f3a0-0000-7000-8000-000000000003"];
    fake.snapshot(&ids.map(|id| session(id, "p/w", "working"))).await.unwrap();
    for i in 1..=UPDATES {
        let mut s = session(ids[(i % 3) as usize], "p/w", "working");
        s.note = format!("update {i}");
        fake.state(&s).await.unwrap();
    }
    fake.drained().await.unwrap();
    assert_eq!((fake.seq(), fake.acked()), (UPDATES, UPDATES), "every event acknowledged");
    assert!(fake.peak_in_flight() <= WINDOW, "{} in flight", fake.peak_in_flight());
    assert!(fake.peak_in_flight() > ACK_EVERY, "the burst did outrun the acks: the window was exercised");
    // every update was published once, in the order sent
    let mut notes = vec![];
    while let Ok(e) = changes.try_recv() {
        if let super::hub::Change::Session(s) = &e.change
            && !s.note.is_empty()
        {
            notes.push(s.note.clone());
        }
    }
    assert_eq!(notes, (1..=UPDATES).map(|i| format!("update {i}")).collect::<Vec<_>>());
    // the fleet holds the last of each, and after the flush so does SQLite
    let fleet = t.get("/api/fabric/sessions").await;
    let mut held: Vec<&str> = fleet["sessions"].as_array().unwrap().iter().map(|s| s["note"].as_str().unwrap()).collect();
    held.sort();
    assert_eq!(held, ["update 298", "update 299", "update 300"]);
    t.hub.flush().await;
    assert_eq!(t.store.lock(Scope::System).fabric_runtime(runtime).unwrap().last_up_seq, UPDATES);
    assert_eq!(t.stored(ids[0]).note, "update 300");

    // the stream drops mid-burst and the runtime resumes: the Welcome says
    // where the hub got to, the snapshot restores the rest
    fake.disconnect();
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", UPDATES).await.unwrap();
    assert_eq!(fake.welcome.last_up_seq, UPDATES);
    let mut s = session(ids[0], "p/w", "working");
    s.note = "after the gap".into();
    fake.snapshot(&[s.clone(), session(ids[1], "p/w", "working"), session(ids[2], "p/w", "working")]).await.unwrap();
    // a lone event is acknowledged within the quarter second, not after sixteen
    s.note = "one more".into();
    fake.state(&s).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), fake.drained()).await.expect("an ack within 250 ms").unwrap();
    assert_eq!(fake.acked(), UPDATES + 1);
}

/// The contract is enforced: order, limits, one live stream, liveness.
#[tokio::test]
async fn a_stream_that_breaks_the_contract_is_ended_with_the_reason() {
    let t = setup().await;
    let (runtime, credential) = t.enrol("mbp");
    let state = |seq, id: &str, address: &str| pb::Up {
        seq,
        body: Some(pb::up::Body::State(pb::SessionState {
            session_id: id.into(),
            address: address.into(),
            state: pb::Status::Working as i32,
            since: 1,
            ..Default::default()
        })),
    };
    async fn end(mut down: tonic::Streaming<pb::Down>) -> tonic::Code {
        loop {
            match down.message().await {
                Ok(Some(_)) => continue,
                Ok(None) => return tonic::Code::Ok,
                Err(s) => return s.code(),
            }
        }
    }
    // the first message must be a Hello
    let (tx, down) = Fake::open(&t.url, Some(&credential)).await.unwrap();
    tx.send(state(1, S1, "taisce/main")).await.unwrap();
    assert_eq!(end(down).await, tonic::Code::FailedPrecondition);
    // a Hello naming another runtime
    let (tx, down) = Fake::open(&t.url, Some(&credential)).await.unwrap();
    let hello = |runtime_id: String, instance: &str| pb::Up {
        seq: 0,
        body: Some(pb::up::Body::Hello(pb::Hello { runtime_id, instance_id: instance.into(), version: "1".into(), last_down_seq: 0 })),
    };
    tx.send(hello(Uuid::now_v7().to_string(), "launch-1")).await.unwrap();
    assert_eq!(end(down).await, tonic::Code::InvalidArgument);
    // its own id is fine; an instance id with a space is not
    let (tx, down) = Fake::open(&t.url, Some(&credential)).await.unwrap();
    tx.send(hello(runtime.to_string(), "launch 1")).await.unwrap();
    assert_eq!(end(down).await, tonic::Code::InvalidArgument);

    // after the Hello, a Snapshot comes first
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    fake.send(state(1, S1, "taisce/main")).await.unwrap();
    assert_eq!(fake.ended().await.map(|s| s.code()), Some(tonic::Code::FailedPrecondition));

    // a field over its limit ends the stream and names the field; nothing of
    // the message is kept
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    fake.snapshot(&[]).await.unwrap();
    fake.send(state(1, S1, &"a".repeat(201))).await.unwrap();
    let ended = fake.ended().await.unwrap();
    assert_eq!(ended.code(), tonic::Code::InvalidArgument);
    assert!(ended.message().starts_with("address"), "{}", ended.message());
    // an event without a number; a session id that is not a UUID; an
    // unknown state
    for bad in [state(0, S1, "taisce/main"), state(1, "pane-1", "taisce/main"), {
        let mut up = state(1, S1, "taisce/main");
        if let Some(pb::up::Body::State(s)) = &mut up.body {
            s.state = 42;
        }
        up
    }] {
        let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
        fake.snapshot(&[]).await.unwrap();
        fake.send(bad).await.unwrap();
        assert_eq!(fake.ended().await.map(|s| s.code()), Some(tonic::Code::InvalidArgument));
    }
    assert!(t.get("/api/fabric/sessions").await["sessions"].as_array().unwrap().is_empty());

    // a second stream for the same runtime takes over; the first is told
    let mut first = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    first.snapshot(&[session(S1, "taisce/main", "working")]).await.unwrap();
    let mut second = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    assert_eq!(first.ended().await.map(|s| s.code()), Some(tonic::Code::Aborted));
    second.snapshot(&[session(S1, "taisce/main", "working")]).await.unwrap();
    second.heartbeat().await.unwrap();
    let fleet = t.get("/api/fabric/sessions").await;
    assert_eq!(fleet["runtimes"][0]["online"], true, "the runtime never went offline");

    // removed while connected: the stream ends, the fleet forgets it
    super::revoke_runtime(&mut t.store.lock(Scope::System), "mbp", None, now()).unwrap();
    let fleet = t.get("/api/fabric/sessions").await;
    assert!(fleet["runtimes"].as_array().unwrap().is_empty() && fleet["sessions"].as_array().unwrap().is_empty());
    assert_eq!(second.ended().await.map(|s| s.code()), Some(tonic::Code::Unauthenticated));
}

/// A runtime that goes silent is hub-observed offline after three
/// heartbeats; one that keeps saying "still here" stays online.
#[tokio::test]
async fn a_silent_stream_goes_offline_and_a_heartbeat_keeps_it() {
    let t = setup_with(Duration::from_millis(100)).await;
    let (_, credential) = t.enrol("mbp");
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    assert_eq!(fake.welcome.heartbeat_secs, 1, "never told to beat every 0 seconds");
    fake.snapshot(&[session(S1, "taisce/main", "working")]).await.unwrap();
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        fake.heartbeat().await.unwrap();
    }
    assert_eq!(t.get("/api/fabric/sessions").await["runtimes"][0]["online"], true, "600 ms of heartbeats");
    let mut sse = t.sse().await;
    assert_eq!(sse.next().await.0, "fleet");
    // silence
    assert_eq!(fake.ended().await.map(|s| s.code()), Some(tonic::Code::DeadlineExceeded));
    let (event, _, r) = sse.next().await;
    assert_eq!((event.as_str(), r["online"].as_bool()), ("runtime", Some(false)));
    let fleet = t.get("/api/fabric/sessions").await;
    assert_eq!(fleet["sessions"][0]["state"], "working", "its last state stays; offline is the runtime's");
}

/// In LOCAL mode there is no hub: every route is a 404.
#[tokio::test]
async fn local_mode_has_no_fleet() {
    let app = super::router(FabricState { hub: None, local_human: Uuid::now_v7() });
    for path in ["/api/fabric/sessions", "/api/fabric/runtimes", "/api/fabric/stream"] {
        let res = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

/// The script the `fake_runtime` example plays by default follows the
/// contract: the hub takes all of it (its pauses skipped here).
#[tokio::test]
async fn the_demo_script_plays_against_the_real_channel() {
    let t = setup().await;
    let (_, credential) = t.enrol("demo");
    let steps: Vec<Step> = taisce_runtime_proto::fake::parse_script(include_str!("../../../runtime-proto/examples/demo-script.json"))
        .unwrap()
        .into_iter()
        .filter(|s| !matches!(s, Step::SleepMs(_)))
        .collect();
    assert!(steps.len() >= 5);
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    assert!(!fake.play(&steps).await.unwrap(), "the demo stays connected");
    // a heartbeat round trip proves the stream is still up after the last step
    fake.heartbeat().await.unwrap();
    let events = fake.seq();
    let mut fleet = t.get("/api/fabric/sessions").await;
    for _ in 0..500 {
        if fleet["sessions"].as_array().unwrap().iter().any(|s| s["state"] == "finished") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        fleet = t.get("/api/fabric/sessions").await;
    }
    let states: Vec<(&str, &str)> =
        fleet["sessions"].as_array().unwrap().iter().map(|s| (s["address"].as_str().unwrap(), s["state"].as_str().unwrap())).collect();
    assert_eq!(states, [("taisce/main", "needs_you"), ("workbox/main", "finished"), ("qompass/mcp-dx", "asleep")]);
    assert_eq!((fleet["runtimes"][0]["online"].as_bool(), fleet["runtimes"][0]["accounts"].as_array().unwrap().len()), (Some(true), 2));
    assert_eq!(alerts_when(&t.alerts, 2).await.iter().map(|a| a.state).collect::<Vec<_>>(), [SessionStatus::NeedsYou, SessionStatus::Finished]);
    assert_eq!(events, 4, "one headroom report and three changes");
}
