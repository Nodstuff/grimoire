//! Server mode: OAuth 2.1 + passkeys in front of every data surface.
//!
//! LOCAL mode (no `--public-url`, today's default) never builds any of this:
//! loopback is trusted through `local_guard` and nothing here runs. SERVER
//! mode (`--public-url https://…`) is for a daemon on the public internet
//! behind a TLS reverse proxy on the same box — every proxied request comes
//! from 127.0.0.1, so loopback means nothing and `local_guard` is replaced by
//! [`require_auth`]: `/api`, `/mcp` and `/ws` need a bearer access token.
//!
//! - `oauth`: RFC 8414 / RFC 9728 metadata, DCR (RFC 7591), authorize (PKCE
//!   S256 only), token (code + rotating refresh), revoke.
//! - `cimd`: Client ID Metadata Documents (the client_id is an https URL),
//!   fetched with an SSRF guard.
//! - `passkey`: the server-rendered login and enrollment pages (webauthn-rs).
//! - `ratelimit`: per-IP token buckets on the unauthenticated endpoints.
//!
//! Secrets (codes, tokens, enrollment links) are random 256-bit values,
//! handed out once and stored only as SHA-256 hashes.

pub mod cimd;
pub mod oauth;
pub mod passkey;
pub mod ratelimit;
#[cfg(test)]
mod tests;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use taisce_store::SqliteStore;
use sha2::Digest as _;
use std::sync::{Arc, Mutex};
use webauthn_rs::prelude::Url;

/// Access tokens live an hour; refresh tokens 30 days from their issue
/// (each rotation issues a fresh one); codes two minutes; enrollment links
/// fifteen.
pub const ACCESS_TTL: i64 = 3600;
pub const REFRESH_TTL: i64 = 30 * 86400;
pub const CODE_TTL: i64 = 120;
pub const ENROLL_TTL: i64 = 15 * 60;
/// The one scope: full access as the authorizing user. Still `grimoire`
/// after the rename: issued tokens and registered clients carry it.
pub const SCOPE: &str = "grimoire";

/// Audit lines go to this target (`RUST_LOG=taisce::audit=info`).
pub const AUDIT: &str = "taisce::audit";

/// Server-mode configuration, fixed at startup.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// The public origin, no trailing slash: `https://taisce.example`. It is
    /// the issuer, the protected resource, and the WebAuthn origin.
    pub base: String,
    /// WebAuthn RP id: the public host.
    pub rp_id: String,
    /// Key rate limits on `X-Forwarded-For` (the proxy's last hop) instead of
    /// the socket peer, which behind a same-box proxy is always 127.0.0.1.
    pub trusted_proxy: bool,
    /// Redirect URIs allowed beyond the built-in set (exact match).
    pub extra_redirects: Vec<String>,
    /// Tests only: let CIMD fetch `http://127.0.0.1` documents.
    pub cimd_allow_insecure: bool,
}

impl AuthConfig {
    /// Parse `--public-url`. An origin only: a path prefix is refused, since
    /// every route (and the RFC 8414 well-known location) is at the root.
    pub fn from_public_url(public_url: &str) -> anyhow::Result<Self> {
        let url = Url::parse(public_url.trim()).map_err(|e| anyhow::anyhow!("--public-url {public_url:?}: {e}"))?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("--public-url needs a host"))?
            .to_string();
        let local_http = url.scheme() == "http" && host == "localhost";
        if url.scheme() != "https" && !local_http {
            anyhow::bail!("--public-url must be https (http only for localhost testing)");
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() || !url.username().is_empty() {
            anyhow::bail!("--public-url must be an origin (scheme://host[:port]), no path/query");
        }
        let base = url.as_str().trim_end_matches('/').to_string();
        Ok(Self {
            base,
            rp_id: host,
            trusted_proxy: false,
            extra_redirects: Vec::new(),
            cimd_allow_insecure: false,
        })
    }

    /// The MCP endpoint as clients address it.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base)
    }

    /// The public authority (`host[:port]`) for rmcp's Host allowlist.
    pub fn authority(&self) -> String {
        self.base.split_once("://").map(|(_, a)| a.to_string()).unwrap_or_default()
    }

    /// Is `resource` (RFC 8707) one this server protects: the origin or the
    /// MCP endpoint, trailing slash ignored.
    pub fn resource_ok(&self, resource: &str) -> bool {
        let r = resource.trim().trim_end_matches('/');
        r == self.base || r == self.mcp_url()
    }
}

/// Everything the auth routes and middleware share.
#[derive(Clone)]
pub struct AuthState {
    pub cfg: Arc<AuthConfig>,
    pub store: Arc<Mutex<SqliteStore>>,
    pub webauthn: Arc<webauthn_rs::Webauthn>,
    pub pending: Arc<Mutex<passkey::Pending>>,
    pub limiter: ratelimit::Limiter,
}

