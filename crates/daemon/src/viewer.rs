//! Who an HTTP request is for (ADR 0004).
//!
//! SERVER mode: `auth::require_auth` inserted `Authenticated`; the viewer is
//! that token's user, and the store runs in `Scope::User`. LOCAL mode (no
//! `--public-url`): the one human of this machine, `Scope::Local`. A SERVER
//! request with no identity is refused — deny by default, never `Local`.

use crate::auth::Authenticated;
use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use taisce_store::Scope;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    /// What every store call of this request runs as.
    pub scope: Scope,
    /// The person's own human principal: their direct writes and resolves.
    pub human: Uuid,
    /// The signed-in user (None in LOCAL mode).
    pub user: Option<Uuid>,
    /// LOCAL mode, or the instance owner (backups, diagnostics, memory sync).
    pub admin: bool,
    /// LOCAL mode, or the person's own app — not a connector token. Sharing
    /// and membership are human surfaces only.
    pub human_surface: bool,
}

impl Viewer {
    /// LOCAL mode's single user.
    pub fn local(human: Uuid) -> Self {
        Self { scope: Scope::Local, human, user: None, admin: true, human_surface: true }
    }

    /// A signed-in SERVER-mode user.
    pub fn from_auth(who: &Authenticated) -> Self {
        Self {
            scope: Scope::User(who.user_id),
            human: who.human,
            user: Some(who.user_id),
            admin: who.instance_owner,
            human_surface: who.owner_app,
        }
    }

    pub fn from_parts(parts: &Parts, server_mode: bool, local_human: Uuid) -> Result<Self, Response> {
        match parts.extensions.get::<Authenticated>() {
            Some(who) => Ok(Self::from_auth(who)),
            None if !server_mode => Ok(Self::local(local_human)),
            None => Err(unauthenticated()),
        }
    }

    /// 403 unless this is LOCAL mode or the instance owner.
    pub fn require_admin(&self) -> Result<(), Response> {
        if self.admin {
            Ok(())
        } else {
            Err(forbidden("only the server's owner can do this"))
        }
    }

    /// 403 unless this request comes from a person, not a connector.
    pub fn require_human_surface(&self) -> Result<(), Response> {
        if self.human_surface {
            Ok(())
        } else {
            tracing::warn!(target: crate::auth::AUDIT, event = "refused.membership_by_connector", user = ?self.user);
            Err(forbidden("sharing is a human surface: use the Taisce app"))
        }
    }
}

pub fn unauthenticated() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"error": "authentication required", "code": "unauthorized"}))).into_response()
}

pub fn forbidden(msg: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"error": format!("forbidden: {msg}"), "code": "forbidden"}))).into_response()
}

impl FromRequestParts<crate::api::ApiState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &crate::api::ApiState) -> Result<Self, Response> {
        Self::from_parts(parts, st.server_mode, st.human)
    }
}

/// The `/api` status layer: a handler that answered `200 {"error": "not
/// found: …"}` (the long-standing shape) answers 404, `forbidden: …` 403.
/// One place, so every route — and every new one — reports an invisible doc
/// exactly like a missing one. Only small JSON bodies are inspected.
pub async fn error_status(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::body::HttpBody as _;
    let res = next.run(req).await;
    if res.status() != StatusCode::OK {
        return res;
    }
    let is_json = res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    let small = res.body().size_hint().exact().is_some_and(|n| n < 4096);
    if !is_json || !small {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, 4096).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "body").into_response(),
    };
    let head = &bytes[..bytes.len().min(64)];
    let starts = |p: &str| head.starts_with(format!("{{\"error\":\"{p}").as_bytes());
    if starts("not found") {
        parts.status = StatusCode::NOT_FOUND;
    } else if starts("forbidden") {
        parts.status = StatusCode::FORBIDDEN;
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}
