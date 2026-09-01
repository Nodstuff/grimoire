//! Localhost admin API: gardener registry CRUD + run-now. The `ksd` CLI is a
//! thin client over these routes so the daemon stays the only DB owner.

use crate::garden;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use grimoire_store::{BlockStore, ConfidencePolicy, GardenerKind, ReviewPolicy, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub type Store = Arc<Mutex<SqliteStore>>;

#[derive(Deserialize)]
pub struct CreateGardener {
    pub name: String,
    /// "tagging" (default) or "reviewer"
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

async fn list_gardeners(State(store): State<Store>) -> Json<Value> {
    let s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_gardeners() {
        Ok(g) => Json(json!(g)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn create_gardener(
    State(store): State<Store>,
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
    let mut s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.create_gardener(&req.name, kind, &req.task_prompt, req.scope_doc, policy) {
        Ok(g) => Json(json!(g)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn run_now(State(store): State<Store>, Json(req): Json<RunReq>) -> Json<Value> {
    let gardeners = {
        let s = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match s.list_gardeners() {
            Ok(g) => g,
            Err(e) => return Json(json!({"error": e.to_string()})),
        }
    };
    let mut outcomes = Vec::new();
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
        outcomes.push(json!({
            "gardener": name,
            "run_id": out.run_id,
            "status": out.status,
            "summary": out.summary,
        }));
    }
    if outcomes.is_empty() {
        return Json(json!({"error": "no matching enabled gardener"}));
    }
    Json(json!(outcomes))
}

async fn list_runs(State(store): State<Store>, Query(q): Query<RunsQuery>) -> Json<Value> {
    let s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_runs(q.limit.unwrap_or(20)) {
        Ok(r) => Json(json!(r)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
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
    State(store): State<Store>,
    Json(req): Json<UpdateGardener>,
) -> Json<Value> {
    let Some(policy) = ConfidencePolicy::parse(&req.confidence_policy) else {
        return Json(json!({"error": format!("bad confidence_policy: {}", req.confidence_policy)}));
    };
    let mut s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
}

#[derive(Deserialize)]
pub struct PolicyReq {
    pub doc_id: Uuid,
    /// "human-review" | "agent-review" | "auto" | null to clear (inherit)
    pub policy: Option<String>,
}

async fn set_policy(State(store): State<Store>, Json(req): Json<PolicyReq>) -> Json<Value> {
    let policy = match req.policy.as_deref() {
        None => None,
        Some(p) => match ReviewPolicy::parse(p) {
            Some(p) => Some(p),
            None => return Json(json!({"error": format!("bad policy: {p}")})),
        },
    };
    let mut s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.set_review_policy(req.doc_id, policy) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

pub fn router(store: Store) -> Router {
    Router::new()
        .route(
            "/admin/gardeners",
            get(list_gardeners).post(create_gardener),
        )
        .route("/admin/garden", post(run_now))
        .route("/admin/gardeners/update", post(update_gardener))
        .route("/admin/runs", get(list_runs))
        .route("/admin/policy", post(set_policy))
        .with_state(store)
}

/// The 16:00 daily cut (§3.4): the daemon self-schedules; no external cron.
pub async fn daily_loop(store: Store) {
    loop {
        let now = chrono::Local::now();
        let today_four = now.date_naive().and_hms_opt(16, 0, 0).unwrap();
        let next = if now.naive_local() < today_four {
            today_four
        } else {
            (now.date_naive() + chrono::Days::new(1))
                .and_hms_opt(16, 0, 0)
                .unwrap()
        };
        let wait = (next - now.naive_local()).to_std().unwrap_or_default();
        tracing::info!("next gardener run in {}s", wait.as_secs());
        tokio::time::sleep(wait).await;

        let gardeners = {
            let s = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            s.list_gardeners().unwrap_or_default()
        };
        // manual-cadence tendings only run via run-now; the daily cut skips them
        for g in gardeners
            .into_iter()
            .filter(|g| g.enabled && g.schedule != "manual")
        {
            let name = g.name.clone();
            let out = garden::run_gardener(store.clone(), g).await;
            tracing::info!("gardener {name}: {} — {}", out.status, out.summary);
        }
    }
}
