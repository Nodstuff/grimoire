//! The embedded web UI's own sign-in in SERVER mode: a first-party,
//! same-origin session cookie, opened by a passkey.
//!
//! - `POST /auth/web/begin` → a passkey challenge; `POST /auth/web/finish`
//!   verifies the assertion (the same webauthn-rs ceremony the OAuth sign-in
//!   page runs) and sets `__Host-taisce_session` (HttpOnly, Secure,
//!   SameSite=Strict, Path=/, no Domain). The value is 256 random bits; only
//!   its SHA-256 is stored (`auth_web_sessions`). Idle expiry 14 days,
//!   absolute 90 days, `last_used_at` rolled at most once a minute.
//! - `POST /auth/web/logout` revokes the session and clears the cookie.
//! - `GET /auth/web/session` says whether this browser is signed in (the UI
//!   asks it to offer Sign out; in LOCAL mode the route does not exist).
//!
//! The cookie is accepted on `/api` only — never `/mcp`, `/ws`, `/oauth/*`
//! or `/admin` — and identifies the person exactly as their Taisce app does
//! (`owner_app`: writes as their human, the same `Viewer`/`Scope`), except
//! that a GET never writes (`Viewer::web`) and push devices stay the app's.
//!
//! CSRF: SameSite=Strict, plus every cookie request that is not GET/HEAD
//! must carry `Origin` equal to the public origin AND `Taisce-CSRF: 1` (a
//! custom header no cross-site form can send and no cross-site fetch can
//! send without a preflight we never answer). Anything else is 403.

use super::passkey::{WebCeremony, json_body, json_err, limited, start_discoverable, verify_assertion};
use super::{AUDIT, AuthConfig, AuthState, Authenticated, hash_secret, now, random_token};
use crate::store_ext::with_store;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use taisce_store::Scope;
use taisce_store::auth::{WEB_SESSION_ABSOLUTE, WEB_SESSION_TOUCH_EVERY};
use webauthn_rs::prelude::PublicKeyCredential;

/// The cookie. `__Host-` makes the browser insist on Secure, Path=/ and no
/// Domain, so no sibling host can plant or read it.
pub const COOKIE: &str = "__Host-taisce_session";
/// The header every cookie-authenticated write must carry (value `1`).
pub const CSRF_HEADER: &str = "taisce-csrf";
/// The client id web sessions show in `Authenticated` and audit lines.
pub const WEB_CLIENT: &str = "taisce-web";
pub use super::passkey::CEREMONY_TTL;

pub fn router(st: AuthState) -> axum::Router {
    axum::Router::new()
        .route("/auth/web/begin", post(begin))
        .route("/auth/web/finish", post(finish))
        .route("/auth/web/logout", post(logout))
        .route("/auth/web/session", get(session))
        .with_state(st)
}

/// `Set-Cookie` for a fresh session: persistent up to the absolute end (the
/// server enforces the idle limit itself).
pub fn set_cookie(token: &str) -> String {
    format!("{COOKIE}={token}; Path=/; Max-Age={WEB_SESSION_ABSOLUTE}; Secure; HttpOnly; SameSite=Strict")
}

/// `Set-Cookie` that deletes the cookie.
pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict")
}

/// The session cookie's value, from every `Cookie` header (HTTP/2 may split
/// them). The first one wins.
pub fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Is the request's `Origin` exactly the public origin? (Browsers send it on
/// every POST/PUT/PATCH/DELETE, same-origin fetches included.)
pub fn origin_is_ours(cfg: &AuthConfig, headers: &HeaderMap) -> bool {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(cfg.base.as_str())
}

/// The CSRF rule for a cookie-authenticated, state-changing request.
pub fn csrf_ok(cfg: &AuthConfig, headers: &HeaderMap) -> bool {
    origin_is_ours(cfg, headers) && headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()) == Some("1")
}

fn is_safe(m: &Method) -> bool {
    m == Method::GET || m == Method::HEAD
}

