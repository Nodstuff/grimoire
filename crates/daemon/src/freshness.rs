//! Doc freshness: `docs.verified_at` and the stale-docs list.
//!
//! A doc is *verified* when an auditor or keeper run evaluated it and found
//! nothing (`garden::run_auditor`), or when a human accepted a fix one of
//! them proposed (the store's `resolve`). Ordinary edits — human or agent —
//! never set it. `GET /api/freshness` lists owned docs never-verified first,
//! then oldest verification; the doc header reads `GET /api/doc/{id}/freshness`.

use crate::api::ApiState;
use grimoire_store::BlockStore;
use crate::store_ext::with_store;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 500;

#[derive(Deserialize, Default)]
pub struct FreshnessQuery {
    pub limit: Option<usize>,
    /// `true` = only docs under an enabled gardener scope.
    pub tended: Option<bool>,
}

/// `GET /api/freshness?limit=50[&tended=true]`
pub async fn freshness(State(st): State<ApiState>, Query(q): Query<FreshnessQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let tended_only = q.tended.unwrap_or(false);
    with_store(&st.store, move |s| {
        let docs = s.list_docs().unwrap_or_default();
        let crumbs = crate::nav::breadcrumbs(&docs);
        match s.freshness(limit, tended_only) {
            Ok(rows) => Json(json!(
                rows.into_iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "title": r.title,
                            "path": crumbs.get(&r.id).cloned().unwrap_or_else(|| r.title.clone()),
                            "verified_at": r.verified_at,
                            "last_edited": r.last_edited,
                            "tended": r.tended,
                        })
                    })
                    .collect::<Vec<_>>()
            )),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

/// `GET /api/doc/{id}/freshness` → `{verified_at}` for the header chip.
pub async fn doc_freshness(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Json<Value> {
    with_store(&st.store, move |s| match s.doc_verified_at(id) {
        Ok(v) => Json(json!({"verified_at": v})),
        Err(e) => Json(json!({"error": e.to_string()})),
    })
    .await
}
