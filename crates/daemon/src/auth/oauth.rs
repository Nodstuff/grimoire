//! The OAuth 2.1 authorization server and the protected-resource metadata.
//! Public clients only (no client secrets), authorization code + PKCE S256,
//! refresh tokens rotated on every use.

use super::ratelimit::Class;
use super::{AUDIT, AuthState, SCOPE, cimd, hash_secret, now, random_token};
use crate::store_ext::with_store;
use axum::Json;
use axum::extract::rejection::{FormRejection, JsonRejection};
use axum::extract::{Form, FromRequest as _, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use taisce_store::auth::{AuthCode, CodeOutcome, Grant, OAuthClient, RefreshOutcome};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest as _;
use std::collections::HashMap;
use webauthn_rs::prelude::Url;

/// Redirect URIs every deployment accepts: Claude's connector callback, the
/// native app's custom scheme. Loopback http on any port (Claude Code, other
/// CLI clients; RFC 8252 §7.3) is accepted by shape in `redirect_allowed`.
pub const FIXED_REDIRECTS: [&str; 3] = [
    "https://claude.ai/api/mcp/auth_callback",
    "https://claude.com/api/mcp/auth_callback",
    "ie.null.taisce:/oauth/callback",
];

pub fn router(st: AuthState) -> axum::Router {
    axum::Router::new()
        .route("/.well-known/oauth-protected-resource", get(resource_metadata_root))
        .route("/.well-known/oauth-protected-resource/mcp", get(resource_metadata_mcp))
        .route("/.well-known/oauth-authorization-server", get(server_metadata))
        // RFC 8414 path-suffixed form, which some clients (Claude Code's SDK)
        // probe for the /mcp resource: the same server, so the same document
        .route("/.well-known/oauth-authorization-server/mcp", get(server_metadata))
        // Apple's universal links and shared web credentials for the app
        .route("/.well-known/apple-app-site-association", get(apple_app_site_association))
        // (a literal, so isolation_tests' route scan sees it; = APP_HTTPS_CALLBACK_PATH)
        .route("/oauth/app-callback", get(app_callback_page))
        // anything else under .well-known (openid-configuration, …) is a JSON
        // 404, never the web UI's HTML, so a client's discovery fallback can parse it
        .route("/.well-known/{*rest}", get(well_known_missing))
        .route("/oauth/register", post(register).options(preflight))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token).options(preflight))
        .route("/oauth/revoke", post(revoke).options(preflight))
        .with_state(st)
}

/// The `apple-app-site-association` document (no auth, no redirect, JSON):
/// the app claims `/oauth/app-callback` as a universal link, so the sign-in
/// redirect reaches only the Taisce app, and may use this host's passkeys
/// (`webcredentials`). Apple's CDN fetches it from the public URL.
pub fn apple_app_site_association_json() -> Value {
    json!({
        "applinks": {
            "details": [{
                "appIDs": [super::APPLE_APP_ID],
                "components": [{"/": super::APP_HTTPS_CALLBACK_PATH, "comment": "the app's OAuth sign-in redirect"}],
            }],
        },
        "webcredentials": {"apps": [super::APPLE_APP_ID]},
    })
}

async fn apple_app_site_association() -> Response {
    let mut r = Json(apple_app_site_association_json()).into_response();
    // short-lived: a team or bundle change should reach Apple's CDN quickly
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=3600"));
    r
}

/// `/oauth/app-callback` reached in a browser: the Taisce app catches this
/// URL before it loads, so landing here means no app was there to catch it.
/// The page never echoes the query (it may carry a code).
async fn app_callback_page() -> Response {
    super::passkey::notice_page(
        "Open Taisce",
        "Open this on a device with the Taisce app",
        "This link finishes signing in to the Taisce app. It does nothing in a browser: start the sign-in again from the app.",
    )
}

async fn well_known_missing() -> Response {
    cors((StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"error": "not found"}))).into_response())
}

/// Discovery and the token endpoints are called cross-origin by browser
/// clients; none of them uses cookies, so `*` is safe.
fn cors(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type, mcp-protocol-version"),
    );
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
    res
}

async fn preflight() -> Response {
    cors(StatusCode::NO_CONTENT.into_response())
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res.headers_mut().insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    cors(res)
}

