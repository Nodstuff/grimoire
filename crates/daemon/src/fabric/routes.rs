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
use super::view::{Change, FleetEvent, FleetView};
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
    Json(req): Json<SnoozeReq>,
) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
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

async fn fleet_events(State(st): State<FabricState>, v: Viewer, headers: HeaderMap) -> Response {
    let (hub, user) = match gate(&st, &v, &headers) {
        Ok(g) => g,
        Err(r) => return r.into_response(),
    };
    // fail before the 200: a store that cannot answer is a 500, not an
    // empty stream
    match hub.watch(user).await {
        Ok((fleet, rx)) => {
            Sse::new(fleet_stream(fleet, rx)).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping")).into_response()
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
        .with_state(state)
}
