//! Share links (SERVER mode): a read-only snapshot of a doc at
//! `<public url>/s/<token>`, with optional comments from whoever holds the
//! link. The contract is `docs/adr/0005-share-links.md`; the store side is
//! `taisce_store::shares`. In LOCAL mode every route here answers 404.
//!
//! Owner routes (`/api/shares…`) are human-only, like workspace membership
//! (ADR 0004): the person's own app or web session. A connector token gets
//! 403 (`Viewer::require_human_surface`, and `refuse_connector_writes` for
//! writes); a PAT never opens `/api` at all (401). Each runs in the person's
//! `Scope::User`: a doc they cannot see, or a share that is not theirs, is
//! 404.
//!
//! - `POST /api/shares {doc_id, snapshot, expires_at, comments_enabled}` → 201 Share
//! - `GET /api/shares[?doc_id=]` → `{shares}` (the caller's own)
//! - `PATCH /api/shares/{id} {snapshot?, expires_at?, comments_enabled?}` → Share
//! - `DELETE /api/shares/{id}` → 204 (revoked for good; the row stays)
//! - `POST /api/shares/preview {snapshot}` → `{html}` (stores nothing)
//! - `GET /api/shares/{id}/comments` → `{comments}` (marks them read)
//! - `POST /api/shares/{id}/comments {body, parent_id?, anchor?}` → 201 Comment
//! - `DELETE /api/shares/{id}/comments/{cid}` → 204
//!
//! Public routes (`/s/…`, no auth, per-IP rate limit `ratelimit::Class::Share`)
//! run in `Scope::Public` and read ONLY the share tables:
//! - `GET /s/{token}` the page; `GET /s/{token}/a/{name}` an image;
//!   `GET|POST /s/{token}/comments`. 404 unknown, 410 expired or revoked.

use crate::auth::ratelimit::{Class, Limiter};
use crate::auth::{AuthConfig, hash_secret, now, random_token};
use crate::share_render::{AssetInfo, PageParts, gone_page, page, render_body};
use crate::store_ext::with_store;
use crate::viewer::Viewer;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use taisce_store::shares::{NewShareComment, Share, ShareAsset, ShareComment, SharePatch, ShareSnapshot};
use taisce_store::{Scope, SharedStore, StoreError};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

// ---- limits (the contract's) ----

pub const MAX_SNAPSHOT: usize = 10 * 1024 * 1024;
pub const MAX_ASSET: usize = 2 * 1024 * 1024;
pub const MAX_ASSETS: usize = 200;
pub const MAX_MARKDOWN: usize = 2 * 1024 * 1024;
/// The request body: a 10 MB snapshot in base64 plus JSON framing.
pub const MAX_BODY: usize = 16 * 1024 * 1024;
pub const MAX_NAME: usize = 60;
pub const MAX_COMMENT: usize = 4000;
pub const MAX_QUOTE: usize = 500;
pub const MAX_TITLE: usize = 300;
/// Public comments: per IP and link an hour / a day, per link a day.
pub const PER_IP_HOUR: i64 = 10;
pub const PER_IP_DAY: i64 = 30;
pub const PER_SHARE_DAY: i64 = 200;
const ALLOWED_TYPES: [&str; 4] = ["image/svg+xml", "image/png", "image/jpeg", "image/webp"];

/// The page's Content-Security-Policy, around this response's nonce.
pub fn page_csp(nonce: &str) -> String {
    format!(
        "default-src 'none'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; font-src 'self' data:; \
script-src 'nonce-{nonce}'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
    )
}
/// An image is a document of its own (an SVG can carry script): nothing runs.
pub const ASSET_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// A new comment alert for the link's owner (`push::notify_share_comment`).
#[derive(Debug, Clone)]
pub struct CommentAlert {
    pub owner: Uuid,
    pub doc_id: Uuid,
    pub share_id: Uuid,
    pub title: String,
    pub author: String,
}

pub type Notifier = Arc<dyn Fn(CommentAlert) + Send + Sync>;

/// SERVER mode's half of the state.
#[derive(Clone)]
pub struct ServerSide {
    pub cfg: Arc<AuthConfig>,
    pub limiter: Limiter,
    /// APNs, when configured
    pub notify: Option<Notifier>,
}

#[derive(Clone)]
pub struct SharesState {
    pub store: SharedStore,
    /// None = LOCAL mode: every route answers 404.
    pub server: Option<ServerSide>,
    /// LOCAL mode's human (for the Viewer of a LOCAL request).
    pub local_human: Uuid,
}