/// RFC 6749 §5.2 error body.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    no_store((status, Json(json!({"error": error, "error_description": description}))).into_response())
}

fn too_many() -> Response {
    let mut r = oauth_error(StatusCode::TOO_MANY_REQUESTS, "slow_down", "too many requests; retry later");
    r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
    r
}

fn resource_metadata(st: &AuthState, resource: String) -> Response {
    cors(
        Json(json!({
            "resource": resource,
            "authorization_servers": [st.cfg.base],
            "bearer_methods_supported": ["header"],
            "scopes_supported": [SCOPE],
            "resource_name": "Taisce",
        }))
        .into_response(),
    )
}

/// RFC 9728 for the origin (the HTTP API and the app).
async fn resource_metadata_root(State(st): State<AuthState>) -> Response {
    resource_metadata(&st, st.cfg.base.clone())
}

/// RFC 9728 §3.1 path-suffixed form for `/mcp`: `resource` is exactly the
/// URL an MCP client was configured with.
async fn resource_metadata_mcp(State(st): State<AuthState>) -> Response {
    resource_metadata(&st, st.cfg.mcp_url())
}

/// RFC 8414 authorization server metadata.
pub fn server_metadata_json(st: &AuthState) -> Value {
    let b = &st.cfg.base;
    json!({
        "issuer": b,
        "authorization_endpoint": format!("{b}/oauth/authorize"),
        "token_endpoint": format!("{b}/oauth/token"),
        "registration_endpoint": format!("{b}/oauth/register"),
        "revocation_endpoint": format!("{b}/oauth/revoke"),
        "scopes_supported": [SCOPE],
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    })
}

async fn server_metadata(State(st): State<AuthState>) -> Response {
    cors(Json(server_metadata_json(&st)).into_response())
}

// ---- redirect URIs ----

/// A redirect URI is a parseable absolute URI with no fragment.
pub fn check_redirect_shape(uri: &str) -> Result<(), String> {
    let u = Url::parse(uri).map_err(|e| format!("redirect_uri {uri:?} is not a URI: {e}"))?;
    if u.fragment().is_some() {
        return Err(format!("redirect_uri {uri:?} must not have a fragment"));
    }
    Ok(())
}

fn loopback_http(uri: &str) -> Option<Url> {
    let u = Url::parse(uri).ok()?;
    let host_ok = matches!(u.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]"));
    (u.scheme() == "http" && host_ok && u.username().is_empty() && u.password().is_none() && u.fragment().is_none())
        .then_some(u)
}

/// Is this server willing to send codes to `uri` at all?
pub fn redirect_allowed(extra: &[String], uri: &str) -> bool {
    FIXED_REDIRECTS.contains(&uri) || extra.iter().any(|e| e == uri) || loopback_http(uri).is_some()
}

/// Exact match, except a loopback redirect may differ in port (RFC 8252 §7.3).
pub fn redirect_matches(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    match (loopback_http(registered), loopback_http(requested)) {
        (Some(mut a), Some(mut b)) => {
            let _ = a.set_port(None);
            let _ = b.set_port(None);
            a == b
        }
        _ => false,
    }
}

/// Printable, trimmed, at most 80 chars.
pub fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(80).collect()
}

// ---- clients ----

/// A client by id: a registered (DCR) one from the store, or a metadata
/// document URL, fetched (or served from the cache while fresh).
pub async fn resolve_client(st: &AuthState, client_id: &str) -> Result<OAuthClient, String> {
    let id = client_id.to_string();
    let cached = with_store(&st.store, taisce_store::Scope::System, move |s| {
        if super::lapsed_app_client(s, &id) {
            // looks unknown: the app's probe re-registers on this 400
            return Ok(None);
        }
        s.oauth_client(&id)
    })
    .await
    .map_err(|e| e.to_string())?;
    if !cimd::is_cimd(client_id) {
        return match cached {
            Some(c) if c.kind == "dcr" => Ok(c),
            _ => Err("unknown client_id".into()),
        };
    }
    let now = now();
    if let Some(c) = &cached
        && c.kind == "cimd"
        && c.refresh_at.is_some_and(|t| t > now)
    {
        return Ok(c.clone());
    }
    let doc = cimd::fetch(client_id, st.cfg.cimd_allow_insecure).await?;
    let client = OAuthClient {
        client_id: doc.client_id,
        kind: "cimd".into(),
        client_name: doc.client_name,
        redirect_uris: doc.redirect_uris,
        metadata: doc.raw,
        created_at: cached.map(|c| c.created_at).unwrap_or(now),
        refresh_at: Some(now + cimd::CACHE_TTL),
    };
    let c = client.clone();
    with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_upsert_client(&c)).await.map_err(|e| e.to_string())?;
    tracing::info!(target: AUDIT, event = "client.cimd_fetch", client = client.client_id, name = client.client_name);
    Ok(client)
}

