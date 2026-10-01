//! The change feed: a sync cursor over the store's change journal
//! (`changes` table, written by triggers on every write path) for clients
//! that keep an offline cache.
//!
//! - `GET /api/changes?since=<seq>&limit=<n>` → `{seq, changes: [{seq,
//!   doc_id, kind, epoch, at}], more}` — rows after `since` in seq order;
//!   `limit` defaults to 500, capped at 2000; `seq` is the journal's head.
//! - `GET /api/changes/stream` — Server-Sent Events. Resumes after the
//!   `Last-Event-ID` header (else `?since=`, else 0): replays the backlog,
//!   then pushes rows as they land. Each event is `id: <seq>`, `event:
//!   change`, `data: <one change object>`; a `: ping` comment every 25s.
//!
//! Waking: the store's commit hook pokes a [`Notify`]; ONE pump task per
//! daemon re-reads `max(seq)` and publishes it on a `watch` channel, with a
//! 2s re-read as the fallback for writes the hook cannot see (another
//! process on the same file). Idle streams hold only a watch receiver — no
//! store lock, no timer of their own — so many of them cost nothing.

use crate::api::ApiState;
use crate::store_ext::with_store;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use grimoire_store::{Change, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;
use tokio::sync::{Notify, watch};

pub const DEFAULT_LIMIT: usize = 500;
pub const MAX_LIMIT: usize = 2000;
const HEARTBEAT: Duration = Duration::from_secs(25);
/// The pump's re-read when no commit poked it.
const FALLBACK_POLL: Duration = Duration::from_secs(2);
/// Rows a stream reads per store round-trip while replaying.
const STREAM_PAGE: usize = 500;

/// The journal head, published to every open stream.
#[derive(Clone)]
pub struct Feed(Arc<Inner>);

struct Inner {
    store: Arc<Mutex<SqliteStore>>,
    wake: Arc<Notify>,
    head: watch::Sender<i64>,
    pump: Once,
}

impl Feed {
    /// Install the commit hook on `store`. The pump starts with the first
    /// stream, so constructing a feed needs no runtime.
    pub fn new(store: &Arc<Mutex<SqliteStore>>) -> Self {
        let wake = Arc::new(Notify::new());
        let w = wake.clone();
        store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .on_commit(move || w.notify_one());
        let (head, _) = watch::channel(0);
        Feed(Arc::new(Inner { store: store.clone(), wake, head, pump: Once::new() }))
    }

    fn subscribe(&self) -> watch::Receiver<i64> {
        let inner = self.0.clone();
        self.0.pump.call_once(move || {
            tokio::spawn(pump(inner));
        });
        self.0.head.subscribe()
    }
}

async fn pump(inner: Arc<Inner>) {
    loop {
        // a commit landing during the read leaves a permit: no lost wake-up
        if let Ok(seq) = with_store(&inner.store, |s| s.latest_change_seq()).await {
            inner.head.send_if_modified(|h| {
                let moved = *h != seq;
                *h = seq;
                moved
            });
        }
        tokio::select! {
            _ = inner.wake.notified() => {}
            _ = tokio::time::sleep(FALLBACK_POLL) => {}
        }
    }
}

#[derive(Deserialize)]
struct ChangesQuery {
    #[serde(default)]
    since: i64,
    limit: Option<usize>,
}

async fn changes(State(st): State<ApiState>, Query(q): Query<ChangesQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    with_store(&st.store, move |s| match s.changes_since(q.since, limit) {
        Ok(page) => Json(json!(page)),
        Err(e) => Json(json!({"error": e.to_string()})),
    })
    .await
}

#[derive(Deserialize)]
struct StreamQuery {
    since: Option<i64>,
}

/// Where a stream resumes: `Last-Event-ID` wins over `?since=`.
fn resume_from(headers: &HeaderMap, since: Option<i64>) -> i64 {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .or(since)
        .unwrap_or(0)
}

struct Cursor {
    store: Arc<Mutex<SqliteStore>>,
    head: watch::Receiver<i64>,
    after: i64,
    queue: VecDeque<Change>,
}

fn change_event(c: &Change) -> Event {
    Event::default()
        .id(c.seq.to_string())
        .event("change")
        .json_data(c)
        .unwrap_or_else(|_| Event::default().comment("unserializable change"))
}

/// The next change after the cursor, waiting for the head to move when the
/// backlog is drained. Ends only when the feed itself is gone.
async fn next_change(mut cur: Cursor) -> Option<(Result<Event, Infallible>, Cursor)> {
    loop {
        if let Some(c) = cur.queue.pop_front() {
            cur.after = c.seq;
            return Some((Ok(change_event(&c)), cur));
        }
        // mark the head seen BEFORE reading, so a row landing after the read
        // still wakes `changed()` below
        cur.head.borrow_and_update();
        let after = cur.after;
        match with_store(&cur.store, move |s| s.changes_since(after, STREAM_PAGE)).await {
            Ok(page) if !page.changes.is_empty() => {
                cur.queue.extend(page.changes);
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("change stream read failed: {e}");
                tokio::time::sleep(FALLBACK_POLL).await;
                continue;
            }
        }
        cur.head.changed().await.ok()?;
    }
}

fn change_stream(feed: &Feed, store: Arc<Mutex<SqliteStore>>, after: i64) -> impl Stream<Item = Result<Event, Infallible>> + use<> {
    let cur = Cursor { store, head: feed.subscribe(), after, queue: VecDeque::new() };
    stream::unfold(cur, next_change)
}

async fn changes_stream(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let after = resume_from(&headers, q.since);
    Sse::new(change_stream(&st.changes, st.store.clone(), after))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT).text("ping"))
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/changes", get(changes))
        .route("/api/changes/stream", get(changes_stream))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use futures_util::StreamExt;
    use grimoire_store::{BlockStore, PrincipalKind};
    use tower::ServiceExt;

    #[test]
    fn last_event_id_beats_since() {
        let mut h = HeaderMap::new();
        assert_eq!(resume_from(&h, None), 0);
        assert_eq!(resume_from(&h, Some(7)), 7);
        h.insert("last-event-id", "42".parse().unwrap());
        assert_eq!(resume_from(&h, Some(7)), 42);
        h.insert("last-event-id", "junk".parse().unwrap());
        assert_eq!(resume_from(&h, Some(7)), 7, "an unreadable id falls back");
    }

    #[tokio::test]
    async fn changes_route_pages_with_since_limit_more() {
        let (app, _) = crate::home::testing::app();
        for i in 0..3 {
            crate::home::testing::call(&app, "POST", "/api/docs", Some(json!({"title": format!("d{i}"), "parent_doc_id": null}))).await;
        }
        let v = crate::home::testing::call(&app, "GET", "/api/changes?since=0&limit=2", None).await;
        assert_eq!(v["seq"], 3);
        assert_eq!(v["more"], true);
        let rows = v["changes"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["seq"], 1);
        assert_eq!(rows[0]["kind"], "tree");
        assert_eq!(rows[0]["epoch"], 0);
        for k in ["seq", "doc_id", "kind", "epoch", "at"] {
            assert!(rows[0].get(k).is_some(), "{k}");
        }
        let v = crate::home::testing::call(&app, "GET", "/api/changes?since=2", None).await;
        assert_eq!(v["more"], false);
        assert_eq!(v["changes"].as_array().unwrap().len(), 1);
        // limit is clamped, never an error
        let v = crate::home::testing::call(&app, "GET", "/api/changes?limit=999999", None).await;
        assert_eq!(v["changes"].as_array().unwrap().len(), 3);
    }

    /// Read SSE frames off a body until `n` `event: change` frames arrived.
    async fn read_events(body: Body, n: usize) -> Vec<(i64, Value)> {
        let mut stream = body.into_data_stream();
        let mut buf = String::new();
        let mut out = Vec::new();
        while out.len() < n {
            let chunk = tokio::time::timeout(Duration::from_secs(5), stream.next())
                .await
                .expect("an event within 5s")
                .expect("stream open")
                .unwrap();
            buf.push_str(std::str::from_utf8(&chunk).unwrap());
            while let Some(end) = buf.find("\n\n") {
                let frame: String = buf.drain(..end + 2).collect();
                let mut id = None;
                let mut data = None;
                let mut event = None;
                for line in frame.lines() {
                    if let Some(v) = line.strip_prefix("id: ") {
                        id = Some(v.parse::<i64>().unwrap());
                    } else if let Some(v) = line.strip_prefix("data: ") {
                        data = Some(serde_json::from_str::<Value>(v).unwrap());
                    } else if let Some(v) = line.strip_prefix("event: ") {
                        event = Some(v.to_string());
                    }
                }
                if event.as_deref() == Some("change") {
                    let (id, data) = (id.unwrap(), data.unwrap());
                    assert_eq!(data["seq"], id, "id is the row's seq");
                    out.push((id, data));
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn stream_replays_after_last_event_id_then_pushes_live_rows() {
        let (app, _) = crate::home::testing::app();
        for i in 0..3 {
            crate::home::testing::call(&app, "POST", "/api/docs", Some(json!({"title": format!("d{i}"), "parent_doc_id": null}))).await;
        }
        // `?since=` is the fallback cursor; Last-Event-ID wins over it
        let res = app
            .clone()
            .oneshot(
                Request::get("/api/changes/stream?since=0")
                    .header("last-event-id", "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        let body = res.into_body();
        let reader = tokio::spawn(read_events(body, 3));
        // replay is 2 and 3; then a live write must arrive as 4
        tokio::time::sleep(Duration::from_millis(100)).await;
        crate::home::testing::call(&app, "POST", "/api/docs", Some(json!({"title": "live", "parent_doc_id": null}))).await;
        let got = reader.await.unwrap();
        assert_eq!(got.iter().map(|e| e.0).collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(got[2].1["kind"], "tree");
    }

    #[tokio::test]
    async fn a_store_write_outside_any_route_wakes_the_stream() {
        let store = Arc::new(Mutex::new(SqliteStore::open_in_memory().unwrap()));
        let feed = Feed::new(&store);
        let tom = {
            let mut s = store.lock().unwrap();
            let tom = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
            for i in 0..3 {
                s.create_doc(&format!("d{i}"), None, tom).unwrap();
            }
            tom
        };
        let mut st = Box::pin(change_stream(&feed, store.clone(), 2));
        let first = tokio::time::timeout(Duration::from_secs(5), st.next()).await.unwrap();
        assert!(first.is_some());
        // a write from outside any HTTP route (a gardener, MCP) still wakes it
        let s2 = store.clone();
        tokio::task::spawn_blocking(move || s2.lock().unwrap().create_doc("bg", None, tom).unwrap())
            .await
            .unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), st.next()).await.unwrap();
        assert!(second.is_some());
        assert_eq!(store.lock().unwrap().latest_change_seq().unwrap(), 4);
    }
}
