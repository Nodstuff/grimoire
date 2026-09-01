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

// --- federation admin (ADR 0002; #57). Human surface only, never MCP. ---

/// Federation context for the admin routes; both None when the instance has
/// no identity (federation disabled).
#[derive(Clone)]
pub struct FedCtx {
    pub node_id: Option<String>,
    pub endpoint: Option<iroh::Endpoint>,
}

#[derive(Clone)]
pub struct FedState {
    pub store: Store,
    pub ctx: FedCtx,
}

#[derive(Deserialize)]
pub struct CreateShare {
    pub root_doc: Uuid,
    /// "view" (default) or "propose"
    pub permission: Option<String>,
}

async fn create_share(State(st): State<FedState>, Json(req): Json<CreateShare>) -> Json<Value> {
    let Some(node_id) = &st.ctx.node_id else {
        return Json(json!({"error": "federation disabled: no instance identity"}));
    };
    let permission = match req.permission.as_deref() {
        None => grimoire_store::SharePermission::View,
        Some(p) => match grimoire_store::SharePermission::parse(p) {
            Some(p) => p,
            None => return Json(json!({"error": format!("bad permission: {p}")})),
        },
    };
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match crate::fed::mint_invite(&mut s, node_id, req.root_doc, permission) {
        Ok((share, link)) => Json(json!({"share": share, "link": link})),
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

async fn list_shares(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_shares() {
        Ok(shares) => Json(json!(shares)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
pub struct IdReq {
    pub id: Uuid,
}

async fn revoke_share(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.set_share_state(req.id, grimoire_store::ShareState::Revoked) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn list_contacts(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_contacts() {
        Ok(c) => Json(json!(c)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
pub struct VerifyReq {
    pub id: Uuid,
    pub verified: bool,
}

async fn verify_contact(State(st): State<FedState>, Json(req): Json<VerifyReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.set_contact_verified(req.id, req.verified) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
pub struct RenameContactReq {
    pub id: Uuid,
    pub petname: String,
}

async fn rename_contact(
    State(st): State<FedState>,
    Json(req): Json<RenameContactReq>,
) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.rename_contact(req.id, req.petname.trim()) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn revoke_contact(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.revoke_contact(req.id) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
pub struct JoinReq {
    pub link: String,
}

/// Join now if the owner is reachable; otherwise queue for the retry loop.
/// Either way the caller learns which happened (async redeem, ADR 0002).
async fn join(State(st): State<FedState>, Json(req): Json<JoinReq>) -> Json<Value> {
    let ticket = match crate::fed::Ticket::parse(&req.link) {
        Ok(t) => t,
        Err(e) => return Json(json!({"error": format!("{e:#}")})),
    };
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "federation disabled: no instance identity"}));
    };
    let attempt = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        crate::fed::join_once(endpoint, &st.store, &ticket),
    )
    .await;
    let err = match attempt {
        Ok(Ok(outcome)) => return Json(json!({"joined": outcome})),
        Ok(Err(e)) => format!("{e:#}"),
        Err(_) => "owner unreachable (timed out)".into(),
    };
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.queue_join(&req.link) {
        Ok(id) => {
            s.record_join_attempt(id, &err).ok();
            Json(json!({"queued": true, "pending_join": id, "last_error": err}))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
pub struct ProposeUpstreamReq {
    pub doc_id: Uuid,
    pub ops: Vec<grimoire_store::OpInput>,
    #[serde(default)]
    pub note: String,
}

async fn propose_upstream(
    State(st): State<FedState>,
    Json(req): Json<ProposeUpstreamReq>,
) -> Json<Value> {
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "federation disabled: no instance identity"}));
    };
    match crate::fed::propose_upstream(endpoint, &st.store, req.doc_id, req.ops, &req.note).await {
        Ok(id) => Json(json!({"proposal": id, "state": "pending"})),
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

async fn list_proposals(State(st): State<FedState>) -> Json<Value> {
    if let Some(endpoint) = &st.ctx.endpoint {
        crate::fed::refresh_outbound(endpoint, &st.store).await;
    }
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_outbound_proposals(false) {
        Ok(p) => Json(json!(p)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn pull_now(State(st): State<FedState>) -> Json<Value> {
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "federation disabled: no instance identity"}));
    };
    let results = crate::fed::pull_all_once(endpoint, &st.store).await;
    let out: Vec<Value> = results
        .into_iter()
        .map(|(share, res)| match res {
            Ok(s) => json!({"share": share, "changed": s.changed, "removed": s.removed}),
            Err(e) => json!({"share": share, "error": format!("{e:#}")}),
        })
        .collect();
    Json(json!(out))
}

async fn list_joins(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_pending_joins() {
        Ok(j) => Json(json!(j)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

pub fn router(store: Store, fed: FedCtx) -> Router {
    let fed_routes = Router::new()
        .route("/admin/shares", get(list_shares).post(create_share))
        .route("/admin/shares/revoke", post(revoke_share))
        .route("/admin/contacts", get(list_contacts))
        .route("/admin/contacts/revoke", post(revoke_contact))
        .route("/admin/contacts/verify", post(verify_contact))
        .route("/admin/contacts/rename", post(rename_contact))
        .route("/admin/join", post(join))
        .route("/admin/joins", get(list_joins))
        .route("/admin/pull", post(pull_now))
        .route("/admin/propose_upstream", post(propose_upstream))
        .route("/admin/proposals", get(list_proposals))
        .with_state(FedState {
            store: store.clone(),
            ctx: fed,
        });
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
        .merge(fed_routes)
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
