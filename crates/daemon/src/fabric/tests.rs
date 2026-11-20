//! Agent Fabric end to end: the real runtime channel on a loopback port,
//! the fake runtime (`taisce_runtime_proto::fake`) dialling it, and the
//! viewer routes under `require_auth` as in SERVER mode. The two-tenant
//! half is in `isolation_tests.rs`, which reuses the helpers here.

use super::grpc::{self, Pace, RuntimeService};
use super::hub::Hub;
use super::view::{AlertKind, Change, SessionAlert};
use super::routes::FabricState;
use crate::auth::{AuthConfig, AuthState, now, require_auth};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
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
pub(crate) async fn serve_channel(store: &SharedStore, hub: &Hub, pace: Pace) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let service =
        RuntimeService { hub: hub.clone(), store: store.clone(), limiter: Default::default(), trusted_proxy: false, pace };
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

async fn setup_with(pace: Pace) -> T {
    let mut raw = SqliteStore::open_in_memory().unwrap();
    let human = raw.create_principal(PrincipalKind::Human, "Tom", None).unwrap().id;
    let user = raw.auth_ensure_owner(human, "Tom", now()).unwrap().id;
    let token = crate::isolation_tests::mint(&mut raw, user, "Taisce", true);
    let store = SharedStore::new(raw);
    let (hub, alerts) = recording_hub(&store);
    let st = AuthState::new(AuthConfig::from_public_url(BASE).unwrap(), store.clone()).unwrap();
    let app = super::router(FabricState { hub: Some(hub.clone()), local_human: human })
        .layer(axum::middleware::from_fn_with_state(st, require_auth));
    let url = serve_channel(&store, &hub, pace).await;
    T { app, store, hub, user, token, alerts, url }
}

async fn setup() -> T {
    setup_with(Pace::default()).await
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

    /// A request with (or without) a JSON body, as the person's app: its
    /// status and JSON answer (null when the answer is not JSON).
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path).header("host", "localhost:7513").header("authorization", format!("Bearer {}", self.token));
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        let req = b.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
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
    assert_eq!((alerts[0].user, alerts[0].session_id.to_string().as_str(), alerts[0].address.as_str(), alerts[0].kind), (t.user, S1, "taisce/main", AlertKind::NeedsYou));
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
    assert_eq!(alerts_when(&t.alerts, 2).await.iter().map(|a| a.kind).collect::<Vec<_>>(), [AlertKind::NeedsYou, AlertKind::Finished]);

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
            state: pb::SessionStatus::Working as i32,
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