impl AuthState {
    pub fn new(cfg: AuthConfig, store: Arc<Mutex<SqliteStore>>) -> anyhow::Result<Self> {
        let origin = Url::parse(&cfg.base)?;
        let webauthn = webauthn_rs::WebauthnBuilder::new(&cfg.rp_id, &origin)
            .map_err(|e| anyhow::anyhow!("webauthn config: {e}"))?
            .rp_name("Taisce")
            .build()
            .map_err(|e| anyhow::anyhow!("webauthn config: {e}"))?;
        Ok(Self {
            cfg: Arc::new(cfg),
            store,
            webauthn: Arc::new(webauthn),
            pending: Arc::new(Mutex::new(passkey::Pending::default())),
            limiter: ratelimit::Limiter::default(),
        })
    }
}

/// The routes that must stay reachable without a token.
pub fn router(state: AuthState) -> axum::Router {
    oauth::router(state.clone())
        .merge(passkey::router(state))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        // every body here is a small form or JSON document
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A fresh 256-bit secret, base64url without padding (43 chars).
pub fn random_token() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS entropy");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// How a secret is stored: lowercase hex SHA-256.
pub fn hash_secret(s: &str) -> String {
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

/// `X-Forwarded-For`'s last hop when the proxy is trusted, else the peer.
pub fn client_ip(cfg: &AuthConfig, headers: &HeaderMap, ext: &axum::http::Extensions) -> String {
    if cfg.trusted_proxy
        && let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(last) = xff.rsplit(',').next().map(str::trim).filter(|s| !s.is_empty())
    {
        return last.to_string();
    }
    ext.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// The default agent principal for an OAuth client's MCP writes:
/// `claude:<slug>`, from a CIMD client_id's host (`claude.ai` →
/// `claude-ai`) or a registered client's name (`Claude Code` → `claude-code`).
pub fn client_principal(client_id: &str, client_name: &str) -> String {
    let source = match Url::parse(client_id) {
        Ok(u) if u.scheme() == "https" || u.scheme() == "http" => u.host_str().unwrap_or("").to_string(),
        _ => client_name.to_string(),
    };
    let mut slug = String::new();
    for c in source.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        let short: String = client_id.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
        format!("claude:oauth-{short}")
    } else {
        format!("claude:{slug}")
    }
}

/// The native app's redirect scheme. A client whose every redirect URI is
/// on it is the owner's own app: it writes as the human, not as an agent.
pub const APP_REDIRECT_SCHEME: &str = "ie.null.taisce:";

fn is_owner_app(redirect_uris: &[String]) -> bool {
    !redirect_uris.is_empty() && redirect_uris.iter().all(|u| u.starts_with(APP_REDIRECT_SCHEME))
}

/// Who a request authenticated as (an extension on authenticated requests).
/// In SERVER mode identity is pinned to the token: `Taisce-Principal`,
/// `?as=` and `?cwd=` are ignored, and the MCP `as` argument is only a label
/// inside the token's own `claude:` namespace (`mcp::Pinned`).
#[derive(Debug, Clone)]
pub struct Authenticated {
    pub user_id: uuid::Uuid,
    pub grant_id: uuid::Uuid,
    pub client_id: String,
    /// `claude:<slug>`: a connector's writes are attributed to it.
    pub principal: String,
    /// The owner's own app (`APP_REDIRECT_SCHEME`): writes as `human`.
    pub owner_app: bool,
    /// The authorizing user's human principal.
    pub human: uuid::Uuid,
}

/// Paths reachable with no token: discovery, the OAuth endpoints, the
/// login/enroll pages, and the health check.
fn is_public(path: &str) -> bool {
    path == "/healthz"
        || path.starts_with("/.well-known/")
        || path.starts_with("/oauth/")
        || path.starts_with("/auth/")
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.strip_prefix(prefix).is_some_and(|r| r.starts_with('/'))
}

/// Data surfaces: always need a token. Anything else that is a GET/HEAD is
/// the embedded UI's static assets (no data); any other method needs a token.
fn needs_token(method: &axum::http::Method, path: &str) -> bool {
    if under(path, "/api") || under(path, "/mcp") || under(path, "/ws") {
        return true;
    }
    !(method == axum::http::Method::GET || method == axum::http::Method::HEAD)
}

/// A request that came through the reverse proxy carries its forwarding
/// headers; the box's own CLI talking to 127.0.0.1 never does.
fn was_forwarded(headers: &HeaderMap) -> bool {
    ["x-forwarded-for", "forwarded", "x-real-ip", "x-forwarded-host"]
        .iter()
        .any(|h| headers.contains_key(*h))
}

/// The 401 every protected surface answers: Claude reads the
/// `resource_metadata` URL off it (and ignores it on any other status).
pub fn unauthorized(cfg: &AuthConfig, path: &str, error: Option<&str>) -> Response {
    let metadata = if under(path, "/mcp") {
        format!("{}/.well-known/oauth-protected-resource/mcp", cfg.base)
    } else {
        format!("{}/.well-known/oauth-protected-resource", cfg.base)
    };
    let mut challenge = format!("Bearer resource_metadata=\"{metadata}\"");
    if let Some(e) = error {
        challenge.push_str(&format!(", error=\"{e}\""));
    }
    let mut res = (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(format!(
            "{{\"error\":\"{}\",\"code\":\"unauthorized\"}}",
            error.unwrap_or("authentication required")
        )),
    )
        .into_response();
    if let Ok(v) = HeaderValue::from_str(&challenge) {
        res.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    res
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, tok) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| tok.trim()).filter(|t| !t.is_empty())
}

/// Resolve a bearer access token to its live grant and client.
pub async fn authenticate(st: &AuthState, token: &str) -> Option<Authenticated> {
    let hash = hash_secret(token);
    let now = now();
    crate::store_ext::with_store(&st.store, move |s| {
        let grant = s.oauth_access_grant(&hash, now).ok()??;
        let client = s.oauth_client(&grant.client_id).ok().flatten();
        let name = client.as_ref().map(|c| c.client_name.clone()).unwrap_or_default();
        let human = s.auth_user(grant.user_id).ok().flatten()?.principal_id;
        Some(Authenticated {
            user_id: grant.user_id,
            grant_id: grant.id,
            principal: client_principal(&grant.client_id, &name),
            owner_app: client.is_some_and(|c| is_owner_app(&c.redirect_uris)),
            human,
            client_id: grant.client_id,
        })
    })
    .await
}

/// The server-mode layer over the whole router (it replaces `local_guard`).
pub async fn require_auth(State(st): State<AuthState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if is_public(&path) {
        return next.run(req).await;
    }
    if under(&path, "/admin") {
        // admin stays the box's own CLI (admin token, as today); never via the proxy
        if was_forwarded(req.headers()) {
            tracing::warn!(target: AUDIT, path, "refused forwarded /admin request");
            return (
                StatusCode::FORBIDDEN,
                [(header::CONTENT_TYPE, "application/json")],
                Body::from("{\"error\":\"forbidden: admin is local-CLI only\",\"code\":\"admin_local_only\"}"),
            )
                .into_response();
        }
        return next.run(req).await;
    }
    if !needs_token(req.method(), &path) {
        return next.run(req).await;
    }
    let Some(token) = bearer(req.headers()).map(str::to_string) else {
        return unauthorized(&st.cfg, &path, None);
    };
    let Some(who) = authenticate(&st, &token).await else {
        return unauthorized(&st.cfg, &path, Some("invalid_token"));
    };
    let is_mcp = under(&path, "/mcp");
    // identity is the token's: whatever the request names itself is dropped.
    // The HTTP API reads the header slot, so a connector's token fills it
    // with its own principal and the owner's app leaves it empty (→ the
    // human); MCP reads `Authenticated` itself (`mcp::RequestHint`).
    req.headers_mut().remove(crate::mcp::PRINCIPAL_HEADER);
    if !who.owner_app && let Ok(v) = HeaderValue::from_str(&who.principal) {
        req.headers_mut().insert(crate::mcp::PRINCIPAL_HEADER, v);
    }
    let ip = client_ip(&st.cfg, req.headers(), req.extensions());
    let method = req.method().clone();
    req.extensions_mut().insert(who.clone());
    if is_mcp && method == axum::http::Method::POST {
        // audit every MCP call: peek the JSON-RPC method and tool name
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, crate::mcp::MAX_MCP_BODY).await {
            Ok(b) => b,
            Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
        };
        let (rpc, tool) = rpc_summary(&bytes);
        let req = Request::from_parts(parts, Body::from(bytes));
        let res = next.run(req).await;
        tracing::info!(
            target: AUDIT,
            event = "mcp.call",
            client = who.client_id,
            grant = %who.grant_id,
            user = %who.user_id,
            principal = who.principal,
            owner_app = who.owner_app,
            ip,
            rpc,
            tool,
            status = res.status().as_u16(),
        );
        return res;
    }
    let res = next.run(req).await;
    if is_mcp {
        tracing::info!(target: AUDIT, event = "mcp.request", client = who.client_id, grant = %who.grant_id, ip,
            method = %method, status = res.status().as_u16());
    }
    res
}

/// `(method, params.name)` of a JSON-RPC message or the first of a batch.
fn rpc_summary(body: &[u8]) -> (String, String) {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let first = if v.is_array() { v.get(0).cloned().unwrap_or_default() } else { v };
    let s = |x: Option<&serde_json::Value>| x.and_then(|x| x.as_str()).unwrap_or("").to_string();
    (s(first.get("method")), s(first.get("params").and_then(|p| p.get("name"))))
}

/// Sweep expired codes/tokens/links and stale login ceremonies, every 10 min.
pub async fn cleanup_loop(st: AuthState) {
    loop {
        let n = crate::store_ext::with_store(&st.store, |s| s.oauth_cleanup(now())).await;
        match n {
            Ok(n) if n > 0 => tracing::info!(rows = n, "auth cleanup"),
            Ok(_) => {}
            Err(e) => tracing::warn!("auth cleanup failed: {e}"),
        }
        st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sweep(now());
        st.limiter.sweep();
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
    }
}
