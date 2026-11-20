//! What a person's own devices read of their fleet (SERVER mode; every
//! route answers 404 in LOCAL mode, where there is no hub).
//!
//! - `GET /api/fabric/sessions` → `{rev, runtimes: [..], sessions: [..]}`:
//!   the caller's fleet. Sessions are sorted by how much each needs its
//!   person (needs_you, stalled, finished, waiting_on_peer, working, idle,
//!   asleep, exited), the most recent change first within a state. Each
//!   runtime says whether it is `online` (its stream is up now), when it
//!   was last seen and its accounts' headroom; a session of a runtime that
//!   is not online is shown as offline by the client.
//! - `GET /api/fabric/runtimes` → `{runtimes: [..]}`: the caller's enrolled
//!   runtimes alone (a runtime that has never connected is listed too).
//! - `GET /api/fabric/stream` — Server-Sent Events. Opens with `retry:
//!   3000` and one `fleet` event (the same body as the sessions route), then
//!   one event per change: `session` (the session, whole), `session_gone`
//!   `{runtime_id, id}`, `runtime` (the runtime, whole) and `runtime_gone`
//!   `{id}`. Each event's `id:` is the fleet's `rev` after it (a counter
//!   that starts again when the daemon does). A stream
//!   that falls too far behind is ended; the client reconnects and is sent
//!   the fleet again, so there is no cursor to resume from. A `: ping`
//!   comment every 25s.
//!
//! Who may read: the person's own app or web session. A fleet carries
//! project names and the last lines of terminal sessions, so a connector
//! token is refused (403) like the other human surfaces; a personal access
//! token never opens `/api` (401), and no MCP tool reads a fleet. Nothing
//! here changes anything: enrolling and removing a runtime is the box CLI's
//! (`taisce fabric runtime …`).

use super::hub::{Change, FleetEvent, FleetView, Hub};
use crate::viewer::Viewer;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::StreamExt as _;
use futures_util::stream::{self, Stream};
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use uuid::Uuid;

const HEARTBEAT: Duration = Duration::from_secs(25);
/// The reconnect delay sent once per stream (`retry:`).
const RETRY: Duration = Duration::from_secs(3);

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
            Refusal::NotAPerson => crate::viewer::forbidden("a fleet is read from the Taisce app, not by a connector"),
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
    user: Uuid,
    /// The fleet's rev as the stream last told it.
    rev: u64,
    rx: broadcast::Receiver<Arc<FleetEvent>>,
}

/// The next change to this person's fleet. The channel carries everyone's:
/// only `user`'s events pass. Ends when the stream fell behind (the client
/// reconnects for a fresh fleet) or the hub is gone.
async fn next_change(mut w: Watcher) -> Option<(Result<Event, Infallible>, Watcher)> {
    loop {
        match w.rx.recv().await {
            Ok(e) if e.user == w.user && e.rev > w.rev => {
                w.rev = e.rev;
                return Some((Ok(change_event(&e)), w));
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn fleet_stream(
    user: Uuid,
    fleet: FleetView,
    rx: broadcast::Receiver<Arc<FleetEvent>>,
) -> impl Stream<Item = Result<Event, Infallible>> + use<> {
    let watcher = Watcher { user, rev: fleet.rev, rx };
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
            Sse::new(fleet_stream(user, fleet, rx)).keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping")).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub fn router(state: FabricState) -> Router {
    Router::new()
        .route("/api/fabric/sessions", get(sessions))
        .route("/api/fabric/runtimes", get(runtimes))
        .route("/api/fabric/stream", get(fleet_events))
        .with_state(state)
}