/// RFC 7591 dynamic registration, public clients only: a request for any
/// token_endpoint_auth_method but `none` is refused (invalid_client_metadata).
async fn register(State(st): State<AuthState>, req: Request) -> Response {
    let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
    if !st.limiter.allow(Class::Register, &ip) {
        return too_many();
    }
    let body: Result<Json<Value>, JsonRejection> = Json::<Value>::from_request(req, &()).await;
    let Ok(Json(body)) = body else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "body must be a JSON object");
    };
    let uris: Vec<String> = match body.get("redirect_uris").and_then(Value::as_array) {
        Some(a) if !a.is_empty() && a.len() <= 10 => match a.iter().map(|v| v.as_str().map(str::to_string)).collect() {
            Some(v) => v,
            None => return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "redirect_uris must be strings"),
        },
        _ => return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "1-10 redirect_uris required"),
    };
    // the app's universal link passes here; that only the app's own
    // registration may carry it is checked below (`app_registration`)
    let app_https = st.cfg.app_https_redirect();
    for u in &uris {
        if let Err(e) = check_redirect_shape(u) {
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", &e);
        }
        if !redirect_allowed(&st.cfg.extra_redirects, u) && *u != app_https {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_redirect_uri",
                &format!("redirect_uri {u:?} is not allowed by this server"),
            );
        }
    }
    let subset = |key: &str, allowed: &[&str]| -> bool {
        match body.get(key) {
            None | Some(Value::Null) => true,
            Some(Value::Array(a)) => a.iter().all(|v| v.as_str().is_some_and(|s| allowed.contains(&s))),
            _ => false,
        }
    };
    match body.get("token_endpoint_auth_method") {
        None | Some(Value::Null) => {}
        Some(Value::String(m)) if m == "none" => {}
        Some(m) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                &format!("token_endpoint_auth_method {m} is not supported: this server registers public clients only (\"none\", with PKCE)"),
            );
        }
    }
    if !subset("grant_types", &["authorization_code", "refresh_token"]) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "grant_types: authorization_code, refresh_token only");
    }
    if !subset("response_types", &["code"]) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "response_types: code only");
    }
    // the app's redirects are first party: its exact registrations (the
    // custom scheme alone, as every shipped build sends; the universal link
    // alone; or both, as newer builds send) get the fixed, server-registered
    // app client; any other use of either is refused (ADR 0004, round 2)
    let app_registration = super::app_registration(&uris, &app_https);
    if app_registration == Some(false) {
        tracing::warn!(target: AUDIT, event = "client.register_refused", why = "app redirect", ip);
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "that redirect is reserved for the Taisce app");
    }
    let now = now();
    if app_registration == Some(true) {
        let https = app_https.clone();
        let ok = with_store(&st.store, taisce_store::Scope::System, move |s| super::ensure_first_party(s, now, &https)).await;
        if let Err(e) = ok {
            return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string());
        }
        tracing::info!(target: AUDIT, event = "client.register_app", client = super::FIRST_PARTY_APP_CLIENT, ip);
        return no_store(
            (
                StatusCode::CREATED,
                Json(json!({
                    "client_id": super::FIRST_PARTY_APP_CLIENT,
                    "client_id_issued_at": now,
                    "client_name": "Taisce",
                    "redirect_uris": [super::APP_REDIRECT_URI, app_https],
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"],
                    "token_endpoint_auth_method": "none",
                })),
            )
                .into_response(),
        );
    }
    let name = clean_name(body.get("client_name").and_then(Value::as_str).unwrap_or(""));
    let name = if name.is_empty() { "OAuth client".to_string() } else { name };
    let client = OAuthClient {
        client_id: format!("dcr_{}", &random_token()[..24]),
        kind: "dcr".into(),
        client_name: name,
        redirect_uris: uris,
        metadata: body.to_string(),
        created_at: now,
        refresh_at: None,
    };
    let c = client.clone();
    if let Err(e) = with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_upsert_client(&c)).await {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string());
    }
    tracing::info!(target: AUDIT, event = "client.register", client = client.client_id, name = client.client_name, ip);
    no_store(
        (
            StatusCode::CREATED,
            Json(json!({
                "client_id": client.client_id,
                "client_id_issued_at": now,
                "client_name": client.client_name,
                "redirect_uris": client.redirect_uris,
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            })),
        )
            .into_response(),
    )
}

