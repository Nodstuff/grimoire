//! The OPDS 1.2 catalog (SERVER mode): a person's EPUB library as Atom
//! feeds an e-reader can browse (`Taisce › EPUB proposal`; the store side
//! is `taisce_store::books`). E-readers cannot do OAuth, so `/opds` speaks
//! HTTP basic auth against per-device app passwords (minted show-once,
//! stored as SHA-256 like PATs), with the same secret accepted in the URL
//! path for clients that cannot send basic auth. In LOCAL mode every route
//! here answers 404.
//!
//! Owner routes (`/api/ereaders…`) are human-only, like share links (ADR
//! 0004): the person's own app or web session (`Viewer::require_human_surface`,
//! and `refuse_connector_writes` for writes); a PAT never opens `/api` at
//! all. Each runs in the person's `Scope::User`.
//!
//! - `GET /api/ereaders` → `{ereaders}` (the caller's own, revoked included)
//! - `POST /api/ereaders {label, stream_downloads?}` → 201 `{ereader, password}`
//! - `POST /api/ereaders/{id}/rotate` → `{password}` (the old one dies at once)
//! - `DELETE /api/ereaders/{id}` → 204 (revoked; the row stays listed)
//!
//! Book upload tokens (`/api/upload-tokens…`, the same owner gate): a
//! `tbu_…` bearer an integration (a Readarr/Chaptarr custom script) sends to
//! `POST /api/books` and nowhere else (`auth::require_auth` enforces it).
//!
//! - `GET /api/upload-tokens` → `{upload_tokens}` (revoked included, never
//!   a secret or hash)
//! - `POST /api/upload-tokens {label}` → 201 `{upload_token, token}` (the
//!   token exists only in this reply)
//! - `DELETE /api/upload-tokens/{id}` → 204 (revoked; the row stays listed)
//!
//! Feed routes (`/opds…`) resolve the device password to its ereader in
//! `Scope::System` — the hash IS the credential, exactly like
//! `authenticate_pat` — then read that person's `book` rows in
//! `Scope::User(ereader.user_id)`. Every failure gets one answer: 401 with
//! `WWW-Authenticate: Basic realm="Taisce books"`, never the OAuth Bearer
//! challenge. `/opds` is a GET surface outside `/api`, `/mcp` and `/ws`, so
//! `auth::require_auth` already lets it through (no token needed); this
//! module's own layer does the device auth. Rate limits: `Class::Opds` per
//! IP, `Class::OpdsDevice` per secret hash, `Class::OpdsAuth` per IP on
//! failures.
//!
//! - `GET /opds/` (or `/opds/{secret}/`) the navigation feed
//! - `GET /opds/new` · `/opds/all` · `/opds/author/{name}` ·
//!   `/opds/series/{name}` · `/opds/search?q=` acquisition feeds, 50 a page
//! - `GET /opds/authors` · `/opds/series` navigation feeds
//! - `GET /opds/opensearch.xml` the OpenSearch description
//!
//! `/opds/book/{id}/file` and `/cover` (and the `{secret}` path forms) are
//! the downloads: they sit in this router under the same `opds_auth` layer,
//! read the resolved device from the [`OpdsClient`] extension, and hand the
//! bytes to `library::book_file`/`book_cover` (a 302 to a presigned URL, or
//! a stream-through for `stream_downloads` devices). Every link the feeds
//! emit keeps the form — basic or path — the client came in with.

use crate::auth::ratelimit::{Class, Limiter};
use crate::auth::{AuthConfig, client_ip, ct_eq, hash_secret, now};
use crate::share_render::esc;
use crate::shares::ip_key;
use crate::store_ext::with_store;
use crate::viewer::Viewer;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use taisce_store::books::{Book, BookField, BookFilter, BookPage, Ereader, UploadToken};
use taisce_store::{Scope, SharedStore, StoreError};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

// ---- the catalog's constants ----

/// Acquisition feeds page at 50 (the proposal's): a page fits an e-ink
/// screen, and one download.
const PAGE: usize = 50;

/// Feed content types, as the OPDS 1.2 profile freezes them.
const NAV_CT: &str = "application/atom+xml;profile=opds-catalog;kind=navigation";
const ACQ_CT: &str = "application/atom+xml;profile=opds-catalog;kind=acquisition";
const OPENSEARCH_CT: &str = "application/opensearchdescription+xml";

/// The one challenge every `/opds` failure answers with (the proposal's
/// realm; never the OAuth Bearer challenge the rest of the server sends).
const BASIC_CHALLENGE: &str = "Basic realm=\"Taisce books\"";

/// SERVER mode's half of the state (None in LOCAL mode: every route 404s,
/// since the library keys shelves to `auth_users` rows).
#[derive(Clone)]
pub struct OpdsServer {
    pub cfg: Arc<AuthConfig>,
    pub limiter: Limiter,
}

#[derive(Clone)]
pub struct OpdsState {
    pub store: SharedStore,
    /// None = LOCAL mode: every route answers 404.
    pub server: Option<OpdsServer>,
    /// LOCAL mode's human (for the Viewer of a LOCAL request).
    pub local_human: Uuid,
    /// The download half: the same `LibraryState` the `/api/books` router
    /// serves from. The feed's book/cover links resolve the device here
    /// (opds_auth) and library::book_file/book_cover serve the bytes.
    pub library: crate::library::LibraryState,
}

