//! KOSYNC reading-progress sync (`/kosync/…`) and the person's progress
//! surfaces (`/api/books/progress`, `/api/books/currently-reading`).
//!
//! KOSYNC is the tiny sync protocol KOReader and CrossPoint speak to a
//! custom progress server: four routes, header credentials, and clients
//! that punish unexpected shapes (a 404 on a pull reads as a sync ERROR in
//! KOReader, not as "no progress yet"). The response shapes below are
//! exactly what those clients parse, verified against KOReader's
//! `plugins/kosync.koplugin` (api.json + main.lua: its pull callback checks
//! `body.percentage` presence) and CrossPoint's
//! `lib/KOReaderSync/KOReaderSyncClient.cpp` (any 2xx is success; the
//! reference server's "200 with an empty object" for unknown documents is
//! called out in its comments).
//!
//! - `POST /kosync/users/create {username, password}` — the official server
//!   CREATES accounts here; ours exist only when a person mints an ereader
//!   at `/api/ereaders` (the feed agent's surface). So this VALIDATES the
//!   pair against `ereader_find_kosync` like a login: 201 `{"username"}`
//!   when it resolves (CrossPoint's "Sign Up" treats any 2xx as success,
//!   so an existing device password signs up cleanly; a KOReader user
//!   simply uses Login), else the protocol's 401. No account is ever
//!   created, and a bad pair is indistinguishable from an unknown one.
//! - `GET /kosync/users/auth` — 200 `{"authorized": "OK"}` when the
//!   `x-auth-user` / `x-auth-key` headers resolve, else 401 (code 2001).
//! - `PUT /kosync/syncs/progress {document, progress, percentage, device,
//!   device_id}` — newest-wins upsert; 200 `{"document", "timestamp"}` with
//!   the row's FINAL stamp (an older push is acknowledged but did not move
//!   the row).
//! - `GET /kosync/syncs/progress/{document}` — 200 with the row
//!   (`document, progress, percentage, device, device_id, timestamp`), or
//!   200 with an EMPTY object `{}` when nothing was pushed: the official
//!   server's answer, and the only one KOReader treats as "no progress
//!   found" (any non-200 is a sync error there; kosync-dotnet's 404 is
//!   handled by CrossPoint but reads as fatal in KOReader).
//!
//! Device auth: `x-auth-key` is the client-computed MD5 of the device
//! password exactly as typed; the mint stored SHA-256 of it for both ways a
//! person types it (`kosync_key_hash` dashed as shown, `_alt` undashed).
//! We SHA-256 the presented key (`auth::hash_secret`) and resolve
//! pre-auth in `Scope::System` — the hash IS the credential, same pattern
//! as `auth::authenticate`. The row's `user_id` scopes every progress
//! query (`Scope::User`); nothing else is touched (`last_used_at` is the
//! feed agent's lifecycle, not ours). Requests may carry
//! `Accept: application/vnd.koreader.v1+json` (and CrossPoint adds a Basic
//! `Authorization` header for calibre-web compat): accepted and ignored.
//!
//! The person's surfaces join the shelf to `kosync_progress` through
//! `booknaming::kosync_document_digest(title, author)` — the same
//! MD5-of-derived-filename the devices compute, so a feed-downloaded
//! book's progress lands on its shelf entry (a metadata edit re-derives
//! the digest; progress pushed under the old name no longer joins —
//! filename matching has the same property on device). Unlike the device
//! protocol, these payloads present `percentage` SCALED to 0–100 (the
//! store holds the device-sent 0..1 fraction; the UI's `BookProgress`
//! contract is percents) and `updated_at` RFC 3339.
//!
//! - `GET /api/books/progress?ids=<uuid>,<uuid>,…` →
//!   `{progress: {<id>: {percentage, device, updated_at, status} | null}}`;
//!   one bad or foreign id is a null entry, never a failed batch.
//! - `GET /api/books/currently-reading` → `{books: [...]}`: books with
//!   `status == "reading"`, newest progress stamp first (limit
//!   [`CURRENTLY_READING_LIMIT`]).
//!
//! Every route answers 404 in LOCAL mode (the `ServerSide` Option, like
//! shares.rs): `book`/`ereader` rows key to `auth_users`, which exist only
//! for signed-in users.