/// 500 small updates sent as fast as the runtime can: the send
/// window holds (never more than 64 unacknowledged), the hub acknowledges
/// them all, and every one of them arrives, in order.
#[tokio::test]
async fn a_burst_of_small_updates_flows_through_the_window_and_nothing_is_lost() {
    use taisce_runtime_proto::window::{ACK_EVERY, ACK_WITHIN, WINDOW};
    const UPDATES: u64 = 500;
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
        if let Change::Session(s) = &e.change
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
    assert_eq!(held, ["update 498", "update 499", "update 500"]);
    t.hub.flush().await;
    assert_eq!(t.stored(ids[2]).note, "update 500");

    // the stream drops and the runtime resumes: the Welcome says where the
    // hub got to (stored when the stream ended), the snapshot restores the rest
    fake.disconnect();
    let mut tries = 0;
    while t.store.lock(Scope::System).fabric_runtime(runtime).unwrap().last_up_seq != UPDATES {
        tries += 1;
        assert!(tries < 500, "the last event number is stored when the stream ends");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", UPDATES).await.unwrap();
    assert_eq!(fake.welcome.last_up_seq, UPDATES);
    let mut s = session(ids[0], "p/w", "working");
    s.note = "after the gap".into();
    fake.snapshot(&[s.clone(), session(ids[1], "p/w", "working"), session(ids[2], "p/w", "working")]).await.unwrap();
    // a slow trickle: each lone event is acknowledged within the coalescing
    // bound (250 ms, and scheduling), not held for fifteen more
    for i in 1..=3 {
        s.note = format!("trickle {i}");
        fake.state(&s).await.unwrap();
        let sent = std::time::Instant::now();
        fake.drained().await.unwrap();
        let took = sent.elapsed();
        assert!(took < ACK_WITHIN + Duration::from_millis(500), "acknowledged after {took:?}");
        assert_eq!(fake.acked(), UPDATES + i);
    }
}

/// A hub that sends its Welcome and then never acknowledges anything: the
/// stand-in for a stalled proxy or a broken hub.
#[derive(Clone)]
struct MuteHub;

#[tonic::async_trait]
impl taisce_runtime_proto::v1::runtime_server::Runtime for MuteHub {
    type AttachStream = tokio_stream::wrappers::ReceiverStream<Result<pb::Down, tonic::Status>>;

    async fn attach(&self, req: tonic::Request<tonic::Streaming<pb::Up>>) -> Result<tonic::Response<Self::AttachStream>, tonic::Status> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut inbound = req.into_inner();
        tokio::spawn(async move {
            let welcome = pb::Welcome { last_up_seq: 0, runtime_id: Uuid::nil().to_string(), heartbeat_secs: 20 };
            let _ = tx.send(Ok(pb::Down { seq: 0, body: Some(pb::down::Body::Welcome(welcome)) })).await;
            // read everything, acknowledge nothing
            while let Ok(Some(_)) = inbound.message().await {}
        });
        Ok(tonic::Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}

/// The window is the sender's to keep: against a hub that stops
/// acknowledging, the runtime stalls at 64 events in flight instead of
/// running on into the proxy's frame budget.
#[tokio::test]
async fn a_hub_that_stops_acknowledging_stalls_the_runtime_at_the_window() {
    use taisce_runtime_proto::window::WINDOW;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(taisce_runtime_proto::v1::runtime_server::RuntimeServer::new(MuteHub))
            .serve_with_incoming(tonic::transport::server::TcpIncoming::from(listener))
            .await
            .unwrap();
    });
    let mut fake = Fake::connect(&url, "trt_anything", "launch-1", 0).await.unwrap();
    fake.snapshot(&[session(S1, "p/w", "working")]).await.unwrap();
    let flood = async {
        for i in 0..200 {
            let mut s = session(S1, "p/w", "working");
            s.note = format!("update {i}");
            fake.state(&s).await.unwrap();
        }
    };
    assert!(tokio::time::timeout(Duration::from_millis(700), flood).await.is_err(), "200 events went out with no ack");
    assert_eq!((fake.seq(), fake.peak_in_flight(), fake.acked()), (WINDOW as u64, WINDOW, 0), "stalled exactly at the window");
    // messages outside the window still go: a stalled runtime can still say it is alive
    fake.heartbeat().await.unwrap();
}

/// THE BLOCKER'S TEST. Production's silence deadline (60 s) is not shorter
/// than its recheck (60 s), so a deadline that any wake-up of the loop
/// re-armed never fired: a Mac that slept without closing its connection
/// stayed online for ever. Here the recheck ticks four times inside one
/// silence deadline; the stream must still be ended.
#[tokio::test]
async fn silence_ends_a_stream_however_often_the_loop_wakes_for_something_else() {
    let pace = Pace { heartbeat: Duration::from_millis(200), recheck: Duration::from_millis(150) };
    let t = setup_with(pace).await;
    let (_, credential) = t.enrol("mbp");
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    fake.snapshot(&[session(S1, "taisce/main", "working")]).await.unwrap();
    // one event, so the ack timer wakes the loop too (250 ms in)
    fake.state(&session(S1, "taisce/main", "needs_you")).await.unwrap();
    let started = std::time::Instant::now();
    let ended = tokio::time::timeout(Duration::from_secs(5), fake.ended()).await.expect("the stream ends");
    assert_eq!(ended.map(|s| s.code()), Some(tonic::Code::DeadlineExceeded));
    let after = started.elapsed();
    assert!(after >= Duration::from_millis(500) && after < Duration::from_millis(2000), "ended after {after:?}, not after 600 ms of silence");
    let fleet = t.get("/api/fabric/sessions").await;
    assert_eq!(fleet["runtimes"][0]["online"], false);
}

