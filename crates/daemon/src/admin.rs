//! Localhost admin API: gardener registry CRUD + run-now. The `ksd` CLI is a
//! thin client over these routes so the daemon stays the only DB owner.

use crate::garden;
use crate::store_ext::with_store;
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
    with_store(&store, move |s| {
        match s.list_gardeners() {
            Ok(g) => Json(json!(g)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    with_store(&store, move |s| {
        if let Some(e) = scope_on_mirror(&s, req.scope_doc) {
            return Json(json!({"error": e}));
        }
        match s.create_gardener(&req.name, kind, &req.task_prompt, req.scope_doc, policy) {
            Ok(g) => Json(json!(g)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

/// Run-now. 409 when every matched gardener is already mid-run (a second
/// click, or the daily cut got there first); a mixed batch reports the
/// running ones with status "running" and runs the rest.
async fn run_now(
    State(AdminState { store, hot }): State<AdminState>,
    Json(req): Json<RunReq>,
) -> axum::response::Response {
    let gardeners = match with_store(&store, |s| s.list_gardeners()).await {
        Ok(g) => g,
        Err(e) => return Json(json!({"error": e.to_string()})).into_response(),
    };
    let mut outcomes = Vec::new();
    let mut started = 0usize;
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
        if out.status != garden::STATUS_RUNNING {
            started += 1;
        }
        outcomes.push(json!({
            "gardener": name,
            "run_id": out.run_id,
            "status": out.status,
            "summary": out.summary,
        }));
    }
    if outcomes.is_empty() {
        return Json(json!({"error": "no matching enabled gardener"})).into_response();
    }
    if started == 0 {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(json!({"error": "already running", "code": "gardener_running", "outcomes": outcomes})),
        )
            .into_response();
    }
    Json(json!(outcomes)).into_response()
}

async fn list_runs(State(AdminState { store, .. }): State<AdminState>, Query(q): Query<RunsQuery>) -> Json<Value> {
    with_store(&store, move |s| {
        match s.list_runs(q.limit.unwrap_or(20)) {
            Ok(r) => Json(json!(r)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    with_store(&store, move |s| {
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
    })
    .await
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
    with_store(&store, move |s| {
        if s.get_mirror(req.doc_id).ok().flatten().is_some() {
            return Json(json!({"error": "this doc is shared with you by its owner — review policy is the owner's call"}));
        }
        match s.set_review_policy(req.doc_id, policy) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    let Some(node_id) = st.ctx.node_id.clone() else {
        return Json(json!({"error": "federation disabled: no instance identity"}));
    };
    let permission = match req.permission.as_deref() {
        None => grimoire_store::SharePermission::View,
        Some(p) => match grimoire_store::SharePermission::parse(p) {
            Some(p) => p,
            None => return Json(json!({"error": format!("bad permission: {p}")})),
        },
    };
    with_store(&st.store, move |s| {
        match crate::fed::mint_invite(s, &node_id, req.root_doc, permission) {
            Ok((share, link)) => Json(json!({"share": share, "link": link})),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        }
    })
    .await
}

/// Owner side of the shares page: every share with what the UI needs to
/// render it in one row — title, size, who, grant, trust, state.
async fn list_shares(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
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
    })
    .await
}

/// Permanently clear a REVOKED share (and its invites) — the shares page's
/// "clear". Active/offered shares must be revoked first (store enforces).
async fn delete_share(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
        match s.delete_share(req.id) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

/// Grantee side of the shares page: one row per share we hold mirrors of,
/// with sync health — a failing pull is a red row saying WHY, never a doc
/// that silently has titles and no content.
async fn list_mirrors(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
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
                    // hub relay (slice 1): the share comes from a hub; how many of its
                    // docs are relayed for other members (each names its true owner)
                    "from_hub": owner.map(|c| c.is_hub).unwrap_or(false),
                    "relayed_docs": ms.iter().filter(|m| m.origin_owner.is_some()).count(),
                })
            })
            .collect();
        Json(json!(rows))
    })
    .await
}

#[derive(Deserialize)]
pub struct ShareIdReq {
    pub share_id: Uuid,
}

/// Leave a share we were granted: drop every mirror of it locally (soft-
/// deleted docs, mirror rows removed). The owner's share is untouched — it is
/// theirs to revoke; a later re-join revives the docs.
async fn leave_share(State(st): State<FedState>, Json(req): Json<ShareIdReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
        let dropped = crate::fed::loops::drop_dead_share(s, req.share_id);
        Json(json!({"ok": true, "dropped": dropped.len()}))
    })
    .await
}

#[derive(Deserialize)]
pub struct ClearJoinsReq {
    pub id: Option<Uuid>,
}

/// Clear pending join attempts: one by id, or all of them.
async fn clear_joins(State(st): State<FedState>, Json(req): Json<ClearJoinsReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
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
    })
    .await
}

/// The instance owner's profile: display name (the petname contacts see),
/// identity, and whether the name was ever confirmed by the user.
async fn get_profile(State(st): State<FedState>) -> Json<Value> {
    let store = st.store.clone();
    let st = st.clone();
    with_store(&store, move |s| {
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
    })
    .await
}

#[derive(Deserialize)]
pub struct ProfileReq {
    pub name: String,
}

async fn set_profile(State(st): State<FedState>, Json(req): Json<ProfileReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
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
    })
    .await
}