impl FromRequestParts<SharesState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &SharesState) -> Result<Self, Response> {
        Self::from_parts(parts, st.server.is_some(), st.local_human)
    }
}

fn err(status: StatusCode, msg: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": msg.to_string()}))).into_response()
}

fn not_found() -> Response {
    err(StatusCode::NOT_FOUND, "not found")
}

fn fail(e: StoreError) -> Response {
    let code = match &e {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::Forbidden(_) => StatusCode::FORBIDDEN,
        StoreError::InvalidOp(_) | StoreError::StaleBase { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(code, e)
}

fn rfc3339(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn share_json(base: &str, s: &Share) -> Value {
    json!({
        "id": s.id,
        "doc_id": s.doc_id,
        "url": format!("{base}/s/{}", s.token),
        "created_at": rfc3339(s.created_at),
        "updated_at": rfc3339(s.updated_at),
        "expires_at": s.expires_at.map(rfc3339),
        "revoked_at": s.revoked_at.map(rfc3339),
        "comments_enabled": s.comments_enabled,
        "revision": s.revision,
        "views": s.views,
        "last_viewed_at": s.last_viewed_at.map(rfc3339),
        "comment_count": s.comment_count,
        "unread_comments": s.unread_comments,
        "title": s.title,
        "theme": s.theme,
    })
}

fn comment_json(c: &ShareComment) -> Value {
    json!({
        "id": c.id,
        "parent_id": c.parent_id,
        "author": c.author,
        "is_owner": c.is_owner,
        "body": c.body,
        "anchor": c.anchor,
        "created_at": rfc3339(c.created_at),
        "revision": c.revision,
    })
}

// ---- the snapshot ----

#[derive(Deserialize)]
pub struct AssetIn {
    name: String,
    content_type: String,
    data: String,
    #[serde(default)]
    width: Option<i64>,
    #[serde(default)]
    height: Option<i64>,
}

#[derive(Deserialize)]
pub struct SnapshotIn {
    #[serde(default)]
    title: String,
    markdown: String,
    #[serde(default)]
    assets: Vec<AssetIn>,
    #[serde(default)]
    theme: Option<String>,
}

/// A validated snapshot, not yet rendered.
struct Checked {
    title: String,
    markdown: String,
    theme: String,
    assets: Vec<ShareAsset>,
}

fn too_large(msg: impl std::fmt::Display) -> Response {
    err(StatusCode::PAYLOAD_TOO_LARGE, msg)
}

fn valid_asset_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && !n.starts_with('.')
        && n.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Do the bytes look like the declared type? (The asset is served with
/// `nosniff` and a sandbox CSP whatever it is; this just refuses junk.)
fn looks_like(ct: &str, d: &[u8]) -> bool {
    match ct {
        "image/png" => d.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => d.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/webp" => d.len() >= 12 && &d[..4] == b"RIFF" && &d[8..12] == b"WEBP",
        "image/svg+xml" => {
            let head = String::from_utf8_lossy(&d[..d.len().min(4096)]).to_ascii_lowercase();
            let t = head.trim_start_matches('\u{feff}').trim_start();
            t.starts_with('<') && head.contains("<svg")
        }
        _ => false,
    }
}

fn check_snapshot(s: SnapshotIn) -> Result<Checked, Response> {
    if s.markdown.len() > MAX_MARKDOWN {
        return Err(too_large(format!("markdown is {} bytes; the limit is 2 MB", s.markdown.len())));
    }
    if s.assets.len() > MAX_ASSETS {
        return Err(too_large(format!("{} assets; the limit is {MAX_ASSETS}", s.assets.len())));
    }
    let theme = match s.theme.as_deref().unwrap_or("auto") {
        t @ ("light" | "dark" | "auto") => t.to_string(),
        t => return Err(err(StatusCode::BAD_REQUEST, format!("theme: light, dark or auto, got {t:?}"))),
    };
    let title: String = s.title.nfc().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(MAX_TITLE).collect();
    let title = if title.is_empty() { "Untitled".to_string() } else { title };
    let mut total = s.markdown.len();
    let mut assets = Vec::with_capacity(s.assets.len());
    let mut names = std::collections::HashSet::new();
    for a in s.assets {
        if !valid_asset_name(&a.name) {
            return Err(err(StatusCode::BAD_REQUEST, format!("asset name {:?}: 1-64 of [A-Za-z0-9._-], not starting with a dot", a.name)));
        }
        if !names.insert(a.name.clone()) {
            return Err(err(StatusCode::BAD_REQUEST, format!("asset {} appears twice", a.name)));
        }
        let ct = a.content_type.trim().to_ascii_lowercase();
        if !ALLOWED_TYPES.contains(&ct.as_str()) {
            return Err(err(StatusCode::BAD_REQUEST, format!("asset {}: type {ct:?} is not one of {}", a.name, ALLOWED_TYPES.join(", "))));
        }
        // refuse before decoding what is plainly too big
        if a.data.len() / 4 * 3 > MAX_ASSET + 3 {
            return Err(too_large(format!("asset {} is over 2 MB", a.name)));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(a.data.trim())
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("asset {}: data is not base64 ({e})", a.name)))?;
        if data.len() > MAX_ASSET {
            return Err(too_large(format!("asset {} is over 2 MB", a.name)));
        }
        if !looks_like(&ct, &data) {
            return Err(err(StatusCode::BAD_REQUEST, format!("asset {}: the data is not {ct}", a.name)));
        }
        total += data.len();
        if total > MAX_SNAPSHOT {
            return Err(too_large("the snapshot is over 10 MB"));
        }
        let dim = |v: Option<i64>| v.filter(|v| (1..=100_000).contains(v));
        assets.push(ShareAsset { name: a.name, content_type: ct, data, width: dim(a.width), height: dim(a.height) });
    }
    Ok(Checked { title, markdown: s.markdown, theme, assets })
}

/// Render a checked snapshot; `url` gives each asset's `src`.
fn rendered(c: Checked, url: impl Fn(&ShareAsset) -> String) -> ShareSnapshot {
    let infos: HashMap<String, AssetInfo> =
        c.assets.iter().map(|a| (a.name.clone(), AssetInfo { url: url(a), width: a.width, height: a.height })).collect();
    let body_html = render_body(&c.markdown, &infos);
    ShareSnapshot { title: c.title, markdown: c.markdown, theme: c.theme, body_html, assets: c.assets }
}

fn asset_path(token: &str, name: &str) -> String {
    format!("/s/{token}/a/{name}")
}

fn data_url(a: &ShareAsset) -> String {
    format!("data:{};base64,{}", a.content_type, base64::engine::general_purpose::STANDARD.encode(&a.data))
}

fn parse_time(field: &str, v: &str) -> Result<i64, Response> {
    chrono::DateTime::parse_from_rfc3339(v.trim())
        .map(|d| d.timestamp())
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{field}: RFC 3339 expected ({e})")))
}

fn future_expiry(v: Option<&str>) -> Result<Option<i64>, Response> {
    match v {
        None => Ok(None),
        Some(v) => {
            let t = parse_time("expires_at", v)?;
            if t <= now() {
                return Err(err(StatusCode::BAD_REQUEST, "expires_at is in the past"));
            }
            Ok(Some(t))
        }
    }
}

// ---- owner routes ----

/// The guard every owner route runs first: SERVER mode, a person (not a
/// connector, not an agent principal). Returns the public base URL.
fn owner_gate(st: &SharesState, v: &Viewer, headers: &HeaderMap) -> Result<String, Response> {
    let Some(server) = &st.server else { return Err(not_found()) };
    v.require_human_surface()?;
    // require_auth puts a connector's principal here and strips it for the
    // person's own app: anything left is an agent identity
    if headers.contains_key(crate::mcp::PRINCIPAL_HEADER) {
        return Err(crate::viewer::forbidden("share links are made by a person, not an agent"));
    }
    Ok(server.cfg.base.clone())
}

fn json_or_400<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    match body {
        Ok(Json(b)) => Ok(b),
        Err(JsonRejection::BytesRejection(e)) => Err(err(e.status(), e.body_text())),
        Err(e) => Err(err(StatusCode::BAD_REQUEST, e.body_text())),
    }
}

#[derive(Deserialize)]
struct CreateReq {
    doc_id: Uuid,
    snapshot: SnapshotIn,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default = "yes")]
    comments_enabled: bool,
}