use crate::auth::{hash_secret, now};
use crate::store_ext::with_store;
use crate::viewer::Viewer;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use taisce_store::books::{Book, BookPage, Ereader, KosyncProgress};
use taisce_store::{Scope, SharedStore, StoreError};
use uuid::Uuid;

// ---- limits (none are the protocol's; a leaked device key should not be a
// db-bloat vector, and real values are far smaller: a document hash is 32
// hex chars, a progress XPath a few hundred bytes) ----

/// `document`, `device`, `device_id`, and the create body's two fields.
const MAX_LABEL: usize = 128;
/// The progress marker (an XPath-ish fragment).
const MAX_PROGRESS: usize = 4096;
/// The most book ids one `/api/books/progress` call may name.
const MAX_PROGRESS_IDS: usize = 200;
/// `/api/books/currently-reading`: one shelf page, and the most books the
/// scan walks before giving up (a bigger shelf pays a capped scan — see
/// the handler for why there is no cheaper way yet).
const PAGE: usize = 100;
const CURRENTLY_READING_SCAN: usize = 500;
const CURRENTLY_READING_LIMIT: usize = 20;

/// SERVER mode's half of the state. The device routes need `auth_users`
/// rows to name in `x-auth-user` and the /api routes need a signed-in user
/// to key a shelf to; LOCAL mode has neither, so every route answers 404.
/// `cfg` finds the client address behind the proxy, `limiter` is the
/// server's one limiter: a failed device sign-in spends the same strict
/// per-IP bucket as a failed OPDS one (`Class::OpdsAuth`).
#[derive(Clone)]
pub struct ServerSide {
    pub cfg: std::sync::Arc<crate::auth::AuthConfig>,
    pub limiter: crate::auth::ratelimit::Limiter,
}

#[derive(Clone)]
pub struct KosyncState {
    pub store: SharedStore,
    /// None = LOCAL mode: every route answers 404.
    pub server: Option<ServerSide>,
    /// LOCAL mode's human (for the Viewer of a LOCAL request).
    pub local_human: Uuid,
}

impl FromRequestParts<KosyncState> for Viewer {
    type Rejection = Response;
    async fn from_request_parts(parts: &mut Parts, st: &KosyncState) -> Result<Self, Response> {
        Self::from_parts(parts, st.server.is_some(), st.local_human)
    }
}

// ---- errors: the device routes carry the protocol's own body (the
// official server's `raise_error` writes `{"code", "message"}`; clients
// key on the status, the code is for logs and curl). The /api routes use
// the daemon's `{"error"}` house shape via `fail`. ----

fn kosync_err(status: StatusCode, code: u32, msg: &str) -> Response {
    (status, Json(json!({"code": code, "message": msg}))).into_response()
}

/// 401, code 2001: the pair did not resolve to a live ereader. Missing vs
/// wrong vs revoked is never revealed.
fn unauthorized() -> Response {
    kosync_err(StatusCode::UNAUTHORIZED, 2001, "Unauthorized")
}

/// 400, code 2000: a malformed or out-of-bounds request body.
fn bad_request(msg: impl std::fmt::Display) -> Response {
    kosync_err(StatusCode::BAD_REQUEST, 2000, &msg.to_string())
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
}

