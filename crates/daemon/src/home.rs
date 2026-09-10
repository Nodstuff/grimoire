//! The briefing home (the stage when no doc is open).
//!
//! - `GET/POST /api/home/visit`: the last-visit stamp (`settings`
//!   `home.last_visit`, ISO-8601 UTC). GET reads it; POST stamps now (or
//!   `{at}`) and answers with the previous value, so the page can render
//!   "since you were here" from what it displaced.
//! - `GET /api/home/since?since=<iso>`: docs created by NON-human principals
//!   (agents, remote peers) since the stamp — the "new docs" line of the
//!   briefing. Creation time comes from the doc's UUIDv7 (docs and mirrors
//!   both keep v7 ids), so no store change is needed.

use crate::api::ApiState;
use crate::store_ext::with_store;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use grimoire_store::{BlockStore, PrincipalKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

pub const LAST_VISIT_KEY: &str = "home.last_visit";

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Milliseconds since the epoch encoded in a UUIDv7, None for other versions.
pub fn uuid_v7_millis(id: Uuid) -> Option<i64> {
    if id.get_version_num() != 7 {
        return None;
    }
    let (secs, nanos) = id.get_timestamp()?.to_unix();
    Some(secs as i64 * 1000 + (nanos / 1_000_000) as i64)
}

fn parse_iso_millis(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp_millis())
}

async fn get_visit(State(st): State<ApiState>) -> Json<Value> {
    with_store(&st.store, move |s| match s.get_setting(LAST_VISIT_KEY) {
        Ok(v) => Json(json!({"last_visit": v})),
        Err(e) => Json(json!({"error": e.to_string()})),
    })
    .await
}

#[derive(Deserialize, Default)]
struct VisitReq {
    /// Override for the stamp (tests, or a page that wants "now" to be its
    /// own render time). Default: the daemon clock.
    at: Option<String>,
}