fn yes() -> bool {
    true
}

async fn create(State(st): State<SharesState>, v: Viewer, headers: HeaderMap, body: Result<Json<CreateReq>, JsonRejection>) -> Response {
    let base = match owner_gate(&st, &v, &headers) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let expires = match future_expiry(req.expires_at.as_deref()) {
        Ok(e) => e,
        Err(r) => return r,
    };
    let checked = match check_snapshot(req.snapshot) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let token = random_token();
    let hash = hash_secret(&token);
    let snap = {
        let t = token.clone();
        tokio::task::spawn_blocking(move || rendered(checked, |a| asset_path(&t, &a.name))).await
    };
    let Ok(snap) = snap else { return err(StatusCode::INTERNAL_SERVER_ERROR, "render failed") };
    let (doc, on) = (req.doc_id, req.comments_enabled);
    let made = with_store(&st.store, v.scope, move |s| s.share_create(doc, &token, &hash, &snap, expires, on, now())).await;
    match made {
        Ok(sh) => {
            tracing::info!(target: crate::auth::AUDIT, event = "share.create", share = %sh.id, doc = %sh.doc_id, user = ?v.user);
            (StatusCode::CREATED, Json(share_json(&base, &sh))).into_response()
        }
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct ListQ {
    #[serde(default)]
    doc_id: Option<Uuid>,
}

async fn list(State(st): State<SharesState>, v: Viewer, headers: HeaderMap, Query(q): Query<ListQ>) -> Response {
    let base = match owner_gate(&st, &v, &headers) {
        Ok(b) => b,
        Err(r) => return r,
    };
    match with_store(&st.store, v.scope, move |s| s.shares_list(q.doc_id)).await {
        Ok(all) => Json(json!({"shares": all.iter().map(|s| share_json(&base, s)).collect::<Vec<_>>()})).into_response(),
        Err(e) => fail(e),
    }
}

/// Absent → None, `null` → Some(None), a value → Some(Some(v)).
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Deserialize)]
struct PatchReq {
    #[serde(default)]
    snapshot: Option<SnapshotIn>,
    #[serde(default, deserialize_with = "nullable")]
    expires_at: Option<Option<String>>,
    #[serde(default)]
    comments_enabled: Option<bool>,
}