/// What a runtime must be able to tell apart: a stream that ends with a
/// reason is not a clean close.
#[tokio::test]
async fn a_broken_message_ends_the_stream_with_a_status_never_a_clean_close() {
    let t = setup().await;
    let (runtime, credential) = t.enrol("mbp");
    // over 1 MiB: tonic refuses to decode it; the runtime is told why
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    fake.snapshot(&[]).await.unwrap();
    let huge = pb::Up {
        seq: 1,
        body: Some(pb::up::Body::State(pb::SessionState {
            session_id: S1.into(),
            address: "taisce/main".into(),
            last_line: "x".repeat(grpc::MAX_MESSAGE_BYTES + 1),
            state: pb::SessionStatus::Working as i32,
            since: 1,
            ..Default::default()
        })),
    };
    fake.send(huge).await.unwrap();
    let ended = fake.ended().await.expect("a status, not a clean end");
    assert_eq!(ended.code(), tonic::Code::InvalidArgument, "{ended:?}");
    assert!(ended.message().contains("1048576"), "{}", ended.message());
    // the same as the very first message
    let (tx, mut down) = Fake::open(&t.url, Some(&credential)).await.unwrap();
    tx.send(pb::Up {
        seq: 0,
        body: Some(pb::up::Body::Hello(pb::Hello { version: "v".repeat(grpc::MAX_MESSAGE_BYTES + 1), ..Default::default() })),
    })
    .await
    .unwrap();
    assert_eq!(down.message().await.unwrap_err().code(), tonic::Code::InvalidArgument);

    // a Hello naming this runtime in capitals, as Swift prints a UUID, is this runtime
    let (tx, mut down) = Fake::open(&t.url, Some(&credential)).await.unwrap();
    let hello = pb::Hello { runtime_id: runtime.to_string().to_uppercase(), instance_id: "launch-2".into(), version: "1".into(), last_down_seq: 0 };
    tx.send(pb::Up { seq: 0, body: Some(pb::up::Body::Hello(hello)) }).await.unwrap();
    assert!(matches!(down.message().await.unwrap().unwrap().body, Some(pb::down::Body::Welcome(_))));
    drop(tx);

    // a state the hub has no name for is shown as unknown, on every
    // reconnect, and is never fatal
    let mut fake = Fake::connect(&t.url, &credential, "launch-3", 0).await.unwrap();
    let odd = |seq| pb::Up {
        seq,
        body: Some(pb::up::Body::State(pb::SessionState {
            session_id: S2.into(),
            address: "taisce/odd".into(),
            state: 42,
            since: 1,
            ..Default::default()
        })),
    };
    let snapshot = pb::Snapshot { replaces_seq: 0, sessions: vec![match odd(0).body { Some(pb::up::Body::State(s)) => s, _ => unreachable!() }] };
    fake.send(pb::Up { seq: 0, body: Some(pb::up::Body::Snapshot(snapshot)) }).await.unwrap();
    fake.heartbeat().await.unwrap();
    let mut fleet = t.get("/api/fabric/sessions").await;
    for _ in 0..500 {
        if !fleet["sessions"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        fleet = t.get("/api/fabric/sessions").await;
    }
    assert_eq!(fleet["sessions"][0]["state"], "unknown");
    assert!(t.alerts.lock().unwrap().is_empty());

    // messages outside the window are counted: a flood of them ends the stream
    for _ in 0..40 {
        if fake.heartbeat().await.is_err() {
            break;
        }
    }
    assert_eq!(fake.ended().await.map(|s| s.code()), Some(tonic::Code::ResourceExhausted));
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
            state: pb::SessionStatus::Working as i32,
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
    // an event without a number; a session id that is not a UUID
    for bad in [state(0, S1, "taisce/main"), state(1, "pane-1", "taisce/main")] {
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
    let t = setup_with(Pace { heartbeat: Duration::from_millis(100), ..Pace::default() }).await;
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
    let path = format!("/api/fabric/sessions/{}/{S1}/snooze", Uuid::now_v7());
    let post = Request::post(&path).header("content-type", "application/json").body(Body::from(r#"{"until":4102444800}"#)).unwrap();
    assert_eq!(app.clone().oneshot(post).await.unwrap().status(), StatusCode::NOT_FOUND);
    let delete = Request::delete(&path).body(Body::empty()).unwrap();
    assert_eq!(app.clone().oneshot(delete).await.unwrap().status(), StatusCode::NOT_FOUND);
}

/// Snooze, on the routes: the person snoozes a session and it says so in
/// the fleet and on the stream; while snoozed it sends nothing; lifted, it
/// does again. A time that is not ahead, or too far, is refused.
#[tokio::test]
async fn a_person_snoozes_a_session_and_lifts_it() {
    let t = setup().await;
    let (runtime, credential) = t.enrol("mbp");
    let mut fake = Fake::connect(&t.url, &credential, "launch-1", 0).await.unwrap();
    fake.snapshot(&[session(S1, "taisce/main", "working"), session(S2, "workbox/main", "working")]).await.unwrap();
    let mut sse = t.sse().await;
    loop {
        // the snapshot is applied on the channel's task: wait for both
        let (event, _, v) = sse.next().await;
        if event == "fleet" && v["sessions"].as_array().unwrap().len() == 2 {
            break;
        }
        sse = t.sse().await;
    }
    let path = format!("/api/fabric/sessions/{runtime}/{S1}/snooze");
    let until = now() + 3600;

    // refused: not ahead, too far ahead, no body, not this person's session
    for bad in [now() - 1, now(), now() + super::routes::MAX_SNOOZE + 60] {
        let (status, out) = t.send("POST", &path, Some(json!({"until": bad}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {out}");
    }
    assert!(t.send("POST", &path, None).await.0.is_client_error());
    let elsewhere = format!("/api/fabric/sessions/{}/{S1}/snooze", Uuid::now_v7());
    assert_eq!(t.send("POST", &elsewhere, Some(json!({"until": until}))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(t.send("DELETE", &elsewhere, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(t.stored(S1).quiet_until, None);

    // snoozed: the answer, the stream, the fleet and the store agree
    let (status, out) = t.send("POST", &path, Some(json!({"until": until}))).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!((out["id"].as_str(), out["quiet_until"].as_i64(), out["state"].as_str()), (Some(S1), Some(until), Some("working")));
    let (event, _, v) = sse.next().await;
    assert_eq!((event.as_str(), v["id"].as_str(), v["quiet_until"].as_i64()), ("session", Some(S1), Some(until)));
    let fleet = t.get("/api/fabric/sessions").await;
    let quiet: Vec<(&str, Option<i64>)> =
        fleet["sessions"].as_array().unwrap().iter().map(|s| (s["id"].as_str().unwrap(), s["quiet_until"].as_i64())).collect();
    assert!(quiet.contains(&(S1, Some(until))) && quiet.contains(&(S2, None)), "still listed, and only it: {quiet:?}");
    assert_eq!(t.stored(S1).quiet_until, Some(until));

    // it comes to need its person: shown, not sent. The other one is sent
    fake.state(&session(S1, "taisce/main", "needs_you")).await.unwrap();
    let (event, _, v) = sse.next().await;
    assert_eq!((event.as_str(), v["state"].as_str(), v["quiet_until"].as_i64()), ("session", Some("needs_you"), Some(until)));
    fake.state(&session(S2, "workbox/main", "needs_you")).await.unwrap();
    let alerts = alerts_when(&t.alerts, 1).await;
    assert_eq!(alerts.iter().map(|a| a.address.as_str()).collect::<Vec<_>>(), ["workbox/main"]);

    // lifted: said on the stream, and the next change is sent
    let (status, out) = t.send("DELETE", &path, None).await;
    assert_eq!((status, out["quiet_until"].is_null()), (StatusCode::OK, true), "{out}");
    loop {
        let (event, _, v) = sse.next().await;
        if event == "session" && v["id"] == S1 && v["quiet_until"].is_null() {
            break;
        }
    }
    fake.state(&session(S1, "taisce/main", "working")).await.unwrap();
    fake.state(&session(S1, "taisce/main", "stalled")).await.unwrap();
    let alerts = alerts_when(&t.alerts, 2).await;
    assert_eq!((alerts[1].address.as_str(), alerts[1].kind), ("taisce/main", AlertKind::Stalled));
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
    assert_eq!(alerts_when(&t.alerts, 2).await.iter().map(|a| a.kind).collect::<Vec<_>>(), [AlertKind::NeedsYou, AlertKind::Finished]);
    assert_eq!(events, 4, "one headroom report and three changes");
}