impl FromRequestParts<OpdsState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &OpdsState) -> Result<Self, Response> {
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

// ---- device secrets ----

/// A device password is 16 base32 chars (80 bits: unguessable, still
/// typeable on an e-ink keyboard), displayed in groups of four.
const SECRET_LEN: usize = 16;
/// RFC 4648's base32 alphabet: no 0/1/8/9, so no ambiguous glyphs.
const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Mint a device password, shown to the person ONCE (at pairing and after
/// each rotation) as `xxxx-xxxx-xxxx-xxxx`. The dashes are display
/// grouping only: [`undash`] strips them before any hash.
pub(crate) fn mint_device_secret() -> String {
    let mut b = [0u8; SECRET_LEN];
    getrandom::fill(&mut b).expect("OS entropy");
    // `& 31` is unbiased: 256 is a whole number of 32s
    let raw: String = b.iter().map(|b| BASE32[(b & 31) as usize] as char).collect();
    format!("{}-{}-{}-{}", &raw[..4], &raw[4..8], &raw[8..12], &raw[12..])
}

/// The canonical form of a presented password: trimmed, display dashes
/// dropped, lowercased (we mint lowercase; a keyboard that capitalizes
/// still matches). People type the dashed form into their reader, so every
/// verification goes through this.
fn undash(s: &str) -> String {
    s.trim().chars().filter(|c| *c != '-').collect::<String>().to_ascii_lowercase()
}

/// The stored hashes for a freshly minted device password (its displayed,
/// dashed lowercase form): SHA-256 of the canonical (undashed) password —
/// the OPDS/basic-auth credential, which [`undash`] normalizes on every
/// request — and the KOSYNC userkeys a device derives from it. A device
/// sends md5(password EXACTLY as typed) and we never see the password, so
/// both ways a person plausibly types it are stored: the dashed form the UI
/// shows (primary) and the undashed one (alt).
pub(crate) fn device_secret_hashes(secret: &str) -> (String, (String, String)) {
    let shown = secret.trim().to_ascii_lowercase();
    let raw = undash(secret);
    let key = |p: &str| hash_secret(&crate::booknaming::md5_hex(p.as_bytes()));
    (hash_secret(&raw), (key(&shown), key(&raw)))
}

/// Is this path segment a presented device password (the minted shape, with
/// or without the display dashes)? Static feed segments ("new", "series",
/// "book", …) never are: none is 16 base32 chars.
fn looks_like_secret(seg: &str) -> bool {
    let s = undash(seg);
    s.len() == SECRET_LEN && s.bytes().all(|b| BASE32.contains(&b))
}


// ---- owner routes (/api/ereaders, a person) ----

/// A device label: NFC, controls dropped, trimmed; 1..=60 chars.
const MAX_LABEL: usize = 60;

fn clean_label(s: &str) -> Result<String, Response> {
    let l: String = s.nfc().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
    match l.chars().count() {
        0 => Err(err(StatusCode::BAD_REQUEST, "label: empty")),
        n if n > MAX_LABEL => Err(err(StatusCode::BAD_REQUEST, format!("label: at most {MAX_LABEL} characters"))),
        _ => Ok(l),
    }
}

/// The guard every owner route runs first: SERVER mode, a person (not a
/// connector, not an agent principal). Returns the signed-in user.
fn owner_gate(st: &OpdsState, v: &Viewer, headers: &HeaderMap) -> Result<Uuid, Response> {
    owner_gate_for(st, v, headers, "pairing an ereader is done by a person, not an agent")
}

fn owner_gate_for(st: &OpdsState, v: &Viewer, headers: &HeaderMap, refusal: &str) -> Result<Uuid, Response> {
    if st.server.is_none() {
        return Err(not_found());
    }
    v.require_human_surface()?;
    // require_auth puts a connector's principal here and strips it for the
    // person's own app: anything left is an agent identity
    if headers.contains_key(crate::mcp::PRINCIPAL_HEADER) {
        return Err(crate::viewer::forbidden(refusal));
    }
    // SERVER mode always carries the signed-in user (LOCAL 404s above)
    v.user.ok_or_else(not_found)
}

fn json_or_400<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    match body {
        Ok(Json(b)) => Ok(b),
        Err(JsonRejection::BytesRejection(e)) => Err(err(e.status(), e.body_text())),
        Err(e) => Err(err(StatusCode::BAD_REQUEST, e.body_text())),
    }
}

/// The device as the owner routes report it: never a hash, never a secret
/// (the password exists only in the create/rotate responses).
fn ereader_json(e: &Ereader) -> Value {
    json!({
        "id": e.id,
        "label": e.label,
        "stream_downloads": e.stream_downloads,
        "created_at": rfc3339(e.created_at),
        "last_used_at": e.last_used_at.map(rfc3339),
        "revoked_at": e.revoked_at.map(rfc3339),
    })
}

async fn list_ereaders(State(st): State<OpdsState>, v: Viewer, headers: HeaderMap) -> Response {
    let user = match owner_gate(&st, &v, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match with_store(&st.store, v.scope, move |s| s.ereader_list(user)).await {
        Ok(all) => Json(json!({"ereaders": all.iter().map(ereader_json).collect::<Vec<_>>()})).into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct CreateReq {
    label: String,
    /// stream downloads through the box instead of 302ing to S3 (the
    /// default, per the proposal, is the redirect)
    #[serde(default)]
    stream_downloads: Option<bool>,
}

async fn create_ereader(State(st): State<OpdsState>, v: Viewer, headers: HeaderMap, body: Result<Json<CreateReq>, JsonRejection>) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let label = match clean_label(&req.label) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let secret = mint_device_secret();
    let (secret_hash, kosync) = device_secret_hashes(&secret);
    let (id, stream) = (Uuid::now_v7(), req.stream_downloads.unwrap_or(false));
    match with_store(&st.store, v.scope, move |s| s.ereader_insert(id, &label, &secret_hash, (&kosync.0, &kosync.1), stream, now())).await {
        Ok(e) => {
            tracing::info!(target: crate::auth::AUDIT, event = "ereader.pair", ereader = %e.id, label = %e.label.as_str(), user = ?v.user);
            (StatusCode::CREATED, Json(json!({"ereader": ereader_json(&e), "password": secret}))).into_response()
        }
        Err(e) => fail(e),
    }
}

async fn rotate_ereader(State(st): State<OpdsState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    let secret = mint_device_secret();
    let (secret_hash, kosync) = device_secret_hashes(&secret);
    match with_store(&st.store, v.scope, move |s| s.ereader_rotate(id, &secret_hash, (&kosync.0, &kosync.1))).await {
        Ok(e) => {
            tracing::info!(target: crate::auth::AUDIT, event = "ereader.rotate", ereader = %e.id, user = ?v.user);
            Json(json!({"password": secret})).into_response()
        }
        Err(e) => fail(e),
    }
}

async fn revoke_ereader(State(st): State<OpdsState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = owner_gate(&st, &v, &headers) {
        return r;
    }
    match with_store(&st.store, v.scope, move |s| s.ereader_revoke(id, now())).await {
        Ok(()) => {
            tracing::info!(target: crate::auth::AUDIT, event = "ereader.revoke", ereader = %id, user = ?v.user);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => fail(e),
    }
}

// ---- book upload tokens (/api/upload-tokens, a person) ----

fn upload_gate(st: &OpdsState, v: &Viewer, headers: &HeaderMap) -> Result<Uuid, Response> {
    owner_gate_for(st, v, headers, "minting an upload token is done by a person, not an agent")
}

/// The token as the owner routes report it: never a hash, never the secret
/// (that exists only in the create response).
fn upload_token_json(t: &UploadToken) -> Value {
    json!({
        "id": t.id,
        "label": t.label,
        "created_at": rfc3339(t.created_at),
        "last_used_at": t.last_used_at.map(rfc3339),
        "revoked_at": t.revoked_at.map(rfc3339),
    })
}

async fn list_upload_tokens(State(st): State<OpdsState>, v: Viewer, headers: HeaderMap) -> Response {
    let user = match upload_gate(&st, &v, &headers) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match with_store(&st.store, v.scope, move |s| s.upload_token_list(user)).await {
        Ok(all) => Json(json!({"upload_tokens": all.iter().map(upload_token_json).collect::<Vec<_>>()})).into_response(),
        Err(e) => fail(e),
    }
}

#[derive(Deserialize)]
struct CreateUploadTokenReq {
    label: String,
}

async fn create_upload_token(
    State(st): State<OpdsState>,
    v: Viewer,
    headers: HeaderMap,
    body: Result<Json<CreateUploadTokenReq>, JsonRejection>,
) -> Response {
    if let Err(r) = upload_gate(&st, &v, &headers) {
        return r;
    }
    let req = match json_or_400(body) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let label = match clean_label(&req.label) {
        Ok(l) => l,
        Err(r) => return r,
    };
    let token = crate::auth::new_upload_token();
    let hash = hash_secret(&token);
    let id = Uuid::now_v7();
    match with_store(&st.store, v.scope, move |s| s.upload_token_insert(id, &label, &hash, now())).await {
        Ok(t) => {
            tracing::info!(target: crate::auth::AUDIT, event = "upload_token.create", token = %t.id, label = %t.label.as_str(), user = ?v.user);
            (StatusCode::CREATED, Json(json!({"upload_token": upload_token_json(&t), "token": token}))).into_response()
        }
        Err(e) => fail(e),
    }
}

async fn revoke_upload_token(State(st): State<OpdsState>, v: Viewer, headers: HeaderMap, Path(id): Path<Uuid>) -> Response {
    if let Err(r) = upload_gate(&st, &v, &headers) {
        return r;
    }
    match with_store(&st.store, v.scope, move |s| s.upload_token_revoke(id, now())).await {
        Ok(()) => {
            tracing::info!(target: crate::auth::AUDIT, event = "upload_token.revoke", token = %id, user = ?v.user);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => fail(e),
    }
}

// ---- the device layer (/opds, basic auth or the path form) ----

/// A resolved OPDS client, inserted into request extensions by
/// [`opds_auth`]: whose shelf the request reads, and the link prefix that
/// keeps the client's own form (`/opds` for basic auth, `/opds/{secret}`
/// for the path form) so follow-up links carry its credentials. The
/// download routes (`/opds/book/{id}/file`, `/cover`) read this too — the
/// ereader row carries `stream_downloads`.
#[derive(Debug, Clone)]
pub struct OpdsClient {
    pub ereader: Ereader,
    pub prefix: String,
}

/// `Authorization: Basic …` as (username, password): the scheme is
/// case-insensitive, the payload `user:password` split on the first colon.
fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let raw = base64::engine::general_purpose::STANDARD.decode(rest.trim()).ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (user, pass) = s.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn auth_challenge() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "authentication required").into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(BASIC_CHALLENGE));
    r
}

fn too_many() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "too many requests; try again shortly").into_response()
}

