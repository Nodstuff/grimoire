//! Workspaces over HTTP (store: `grimoire_store::workspaces`). A workspace
//! is a label on a doc, inherited by its subtree; unlabelled docs are
//! Unsorted. Nothing here moves or deletes a doc.
//!
//! - `GET /api/workspaces` → `{workspaces: [{id, name, color, icon,
//!   sort_key, created_at, doc_ids, doc_count}], unsorted_count}` —
//!   `doc_ids` are the explicitly labelled live docs (with each doc's
//!   `parent_id` that resolves any doc locally), `doc_count` the live docs
//!   resolving to it, `unsorted_count` those resolving to none.
//! - `POST /api/workspaces {name, color?, icon?, sort_key?, request_id?}` →
//!   the workspace. Names are unique case-insensitively; "Unsorted" is
//!   reserved. No `sort_key` appends.
//! - `PATCH /api/workspaces/{id} {name?, color?, icon?, sort_key?}` — an
//!   absent field is kept, `null` clears color/icon/sort_key.
//! - `DELETE /api/workspaces/{id}` → `{deleted: id, unlabelled: n}`: the
//!   labels go, the docs stay (Unsorted, or an outer workspace).
//! - `PUT /api/docs/{id}/workspace {workspace_id | null, request_id?}` →
//!   `{doc_id, label, workspace_id}`: `label` is the doc's own label,
//!   `workspace_id` what it resolves to now.
//!
//! Every label or workspace change journals a `tree` change for each doc it
//! touches (`/api/changes`, whose `doc` summaries carry `workspace_id`).
//! Errors are real statuses with `{error}`: 400 bad input, 404 unknown id,
//! 409 a taken name, 500 a store failure.

use crate::api::ApiState;
use crate::store_ext::with_store;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, put};
use axum::{Json, Router};
use grimoire_store::{SqliteStore, StoreError, Workspace, WorkspaceFilter, WorkspacePatch};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