async fn update(
    State(st): State<SharesState>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<PatchReq>, JsonRejection>,
) -> Response {
    let base = match owner_gate(&st, &v, &headers) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let expires_at = match req.expires_at {
        None => None,
        Some(None) => Some(None),
        Some(Some(t)) => match future_expiry(Some(&t)) {
            Ok(e) => Some(e),
            Err(r) => return r,
        },
    };
    // the token (for the asset URLs) comes from the caller's own share
    let cur = match with_store(&st.store, v.scope, move |s| s.share_get(id)).await {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let snapshot = match req.snapshot {
        None => None,
        Some(s) => {
            let checked = match check_snapshot(s) {
                Ok(c) => c,
                Err(r) => return r,
            };
            let t = cur.token.clone();
            match tokio::task::spawn_blocking(move || rendered(checked, |a| asset_path(&t, &a.name))).await {
                Ok(s) => Some(s),
                Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "render failed"),
            }
        }
    };
    let p = SharePatch { snapshot, expires_at, comments_enabled: req.comments_enabled };
    match with_store(&st.store, v.scope, move |s| s.share_update(id, p, now())).await {
        Ok(sh) => Json(share_json(&base, &sh)).into_response(),
        Err(e) => fail(e),
    }
}

async fn revoke(State(st): State<SharesState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    match with_store(&st.store, v.scope, move |s| s.share_revoke(id, now())).await {
        Ok(()) => {
            tracing::info!(target: crate::auth::AUDIT, event = "share.revoke", share = %id, user = ?v.user);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct PreviewReq {
    snapshot: SnapshotIn,
}

/// The exact page, images inlined as data: URLs, without the comment
/// script (a preview or a PDF has nobody to post to). Stores nothing.
async fn preview(State(st): State<SharesState>, v: Viewer, headers: HeaderMap, body: Result<Json<PreviewReq>, JsonRejection>) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let checked = match check_snapshot(req.snapshot) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let html = tokio::task::spawn_blocking(move || {
        let snap = rendered(checked, data_url);
        page(&PageParts {
            title: &snap.title,
            body_html: &snap.body_html,
            theme: &snap.theme,
            snapshot_date: &chrono::Utc::now().format("%Y-%m-%d").to_string(),
            nonce: None,
            comments_enabled: false,
        })
    })
    .await;
    match html {
        Ok(html) => Json(json!({"html": html})).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "render failed"),
    }
}

async fn owner_comments(State(st): State<SharesState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    match with_store(&st.store, v.scope, move |s| s.share_comments_for_owner(id, now())).await {
        Ok(cs) => Json(json!({"comments": cs.iter().map(comment_json).collect::<Vec<_>>()})).into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct ReplyReq {
    body: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    anchor: Option<Value>,
}

async fn owner_reply(
    State(st): State<SharesState>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Result<Json<ReplyReq>, JsonRejection>,
) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let (body, anchor) = match (clean_body(&req.body), clean_anchor(req.anchor)) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let c = NewShareComment { author: String::new(), body, parent_id: req.parent_id, anchor, ip_hash: None };
    match with_store(&st.store, v.scope, move |s| s.share_owner_reply(id, c, now())).await {
        Ok(c) => (StatusCode::CREATED, Json(comment_json(&c))).into_response(),
        Err(e) => fail(e),
    }
}

async fn owner_delete_comment(
    State(st): State<SharesState>,
    v: Viewer,
    headers: HeaderMap,
    Path((id, cid)): Path<(Uuid, Uuid)>,
) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    match with_store(&st.store, v.scope, move |s| s.share_delete_comment(id, cid)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => fail(e),
    }
}

// ---- comment text ----

/// NFC, controls dropped (newlines and tabs kept), trimmed.
fn clean_text(s: &str, keep_lines: bool) -> String {
    s.nfc()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|c| !c.is_control() || (keep_lines && (*c == '\n' || *c == '\t')))
        .collect::<String>()
        .trim()
        .to_string()
}

