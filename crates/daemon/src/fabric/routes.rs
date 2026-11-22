//! What a person's own devices read of their fleet (SERVER mode; every
//! route answers 404 in LOCAL mode, where there is no hub).
//!
//! - `GET /api/fabric/sessions` → `{rev, runtimes: [..], sessions: [..]}`:
//!   the caller's fleet. Sessions are sorted by how much each needs its
//!   person (needs_you, stalled, finished, waiting_on_peer, working,
//!   unknown, idle, asleep, exited; `unknown` is a state a newer runtime
//!   reports that this server has no name for), the most recent change first within a state. Each
//!   runtime says whether it is `online` (its stream is up now), when it
//!   was last seen and its accounts' headroom; a session of a runtime that
//!   is not online is shown as offline by the client.
//! - `GET /api/fabric/runtimes` → `{runtimes: [..]}`: the caller's enrolled
//!   runtimes alone (a runtime that has never connected is listed too).
//! - `GET /api/fabric/stream` — Server-Sent Events. Opens with `retry:
//!   3000` and one `fleet` event (the same body as the sessions route), then
//!   one event per change: `session` (the session, whole), `session_gone`
//!   `{runtime_id, id}`, `runtime` (the runtime, whole) and `runtime_gone`
//!   `{id}` (the runtime was removed, and every session of it with it: no
//!   `session_gone` follows for each). Each event's `id:` is the fleet's `rev` after it (a counter
//!   that starts again when the daemon does). A stream
//!   that falls too far behind is ended; the client reconnects and is sent
//!   the fleet again, so there is no cursor to resume from. A `: ping`
//!   comment every 25s.
//! - `POST /api/fabric/sessions/{runtime_id}/{session_id}/snooze` with
//!   `{"until": <unix seconds>}` snoozes one session: the hub sends no
//!   notification for it before then (a visible push cannot be taken back by
//!   the app, so it is held back here). `until` must lie ahead, at most
//!   [`MAX_SNOOZE`] away. `DELETE` on the same path lifts it. Both answer the
//!   session, whole; the change is a `session` event on the stream, and so
//!   is the snooze running out. A snoozed session stays in the fleet with
//!   its `quiet_until`; nothing is hidden. A session that is not the
//!   caller's is 404.
//!
//! Who may read or snooze: the person's own app or web session. A fleet carries
//! project names and the last lines of terminal sessions, so a connector
//! token is refused (403) like the other human surfaces; a personal access
//! token never opens `/api` (401), and no MCP tool reads a fleet. A snooze
//! is the one thing these routes change, and it is the person's own
//! setting: nothing here reaches a runtime or a session. Enrolling and
//! removing a runtime is the box CLI's (`taisce fabric runtime …`).

use super::hub::Hub;
use super::record::{Reject, SignedJson};
use super::view::{Change, FleetEvent, FleetView, OrderView, PairingView, TurnView};
use crate::store_ext::with_store;
use axum::extract::Query;
use axum::routing::{delete, put};
use serde_json::Value;
use taisce_store::Scope;
use crate::viewer::Viewer;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use futures_util::stream::{self, Stream};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use taisce_store::StoreError;
use tokio::sync::broadcast;
use uuid::Uuid;

const HEARTBEAT: Duration = Duration::from_secs(25);
/// The reconnect delay sent once per stream (`retry:`).
const RETRY: Duration = Duration::from_secs(3);
/// The furthest ahead a snooze may reach: a session is history after this
/// long anyway (`SESSION_RETENTION`).
pub const MAX_SNOOZE: i64 = taisce_store::fabric::SESSION_RETENTION;

#[derive(Clone)]
pub struct FabricState {
    /// None in LOCAL mode: Agent Fabric exists only on the server.
    pub hub: Option<Hub>,
    pub local_human: Uuid,
}

impl FromRequestParts<FabricState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &FabricState) -> Result<Self, Response> {
        Self::from_parts(parts, st.hub.is_some(), st.local_human)
    }
}

fn error(status: StatusCode, msg: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": msg.to_string()}))).into_response()
}

/// Why a route refused before reading anything.
enum Refusal {
    /// LOCAL mode: there is no fleet.
    NoFleet,
    /// A connector, or an agent identity: not the person's own surface.
    NotAPerson,
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Refusal::NoFleet => error(StatusCode::NOT_FOUND, "not found"),
            Refusal::NotAPerson => crate::viewer::forbidden("a fleet is its person's, in the Taisce app: not a connector's"),
        }
    }
}

