//! Server mode: OAuth 2.1 + passkeys in front of every data surface.
//!
//! LOCAL mode (no `--public-url`, today's default) never builds any of this:
//! loopback is trusted through `local_guard` and nothing here runs. SERVER
//! mode (`--public-url https://…`) is for a daemon on the public internet
//! behind a TLS reverse proxy on the same box — every proxied request comes
//! from 127.0.0.1, so loopback means nothing and `local_guard` is replaced by
//! [`require_auth`]: `/api`, `/mcp` and `/ws` need a bearer access token.
//! A personal access token (`tsk_…`, minted by `taisce auth token create`)
//! is the one other bearer, and it opens `/mcp` only.
//!
//! - `oauth`: RFC 8414 / RFC 9728 metadata, DCR (RFC 7591), authorize (PKCE
//!   S256 only), token (code + rotating refresh), revoke.
//! - `cimd`: Client ID Metadata Documents (the client_id is an https URL),
//!   fetched with an SSRF guard.
//! - `passkey`: the server-rendered login and enrollment pages (webauthn-rs).
//! - `ratelimit`: per-IP token buckets on the unauthenticated endpoints.
//! - `web`: the embedded web UI's own sign-in — a passkey opens a
//!   same-origin session cookie, accepted on `/api` only.
//!
//! Secrets (codes, tokens, enrollment links) are random 256-bit values,
//! handed out once and stored only as SHA-256 hashes.

pub mod cimd;
pub mod oauth;
pub mod passkey;
pub mod ratelimit;
pub mod web;
#[cfg(test)]
mod tests;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use taisce_store::{Scope, SharedStore, SqliteStore};
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

    /// The app's universal-link sign-in redirect on this server
    /// (`https://<public host>/oauth/app-callback`): a second exact redirect
    /// of the pinned `taisce-app` client, claimed by the app through the
    /// `apple-app-site-association` file, so no other app can catch it.
    pub fn app_https_redirect(&self) -> String {
        format!("{}{APP_HTTPS_CALLBACK_PATH}", self.base)
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
    pub store: SharedStore,
    pub webauthn: Arc<webauthn_rs::Webauthn>,
    pub pending: Arc<Mutex<passkey::Pending>>,
    pub limiter: ratelimit::Limiter,
}

