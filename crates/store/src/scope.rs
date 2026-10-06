//! Who is asking (ADR 0004). Every store read and write that touches docs,
//! blocks, workspaces or anything derived from them is filtered by the
//! store's current [`Scope`]; the daemon can only reach a store through
//! [`SharedStore::lock`], which takes one, so forgetting the scope is a
//! compile error rather than a leak.

use crate::sqlite::SqliteStore;
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;

/// The viewer a store call runs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Sees everything: migrations, the embed indexer, backups, the admin
    /// CLI, push fan-out, gardener bookkeeping. Never a request's scope in
    /// SERVER mode.
    System,
    /// LOCAL mode's single user (no `--public-url`): sees everything, as
    /// before multi-user existed.
    Local,
    /// A signed-in SERVER-mode user (OAuth grant or PAT owner).
    User(Uuid),
    /// `User(user)` narrowed to one workspace: automated writers whose target
    /// sits in a shared workspace read only from that workspace, so nothing
    /// private to one member is copied where the others read it.
    Within { user: Uuid, workspace: Uuid },
    /// A reader of a public share link (`/s/{token}`): no person, and no doc
    /// is visible (every visibility predicate is false). The share routes
    /// read only the share tables (shares.sql); this scope makes an
    /// accidental doc read there a NotFound instead of a leak.
    Public,
}

impl Scope {
    /// The user this scope acts for (None for System/Local).
    pub fn user(self) -> Option<Uuid> {
        match self {
            Scope::User(u) | Scope::Within { user: u, .. } => Some(u),
            Scope::System | Scope::Local | Scope::Public => None,
        }
    }

    /// Unfiltered: System or Local.
    pub fn sees_all(self) -> bool {
        matches!(self, Scope::System | Scope::Local)
    }

    /// `User(u)` for an owned thing, else Local — for background work that
    /// belongs to someone (a gardener, the memory sync) on a database that
    /// may never have had users (LOCAL mode).
    pub fn for_owner(owner: Option<Uuid>) -> Scope {
        owner.map_or(Scope::Local, Scope::User)
    }
}

/// A member's role in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Editor,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
    pub fn can_write(self) -> bool {
        self >= Role::Editor
    }
}

/// The daemon's one handle on the store. There is no way in without a
/// [`Scope`]: `lock(scope)` sets it for the guard's lifetime.
#[derive(Clone)]
pub struct SharedStore(Arc<Mutex<SqliteStore>>);

impl SharedStore {
    pub fn new(store: SqliteStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    /// Lock the store for `scope`. A poisoned lock is recovered (a panic
    /// mid-request must not take the daemon down with it).
    pub fn lock(&self, scope: Scope) -> ScopedStore<'_> {
        let mut g = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        g.set_scope(scope);
        ScopedStore(g)
    }

    pub fn is_poisoned(&self) -> bool {
        self.0.is_poisoned()
    }

    /// Tests only: poison the lock from a panicking thread.
    #[doc(hidden)]
    pub fn poison_for_test(&self) {
        let s = self.clone();
        let _ = std::thread::spawn(move || {
            let _g = s.0.lock().unwrap();
            panic!("poison");
        })
        .join();
    }
}

/// A locked store running for one scope.
pub struct ScopedStore<'a>(MutexGuard<'a, SqliteStore>);

impl std::ops::Deref for ScopedStore<'_> {
    type Target = SqliteStore;
    fn deref(&self) -> &SqliteStore {
        &self.0
    }
}

impl std::ops::DerefMut for ScopedStore<'_> {
    fn deref_mut(&mut self) -> &mut SqliteStore {
        &mut self.0
    }
}