fn fail(e: StoreError) -> Response {
    let code = match &e {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::Forbidden(_) => StatusCode::FORBIDDEN,
        StoreError::InvalidOp(_) | StoreError::StaleBase { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, Json(json!({"error": e.to_string()}))).into_response()
}

pub(crate) fn rfc3339(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// The shelf's reading status from a stored kosync percentage (the
/// device-sent fraction, 0..1 for KOReader and CrossPoint): 0 is "unread",
/// 98% and up is "finished", anything between is "reading". The bands are
/// compared on the stored fraction, not on `percentage * 100`: 0.98 * 100
/// is 97.999… in f64 and would misclassify the edge.
pub(crate) fn status(percentage: f64) -> &'static str {
    if percentage >= 0.98 {
        "finished"
    } else if percentage > 0.0 {
        "reading"
    } else {
        "unread"
    }
}

// ---- the device protocol ----

/// The live ereader behind the `x-auth-user` / `x-auth-key` pair, resolved
/// pre-auth in `Scope::System` (the hash is the credential — same pattern
/// as `auth::authenticate`). Its `user_id` is who the progress queries run
/// for.
async fn device(st: &KosyncState, headers: &HeaderMap, ext: &Extensions) -> Result<Ereader, Response> {
    let Some(server) = &st.server else {
        return Err(not_found());
    };
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let (Some(username), Some(key)) = (header("x-auth-user"), header("x-auth-key")) else {
        return Err(auth_failed(server, headers, ext));
    };
    let key_hash = hash_secret(&key);
    match with_store(&st.store, Scope::System, move |s| {
        s.ereader_find_kosync(&username, &key_hash)
    })
    .await
    {
        Ok(Some(e)) => Ok(e),
        Ok(None) => Err(auth_failed(server, headers, ext)),
        Err(e) => Err(fail(e)),
    }
}

/// A failed device sign-in: the protocol's 401, or 429 once this address
/// has spent the strict per-IP failure bucket (shared with OPDS).
fn auth_failed(server: &ServerSide, headers: &HeaderMap, ext: &Extensions) -> Response {
    let ip = crate::shares::ip_key(&crate::auth::client_ip(&server.cfg, headers, ext));
    tracing::warn!(target: crate::auth::AUDIT, event = "kosync.auth_failed", ip = %ip);
    if !server.limiter.allow(crate::auth::ratelimit::Class::OpdsAuth, &ip) {
        return kosync_err(StatusCode::TOO_MANY_REQUESTS, 2001, "too many failed sign-ins; try again shortly");
    }
    unauthorized()
}

#[derive(Deserialize)]
struct CreateUserReq {
    username: String,
    password: String,
}

/// `POST /kosync/users/create` — validates the pair like a login (accounts
/// are minted only at `/api/ereaders`; see the module docs).
async fn create_user(
    State(st): State<KosyncState>,
    headers: HeaderMap,
    ext: Extensions,
    body: Result<Json<CreateUserReq>, JsonRejection>,
) -> Response {
    let Some(server) = &st.server else {
        return not_found();
    };
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return bad_request(e.body_text()),
    };
    let username = req.username.trim().to_string();
    if username.is_empty()
        || username.len() > MAX_LABEL
        || req.password.is_empty()
        || req.password.len() > MAX_LABEL
    {
        return bad_request("username and password are required");
    }
    let key_hash = hash_secret(&req.password);
    let name = username.clone();
    let found = with_store(&st.store, Scope::System, move |s| {
        s.ereader_find_kosync(&name, &key_hash)
    })
    .await;
    match found {
        // the account exists (it was minted with the ereader): the device
        // hears the protocol's success — 201, the only status KOReader's
        // register accepts (KOSyncClient.lua: `res.status == 201`) — and
        // never learns it created nothing
        Ok(Some(_)) => (StatusCode::CREATED, Json(json!({"username": username}))).into_response(),
        Ok(None) => auth_failed(server, &headers, &ext),
        Err(e) => fail(e),
    }
}

