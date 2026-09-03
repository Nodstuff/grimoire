//! Localhost admin API: gardener registry CRUD + run-now. The `ksd` CLI is a
//! thin client over these routes so the daemon stays the only DB owner.

use crate::garden;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::response::IntoResponse;
use axum::{Json, Router};
use grimoire_store::{BlockStore, ConfidencePolicy, GardenerKind, ReviewPolicy, SqliteStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub type Store = Arc<Mutex<SqliteStore>>;

/// A gardener scope must be your OWN doc, never a mirror or anything under one:
/// a shared doc is tended by its owner's agents, and letting the grantee tend
/// it too means two agents editing both copies. Returns a refusal message when
/// the scope (or an ancestor) is a mirror.
fn scope_on_mirror(s: &SqliteStore, scope_doc: Option<Uuid>) -> Option<String> {
    let mut cur = scope_doc;
    while let Some(id) = cur {
        if s.get_mirror(id).ok().flatten().is_some() {
            return Some(
                "this doc is shared with you by its owner — it is tended on their side; \
                 tending it here would have two agents editing both copies"
                    .into(),
            );
        }
        cur = s.get_doc(id).ok().and_then(|d| d.parent_id);
    }
    None
}

/// Gardener/admin route state: the store plus the hot set (gardener runs
/// must honour the freeze on live docs, P2.3).
#[derive(Clone)]
pub struct AdminState {
    pub store: Store,
    pub hot: crate::hot::HotState,
}

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

async fn list_gardeners(State(AdminState { store, .. }): State<AdminState>) -> Json<Value> {
    let s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.list_gardeners() {
        Ok(g) => Json(json!(g)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn create_gardener(
    State(AdminState { store, .. }): State<AdminState>,
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
    if let Some(e) = scope_on_mirror(&s, req.scope_doc) {
        return Json(json!({"error": e}));
    }
    match s.create_gardener(&req.name, kind, &req.task_prompt, req.scope_doc, policy) {
        Ok(g) => Json(json!(g)),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn run_now(
    State(AdminState { store, hot }): State<AdminState>,
    Json(req): Json<RunReq>,
) -> Json<Value> {
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
        let out = garden::run_gardener(store.clone(), hot.clone(), g).await;
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

async fn list_runs(State(AdminState { store, .. }): State<AdminState>, Query(q): Query<RunsQuery>) -> Json<Value> {
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
    State(AdminState { store, .. }): State<AdminState>,
    Json(req): Json<UpdateGardener>,
) -> Json<Value> {
    let Some(policy) = ConfidencePolicy::parse(&req.confidence_policy) else {
        return Json(json!({"error": format!("bad confidence_policy: {}", req.confidence_policy)}));
    };
    let mut s = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(e) = scope_on_mirror(&s, req.scope_doc) {
        return Json(json!({"error": e}));
    }
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

async fn set_policy(State(AdminState { store, .. }): State<AdminState>, Json(req): Json<PolicyReq>) -> Json<Value> {
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
    if s.get_mirror(req.doc_id).ok().flatten().is_some() {
        return Json(json!({"error": "this doc is shared with you by its owner — review policy is the owner's call"}));
    }
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
    pub hot: crate::hot::HotState,
    /// Neighbours (mDNS presence) live here.
    pub runtime: crate::fed::Runtime,
}

/// The local trust boundary for `/admin/*` (shares, trust, gardeners,
/// policies — every gate-weakening surface). A per-boot random token: the
/// Tauri shell reads it from `<db_dir>/admin.token` (0600) and hands it to
/// the page; the CLI reads the same file. Any other local process — a
/// browser tab, a sandboxed app, another user — is refused. A process
/// running AS the user can read the file; that is the honest boundary.
#[derive(Clone)]
pub struct AdminToken(Arc<str>);

pub const ADMIN_TOKEN_FILE: &str = "admin.token";
pub const ADMIN_HEADER: &str = "x-grimoire-admin";

impl AdminToken {
    /// Mint a fresh token and write it beside the db (overwriting the last
    /// boot's), so a stale copy in an old tab never works.
    pub fn mint(db_dir: &std::path::Path) -> anyhow::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("OS entropy: {e}"))?;
        let token = hex::encode(bytes);
        crate::identity::write_secret_file(&db_dir.join(ADMIN_TOKEN_FILE), &token)?;
        Ok(Self(token.into()))
    }

    /// A fixed token (tests).
    #[cfg(test)]
    pub fn fixed(token: &str) -> Self {
        Self(token.into())
    }

    /// The token the CLI should send: read from the file the daemon wrote.
    pub fn read_from(db_dir: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(db_dir.join(ADMIN_TOKEN_FILE))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn matches(&self, presented: Option<&str>) -> bool {
        // constant-time compare: the token is a secret
        let Some(p) = presented else { return false };
        let a = self.0.as_bytes();
        let b = p.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

/// axum middleware: refuse `/admin/*` without the header. Same JSON error
/// shape as every other refusal, with a typed `code` the UI branches on.
async fn require_admin(
    State(token): State<AdminToken>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let presented = req
        .headers()
        .get(ADMIN_HEADER)
        .and_then(|v| v.to_str().ok());
    if token.matches(presented) {
        return next.run(req).await;
    }
    tracing::warn!(path = %req.uri().path(), "admin request without a valid token refused");
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": "this action needs the app's admin token — open Grimoire from the app, or add ?admin_token=<contents of ~/.grimoire/admin.token> to the URL",
            "code": "admin_token",
        })),
    )
        .into_response()
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

/// Owner side of the shares page: every share with what the UI needs to
/// render it in one row — title, size, who, grant, trust, state.
async fn list_shares(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let contacts = s.list_contacts().unwrap_or_default();
    match s.list_shares() {
        Ok(shares) => Json(json!(
            shares
                .into_iter()
                .map(|sh| {
                    let root = s.get_doc(sh.root_doc).ok();
                    let doc_count = s.docs_in_share(sh.id).map(|d| d.len()).unwrap_or(0);
                    let petname = sh
                        .contact
                        .and_then(|c| contacts.iter().find(|x| x.id == c))
                        .map(|c| c.petname.clone());
                    let mut v = json!(sh);
                    v["root_title"] = json!(root.as_ref().map(|d| d.title.clone()).unwrap_or_default());
                    v["doc_count"] = json!(doc_count);
                    v["contact_petname"] = json!(petname);
                    // invites v2: an unredeemed invite offered to a contact over the wire
                    if sh.state == grimoire_store::ShareState::Offered
                        && let Ok(Some(to)) = s.invite_offered_to(sh.id)
                        && let Some(c) = contacts.iter().find(|x| x.id == to)
                    {
                        v["offered_to_petname"] = json!(c.petname);
                    }
                    v
                })
                .collect::<Vec<_>>()
        )),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Permanently clear a REVOKED share (and its invites) — the shares page's
/// "clear". Active/offered shares must be revoked first (store enforces).
async fn delete_share(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.delete_share(req.id) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Grantee side of the shares page: one row per share we hold mirrors of,
/// with sync health — a failing pull is a red row saying WHY, never a doc
/// that silently has titles and no content.
async fn list_mirrors(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let contacts = s.list_contacts().unwrap_or_default();
    let mirrors = s.list_mirrors().unwrap_or_default();
    let mut by_share: std::collections::BTreeMap<Uuid, Vec<&grimoire_store::Mirror>> = Default::default();
    for m in &mirrors {
        by_share.entry(m.share_id).or_default().push(m);
    }
    let rows: Vec<Value> = by_share
        .into_iter()
        .map(|(share_id, ms)| {
            let ids: std::collections::HashSet<Uuid> = ms.iter().map(|m| m.doc_id).collect();
            // the root: the mirror whose parent is not a mirror of this share
            let root = ms
                .iter()
                .filter_map(|m| s.get_doc(m.doc_id).ok())
                .find(|d| d.parent_id.map(|p| !ids.contains(&p)).unwrap_or(true));
            let owner = contacts.iter().find(|c| c.id == ms[0].owner);
            json!({
                "share_id": share_id,
                "owner_petname": owner.map(|c| c.petname.clone()).unwrap_or_else(|| "?".into()),
                "owner_pubkey": owner.map(|c| c.pubkey.clone()).unwrap_or_default(),
                "permission": ms[0].permission,
                "root_doc_id": root.as_ref().map(|d| d.id),
                "root_title": root.as_ref().map(|d| d.title.clone()).unwrap_or_else(|| "(shared docs)".into()),
                "doc_count": ms.len(),
                "synced_epoch_max": ms.iter().map(|m| m.synced_epoch).max().unwrap_or(0),
                // docs whose owner epoch (from the last meta) is past what we landed
                "behind": ms.iter().filter(|m| m.owner_epoch > m.synced_epoch).count(),
                "last_pulled_at": ms.iter().filter_map(|m| m.last_pulled_at.clone()).max(),
                "last_error": ms.iter().find_map(|m| m.last_error.clone()),
                "owner_tended": ms.iter().any(|m| m.owner_tended),
            })
        })
        .collect();
    Json(json!(rows))
}

#[derive(Deserialize)]
pub struct ShareIdReq {
    pub share_id: Uuid,
}

/// Leave a share we were granted: drop every mirror of it locally (soft-
/// deleted docs, mirror rows removed). The owner's share is untouched — it is
/// theirs to revoke; a later re-join revives the docs.
async fn leave_share(State(st): State<FedState>, Json(req): Json<ShareIdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dropped = crate::fed::loops::drop_dead_share(&mut s, req.share_id);
    Json(json!({"ok": true, "dropped": dropped.len()}))
}

#[derive(Deserialize)]
pub struct ClearJoinsReq {
    pub id: Option<Uuid>,
}

/// Clear pending join attempts: one by id, or all of them.
async fn clear_joins(State(st): State<FedState>, Json(req): Json<ClearJoinsReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ids: Vec<Uuid> = match req.id {
        Some(id) => vec![id],
        None => s.list_pending_joins().unwrap_or_default().into_iter().map(|j| j.id).collect(),
    };
    let mut n = 0;
    for id in ids {
        if s.remove_pending_join(id).is_ok() {
            n += 1;
        }
    }
    Json(json!({"ok": true, "cleared": n}))
}

/// The instance owner's profile: display name (the petname contacts see),
/// identity, and whether the name was ever confirmed by the user.
async fn get_profile(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let human = s
        .list_principals()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.kind == grimoire_store::PrincipalKind::Human);
    let Some(human) = human else {
        return Json(json!({"error": "no human principal"}));
    };
    let confirmed = s.get_setting("profile.confirmed").ok().flatten().as_deref() == Some("1");
    Json(json!({
        "name": human.display_name,
        "principal_id": human.id,
        "node_id": st.ctx.node_id,
        "fingerprint": st.ctx.node_id.as_deref().map(crate::identity::fingerprint_of),
        "confirmed": confirmed,
    }))
}

#[derive(Deserialize)]
pub struct ProfileReq {
    pub name: String,
}

async fn set_profile(State(st): State<FedState>, Json(req): Json<ProfileReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let human = s
        .list_principals()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.kind == grimoire_store::PrincipalKind::Human);
    let Some(human) = human else {
        return Json(json!({"error": "no human principal"}));
    };
    match s.rename_principal(human.id, &req.name) {
        Ok(()) => {
            s.set_setting("profile.confirmed", "1").ok();
            Json(json!({"ok": true, "name": req.name.trim()}))
        }
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
        Ok(()) => {
            // a live bridge on this share is cut now, not at the next re-auth
            let cut = st.hot.drop_bridges_for_share(req.id);
            Json(json!({"ok": true, "bridges_cut": cut}))
        }
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
pub struct TrustReq {
    pub id: Uuid,
    /// "review" (park for review, default) or "yellow" (trusted: apply flagged)
    pub trust: String,
}

async fn set_share_trust(State(st): State<FedState>, Json(req): Json<TrustReq>) -> Json<Value> {
    let Some(trust) = grimoire_store::ShareTrust::parse(&req.trust) else {
        return Json(json!({"error": format!("bad trust: {}", req.trust)}));
    };
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.set_share_trust(req.id, trust) {
        Ok(()) => Json(json!({"ok": true})),
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
    let pubkey = s
        .list_contacts()
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.id == req.id)
        .map(|c| c.pubkey);
    match s.revoke_contact(req.id) {
        Ok(()) => {
            let cut = pubkey.map(|pk| st.hot.drop_bridges_for_peer(&pk)).unwrap_or(0);
            Json(json!({"ok": true, "bridges_cut": cut}))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Remove a contact without blocking: their shares are revoked, live bridges
/// cut, the contact row gone. A fresh invite pairs them again like anyone.
async fn remove_contact(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pubkey = s
        .list_contacts()
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.id == req.id)
        .map(|c| c.pubkey);
    match s.remove_contact(req.id) {
        Ok(()) => {
            let cut = pubkey.map(|pk| st.hot.drop_bridges_for_peer(&pk)).unwrap_or(0);
            Json(json!({"ok": true, "bridges_cut": cut}))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Re-enable a revoked contact (human surface only, never MCP). Shares stay
/// revoked; the owner re-shares deliberately.
async fn unrevoke_contact(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.unrevoke_contact(req.id) {
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
        Ok(Ok(outcome)) => {
            // fetch the tree NOW so the reply can say "45 docs", not "1 placeholder"
            let pulled = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                crate::fed::pull_after_join(endpoint, &st.store, &outcome.root_doc),
            )
            .await;
            return match pulled {
                Ok(Ok(sum)) => Json(json!({"joined": outcome, "docs": sum.changed})),
                Ok(Err(e)) => {
                    tracing::warn!(root = outcome.root_doc, "first pull after join failed: {e:#}");
                    Json(json!({"joined": outcome, "pull_error": format!("{e:#}")}))
                }
                Err(_) => Json(json!({"joined": outcome, "pull_error": "the first sync is taking a while; it continues in the background"})),
            };
        }
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

#[derive(Deserialize)]
pub struct CommentUpstreamReq {
    pub block_id: Uuid,
    pub text: String,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
}

async fn comment_upstream(
    State(st): State<FedState>,
    Json(req): Json<CommentUpstreamReq>,
) -> Json<Value> {
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "federation disabled: no instance identity"}));
    };
    match crate::fed::comment_upstream(endpoint, &st.store, req.block_id, &req.text, req.reply_to)
        .await
    {
        Ok(id) => {
            // bring the thread home immediately rather than waiting a pull tick
            crate::fed::pull_all_once(endpoint, &st.store).await;
            Json(json!({"comment": id}))
        }
        Err(e) => Json(json!({"error": format!("{e:#}")})),
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

#[derive(Deserialize)]
pub struct OfferShare {
    pub root_doc: Uuid,
    pub permission: Option<String>,
    pub contact_id: Uuid,
}

/// Invites v2, owner side: share a subtree WITH A CONTACT — no link. Mints
/// the invite, records whom it was offered to, and dials them with `Offer`.
/// If they cannot be reached the invite still exists: the reply carries the
/// link so the owner can send it by hand (`delivered: false`).
async fn offer_share(State(st): State<FedState>, Json(req): Json<OfferShare>) -> Json<Value> {
    let (Some(node_id), Some(endpoint)) = (&st.ctx.node_id, &st.ctx.endpoint) else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    let permission = match req.permission.as_deref() {
        None => grimoire_store::SharePermission::View,
        Some(p) => match grimoire_store::SharePermission::parse(p) {
            Some(p) => p,
            None => return Json(json!({"error": format!("bad permission: {p}")})),
        },
    };
    let (share, minted, contact, root_title) = {
        let mut s = st
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(contact) = s
            .list_contacts()
            .unwrap_or_default()
            .into_iter()
            .find(|c| c.id == req.contact_id && !c.revoked)
        else {
            return Json(json!({"error": "that contact is not available (removed or blocked)"}));
        };
        let root_title = s.get_doc(req.root_doc).map(|d| d.title).unwrap_or_default();
        match crate::fed::mint_invite_full(&mut s, node_id, req.root_doc, permission) {
            Ok((share, minted)) => {
                if let Err(e) = s.set_invite_offered_to(share.id, contact.id) {
                    return Json(json!({"error": e.to_string()}));
                }
                (share, minted, contact, root_title)
            }
            Err(e) => return Json(json!({"error": format!("{e:#}")})),
        }
    };
    let delivered = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        crate::fed::offer_share(endpoint, &contact.pubkey, share.id, &root_title, permission, &minted),
    )
    .await;
    match delivered {
        Ok(Ok(())) => Json(json!({"share": share, "delivered": true, "to": contact.petname})),
        Ok(Err(e)) => {
            tracing::warn!(to = contact.petname, "share offer not delivered: {e:#}");
            Json(json!({"share": share, "delivered": false, "to": contact.petname, "link": minted.link, "reason": format!("{e:#}")}))
        }
        Err(_) => Json(json!({"share": share, "delivered": false, "to": contact.petname, "link": minted.link, "reason": "they are offline or unreachable right now"})),
    }
}

/// Recipient side: open share offers, with who they are from.
async fn list_offers(State(st): State<FedState>) -> Json<Value> {
    let s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let contacts = s.list_contacts().unwrap_or_default();
    match s.list_share_offers(true) {
        Ok(offers) => Json(json!(offers
            .into_iter()
            .map(|o| {
                let c = contacts.iter().find(|c| c.id == o.from_contact);
                let mut v = json!(o);
                v["from_petname"] = json!(c.map(|c| c.petname.clone()).unwrap_or_else(|| "someone".into()));
                v["from_pubkey"] = json!(c.map(|c| c.pubkey.clone()).unwrap_or_default());
                // the secret never leaves the daemon
                v.as_object_mut().map(|m| m.remove("secret"));
                v
            })
            .collect::<Vec<_>>())),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Accept an offer: redeem it exactly like a pasted link, then pull the tree.
async fn accept_offer(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    let offer = {
        let s = st
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match s.get_share_offer(req.id) {
            Ok(o) => o,
            Err(e) => return Json(json!({"error": e.to_string()})),
        }
    };
    if offer.state != grimoire_store::ShareOfferState::Open {
        return Json(json!({"error": format!("this request is already {}", offer.state.as_str())}));
    }
    let ticket = crate::fed::ticket_for_offer(&offer);
    let attempt = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        crate::fed::join_once(endpoint, &st.store, &ticket),
    )
    .await;
    match attempt {
        Ok(Ok(outcome)) => {
            {
                let mut s = st
                    .store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                s.set_share_offer_state(offer.id, grimoire_store::ShareOfferState::Accepted).ok();
            }
            let pulled = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                crate::fed::pull_after_join(endpoint, &st.store, &outcome.root_doc),
            )
            .await;
            match pulled {
                Ok(Ok(sum)) => Json(json!({"joined": outcome, "docs": sum.changed})),
                Ok(Err(e)) => Json(json!({"joined": outcome, "pull_error": format!("{e:#}")})),
                Err(_) => Json(json!({"joined": outcome, "pull_error": "the first sync is taking a while; it continues in the background"})),
            }
        }
        Ok(Err(e)) => {
            // a dead invite (expired/burned) closes the offer; unreachable keeps it open
            let msg = format!("{e:#}");
            if crate::fed::loops::join_failure_is_dead(&e) {
                let mut s = st
                    .store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                s.set_share_offer_state(offer.id, grimoire_store::ShareOfferState::Expired).ok();
            }
            Json(json!({"error": msg}))
        }
        Err(_) => Json(json!({"error": "owner unreachable (timed out) — try again when they are online"})),
    }
}

async fn decline_offer(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.set_share_offer_state(req.id, grimoire_store::ShareOfferState::Declined) {
        Ok(()) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn clear_offers(State(st): State<FedState>) -> Json<Value> {
    let mut s = st
        .store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match s.clear_share_offers() {
        Ok(n) => Json(json!({"ok": true, "cleared": n})),
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

/// Grimoires visible on this LAN right now (mDNS). Presence only — each is
/// flagged if it is already a contact so the UI can offer the right action.
async fn list_neighbours(State(st): State<FedState>) -> Json<Value> {
    let me = st.ctx.node_id.clone().unwrap_or_default();
    let contacts = {
        let s = st
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.list_contacts().unwrap_or_default()
    };
    let rows: Vec<Value> = st
        .runtime
        .neighbours(&me)
        .into_iter()
        .map(|n| {
            let c = contacts.iter().find(|c| c.pubkey == n.pubkey);
            json!({
                "pubkey": n.pubkey,
                "name": n.name,
                "seen_secs_ago": n.seen_secs_ago,
                "contact_id": c.map(|c| c.id),
                "contact_petname": c.map(|c| c.petname.clone()),
                "blocked": c.map(|c| c.revoked).unwrap_or(false),
            })
        })
        .collect();
    Json(json!(rows))
}

pub fn router(
    store: Store,
    fed: FedCtx,
    hot: crate::hot::HotState,
    runtime: crate::fed::Runtime,
    token: AdminToken,
) -> Router {
    let fed_state = FedState {
        store: store.clone(),
        ctx: fed,
        hot: hot.clone(),
        runtime,
    };
    // the profile is not gate-weakening (your own name + public identity):
    // it stays open so the first-run prompt works from any local client
    let open_routes = Router::new()
        .route("/api/profile", get(get_profile).post(set_profile))
        .with_state(fed_state.clone());
    let fed_routes = Router::new()
        .route("/admin/shares", get(list_shares).post(create_share))
        .route("/admin/shares/offer", post(offer_share))
        .route("/admin/offers", get(list_offers))
        .route("/admin/offers/accept", post(accept_offer))
        .route("/admin/offers/decline", post(decline_offer))
        .route("/admin/offers/clear", post(clear_offers))
        .route("/admin/neighbours", get(list_neighbours))
        .route("/admin/shares/revoke", post(revoke_share))
        .route("/admin/shares/delete", post(delete_share))
        .route("/admin/mirrors", get(list_mirrors))
        .route("/admin/mirrors/leave", post(leave_share))
        .route("/admin/joins/clear", post(clear_joins))
        .route("/admin/shares/trust", post(set_share_trust))
        .route("/admin/contacts", get(list_contacts))
        .route("/admin/contacts/revoke", post(revoke_contact))
        .route("/admin/contacts/unrevoke", post(unrevoke_contact))
        .route("/admin/contacts/remove", post(remove_contact))
        .route("/admin/contacts/verify", post(verify_contact))
        .route("/admin/contacts/rename", post(rename_contact))
        .route("/admin/join", post(join))
        .route("/admin/joins", get(list_joins))
        .route("/admin/pull", post(pull_now))
        .route("/admin/propose_upstream", post(propose_upstream))
        .route("/admin/comment_upstream", post(comment_upstream))
        .route("/admin/proposals", get(list_proposals))
        .route_layer(axum::middleware::from_fn_with_state(token.clone(), require_admin))
        .with_state(fed_state);
    Router::new()
        .route(
            "/admin/gardeners",
            get(list_gardeners).post(create_gardener),
        )
        .route("/admin/garden", post(run_now))
        .route("/admin/gardeners/update", post(update_gardener))
        .route("/admin/runs", get(list_runs))
        .route("/admin/policy", post(set_policy))
        .route_layer(axum::middleware::from_fn_with_state(token, require_admin))
        .with_state(AdminState { store, hot })
        .merge(fed_routes)
        .merge(open_routes)
}

#[cfg(test)]
mod token_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> Router {
        let store: Store = Arc::new(Mutex::new(SqliteStore::open_in_memory().unwrap()));
        let hot = crate::hot::HotState::new(std::env::temp_dir().join(format!("grimoire-admin-test-{}", Uuid::now_v7())));
        router(
            store,
            FedCtx {
                node_id: None,
                endpoint: None,
            },
            hot,
            crate::fed::Runtime::default(),
            AdminToken::fixed("s3cret"),
        )
    }

    #[tokio::test]
    async fn admin_routes_need_the_token_and_profile_does_not() {
        let app = app();
        // no header → 401 with the typed code
        let res = app
            .clone()
            .oneshot(Request::get("/admin/shares").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "admin_token");
        // wrong token → 401
        let res = app
            .clone()
            .oneshot(
                Request::get("/admin/gardeners")
                    .header(ADMIN_HEADER, "nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // right token → the handler runs
        let res = app
            .clone()
            .oneshot(
                Request::get("/admin/gardeners")
                    .header(ADMIN_HEADER, "s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // the profile is open: no token needed (first-run name prompt)
        let res = app
            .oneshot(Request::get("/api/profile").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn token_compare_is_exact() {
        let t = AdminToken::fixed("abc");
        assert!(t.matches(Some("abc")));
        assert!(!t.matches(Some("abd")));
        assert!(!t.matches(Some("ab")));
        assert!(!t.matches(Some("abcd")));
        assert!(!t.matches(None));
    }

    #[test]
    fn mint_writes_a_0600_file_and_read_from_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let t = AdminToken::mint(dir.path()).unwrap();
        let on_disk = AdminToken::read_from(dir.path()).unwrap();
        assert!(t.matches(Some(&on_disk)));
        assert_eq!(on_disk.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(ADMIN_TOKEN_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // a second boot replaces it
        let t2 = AdminToken::mint(dir.path()).unwrap();
        assert!(!t2.matches(Some(&on_disk)));
    }
}

/// The 16:00 daily cut (§3.4): the daemon self-schedules; no external cron.
pub async fn daily_loop(store: Store, hot: crate::hot::HotState) {
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
            let out = garden::run_gardener(store.clone(), hot.clone(), g).await;
            tracing::info!("gardener {name}: {} — {}", out.status, out.summary);
        }
    }
}