/// One wrong-or-missing-credential answer for every failure (a guess learns
/// nothing), spending from the stricter per-IP bucket first. The path is
/// never logged: in the path form it would be the password.
fn auth_failed(limiter: &Limiter, ip: &str, form: &'static str) -> Response {
    tracing::warn!(target: crate::auth::AUDIT, event = "ereader.auth_failed", ip, form);
    if !limiter.allow(Class::OpdsAuth, ip) {
        return too_many();
    }
    auth_challenge()
}

/// The `/opds` auth layer (also the download routes' layer, when the
/// download agent mounts beside this router). Resolution order: a
/// secret-shaped second path segment is the path form (`/opds/{secret}/…`);
/// anything else is the basic-auth form. Both resolve to the same ereader
/// row — basic additionally binds the username to the password's owner
/// (matched case-insensitively, as auth finds users everywhere) — then the
/// request runs as `Scope::User(ereader.user_id)` downstream of here.
pub(crate) async fn opds_auth(State(st): State<OpdsState>, mut req: Request, next: Next) -> Response {
    let Some(server) = &st.server else { return not_found() };
    let ip = ip_key(&client_ip(&server.cfg, req.headers(), req.extensions()));
    if !server.limiter.allow(Class::Opds, &ip) {
        return too_many();
    }
    let path = req.uri().path().to_string();
    let seg2 = path.split('/').nth(2).unwrap_or("");
    let (secret, username, prefix, form) = if looks_like_secret(seg2) {
        (undash(seg2), None, format!("/opds/{seg2}"), "path")
    } else {
        match basic_credentials(req.headers()) {
            Some((u, p)) => (undash(&p), Some(u), "/opds".to_string(), "basic"),
            // no Authorization header at all is the challenge round-trip
            // every client makes first, not a guess: it answers the
            // challenge without spending the strict failure bucket (the
            // per-IP `Class::Opds` limit above still holds)
            None => return auth_challenge(),
        }
    };
    let hash = hash_secret(&secret);
    // per device (keyed on the hash, before the store sees anything): many
    // addresses hammering one password
    if !server.limiter.allow(Class::OpdsDevice, &hash) {
        return too_many();
    }
    let resolved = with_store(&st.store, Scope::System, move |s| -> taisce_store::Result<Option<Ereader>> {
        let Some(e) = s.ereader_find_by_secret_hash(&hash)? else { return Ok(None) };
        // paranoia: the lookup was by this hash; a collation quirk stops here
        if !ct_eq(&e.secret_hash, &hash) {
            return Ok(None);
        }
        if let Some(name) = &username {
            let owner = s.auth_user(e.user_id)?;
            if !owner.is_some_and(|o| o.name.eq_ignore_ascii_case(name.trim())) {
                return Ok(None);
            }
        }
        // "last fetched the feed", throttled to a write a minute by the store
        s.ereader_touch_last_used(e.id, now())?;
        Ok(Some(e))
    })
    .await;
    match resolved {
        Ok(Some(ereader)) => {
            req.extensions_mut().insert(OpdsClient { ereader, prefix });
            next.run(req).await
        }
        Ok(None) => auth_failed(&server.limiter, &ip, form),
        Err(e) => {
            tracing::error!("opds: resolving a device password failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "something went wrong").into_response()
        }
    }
}

// ---- feed XML (hand-built Atom; every title/name/label escaped) ----

/// Percent-encode for a path segment or query value: everything outside the
/// unreserved set, each UTF-8 byte on its own.
fn url_enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(*b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The head of every feed: declaration, <feed>, identity. `title` is raw
/// text (escaped here); `id` a stable URN.
fn feed_head(id: &str, title: &str, updated: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\" xmlns:opds=\"http://opds-spec.org/2010/catalog\">\n\
         <id>{}</id>\n\
         <title>{}</title>\n\
         <updated>{updated}</updated>\n",
        esc(id),
        esc(title),
    )
}

/// The links every feed carries: itself, the catalog root, the search
/// description.
fn spine_links(x: &mut String, prefix: &str, self_href: &str, self_ct: &str) {
    x.push_str(&format!("<link rel=\"self\" type=\"{self_ct}\" href=\"{}\"/>\n", esc(self_href)));
    x.push_str(&format!("<link rel=\"start\" type=\"{NAV_CT}\" href=\"{prefix}/\"/>\n"));
    x.push_str(&format!("<link rel=\"search\" type=\"{OPENSEARCH_CT}\" href=\"{prefix}/opensearch.xml\"/>\n"));
}

/// One navigation entry (a shelf, or the search description).
fn nav_entry_xml(id: &str, title: &str, href: &str, rel: &str, ct: &str, content: &str, updated: &str) -> String {
    let content = if content.is_empty() { String::new() } else { format!("<content type=\"text\">{}</content>\n", esc(content)) };
    format!(
        "<entry>\n\
         <id>{}</id>\n\
         <title>{}</title>\n\
         <updated>{updated}</updated>\n\
         <link rel=\"{rel}\" href=\"{}\" type=\"{ct}\"/>\n\
         {content}</entry>",
        esc(id),
        esc(title),
        esc(href),
    )
}

/// "2" for 2.0, "0.5" for 0.5: series indexes are floats in the db, readers
/// expect plain numerals.
fn fmt_series_index(i: f64) -> String {
    if i.fract() == 0.0 && i.abs() < 9.0e15 { format!("{}", i as i64) } else { format!("{i}") }
}

/// One book as an acquisition entry. `link_prefix` is the catalog root the
/// client came in on (`/opds`, or `/opds/{secret}` in the path form): cover
/// and file links keep that form so its credentials carry over to the
/// download routes.
pub(crate) fn book_entry_xml(book: &Book, link_prefix: &str) -> String {
    // the title/author the devices name the download from: booknaming's
    // feed form, so both clients derive exactly `derived_book_filename`
    // (the kosync key). `<author>` is always present, an empty `<name>` for
    // a book without one: KOReader turns a MISSING author into "Unknown
    // Author" and would name the file after that.
    let (title, author) = crate::booknaming::feed_title(&book.title, &book.author);
    let mut x = format!(
        "<entry>\n\
         <id>urn:uuid:{}</id>\n\
         <title>{}</title>\n\
         <author><name>{}</name></author>\n\
         <updated>{}</updated>\n\
         <published>{}</published>\n",
        book.id,
        esc(&title),
        esc(&author),
        rfc3339(book.updated_at),
        rfc3339(book.added_at),
    );
    // the series as a category (calibre's convention), its index in the label
    if let Some(series) = book.series.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let label = match book.series_index {
            Some(i) => format!("{series} #{}", fmt_series_index(i)),
            None => series.to_string(),
        };
        x.push_str(&format!("<category term=\"{}\" label=\"{}\"/>\n", esc(series), esc(&label)));
    }
    // the cover links are on every entry (a coverless book's cover route
    // 404s, and the clients show their own title card for it)
    x.push_str(&format!(
        "<link rel=\"http://opds-spec.org/image\" href=\"{link_prefix}/book/{}/cover\"/>\n\
         <link rel=\"http://opds-spec.org/image/thumbnail\" href=\"{link_prefix}/book/{}/cover\"/>\n",
        book.id, book.id
    ));
    x.push_str(&format!(
        "<link rel=\"http://opds-spec.org/acquisition\" type=\"application/epub+zip\" href=\"{link_prefix}/book/{}/file\" length=\"{}\"/>\n\
         </entry>",
        book.id, book.size
    ));
    x
}