#[derive(Deserialize)]
pub struct IdReq {
    pub id: Uuid,
}

async fn revoke_share(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let (target, revoked) = with_store(&st.store, move |s| {
        // who held it, so we can tell them now rather than at their next sweep
        let target = s.get_share(req.id).ok().and_then(|sh| {
            let contact = s.list_contacts().ok()?.into_iter().find(|c| Some(c.id) == sh.contact)?;
            let title = s.get_doc(sh.root_doc).map(|d| d.title).unwrap_or_default();
            Some((contact, sh.root_doc, title))
        });
        (target, s.set_share_state(req.id, grimoire_store::ShareState::Revoked))
    })
    .await;
    match revoked {
        Ok(()) => {
            // a live bridge on this share is cut now, not at the next re-auth
            let cut = st.hot.drop_bridges_for_share(req.id);
            // nudge the grantee: their next pull is refused ShareRevoked and
            // drops the mirrors at once (a hub un-relays the publication) —
            // without this they keep stale docs until the 120s sweep
            if let (Some((contact, root, title)), Some(ep)) = (target, st.ctx.endpoint.clone())
                && let Ok(peer) = contact.pubkey.parse::<iroh::EndpointId>()
            {
                let item = crate::fed::NotifyItem {
                    doc: root.to_string(),
                    title,
                    kind: crate::fed::NotifyKind::DocChanged,
                };
                tokio::spawn(crate::fed::send_nudges(ep, peer, req.id, vec![item], contact.petname));
            }
            Json(json!({"ok": true, "bridges_cut": cut}))
        }
        Err(e) => Json(json!({"error": e.to_string()})),
    }
}