// ---- authorize ----

#[derive(Deserialize, Default)]
pub struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
}

/// `uri` with query parameters appended (works for custom schemes too).
pub fn redirect_with(uri: &str, params: &[(&str, &str)]) -> String {
    match Url::parse(uri) {
        Ok(mut u) => {
            {
                let mut q = u.query_pairs_mut();
                for (k, v) in params {
                    q.append_pair(k, v);
                }
            }
            u.to_string()
        }
        Err(_) => uri.to_string(),
    }
}

fn error_redirect(st: &AuthState, redirect: &str, state: Option<&str>, error: &str, description: &str) -> Response {
    let mut p = vec![("error", error), ("error_description", description), ("iss", st.cfg.base.as_str())];
    if let Some(s) = state {
        p.push(("state", s));
    }
    found(&redirect_with(redirect, &p))
}

pub fn found(location: &str) -> Response {
    let mut r = StatusCode::FOUND.into_response();
    if let Ok(v) = HeaderValue::from_str(location) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn pkce_challenge_ok(c: &str) -> bool {
    (43..=128).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn pkce_verifier_ok(v: &str) -> bool {
    (43..=128).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// RFC 7636 S256: BASE64URL(SHA256(verifier)) == challenge.
pub fn pkce_s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// The authorization endpoint. Errors about the client or its redirect are
/// shown on a page (never redirected: the redirect is what is in doubt);
/// everything after that goes back to the client as `?error=`.
async fn authorize(State(st): State<AuthState>, headers: HeaderMap, Query(q): Query<AuthorizeQuery>, req: Request) -> Response {
    let ip = super::client_ip(&st.cfg, &headers, req.extensions());
    if !st.limiter.allow(Class::Login, &ip) {
        return super::passkey::error_page(StatusCode::TOO_MANY_REQUESTS, "Too many attempts. Wait a minute and try again.");
    }
    let Some(client_id) = q.client_id.as_deref().filter(|s| !s.is_empty()) else {
        return super::passkey::error_page(StatusCode::BAD_REQUEST, "The sign-in request names no client (client_id missing).");
    };
    let client = match resolve_client(&st, client_id).await {
        Ok(c) => c,
        Err(e) => return super::passkey::error_page(StatusCode::BAD_REQUEST, &format!("Unknown or invalid client: {e}")),
    };
    let redirect = match q.redirect_uri.as_deref() {
        Some(r) => {
            if !client.redirect_uris.iter().any(|reg| redirect_matches(reg, r)) {
                return super::passkey::error_page(StatusCode::BAD_REQUEST, "redirect_uri does not match the client's registration.");
            }
            r.to_string()
        }
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        None => return super::passkey::error_page(StatusCode::BAD_REQUEST, "redirect_uri is required."),
    };
    // the app's universal link goes only to the fixed app client: DCR can't
    // register it for anyone else, and a CIMD document listing it is refused here
    let app_link = redirect == st.cfg.app_https_redirect();
    if app_link && client.client_id != super::FIRST_PARTY_APP_CLIENT {
        tracing::warn!(target: AUDIT, event = "authorize.refused", why = "app link for another client", client = client.client_id, ip);
        return super::passkey::error_page(StatusCode::BAD_REQUEST, "This server does not send sign-ins to that redirect_uri.");
    }
    if !redirect_allowed(&st.cfg.extra_redirects, &redirect) && !app_link {
        return super::passkey::error_page(StatusCode::BAD_REQUEST, "This server does not send sign-ins to that redirect_uri.");
    }
    let state = q.state.as_deref();
    if q.response_type.as_deref() != Some("code") {
        return error_redirect(&st, &redirect, state, "unsupported_response_type", "response_type must be code");
    }
    match (q.code_challenge.as_deref(), q.code_challenge_method.as_deref()) {
        (Some(c), Some("S256")) if pkce_challenge_ok(c) => {}
        (Some(_), Some("S256")) => return error_redirect(&st, &redirect, state, "invalid_request", "malformed code_challenge"),
        _ => return error_redirect(&st, &redirect, state, "invalid_request", "PKCE with code_challenge_method=S256 is required"),
    }
    if let Some(r) = q.resource.as_deref()
        && !st.cfg.resource_ok(r)
    {
        return error_redirect(&st, &redirect, state, "invalid_target", "resource is not served here");
    }
    let has_passkey = with_store(&st.store, taisce_store::Scope::System, |s| s.auth_credentials(None).map(|c| !c.is_empty()).unwrap_or(false)).await;
    if !has_passkey {
        return super::passkey::error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "No passkey is enrolled yet. On the server, run `taisce auth enroll` and open the link it prints.",
        );
    }
    let pending = super::passkey::AuthzRequest {
        client_id: client.client_id.clone(),
        client_name: client.client_name.clone(),
        redirect_uri: redirect,
        state: q.state.clone(),
        code_challenge: q.code_challenge.clone().unwrap_or_default(),
        resource: q.resource.clone(),
        scope: SCOPE.into(),
        created: now(),
        ip: ip.clone(),
        ceremony: None,
    };
    let Some(id) = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).add_authz(pending) else {
        return super::passkey::error_page(StatusCode::SERVICE_UNAVAILABLE, "Too many sign-ins in progress. Try again in a few minutes.");
    };
    let cid = client.client_id.clone();
    let first_party = with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_is_first_party(&cid).unwrap_or(false)).await;
    super::passkey::login_page(
        &id,
        &super::passkey::LoginContext {
            client_name: &client.client_name,
            client_id: &client.client_id,
            first_party,
            user_agent: headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()),
            at: now(),
        },
    )
}