fn fail(e: StoreError) -> Response {
    let code = match &e {
        StoreError::NotFound(_) => StatusCode::NOT_FOUND,
        StoreError::InvalidOp(m) if m.contains("already exists") => StatusCode::CONFLICT,
        StoreError::InvalidOp(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, Json(json!({"error": e.to_string()}))).into_response()
}

/// A workspace as the API hands it out, with its resolved doc count.
fn ws_json(w: &Workspace, counts: &HashMap<Option<Uuid>, usize>) -> Value {
    let mut v = json!(w);
    v["doc_count"] = json!(counts.get(&Some(w.id)).copied().unwrap_or(0));
    v
}

fn counts(s: &SqliteStore) -> grimoire_store::Result<HashMap<Option<Uuid>, usize>> {
    let mut out = HashMap::new();
    for ws in s.workspace_map()?.into_values() {
        *out.entry(ws).or_default() += 1;
    }
    Ok(out)
}

async fn list(State(st): State<ApiState>) -> Response {
    with_store(&st.store, |s| {
        let run = || -> grimoire_store::Result<Value> {
            let c = counts(s)?;
            let all: Vec<Value> = s.list_workspaces()?.iter().map(|w| ws_json(w, &c)).collect();
            Ok(json!({"workspaces": all, "unsorted_count": c.get(&None).copied().unwrap_or(0)}))
        };
        match run() {
            Ok(v) => Json(v).into_response(),
            Err(e) => fail(e),
        }
    })
    .await
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    sort_key: Option<String>,
    #[serde(default)]
    request_id: Option<Uuid>,
}

async fn create(State(st): State<ApiState>, Json(req): Json<CreateReq>) -> Response {
    let (human, dedupe) = (st.human, st.dedupe.clone());
    with_store(&st.store, move |s| {
        if let Some(rid) = req.request_id
            && let Some(prev) = crate::mcp::durable_get(s, &dedupe, human, rid, crate::mcp::REQUEST_ID_TTL)
        {
            return Json(prev).into_response();
        }
        let made = s
            .create_workspace(&req.name, req.color.as_deref(), req.icon.as_deref(), req.sort_key.as_deref())
            .and_then(|w| Ok(ws_json(&w, &counts(s)?)));
        match made {
            Ok(v) => {
                if let Some(rid) = req.request_id {
                    crate::mcp::durable_put(s, &dedupe, human, rid, v.clone(), crate::mcp::REQUEST_ID_TTL);
                }
                Json(v).into_response()
            }
            Err(e) => fail(e),
        }
    })
    .await
}

/// Absent → None, `null` → Some(None), a value → Some(Some(v)).
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

#[derive(Deserialize)]
struct PatchReq {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    color: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    icon: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    sort_key: Option<Option<String>>,
}

async fn update(State(st): State<ApiState>, Path(id): Path<Uuid>, Json(req): Json<PatchReq>) -> Response {
    with_store(&st.store, move |s| {
        let patch = WorkspacePatch { name: req.name, color: req.color, icon: req.icon, sort_key: req.sort_key };
        match s.update_workspace(id, patch).and_then(|w| Ok(ws_json(&w, &counts(s)?))) {
            Ok(v) => Json(v).into_response(),
            Err(e) => fail(e),
        }
    })
    .await
}

async fn remove(State(st): State<ApiState>, Path(id): Path<Uuid>) -> Response {
    with_store(&st.store, move |s| match s.delete_workspace(id) {
        Ok(n) => Json(json!({"deleted": id, "unlabelled": n})).into_response(),
        Err(e) => fail(e),
    })
    .await
}

#[derive(Deserialize)]
struct AssignReq {
    /// required key: `null` clears the label
    workspace_id: Option<Uuid>,
    #[serde(default)]
    request_id: Option<Uuid>,
}

async fn assign(State(st): State<ApiState>, Path(doc): Path<Uuid>, Json(req): Json<AssignReq>) -> Response {
    let (human, dedupe) = (st.human, st.dedupe.clone());
    with_store(&st.store, move |s| {
        if let Some(rid) = req.request_id
            && let Some(prev) = crate::mcp::durable_get(s, &dedupe, human, rid, crate::mcp::REQUEST_ID_TTL)
        {
            return Json(prev).into_response();
        }
        let done = s.set_doc_workspace(doc, req.workspace_id).and_then(|resolved| {
            Ok(json!({"doc_id": doc, "label": s.doc_label(doc)?, "workspace_id": resolved}))
        });
        match done {
            Ok(v) => {
                if let Some(rid) = req.request_id {
                    crate::mcp::durable_put(s, &dedupe, human, rid, v.clone(), crate::mcp::REQUEST_ID_TTL);
                }
                Json(v).into_response()
            }
            Err(e) => fail(e),
        }
    })
    .await
}

/// A `?workspace=` value: `unsorted`, an id, or a name (case-insensitive).
/// Blank = no filter.
pub fn filter_param(s: &SqliteStore, raw: Option<&str>) -> Result<Option<WorkspaceFilter>, String> {
    match raw.map(str::trim).filter(|w| !w.is_empty()) {
        None => Ok(None),
        Some(w) => s.parse_workspace_filter(w).map(Some).map_err(|e| format!("workspace: {e}")),
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/workspaces", get(list).post(create))
        .route("/api/workspaces/{id}", patch(update).delete(remove))
        .route("/api/docs/{id}/workspace", put(assign))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use grimoire_store::{BlockStore, PrincipalKind};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    pub(crate) fn app() -> (Router, Arc<Mutex<SqliteStore>>, Uuid) {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let human = store.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let dir = std::env::temp_dir().join(format!("grimoire-ws-test-{}", Uuid::now_v7()));
        let store = Arc::new(Mutex::new(store));
        let st = ApiState {
            changes: crate::changes::Feed::new(&store),
            store: store.clone(),
            human,
            db_path: dir.join("ks.db"),
            embedder: None,
            dedupe: crate::mcp::new_dedupe(),
        };
        (crate::api::router(st), store, human)
    }

    pub(crate) async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(path);
        let req = match body {
            Some(b) => req.header("content-type", "application/json").body(Body::from(serde_json::to_vec(&b).unwrap())).unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn crud_assign_filters_and_change_rows() {
        let (app, store, human) = app();
        let (a, b, loose) = {
            let mut s = store.lock().unwrap();
            let ops = |md: &str| grimoire_store::import::to_ops(grimoire_store::import::segment(md));
            let a = s.create_doc("Projects", None, human).unwrap().id;
            let b = s.create_doc_with_ops("Grimoire", Some(a), human, ops("workspace needle here\n")).unwrap().0.id;
            let loose = s.create_doc_with_ops("Loose", None, human, ops("workspace needle there\n")).unwrap().0.id;
            (a, b, loose)
        };

        let rid = Uuid::now_v7();
        let (code, work) = call(&app, "POST", "/api/workspaces", Some(json!({"name": "Work", "color": "#36c", "request_id": rid}))).await;
        assert_eq!(code, StatusCode::OK, "{work}");
        let (_, again) = call(&app, "POST", "/api/workspaces", Some(json!({"name": "Work", "color": "#36c", "request_id": rid}))).await;
        assert_eq!(again["id"], work["id"], "a request_id replays the first outcome");
        let (code, _) = call(&app, "POST", "/api/workspaces", Some(json!({"name": "work"}))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        let wid = work["id"].as_str().unwrap().to_string();

        let (_, head) = call(&app, "GET", "/api/changes?limit=0", None).await;
        let (code, put) = call(&app, "PUT", &format!("/api/docs/{a}/workspace"), Some(json!({"workspace_id": wid}))).await;
        assert_eq!(code, StatusCode::OK, "{put}");
        assert_eq!(put["workspace_id"], json!(wid));
        let (_, page) = call(&app, "GET", &format!("/api/changes?since={}", head["seq"]), None).await;
        let rows = page["changes"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "the labelled doc and its child: {page}");
        assert!(rows.iter().all(|r| r["kind"] == "tree" && r["doc"]["workspace_id"] == json!(wid)));

        // /api/docs: workspace_id on every entry, ?workspace filters by subtree
        let (_, all) = call(&app, "GET", "/api/docs", None).await;
        let ws_of = |id: Uuid| all.as_array().unwrap().iter().find(|d| d["id"] == json!(id)).unwrap()["workspace_id"].clone();
        assert_eq!(ws_of(b), json!(wid));
        assert_eq!(ws_of(loose), Value::Null);
        let (_, only) = call(&app, "GET", &format!("/api/docs?workspace={wid}"), None).await;
        assert_eq!(only.as_array().unwrap().len(), 2);
        let (_, by_name) = call(&app, "GET", "/api/docs?workspace=WORK", None).await;
        assert_eq!(by_name.as_array().unwrap().len(), 2);
        let (_, unsorted) = call(&app, "GET", "/api/docs?workspace=unsorted", None).await;
        assert_eq!(unsorted.as_array().unwrap().iter().map(|d| d["title"].as_str().unwrap()).collect::<Vec<_>>(), ["Loose"]);
        let (_, bad) = call(&app, "GET", "/api/docs?workspace=nope", None).await;
        assert!(bad["error"].is_string());

        // search
        let (_, hits) = call(&app, "GET", "/api/search?q=workspace%20needle", None).await;
        assert_eq!(hits.as_array().unwrap().len(), 2);
        let (_, hits) = call(&app, "GET", &format!("/api/search?q=workspace%20needle&workspace={wid}"), None).await;
        assert_eq!(hits.as_array().unwrap().iter().map(|h| h["block"]["doc_id"].clone()).collect::<Vec<_>>(), [json!(b)]);
        let (_, hits) = call(&app, "GET", "/api/search?q=workspace%20needle&workspace=unsorted", None).await;
        assert_eq!(hits.as_array().unwrap().iter().map(|h| h["block"]["doc_id"].clone()).collect::<Vec<_>>(), [json!(loose)]);

        // list: counts
        let (_, list) = call(&app, "GET", "/api/workspaces", None).await;
        assert_eq!(list["workspaces"][0]["doc_count"], 2);
        assert_eq!(list["workspaces"][0]["doc_ids"], json!([a]));
        assert_eq!(list["unsorted_count"], 1);

        // patch: absent keeps, null clears
        let (_, p) = call(&app, "PATCH", &format!("/api/workspaces/{wid}"), Some(json!({"icon": "briefcase"}))).await;
        assert_eq!((p["color"].clone(), p["icon"].clone()), (json!("#36c"), json!("briefcase")));
        let (_, p) = call(&app, "PATCH", &format!("/api/workspaces/{wid}"), Some(json!({"color": null, "name": "Job"}))).await;
        assert_eq!((p["color"].clone(), p["name"].clone()), (Value::Null, json!("Job")));

        // un-assign, re-assign, delete un-labels without deleting docs
        let (_, put) = call(&app, "PUT", &format!("/api/docs/{a}/workspace"), Some(json!({"workspace_id": null}))).await;
        assert_eq!(put["workspace_id"], Value::Null);
        call(&app, "PUT", &format!("/api/docs/{a}/workspace"), Some(json!({"workspace_id": wid}))).await;
        let (code, _) = call(&app, "PUT", &format!("/api/docs/{a}/workspace"), Some(json!({"workspace_id": Uuid::now_v7()}))).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let (code, del) = call(&app, "DELETE", &format!("/api/workspaces/{wid}"), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(del["unlabelled"], 1);
        let (_, all) = call(&app, "GET", "/api/docs", None).await;
        assert_eq!(all.as_array().unwrap().len(), 3);
        assert!(all.as_array().unwrap().iter().all(|d| d["workspace_id"].is_null()));
        let (code, _) = call(&app, "DELETE", &format!("/api/workspaces/{wid}"), None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }
}