async fn list_contacts(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
        match s.list_contacts() {
            Ok(c) => Json(json!(c)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    with_store(&st.store, move |s| {
        match s.set_share_trust(req.id, trust) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

#[derive(Deserialize)]
pub struct VerifyReq {
    pub id: Uuid,
    pub verified: bool,
}

async fn verify_contact(State(st): State<FedState>, Json(req): Json<VerifyReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
        match s.set_contact_verified(req.id, req.verified) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    with_store(&st.store, move |s| {
        match s.rename_contact(req.id, req.petname.trim()) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

async fn revoke_contact(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let store = st.store.clone();
    let st = st.clone();
    with_store(&store, move |s| {
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
    })
    .await
}

/// Remove a contact without blocking: their shares are revoked, live bridges
/// cut, the contact row gone. A fresh invite pairs them again like anyone.
async fn remove_contact(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let store = st.store.clone();
    let st = st.clone();
    with_store(&store, move |s| {
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
    })
    .await
}

/// Re-enable a revoked contact (human surface only, never MCP). Shares stay
/// revoked; the owner re-shares deliberately.
async fn unrevoke_contact(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
        match s.unrevoke_contact(req.id) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
        // hub: paired but waiting for an admin — nothing to pull yet
        Ok(Ok(outcome)) if outcome.membership.as_deref() == Some("pending") => {
            return Json(json!({"joined": outcome, "pending": true}));
        }
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
    with_store(&st.store, move |s| {
        match s.queue_join(&req.link) {
            Ok(id) => {
                s.record_join_attempt(id, &err).ok();
                Json(json!({"queued": true, "pending_join": id, "last_error": err}))
            }
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    with_store(&st.store, move |s| {
        match s.list_outbound_proposals(false) {
            Ok(p) => Json(json!(p)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    with_store(&st.store, move |s| {
        match s.list_pending_joins() {
            Ok(j) => Json(json!(j)),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
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
    let (Some(node_id), Some(endpoint)) = (st.ctx.node_id.clone(), &st.ctx.endpoint) else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    let permission = match req.permission.as_deref() {
        None => grimoire_store::SharePermission::View,
        Some(p) => match grimoire_store::SharePermission::parse(p) {
            Some(p) => p,
            None => return Json(json!({"error": format!("bad permission: {p}")})),
        },
    };
    let minted = {
        let (contact_id, root_doc) = (req.contact_id, req.root_doc);
        with_store(&st.store, move |s| {
            let Some(contact) = s
                .list_contacts()
                .unwrap_or_default()
                .into_iter()
                .find(|c| c.id == contact_id && !c.revoked)
            else {
                return Err(Json(json!({"error": "that contact is not available (removed or blocked)"})));
            };
            let root_title = s.get_doc(root_doc).map(|d| d.title).unwrap_or_default();
            match crate::fed::mint_invite_full(s, &node_id, root_doc, permission) {
                Ok((share, minted)) => {
                    if let Err(e) = s.set_invite_offered_to(share.id, contact.id) {
                        return Err(Json(json!({"error": e.to_string()})));
                    }
                    Ok((share, minted, contact, root_title))
                }
                Err(e) => Err(Json(json!({"error": format!("{e:#}")}))),
            }
        })
        .await
    };
    let (share, minted, contact, root_title) = match minted {
        Ok(v) => v,
        Err(r) => return r,
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
    with_store(&st.store, move |s| {
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
    })
    .await
}

/// Accept an offer: redeem it exactly like a pasted link, then pull the tree.
async fn accept_offer(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    let offer = match with_store(&st.store, move |s| s.get_share_offer(req.id)).await {
        Ok(o) => o,
        Err(e) => return Json(json!({"error": e.to_string()})),
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
                with_store(&st.store, move |s| {
                    s.set_share_offer_state(offer.id, grimoire_store::ShareOfferState::Accepted).ok();
                })
                .await
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
                with_store(&st.store, move |s| {
                    s.set_share_offer_state(offer.id, grimoire_store::ShareOfferState::Expired).ok();
                })
                .await
            }
            Json(json!({"error": msg}))
        }
        Err(_) => Json(json!({"error": "owner unreachable (timed out) — try again when they are online"})),
    }
}

async fn decline_offer(State(st): State<FedState>, Json(req): Json<IdReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
        match s.set_share_offer_state(req.id, grimoire_store::ShareOfferState::Declined) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

async fn clear_offers(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
        match s.clear_share_offers() {
            Ok(n) => Json(json!({"ok": true, "cleared": n})),
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

/// Grimoires visible on this LAN right now (mDNS). Presence only — each is
/// flagged if it is already a contact so the UI can offer the right action.
async fn list_neighbours(State(st): State<FedState>) -> Json<Value> {
    let me = st.ctx.node_id.clone().unwrap_or_default();
    let contacts = {
        with_store(&st.store, move |s| {
            s.list_contacts().unwrap_or_default()
        })
        .await
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

// --- hubs (slice 1) ---------------------------------------------------------

/// A hub contact as the Shares page lists it, from LOCAL rows only (no dial):
/// my standing there (as last told by the hub), the hub folder if I hold it,
/// and the subtrees I have published (my `propose` shares to the hub).
async fn list_hubs(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
        let contacts = s.list_contacts().unwrap_or_default();
        let shares = s.list_shares().unwrap_or_default();
        let mirrors = s.list_mirrors().unwrap_or_default();
        let transfers = s.list_doc_transfers().unwrap_or_default();
        let rows: Vec<Value> = contacts
            .iter()
            .filter(|c| c.is_hub && !c.revoked)
            .map(|c| {
                let ids: std::collections::HashSet<Uuid> =
                    mirrors.iter().filter(|m| m.owner == c.id).map(|m| m.doc_id).collect();
                let root = mirrors
                    .iter()
                    .filter(|m| m.owner == c.id)
                    .filter_map(|m| s.get_doc(m.doc_id).ok())
                    .find(|d| d.parent_id.map(|p| !ids.contains(&p)).unwrap_or(true));
                // slice 2: folders I handed over (done) or offered (waiting for an admin)
                let mine_out: Vec<&grimoire_store::DocTransfer> = transfers
                    .iter()
                    .filter(|t| t.counterparty == c.id && t.direction == grimoire_store::TransferDirection::Out)
                    .collect();
                let transferred: std::collections::HashSet<Uuid> =
                    mine_out.iter().filter(|t| t.state == "done").map(|t| t.root_doc).collect();
                let transfers_json: Vec<Value> = mine_out
                    .iter()
                    .map(|t| {
                        json!({
                            "id": t.id,
                            "root_doc": t.root_doc,
                            "root_title": s.get_doc(t.root_doc).map(|d| d.title).unwrap_or_default(),
                            "state": t.state,
                            "at": t.at,
                        })
                    })
                    .collect();
                let publications: Vec<Value> = shares
                    .iter()
                    .filter(|sh| sh.contact == Some(c.id) && sh.state != grimoire_store::ShareState::Revoked)
                    // a transferred folder's share is the pipe the hub pulled through, not a publication
                    .filter(|sh| !transferred.contains(&sh.root_doc))
                    .map(|sh| {
                        json!({
                            "share_id": sh.id,
                            "root_doc": sh.root_doc,
                            "root_title": s.get_doc(sh.root_doc).map(|d| d.title).unwrap_or_default(),
                            "doc_count": s.docs_in_share(sh.id).map(|d| d.len()).unwrap_or(0),
                            "state": sh.state,
                        })
                    })
                    .collect();
                json!({
                    "contact_id": c.id,
                    "name": c.petname,
                    "pubkey": c.pubkey,
                    "role": c.role,
                    "membership": c.membership,
                    "root_doc_id": root.as_ref().map(|d| d.id),
                    "relayed_docs": mirrors.iter().filter(|m| m.owner == c.id && m.origin_owner.is_some()).count(),
                    "publications": publications,
                    "transfers": transfers_json,
                })
            })
            .collect();
        Json(json!(rows))
    })
    .await
}

/// Dial a hub I belong to with one request (15s cap).
async fn dial_hub(st: &FedState, hub: Uuid, req: crate::fed::wire::Request) -> Result<crate::fed::wire::Response, String> {
    let endpoint = st
        .ctx
        .endpoint
        .as_ref()
        .ok_or_else(|| "sharing is off: this Grimoire has no identity yet".to_string())?;
    let contact = with_store(&st.store, move |s| {
        s.list_contacts()
            .unwrap_or_default()
            .into_iter()
            .find(|c| c.id == hub && c.is_hub && !c.revoked)
            .ok_or_else(|| "that hub is not one of your contacts".to_string())
    })
    .await?;
    let id: iroh::EndpointId = contact.pubkey.parse().map_err(|_| "hub pubkey malformed".to_string())?;
    match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        crate::fed::client::request(endpoint, iroh::EndpointAddr::from(id), req),
    )
    .await
    {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(format!("{} is unreachable: {e:#}", contact.petname)),
        Err(_) => Err(format!("{} is offline or unreachable right now", contact.petname)),
    }
}

#[derive(Deserialize)]
pub struct HubQuery {
    pub hub: Uuid,
}

/// Members of a hub I belong to. First asks where I stand (and records it
/// locally, so a role change reaches the UI), then lists members if I am an
/// admin — `members: null` otherwise. Fetched when the UI section opens;
/// never polled.
async fn hub_members(State(st): State<FedState>, Query(q): Query<HubQuery>) -> Json<Value> {
    use crate::fed::wire::{HubAction, Request, Response};
    let status = match dial_hub(&st, q.hub, Request::HubStatus).await {
        Ok(Response::HubStatusIs { name, role, membership, members, pending }) => {
            with_store(&st.store, move |s| {
                if let Some(r) = grimoire_store::ContactRole::parse(&role) {
                    s.set_contact_role(q.hub, r).ok();
                }
                if let Some(m) = grimoire_store::Membership::parse(&membership) {
                    s.set_contact_membership(q.hub, m).ok();
                }
                json!({"name": name, "role": role, "membership": membership, "members": members, "pending": pending})
            })
            .await
        }
        Ok(Response::Refused { reason, .. }) => return Json(json!({"error": reason})),
        Ok(other) => return Json(json!({"error": format!("unexpected reply: {other:?}")})),
        Err(e) => return Json(json!({"error": e})),
    };
    if status["role"] != "admin" {
        return Json(json!({"status": status, "members": Value::Null}));
    }
    match dial_hub(&st, q.hub, Request::HubAdmin { action: HubAction::ListMembers }).await {
        Ok(Response::HubMembers { members }) => Json(json!({"status": status, "members": members})),
        Ok(Response::Refused { reason, .. }) => Json(json!({"status": status, "members": Value::Null, "error": reason})),
        Ok(other) => Json(json!({"error": format!("unexpected reply: {other:?}")})),
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(Deserialize)]
pub struct HubMemberReq {
    pub hub: Uuid,
    pub contact_id: Uuid,
    pub role: Option<String>,
}

async fn hub_action(st: &FedState, hub: Uuid, action: crate::fed::wire::HubAction) -> Json<Value> {
    use crate::fed::wire::{Request, Response};
    match dial_hub(st, hub, Request::HubAdmin { action }).await {
        Ok(Response::Noted) => Json(json!({"ok": true})),
        Ok(Response::HubInvite { link }) => Json(json!({"ok": true, "link": link})),
        Ok(Response::Refused { reason, .. }) => Json(json!({"error": reason})),
        Ok(other) => Json(json!({"error": format!("unexpected reply: {other:?}")})),
        Err(e) => Json(json!({"error": e})),
    }
}

async fn hub_approve(State(st): State<FedState>, Json(req): Json<HubMemberReq>) -> Json<Value> {
    hub_action(&st, req.hub, crate::fed::wire::HubAction::Approve { contact_id: req.contact_id.to_string() }).await
}

async fn hub_eject(State(st): State<FedState>, Json(req): Json<HubMemberReq>) -> Json<Value> {
    hub_action(&st, req.hub, crate::fed::wire::HubAction::Eject { contact_id: req.contact_id.to_string() }).await
}

async fn hub_role(State(st): State<FedState>, Json(req): Json<HubMemberReq>) -> Json<Value> {
    let Some(role) = req.role else {
        return Json(json!({"error": "role is required: member or admin"}));
    };
    hub_action(&st, req.hub, crate::fed::wire::HubAction::SetRole { contact_id: req.contact_id.to_string(), role }).await
}

#[derive(Deserialize)]
pub struct HubReq {
    pub hub: Uuid,
}

/// Ask a hub I administer for a fresh invite link (to onboard someone).
async fn hub_invite(State(st): State<FedState>, Json(req): Json<HubReq>) -> Json<Value> {
    hub_action(&st, req.hub, crate::fed::wire::HubAction::Invite).await
}

// --- hubs (slice 2): hub-owned queue, transfers ------------------------------

/// Open proposals on docs the hub itself owns (admins only; fetched when the
/// section opens, never polled). Same item shape as /api/queue.
async fn hub_queue(State(st): State<FedState>, Query(q): Query<HubQuery>) -> Json<Value> {
    use crate::fed::wire::{HubAction, Request, Response};
    match dial_hub(&st, q.hub, Request::HubAdmin { action: HubAction::ReviewQueue }).await {
        Ok(Response::HubQueue { items }) => Json(json!({"items": items})),
        Ok(Response::Refused { reason, .. }) => Json(json!({"error": reason})),
        Ok(other) => Json(json!({"error": format!("unexpected reply: {other:?}")})),
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(Deserialize)]
pub struct HubResolveReq {
    pub hub: Uuid,
    pub annotation_id: Uuid,
    pub decision: String,
}

async fn hub_resolve(State(st): State<FedState>, Json(req): Json<HubResolveReq>) -> Json<Value> {
    hub_action(
        &st,
        req.hub,
        crate::fed::wire::HubAction::Resolve {
            annotation_id: req.annotation_id.to_string(),
            decision: req.decision,
        },
    )
    .await
}

/// Transfer offers at a hub I administer, every state.
async fn hub_transfers(State(st): State<FedState>, Query(q): Query<HubQuery>) -> Json<Value> {
    use crate::fed::wire::{HubAction, Request, Response};
    match dial_hub(&st, q.hub, Request::HubAdmin { action: HubAction::ListTransfers }).await {
        Ok(Response::HubTransfers { transfers }) => Json(json!({"transfers": transfers})),
        Ok(Response::Refused { reason, .. }) => Json(json!({"error": reason})),
        Ok(other) => Json(json!({"error": format!("unexpected reply: {other:?}")})),
        Err(e) => Json(json!({"error": e})),
    }
}

#[derive(Deserialize)]
pub struct HubTransferReq {
    pub hub: Uuid,
    pub id: Uuid,
}

async fn hub_transfer_accept(State(st): State<FedState>, Json(req): Json<HubTransferReq>) -> Json<Value> {
    hub_action(&st, req.hub, crate::fed::wire::HubAction::AcceptTransfer { id: req.id.to_string() }).await
}

async fn hub_transfer_decline(State(st): State<FedState>, Json(req): Json<HubTransferReq>) -> Json<Value> {
    hub_action(&st, req.hub, crate::fed::wire::HubAction::DeclineTransfer { id: req.id.to_string() }).await
}

#[derive(Deserialize)]
pub struct TransferOfferReq {
    pub hub: Uuid,
    pub root_doc: Uuid,
}

/// Offer a subtree I own to a hub as a TRANSFER: the hub will own it, my copy
/// becomes a read-only mirror once an admin accepts. Nothing changes here
/// until then — this only records the offer on both sides.
async fn offer_transfer(State(st): State<FedState>, Json(req): Json<TransferOfferReq>) -> Json<Value> {
    use crate::fed::wire::{Request, Response};
    let checked = {
        let (root_doc, hub) = (req.root_doc, req.hub);
        with_store(&st.store, move |s| {
            let Ok(doc) = s.get_doc(root_doc) else {
                return Err(Json(json!({"error": "no such doc"})));
            };
            if s.doc_is_tombstoned(root_doc).unwrap_or(true) {
                return Err(Json(json!({"error": "that folder is in the trash"})));
            }
            let subtree = s.doc_subtree_ids(root_doc).unwrap_or_default();
            for id in &subtree {
                if s.get_mirror(*id).ok().flatten().is_some() {
                    let t = s.get_doc(*id).map(|d| d.title).unwrap_or_default();
                    return Err(Json(json!({"error": format!("“{t}” was shared to you — only its owner can transfer it")})));
                }
            }
            if !s
                .list_contacts()
                .unwrap_or_default()
                .iter()
                .any(|c| c.id == hub && c.is_hub && !c.revoked && c.membership == grimoire_store::Membership::Active)
            {
                return Err(Json(json!({"error": "you are not an active member of that hub"})));
            }
            Ok((doc.title, subtree.len()))
        })
        .await
    };
    let (title, doc_count) = match checked {
        Ok(v) => v,
        Err(r) => return r,
    };
    match dial_hub(
        &st,
        req.hub,
        Request::TransferOffer {
            root_doc: req.root_doc.to_string(),
            title: title.clone(),
            doc_count,
        },
    )
    .await
    {
        Ok(Response::TransferOffered { id }) => {
            let (root_doc, hub) = (req.root_doc, req.hub);
            if let Err(e) = with_store(&st.store, move |s| {
                s.add_doc_transfer(root_doc, hub, grimoire_store::TransferDirection::Out, "offered")
            })
            .await
            {
                return Json(json!({"error": e.to_string()}));
            }
            Json(json!({"ok": true, "id": id, "title": title, "doc_count": doc_count}))
        }
        Ok(Response::Refused { reason, .. }) => Json(json!({"error": reason})),
        Ok(other) => Json(json!({"error": format!("unexpected reply: {other:?}")})),
        Err(e) => Json(json!({"error": e})),
    }
}

// --- the hub box itself (slice 2): queue + transfers for the CLI ----------------

async fn local_hub_transfers(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
        if crate::fed::hub::config(&s).is_none() {
            return Json(json!({"error": "this Grimoire is not a hub (start it with --hub)"}));
        }
        Json(json!(crate::fed::hub::transfers(&s)))
    })
    .await
}

#[derive(Deserialize)]
pub struct LocalTransferReq {
    pub id: Uuid,
}

async fn local_hub_transfer_accept(State(st): State<FedState>, Json(req): Json<LocalTransferReq>) -> Json<Value> {
    let Some(endpoint) = &st.ctx.endpoint else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    {
        let id = req.id;
        let early = with_store(&st.store, move |s| -> Result<(), Json<Value>> {
            if crate::fed::hub::config(s).is_none() {
                return Err(Json(json!({"error": "this Grimoire is not a hub (start it with --hub)"})));
            }
            match s.get_hub_transfer(id) {
                Ok(t) if t.state == grimoire_store::HubTransferState::Done => return Err(Json(json!({"ok": true, "state": "done"}))),
                Ok(t) if t.state == grimoire_store::HubTransferState::Declined => return Err(Json(json!({"error": "that transfer was declined"}))),
                Ok(_) => {}
                Err(e) => return Err(Json(json!({"error": e.to_string()}))),
            }
            if let Err(e) = s.set_hub_transfer_state(id, grimoire_store::HubTransferState::Accepted) {
                return Err(Json(json!({"error": e.to_string()})));
            }
            Ok(())
        })
        .await;
        if let Err(r) = early {
            return r;
        }
    }
    // synchronous here (the CLI waits): dial the member, pull, take over
    match crate::fed::transfer::hub_complete(endpoint, &st.store, req.id, None).await {
        Ok(()) => Json(json!({"ok": true, "state": "done"})),
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

async fn local_hub_transfer_decline(State(st): State<FedState>, Json(req): Json<LocalTransferReq>) -> Json<Value> {
    with_store(&st.store, move |s| {
        if crate::fed::hub::config(&s).is_none() {
            return Json(json!({"error": "this Grimoire is not a hub (start it with --hub)"}));
        }
        match s.get_hub_transfer(req.id) {
            Ok(t) if t.state == grimoire_store::HubTransferState::Done => Json(json!({"error": "that folder is already the hub's"})),
            Ok(_) => match s.set_hub_transfer_state(req.id, grimoire_store::HubTransferState::Declined) {
                Ok(()) => Json(json!({"ok": true})),
                Err(e) => Json(json!({"error": e.to_string()})),
            },
            Err(e) => Json(json!({"error": e.to_string()})),
        }
    })
    .await
}

// --- the hub box itself: local routes for the CLI over an SSH tunnel -------

async fn local_hub_members(State(st): State<FedState>) -> Json<Value> {
    with_store(&st.store, move |s| {
        if crate::fed::hub::config(&s).is_none() {
            return Json(json!({"error": "this Grimoire is not a hub (start it with --hub)"}));
        }
        match crate::fed::hub::members(&s) {
            Ok(m) => Json(json!(m)),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        }
    })
    .await
}

#[derive(Deserialize)]
pub struct ContactIdReq {
    pub contact_id: Uuid,
    pub role: Option<String>,
}

async fn local_hub_approve(State(st): State<FedState>, Json(req): Json<ContactIdReq>) -> Json<Value> {
    let (Some(node_id), Some(endpoint)) = (st.ctx.node_id.clone(), &st.ctx.endpoint) else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    let minted = {
        with_store(&st.store, move |s| {
            crate::fed::hub::approve(s, &node_id, req.contact_id)
        })
        .await
    };
    match minted {
        Ok((hub, member, share, minted)) => {
            Json(crate::fed::hub::deliver_membership(endpoint, &hub, &member, &share, &minted).await)
        }
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

async fn local_hub_eject(State(st): State<FedState>, Json(req): Json<ContactIdReq>) -> Json<Value> {
    let (pubkey, res) = with_store(&st.store, move |s| {
        let pubkey = s
            .list_contacts()
            .unwrap_or_default()
            .into_iter()
            .find(|c| c.id == req.contact_id)
            .map(|c| c.pubkey);
        (pubkey, crate::fed::hub::eject(s, req.contact_id))
    })
    .await;
    match res {
        Ok(dropped) => {
            let cut = pubkey.map(|pk| st.hot.drop_bridges_for_peer(&pk)).unwrap_or(0);
            Json(json!({"ok": true, "dropped": dropped, "bridges_cut": cut}))
        }
        Err(e) => Json(json!({"error": format!("{e:#}")})),
    }
}

async fn local_hub_role(State(st): State<FedState>, Json(req): Json<ContactIdReq>) -> Json<Value> {
    let Some(role) = req.role.as_deref().and_then(grimoire_store::ContactRole::parse) else {
        return Json(json!({"error": "role must be member or admin"}));
    };
    with_store(&st.store, move |s| {
        match crate::fed::hub::set_role(s, req.contact_id, role) {
            Ok(()) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        }
    })
    .await
}

/// Mint a one-time `propose` invite for the hub root (how the first admin,
/// and anyone onboarding from the box, gets a link).
async fn local_hub_invite(State(st): State<FedState>) -> Json<Value> {
    let Some(node_id) = st.ctx.node_id.clone() else {
        return Json(json!({"error": "sharing is off: this Grimoire has no identity yet"}));
    };
    with_store(&st.store, move |s| {
        let Some(hub) = crate::fed::hub::config(s) else {
            return Json(json!({"error": "this Grimoire is not a hub (start it with --hub)"}));
        };
        match crate::fed::mint_invite(s, &node_id, hub.root_doc, grimoire_store::SharePermission::Propose) {
            Ok((share, link)) => Json(json!({"share": share, "link": link, "hub": hub.name})),
            Err(e) => Json(json!({"error": format!("{e:#}")})),
        }
    })
    .await
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
        // hubs I belong to (dials the hub) …
        .route("/admin/hubs", get(list_hubs))
        .route("/admin/hubs/members", get(hub_members))
        .route("/admin/hubs/approve", post(hub_approve))
        .route("/admin/hubs/eject", post(hub_eject))
        .route("/admin/hubs/role", post(hub_role))
        .route("/admin/hubs/invite", post(hub_invite))
        .route("/admin/hubs/queue", get(hub_queue))
        .route("/admin/hubs/resolve", post(hub_resolve))
        .route("/admin/hubs/transfers", get(hub_transfers))
        .route("/admin/hubs/transfers/accept", post(hub_transfer_accept))
        .route("/admin/hubs/transfers/decline", post(hub_transfer_decline))
        .route("/admin/hubs/transfer", post(offer_transfer))
        .route("/admin/hub/transfers", get(local_hub_transfers))
        .route("/admin/hub/transfers/accept", post(local_hub_transfer_accept))
        .route("/admin/hub/transfers/decline", post(local_hub_transfer_decline))
        // … and THIS Grimoire as a hub (the CLI on the box)
        .route("/admin/hub/members", get(local_hub_members))
        .route("/admin/hub/approve", post(local_hub_approve))
        .route("/admin/hub/eject", post(local_hub_eject))
        .route("/admin/hub/role", post(local_hub_role))
        .route("/admin/hub/invite", post(local_hub_invite))
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

    #[tokio::test]
    async fn run_now_is_409_while_the_gardener_is_mid_run() {
        let app = app();
        let create = Request::post("/admin/gardeners")
            .header(ADMIN_HEADER, "s3cret")
            .header("content-type", "application/json")
            .body(Body::from(json!({"name": "tags", "task_prompt": "tag"}).to_string()))
            .unwrap();
        let res = app.clone().oneshot(create).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let g: Value = serde_json::from_slice(&body).unwrap();
        let id: Uuid = g["id"].as_str().unwrap().parse().unwrap();
        // another run holds the claim
        let claim = garden::claim_run(id).unwrap();
        let res = app
            .clone()
            .oneshot(
                Request::post("/admin/garden")
                    .header(ADMIN_HEADER, "s3cret")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "tags"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["code"], "gardener_running");
        // no run row was written for the refused attempt
        let res = app
            .clone()
            .oneshot(Request::get("/admin/runs").header(ADMIN_HEADER, "s3cret").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap().as_array().unwrap().len(), 0);
        drop(claim);
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
            with_store(&store, move |s| {
                s.list_gardeners().unwrap_or_default()
            })
            .await
        };
        // manual-cadence tendings only run via run-now; the daily cut skips them
        for g in gardeners
            .into_iter()
            .filter(|g| g.enabled && g.schedule != "manual")
        {
            if garden::is_running(g.id) {
                tracing::info!("gardener {}: skipped, a run-now is still going", g.name);
                continue;
            }
            let name = g.name.clone();
            let out = garden::run_gardener(store.clone(), hot.clone(), g).await;
            tracing::info!("gardener {name}: {} — {}", out.status, out.summary);
        }
    }
}