/// After the passkey: mint the single-use code and the redirect carrying it.
pub async fn issue_code(
    st: &AuthState,
    req: &super::passkey::AuthzRequest,
    user_id: uuid::Uuid,
    credential: uuid::Uuid,
) -> Result<String, String> {
    let code = random_token();
    let row = AuthCode {
        code_hash: hash_secret(&code),
        client_id: req.client_id.clone(),
        user_id,
        redirect_uri: req.redirect_uri.clone(),
        code_challenge: req.code_challenge.clone(),
        resource: req.resource.clone(),
        scope: req.scope.clone(),
        expires_at: now() + super::CODE_TTL,
        credential_id: Some(credential),
    };
    with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_insert_code(&row)).await.map_err(|e| e.to_string())?;
    tracing::info!(target: AUDIT, event = "code.issue", client = req.client_id, user = %user_id);
    let mut p = vec![("code", code.as_str()), ("iss", st.cfg.base.as_str())];
    if let Some(s) = req.state.as_deref() {
        p.push(("state", s));
    }
    Ok(redirect_with(&req.redirect_uri, &p))
}

/// The redirect for a user who declined.
pub fn denied_redirect(st: &AuthState, req: &super::passkey::AuthzRequest) -> String {
    let mut p = vec![("error", "access_denied"), ("error_description", "the user declined"), ("iss", st.cfg.base.as_str())];
    if let Some(s) = req.state.as_deref() {
        p.push(("state", s));
    }
    redirect_with(&req.redirect_uri, &p)
}

// ---- token ----

fn token_response(access: &str, refresh: &str) -> Response {
    no_store(
        Json(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": super::ACCESS_TTL,
            "refresh_token": refresh,
            "scope": SCOPE,
        }))
        .into_response(),
    )
}