/// The top navigation feed: New, All, Authors, Series and Search (the
/// OpenSearch description).
fn root_xml(prefix: &str, updated: &str, docs: bool) -> String {
    let mut x = feed_head("urn:taisce:books", "Taisce books", updated);
    spine_links(&mut x, prefix, &format!("{prefix}/"), NAV_CT);
    // the person's docs sent to e-readers (sentdocs.rs), first, only when any
    if docs {
        x.push_str(&nav_entry_xml(
            "urn:taisce:docs",
            "Docs",
            &format!("{prefix}/docs"),
            "subsection",
            ACQ_CT,
            "Docs sent from Taisce",
            updated,
        ));
        x.push('\n');
    }
    for (id, title, href, rel, ct, content) in [
        ("urn:taisce:books:new", "New", format!("{prefix}/new"), "subsection", ACQ_CT, "The newest books"),
        ("urn:taisce:books:all", "All", format!("{prefix}/all"), "subsection", ACQ_CT, "Every book, by title"),
        ("urn:taisce:books:authors", "Authors", format!("{prefix}/authors"), "subsection", NAV_CT, "Books by author"),
        ("urn:taisce:books:series", "Series", format!("{prefix}/series"), "subsection", NAV_CT, "Books by series"),
        ("urn:taisce:books:search", "Search", format!("{prefix}/opensearch.xml"), "search", OPENSEARCH_CT, "Search title, author and series"),
    ] {
        x.push_str(&nav_entry_xml(id, title, &href, rel, ct, content, updated));
        x.push('\n');
    }
    x.push_str("</feed>");
    x
}

/// A page of books as an acquisition feed with the catalog's spine links.
fn acq_xml(prefix: &str, id: &str, title: &str, self_href: &str, next_href: Option<&str>, up_href: Option<&str>, books: &[Book], updated: &str) -> String {
    let mut x = feed_head(id, title, updated);
    spine_links(&mut x, prefix, self_href, ACQ_CT);
    if let Some(up) = up_href {
        x.push_str(&format!("<link rel=\"up\" type=\"{NAV_CT}\" href=\"{}\"/>\n", esc(up)));
    }
    if let Some(next) = next_href {
        x.push_str(&format!("<link rel=\"next\" type=\"{ACQ_CT}\" href=\"{}\"/>\n", esc(next)));
    }
    for b in books {
        x.push_str(&book_entry_xml(b, prefix));
        x.push('\n');
    }
    x.push_str("</feed>");
    x
}

/// The distinct values of one book field as a navigation feed of filtered
/// feeds (`/opds/authors`, `/opds/series`), each entry carrying its count.
fn index_xml(prefix: &str, feed_segment: &str, link_segment: &str, title: &str, entries: &[(String, usize)], updated: &str) -> String {
    let mut x = feed_head(&format!("urn:taisce:books:{feed_segment}"), title, updated);
    spine_links(&mut x, prefix, &format!("{prefix}/{feed_segment}"), NAV_CT);
    for (name, n) in entries {
        let key = name.to_ascii_lowercase();
        let content = if *n == 1 { "1 book".to_string() } else { format!("{n} books") };
        x.push_str(&nav_entry_xml(
            &format!("urn:taisce:books:{feed_segment}:{}", url_enc(&key)),
            name,
            &format!("{prefix}/{link_segment}/{}", url_enc(name)),
            "subsection",
            ACQ_CT,
            &content,
            updated,
        ));
        x.push('\n');
    }
    x.push_str("</feed>");
    x
}

/// The OpenSearch description: KOReader shows it as the catalog's search
/// box, substituting `{searchTerms}`.
fn opensearch_xml(prefix: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <OpenSearchDescription xmlns=\"http://a9.com/-/spec/opensearch/1.1/\">\n\
         <ShortName>Taisce books</ShortName>\n\
         <Description>Search the Taisce EPUB library</Description>\n\
         <InputEncoding>UTF-8</InputEncoding>\n\
         <Url type=\"{ACQ_CT}\" template=\"{}/search?q={{searchTerms}}\"/>\n\
         </OpenSearchDescription>",
        esc(prefix)
    )
}

fn xml(ct: &'static str, body: String) -> Response {
    let mut r = (StatusCode::OK, [(header::CONTENT_TYPE, ct)], body).into_response();
    let h = r.headers_mut();
    // a person's own library: never cached, never indexed, never a document
    // scripts could ride in on
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    r
}

// ---- feed handlers ----

#[derive(Deserialize)]
struct PageQ {
    /// the keyset cursor `added_at,id` (new / author / series / search)
    #[serde(default)]
    cursor: Option<String>,
    /// the search text (search)
    #[serde(default)]
    q: Option<String>,
    /// the offset (all: its order is computed, so a keyset cannot name a page)
    #[serde(default)]
    start: Option<usize>,
}

fn parse_cursor(c: Option<&str>) -> Result<BookPage, Response> {
    let Some(c) = c.filter(|c| !c.trim().is_empty()) else { return Ok(BookPage::default()) };
    let bad = || err(StatusCode::BAD_REQUEST, "cursor: expected \"<added_at>,<book id>\"");
    let Some((at, id)) = c.split_once(',') else { return Err(bad()) };
    match (at.trim().parse::<i64>(), Uuid::parse_str(id.trim())) {
        (Ok(at), Ok(id)) => Ok(BookPage { before: Some((at, id)), after: None }),
        _ => Err(bad()),
    }
}

/// What a keyset-paged feed narrows to (the store's [`BookFilter`], owned).
#[derive(Clone)]
enum Narrow {
    None,
    Search(String),
    Author(String),
    Series(String),
}

/// What one keyset-paged acquisition feed is: its identity, its address
/// (without the page cursor), its parent, its store-level search.
struct FeedRef {
    id: String,
    title: String,
    self_base: String,
    up: Option<String>,
    narrow: Narrow,
}

