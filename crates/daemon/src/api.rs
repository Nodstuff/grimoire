//! JSON API for the human UI (M5). This surface acts as the HUMAN principal
//! (tom) — resolve here is Tom clicking accept/decline. Agents use MCP.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use ks_store::{BlockStore, ReviewDecision, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<SqliteStore>>,
    pub human: Uuid,
}

async fn docs(State(st): State<ApiState>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.list_docs() {
        Ok(d) => Json(json!(d)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn doc(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.read_doc(id) {
        Ok(t) => Json(json!(t)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn backlinks(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.backlinks(id) {
        Ok(b) => Json(json!(b)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn queue(State(st): State<ApiState>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.review_queue(None) {
        Ok(q) => {
            // decorate with doc titles + principal names for rendering
            let rows: Vec<Value> = q
                .into_iter()
                .map(|item| {
                    let doc_title = s
                        .get_doc(item.annotation.doc_id)
                        .map(|d| d.title)
                        .unwrap_or_default();
                    let proposer = s
                        .get_principal(item.op.principal)
                        .map(|p| p.display_name)
                        .unwrap_or_default();
                    let current = item
                        .op
                        .kind
                        .target_block()
                        .and_then(|t| s.read_block(t).ok())
                        .map(|b| b.content);
                    json!({
                        "item": item,
                        "doc_title": doc_title,
                        "proposer": proposer,
                        "current_content": current,
                    })
                })
                .collect();
            Json(json!(rows))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct ResolveReq {
    annotation_id: Uuid,
    decision: String,
}

async fn resolve(State(st): State<ApiState>, Json(req): Json<ResolveReq>) -> Json<Value> {
    let decision = match req.decision.as_str() {
        "accept" => ReviewDecision::Accept,
        "decline" => ReviewDecision::Decline,
        other => return Json(json!({"error": format!("bad decision: {other}")})),
    };
    let mut s = st.store.lock().unwrap();
    match s.resolve(req.annotation_id, st.human, decision) {
        Ok(receipt) => Json(json!({"ok": true, "receipt": receipt})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(State(st): State<ApiState>, Query(p): Query<SearchQuery>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.search_blocks(&p.q, 20) {
        Ok(h) => Json(json!(h)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn tags(State(st): State<ApiState>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.list_tags() {
        Ok(t) => Json(json!(t)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn runs(State(st): State<ApiState>) -> Json<Value> {
    let s = st.store.lock().unwrap();
    match s.list_runs(20) {
        Ok(r) => Json(json!(r)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/docs", get(docs))
        .route("/api/doc/{id}", get(doc))
        .route("/api/doc/{id}/backlinks", get(backlinks))
        .route("/api/queue", get(queue))
        .route("/api/resolve", post(resolve))
        .route("/api/search", get(search))
        .route("/api/tags", get(tags))
        .route("/api/runs", get(runs))
        .with_state(state)
}