async fn token(State(st): State<AuthState>, req: Request) -> Response {
    let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
    if !st.limiter.allow(Class::Token, &ip) {
        return too_many();
    }
    let form: Result<Form<HashMap<String, String>>, FormRejection> =
        Form::<HashMap<String, String>>::from_request(req, &()).await;
    let Ok(Form(f)) = form else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "body must be application/x-www-form-urlencoded");
    };
    let get = |k: &str| f.get(k).map(String::as_str).filter(|s| !s.is_empty());
    let Some(client_id) = get("client_id") else {
        return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "client_id is required (public client)");
    };
    {
        let id = client_id.to_string();
        if with_store(&st.store, taisce_store::Scope::System, move |s| super::lapsed_app_client(s, &id)).await {
            return oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "unknown client_id");
        }
    }
    if let Some(r) = get("resource")
        && !st.cfg.resource_ok(r)
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_target", "resource is not served here");
    }
    match get("grant_type") {
        Some("authorization_code") => {
            let (Some(code), Some(verifier)) = (get("code"), get("code_verifier")) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "code and code_verifier are required");
            };
            code_grant(&st, client_id, code, verifier, get("redirect_uri"), get("resource"), &ip).await
        }
        Some("refresh_token") => {
            let Some(refresh) = get("refresh_token") else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "refresh_token is required");
            };
            refresh_grant(&st, client_id, refresh, &ip).await
        }
        Some(_) => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type", "authorization_code or refresh_token"),
        None => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "grant_type is required"),
    }
}

async fn code_grant(
    st: &AuthState,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: Option<&str>,
    resource: Option<&str>,
    ip: &str,
) -> Response {
    let code_hash = hash_secret(code);
    let now = now();
    let h = code_hash.clone();
    let outcome = match with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_consume_code(&h, now)).await {
        Ok(o) => o,
        Err(e) => return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string()),
    };
    let c = match outcome {
        CodeOutcome::Fresh(c) => c,
        CodeOutcome::Replayed { grant_id } => {
            tracing::warn!(target: AUDIT, event = "code.replay", client = client_id, grant = ?grant_id, ip, "authorization code replayed: grant revoked");
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "authorization code already used");
        }
        CodeOutcome::Invalid => return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "authorization code is invalid or expired"),
    };
    if c.client_id != client_id {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "code was issued to another client");
    }
    if redirect_uri.is_some_and(|r| r != c.redirect_uri) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "redirect_uri does not match the authorization request");
    }
    if !pkce_verifier_ok(verifier) || pkce_s256(verifier) != c.code_challenge {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "PKCE verification failed");
    }
    let grant = Grant {
        id: uuid::Uuid::now_v7(),
        client_id: c.client_id.clone(),
        user_id: c.user_id,
        resource: resource.map(str::to_string).or(c.resource.clone()),
        scope: c.scope.clone(),
        created_at: now,
        revoked_at: None,
        revoke_why: None,
        // deleting the passkey that signed this in revokes the grant
        credential_id: c.credential_id,
    };
    let (access, refresh) = (random_token(), random_token());
    let (ah, rh, g) = (hash_secret(&access), hash_secret(&refresh), grant.clone());
    let res = with_store(&st.store, taisce_store::Scope::System, move |s| {
        s.oauth_issue_grant(Some(&code_hash), &g, &ah, now + super::ACCESS_TTL, &rh, now + super::REFRESH_TTL)
    })
    .await;
    if let Err(e) = res {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string());
    }
    tracing::info!(target: AUDIT, event = "token.issue", client = client_id, grant = %grant.id, user = %grant.user_id, ip);
    token_response(&access, &refresh)
}