/// Fetch the page the cursor names — one extra row decides whether a `next`
/// link exists, and the page's last book is the next cursor — and render it.
async fn paged_feed(st: &OpdsState, client: &OpdsClient, feed: FeedRef, q: &PageQ) -> Response {
    let cursor = match parse_cursor(q.cursor.as_deref()) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let user = client.ereader.user_id;
    let narrow = feed.narrow.clone();
    let books = with_store(&st.store, Scope::User(user), move |s| {
        let filter = match &narrow {
            Narrow::None => BookFilter::All,
            Narrow::Search(q) => BookFilter::Search(q),
            Narrow::Author(a) => BookFilter::Author(a),
            Narrow::Series(v) => BookFilter::Series(v),
        };
        s.book_list_where(user, &cursor, PAGE + 1, filter)
    })
    .await;
    let mut books = match books {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let sep = if feed.self_base.contains('?') { '&' } else { '?' };
    let next_href = if books.len() > PAGE {
        books.truncate(PAGE);
        books.last().map(|b| format!("{}{sep}cursor={}", feed.self_base, url_enc(&format!("{},{}", b.added_at, b.id))))
    } else {
        None
    };
    let self_href = match q.cursor.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(c) => format!("{}{sep}cursor={}", feed.self_base, url_enc(c)),
        None => feed.self_base,
    };
    xml(ACQ_CT, acq_xml(&client.prefix, &feed.id, &feed.title, &self_href, next_href.as_deref(), feed.up.as_deref(), &books, &rfc3339(now())))
}

async fn root_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>) -> Response {
    let user = client.ereader.user_id;
    let docs = match with_store(&st.store, Scope::User(user), move |s| s.ereader_sent_docs()).await {
        Ok(d) => !d.is_empty(),
        Err(e) => return fail(e),
    };
    xml(NAV_CT, root_xml(&client.prefix, &rfc3339(now()), docs))
}

// ---- docs sent to e-readers (sentdocs.rs renders; ONLY docs the device's
// owner tagged `ereader` themselves, in Scope::User(owner)) ----

/// One sent doc as an acquisition entry: the feed title and the author
/// "Taisce" are exactly what both clients derive `Taisce - <Title>.epub`
/// from (booknaming's rule, the kosync key).
fn doc_entry_xml(b: &crate::sentdocs::SentBook, size: usize, prefix: &str) -> String {
    let n = b.spec.chapters.len();
    let mut content = if n == 1 { "1 chapter".to_string() } else { format!("{n} chapters") };
    if let Some(w) = &b.workspace {
        content.push_str(&format!(" \u{b7} {w}"));
    }
    format!(
        "<entry>\n\
         <id>urn:uuid:{}</id>\n\
         <title>{}</title>\n\
         <author><name>{}</name></author>\n\
         <updated>{}</updated>\n\
         <content type=\"text\">{}</content>\n\
         <link rel=\"http://opds-spec.org/acquisition\" type=\"application/epub+zip\" href=\"{prefix}/doc/{}/file\" length=\"{size}\"/>\n\
         </entry>",
        b.spec.id,
        esc(&b.feed_title),
        crate::sentdocs::FEED_AUTHOR,
        esc(&b.spec.modified),
        esc(&content),
        b.spec.id,
    )
}

async fn docs_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>) -> Response {
    let user = client.ereader.user_id;
    let books = match with_store(&st.store, Scope::User(user), move |s| crate::sentdocs::sent_books(s, Some(user))).await {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let prefix = &client.prefix;
    let mut x = feed_head("urn:taisce:docs", "Docs", &rfc3339(now()));
    spine_links(&mut x, prefix, &format!("{prefix}/docs"), ACQ_CT);
    x.push_str(&format!("<link rel=\"up\" type=\"{NAV_CT}\" href=\"{prefix}/\"/>\n"));
    for b in &books {
        let size = match crate::epub::CACHE.get_or_render(&b.spec) {
            Ok(bytes) => bytes.len(),
            Err(e) => return crate::sentdocs::render_failed(e),
        };
        x.push_str(&doc_entry_xml(b, size, prefix));
        x.push('\n');
    }
    x.push_str("</feed>");
    xml(ACQ_CT, x)
}

/// `GET|HEAD /opds/doc/{id}/file`: a sent doc of the device's owner,
/// rendered on demand (cached by content), streamed from the daemon. Any
/// other id — untagged, tagged by someone else, a chapter of a sent folder,
/// invisible — is the same 404.
async fn doc_file(st: &OpdsState, client: &OpdsClient, id: Uuid) -> Response {
    let user = client.ereader.user_id;
    let found = with_store(&st.store, Scope::User(user), move |s| -> taisce_store::Result<Option<(crate::epub::BookSpec, String)>> {
        match s.ereader_sent(id) {
            Ok(true) => {}
            Ok(false) | Err(StoreError::NotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        }
        Ok(crate::sentdocs::sent_books(s, Some(user))?.into_iter().find(|b| b.spec.id == id).map(|b| (b.spec, b.filename)))
    })
    .await;
    match found {
        Ok(Some((spec, name))) => match crate::epub::CACHE.get_or_render(&spec) {
            Ok(bytes) => crate::sentdocs::epub_response(&bytes, &name),
            Err(e) => crate::sentdocs::render_failed(e),
        },
        Ok(None) => not_found(),
        Err(e) => fail(e),
    }
}

async fn doc_file_dl(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Path(id): Path<Uuid>) -> Response {
    doc_file(&st, &client, id).await
}

async fn doc_file_dl_s(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Path((_, id)): Path<(String, Uuid)>) -> Response {
    doc_file(&st, &client, id).await
}

async fn opensearch_doc(Extension(client): Extension<OpdsClient>) -> Response {
    xml(OPENSEARCH_CT, opensearch_xml(&client.prefix))
}

async fn new_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Query(q): Query<PageQ>) -> Response {
    let prefix = client.prefix.clone();
    paged_feed(
        &st,
        &client,
        FeedRef { id: "urn:taisce:books:new".into(), title: "New".into(), self_base: format!("{prefix}/new"), up: None, narrow: Narrow::None },
        &q,
    )
    .await
}

/// `/all` is title-sorted: the store orders by title and the feed pages by
/// offset (`start`) — a keyset cannot name a page of a computed order. One
/// extra row decides whether a `next` link exists; no cap, every book is
/// reachable.
async fn all_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Query(q): Query<PageQ>) -> Response {
    let start = q.start.unwrap_or(0);
    let user = client.ereader.user_id;
    let mut page = match with_store(&st.store, Scope::User(user), move |s| s.book_list_by_title(user, start, PAGE + 1)).await {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let more = page.len() > PAGE;
    page.truncate(PAGE);
    let prefix = &client.prefix;
    let base = format!("{prefix}/all");
    let self_href = if start == 0 { base.clone() } else { format!("{base}?start={start}") };
    let next_href = more.then(|| format!("{base}?start={}", start + PAGE));
    xml(ACQ_CT, acq_xml(prefix, "urn:taisce:books:all", "All", &self_href, next_href.as_deref(), None, &page, &rfc3339(now())))
}

/// An author or series index: one navigation entry per distinct name
/// (trimmed, case-insensitive), the whole shelf, no cap.
async fn index_feed(st: &OpdsState, client: &OpdsClient, field: BookField) -> Response {
    let user = client.ereader.user_id;
    let names = match with_store(&st.store, Scope::User(user), move |s| s.book_index(user, field)).await {
        Ok(n) => n,
        Err(e) => return fail(e),
    };
    let (feed, link, title) = match field {
        BookField::Author => ("authors", "author", "Authors"),
        BookField::Series => ("series", "series", "Series"),
    };
    xml(NAV_CT, index_xml(&client.prefix, feed, link, title, &names, &rfc3339(now())))
}

async fn authors_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>) -> Response {
    index_feed(&st, &client, BookField::Author).await
}

async fn series_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>) -> Response {
    index_feed(&st, &client, BookField::Series).await
}