/// The guard every route runs first: SERVER mode, and the person's own
/// surface. Returns the hub and the signed-in user.
fn gate(st: &FabricState, v: &Viewer, headers: &HeaderMap) -> Result<(Hub, Uuid), Refusal> {
    let Some(hub) = &st.hub else { return Err(Refusal::NoFleet) };
    // require_auth puts a connector's principal in this header and strips
    // it for the person's own app: anything left is an agent identity
    if !v.human_surface || headers.contains_key(crate::mcp::PRINCIPAL_HEADER) {
        tracing::warn!(target: crate::auth::AUDIT, event = "refused.fleet_by_connector", user = ?v.user);
        return Err(Refusal::NotAPerson);
    }
    // SERVER mode always carries the signed-in user (LOCAL 404s above)
    let user = v.user.ok_or(Refusal::NoFleet)?;
    Ok((hub.clone(), user))
}

async fn sessions(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
    };
    match hub.fleet(user).await {
        Ok(fleet) => Json(fleet).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn runtimes(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
    };
    match hub.fleet(user).await {
        Ok(fleet) => Json(json!({"runtimes": fleet.runtimes})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(serde::Deserialize)]
struct SnoozeReq {
    /// Unix seconds: no notification for the session before then.
    until: i64,
}

/// A snooze set or lifted: the session as it now stands. One that is not
/// the caller's does not exist.
fn snoozed(result: Result<super::view::SessionView, StoreError>) -> Response {
    match result {
        Ok(session) => Json(session).into_response(),
        Err(StoreError::NotFound(_)) => error(StatusCode::NOT_FOUND, "not found"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn snooze(
    State(st): State<FabricState>,
    v: Viewer,
    headers: HeaderMap,
    Path((runtime, session)): Path<(Uuid, Uuid)>,
    body: axum::body::Bytes,
) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
    };
    // read after the gate: whoever may not snooze is told so (403, or 404
    // in LOCAL mode) whatever they sent, never what was wrong with it
    let Ok(req) = serde_json::from_slice::<SnoozeReq>(&body) else {
        return error(StatusCode::BAD_REQUEST, "until: unix seconds, as JSON {\"until\": <number>}");
    };
    let now = crate::auth::now();
    if req.until <= now || req.until - now > MAX_SNOOZE {
        return error(StatusCode::BAD_REQUEST, "until: a time ahead of now, at most 7 days away");
    }
    snoozed(hub.snooze(user, runtime, session, Some(req.until)).await)
}

async fn unsnooze(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path((runtime, session)): Path<(Uuid, Uuid)>) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
    };
    snoozed(hub.snooze(user, runtime, session, None).await)
}

fn sse(name: &str, rev: u64, data: &impl serde::Serialize) -> Event {
    Event::default()
        .id(rev.to_string())
        .event(name)
        .json_data(data)
        .unwrap_or_else(|_| Event::default().comment("unserializable fleet event"))
}

fn change_event(e: &FleetEvent) -> Event {
    match &e.change {
        Change::Session(s) => sse("session", e.rev, s),
        Change::SessionGone { runtime_id, id } => sse("session_gone", e.rev, &json!({"runtime_id": runtime_id, "id": id})),
        Change::Runtime(r) => sse("runtime", e.rev, r),
        Change::RuntimeGone { id } => sse("runtime_gone", e.rev, &json!({"id": id})),
        Change::Turn(t) => sse("turn", e.rev, t),
        Change::Attention(a) => sse("attention", e.rev, a),
        Change::AttentionCleared { id, runtime_id, session_id, resolution } => {
            sse("attention_cleared", e.rev, &json!({"id": id, "runtime_id": runtime_id, "session_id": session_id, "resolution": resolution}))
        }
        Change::Order(o) => sse("order", e.rev, o),
        Change::Settings(v) => sse("settings", e.rev, v),
        Change::Message(m) => sse("message", e.rev, m),
        Change::Conversation(c) => sse("conversation", e.rev, c),
        Change::Pairing(p) => sse("pairing", e.rev, p),
        Change::Project(p) => sse("project", e.rev, p),
    }
}

struct Watcher {
    /// The fleet's rev as the stream last told it.
    rev: u64,
    /// This person's own channel (`Hub::watch`): nothing to filter.
    rx: broadcast::Receiver<Arc<FleetEvent>>,
}

/// The next change to the fleet. Ends when the stream fell behind (the
/// client reconnects for a fresh fleet) or the hub is gone.
async fn next_change(mut w: Watcher) -> Option<(Result<Event, Infallible>, Watcher)> {
    loop {
        match w.rx.recv().await {
            Ok(e) if e.rev > w.rev => {
                w.rev = e.rev;
                return Some((Ok(change_event(&e)), w));
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn fleet_stream(
    fleet: FleetView,
    rx: broadcast::Receiver<Arc<FleetEvent>>,
) -> impl Stream<Item = Result<Event, Infallible>> + use<> {
    let watcher = Watcher { rev: fleet.rev, rx };
    stream::once(async { Ok(Event::default().retry(RETRY)) })
        .chain(stream::once(async move { Ok(sse("fleet", fleet.rev, &fleet)) }))
        .chain(stream::unfold(watcher, next_change))
}

async fn fleet_events(
    State(st): State<FabricState>,
    v: Viewer,
    who: Option<axum::Extension<crate::auth::Authenticated>>,
    headers: HeaderMap,
) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
    };
    // fail before the 200: a store that cannot answer is a 500, not an
    // empty stream
    match hub.watch(user).await {
        Ok((fleet, rx)) => {
            let stream = crate::auth::until_revoked(fleet_stream(fleet, rx), hub.0.store.clone(), who.map(|w| w.0), crate::auth::STREAM_RECHECK);
            Sse::new(stream).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping")).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub fn router(state: FabricState) -> Router {
    Router::new()
        .route("/api/fabric/sessions", get(sessions))
        .route("/api/fabric/runtimes", get(runtimes))
        .route("/api/fabric/stream", get(fleet_events))
        .route("/api/fabric/sessions/{runtime_id}/{session_id}/snooze", post(snooze).delete(unsnooze))
        .route("/api/fabric/sessions/{runtime_id}/{session_id}/follow", post(follow).delete(unfollow))
        .route("/api/fabric/sessions/{runtime_id}/{session_id}/turns", get(session_turns))
        .route("/api/fabric/turns", get(turns_since))
        .route("/api/fabric/attention", get(attention))
        .route("/api/fabric/attention/{id}", delete(dismiss))
        .route("/api/fabric/orders", get(orders).post(submit_order))
        .route("/api/fabric/orders/{id}", get(order).delete(withdraw))
        .route("/api/fabric/settings", get(settings).put(put_settings))
        .route("/api/fabric/fabrics", get(fabrics))
        .route("/api/fabric/projects", get(projects))
        .route("/api/fabric/projects/{name}", put(put_project))
        .route("/api/fabric/pairing", post(offer_pairing))
        .route("/api/fabric/pairing/{id}", get(pairing))
        .route("/api/fabric/conversations", get(conversations))
        .route("/api/fabric/conversations/{id}", get(conversation))
        .route("/api/fabric/conversations/{id}/continue", post(continue_pair))
        .route("/api/fabric/conversations/{id}/stop", post(stop_pair))
        .with_state(state)
}

// ---- Slices 2-5 (ADR 0008 §11) ----

fn rejected(r: Reject) -> Response {
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_REQUEST);
    let mut body = json!({"error": r.message, "code": r.code});
    if let Some(v) = r.version {
        body["version"] = json!(v);
    }
    (status, Json(body)).into_response()
}

fn store_error(e: StoreError) -> Response {
    match e {
        StoreError::NotFound(_) => error(StatusCode::NOT_FOUND, "not found"),
        e => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// Run the gate, then `f` with the hub and the person.
macro_rules! gated {
    ($st:expr, $v:expr, $headers:expr) => {
        match gate(&$st, &$v, &$headers) {
            Ok(g) => g,
            Err(r) => return r.into_response(),
        }
    };
}

/// A JSON body, read after the gate: whoever may not use the route is told
/// so whatever they sent.
fn body<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T, Reject> {
    serde_json::from_slice(bytes).map_err(|e| super::record::reject(400, "malformed", format!("{what}: {e}")))
}

async fn follow(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path((runtime, session)): Path<(Uuid, Uuid)>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    snoozed(hub.follow(user, runtime, session, true).await)
}

async fn unfollow(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path((runtime, session)): Path<(Uuid, Uuid)>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    snoozed(hub.follow(user, runtime, session, false).await)
}

#[derive(serde::Deserialize)]
struct Page {
    before: Option<i64>,
    /// With `before`: the turn_id (orders: the order id) of the oldest row
    /// held, which ended (was created) at `before`. The page is then the
    /// rows strictly older than that row, so same-second rows are not
    /// skipped. Without it, `before` alone is `ended < before`, as ever.
    before_id: Option<String>,
    after: Option<i64>,
    /// With `after`: the turn_id of the newest turn held (ended at `after`).
    after_id: Option<String>,
    since: Option<i64>,
    limit: Option<usize>,
    runtime_id: Option<Uuid>,
    session_id: Option<Uuid>,
}

async fn session_turns(
    State(st): State<FabricState>,
    v: Viewer,
    headers: HeaderMap,
    Path((runtime, session)): Path<(Uuid, Uuid)>,
    Query(q): Query<Page>,
) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let (before, after) = (q.before, q.after);
    if before.is_some() && after.is_some() {
        return error(StatusCode::BAD_REQUEST, "before or after, not both");
    }
    if (q.before_id.is_some() && before.is_none()) || (q.after_id.is_some() && after.is_none()) {
        return error(StatusCode::BAD_REQUEST, "before_id needs before, after_id needs after");
    }
    let (before_id, after_id) = (q.before_id, q.after_id);
    // `after`: the turns ended since, oldest first (a client topping up what
    // it holds); otherwise the newest page, newest first. With `_id` the
    // cursor is that exact turn (ADR 0008), so same-second turns survive.
    let read = move |s: &mut taisce_store::SqliteStore| match (after, after_id, before, before_id) {
        (Some(after), Some(id), _, _) => s.fabric_turns_after_id(runtime, session, after, &id, limit + 1),
        (Some(after), None, _, _) => s.fabric_turns_after(runtime, session, after, limit + 1),
        (None, _, Some(before), Some(id)) => s.fabric_turns_before(runtime, session, before, &id, limit + 1),
        (None, _, before, _) => s.fabric_turns(runtime, session, before, limit + 1),
    };
    match with_store(&hub.0.store, Scope::User(user), read).await {
        Ok(mut turns) => {
            let more = turns.len() > limit;
            turns.truncate(limit);
            Json(json!({"turns": turns.iter().map(TurnView::from).collect::<Vec<_>>(), "more": more})).into_response()
        }
        Err(e) => store_error(e),
    }
}

async fn turns_since(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Query(q): Query<Page>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let (since, limit) = (q.since.unwrap_or(0), q.limit.unwrap_or(100).clamp(1, 200));
    match with_store(&hub.0.store, Scope::User(user), move |s| s.fabric_turns_since(since, limit)).await {
        Ok(turns) => Json(json!({"turns": turns.iter().map(TurnView::from).collect::<Vec<_>>()})).into_response(),
        Err(e) => store_error(e),
    }
}

async fn attention(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.open_attention(user).await {
        Ok(items) => Json(json!({"items": items})).into_response(),
        Err(e) => store_error(e),
    }
}

async fn dismiss(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.dismiss(user, &id, crate::auth::now()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(r) => rejected(r),
    }
}

async fn submit_order(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, bytes: axum::body::Bytes) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let signed: SignedJson = match body(&bytes, "body: {payload, device_id, signature}") {
        Ok(b) => b,
        Err(r) => return rejected(r),
    };
    match hub.submit_order(user, signed, crate::auth::now()).await {
        Ok((order, true)) => (StatusCode::ACCEPTED, Json(json!({"order": order}))).into_response(),
        Ok((order, false)) => Json(json!({"order": order})).into_response(),
        Err(r) => rejected(r),
    }
}

async fn orders(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Query(q): Query<Page>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    if q.before_id.is_some() && q.before.is_none() {
        return error(StatusCode::BAD_REQUEST, "before_id needs before");
    }
    let read = move |s: &mut taisce_store::SqliteStore| match (q.before, q.before_id.as_deref()) {
        (Some(before), Some(id)) => s.fabric_orders_before(q.runtime_id, q.session_id, before, id, limit),
        (before, _) => s.fabric_orders(q.runtime_id, q.session_id, before, limit),
    };
    match with_store(&hub.0.store, Scope::User(user), read).await {
        Ok(orders) => Json(json!({"orders": orders.iter().map(OrderView::from).collect::<Vec<_>>()})).into_response(),
        Err(e) => store_error(e),
    }
}

async fn order(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match with_store(&hub.0.store, Scope::User(user), move |s| s.fabric_order_find(&id)).await {
        Ok(o) => Json(json!({"order": OrderView::from(&o)})).into_response(),
        Err(e) => store_error(e),
    }
}

async fn withdraw(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.withdraw_order(user, &id, crate::auth::now()).await {
        Ok(o) => Json(json!({"order": o})).into_response(),
        Err(r) => rejected(r),
    }
}

async fn settings(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.settings_view(user).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => store_error(e),
    }
}

async fn put_settings(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, bytes: axum::body::Bytes) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let signed: SignedJson = match body(&bytes, "body: {payload, device_id, signature}") {
        Ok(b) => b,
        Err(r) => return rejected(r),
    };
    match hub.put_settings(user, signed, crate::auth::now()).await {
        Ok(view) => Json(view).into_response(),
        Err(r) => rejected(r),
    }
}

/// The named fabrics, then each project's own, with their live sessions.
async fn fabrics(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let view = match hub.settings_view(user).await {
        Ok(v) => v,
        Err(e) => return store_error(e),
    };
    let fleets = hub.fleets();
    // live sessions only: an exited one is in no fabric now
    let all: Vec<&taisce_store::fabric::FabricSession> = fleets
        .get(&user)
        .map(|f| {
            f.runtimes
                .values()
                .flat_map(|r| r.sessions.values().map(|s| &s.row))
                .filter(|s| s.state != taisce_store::fabric::SessionStatus::Exited)
                .collect()
        })
        .unwrap_or_default();
    let member = |s: &taisce_store::fabric::FabricSession| json!({"runtime_id": s.runtime_id, "session_id": s.id, "address": s.address});
    let mut out = vec![];
    for f in view["settings"]["fabrics"].as_array().cloned().unwrap_or_default() {
        let id = f["id"].as_str().unwrap_or_default().to_string();
        let sessions: Vec<Value> = all.iter().filter(|s| s.fabric_id == id).map(|s| member(s)).collect();
        out.push(json!({"id": id, "name": f["name"], "purpose": f["purpose"], "doc_id": f["doc_id"], "doc_title": f["doc_title"],
            "project_default": false, "sessions": sessions}));
    }
    let mut projects: Vec<String> = all.iter().filter(|s| s.fabric_id.is_empty()).map(|s| s.project.clone()).collect();
    projects.sort();
    projects.dedup();
    for p in projects {
        let sessions: Vec<Value> = all.iter().filter(|s| s.fabric_id.is_empty() && s.project == p).map(|s| member(s)).collect();
        out.push(json!({"id": null, "name": p, "purpose": "", "doc_id": "", "doc_title": "", "project_default": true, "sessions": sessions}));
    }
    Json(json!({"fabrics": out})).into_response()
}

async fn projects(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.projects(user).await {
        Ok(p) => Json(json!({"projects": p})).into_response(),
        Err(e) => store_error(e),
    }
}

#[derive(serde::Deserialize)]
struct ProjectReq {
    text_off: Option<bool>,
    private_notifications: Option<bool>,
}

async fn put_project(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(name): Path<String>, bytes: axum::body::Bytes) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let req: ProjectReq = match body(&bytes, "body: {text_off?, private_notifications?}") {
        Ok(b) => b,
        Err(r) => return rejected(r),
    };
    if name.is_empty() || name.len() > 200 {
        return error(StatusCode::BAD_REQUEST, "name: 1-200 bytes");
    }
    match hub.set_project(user, &name, req.text_off, req.private_notifications, crate::auth::now()).await {
        Ok(p) => Json(p).into_response(),
        Err(r) => rejected(r),
    }
}

#[derive(serde::Deserialize)]
struct PairingReq {
    runtime_id: Uuid,
    pairing_id: String,
    sealed: String,
}

async fn offer_pairing(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, bytes: axum::body::Bytes) -> Response {
    let (hub, user) = gated!(st, v, headers);
    let req: PairingReq = match body(&bytes, "body: {runtime_id, pairing_id, sealed}") {
        Ok(b) => b,
        Err(r) => return rejected(r),
    };
    match hub.offer_pairing(user, req.runtime_id, &req.pairing_id, &req.sealed, crate::auth::now()).await {
        Ok(p) => (StatusCode::ACCEPTED, Json(p)).into_response(),
        Err(r) => rejected(r),
    }
}

async fn pairing(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match with_store(&hub.0.store, Scope::User(user), move |s| s.fabric_pairing_find(&id)).await {
        Ok(p) => Json(PairingView::from(&p)).into_response(),
        Err(e) => store_error(e),
    }
}

async fn conversations(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.conversations(user).await {
        Ok(c) => Json(json!({"conversations": c})).into_response(),
        Err(e) => store_error(e),
    }
}

async fn conversation(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<Page>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.conversation(user, &id, q.limit.unwrap_or(100).clamp(1, 500)).await {
        Ok((c, messages)) => Json(json!({"conversation": c, "messages": messages})).into_response(),
        Err(e) => store_error(e),
    }
}

async fn continue_pair(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.resolve_pair(user, &id, true, crate::auth::now()).await {
        Ok(c) => Json(c).into_response(),
        Err(r) => rejected(r),
    }
}

async fn stop_pair(State(st): State<FabricState>, v: Viewer, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (hub, user) = gated!(st, v, headers);
    match hub.resolve_pair(user, &id, false, crate::auth::now()).await {
        Ok(c) => Json(c).into_response(),
        Err(r) => rejected(r),
    }
}