async fn refresh_grant(st: &AuthState, client_id: &str, refresh: &str, ip: &str) -> Response {
    let now = now();
    let (access, next) = (random_token(), random_token());
    let (old, ah, rh) = (hash_secret(refresh), hash_secret(&access), hash_secret(&next));
    let outcome = with_store(&st.store, taisce_store::Scope::System, move |s| {
        s.oauth_rotate_refresh(&old, now, &ah, now + super::ACCESS_TTL, &rh, now + super::REFRESH_TTL)
    })
    .await;
    match outcome {
        Ok(RefreshOutcome::Rotated(g)) if g.client_id == client_id => {
            tracing::info!(target: AUDIT, event = "token.refresh", client = client_id, grant = %g.id, ip);
            token_response(&access, &next)
        }
        Ok(RefreshOutcome::Reissued(g)) if g.client_id == client_id => {
            // a retry inside the grace window: the lost successor is replaced
            tracing::info!(target: AUDIT, event = "token.reissue", client = client_id, grant = %g.id, ip);
            token_response(&access, &next)
        }
        Ok(RefreshOutcome::Rotated(g) | RefreshOutcome::Reissued(g)) => {
            // another client holding this client's refresh token: stolen
            let gid = g.id.to_string();
            let _ = with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_revoke_grant(&gid, "refresh token presented by another client", now)).await;
            tracing::warn!(target: AUDIT, event = "token.revoke", why = "client mismatch", client = client_id, grant = %g.id, ip);
            oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "refresh token was issued to another client")
        }
        Ok(RefreshOutcome::Reused { grant_id }) => {
            tracing::warn!(target: AUDIT, event = "token.revoke", why = "refresh reuse", client = client_id, grant = %grant_id, ip);
            oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "refresh token already used; the grant is revoked")
        }
        Ok(RefreshOutcome::Invalid) => oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "refresh token is invalid, expired or revoked"),
        Err(e) => oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string()),
    }
}

/// RFC 7009: always 200, whether or not the token was live.
async fn revoke(State(st): State<AuthState>, req: Request) -> Response {
    let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
    if !st.limiter.allow(Class::Token, &ip) {
        return too_many();
    }
    let form: Result<Form<HashMap<String, String>>, FormRejection> =
        Form::<HashMap<String, String>>::from_request(req, &()).await;
    let Some(tok) = form.ok().and_then(|Form(f)| f.get("token").cloned()) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "token is required");
    };
    let h = hash_secret(&tok);
    let now = now();
    if let Ok(Some(g)) = with_store(&st.store, taisce_store::Scope::System, move |s| s.oauth_revoke_by_token(&h, "revoked by client", now)).await {
        tracing::info!(target: AUDIT, event = "token.revoke", why = "client request", grant = %g, ip);
    }
    no_store(StatusCode::OK.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_policy() {
        let none: Vec<String> = vec![];
        for ok in [
            "https://claude.ai/api/mcp/auth_callback",
            "https://claude.com/api/mcp/auth_callback",
            "ie.null.taisce:/oauth/callback",
            "http://127.0.0.1:33418/callback",
            "http://localhost:6274/oauth/callback",
            "http://localhost/cb",
        ] {
            assert!(redirect_allowed(&none, ok), "{ok}");
        }
        for bad in [
            "https://evil.example/cb",
            "https://claude.ai/api/mcp/auth_callback/x",
            "http://127.0.0.1.evil.example/cb",
            "https://127.0.0.1/cb",
            "http://10.0.0.1/cb",
            "ie.null.taisce:/other",
            "javascript:alert(1)",
        ] {
            assert!(!redirect_allowed(&none, bad), "{bad}");
        }
        assert!(redirect_allowed(&["https://app.example/cb".into()], "https://app.example/cb"));
    }

    #[test]
    fn redirect_matching_is_exact_except_loopback_port() {
        assert!(redirect_matches("http://127.0.0.1:1000/cb", "http://127.0.0.1:2000/cb"));
        assert!(!redirect_matches("http://127.0.0.1:1000/cb", "http://127.0.0.1:2000/other"));
        assert!(!redirect_matches("http://127.0.0.1:1000/cb", "http://localhost:1000/cb"));
        assert!(!redirect_matches("https://claude.ai/api/mcp/auth_callback", "https://claude.ai/api/mcp/auth_callback?x=1"));
        assert!(redirect_matches("ie.null.taisce:/oauth/callback", "ie.null.taisce:/oauth/callback"));
    }

    #[test]
    fn pkce_vectors() {
        // RFC 7636 appendix B
        assert_eq!(pkce_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert!(pkce_verifier_ok("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert!(!pkce_verifier_ok("short"));
        assert!(!pkce_challenge_ok("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw=cM"));
    }

    #[test]
    fn redirects_carry_params_on_custom_schemes() {
        let r = redirect_with("ie.null.taisce:/oauth/callback", &[("code", "a b"), ("state", "s")]);
        assert_eq!(r, "ie.null.taisce:/oauth/callback?code=a+b&state=s");
    }
}