impl AuthState {
    pub fn new(cfg: AuthConfig, store: SharedStore) -> anyhow::Result<Self> {
        let origin = Url::parse(&cfg.base)?;
        let webauthn = webauthn_rs::WebauthnBuilder::new(&cfg.rp_id, &origin)
            .map_err(|e| anyhow::anyhow!("webauthn config: {e}"))?
            .rp_name("Taisce")
            .build()
            .map_err(|e| anyhow::anyhow!("webauthn config: {e}"))?;
        ensure_first_party(&mut store.lock(taisce_store::Scope::System), now(), &cfg.app_https_redirect())?;
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
        .merge(passkey::router(state.clone()))
        .merge(web::router(state))
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

/// Personal access tokens start with this, so `require_auth` can tell them
/// from OAuth access tokens without a second lookup.
pub const PAT_PREFIX: &str = "tsk_";

/// A fresh personal access token: `tsk_` + 256 random bits, base64url.
pub fn new_api_token() -> String {
    format!("{PAT_PREFIX}{}", random_token())
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
/// The one redirect the Taisce app uses (TaisceKit `OAuthClient.redirectURI`).
pub const APP_REDIRECT_URI: &str = "ie.null.taisce:/oauth/callback";
/// The app's fixed, server-registered client (ADR 0004, review round 2).
/// First party is decided by client_id (`oauth_first_party`), never by the
/// redirect a client declares: DCR maps the app's exact registration to this
/// client and refuses any other use of the scheme.
pub const FIRST_PARTY_APP_CLIENT: &str = "taisce-app";
/// The path of the app's universal-link redirect (`AuthConfig::app_https_redirect`).
pub const APP_HTTPS_CALLBACK_PATH: &str = "/oauth/app-callback";
/// The Apple app the `apple-app-site-association` file names:
/// `<team id>.<bundle id>`, from `apple/App/project.yml` (`DEVELOPMENT_TEAM`
/// 6UP35L9425, `PRODUCT_BUNDLE_IDENTIFIER` ie.null.taisce). The Mac
/// (Catalyst) build shares the bundle id, so one entry covers both.
pub const APPLE_APP_ID: &str = "6UP35L9425.ie.null.taisce";

/// Is `uris` a registration of the Taisce app: exactly the custom-scheme
/// redirect, exactly the universal-link one, or both. `Some(false)` when
/// it uses either but adds anything else (refused: reserved for the app);
/// `None` when it uses neither (an ordinary client).
pub fn app_registration(uris: &[String], app_https: &str) -> Option<bool> {
    let claims = uris.iter().any(|u| u.starts_with(APP_REDIRECT_SCHEME) || u == app_https);
    claims.then(|| uris.iter().all(|u| u == APP_REDIRECT_URI || u == app_https))
}

/// A "lapsed" app client (ADR 0004, review round 4): a DCR client whose
/// only redirect is the app's, but which is not pinned first party — a
/// device whose grant had lapsed (or was revoked) when the one-time
/// grandfathering ran. It must look UNKNOWN everywhere the unchanged app
/// asks, so the app re-registers on its own and gets `taisce-app`: the app's
/// probe (`OAuthClient.clientIsKnown`, GET /oauth/authorize without PKCE)
/// re-registers on a 400; a refresh answers `invalid_client`; its old access
/// tokens answer 401. Otherwise its next sign-in would mint a connector token
/// for the person's own app, forever.
pub fn lapsed_app_client(s: &SqliteStore, client_id: &str) -> bool {
    if client_id == FIRST_PARTY_APP_CLIENT {
        return false;
    }
    let Ok(Some(c)) = s.oauth_client(client_id) else { return false };
    c.kind == "dcr"
        && c.redirect_uris.len() == 1
        && c.redirect_uris[0] == APP_REDIRECT_URI
        && !s.oauth_is_first_party(client_id).unwrap_or(false)
}

/// Register the fixed app client and pin first-party clients (idempotent).
/// Its redirects are the custom scheme (every app build) and `app_https`,
/// the universal link on this server's public URL (newer builds); a row from
/// an earlier build, or from another public URL, is brought up to date.
pub fn ensure_first_party(s: &mut SqliteStore, now: i64, app_https: &str) -> taisce_store::Result<()> {
    let redirects = vec![APP_REDIRECT_URI.to_string(), app_https.to_string()];
    let existing = s.oauth_client(FIRST_PARTY_APP_CLIENT)?;
    if existing.as_ref().is_none_or(|c| c.redirect_uris != redirects) {
        s.oauth_upsert_client(&taisce_store::auth::OAuthClient {
            client_id: FIRST_PARTY_APP_CLIENT.into(),
            kind: "dcr".into(),
            client_name: "Taisce".into(),
            redirect_uris: redirects,
            metadata: "{\"first_party\":true}".into(),
            created_at: existing.map_or(now, |c| c.created_at),
            refresh_at: None,
        })?;
    }
    // the app's DCR clients from before this pin keep their sessions
    let n = s.oauth_grandfather_first_party(APP_REDIRECT_URI, now)?;
    if n > 0 {
        tracing::info!(target: AUDIT, event = "client.first_party_grandfathered", clients = n);
    }
    s.oauth_mark_first_party(FIRST_PARTY_APP_CLIENT, now)
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
    /// ADR 0004: the user is the instance owner (the first `owner`-role
    /// user): server-level surfaces (backups, diagnostics) are theirs.
    pub instance_owner: bool,
    /// The web UI's session cookie (`web`), not a bearer token. First party
    /// like the app (`owner_app`), but its GETs never write and it is not a
    /// push device.
    pub web_session: bool,
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
    crate::store_ext::with_store(&st.store, Scope::System, move |s| {
        let grant = s.oauth_access_grant(&hash, now).ok()??;
        // a lapsed app client's old tokens stop working (the app re-signs in
        // and re-registers as the pinned app client)
        if lapsed_app_client(s, &grant.client_id) {
            return None;
        }
        let client = s.oauth_client(&grant.client_id).ok().flatten();
        let name = client.as_ref().map(|c| c.client_name.clone()).unwrap_or_default();
        let human = s.auth_user(grant.user_id).ok().flatten()?.principal_id;
        let instance_owner = s.instance_owner().ok().flatten() == Some(grant.user_id);
        Some(Authenticated {
            user_id: grant.user_id,
            grant_id: grant.id,
            principal: client_principal(&grant.client_id, &name),
            // first party by client_id only (never by a declared redirect)
            owner_app: s.oauth_is_first_party(&grant.client_id).unwrap_or(false) && client.is_some(),
            human,
            client_id: grant.client_id,
            instance_owner,
            web_session: false,
        })
    })
    .await
}

/// Resolve a personal access token to its owner, with the identity an OAuth
/// connector token for that user would carry: client `pat:<name>`, principal
/// `claude:<name>`, never the owner's app. `grant_id` is the token's id.
/// Records the use (at most once a minute) and audits the first one.
pub async fn authenticate_pat(st: &AuthState, token: &str, ip: String) -> Option<Authenticated> {
    let hash = hash_secret(token);
    let now = now();
    let (t, human, first, instance_owner) = crate::store_ext::with_store(&st.store, Scope::System, move |s| {
        let t = s.auth_api_token_by_hash(&hash).ok()??;
        let human = s.auth_user(t.user_id).ok().flatten()?.principal_id;
        let stale = t.last_used_at.is_none_or(|u| u <= now - taisce_store::auth::API_TOKEN_TOUCH_EVERY);
        let first = stale && s.auth_touch_api_token(t.id, now).unwrap_or(false) && t.last_used_at.is_none();
        let instance_owner = s.instance_owner().ok().flatten() == Some(t.user_id);
        Some((t, human, first, instance_owner))
    })
    .await?;
    if first {
        tracing::info!(target: AUDIT, event = "pat.first_use", token = %t.id, name = t.name, user = %t.user_id, ip);
    }
    let client_id = format!("pat:{}", t.name);
    Some(Authenticated {
        user_id: t.user_id,
        grant_id: t.id,
        principal: client_principal(&client_id, &t.name),
        owner_app: false,
        human,
        client_id,
        instance_owner,
        web_session: false,
    })
}

/// A token name: what `revoke` and the audit log call it, and the label of
/// its default principal (`claude:<name>`).
pub fn valid_token_name(name: &str) -> anyhow::Result<&str> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with(PAT_PREFIX);
    if !ok {
        anyhow::bail!("token name {name:?}: 1-40 of [A-Za-z0-9._-]");
    }
    Ok(name)
}

/// Mint a personal access token for `user` (or the owner). The secret is
/// returned once and stored only as its hash; the audit line names the
/// token, never its value. With `hash` (a token minted elsewhere, by its
/// lowercase-hex SHA-256) nothing is minted and no secret is returned.
pub fn create_api_token(
    store: &mut SqliteStore,
    user: Option<&str>,
    name: &str,
    hash: Option<&str>,
    now: i64,
) -> anyhow::Result<(taisce_store::auth::ApiToken, Option<String>)> {
    let name = valid_token_name(name)?;
    let user_id = match user.map(str::trim) {
        None => store.auth_owner()?.ok_or_else(|| anyhow::anyhow!("no owner yet: serve once in server mode"))?.id,
        Some(key) => {
            let users: Vec<_> = store
                .auth_users()?
                .into_iter()
                .filter(|u| !key.is_empty() && u.id.to_string().starts_with(key))
                .collect();
            match users.as_slice() {
                [u] => u.id,
                _ => anyhow::bail!("no single user matches {key:?}"),
            }
        }
    };
    let (hash, secret) = match hash.map(str::trim) {
        Some(h) => {
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                anyhow::bail!("--hash: 64 lowercase hex chars (sha256 of the tsk_… token)");
            }
            (h.to_string(), None)
        }
        None => {
            let secret = new_api_token();
            (hash_secret(&secret), Some(secret))
        }
    };
    let t = store.auth_create_api_token(user_id, name, &hash, now)?;
    tracing::info!(target: AUDIT, event = "pat.create", token = %t.id, name = t.name, user = %t.user_id, minted_here = secret.is_some());
    Ok((t, secret))
}

/// Revoke a live personal access token by name or id (prefix).
pub fn revoke_api_token(store: &mut SqliteStore, key: &str, now: i64) -> anyhow::Result<taisce_store::auth::ApiToken> {
    let t = store
        .auth_revoke_api_token(key, now)?
        .ok_or_else(|| anyhow::anyhow!("no live token matches {key:?} (ambiguous prefix?)"))?;
    tracing::info!(target: AUDIT, event = "pat.revoke", token = %t.id, name = t.name, user = %t.user_id);
    Ok(t)
}

/// `taisce auth revoke <id>`: an OAuth grant, else a web UI session, else a
/// passkey, by id or unique prefix. What was revoked, as the CLI prints it.
pub fn cli_revoke(store: &mut SqliteStore, id: &str, now: i64) -> anyhow::Result<String> {
    if let Some(g) = store.oauth_revoke_grant(id, "revoked from the CLI", now)? {
        tracing::info!(target: AUDIT, event = "token.revoke", why = "cli", grant = %g);
        return Ok(format!("revoked grant {g}"));
    }
    if let Some(w) = web::revoke_session(store, id, now)? {
        return Ok(format!("revoked web session {w}"));
    }
    if store.auth_delete_credential(id)? == 1 {
        tracing::info!(target: AUDIT, event = "passkey.delete", credential = id);
        return Ok(format!("deleted passkey {id}"));
    }
    anyhow::bail!("no live grant, web session or passkey matches {id:?} (ambiguous prefix?)")
}

/// `taisce auth token list`: one line per token, revoked ones included.
/// Only metadata is stored, so there is no secret here to leak.
pub fn api_token_lines(store: &SqliteStore) -> anyhow::Result<Vec<String>> {
    let at = |t: i64| {
        chrono::DateTime::from_timestamp(t, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default()
    };
    Ok(store
        .auth_api_tokens()?
        .into_iter()
        .map(|t| {
            let used = t.last_used_at.map(at).unwrap_or_else(|| "never".into());
            let state = t.revoked_at.map(|r| format!("revoked {}", at(r))).unwrap_or_else(|| "live".into());
            format!("token {}  {:<20}  user {}  created {}  last used {}  {}", t.id, t.name, t.user_id, at(t.created_at), used, state)
        })
        .collect())
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
        // the web UI's session cookie opens /api and nothing else (not /mcp,
        // /ws, /oauth/* or /admin); a bearer token, when present, wins
        if under(&path, "/api")
            && let Some(cookie) = web::session_cookie(req.headers())
        {
            return web::with_session(&st, req, next, cookie).await;
        }
        return unauthorized(&st.cfg, &path, None);
    };
    let is_mcp = under(&path, "/mcp");
    // a PAT opens MCP and nothing else: elsewhere only OAuth tokens are
    // looked up, so a PAT there is just an unknown token
    let pat = if is_mcp && token.starts_with(PAT_PREFIX) {
        authenticate_pat(&st, &token, client_ip(&st.cfg, req.headers(), req.extensions())).await
    } else {
        None
    };
    let who = match pat {
        Some(w) => Some(w),
        None => authenticate(&st, &token).await,
    };
    let Some(who) = who else {
        return unauthorized(&st.cfg, &path, Some("invalid_token"));
    };
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
        let n = crate::store_ext::with_store(&st.store, Scope::System, |s| s.oauth_cleanup(now())).await;
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