/// One author's books: exactly that author (trimmed, case-insensitive — the
/// key the index grouped by), never a substring of a title or series.
async fn author_books(st: &OpdsState, client: &OpdsClient, name: String, q: &PageQ) -> Response {
    let prefix = client.prefix.clone();
    paged_feed(
        st,
        client,
        FeedRef {
            id: format!("urn:taisce:books:author:{}", url_enc(&name.to_lowercase())),
            title: name.clone(),
            self_base: format!("{prefix}/author/{}", url_enc(&name)),
            up: Some(format!("{prefix}/authors")),
            narrow: Narrow::Author(name),
        },
        q,
    )
    .await
}

async fn author_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Path(name): Path<String>, Query(q): Query<PageQ>) -> Response {
    author_books(&st, &client, name, &q).await
}

/// The path-form variant (the first segment is the device password, which
/// the auth layer already resolved).
async fn author_feed_s(
    State(st): State<OpdsState>,
    Extension(client): Extension<OpdsClient>,
    Path((_, name)): Path<(String, String)>,
    Query(q): Query<PageQ>,
) -> Response {
    author_books(&st, &client, name, &q).await
}

async fn series_books(st: &OpdsState, client: &OpdsClient, name: String, q: &PageQ) -> Response {
    let prefix = client.prefix.clone();
    paged_feed(
        st,
        client,
        FeedRef {
            id: format!("urn:taisce:books:series:{}", url_enc(&name.to_lowercase())),
            title: name.clone(),
            self_base: format!("{prefix}/series/{}", url_enc(&name)),
            up: Some(format!("{prefix}/series")),
            narrow: Narrow::Series(name),
        },
        q,
    )
    .await
}

async fn series_books_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Path(name): Path<String>, Query(q): Query<PageQ>) -> Response {
    series_books(&st, &client, name, &q).await
}

/// The path-form variant.
async fn series_books_feed_s(
    State(st): State<OpdsState>,
    Extension(client): Extension<OpdsClient>,
    Path((_, name)): Path<(String, String)>,
    Query(q): Query<PageQ>,
) -> Response {
    series_books(&st, &client, name, &q).await
}

async fn search_feed(State(st): State<OpdsState>, Extension(client): Extension<OpdsClient>, Query(q): Query<PageQ>) -> Response {
    let query = q.q.as_deref().unwrap_or("").trim().to_string();
    let prefix = client.prefix.clone();
    let title = if query.is_empty() { "Search".to_string() } else { format!("Search: {query}") };
    paged_feed(
        &st,
        &client,
        FeedRef {
            id: "urn:taisce:books:search".into(),
            title,
            self_base: format!("{prefix}/search?q={}", url_enc(&query)),
            up: None,
            narrow: Narrow::Search(query),
        },
        &q,
    )
    .await
}

// ---- the download routes (opds resolves the device, library serves the
// bytes — the contract in library.rs's "downloads" section) ----

/// `GET|HEAD /opds/book/{id}/file`: the ereader was resolved by the
/// `opds_auth` layer (its `stream_downloads` picks the transport).
async fn book_file_dl(
    State(st): State<OpdsState>,
    Extension(client): Extension<OpdsClient>,
    Path(id): Path<Uuid>,
    method: axum::http::Method,
) -> Response {
    crate::library::book_file(&st.library, &client.ereader, id, &method).await
}

async fn book_cover_dl(
    State(st): State<OpdsState>,
    Extension(client): Extension<OpdsClient>,
    Path(id): Path<Uuid>,
    method: axum::http::Method,
) -> Response {
    crate::library::book_cover(&st.library, &client.ereader, id, &method).await
}

/// The path-form variants (the first segment is the device password, which
/// the auth layer already resolved).
async fn book_file_dl_s(
    State(st): State<OpdsState>,
    Extension(client): Extension<OpdsClient>,
    Path((_, id)): Path<(String, Uuid)>,
    method: axum::http::Method,
) -> Response {
    crate::library::book_file(&st.library, &client.ereader, id, &method).await
}

async fn book_cover_dl_s(
    State(st): State<OpdsState>,
    Extension(client): Extension<OpdsClient>,
    Path((_, id)): Path<(String, Uuid)>,
    method: axum::http::Method,
) -> Response {
    crate::library::book_cover(&st.library, &client.ereader, id, &method).await
}

