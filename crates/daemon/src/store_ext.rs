//! Run store work off the async workers.
//!
//! The daemon holds one `SharedStore`; every guard is block-scoped before
//! any `.await`, but the SQLite work itself used to run on the tokio worker
//! threads, so one long query stalled every other request on that worker.
//! `with_store` moves the lock + query onto the blocking pool and hands the
//! result back. It takes the `Scope` the work runs for (ADR 0004): there is
//! no way to reach the store without one.

use taisce_store::{BlockStore, PrincipalKind, Scope, SharedStore, SqliteStore};
use uuid::Uuid;

/// Lock the store for `scope` inside `spawn_blocking`, run `f`, return its
/// value. A poisoned lock is recovered; a panic inside `f` is re-raised on
/// the caller so it is not silently lost.
pub async fn with_store<T: Send + 'static>(
    store: &SharedStore,
    scope: Scope,
    f: impl FnOnce(&mut SqliteStore) -> T + Send + 'static,
) -> T {
    let store = store.clone();
    blocking(move || {
        let mut s = store.lock(scope);
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

/// The agent principal ask-the-vault, living answers and memory sync write
/// under ("scribe"), created on first use.
pub fn scribe_principal(store: &mut SqliteStore) -> taisce_store::Result<Uuid> {
    const NAME: &str = "scribe";
    if let Some(p) = store.principal_named(PrincipalKind::Agent, NAME)? {
        return Ok(p.id);
    }
    Ok(store.create_principal(PrincipalKind::Agent, NAME, None)?.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_closure_and_returns_value() {
        let store = SharedStore::new(SqliteStore::open_in_memory().unwrap());
        let n = with_store(&store, Scope::System, |s| s.list_docs().unwrap().len()).await;
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn recovers_poisoned_lock() {
        let store = SharedStore::new(SqliteStore::open_in_memory().unwrap());
        store.poison_for_test();
        assert!(store.is_poisoned());
        let n = with_store(&store, Scope::System, |s| s.list_docs().unwrap().len()).await;
        assert_eq!(n, 0);
    }

    #[tokio::test]
    #[should_panic(expected = "boom")]
    async fn panic_in_closure_propagates() {
        let store = SharedStore::new(SqliteStore::open_in_memory().unwrap());
        with_store(&store, Scope::System, |_| panic!("boom")).await;
    }

    #[tokio::test]
    async fn the_lock_runs_in_the_scope_it_was_given() {
        let store = SharedStore::new(SqliteStore::open_in_memory().unwrap());
        let u = Uuid::now_v7();
        assert_eq!(with_store(&store, Scope::User(u), |s| s.scope()).await, Scope::User(u));
        assert_eq!(with_store(&store, Scope::Local, |s| s.scope()).await, Scope::Local);
    }
}