/// `GET /kosync/users/auth` — the credential check every client runs first.
async fn authorize(State(st): State<KosyncState>, headers: HeaderMap, ext: Extensions) -> Response {
    match device(&st, &headers, &ext).await {
        Ok(_) => Json(json!({"authorized": "OK"})).into_response(),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ProgressIn {
    document: String,
    progress: String,
    percentage: f64,
    device: String,
    device_id: String,
    // KOReader sends an optional `metadata` object (and a crosspoint-sync
    // server gets a `position` object): serde ignores unknown fields, so
    // neither is named here.
}

/// `PUT /kosync/syncs/progress` — record a push, newest-wins.
async fn put_progress(
    State(st): State<KosyncState>,
    headers: HeaderMap,
    ext: Extensions,
    body: Result<Json<ProgressIn>, JsonRejection>,
) -> Response {
    let dev = match device(&st, &headers, &ext).await {
        Ok(d) => d,
        Err(r) => return r,
    };
    let req = match body {
        Ok(Json(r)) => r,
        Err(e) => return bad_request(e.body_text()),
    };
    if req.document.is_empty() || req.document.len() > MAX_LABEL {
        return bad_request(format!("document: 1-{MAX_LABEL} chars"));
    }
    if req.progress.len() > MAX_PROGRESS {
        return bad_request(format!("progress: at most {MAX_PROGRESS} bytes"));
    }
    if !req.percentage.is_finite() {
        return bad_request("percentage: a finite number");
    }
    if req.device.len() > MAX_LABEL || req.device_id.len() > MAX_LABEL {
        return bad_request(format!("device, device_id: at most {MAX_LABEL} bytes"));
    }
    let user = dev.user_id;
    let document = req.document;
    let answer_doc = document.clone();
    // the server stamps: the store's newest-wins compares these
    let ts = now();
    let put = with_store(&st.store, Scope::User(user), move |s| {
        s.kosync_upsert(
            user,
            &document,
            &req.progress,
            req.percentage,
            &req.device,
            &req.device_id,
            ts,
        )
    })
    .await;
    match put {
        // the answer's timestamp is the row's FINAL stamp: an older push is
        // acknowledged but did not move the row
        Ok(stored) => Json(json!({"document": answer_doc, "timestamp": stored})).into_response(),
        Err(e) => fail(e),
    }
}

/// The pull body: exactly the fields KOReader and CrossPoint read, with the
/// percentage passed through as the device sent it (the protocol's unit is
/// the 0..1 fraction — only the /api surfaces rescale).
fn progress_doc(p: &KosyncProgress) -> Value {
    json!({
        "document": p.document,
        "progress": p.progress,
        "percentage": p.percentage,
        "device": p.device,
        "device_id": p.device_id,
        "timestamp": p.updated_at,
    })
}

/// `GET /kosync/syncs/progress/{document}` — the pull.
async fn get_progress(
    State(st): State<KosyncState>,
    headers: HeaderMap,
    ext: Extensions,
    Path(document): Path<String>,
) -> Response {
    let dev = match device(&st, &headers, &ext).await {
        Ok(d) => d,
        Err(r) => return r,
    };
    let user = dev.user_id;
    let got = with_store(&st.store, Scope::User(user), move |s| {
        s.kosync_get(user, &document)
    })
    .await;
    match got {
        Ok(Some(p)) => Json(progress_doc(&p)).into_response(),
        // never pushed: 200 with an EMPTY object, not 404 — the official
        // server's answer, and the only one KOReader's pull treats as "no
        // progress found" (its callback checks body.percentage; any non-200
        // is a sync error there)
        Ok(None) => Json(json!({})).into_response(),
        Err(e) => fail(e),
    }
}

// ---- the person's surfaces (/api, Viewer scope) ----

/// The user an /api route answers for: SERVER mode (LOCAL has no shelves —
/// `book` rows key to `auth_users`) and the Viewer carries one. Anything
/// else is 404: the feature does not exist for this request.
fn api_user(st: &KosyncState, v: &Viewer) -> Option<Uuid> {
    if st.server.is_none() {
        return None;
    }
    v.user
}

/// One entry of the /api progress payloads: the fields the shelf UI
/// renders, with `percentage` SCALED to 0–100 (the store holds the
/// device-sent 0..1 fraction; the UI's `BookProgress` contract is
/// percents) and `updated_at` RFC 3339 — both unlike the device protocol,
/// which passes the fraction through and stamps unix seconds.
pub(crate) fn progress_json(p: &KosyncProgress) -> Value {
    json!({
        "percentage": p.percentage * 100.0,
        "device": p.device,
        "updated_at": rfc3339(p.updated_at),
        "status": status(p.percentage),
    })
}

#[derive(Deserialize)]
struct ProgressQuery {
    /// `ids=<uuid>,<uuid>,…` (the shelf UI sends its visible page).
    #[serde(default)]
    ids: String,
}

/// `GET /api/books/progress?ids=…` — a separate endpoint so the feed
/// agent's `/api/books` stays untouched.
async fn books_progress(
    State(st): State<KosyncState>,
    v: Viewer,
    Query(q): Query<ProgressQuery>,
) -> Response {
    let Some(user) = api_user(&st, &v) else {
        return not_found();
    };
    let wanted: Vec<(String, Option<Uuid>)> = q
        .ids
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| (t.to_string(), Uuid::parse_str(t).ok()))
        .collect();
    if wanted.len() > MAX_PROGRESS_IDS {
        return fail(StoreError::InvalidOp(format!(
            "at most {MAX_PROGRESS_IDS} ids"
        )));
    }
    let res = with_store(
        &st.store,
        v.scope,
        move |s| -> Result<Response, StoreError> {
            let mut map = serde_json::Map::new();
            // one lock for the whole batch; N is a shelf page, not a library
            for (token, id) in &wanted {
                // an id that is not a uuid (or names a book the caller cannot
                // see) is a null entry, exactly like "no progress yet": the
                // batch never 404s on one id
                let Some(id) = id else {
                    map.insert(token.clone(), Value::Null);
                    continue;
                };
                let book = match s.book_get(*id) {
                    Ok(b) => b,
                    Err(StoreError::NotFound(_)) => {
                        map.insert(token.clone(), Value::Null);
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let digest = crate::booknaming::kosync_document_digest(&book.title, &book.author);
                let entry = s
                    .kosync_get(user, &digest)?
                    .map(|p| progress_json(&p))
                    .unwrap_or(Value::Null);
                map.insert(token.clone(), entry);
            }
            Ok(Json(json!({"progress": map})).into_response())
        },
    )
    .await;
    match res {
        Ok(r) => r,
        Err(e) => fail(e),
    }
}

/// A currently-reading row: the book's fields (the same shape the feed
/// contract serves, so rail and shelf render alike) with the progress
/// flattened in. The row's `updated_at` is the PROGRESS stamp ("last
/// read"), shadowing the book's own metadata stamp — the UI's ReadingBook
/// flattens the two onto one field and the rail renders "last read".
/// `book_identifier` is "" when absent (the UI contract declares it
/// non-null).
fn reading_json(b: &Book, p: &KosyncProgress) -> Value {
    json!({
        "id": b.id,
        "title": b.title,
        "author": b.author,
        "series": b.series,
        "series_index": b.series_index,
        "book_identifier": b.book_identifier.clone().unwrap_or_default(),
        "original_filename": b.original_filename,
        "size": b.size,
        "has_cover": b.shown_cover_key().is_some(),
        "cover_version": b.cover_choice_at.unwrap_or(0),
        "added_at": rfc3339(b.added_at),
        "updated_at": rfc3339(p.updated_at),
        "percentage": p.percentage * 100.0,
        "device": p.device,
        "status": status(p.percentage),
    })
}

/// `GET /api/books/currently-reading` — books with `status == "reading"`,
/// newest progress stamp first.
async fn currently_reading(State(st): State<KosyncState>, v: Viewer) -> Response {
    let Some(user) = api_user(&st, &v) else {
        return not_found();
    };
    let res = with_store(
        &st.store,
        v.scope,
        move |s| -> Result<Response, StoreError> {
            // The store has no kosync_list_recent(user, limit) (and this file
            // may not add one): walk the shelf newest-first in pages, join each
            // book's digest to its progress row, keep the "reading" ones, then
            // sort by the progress stamp. A big shelf pays a full scan, capped
            // at CURRENTLY_READING_SCAN books — the honest price of doing this
            // without a store method (flagged in the report).
            let mut reading: Vec<(Book, KosyncProgress)> = Vec::new();
            let mut page = BookPage::default();
            let mut scanned = 0usize;
            loop {
                let books = s.book_list(user, &page, PAGE, None)?;
                let Some(last) = books.last() else { break };
                page = BookPage {
                    before: Some((last.added_at, last.id)),
                    after: None,
                };
                scanned += books.len();
                let done = books.len() < PAGE;
                for b in books {
                    let digest = crate::booknaming::kosync_document_digest(&b.title, &b.author);
                    if let Some(p) = s.kosync_get(user, &digest)?
                        && status(p.percentage) == "reading"
                    {
                        reading.push((b, p));
                    }
                }
                if done || scanned >= CURRENTLY_READING_SCAN {
                    break;
                }
            }
            reading.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
            reading.truncate(CURRENTLY_READING_LIMIT);
            let rows: Vec<Value> = reading.iter().map(|(b, p)| reading_json(b, p)).collect();
            Ok(Json(json!({"books": rows})).into_response())
        },
    )
    .await;
    match res {
        Ok(r) => r,
        Err(e) => fail(e),
    }
}

pub fn router(state: KosyncState) -> Router {
    Router::new()
        // the device protocol (its own x-auth-* auth; outside OAuth —
        // auth::is_public names /kosync so require_auth lets it through)
        .route("/kosync/users/create", post(create_user))
        .route("/kosync/users/auth", get(authorize))
        .route("/kosync/syncs/progress", put(put_progress))
        .route("/kosync/syncs/progress/{document}", get(get_progress))
        // the person's progress surfaces (Viewer scope)
        .route("/api/books/progress", get(books_progress))
        .route("/api/books/currently-reading", get(currently_reading))
        // kosync bodies are a few hundred bytes at most
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Authenticated;
    use axum::body::Body;
    use axum::http::Request;
    use taisce_store::books::NewBook;
    use taisce_store::{BlockStore, PrincipalKind, SqliteStore};
    use tower::ServiceExt;

    /// What the device sends as `x-auth-key` (a password's MD5, per the
    /// protocol); the mint stored SHA-256 of it.
    const DEVICE_KEY: &str = "d8578edf8458ce06fbc5bb76a58c5ca4";
    const AUTH: &[(&str, &str)] = &[("x-auth-user", "tom"), ("x-auth-key", DEVICE_KEY)];

    fn app() -> (Router, SharedStore, Uuid, Uuid) {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let human = s
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap()
            .id;
        let user = s.auth_ensure_owner(human, "Tom", 1).unwrap().id;
        let store = SharedStore::new(s);
        store
            .lock(Scope::User(user))
            .ereader_insert(
                Uuid::now_v7(),
                "kindle",
                "sh1",
                (&hash_secret(DEVICE_KEY), ""),
                true,
                10,
            )
            .unwrap();
        let st = KosyncState {
            store: store.clone(),
            server: Some(test_server()),
            local_human: human,
        };
        (router(st), store, user, human)
    }

    fn test_server() -> ServerSide {
        ServerSide {
            cfg: std::sync::Arc::new(crate::auth::AuthConfig::from_public_url("http://localhost:7513").unwrap()),
            limiter: Default::default(),
        }
    }

    fn get_req(url: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut req = Request::get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.body(Body::empty()).unwrap()
    }

    fn send_json(method: &str, url: &str, headers: &[(&str, &str)], body: Value) -> Request<Body> {
        let mut req = Request::builder()
            .method(method)
            .uri(url)
            .header("content-type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    /// A SERVER-mode /api request: what it looks like after require_auth.
    fn viewer_req(url: &str, user: Uuid, human: Uuid) -> Request<Body> {
        Request::get(url)
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
            .body(Body::empty())
            .unwrap()
    }

    async fn body_json(res: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    fn add_book(store: &SharedStore, user: Uuid, title: &str, author: &str, added_at: i64) -> Uuid {
        let book = NewBook {
            title: title.into(),
            author: author.into(),
            series: None,
            series_index: None,
            book_identifier: None,
            original_filename: format!("{title}.epub"),
            file_sha256: format!("sha256:{title}"),
            size: 4096,
            file_s3_key: format!("u/{title}.epub"),
            cover_s3_key: None,
        };
        store
            .lock(Scope::User(user))
            .book_insert(Uuid::now_v7(), &book, added_at)
            .unwrap()
            .id
    }

    /// Progress as a device would push it: keyed to the derived-filename
    /// digest of the book, like a feed download on device.
    fn push_progress(
        store: &SharedStore,
        user: Uuid,
        title: &str,
        author: &str,
        percentage: f64,
        ts: i64,
    ) {
        let digest = crate::booknaming::kosync_document_digest(title, author);
        store
            .lock(Scope::User(user))
            .kosync_upsert(
                user,
                &digest,
                "/body/DocFragment[11]",
                percentage,
                "kobo",
                "k1",
                ts,
            )
            .unwrap();
    }

    #[test]
    fn status_bands() {
        assert_eq!(status(0.0), "unread");
        assert_eq!(status(-0.5), "unread");
        assert_eq!(status(0.001), "reading");
        assert_eq!(status(0.5), "reading");
        assert_eq!(status(0.97), "reading");
        assert_eq!(
            status(0.98),
            "finished",
            "the 98% edge, compared on the stored fraction (0.98*100 != 98 in f64)"
        );
        assert_eq!(status(1.0), "finished");
    }

    #[tokio::test]
    async fn users_create_validates_the_minted_device_key() {
        let (app, ..) = app();
        // the pair resolves: 200 {"username"} — the account exists (it was
        // minted with the ereader); nothing is created here
        let res = app
            .clone()
            .oneshot(send_json(
                "POST",
                "/kosync/users/create",
                &[],
                json!({"username": "Tom", "password": DEVICE_KEY}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED, "KOReader's register accepts only 201");
        assert_eq!(body_json(res).await, json!({"username": "Tom"}));
        // a bad pair: the protocol's 401 (code 2001), no hint which half failed
        let res = app
            .oneshot(send_json(
                "POST",
                "/kosync/users/create",
                &[],
                json!({"username": "Tom", "password": "wrong"}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(res).await["code"], json!(2001));
    }

    #[tokio::test]
    async fn users_auth_answers_ok_only_for_a_live_device() {
        let (app, ..) = app();
        let res = app
            .clone()
            .oneshot(get_req("/kosync/users/auth", AUTH))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({"authorized": "OK"}));
        let res = app
            .clone()
            .oneshot(get_req("/kosync/users/auth", &[]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app
            .oneshot(get_req(
                "/kosync/users/auth",
                &[("x-auth-user", "tom"), ("x-auth-key", "nope")],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_revoked_ereader_no_longer_resolves() {
        let (app, store, user, _human) = app();
        let id = store.lock(Scope::User(user)).ereader_list(user).unwrap()[0].id;
        store
            .lock(Scope::User(user))
            .ereader_revoke(id, 100)
            .unwrap();
        let res = app
            .oneshot(get_req("/kosync/users/auth", AUTH))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn progress_round_trips_in_the_protocols_shapes() {
        let (app, ..) = app();
        let doc = "0123456789abcdef0123456789abcdef";
        let res = app
            .clone()
            .oneshot(send_json(
                "PUT",
                "/kosync/syncs/progress",
                AUTH,
                json!({"document": doc, "progress": "/body/DocFragment[11]", "percentage": 0.42, "device": "kindle", "device_id": "k1"}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["document"], json!(doc));
        assert!(v["timestamp"].as_i64().unwrap() > 0, "{v}");
        assert_eq!(
            v.as_object().unwrap().len(),
            2,
            "exactly document + timestamp: {v}"
        );
        // the pull: every field KOReader and CrossPoint read, percentage as sent
        let res = app
            .clone()
            .oneshot(get_req(&format!("/kosync/syncs/progress/{doc}"), AUTH))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(v["percentage"], json!(0.42));
        assert_eq!(v["progress"], json!("/body/DocFragment[11]"));
        assert_eq!(v["device"], json!("kindle"));
        assert_eq!(v["device_id"], json!("k1"));
        assert!(v["timestamp"].is_i64(), "{v}");
        // never pushed: 200 with an EMPTY object (not 404 — see the module docs)
        let res = app
            .clone()
            .oneshot(get_req(
                "/kosync/syncs/progress/ffffffffffffffffffffffffffffffff",
                AUTH,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({}));
        // a bad pair is a 401 even on a document that exists
        let res = app
            .oneshot(get_req(
                &format!("/kosync/syncs/progress/{doc}"),
                &[("x-auth-user", "tom"), ("x-auth-key", "nope")],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn put_progress_refuses_junk_fields() {
        let (app, ..) = app();
        let put = |document: &str| {
            send_json(
                "PUT",
                "/kosync/syncs/progress",
                AUTH,
                json!({"document": document, "progress": "p", "percentage": 0.5, "device": "d", "device_id": "d1"}),
            )
        };
        let res = app.clone().oneshot(put("")).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(res).await["code"], json!(2000));
        let res = app
            .clone()
            .oneshot(put(&"x".repeat(MAX_LABEL + 1)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        // a push without headers is a 401, not a 400: auth comes first
        let res = app
            .oneshot(send_json(
                "PUT",
                "/kosync/syncs/progress",
                &[],
                json!({"document": "d", "progress": "p", "percentage": 0.5, "device": "d", "device_id": "d1"}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn books_progress_joins_the_shelf_to_kosync_rows() {
        let (app, store, user, human) = app();
        let dune = add_book(&store, user, "Dune", "Frank Herbert", 10);
        let unread = add_book(&store, user, "Unread", "A", 20);
        push_progress(&store, user, "Dune", "Frank Herbert", 0.5, 30);
        let missing = Uuid::now_v7();
        let url = format!("/api/books/progress?ids={dune},{unread},{missing},not-a-uuid");
        let res = app.oneshot(viewer_req(&url, user, human)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let dune_id = dune.to_string();
        let p = &v["progress"][dune_id.as_str()];
        assert_eq!(
            p["percentage"],
            json!(50.0),
            "scaled to 0-100 for the UI: {p}"
        );
        assert_eq!(p["device"], json!("kobo"));
        assert_eq!(p["status"], json!("reading"));
        assert!(
            p["updated_at"].as_str().unwrap().contains('T'),
            "RFC 3339: {p}"
        );
        // no row, no such book, not a uuid: three ways to be a null entry
        assert_eq!(v["progress"][unread.to_string().as_str()], Value::Null);
        assert_eq!(v["progress"][missing.to_string().as_str()], Value::Null);
        assert_eq!(v["progress"]["not-a-uuid"], Value::Null);
    }

    #[tokio::test]
    async fn currently_reading_lists_reading_books_newest_progress_first() {
        let (app, store, user, human) = app();
        let older = add_book(&store, user, "Older", "A", 10);
        let newer = add_book(&store, user, "Newer", "B", 20);
        let _done = add_book(&store, user, "Done", "C", 30);
        let _shelf_only = add_book(&store, user, "Untouched", "D", 40);
        push_progress(&store, user, "Older", "A", 0.5, 100);
        push_progress(&store, user, "Newer", "B", 0.25, 200);
        push_progress(&store, user, "Done", "C", 1.0, 300);
        let res = app
            .oneshot(viewer_req("/api/books/currently-reading", user, human))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let books = v["books"].as_array().unwrap();
        let ids: Vec<&str> = books.iter().map(|b| b["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            vec![newer.to_string().as_str(), older.to_string().as_str()],
            "finished and unread are out: {v}"
        );
        let row = &books[0];
        assert_eq!(row["title"], json!("Newer"));
        assert_eq!(row["percentage"], json!(25.0), "scaled to 0-100: {row}");
        assert_eq!(row["device"], json!("kobo"));
        assert_eq!(row["status"], json!("reading"));
        assert_eq!(row["has_cover"], json!(false));
        assert_eq!(
            row["book_identifier"],
            json!(""),
            "the UI contract declares it non-null"
        );
        assert!(
            row["updated_at"].as_str().unwrap().contains('T'),
            "the progress stamp, RFC 3339: {row}"
        );
    }

    #[tokio::test]
    async fn local_mode_answers_404_everywhere() {
        let mut s = SqliteStore::open_in_memory().unwrap();
        let human = s
            .create_principal(PrincipalKind::Human, "tom", None)
            .unwrap()
            .id;
        let app = router(KosyncState {
            store: SharedStore::new(s),
            server: None,
            local_human: human,
        });
        for (method, url) in [
            ("POST", "/kosync/users/create"),
            ("GET", "/kosync/users/auth"),
            ("PUT", "/kosync/syncs/progress"),
            ("GET", "/kosync/syncs/progress/abc"),
            ("GET", "/api/books/progress?ids=x"),
            ("GET", "/api/books/currently-reading"),
        ] {
            let req = match method {
                "GET" => get_req(url, &[]),
                _ => send_json(method, url, &[], json!({})),
            };
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method} {url}");
        }
    }
}