pub fn router(state: OpdsState) -> Router {
    let owner = Router::new()
        .route("/api/ereaders", get(list_ereaders).post(create_ereader))
        .route("/api/ereaders/{id}/rotate", post(rotate_ereader))
        .route("/api/ereaders/{id}", delete(revoke_ereader))
        .route("/api/upload-tokens", get(list_upload_tokens).post(create_upload_token))
        .route("/api/upload-tokens/{id}", delete(revoke_upload_token))
        // labels and flags only
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        // connectors write only through the gated routes (ADR 0004)
        .layer(axum::middleware::from_fn(crate::viewer::refuse_connector_writes));
    // every feed route twice: the basic-auth form, then the path form (the
    // secret in the URL). Static segments win over {secret}, and the minted
    // shape (xxxx-xxxx-xxxx-xxxx) never collides with one of them.
    let feeds = Router::new()
        .route("/opds", get(root_feed))
        .route("/opds/", get(root_feed))
        .route("/opds/opensearch.xml", get(opensearch_doc))
        .route("/opds/new", get(new_feed))
        .route("/opds/all", get(all_feed))
        .route("/opds/authors", get(authors_feed))
        .route("/opds/author/{name}", get(author_feed))
        .route("/opds/series", get(series_feed))
        .route("/opds/series/{name}", get(series_books_feed))
        .route("/opds/search", get(search_feed))
        .route("/opds/docs", get(docs_feed))
        .route("/opds/doc/{id}/file", get(doc_file_dl))
        .route("/opds/{secret}/docs", get(docs_feed))
        .route("/opds/{secret}/doc/{id}/file", get(doc_file_dl_s))
        .route("/opds/{secret}", get(root_feed))
        .route("/opds/{secret}/", get(root_feed))
        .route("/opds/{secret}/opensearch.xml", get(opensearch_doc))
        .route("/opds/{secret}/new", get(new_feed))
        .route("/opds/{secret}/all", get(all_feed))
        .route("/opds/{secret}/authors", get(authors_feed))
        .route("/opds/{secret}/author/{name}", get(author_feed_s))
        .route("/opds/{secret}/series", get(series_feed))
        .route("/opds/{secret}/series/{name}", get(series_books_feed_s))
        .route("/opds/{secret}/search", get(search_feed))
        // the downloads (`get` answers HEAD too; the handler signs a HEAD
        // URL of its own): basic-auth form, then the path form. Static
        // "book" wins over {secret}, which never has its shape anyway.
        .route("/opds/book/{id}/file", get(book_file_dl))
        .route("/opds/book/{id}/cover", get(book_cover_dl))
        .route("/opds/{secret}/book/{id}/file", get(book_file_dl_s))
        .route("/opds/{secret}/book/{id}/cover", get(book_cover_dl_s))
        .layer(axum::middleware::from_fn_with_state(state.clone(), opds_auth));
    Router::new().merge(owner).merge(feeds).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(title: &str, author: &str, series: Option<&str>, index: Option<f64>) -> Book {
        Book {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            title: title.into(),
            author: author.into(),
            series: series.map(Into::into),
            series_index: index,
            book_identifier: None,
            original_filename: format!("{title}.epub"),
            file_sha256: "ab".repeat(32),
            size: 12345,
            file_s3_key: "library/u/x.epub".into(),
            cover_s3_key: None,
            added_at: 1_700_000_000,
            updated_at: 1_700_100_000,
            deleted_at: None,
            cover_grey: None,
            isbns: None,
            color_cover_s3_key: None,
            cover_choice: Default::default(),
            cover_choice_by: None,
            cover_choice_at: None,
            color_lookup_at: None,
            color_lookup_matcher: None,
        }
    }

    #[test]
    fn minted_secrets_are_grouped_base32_and_unique() {
        let s = mint_device_secret();
        assert_eq!(s.len(), 19, "{s}");
        assert_eq!(s.chars().filter(|c| *c == '-').count(), 3);
        let raw = undash(&s);
        assert_eq!(raw.len(), SECRET_LEN);
        assert!(raw.bytes().all(|b| BASE32.contains(&b)));
        assert_ne!(mint_device_secret(), s);
        assert!(looks_like_secret(&s));
        assert!(looks_like_secret(&raw));
        assert!(looks_like_secret(&s.to_uppercase()), "case-insensitive");
        assert!(!looks_like_secret("new"));
        assert!(!looks_like_secret("k7mq-4xpt-9wre-hz2!"));
        assert!(!looks_like_secret(""));
    }

    #[test]
    fn hashes_cover_both_typed_forms_of_the_kosync_key() {
        let (h, (dashed, raw)) = device_secret_hashes("k7mq-4xpt-9wre-hz2c");
        assert_eq!(h, hash_secret("k7mq4xpt9wrehz2c"), "basic auth keys on the canonical form");
        // the device sends md5(password as typed): both typings resolve
        assert_eq!(dashed, hash_secret(&crate::booknaming::md5_hex(b"k7mq-4xpt-9wre-hz2c")));
        assert_eq!(raw, hash_secret(&crate::booknaming::md5_hex(b"k7mq4xpt9wrehz2c")));
        assert_ne!(dashed, raw);
        assert!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }

    /// The real mint path end to end: pair a device through /api/ereaders,
    /// take the password the person is shown, and sign in the way each
    /// client does — KOSYNC with md5(password exactly as typed), dashed or
    /// not; OPDS basic auth with the password however the keyboard
    /// capitalized it.
    #[tokio::test]
    async fn a_minted_password_signs_in_to_kosync_and_opds_as_typed() {
        use crate::auth::Authenticated;
        use axum::body::Body;
        use axum::http::Request;
        use taisce_store::{BlockStore, PrincipalKind, SqliteStore};
        use tower::ServiceExt;
        let mut raw = SqliteStore::open_in_memory().unwrap();
        let human = raw.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let user = raw.auth_ensure_owner(human, "Tom", 1).unwrap().id;
        let store = SharedStore::new(raw);
        let dir = tempfile::tempdir().unwrap();
        let library = crate::library::LibraryState {
            store: store.clone(),
            s3: None,
            server_mode: true,
            local_human: human,
            spool_dir: dir.path().join("spool"),
            covers: std::sync::Arc::new(crate::covers::OpenLibrary::for_test()),
        };
        let app = router(OpdsState {
            store: store.clone(),
            server: Some(OpdsServer {
                cfg: Arc::new(AuthConfig::from_public_url("http://localhost:7513").unwrap()),
                limiter: Limiter::default(),
            }),
            local_human: human,
            library,
        })
        .merge(crate::kosync::router(crate::kosync::KosyncState {
            store: store.clone(),
            server: Some(crate::kosync::ServerSide {
                cfg: Arc::new(AuthConfig::from_public_url("http://localhost:7513").unwrap()),
                limiter: Limiter::default(),
            }),
            local_human: human,
        }));
        let mint = Request::post("/api/ereaders")
            .header("content-type", "application/json")
            .extension(Authenticated {
                user_id: user,
                grant_id: Uuid::now_v7(),
                client_id: "test".into(),
                principal: "claude:test".into(),
                owner_app: true,
                human,
                instance_owner: true,
                web_session: false,
                book_upload: false,
            })
            .body(Body::from(r#"{"label":"Kobo"}"#))
            .unwrap();
        let res = app.clone().oneshot(mint).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let v: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap()).unwrap();
        let shown = v["password"].as_str().unwrap().to_string();
        assert!(shown.contains('-'), "the UI shows the dashed form: {shown}");
        let kosync = |typed: String| {
            Request::get("/kosync/users/auth")
                .header("x-auth-user", "tom")
                .header("x-auth-key", crate::booknaming::md5_hex(typed.as_bytes()))
                .body(Body::empty())
                .unwrap()
        };
        for typed in [shown.clone(), shown.replace('-', "")] {
            let res = app.clone().oneshot(kosync(typed.clone())).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "kosync with {typed:?}");
        }
        let res = app.clone().oneshot(kosync("not-the-password".into())).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // OPDS: an e-ink keyboard capitalizes the first letter
        let mut caps = shown.clone();
        caps[..1].make_ascii_uppercase();
        for typed in [shown.clone(), caps] {
            let basic = base64::engine::general_purpose::STANDARD.encode(format!("Tom:{typed}"));
            let req = Request::get("/opds/").header("authorization", format!("Basic {basic}")).body(Body::empty()).unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "opds with {typed:?}");
        }
    }

    #[test]
    fn ct_eq_compares_exactly() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("abcd", "abc"));
    }

    #[test]
    fn basic_auth_parses_and_splits_on_the_first_colon() {
        let mut h = HeaderMap::new();
        let enc = base64::engine::general_purpose::STANDARD.encode("tom:k7mq-4xpt-9wre-hz2c");
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Basic {enc}")).unwrap());
        assert_eq!(basic_credentials(&h), Some(("tom".to_string(), "k7mq-4xpt-9wre-hz2c".to_string())));
        // the scheme is case-insensitive; a password may itself contain colons
        let enc = base64::engine::general_purpose::STANDARD.encode("tom:a:b");
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("basic {enc}")).unwrap());
        assert_eq!(basic_credentials(&h), Some(("tom".to_string(), "a:b".to_string())));
        assert!(basic_credentials(&HeaderMap::new()).is_none());
        let enc = base64::engine::general_purpose::STANDARD.encode("nocolon");
        h.insert(header::AUTHORIZATION, HeaderValue::from_str(&format!("Basic {enc}")).unwrap());
        assert!(basic_credentials(&h).is_none());
    }

    #[test]
    fn cursor_round_trip() {
        let b = book("Dune", "Frank Herbert", None, None);
        let c = format!("{},{}", b.added_at, b.id);
        let p = parse_cursor(Some(c.as_str())).ok().unwrap();
        assert_eq!(p.before, Some((b.added_at, b.id)));
        assert_eq!(parse_cursor(None).ok().unwrap(), BookPage::default());
        assert_eq!(parse_cursor(Some("")).ok().unwrap(), BookPage::default());
        assert!(parse_cursor(Some("junk")).is_err());
        assert!(parse_cursor(Some("123,not-a-uuid")).is_err());
    }

    #[test]
    fn url_enc_covers_everything_outside_the_unreserved_set() {
        assert_eq!(url_enc("Frank Herbert"), "Frank%20Herbert");
        assert_eq!(url_enc("a/b?c&d=e"), "a%2Fb%3Fc%26d%3De");
        assert_eq!(url_enc("café"), "caf%C3%A9");
        assert_eq!(url_enc("ok-._~"), "ok-._~");
    }

    #[test]
    fn book_entry_escapes_and_links_keep_the_prefix() {
        let b = book("Tom & \"Jerry\" <Special>", "A & B", Some("Series <1>"), Some(2.0));
        let x = book_entry_xml(&b, "/opds/k7mq-4xpt-9wre-hz2c");
        assert!(x.contains(&format!("<id>urn:uuid:{}</id>", b.id)));
        assert!(x.contains("<title>Tom &amp; &quot;Jerry&quot; &lt;Special&gt;</title>"));
        assert!(x.contains("<name>A &amp; B</name>"));
        assert!(x.contains("<category term=\"Series &lt;1&gt;\" label=\"Series &lt;1&gt; #2\"/>"));
        assert!(x.contains("href=\"/opds/k7mq-4xpt-9wre-hz2c/book/"));
        assert!(x.contains("rel=\"http://opds-spec.org/acquisition\" type=\"application/epub+zip\""));
        assert!(x.contains("length=\"12345\""));
        assert!(x.contains("http://opds-spec.org/image/thumbnail"), "the cover links are on every entry");
        let basic = book_entry_xml(&book("T", "A", None, None), "/opds");
        assert!(basic.contains("href=\"/opds/book/"));
    }

    /// The feed serves booknaming's feed form, so both clients name the file
    /// exactly `derived_book_filename` (the kosync key); the stored title is
    /// untouched elsewhere.
    #[test]
    fn book_entry_serves_the_feed_form_of_title_and_author() {
        let b = book("  Line\none: two  ", "  .Ann  Author ", None, None);
        let x = book_entry_xml(&b, "/opds");
        assert!(x.contains("<title>Line one: two</title>"), "{x}");
        assert!(x.contains("<name>Ann Author</name>"), "{x}");
        // no author: the element is still there, with an empty name (a
        // MISSING author is "Unknown Author" to KOReader)
        let x = book_entry_xml(&book("Solo", "", None, None), "/opds");
        assert!(x.contains("<author><name></name></author>"), "{x}");
    }

    #[test]
    fn series_index_formats_like_a_numeral() {
        assert_eq!(fmt_series_index(2.0), "2");
        assert_eq!(fmt_series_index(0.5), "0.5");
        assert_eq!(fmt_series_index(2.25), "2.25");
    }

    #[test]
    fn root_feed_links_keep_the_prefix() {
        let x = root_xml("/opds/abcd-efgh-ijkl-mnop", "2026-01-01T00:00:00Z", false);
        for path in ["/new", "/all", "/authors", "/series", "/opensearch.xml"] {
            assert!(x.contains(&format!("href=\"/opds/abcd-efgh-ijkl-mnop{path}\"")), "{path}");
        }
        assert!(x.contains("rel=\"search\""));
        assert!(x.contains("kind=navigation"));
    }

    #[test]
    fn acquisition_feed_carries_self_start_search_and_next() {
        let books = vec![book("Dune", "Frank Herbert", None, None)];
        let x = acq_xml("/opds", "urn:taisce:books:new", "New", "/opds/new", Some("/opds/new?cursor=1%2Cx"), None, &books, "2026-01-01T00:00:00Z");
        assert!(x.contains("rel=\"self\" type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\" href=\"/opds/new\""));
        assert!(x.contains("rel=\"start\" type=\"application/atom+xml;profile=opds-catalog;kind=navigation\" href=\"/opds/\""));
        assert!(x.contains("rel=\"search\""));
        assert!(x.contains("rel=\"next\" type=\"application/atom+xml;profile=opds-catalog;kind=acquisition\" href=\"/opds/new?cursor=1%2Cx\""));
        assert!(x.contains("<title>New</title>"));
        assert!(x.contains("urn:uuid"));
    }

    #[test]
    fn opensearch_points_at_the_search_feed_in_the_clients_form() {
        let x = opensearch_xml("/opds/k7mq-4xpt-9wre-hz2c");
        assert!(x.contains("template=\"/opds/k7mq-4xpt-9wre-hz2c/search?q={searchTerms}\""));
        let x = opensearch_xml("/opds");
        assert!(x.contains("template=\"/opds/search?q={searchTerms}\""));
    }

    /// The challenge round-trip (no Authorization header) never spends the
    /// strict failure bucket; a wrong credential does, and a failed KOSYNC
    /// sign-in spends the same bucket.
    #[tokio::test]
    async fn only_a_wrong_credential_spends_the_auth_failure_bucket() {
        use axum::body::Body;
        use axum::http::Request;
        use taisce_store::{BlockStore, PrincipalKind, SqliteStore};
        use tower::ServiceExt;
        let mut raw = SqliteStore::open_in_memory().unwrap();
        let human = raw.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        raw.auth_ensure_owner(human, "Tom", 1).unwrap();
        let store = SharedStore::new(raw);
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(AuthConfig::from_public_url("http://localhost:7513").unwrap());
        let limiter = Limiter::default();
        let library = crate::library::LibraryState {
            store: store.clone(),
            s3: None,
            server_mode: true,
            local_human: human,
            spool_dir: dir.path().join("spool"),
            covers: std::sync::Arc::new(crate::covers::OpenLibrary::for_test()),
        };
        let app = router(OpdsState {
            store: store.clone(),
            server: Some(OpdsServer { cfg: cfg.clone(), limiter: limiter.clone() }),
            local_human: human,
            library,
        })
        .merge(crate::kosync::router(crate::kosync::KosyncState {
            store: store.clone(),
            server: Some(crate::kosync::ServerSide { cfg, limiter }),
            local_human: human,
        }));
        for _ in 0..30 {
            let res = app.clone().oneshot(Request::get("/opds/").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "a bare request is the challenge, never 429");
            assert!(res.headers().contains_key(header::WWW_AUTHENTICATE));
        }
        let wrong = || {
            let basic = base64::engine::general_purpose::STANDARD.encode("Tom:k7mq-4xpt-9wre-hz2c");
            Request::get("/opds/").header("authorization", format!("Basic {basic}")).body(Body::empty()).unwrap()
        };
        // half the bucket on OPDS guesses, the rest on KOSYNC ones
        for _ in 0..5 {
            assert_eq!(app.clone().oneshot(wrong()).await.unwrap().status(), StatusCode::UNAUTHORIZED);
        }
        let kosync_wrong = || {
            Request::get("/kosync/users/auth")
                .header("x-auth-user", "Tom")
                .header("x-auth-key", "0123456789abcdef0123456789abcdef")
                .body(Body::empty())
                .unwrap()
        };
        let mut statuses = Vec::new();
        for _ in 0..8 {
            statuses.push(app.clone().oneshot(kosync_wrong()).await.unwrap().status());
        }
        assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS), "kosync failures share the bucket: {statuses:?}");
        assert_eq!(app.clone().oneshot(wrong()).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[test]
    fn index_feed_links_each_name_with_its_count() {
        let names = vec![("Frank Herbert".to_string(), 2), ("J. R. R. Tolkien".to_string(), 1)];
        let x = index_xml("/opds", "authors", "author", "Authors", &names, "2026-01-01T00:00:00Z");
        assert!(x.contains("href=\"/opds/author/Frank%20Herbert\""));
        assert!(x.contains("urn:taisce:books:authors:frank%20herbert"));
        assert!(x.contains(">2 books</content>") && x.contains(">1 book</content>"));
    }

    #[test]
    fn ereader_json_never_carries_a_hash_or_secret() {
        let e = Ereader {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            label: "kindle".into(),
            secret_hash: "sh".into(),
            kosync_key_hash: "kh".into(),
            kosync_key_hash_alt: "ka".into(),
            stream_downloads: true,
            created_at: 1_700_000_000,
            last_used_at: None,
            revoked_at: None,
        };
        let v = ereader_json(&e);
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("hash") && !s.contains("sh") && !s.contains("kh") && !s.contains("\"ka\""), "{s}");
        assert_eq!(v["label"], "kindle");
        assert_eq!(v["stream_downloads"], true);
        assert!(v["last_used_at"].is_null());
    }
}