fn forbidden_csrf() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": "forbidden: cross-site request (Origin and Taisce-CSRF required)", "code": "csrf"})),
    )
        .into_response()
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Resolve a session cookie to its person, rolling `last_used_at` (at most
/// one write a minute). The identity is the person's own, first party, as
/// their Taisce app's: `owner_app`, so writes are the human's.
pub async fn authenticate_session(st: &AuthState, token: &str) -> Option<Authenticated> {
    let hash = hash_secret(token);
    let now = now();
    crate::store_ext::with_store(&st.store, Scope::System, move |s| {
        let w = s.auth_web_session_by_hash(&hash, now).ok()??;
        if w.last_used_at <= now - WEB_SESSION_TOUCH_EVERY {
            let _ = s.auth_touch_web_session(w.id, now);
        }
        let human = s.auth_user(w.user_id).ok().flatten()?.principal_id;
        let instance_owner = s.instance_owner().ok().flatten() == Some(w.user_id);
        Some(Authenticated {
            user_id: w.user_id,
            grant_id: w.id,
            client_id: WEB_CLIENT.into(),
            principal: WEB_CLIENT.into(),
            owner_app: true,
            human,
            instance_owner,
            web_session: true,
        })
    })
    .await
}

/// `require_auth`'s cookie branch, for `/api` requests that carry no bearer
/// token: CSRF first (a refused write never reaches the store), then the
/// session. An unknown or lapsed cookie answers 401 and clears itself.
pub async fn with_session(st: &AuthState, mut req: Request, next: Next, token: String) -> Response {
    let path = req.uri().path().to_string();
    if !is_safe(req.method()) && !csrf_ok(&st.cfg, req.headers()) {
        let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
        tracing::warn!(target: AUDIT, event = "refused.csrf", path, method = %req.method(), ip);
        return forbidden_csrf();
    }
    let Some(who) = authenticate_session(st, &token).await else {
        let mut r = super::unauthorized(&st.cfg, &path, Some("invalid_token"));
        if let Ok(v) = HeaderValue::from_str(&clear_cookie()) {
            r.headers_mut().insert(header::SET_COOKIE, v);
        }
        return r;
    };
    // the person's own: no agent principal in the header slot (→ the human)
    req.headers_mut().remove(crate::mcp::PRINCIPAL_HEADER);
    req.extensions_mut().insert(who);
    next.run(req).await
}

// ---- sign-in ----

async fn begin(State(st): State<AuthState>, req: Request) -> Response {
    if limited(&st, &req) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    if !origin_is_ours(&st.cfg, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
    // a discoverable challenge: no allowCredentials, nothing about who is enrolled
    let (options, ceremony) = match start_discoverable(&st).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let added = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).add_web(WebCeremony { ip, at: now(), ceremony });
    let Some(id) = added else {
        return json_err(StatusCode::SERVICE_UNAVAILABLE, "too many sign-ins in progress; try again in a few minutes");
    };
    no_store(Json(json!({"ceremony": id, "options": options})).into_response())
}

#[derive(Deserialize)]
struct Finish {
    ceremony: String,
    credential: PublicKeyCredential,
}

async fn finish(State(st): State<AuthState>, req: Request) -> Response {
    if limited(&st, &req) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    if !origin_is_ours(&st.cfg, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
    let ua = super::passkey::device_summary(req.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok()));
    let body: Finish = match json_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // single-shot: taken out whether it verifies or not
    let ceremony = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).web.remove(&body.ceremony);
    let Some(c) = ceremony.filter(|c| now() - c.at < CEREMONY_TTL) else {
        return json_err(StatusCode::GONE, "this sign-in expired; press the button again");
    };
    let (user, credential) = match verify_assertion(&st, &body.credential, c.ceremony, WEB_CLIENT).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let token = random_token();
    let (hash, ua2) = (hash_secret(&token), ua.clone());
    let made = with_store(&st.store, Scope::System, move |s| {
        let w = s.auth_create_web_session(user, Some(credential), &hash, &ua2, now()).map_err(|e| e.to_string())?;
        let name = s.auth_user(user).ok().flatten().map(|u| u.name).unwrap_or_default();
        let _ = s.audit("web.signin", &w.id.to_string(), json!({"user": user, "device": ua2}));
        Ok::<_, String>((w, name))
    })
    .await;
    let (w, name) = match made {
        Ok(x) => x,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    tracing::info!(target: AUDIT, event = "web.signin", session = %w.id, user = %user, device = ua, ip);
    let mut r = no_store(Json(json!({"ok": true, "name": name})).into_response());
    if let Ok(v) = HeaderValue::from_str(&set_cookie(&token)) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

/// Sign out: revoke this browser's session (if any) and clear the cookie.
/// A write like any other: Origin + `Taisce-CSRF` (a cross-site page must
/// not be able to sign you out either).
async fn logout(State(st): State<AuthState>, req: Request) -> Response {
    if !csrf_ok(&st.cfg, req.headers()) {
        return forbidden_csrf();
    }
    if let Some(token) = session_cookie(req.headers()) {
        let hash = hash_secret(&token);
        let gone = with_store(&st.store, Scope::System, move |s| {
            let w = s.auth_revoke_web_session_by_hash(&hash, now()).ok().flatten()?;
            let _ = s.audit("web.signout", &w.id.to_string(), json!({"user": w.user_id}));
            Some(w)
        })
        .await;
        if let Some(w) = gone {
            tracing::info!(target: AUDIT, event = "web.signout", session = %w.id, user = %w.user_id);
        }
    }
    let mut r = no_store(Json(json!({"ok": true})).into_response());
    if let Ok(v) = HeaderValue::from_str(&clear_cookie()) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

/// Is this browser signed in? Read-only: it never rolls `last_used_at`.
async fn session(State(st): State<AuthState>, headers: HeaderMap) -> Response {
    let found = match session_cookie(&headers) {
        Some(token) => {
            let hash = hash_secret(&token);
            with_store(&st.store, Scope::System, move |s| {
                let w = s.auth_web_session_by_hash(&hash, now()).ok()??;
                s.auth_user(w.user_id).ok().flatten().map(|u| u.name)
            })
            .await
        }
        None => None,
    };
    no_store(Json(json!({"server": true, "signed_in": found.is_some(), "name": found})).into_response())
}

// ---- the box's CLI ----

/// `taisce auth list`: one line per live web session (and recently ended
/// ones, marked), never a secret.
pub fn session_lines(store: &taisce_store::SqliteStore, now: i64) -> anyhow::Result<Vec<String>> {
    let at = |t: i64| {
        chrono::DateTime::from_timestamp(t, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default()
    };
    let names: std::collections::HashMap<uuid::Uuid, String> =
        store.auth_users()?.into_iter().map(|u| (u.id, u.name)).collect();
    Ok(store
        .auth_web_sessions()?
        .into_iter()
        .map(|w| {
            let idle_end = w.last_used_at + taisce_store::auth::WEB_SESSION_IDLE;
            let state = match w.revoked_at {
                Some(r) => format!("revoked {}", at(r)),
                None if w.expires_at <= now || idle_end <= now => "expired".into(),
                None => "live".into(),
            };
            let id = w.id.to_string();
            let device = if w.user_agent.is_empty() { "unknown device" } else { w.user_agent.as_str() };
            format!(
                "web session {}  {}  {}  signed in {}  last used {}  {}",
                &id[..18.min(id.len())],
                names.get(&w.user_id).map(String::as_str).unwrap_or("?"),
                device,
                at(w.created_at),
                at(w.last_used_at),
                state
            )
        })
        .collect())
}

/// `taisce auth revoke <id>` for a web session (id or unique prefix).
pub fn revoke_session(store: &mut taisce_store::SqliteStore, key: &str, now: i64) -> anyhow::Result<Option<uuid::Uuid>> {
    let Some(w) = store.auth_revoke_web_session(key, now)? else { return Ok(None) };
    store.audit("web.revoke", &w.id.to_string(), json!({"user": w.user_id, "why": "cli"}))?;
    tracing::info!(target: AUDIT, event = "web.revoke", why = "cli", session = %w.id, user = %w.user_id);
    Ok(Some(w.id))
}

// ---- security headers ----

/// The web UI's Content-Security-Policy (SERVER mode). The built UI loads
/// only its own hashed module scripts and stylesheet; mermaid and the editor
/// set inline `style` attributes; doc images may be remote or data/blob URLs;
/// every fetch is same-origin. No eval, no wasm, no workers, no frames.
pub const UI_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data: blob: https:; font-src 'self' data:; connect-src 'self'; object-src 'none'; \
base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// SERVER mode only (LOCAL keeps today's headers): a CSP, framing refused,
/// no referrer beyond this origin, no MIME sniffing. A response that set its
/// own CSP (the nonce'd passkey pages) keeps it.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(UI_CSP));
    }
    h.entry(header::X_FRAME_OPTIONS).or_insert(HeaderValue::from_static("DENY"));
    h.entry(header::REFERRER_POLICY).or_insert(HeaderValue::from_static("same-origin"));
    h.entry("x-content-type-options").or_insert(HeaderValue::from_static("nosniff"));
    res
}