async fn post_visit(State(st): State<ApiState>, body: axum::body::Bytes) -> Json<Value> {
    let req: VisitReq = if body.is_empty() {
        VisitReq::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return Json(json!({"error": format!("bad body: {e}")})),
        }
    };
    let at = match req.at {
        Some(a) if parse_iso_millis(&a).is_some() => a,
        Some(a) => return Json(json!({"error": format!("`at` is not RFC 3339: {a}")})),
        None => now_iso(),
    };
    with_store(&st.store, move |s| {
        let previous = match s.get_setting(LAST_VISIT_KEY) {
            Ok(v) => v,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        match s.set_setting(LAST_VISIT_KEY, &at) {
            Ok(()) => Json(json!({"last_visit": at, "previous": previous})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

#[derive(Deserialize)]
struct SinceQuery {
    since: Option<String>,
    limit: Option<usize>,
}

async fn since(State(st): State<ApiState>, Query(q): Query<SinceQuery>) -> Json<Value> {
    let floor = q.since.as_deref().and_then(parse_iso_millis);
    if q.since.is_some() && floor.is_none() {
        return Json(json!({"error": "`since` is not RFC 3339"}));
    }
    let limit = q.limit.unwrap_or(20).min(200);
    with_store(&st.store, move |s| {
        let principals: HashMap<Uuid, (String, PrincipalKind)> = match s.list_principals() {
            Ok(ps) => ps.into_iter().map(|p| (p.id, (p.display_name, p.kind))).collect(),
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        let docs = match s.list_docs() {
            Ok(d) => d,
            Err(e) => return Json(json!({"error": e.to_string()})),
        };
        let mut rows: Vec<(i64, Value)> = docs
            .into_iter()
            .filter_map(|d| {
                let (name, kind) = principals.get(&d.created_by)?;
                if *kind == PrincipalKind::Human {
                    return None;
                }
                let ms = uuid_v7_millis(d.id)?;
                if floor.is_some_and(|f| ms < f) {
                    return None;
                }
                let created_at = chrono::DateTime::from_timestamp_millis(ms)?
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                Some((
                    ms,
                    json!({
                        "id": d.id,
                        "title": d.title,
                        "parent_id": d.parent_id,
                        "created_by": d.created_by,
                        "created_by_name": name,
                        "created_by_kind": kind.as_str(),
                        "created_at": created_at,
                    }),
                ))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        Json(json!({"docs": rows.into_iter().take(limit).map(|r| r.1).collect::<Vec<_>>()}))
    })
    .await
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/home/visit", get(get_visit).post(post_visit))
        .route("/api/home/since", get(since))
        .with_state(state)
}

/// Test scaffolding shared with `inbox`: an in-memory store behind the full
/// human API (api + home + inbox routers), and the human principal.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use grimoire_store::SqliteStore;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    pub fn app() -> (Router, Uuid) {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let human = store.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let dir = std::env::temp_dir().join(format!("grimoire-home-test-{}", Uuid::now_v7()));
        let st = ApiState {
            store: Arc::new(Mutex::new(store)),
            human,
            hot: crate::hot::HotState::new(dir.clone()),
            runtime: crate::fed::Runtime::default(),
            db_path: dir.join("ks.db"),
            node_id: None,
            embedder: None,
            dedupe: crate::mcp::new_dedupe(),
        };
        (crate::api::router(st), human)
    }

    pub async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> Value {
        let req = Request::builder().method(method).uri(path);
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{app, call};
    use super::*;

    #[tokio::test]
    async fn visit_stamp_round_trips_and_reports_the_displaced_value() {
        let (app, _) = app();
        let v = call(&app, "GET", "/api/home/visit", None).await;
        assert!(v["last_visit"].is_null(), "{v}");
        // first stamp: nothing displaced
        let v = call(&app, "POST", "/api/home/visit", Some(json!({"at": "2026-09-09T08:00:00Z"}))).await;
        assert_eq!(v["last_visit"], "2026-09-09T08:00:00Z");
        assert!(v["previous"].is_null());
        // second stamp (daemon clock) hands back the first
        let v = call(&app, "POST", "/api/home/visit", None).await;
        assert_eq!(v["previous"], "2026-09-09T08:00:00Z");
        let now = v["last_visit"].as_str().unwrap().to_string();
        assert!(parse_iso_millis(&now).is_some(), "{now}");
        let v = call(&app, "GET", "/api/home/visit", None).await;
        assert_eq!(v["last_visit"], now);
        // garbage is refused, nothing written
        let v = call(&app, "POST", "/api/home/visit", Some(json!({"at": "yesterday"}))).await;
        assert!(v["error"].as_str().unwrap().contains("RFC 3339"));
        let v = call(&app, "GET", "/api/home/visit", None).await;
        assert_eq!(v["last_visit"], now);
    }

    #[tokio::test]
    async fn since_lists_only_non_human_docs_after_the_stamp() {
        let (app, _) = app();
        // a human doc never shows; an agent doc does (X-Grimoire-Principal)
        call(&app, "POST", "/api/docs", Some(json!({"title": "mine", "parent_doc_id": null}))).await;
        let early = chrono::Utc::now().to_rfc3339();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let req = axum::http::Request::post("/api/docs")
            .header("content-type", "application/json")
            .header(crate::api::PRINCIPAL_HEADER, "scribe")
            .body(axum::body::Body::from(r#"{"title":"Answer 1","parent_doc_id":null}"#))
            .unwrap();
        use tower::ServiceExt;
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        let v = call(&app, "GET", "/api/home/since", None).await;
        let docs = v["docs"].as_array().unwrap();
        assert_eq!(docs.len(), 1, "{v}");
        assert_eq!(docs[0]["title"], "Answer 1");
        assert_eq!(docs[0]["created_by_name"], "scribe");
        assert_eq!(docs[0]["created_by_kind"], "agent");
        assert!(docs[0]["created_at"].as_str().unwrap() >= early.as_str());
        // a stamp in the future hides it
        let v = call(&app, "GET", "/api/home/since?since=2099-01-01T00:00:00Z", None).await;
        assert_eq!(v["docs"].as_array().unwrap().len(), 0);
        let v = call(&app, "GET", "/api/home/since?since=nope", None).await;
        assert!(v["error"].is_string());
    }

    #[test]
    fn v7_ids_carry_their_creation_time() {
        let before = chrono::Utc::now().timestamp_millis();
        let ms = uuid_v7_millis(Uuid::now_v7()).unwrap();
        assert!(ms >= before && ms <= before + 5_000, "{ms} vs {before}");
        assert_eq!(uuid_v7_millis(Uuid::new_v4()), None);
    }
}