fn clean_body(b: &str) -> Result<String, Response> {
    let b = clean_text(b, true);
    match b.chars().count() {
        0 => Err(err(StatusCode::BAD_REQUEST, "body: empty")),
        n if n > MAX_COMMENT => Err(err(StatusCode::BAD_REQUEST, format!("body: at most {MAX_COMMENT} characters"))),
        _ => Ok(b),
    }
}

fn clean_name(n: &str) -> Result<String, Response> {
    let n = clean_text(n, false);
    match n.chars().count() {
        0 => Err(err(StatusCode::BAD_REQUEST, "name: empty")),
        c if c > MAX_NAME => Err(err(StatusCode::BAD_REQUEST, format!("name: at most {MAX_NAME} characters"))),
        _ => Ok(n),
    }
}

/// `{block: n, quote: "…"}` or null.
fn clean_anchor(a: Option<Value>) -> Result<Option<Value>, Response> {
    let Some(a) = a.filter(|a| !a.is_null()) else { return Ok(None) };
    let block = a.get("block").and_then(Value::as_u64).filter(|b| *b <= 1_000_000);
    let quote = a.get("quote").and_then(Value::as_str).map(|q| clean_text(q, false));
    match (block, quote) {
        (Some(b), Some(q)) if q.chars().count() <= MAX_QUOTE => Ok(Some(json!({"block": b, "quote": q}))),
        _ => Err(err(StatusCode::BAD_REQUEST, format!("anchor: {{block: n, quote: ≤{MAX_QUOTE} characters}}"))),
    }
}

// ---- public routes ----

/// The headers every public response carries.
fn public_headers(h: &mut HeaderMap) {
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
}

fn html_response(status: StatusCode, html: String, nonce: Option<&str>) -> Response {
    let mut r = (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    let csp = match nonce {
        Some(n) => page_csp(n),
        None => page_csp("none").replace("'nonce-none'", "'none'"),
    };
    if let Ok(v) = HeaderValue::from_str(&csp) {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
    }
    public_headers(r.headers_mut());
    r
}

fn public_json(status: StatusCode, v: Value) -> Response {
    let mut r = (status, Json(v)).into_response();
    r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"));
    public_headers(r.headers_mut());
    r
}

enum Found {
    Live(Share),
    Gone,
    Unknown,
}

/// A token as minted: 32 bytes of base64url, no padding.
fn token_shape(t: &str) -> bool {
    t.len() == 43 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The per-IP limit, then the share the token names (by its hash).
async fn lookup(st: &SharesState, req_headers: &HeaderMap, ext: &axum::http::Extensions, token: &str) -> Result<(Found, String), Response> {
    let Some(server) = &st.server else { return Err(not_found()) };
    let ip = crate::auth::client_ip(&server.cfg, req_headers, ext);
    if !server.limiter.allow(Class::Share, &ip) {
        return Err(public_json(StatusCode::TOO_MANY_REQUESTS, json!({"error": "too many requests; try again shortly"})));
    }
    if !token_shape(token) {
        return Ok((Found::Unknown, ip));
    }
    let hash = hash_secret(token);
    let found = with_store(&st.store, Scope::Public, move |s| s.share_by_token_hash(&hash)).await;
    Ok(match found {
        Ok(Some(sh)) if sh.is_gone(now()) => (Found::Gone, ip),
        Ok(Some(sh)) => (Found::Live(sh), ip),
        Ok(None) => (Found::Unknown, ip),
        Err(e) => return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e)),
    })
}

