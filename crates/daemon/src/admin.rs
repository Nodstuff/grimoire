//! Localhost admin API: gardener registry CRUD + run-now. The `ksd` CLI is a
//! thin client over these routes so the daemon stays the only DB owner.

use crate::garden;
use crate::store_ext::with_store;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::response::IntoResponse;
use axum::{Json, Router};
use taisce_store::{BlockStore, ConfidencePolicy, GardenerKind, ReviewPolicy, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub type Store = taisce_store::SharedStore;

/// Gardener/admin route state.
#[derive(Clone)]
pub struct AdminState {
    pub store: Store,
    /// SERVER mode: `/api/profile` is the signed-in user's own profile.
    pub server_mode: bool,
}

#[derive(Deserialize)]
pub struct CreateGardener {
    pub name: String,
    /// "tagging" (default), "auditor", "scribe", "keeper" or "filer"
    pub kind: Option<String>,
    pub task_prompt: String,
    pub scope_doc: Option<Uuid>,
    /// "review" (default) or "gate"
    pub confidence_policy: Option<String>,
}

#[derive(Deserialize)]
pub struct RunReq {
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct RunsQuery {
    pub limit: Option<usize>,
}

async fn list_gardeners(State(AdminState { store, .. }): State<AdminState>) -> Json<Value> {
    with_store(&store, Scope::System, move |s| {
        match s.list_gardeners() {
            Ok(g) => Json(json!(g)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

async fn create_gardener(
    State(AdminState { store, .. }): State<AdminState>,
    Json(req): Json<CreateGardener>,
) -> Json<Value> {
    let policy = match req.confidence_policy.as_deref() {
        None => ConfidencePolicy::Review,
        Some(p) => match ConfidencePolicy::parse(p) {
            Some(p) => p,
            None => return Json(json!({"error": format!("bad confidence_policy: {p}")})),
        },
    };
    let kind = match req.kind.as_deref() {
        None => GardenerKind::Tagging,
        Some(k) => match GardenerKind::parse(k) {
            Some(k) => k,
            None => return Json(json!({"error": format!("bad kind: {k}")})),
        },
    };
    with_store(&store, Scope::System, move |s| {
        match s.create_gardener(&req.name, kind, &req.task_prompt, req.scope_doc, policy) {
            Ok(g) => Json(json!(g)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

/// Run-now. 409 when every matched gardener is already mid-run (a second
/// click, or the daily cut got there first); a mixed batch reports the
/// running ones with status "running" and runs the rest.
async fn run_now(
    State(AdminState { store, .. }): State<AdminState>,
    Json(req): Json<RunReq>,
) -> axum::response::Response {
    let gardeners = match with_store(&store, Scope::System, |s| s.list_gardeners()).await {
        Ok(g) => g,
        Err(e) => return Json(json!({"error": e.to_string()})).into_response(),
    };
    let mut outcomes = Vec::new();
    let mut started = 0usize;
    for g in gardeners {
        if !g.enabled {
            continue;
        }
        if let Some(name) = &req.name
            && &g.name != name
        {
            continue;
        }
        let name = g.name.clone();
        let out = garden::run_gardener(store.clone(), g).await;
        if out.status != garden::STATUS_RUNNING {
            started += 1;
        }
        outcomes.push(json!({
            "gardener": name,
            "run_id": out.run_id,
            "status": out.status,
            "summary": out.summary,
        }));
    }
    if outcomes.is_empty() {
        return Json(json!({"error": "no matching enabled gardener"})).into_response();
    }
    if started == 0 {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({"error": "already running", "code": "gardener_running", "outcomes": outcomes})),
        )
            .into_response();
    }
    Json(json!(outcomes)).into_response()
}

async fn list_runs(State(AdminState { store, .. }): State<AdminState>, Query(q): Query<RunsQuery>) -> Json<Value> {
    with_store(&store, Scope::System, move |s| {
        match s.list_runs(q.limit.unwrap_or(20)) {
            Ok(r) => Json(json!(r)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

#[derive(Deserialize)]
pub struct UpdateGardener {
    pub id: Uuid,
    pub task_prompt: String,
    pub schedule: String,
    pub confidence_policy: String,
    pub scope_doc: Option<Uuid>,
    pub enabled: bool,
    #[serde(default)]
    pub bindings: serde_json::Value,
}

async fn update_gardener(
    State(AdminState { store, .. }): State<AdminState>,
    Json(req): Json<UpdateGardener>,
) -> Json<Value> {
    let Some(policy) = ConfidencePolicy::parse(&req.confidence_policy) else {
        return Json(json!({"error": format!("bad confidence_policy: {}", req.confidence_policy)}));
    };
    with_store(&store, Scope::System, move |s| {
        let bindings = if req.bindings.is_null() {
            serde_json::json!([])
        } else {
            req.bindings
        };
        match s.update_gardener(
            req.id,
            &req.task_prompt,
            &req.schedule,
            policy,
            req.scope_doc,
            req.enabled,
            bindings,
        ) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

#[derive(Deserialize)]
pub struct PolicyReq {
    pub doc_id: Uuid,
    /// "human-review" | "agent-review" | "auto" | null to clear (inherit)
    pub policy: Option<String>,
}

async fn set_policy(State(AdminState { store, .. }): State<AdminState>, Json(req): Json<PolicyReq>) -> Json<Value> {
    let policy = match req.policy.as_deref() {
        None => None,
        Some(p) => match ReviewPolicy::parse(p) {
            Some(p) => Some(p),
            None => return Json(json!({"error": format!("bad policy: {p}")})),
        },
    };
    with_store(&store, Scope::System, move |s| {
        match s.set_review_policy(req.doc_id, policy) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

/// The local trust boundary for `/admin/*` (gardeners, policies — every
/// gate-weakening surface). A per-boot random token: the
/// Tauri shell reads it from `<db_dir>/admin.token` (0600) and hands it to
/// the page; the CLI reads the same file. Any other local process — a
/// browser tab, a sandboxed app, another user — is refused. A process
/// running AS the user can read the file; that is the honest boundary.
#[derive(Clone)]
pub struct AdminToken(Arc<str>);

pub const ADMIN_TOKEN_FILE: &str = "admin.token";
pub const ADMIN_HEADER: &str = "taisce-admin";

impl AdminToken {
    /// Mint a fresh token and write it beside the db (overwriting the last
    /// boot's), so a stale copy in an old tab never works.
    pub fn mint(db_dir: &std::path::Path) -> anyhow::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("OS entropy: {e}"))?;
        let token = hex::encode(bytes);
        write_secret_file(&db_dir.join(ADMIN_TOKEN_FILE), &token)?;
        Ok(Self(token.into()))
    }

    /// A fixed token (tests).
    #[cfg(test)]
    pub fn fixed(token: &str) -> Self {
        Self(token.into())
    }

    /// The token the CLI should send: read from the file the daemon wrote.
    pub fn read_from(db_dir: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(db_dir.join(ADMIN_TOKEN_FILE))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn matches(&self, presented: Option<&str>) -> bool {
        // constant-time compare: the token is a secret
        let Some(p) = presented else { return false };
        let a = self.0.as_bytes();
        let b = p.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

/// Write a secret beside the db, owner-only (0600).
fn write_secret_file(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// axum middleware: refuse `/admin/*` without the header. Same JSON error
/// shape as every other refusal, with a typed `code` the UI branches on.
async fn require_admin(
    State(token): State<AdminToken>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let presented = req
        .headers()
        .get(ADMIN_HEADER)
        .and_then(|v| v.to_str().ok());
    if token.matches(presented) {
        return next.run(req).await;
    }
    tracing::warn!(path = %req.uri().path(), "admin request without a valid token refused");
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "this action needs the app's admin token — open Taisce from the app, or add ?admin_token=<contents of ~/.grimoire/admin.token> to the URL",
            "code": "admin_token",
        })),
    )
        .into_response()
}

/// The instance owner's profile: display name and whether the name was
/// ever confirmed by the user.
/// The person a profile request is for: the signed-in user's own human
/// principal (SERVER), else the instance's one human (LOCAL).
fn profile_human(s: &taisce_store::SqliteStore, v: &crate::viewer::Viewer) -> Option<taisce_store::Principal> {
    match v.user {
        Some(_) => s.get_principal(v.human).ok(),
        None => s
            .list_principals()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.kind == taisce_store::PrincipalKind::Human),
    }
}

impl axum::extract::FromRequestParts<AdminState> for crate::viewer::Viewer {
    type Rejection = axum::response::Response;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, st: &AdminState) -> Result<Self, Self::Rejection> {
        Self::from_parts(parts, st.server_mode, Uuid::nil())
    }
}

async fn get_profile(State(AdminState { store, .. }): State<AdminState>, v: crate::viewer::Viewer) -> Json<Value> {
    with_store(&store, v.scope, move |s| {
        let human = profile_human(s, &v);
        let Some(human) = human else {
            return Json(json!({"error": "no human principal"}));
        };
        let confirmed = s.get_setting("profile.confirmed").ok().flatten().as_deref() == Some("1");
        Json(json!({
            "name": human.display_name,
            "principal_id": human.id,
            "confirmed": confirmed,
        }))
    })
    .await
}

#[derive(Deserialize)]
pub struct ProfileReq {
    pub name: String,
}

async fn set_profile(
    State(AdminState { store, .. }): State<AdminState>,
    v: crate::viewer::Viewer,
    Json(req): Json<ProfileReq>,
) -> Json<Value> {
    with_store(&store, v.scope, move |s| {
        let human = profile_human(s, &v);
        let Some(human) = human else {
            return Json(json!({"error": "no human principal"}));
        };
        match s.rename_principal(human.id, &req.name) {
            Ok(()) => {
                s.set_setting("profile.confirmed", "1").ok();
                Json(json!({"ok": true, "name": req.name.trim()}))
            }
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

pub fn router(store: Store, token: AdminToken, server_mode: bool) -> Router {
    let state = AdminState { store, server_mode };
    // the profile is not gate-weakening (your own name): it stays open so
    // the first-run prompt works from any local client
    let open_routes = Router::new()
        .route("/api/profile", get(get_profile).post(set_profile))
        .with_state(state.clone());
    Router::new()
        .route(
            "/admin/gardeners",
            get(list_gardeners).post(create_gardener),
        )
        .route("/admin/garden", post(run_now))
        .route("/admin/gardeners/update", post(update_gardener))
        .route("/admin/runs", get(list_runs))
        .route("/admin/policy", post(set_policy))
        .route("/admin/living/refresh", post(crate::living::refresh_now))
        .route_layer(axum::middleware::from_fn_with_state(token, require_admin))
        .with_state(state)
        .merge(open_routes)
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> Router {
        let store: Store = taisce_store::SharedStore::new(taisce_store::SqliteStore::open_in_memory().unwrap());
        router(store, AdminToken::fixed("s3cret"), false)
    }

    #[tokio::test]
    async fn admin_routes_need_the_token_and_profile_does_not() {
        let app = app();
        // no header → 401 with the typed code
        let res = app
            .clone()
            .oneshot(Request::get("/admin/runs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "admin_token");
        // wrong token → 401
        let res = app
            .clone()
            .oneshot(
                Request::get("/admin/gardeners")
                    .header(ADMIN_HEADER, "nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // right token → the handler runs
        let res = app
            .clone()
            .oneshot(
                Request::get("/admin/gardeners")
                    .header(ADMIN_HEADER, "s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // the profile is open: no token needed (first-run name prompt)
        let res = app
            .oneshot(Request::get("/api/profile").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn run_now_is_409_while_the_gardener_is_mid_run() {
        let app = app();
        let create = Request::post("/admin/gardeners")
            .header(ADMIN_HEADER, "s3cret")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "tags", "task_prompt": "tag"}).to_string()))
            .unwrap();
        let res = app.clone().oneshot(create).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let g: Value = serde_json::from_slice(&body).unwrap();
        let id: Uuid = g["id"].as_str().unwrap().parse().unwrap();
        // another run holds the claim
        let claim = garden::claim_run(id).unwrap();
        let res = app
            .clone()
            .oneshot(
                Request::post("/admin/garden")
                    .header(ADMIN_HEADER, "s3cret")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "tags"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "gardener_running");
        // no run row was written for the refused attempt
        let res = app
            .clone()
            .oneshot(Request::get("/admin/runs").header(ADMIN_HEADER, "s3cret").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap().as_array().unwrap().len(), 0);
        drop(claim);
    }

    #[test]
    fn token_compare_is_exact() {
        let t = AdminToken::fixed("abc");
        assert!(t.matches(Some("abc")));
        assert!(!t.matches(Some("abd")));
        assert!(!t.matches(Some("ab")));
        assert!(!t.matches(Some("abcd")));
        assert!(!t.matches(None));
    }

    #[test]
    fn mint_writes_a_0600_file_and_read_from_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let t = AdminToken::mint(dir.path()).unwrap();
        let on_disk = AdminToken::read_from(dir.path()).unwrap();
        assert!(t.matches(Some(&on_disk)));
        assert_eq!(on_disk.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(ADMIN_TOKEN_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // a second boot replaces it
        let t2 = AdminToken::mint(dir.path()).unwrap();
        assert!(!t2.matches(Some(&on_disk)));
    }
}

/// The 16:00 UTC daily cut (§3.4): the daemon self-schedules; no external
/// cron. UTC, like every server clock: the process never reads a timezone.
pub async fn daily_loop(store: Store) {
    loop {
        let now = chrono::Utc::now();
        let today_four = now.date_naive().and_hms_opt(16, 0, 0).unwrap();
        let next = if now.naive_utc() < today_four {
            today_four
        } else {
            (now.date_naive() + chrono::Days::new(1))
                .and_hms_opt(16, 0, 0)
                .unwrap()
        };
        let wait = (next - now.naive_utc()).to_std().unwrap_or_default();
        tracing::info!("next gardener run in {}s", wait.as_secs());
        tokio::time::sleep(wait).await;

        let gardeners = {
            with_store(&store, Scope::System, move |s| {
                s.list_gardeners().unwrap_or_default()
            })
            .await
        };
        // manual-cadence tendings only run via run-now; the daily cut skips them
        for g in gardeners
            .into_iter()
            .filter(|g| g.enabled && g.schedule != "manual")
        {
            if garden::is_running(g.id) {
                tracing::info!("gardener {}: skipped, a run-now is still going", g.name);
                continue;
            }
            let name = g.name.clone();
            let out = garden::run_gardener(store.clone(), g).await;
            tracing::info!("gardener {name}: {} — {}", out.status, out.summary);
        }
        // living answers: re-ground answers whose cited blocks moved on
        let lines = crate::living::refresh_sweep(store.clone(), None).await;
        tracing::info!("living answers sweep: {}", lines.join(" | "));
    }
}
