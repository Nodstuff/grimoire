//! Run store work off the async workers.
//!
//! The daemon holds one `Arc<Mutex<SqliteStore>>`; every guard is block-scoped
//! before any `.await`, but the SQLite work itself used to run on the tokio
//! worker threads, so one long query stalled every other request on that
//! worker. `with_store` moves the lock + query onto the blocking pool and
//! hands the result back; the Mutex type and the store signatures stay as
//! they are.

use grimoire_store::SqliteStore;
use std::sync::{Arc, Mutex};

/// Lock the store inside `spawn_blocking`, run `f`, return its value.
/// A poisoned lock is recovered (same as the inline `lock()` sites did); a
/// panic inside `f` is re-raised on the caller so it is not silently lost.
pub async fn with_store<T: Send + 'static>(
    store: &Arc<Mutex<SqliteStore>>,
    f: impl FnOnce(&mut SqliteStore) -> T + Send + 'static,
) -> T {
    let store = Arc::clone(store);
    blocking(move || {
        let mut s = store.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut s)
    })
    .await
}

/// `spawn_blocking` that re-raises a panic in `f` on the caller instead of
/// handing back a `JoinError`. For sync fns that lock the store themselves.
///
/// A blocking task is only ever *cancelled* (never started) when the runtime
/// is shutting down; there is no `T` to hand back, so the caller parks until
/// the shutdown drops it — a benign early exit rather than a panic that
/// would read as a store failure in the logs.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => {
            tracing::debug!("store task cancelled (runtime shutting down): {e}");
            std::future::pending().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_closure_and_returns_value() {
        let store = Arc::new(Mutex::new(SqliteStore::open_in_memory().unwrap()));
        let n = with_store(&store, |s| s.canvas_doc_ids().unwrap().len()).await;
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn recovers_poisoned_lock() {
        let store = Arc::new(Mutex::new(SqliteStore::open_in_memory().unwrap()));
        let poison = Arc::clone(&store);
        let _ = std::thread::spawn(move || {
            let _g = poison.lock().unwrap();
            panic!("poison");
        })
        .join();
        assert!(store.is_poisoned());
        let n = with_store(&store, |s| s.canvas_doc_ids().unwrap().len()).await;
        assert_eq!(n, 0);
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn panic_in_closure_propagates() {
        let store = Arc::new(Mutex::new(SqliteStore::open_in_memory().unwrap()));
        with_store(&store, |_| panic!("boom")).await;
    }
}