async fn public_page(State(st): State<SharesState>, Path(token): Path<String>, req: Request) -> Response {
    let (found, _) = match lookup(&st, req.headers(), req.extensions(), &token).await {
        Ok(f) => f,
        Err(r) => return r,
    };
    let sh = match found {
        Found::Live(sh) => sh,
        Found::Gone => return html_response(StatusCode::GONE, gone_page("This link has expired or was turned off."), None),
        Found::Unknown => return html_response(StatusCode::NOT_FOUND, gone_page("There is nothing at this link."), None),
    };
    let id = sh.id;
    let body = with_store(&st.store, Scope::Public, move |s| {
        let b = s.share_public_body(id)?;
        s.share_public_viewed(id, now())?;
        Ok::<_, StoreError>(b)
    })
    .await;
    let body = match body {
        Ok(b) => b,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let nonce = random_token();
    let date = chrono::DateTime::from_timestamp(sh.snapshot_at, 0).map(|d| d.format("%-d %B %Y").to_string()).unwrap_or_default();
    let html = page(&PageParts {
        title: &sh.title,
        body_html: &body,
        theme: &sh.theme,
        snapshot_date: &date,
        nonce: Some(&nonce),
        comments_enabled: sh.comments_enabled,
    });
    html_response(StatusCode::OK, html, Some(&nonce))
}

async fn public_asset(State(st): State<SharesState>, Path((token, name)): Path<(String, String)>, req: Request) -> Response {
    let (found, _) = match lookup(&st, req.headers(), req.extensions(), &token).await {
        Ok(f) => f,
        Err(r) => return r,
    };
    let plain = |status: StatusCode, msg: &'static str| {
        let mut r = (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], msg).into_response();
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(ASSET_CSP));
        public_headers(r.headers_mut());
        r
    };
    let sh = match found {
        Found::Live(sh) => sh,
        Found::Gone => return plain(StatusCode::GONE, "gone"),
        Found::Unknown => return plain(StatusCode::NOT_FOUND, "not found"),
    };
    if !valid_asset_name(&name) {
        return plain(StatusCode::NOT_FOUND, "not found");
    }
    let id = sh.id;
    let asset = match with_store(&st.store, Scope::Public, move |s| s.share_public_asset(id, &name)).await {
        Ok(Some(a)) => a,
        Ok(None) => return plain(StatusCode::NOT_FOUND, "not found"),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut r = asset.data.into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&asset.content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(ASSET_CSP));
    h.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("inline"));
    public_headers(h);
    r
}

async fn public_comments(State(st): State<SharesState>, Path(token): Path<String>, req: Request) -> Response {
    let (found, _) = match lookup(&st, req.headers(), req.extensions(), &token).await {
        Ok(f) => f,
        Err(r) => return r,
    };
    let sh = match found {
        Found::Live(sh) => sh,
        Found::Gone => return public_json(StatusCode::GONE, json!({"error": "this link has expired or was turned off"})),
        Found::Unknown => return public_json(StatusCode::NOT_FOUND, json!({"error": "not found"})),
    };
    if !sh.comments_enabled {
        return public_json(StatusCode::OK, json!({"comments": [], "enabled": false}));
    }
    let id = sh.id;
    match with_store(&st.store, Scope::Public, move |s| s.share_public_comments(id)).await {
        Ok(cs) => public_json(StatusCode::OK, json!({"comments": cs.iter().map(comment_json).collect::<Vec<_>>(), "enabled": true})),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

#[derive(Deserialize)]
struct PublicCommentReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    anchor: Option<Value>,
    #[serde(default)]
    parent_id: Option<Uuid>,
    /// the honeypot: a person never fills it in
    #[serde(default)]
    website: Option<String>,
}

async fn public_comment(State(st): State<SharesState>, Path(token): Path<String>, req: Request) -> Response {
    let (found, ip) = match lookup(&st, req.headers(), req.extensions(), &token).await {
        Ok(f) => f,
        Err(r) => return r,
    };
    let sh = match found {
        Found::Live(sh) => sh,
        Found::Gone => return public_json(StatusCode::GONE, json!({"error": "this link has expired or was turned off"})),
        Found::Unknown => return public_json(StatusCode::NOT_FOUND, json!({"error": "not found"})),
    };
    if !sh.comments_enabled {
        return public_json(StatusCode::FORBIDDEN, json!({"error": "comments are off for this link"}));
    }
    let bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return public_json(StatusCode::PAYLOAD_TOO_LARGE, json!({"error": "too large"})),
    };
    let Ok(b) = serde_json::from_slice::<PublicCommentReq>(&bytes) else {
        return public_json(StatusCode::BAD_REQUEST, json!({"error": "expected {name, body}"}));
    };
    let (name, body, anchor) = match (clean_name(&b.name), clean_body(&b.body), clean_anchor(b.anchor)) {
        (Ok(n), Ok(t), Ok(a)) => (n, t, a),
        (Err(r), _, _) | (_, Err(r), _) | (_, _, Err(r)) => {
            let mut r = r;
            public_headers(r.headers_mut());
            return r;
        }
    };
    let fake = |name: String, body: String, anchor: Option<Value>| {
        json!({"id": Uuid::now_v7(), "parent_id": b.parent_id, "author": name, "is_owner": false, "body": body,
            "anchor": anchor, "created_at": rfc3339(now()), "revision": sh.revision})
    };
    if b.website.as_deref().is_some_and(|w| !w.trim().is_empty()) {
        // a bot: it hears success, nothing is stored or sent
        tracing::info!(target: crate::auth::AUDIT, event = "share.comment_honeypot", share = %sh.id);
        return public_json(StatusCode::CREATED, fake(name, body, anchor));
    }
    let ip_hash = hash_secret(&format!("{}|{ip}", sh.id));
    let (id, parent) = (sh.id, b.parent_id);
    let made = with_store(&st.store, Scope::Public, move |s| {
        let n = s.share_comment_counts(id, &ip_hash, now())?;
        if n.ip_hour >= PER_IP_HOUR || n.ip_day >= PER_IP_DAY || n.share_day >= PER_SHARE_DAY {
            return Ok(None);
        }
        let c = NewShareComment { author: name, body, parent_id: parent, anchor, ip_hash: Some(ip_hash) };
        s.share_public_comment(id, c, now()).map(Some)
    })
    .await;
    match made {
        Ok(Some(c)) => {
            if let Some(notify) = st.server.as_ref().and_then(|s| s.notify.clone()) {
                notify(CommentAlert { owner: sh.owner_id, doc_id: sh.doc_id, share_id: sh.id, title: sh.title.clone(), author: c.author.clone() });
            }
            public_json(StatusCode::CREATED, comment_json(&c))
        }
        Ok(None) => public_json(StatusCode::TOO_MANY_REQUESTS, json!({"error": "too many comments on this link for now; try again later"})),
        Err(StoreError::NotFound(_)) => public_json(StatusCode::GONE, json!({"error": "this link has expired or was turned off"})),
        Err(StoreError::Forbidden(m)) => public_json(StatusCode::FORBIDDEN, json!({"error": m})),
        Err(StoreError::InvalidOp(m)) => public_json(StatusCode::BAD_REQUEST, json!({"error": m})),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub fn router(state: SharesState) -> Router {
    let owner = Router::new()
        .route("/api/shares", get(list).post(create))
        .route("/api/shares/preview", post(preview))
        .route("/api/shares/{id}", patch(update).delete(revoke))
        .route("/api/shares/{id}/comments", get(owner_comments).post(owner_reply))
        .route("/api/shares/{id}/comments/{cid}", delete(owner_delete_comment))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        // connectors write only through the gated routes (ADR 0004)
        .layer(axum::middleware::from_fn(crate::viewer::refuse_connector_writes));
    Router::new()
        .route("/s/{token}", get(public_page))
        .route("/s/{token}/a/{name}", get(public_asset))
        .route("/s/{token}/comments", get(public_comments).post(public_comment))
        .merge(owner)
        .with_state(state)
}
