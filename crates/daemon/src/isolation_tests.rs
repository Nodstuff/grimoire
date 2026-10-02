//! ADR 0004: the two-tenant isolation suite, end to end in SERVER mode —
//! the real routers under `require_auth`, real bearer tokens.
//!
//! A (Tom, the instance owner) has a private "Work" with a doc whose text is
//! `zebrasecret`, an Unsorted doc, a trashed doc, a comment and an agent's
//! flagged edit on the private doc, a tag, a To-do item and an Inbox note. A
//! shares "Family" with B (Aoife) as a viewer. B has her own "Work" and an
//! Unsorted doc. For every route and MCP tool, B sees nothing of A's private
//! data, only the shared workspace, and a by-id read of A's private doc
//! answers 404 exactly like a doc that never existed.
//!
//! `COVERAGE` lists every route (by path) and every MCP tool with the test
//! that proves it; `every_route_and_tool_is_covered` parses the routers'
//! sources and the tool list and fails when something new is missing.

use crate::auth::{AuthConfig, AuthState, hash_secret, now, random_token, require_auth};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use taisce_store::auth::{Grant, OAuthClient};
use taisce_store::{BlockStore, OpInput, OpKind, PrincipalKind, Role, Scope, SharedStore, SqliteStore};
use tower::ServiceExt;
use uuid::Uuid;

const BASE: &str = "http://localhost:7513";

/// route path (or `mcp:<tool>`) → the test that proves it, or why it holds
/// no tenant data.
const COVERAGE: &[(&str, &str)] = &[
    ("/api/docs", "api_reads_never_show_a_private_doc"),
    ("/api/propose", "api_writes_answer_404_or_403"),
    ("/api/propose_markdown", "api_writes_answer_404_or_403"),
    ("/api/doc/{id}", "api_reads_never_show_a_private_doc"),
    ("/api/doc/{id}/backlinks", "api_reads_never_show_a_private_doc"),
    ("/api/queue", "api_lists_are_per_user"),
    ("/api/doc/{id}/review", "api_reads_never_show_a_private_doc"),
    ("/api/flags", "api_lists_are_per_user"),
    ("/api/flags/dismiss", "api_writes_answer_404_or_403"),
    ("/api/principals", "api_lists_are_per_user"),
    ("/api/doc/{id}/history", "api_reads_never_show_a_private_doc"),
    ("/api/doc/{id}/status", "api_writes_answer_404_or_403"),
    ("/api/doc/{id}/move", "api_writes_answer_404_or_403"),
    ("/api/buildinfo", "server_level_routes_are_the_owners (no tenant data: version only)"),
    ("/api/diagnostics", "server_level_routes_are_the_owners"),
    ("/api/gardeners/preflight", "server_level_routes_are_the_owners (no tenant data: is claude installed)"),
    ("/api/stamp", "api_lists_are_per_user"),
    ("/api/doc/{id}/delete", "api_writes_answer_404_or_403"),
    ("/api/doc/{id}/restore", "api_writes_answer_404_or_403"),
    ("/api/trash", "api_lists_are_per_user"),
    ("/api/backups", "server_level_routes_are_the_owners"),
    ("/api/backups/reveal", "server_level_routes_are_the_owners"),
    ("/api/export_vault", "server_level_routes_are_the_owners"),
    ("/api/doc/{id}/export", "server_level_routes_are_the_owners"),
    ("/api/doc/{id}/markdown", "api_reads_never_show_a_private_doc"),
    ("/api/import", "import_and_ask_stay_in_the_callers_tenant"),
    ("/api/ask", "import_and_ask_stay_in_the_callers_tenant"),
    ("/api/memory/sync", "server_level_routes_are_the_owners"),
    ("/api/doc/{id}/rename", "api_writes_answer_404_or_403"),
    ("/api/doc/{id}/tendings", "api_reads_never_show_a_private_doc"),
    ("/api/comment", "api_writes_answer_404_or_403"),
    ("/api/resolve", "api_writes_answer_404_or_403"),
    ("/api/resolve_bulk", "api_writes_answer_404_or_403"),
    ("/api/search", "api_lists_are_per_user"),
    ("/api/tags", "api_lists_are_per_user"),
    ("/api/runs", "api_lists_are_per_user"),
    ("/api/graph", "api_lists_are_per_user"),
    ("/api/render/d2", "server_level_routes_are_the_owners (no tenant data: renders the posted source)"),
    ("/api/doc/{id}/living", "api_reads_never_show_a_private_doc"),
    ("/api/home/visit", "home_inbox_and_todo_are_each_users_own"),
    ("/api/home/since", "home_inbox_and_todo_are_each_users_own"),
    ("/api/inbox", "home_inbox_and_todo_are_each_users_own"),
    ("/api/changes", "the_feed_shows_only_visible_docs_and_tells_b_to_drop_revoked_ones"),
    ("/api/changes/stream", "the_feed_shows_only_visible_docs_and_tells_b_to_drop_revoked_ones"),
    ("/api/{*rest}", "unknown_api_paths_are_404"),
    ("/api/workspaces", "workspaces_and_membership_routes"),
    ("/api/workspaces/{id}", "workspaces_and_membership_routes"),
    ("/api/workspaces/{id}/members", "workspaces_and_membership_routes"),
    ("/api/workspaces/{id}/members/{user}", "workspaces_and_membership_routes"),
    ("/api/docs/{id}/workspace", "api_writes_answer_404_or_403"),
    ("/api/todo", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/parse", "home_inbox_and_todo_are_each_users_own (no tenant data: parses the posted text)"),
    ("/api/todo/due", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/toggle", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/edit", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/remove", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/move", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/deadline", "home_inbox_and_todo_are_each_users_own"),
    ("/api/todo/note", "home_inbox_and_todo_are_each_users_own"),
    ("/api/devices", "devices_and_profile_are_the_callers"),
    ("/api/devices/{token}", "devices_and_profile_are_the_callers"),
    ("/api/profile", "devices_and_profile_are_the_callers"),
    // the box's own CLI: admin token, refused when forwarded (auth::require_auth), System scope
    ("/admin/gardeners", "admin_routes_are_the_boxs_cli"),
    ("/admin/garden", "admin_routes_are_the_boxs_cli"),
    ("/admin/gardeners/update", "admin_routes_are_the_boxs_cli"),
    ("/admin/runs", "admin_routes_are_the_boxs_cli"),
    ("/admin/policy", "admin_routes_are_the_boxs_cli"),
    ("/admin/living/refresh", "admin_routes_are_the_boxs_cli"),
    ("mcp:find_doc", "mcp_reads_never_show_a_private_doc"),
    ("mcp:orient", "mcp_reads_never_show_a_private_doc"),
    ("mcp:read_doc", "mcp_reads_never_show_a_private_doc"),
    ("mcp:edit_doc", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:append", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:propose_markdown", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:propose", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:diff_since", "mcp_reads_never_show_a_private_doc"),
    ("mcp:search", "mcp_reads_never_show_a_private_doc"),
    ("mcp:grep", "mcp_reads_never_show_a_private_doc"),
    ("mcp:related", "mcp_reads_never_show_a_private_doc"),
    ("mcp:create_doc", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:doc_op", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:add_comment", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:resolve", "mcp_writes_answer_not_found_or_read_only"),
    ("mcp:proposals", "mcp_reads_never_show_a_private_doc"),
    // regressions from the adversarial review (a route may carry several rows)
    ("/api/flags", "unshare_then_a_stale_anchor_reads_nothing_through_flags"),
    ("mcp:propose", "unshare_then_a_stale_anchor_reads_nothing_through_flags"),
    ("/api/resolve", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/resolve_bulk", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/docs/{id}/workspace", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/doc/{id}/move", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/doc/{id}/rename", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/doc/{id}/status", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/doc/{id}/delete", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/workspaces", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/profile", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/import", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/propose", "connector_tokens_on_api_get_the_agent_treatment"),
    ("/api/todo", "a_viewer_reading_a_shared_todo_creates_nothing"),
    ("/api/docs/{id}/workspace", "unlabel_into_a_hidden_parent_is_a_generic_refusal"),
    ("mcp:append", "agent_principals_are_per_person"),
    ("/api/todo", "a_connectors_get_writes_nothing"),
    ("/api/devices", "first_party_is_a_pinned_client_not_a_declared_redirect"),
    ("/api/doc/{id}", "a_pre_access_tombstone_is_gone_on_every_block_path"),
    ("mcp:read_doc", "a_pre_access_tombstone_is_gone_on_every_block_path"),
    ("mcp:add_comment", "a_pre_access_tombstone_is_gone_on_every_block_path"),
    ("mcp:related", "a_pre_access_tombstone_is_gone_on_every_block_path"),
    ("/api/doc/{id}/history", "agent_principals_are_per_person"),
    ("/api/doc/{id}/history", "history_says_which_principals_are_yours"),
    // auth/oauth.rs: public (no token), server-level documents and the OAuth
    // endpoints; none reads a tenant's data
    ("/.well-known/oauth-protected-resource", "public_auth_routes_carry_no_tenant_data"),
    ("/.well-known/oauth-protected-resource/mcp", "public_auth_routes_carry_no_tenant_data"),
    ("/.well-known/oauth-authorization-server", "public_auth_routes_carry_no_tenant_data"),
    ("/.well-known/oauth-authorization-server/mcp", "public_auth_routes_carry_no_tenant_data"),
    ("/.well-known/apple-app-site-association", "public_auth_routes_carry_no_tenant_data"),
    ("/.well-known/{*rest}", "public_auth_routes_carry_no_tenant_data"),
    ("/oauth/app-callback", "public_auth_routes_carry_no_tenant_data"),
    ("/oauth/register", "public_auth_routes_carry_no_tenant_data (client rows only; auth::tests covers DCR)"),
    ("/oauth/authorize", "public_auth_routes_carry_no_tenant_data (the passkey decides the user; auth::tests)"),
    ("/oauth/token", "public_auth_routes_carry_no_tenant_data (a code or refresh token names its user; auth::tests)"),
    ("/oauth/revoke", "public_auth_routes_carry_no_tenant_data"),
    // auth/web.rs: the web UI's sign-in (public: a passkey decides the user)
    ("/auth/web/begin", "web_session_routes_answer_only_for_the_caller (a passkey challenge; auth::tests)"),
    ("/auth/web/finish", "web_session_routes_answer_only_for_the_caller (the passkey names the user; auth::tests)"),
    ("/auth/web/logout", "web_session_routes_answer_only_for_the_caller"),
    ("/auth/web/session", "web_session_routes_answer_only_for_the_caller"),
    // the web UI's session cookie on every /api route above (the suites re-run as B's browser)
    ("/api/docs", "a_web_session_for_b_sees_none_of_as_data"),
    ("/api/workspaces/{id}/members", "the_owner_shares_from_the_web_ui_and_a_session_get_writes_nothing"),
    ("/api/todo", "the_owner_shares_from_the_web_ui_and_a_session_get_writes_nothing"),
];

thread_local! {
    /// Re-run a suite with B signed in through the web UI's session cookie
    /// instead of her app's bearer token (`a_web_session_for_b_sees_none_of_as_data`).
    static B_VIA_COOKIE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `Fx::b_app` when B is a browser: the cookie's value behind this prefix.
const COOKIE_TOKEN: &str = "cookie:";

struct Fx {
    app: Router,
    store: SharedStore,
    a: Uuid,
    b: Uuid,
    a_human: Uuid,
    a_app: String,
    b_app: String,
    /// B's Claude over OAuth (a connector, not her app)
    b_conn: String,
    /// B's personal access token (MCP only)
    b_pat: String,
    a_work: Uuid,
    b_work: Uuid,
    family: Uuid,
    a_secret: Uuid,
    a_secret_block: Uuid,
    a_comment: Uuid,
    a_ann: Uuid,
    a_loose: Uuid,
    a_trash: Uuid,
    shared_doc: Uuid,
    shared_block: Uuid,
    b_doc: Uuid,
    /// B is signed in through the web UI (`b_app` is `cookie:<value>`)
    cookie: bool,
}

fn para(content: &str) -> (Uuid, OpInput) {
    let id = Uuid::now_v7();
    (
        id,
        OpInput {
            kind: OpKind::Insert {
                block_id: id,
                parent_id: None,
                order_key: String::new(),
                block_type: taisce_store::BlockType::Paragraph,
                content: content.into(),
                refers_to: None,
            },
            source_refs: vec![],
        },
    )
}

/// An OAuth access token for `user`, minted straight into the store (the
/// passkey ceremony is `auth::tests`' job). `app` = the person's own app.
fn mint(s: &mut SqliteStore, user: Uuid, name: &str, app: bool) -> String {
    let redirect = if app { "ie.null.taisce:/cb".to_string() } else { "https://claude.ai/api/mcp/auth_callback".to_string() };
    let client_id = format!("dcr_{}", Uuid::now_v7().simple());
    s.oauth_upsert_client(&OAuthClient {
        client_id: client_id.clone(),
        kind: "dcr".into(),
        client_name: name.into(),
        redirect_uris: vec![redirect],
        metadata: "{}".into(),
        created_at: now(),
        refresh_at: None,
    })
    .unwrap();
    let access = random_token();
    let grant = Grant {
        id: Uuid::now_v7(),
        client_id,
        user_id: user,
        resource: None,
        scope: crate::auth::SCOPE.into(),
        created_at: now(),
        revoked_at: None,
        revoke_why: None,
        credential_id: None,
    };
    s.oauth_issue_grant(None, &grant, &hash_secret(&access), now() + 3600, &hash_secret(&random_token()), now() + 86400)
        .unwrap();
    if app {
        // first party is a pinned client_id, never a declared redirect
        s.oauth_mark_first_party(&grant.client_id, now()).unwrap();
    }
    access
}

fn fixture() -> Fx {
    let mut raw = SqliteStore::open_in_memory().unwrap();
    let a_human = raw.create_principal(PrincipalKind::Human, "Tom", None).unwrap().id;
    let agent = raw.create_principal(PrincipalKind::Agent, "claude", None).unwrap().id;
    let gardener_agent = raw.create_principal(PrincipalKind::Agent, "claude:tagger", None).unwrap().id;
    let a = raw.auth_ensure_owner(a_human, "Tom", now()).unwrap().id;
    let bu = raw.auth_add_user("Aoife", now()).unwrap();
    let (b, b_human) = (bu.id, bu.principal_id);
    let a_app = mint(&mut raw, a, "Taisce", true);
    let b_app = mint(&mut raw, b, "Taisce", true);
    let b_conn = mint(&mut raw, b, "Claude", false);
    let b_pat = crate::auth::create_api_token(&mut raw, Some(&b.to_string()), "claude-code", None, now()).unwrap().1.unwrap();
    let cookie = B_VIA_COOKIE.with(|c| c.get());
    let b_app = if cookie { web_session(&mut raw, b) } else { b_app };
    let store = SharedStore::new(raw);

    let (a_work, family, a_secret, a_secret_block, a_comment, a_ann, a_loose, a_trash, shared_doc, shared_block) = {
        let mut s = store.lock(Scope::User(a));
        let a_work = s.create_workspace("Work", None, None, None).unwrap().id;
        let family = s.create_workspace("Family", None, None, None).unwrap().id;
        let (wroot, _) = s.create_doc_with_ops("Work root", None, a_human, vec![para("work root text").1]).unwrap();
        s.set_doc_workspace(wroot.id, Some(a_work), a_human).unwrap();
        let (sb, op) = para("zebrasecret salary figures [[Family plan]]");
        let (secret, _) =
            s.create_doc_with_ops("Salary review", Some(wroot.id), a_human, vec![op, para("---\ntags: [moneytag]\n---").1]).unwrap();
        let comment = s.add_comment(sb, agent, "zebrasecret flag from an agent", None).unwrap().id;
        // an agent's stale write → an open review annotation on A's private doc
        let e = s.get_doc(secret.id).unwrap().current_epoch;
        s.propose_reviewed(secret.id, e, gardener_agent, vec![para("zebrasecret agent note").1]).unwrap();
        let ann = s.review_queue(Some(secret.id)).unwrap()[0].annotation.id;
        let (loose, _) = s.create_doc_with_ops("Tom loose", None, a_human, vec![para("zebrasecret unsorted note").1]).unwrap();
        let (trash, _) = s.create_doc_with_ops("Tom trashed", None, a_human, vec![para("zebrasecret trashed").1]).unwrap();
        s.delete_doc(trash.id).unwrap();
        let (shb, op) = para("familyshared plan text");
        let (fam, _) = s.create_doc_with_ops("Family plan", None, a_human, vec![op]).unwrap();
        s.set_doc_workspace(fam.id, Some(family), a_human).unwrap();
        s.share_workspace(family, b, Role::Viewer).unwrap();
        (a_work, family, secret.id, sb, comment, ann, loose.id, trash.id, fam.id, shb)
    };
    let (b_work, b_doc) = {
        let mut s = store.lock(Scope::User(b));
        let b_work = s.create_workspace("Work", None, None, None).unwrap().id;
        let (d, _) = s.create_doc_with_ops("Aoife work", None, b_human, vec![para("aoifeown work text").1]).unwrap();
        s.set_doc_workspace(d.id, Some(b_work), b_human).unwrap();
        s.create_doc_with_ops("Aoife loose", None, b_human, vec![para("aoifeown unsorted").1]).unwrap();
        (b_work, d.id)
    };

    let cfg = AuthConfig::from_public_url(BASE).unwrap();
    let st = AuthState::new(cfg, store.clone()).unwrap();
    let hosts = vec![st.cfg.authority(), st.cfg.rp_id.clone()];
    let dedupe = crate::mcp::new_dedupe();
    let dir = std::env::temp_dir().join(format!("taisce-isolation-{}", Uuid::now_v7()));
    let app = crate::mcp::router_with_hosts(store.clone(), agent, dedupe.clone(), None, Some(hosts))
        .merge(crate::api::router(crate::api::ApiState {
            changes: crate::changes::Feed::new(&store),
            store: store.clone(),
            human: a_human,
            server_mode: true,
            db_path: dir.join("ks.db"),
            embedder: None,
            dedupe,
        }))
        .merge(crate::admin::router(store.clone(), crate::admin::AdminToken::fixed("admintok"), true))
        .merge(crate::push::router(crate::push::DevicesState { store: store.clone(), default_env: "production".into() }))
        .merge(crate::auth::router(st.clone()))
        .layer(axum::middleware::from_fn_with_state(st, require_auth));
    Fx {
        app,
        store,
        a,
        b,
        a_human,
        a_app,
        b_app,
        b_conn,
        b_pat,
        a_work,
        b_work,
        family,
        a_secret,
        a_secret_block,
        a_comment,
        a_ann,
        a_loose,
        a_trash,
        shared_doc,
        shared_block,
        b_doc,
        cookie,
    }
}

/// A web UI session for `user`, minted straight into the store (the passkey
/// ceremony is `auth::tests`' job): `cookie:<value>` for `Fx::call`.
fn web_session(s: &mut SqliteStore, user: Uuid) -> String {
    let value = random_token();
    s.auth_create_web_session(user, None, &hash_secret(&value), "Mac · Safari", now()).unwrap();
    format!("{COOKIE_TOKEN}{value}")
}

/// Authenticate a request as `token`: a bearer, or (`cookie:…`) the web UI's
/// session cookie with the same-origin headers the UI sends on writes.
fn authed(mut b: axum::http::request::Builder, token: &str, method: &str) -> axum::http::request::Builder {
    match token.strip_prefix(COOKIE_TOKEN) {
        Some(value) => {
            b = b.header("cookie", format!("{}={value}", crate::auth::web::COOKIE));
            if method != "GET" && method != "HEAD" {
                b = b.header("origin", BASE).header(crate::auth::web::CSRF_HEADER, "1");
            }
            b
        }
        None => b.header("authorization", format!("Bearer {token}")),
    }
}

impl Fx {
    /// Every marker of A's private data: none may appear in anything B gets.
    fn secrets(&self) -> Vec<String> {
        vec![
            "zebrasecret".into(),
            "Salary review".into(),
            "Tom loose".into(),
            "Tom trashed".into(),
            "moneytag".into(),
            self.a_secret.to_string(),
            self.a_loose.to_string(),
            self.a_trash.to_string(),
            self.a_work.to_string(),
            self.a_comment.to_string(),
            self.a_ann.to_string(),
        ]
    }

    #[track_caller]
    fn no_leak(&self, what: &str, text: &str) {
        self.no_leak_besides(what, text, &[]);
    }

    /// `no_leak`, allowing the ids the caller itself sent (an error may
    /// echo the id it was asked about — that tells the caller nothing).
    #[track_caller]
    fn no_leak_besides(&self, what: &str, text: &str, echo: &[Uuid]) {
        let mut text = text.to_string();
        for id in echo {
            text = text.replace(&id.to_string(), "<asked>");
        }
        for s in self.secrets() {
            assert!(!text.contains(&s), "{what} leaks {s:?}: {}", &text[..text.len().min(600)]);
        }
    }

    async fn call(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> (StatusCode, String) {
        let mut b = authed(Request::builder().method(method).uri(path).header("host", "localhost:7513"), token, method);
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let res = self.app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn b(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, String) {
        self.call(&self.b_app, method, path, body).await
    }

    /// One stateless MCP tools/call as `token`: (is_error, text).
    async fn tool(&self, token: &str, name: &str, args: Value) -> (bool, String) {
        let req = |body: Value, session: Option<&str>| {
            let mut b = Request::post("/mcp")
                .header("host", "localhost:7513")
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("authorization", format!("Bearer {token}"));
            if let Some(s) = session {
                b = b.header("mcp-session-id", s);
            }
            b.body(Body::from(body.to_string())).unwrap()
        };
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
        let r = self.app.clone().oneshot(req(init, None)).await.unwrap();
        let session = r.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
        let _ = self.app.clone().oneshot(req(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), session.as_deref())).await;
        let call = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name": name, "arguments": args}});
        let r = self.app.clone().oneshot(req(call, session.as_deref())).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                .last()
                .unwrap_or_else(|| panic!("no JSON-RPC message in {text}"))
        });
        let res = &v["result"];
        (res["isError"].as_bool().unwrap_or(false), res["content"][0]["text"].as_str().unwrap_or_default().to_string())
    }
}

/// The 404 B gets for A's private doc must be the 404 anyone gets for a doc
/// that never existed: same status, same body shape.
#[track_caller]
fn same_as_missing(fx: &Fx, what: &str, got: (StatusCode, String), missing: (StatusCode, String)) {
    assert_eq!(got.0, StatusCode::NOT_FOUND, "{what}: {}", got.1);
    assert_eq!(missing.0, StatusCode::NOT_FOUND, "{what} (missing): {}", missing.1);
    fx.no_leak_besides(what, &got.1, &[fx.a_secret]);
    let strip = |s: &str| s.replace(&fx.a_secret.to_string(), "ID").replace(&fx.a_secret_block.to_string(), "ID");
    let g = strip(&got.1);
    let m = missing.1.replace(&MISSING.to_string(), "ID");
    assert_eq!(g, m, "{what}: an invisible doc must answer exactly like a missing one");
}

const MISSING: Uuid = Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_dead_beef);

#[tokio::test]
async fn api_reads_never_show_a_private_doc() {
    let fx = fixture();
    let (st, docs) = fx.b("GET", "/api/docs", None).await;
    assert_eq!(st, StatusCode::OK);
    fx.no_leak("/api/docs", &docs);
    assert!(docs.contains(&fx.shared_doc.to_string()) && docs.contains(&fx.b_doc.to_string()), "{docs}");
    for (what, path) in [
        ("doc", "/api/doc/{}"),
        ("backlinks", "/api/doc/{}/backlinks"),
        ("review", "/api/doc/{}/review"),
        ("history", "/api/doc/{}/history"),
        ("markdown", "/api/doc/{}/markdown"),
        ("living", "/api/doc/{}/living"),
        ("tendings", "/api/doc/{}/tendings"),
    ] {
        let got = fx.b("GET", &path.replace("{}", &fx.a_secret.to_string()), None).await;
        let missing = fx.b("GET", &path.replace("{}", &MISSING.to_string()), None).await;
        same_as_missing(&fx, what, got, missing);
    }
    // the shared doc reads fine; its backlinks do not reveal A's private linker
    let (st, shared) = fx.b("GET", &format!("/api/doc/{}", fx.shared_doc), None).await;
    assert_eq!(st, StatusCode::OK, "{shared}");
    let (st, back) = fx.b("GET", &format!("/api/doc/{}/backlinks", fx.shared_doc), None).await;
    assert_eq!(st, StatusCode::OK);
    fx.no_leak("backlinks of the shared doc", &back);
    assert_eq!(back.trim(), "[]");
    // the owner still reads it
    assert_eq!(fx.call(&fx.a_app, "GET", &format!("/api/doc/{}", fx.a_secret), None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn api_lists_are_per_user() {
    let fx = fixture();
    for path in ["/api/search?q=zebrasecret", "/api/search?q=salary", "/api/tags", "/api/graph", "/api/flags", "/api/queue", "/api/trash", "/api/runs"] {
        let (st, body) = fx.b("GET", path, None).await;
        assert_eq!(st, StatusCode::OK, "{path}: {body}");
        fx.no_leak(path, &body);
    }
    let (_, hits) = fx.b("GET", "/api/search?q=familyshared", None).await;
    assert!(hits.contains(&fx.shared_doc.to_string()), "B finds the shared doc: {hits}");
    // A sees her own trash, queue and flags
    let (_, t) = fx.call(&fx.a_app, "GET", "/api/trash", None).await;
    assert!(t.contains(&fx.a_trash.to_string()));
    let (_, q) = fx.call(&fx.a_app, "GET", "/api/queue", None).await;
    assert!(q.contains(&fx.a_ann.to_string()));
    // principals: Tom shares Family with B, so his name shows; a stranger's does not
    fx.store.lock(Scope::System).auth_add_user("Stranger", now()).unwrap();
    let (_, p) = fx.b("GET", "/api/principals", None).await;
    assert!(p.contains("Aoife") && p.contains("Tom") && !p.contains("Stranger"), "{p}");
    // the change stamp: A editing her private doc does not move B's
    let (_, s1) = fx.b("GET", "/api/stamp", None).await;
    {
        let mut s = fx.store.lock(Scope::User(fx.a));
        let e = s.get_doc(fx.a_secret).unwrap().current_epoch;
        s.apply(fx.a_secret, e, fx.a_human, vec![para("zebrasecret more").1]).unwrap();
    }
    let (_, s2) = fx.b("GET", "/api/stamp", None).await;
    let stamp = |s: &str| serde_json::from_str::<Value>(s).unwrap()["stamp"].clone();
    assert_eq!(stamp(&s1), stamp(&s2), "B's stamp must not move with A's private edits");
}

#[tokio::test]
async fn api_writes_answer_404_or_403() {
    let fx = fixture();
    let secret = fx.a_secret;
    let not_found = [
        ("POST", "/api/propose".to_string(), json!({"doc_id": secret, "base_epoch": 1, "ops": []})),
        ("POST", "/api/propose_markdown".to_string(), json!({"doc_id": secret, "base_epoch": 1, "markdown": "x"})),
        ("POST", "/api/docs".to_string(), json!({"title": "child", "parent_doc_id": secret})),
        ("POST", "/api/comment".to_string(), json!({"block_id": fx.a_secret_block, "text": "hi"})),
        ("POST", format!("/api/doc/{secret}/status"), json!({"status": "draft"})),
        ("POST", format!("/api/doc/{secret}/move"), json!({"parent_id": null})),
        ("POST", format!("/api/doc/{secret}/rename"), json!({"title": "mine"})),
        ("POST", format!("/api/doc/{secret}/delete"), json!({})),
        ("POST", format!("/api/doc/{}/restore", fx.a_trash), json!({})),
        ("POST", "/api/resolve".to_string(), json!({"annotation_id": fx.a_ann, "decision": "accept"})),
        ("POST", "/api/flags/dismiss".to_string(), json!({"comment_id": fx.a_comment})),
        ("PUT", format!("/api/docs/{secret}/workspace"), json!({"workspace_id": fx.b_work})),
        ("PUT", format!("/api/docs/{}/workspace", fx.b_doc), json!({"workspace_id": fx.a_work})),
    ];
    let asked = [fx.a_secret, fx.a_secret_block, fx.a_trash, fx.a_ann, fx.a_comment, fx.a_work];
    for (m, path, body) in not_found {
        let (st, out) = fx.b(m, &path, Some(body)).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{m} {path}: {out}");
        fx.no_leak_besides(&path, &out, &asked);
    }
    let (st, bulk) = fx.b("POST", "/api/resolve_bulk", Some(json!({"annotation_ids": [fx.a_ann], "decision": "decline"}))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(bulk.contains("\"resolved\":0") && bulk.contains("not found"), "{bulk}");
    fx.no_leak_besides("resolve_bulk", &bulk, &asked);
    // the shared doc as a viewer: visible, read-only → 403
    let shared = fx.shared_doc;
    let e = fx.store.lock(Scope::System).get_doc(shared).unwrap().current_epoch;
    let read_only = [
        ("POST", "/api/propose".to_string(), json!({"doc_id": shared, "base_epoch": e, "ops": [para("x").1]})),
        ("POST", "/api/comment".to_string(), json!({"block_id": fx.shared_block, "text": "hi"})),
        ("POST", format!("/api/doc/{shared}/rename"), json!({"title": "ours"})),
        ("POST", format!("/api/doc/{shared}/delete"), json!({})),
        ("POST", "/api/docs".to_string(), json!({"title": "child", "parent_doc_id": shared})),
        ("PUT", format!("/api/docs/{}/workspace", fx.b_doc), json!({"workspace_id": fx.family})),
    ];
    for (m, path, body) in read_only {
        let (st, out) = fx.b(m, &path, Some(body)).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{m} {path}: {out}");
    }
    // nothing of A's moved
    let s = fx.store.lock(Scope::System);
    assert_eq!(s.get_doc(secret).unwrap().title, "Salary review");
    assert!(s.doc_is_tombstoned(fx.a_trash).unwrap());
    assert_eq!(s.review_queue(Some(secret)).unwrap().len(), 1);
}

#[tokio::test]
async fn the_feed_shows_only_visible_docs_and_tells_b_to_drop_revoked_ones() {
    let fx = fixture();
    let (st, page) = fx.b("GET", "/api/changes?since=0&limit=2000", None).await;
    assert_eq!(st, StatusCode::OK);
    fx.no_leak("/api/changes", &page);
    assert!(page.contains(&fx.shared_doc.to_string()));
    let seq = serde_json::from_str::<Value>(&page).unwrap()["seq"].as_i64().unwrap();
    // the stream's replay is filtered the same way
    let req = authed(Request::get("/api/changes/stream?since=0").header("host", "localhost:7513"), &fx.b_app, "GET")
        .body(Body::empty())
        .unwrap();
    let res = fx.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let mut stream = res.into_body().into_data_stream();
    let mut replay = String::new();
    use futures_util::StreamExt;
    while let Ok(Some(Ok(chunk))) = tokio::time::timeout(std::time::Duration::from_millis(300), stream.next()).await {
        replay.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(replay.contains(&fx.shared_doc.to_string()), "{replay}");
    fx.no_leak("/api/changes/stream", &replay);
    // revocation: A removes B from Family → B is told to drop the doc
    let (st, _) = fx.call(&fx.a_app, "DELETE", &format!("/api/workspaces/{}/members/{}", fx.family, fx.b), None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, page) = fx.b("GET", &format!("/api/changes?since={seq}"), None).await;
    let v: Value = serde_json::from_str(&page).unwrap();
    let drop_row = v["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["doc_id"] == json!(fx.shared_doc))
        .unwrap_or_else(|| panic!("no drop row: {page}"));
    assert_eq!(drop_row["kind"], "deleted");
    assert_eq!(drop_row["access"], "revoked");
    assert!(drop_row.get("doc").is_none(), "no summary of a doc she cannot see: {drop_row}");
    let (_, hits) = fx.b("GET", "/api/search?q=familyshared", None).await;
    assert_eq!(hits.trim(), "[]", "search forgets the revoked doc");
    assert_eq!(fx.b("GET", &format!("/api/doc/{}", fx.shared_doc), None).await.0, StatusCode::NOT_FOUND);
    // and MCP search too
    let (_, out) = fx.tool(&fx.b_pat, "search", json!({"query": "familyshared"})).await;
    assert!(!out.contains(&fx.shared_doc.to_string()), "{out}");
}

#[tokio::test]
async fn home_inbox_and_todo_are_each_users_own() {
    let fx = fixture();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    // A's to-do item and inbox note
    let (st, out) = fx
        .call(&fx.a_app, "POST", "/api/todo", Some(json!({"date": today, "text": "zebrasecret errand", "today": today})))
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let (st, out) = fx.call(&fx.a_app, "POST", "/api/inbox", Some(json!({"text": "zebrasecret capture"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let a_item = serde_json::from_str::<Value>(
        &fx.call(&fx.a_app, "GET", &format!("/api/todo?date={today}&today={today}"), None).await.1,
    )
    .unwrap()["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for path in [
        format!("/api/todo?date={today}&today={today}"),
        format!("/api/todo/due?today={today}"),
        format!("/api/todo/parse?text=tomorrow&today={today}"),
        "/api/inbox".to_string(),
        "/api/home/since".to_string(),
        "/api/home/visit".to_string(),
    ] {
        let (st, body) = fx.b("GET", &path, None).await;
        // a browser session's GET never creates B's (missing) list
        let read_only_404 = fx.cookie && path.starts_with("/api/todo?") && st == StatusCode::NOT_FOUND;
        assert!(st == StatusCode::OK || read_only_404, "{path}: {st} {body}");
        fx.no_leak(&path, &body);
    }
    // B's writes land in B's own list; A's item ids mean nothing there
    let (_, mine) = fx.b("POST", "/api/todo", Some(json!({"date": today, "text": "aoife errand", "today": today}))).await;
    assert!(mine.contains("aoife errand"));
    fx.no_leak("B's to-do", &mine);
    for (path, body) in [
        ("/api/todo/toggle", json!({"date": today, "item_id": a_item, "done": true, "today": today})),
        ("/api/todo/edit", json!({"date": today, "item_id": a_item, "text": "hijacked", "today": today})),
        ("/api/todo/remove", json!({"date": today, "item_id": a_item, "today": today})),
        ("/api/todo/move", json!({"date": today, "item_id": a_item, "to_date": "2030-01-01", "today": today})),
        ("/api/todo/deadline", json!({"date": today, "item_id": a_item, "deadline": "2030-01-01", "today": today})),
        ("/api/todo/note", json!({"date": today, "item_id": a_item, "note": "x", "today": today})),
    ] {
        let (_, out) = fx.b("POST", path, Some(body)).await;
        fx.no_leak(path, &out);
    }
    let (_, a_list) = fx.call(&fx.a_app, "GET", &format!("/api/todo?date={today}&today={today}"), None).await;
    assert!(a_list.contains("zebrasecret errand") && !a_list.contains("hijacked") && !a_list.contains("aoife errand"), "{a_list}");
    // inbox: B's capture makes B's own Inbox
    let (_, b_cap) = fx.b("POST", "/api/inbox", Some(json!({"text": "aoife capture"}))).await;
    let (_, a_inbox) = fx.call(&fx.a_app, "GET", "/api/inbox", None).await;
    assert!(!a_inbox.contains("aoife capture"));
    let b_inbox_id = serde_json::from_str::<Value>(&b_cap).unwrap()["inbox_id"].clone();
    assert_ne!(b_inbox_id, serde_json::from_str::<Value>(&a_inbox).unwrap()["doc_id"]);
    // visit stamps are per user
    fx.b("POST", "/api/home/visit", Some(json!({"at": "2030-01-01T00:00:00.000Z"}))).await;
    let (_, a_visit) = fx.call(&fx.a_app, "GET", "/api/home/visit", None).await;
    assert!(!a_visit.contains("2030-01-01"), "{a_visit}");
}

#[tokio::test]
async fn workspaces_and_membership_routes() {
    let fx = fixture();
    let (_, list) = fx.b("GET", "/api/workspaces", None).await;
    let v: Value = serde_json::from_str(&list).unwrap();
    let ws = v["workspaces"].as_array().unwrap();
    assert_eq!(ws.len(), 2, "{list}");
    fx.no_leak("/api/workspaces", &list);
    let fam = ws.iter().find(|w| w["id"] == json!(fx.family)).unwrap();
    assert_eq!((fam["owner_name"].as_str(), fam["role"].as_str(), fam["shared"].as_bool()), (Some("Tom"), Some("viewer"), Some(true)));
    // A's Work: 404 for every by-id route; Family (viewer): 403 for owner-only writes
    for (m, path, body) in [
        ("PATCH", format!("/api/workspaces/{}", fx.a_work), Some(json!({"name": "x"}))),
        ("DELETE", format!("/api/workspaces/{}", fx.a_work), None),
        ("GET", format!("/api/workspaces/{}/members", fx.a_work), None),
        ("POST", format!("/api/workspaces/{}/members", fx.a_work), Some(json!({"user": "Aoife", "role": "editor"}))),
    ] {
        let (st, out) = fx.b(m, &path, body).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{m} {path}: {out}");
    }
    for (m, path, body) in [
        ("PATCH", format!("/api/workspaces/{}", fx.family), Some(json!({"name": "Ours"}))),
        ("DELETE", format!("/api/workspaces/{}", fx.family), None),
        ("POST", format!("/api/workspaces/{}/members", fx.family), Some(json!({"user": "Aoife", "role": "editor"}))),
        ("DELETE", format!("/api/workspaces/{}/members/{}", fx.family, fx.a), None),
    ] {
        let (st, out) = fx.b(m, &path, body).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{m} {path}: {out}");
    }
    let (st, members) = fx.b("GET", &format!("/api/workspaces/{}/members", fx.family), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(members.contains("Tom") && members.contains("Aoife"));
    // membership is a human surface: a connector token (Claude over OAuth)
    // is refused even for the owner's own workspace, and a PAT never opens /api
    let (st, _) = fx.call(&fx.b_conn, "POST", &format!("/api/workspaces/{}/members", fx.b_work), Some(json!({"user": "Tom", "role": "viewer"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = fx.call(&fx.b_conn, "DELETE", &format!("/api/workspaces/{}/members/{}", fx.b_work, fx.a), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for (m, path) in [("GET", "/api/workspaces".to_string()), ("POST", format!("/api/workspaces/{}/members", fx.b_work))] {
        let (st, _) = fx.call(&fx.b_pat, m, &path, Some(json!({"user": "Tom", "role": "viewer"}))).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{m} {path} with a PAT");
    }
    // A shares her Work with B: B now sees two "Work"s, A's labelled with its owner
    // the route takes an id and answers an unknown one exactly like a known
    // one (it never confirms who has an account); names are the CLI's
    let (st_unknown, unknown) = fx
        .call(&fx.a_app, "POST", &format!("/api/workspaces/{}/members", fx.a_work), Some(json!({"user": Uuid::now_v7().to_string(), "role": "editor"})))
        .await;
    let (st_name, by_name) = fx.call(&fx.a_app, "POST", &format!("/api/workspaces/{}/members", fx.a_work), Some(json!({"user": "Aoife", "role": "editor"}))).await;
    let (st, out) = fx.call(&fx.a_app, "POST", &format!("/api/workspaces/{}/members", fx.a_work), Some(json!({"user": fx.b.to_string(), "role": "editor"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!((st_unknown, unknown.as_str(), st_name, by_name.as_str()), (st, out.as_str(), st, out.as_str()), "indistinguishable");
    let (_, list) = fx.b("GET", "/api/workspaces", None).await;
    let v: Value = serde_json::from_str(&list).unwrap();
    let names: Vec<&str> = v["workspaces"].as_array().unwrap().iter().map(|w| w["display_name"].as_str().unwrap()).collect();
    assert!(names.contains(&"Work") && names.contains(&"Work · Tom"), "{names:?}");
    // B creates her own "Family" (names are per owner)
    let (st, _) = fx.b("POST", "/api/workspaces", Some(json!({"name": "Family"}))).await;
    assert_eq!(st, StatusCode::OK);
    let audits: Vec<String> = fx.store.lock(Scope::System).audit_events(100).unwrap().into_iter().map(|e| e.event).collect();
    assert!(audits.contains(&"workspace.share".into()), "{audits:?}");
}

#[tokio::test]
async fn server_level_routes_are_the_owners() {
    let fx = fixture();
    for (m, path) in [
        ("GET", "/api/diagnostics"),
        ("GET", "/api/backups"),
        ("POST", "/api/backups"),
        ("POST", "/api/backups/reveal"),
        ("POST", "/api/export_vault"),
        ("POST", "/api/memory/sync"),
    ] {
        let (st, out) = fx.b(m, path, Some(json!({}))).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{m} {path}: {out}");
    }
    let (st, _) = fx.b("POST", &format!("/api/doc/{}/export", fx.b_doc), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "export writes to the server's disk");
    // no tenant data: version, preflight, a d2 render of the posted source
    for (m, path, body) in [
        ("GET", "/api/buildinfo", None),
        ("GET", "/api/gardeners/preflight", None),
        ("POST", "/api/render/d2", Some(json!({"source": "a -> b"}))),
    ] {
        let (st, out) = fx.b(m, path, body).await;
        assert!(st == StatusCode::OK, "{m} {path}: {st} {out}");
        fx.no_leak(path, &out);
    }
    // the owner may read diagnostics
    assert_eq!(fx.call(&fx.a_app, "GET", "/api/diagnostics", None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn admin_routes_are_the_boxs_cli() {
    let fx = fixture();
    for path in ["/admin/gardeners", "/admin/runs"] {
        // through the proxy (forwarded): refused, whatever the token
        let req = authed(Request::get(path).header("host", "localhost:7513"), &fx.b_app, "GET")
            .header("x-forwarded-for", "203.0.113.9")
            .header("taisce-admin", "admintok")
            .body(Body::empty())
            .unwrap();
        assert_eq!(fx.app.clone().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN, "{path}");
        // a token without the admin secret: refused
        let (st, _) = fx.b("GET", path, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{path}");
    }
    for path in ["/admin/garden", "/admin/gardeners/update", "/admin/policy", "/admin/living/refresh"] {
        let (st, _) = fx.b("POST", path, Some(json!({}))).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn import_and_ask_stay_in_the_callers_tenant() {
    let fx = fixture();
    let (st, out) = fx
        .b("POST", "/api/import", Some(json!({"files": [{"path": "notes/aoife-import.md", "content": "aoifeimported text\n"}]})))
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let (_, a_hits) = fx.call(&fx.a_app, "GET", "/api/search?q=aoifeimported", None).await;
    assert!(!a_hits.contains("aoifeimported"), "B's import is B's alone: {a_hits}");
    let (_, b_hits) = fx.b("GET", "/api/search?q=aoifeimported", None).await;
    assert!(b_hits.contains("aoifeimported"));
    // ask: nothing of A's can ground B's answer
    let (st, ans) = fx.b("POST", "/api/ask", Some(json!({"question": "what are the salary figures?"}))).await;
    assert_eq!(st, StatusCode::OK, "{ans}");
    let v: Value = serde_json::from_str(&ans).unwrap();
    assert!(v.get("error").is_none(), "{ans}");
    // nothing of B's matches either: no answer doc, so no synthesis is ever
    // spawned from a test
    assert!(v["doc_id"].is_null(), "{ans}");
    fx.no_leak("/api/ask", &ans);
    let s = fx.store.lock(Scope::User(fx.b));
    for d in s.list_docs().unwrap() {
        let md = taisce_store::export::export_doc(&*s, d.id).unwrap();
        assert!(!md.contains("zebrasecret"), "{} carries A's text", d.title);
    }
}

#[tokio::test]
async fn devices_and_profile_are_the_callers() {
    let fx = fixture();
    let token = "ab".repeat(32);
    let (st, out) = fx.b("POST", "/api/devices", Some(json!({"token": token, "platform": "ios", "app_version": "1"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(fx.store.lock(Scope::System).push_device(&token).unwrap().unwrap().user_id, fx.b);
    // A cannot delete B's device
    let (st, _) = fx.call(&fx.a_app, "DELETE", &format!("/api/devices/{token}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = fx.b("DELETE", &format!("/api/devices/{token}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, profile) = fx.b("GET", "/api/profile", None).await;
    assert!(profile.contains("Aoife") && !profile.contains("Tom"), "{profile}");
    let (st, _) = fx.b("POST", "/api/profile", Some(json!({"name": "Aoife M"}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, a_profile) = fx.call(&fx.a_app, "GET", "/api/profile", None).await;
    assert!(a_profile.contains("Tom") && !a_profile.contains("Aoife"), "renaming herself never renames Tom: {a_profile}");
}

#[tokio::test]
async fn unknown_api_paths_are_404() {
    let fx = fixture();
    assert_eq!(fx.b("GET", "/api/no/such/route", None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mcp_reads_never_show_a_private_doc() {
    let fx = fixture();
    let secret = fx.a_secret.to_string();
    for (tool, args) in [
        ("find_doc", json!({"query": "Salary"})),
        ("find_doc", json!({"query": "Tom"})),
        ("orient", json!({})),
        ("search", json!({"query": "zebrasecret"})),
        ("search", json!({"query": "zebrasecret", "kind": "docs"})),
        ("grep", json!({"pattern": "zebra"})),
        ("related", json!({"tags": true})),
        ("related", json!({"by": "tag", "tag": "moneytag"})),
        ("related", json!({"doc_id": fx.shared_doc.to_string()})),
        ("proposals", json!({"kind": "pending"})),
        ("proposals", json!({"kind": "mine"})),
    ] {
        let (_, out) = fx.tool(&fx.b_pat, tool, args.clone()).await;
        fx.no_leak(&format!("mcp {tool} {args}"), &out);
    }
    for (tool, args) in [
        ("read_doc", json!({"doc_id": secret})),
        ("read_doc", json!({"doc_id": secret, "mode": "outline"})),
        ("diff_since", json!({"doc_id": secret, "since_epoch": 0})),
        ("related", json!({"doc_id": secret})),
        ("related", json!({"block_id": fx.a_secret_block.to_string()})),
        ("orient", json!({"root_doc_id": secret})),
        ("search", json!({"query": "zebrasecret", "scope_doc_id": secret})),
    ] {
        let (is_err, out) = fx.tool(&fx.b_pat, tool, args.clone()).await;
        fx.no_leak_besides(&format!("mcp {tool} {args}"), &out, &[fx.a_secret, fx.a_secret_block]);
        if tool != "search" {
            assert!(is_err && out.contains("not found"), "mcp {tool} {args}: {out}");
        }
    }
    // a ^ref to A's block resolves to nothing
    let short = taisce_store::locate::short_ref(fx.a_secret_block);
    let (is_err, out) = fx.tool(&fx.b_pat, "read_doc", json!({"doc_id": fx.shared_doc.to_string(), "block": short})).await;
    assert!(is_err && out.contains("no block ends with"), "{out}");
    // the shared doc reads
    let (is_err, out) = fx.tool(&fx.b_pat, "read_doc", json!({"doc_id": fx.shared_doc.to_string()})).await;
    assert!(!is_err && out.contains("familyshared"), "{out}");
    // a connector token: same isolation
    let (_, out) = fx.tool(&fx.b_conn, "search", json!({"query": "zebrasecret"})).await;
    fx.no_leak("connector search", &out);
}

#[tokio::test]
async fn mcp_writes_answer_not_found_or_read_only() {
    let fx = fixture();
    let secret = fx.a_secret.to_string();
    for (tool, args) in [
        ("edit_doc", json!({"doc_id": secret, "old": "salary", "new": "x"})),
        ("append", json!({"doc_id": secret, "markdown": "x"})),
        ("propose_markdown", json!({"doc_id": secret, "base_epoch": 1, "markdown": "x"})),
        ("propose", json!({"doc_id": secret, "base_epoch": 1, "ops": []})),
        ("create_doc", json!({"title": "child", "parent_doc_id": secret})),
        ("doc_op", json!({"op": "rename", "doc_id": secret, "title": "x"})),
        ("doc_op", json!({"op": "delete", "doc_id": secret})),
        ("doc_op", json!({"op": "workspace", "doc_id": secret, "workspace": "Work"})),
        ("doc_op", json!({"op": "move", "doc_id": fx.b_doc.to_string(), "new_parent_id": secret})),
        ("doc_op", json!({"op": "merge", "doc_id": fx.b_doc.to_string(), "into_doc_id": secret})),
        ("add_comment", json!({"block_id": fx.a_secret_block.to_string(), "text": "hi"})),
        ("resolve", json!({"annotation_id": fx.a_ann.to_string(), "decision": "accept"})),
    ] {
        let (is_err, out) = fx.tool(&fx.b_pat, tool, args.clone()).await;
        assert!(is_err, "mcp {tool} {args} must fail: {out}");
        assert!(out.contains("not found") || out.contains("no block"), "mcp {tool} {args}: {out}");
        fx.no_leak_besides(&format!("mcp {tool}"), &out, &[fx.a_secret, fx.a_secret_block, fx.a_ann]);
    }
    // the shared doc, as a viewer: read-only
    for (tool, args) in [
        ("append", json!({"doc_id": fx.shared_doc.to_string(), "markdown": "x"})),
        ("add_comment", json!({"block_id": fx.shared_block.to_string(), "text": "hi"})),
        ("doc_op", json!({"op": "workspace", "doc_id": fx.b_doc.to_string(), "workspace": "Family"})),
    ] {
        let (is_err, out) = fx.tool(&fx.b_pat, tool, args.clone()).await;
        assert!(is_err && (out.contains("read-only") || out.contains("forbidden")), "mcp {tool} {args}: {out}");
    }
    let s = fx.store.lock(Scope::System);
    assert_eq!(s.get_doc(fx.a_secret).unwrap().title, "Salary review");
    assert_eq!(s.review_queue(Some(fx.a_secret)).unwrap().len(), 1, "B resolved nothing of A's");
}

#[tokio::test]
async fn mcp_workspace_names_resolve_within_the_callers_workspaces() {
    let fx = fixture();
    // own beats shared: with A's Work shared to B, "Work" is still B's own
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.a_work, fx.b, Role::Editor).unwrap();
    let (_, found) = fx.tool(&fx.b_pat, "find_doc", json!({"query": "work", "workspace": "Work"})).await;
    assert!(found.contains(&fx.b_doc.to_string()) && !found.contains("Work root"), "{found}");
    let (_, theirs) = fx.tool(&fx.b_pat, "find_doc", json!({"query": "work", "workspace": "Work · Tom"})).await;
    assert!(theirs.contains("Work root") && !theirs.contains(&fx.b_doc.to_string()), "{theirs}");
    let (_, by_id) = fx.tool(&fx.b_pat, "search", json!({"query": "work root text", "workspace": fx.a_work.to_string()})).await;
    assert!(by_id.contains("work root"), "{by_id}");
    // create_doc into "Work" lands in B's own
    let (is_err, made) = fx.tool(&fx.b_pat, "create_doc", json!({"title": "B note", "workspace": "Work"})).await;
    assert!(!is_err, "{made}");
    let id = made.split("doc ").nth(1).unwrap().split(' ').next().unwrap().to_string();
    assert_eq!(fx.store.lock(Scope::System).doc_workspace(Uuid::parse_str(&id).unwrap()).unwrap(), Some(fx.b_work));
    // create_missing makes the workspace under the caller
    let (is_err, out) = fx.tool(&fx.b_pat, "doc_op", json!({"op": "workspace", "doc_id": id, "workspace": "Garden", "create_missing": true})).await;
    assert!(!is_err, "{out}");
    let s = fx.store.lock(Scope::System);
    let garden = s.list_workspaces().unwrap().into_iter().find(|w| w.name == "Garden").unwrap();
    assert_eq!(garden.owner_id, Some(fx.b));
    drop(s);
    assert!(fx.store.lock(Scope::User(fx.a)).list_workspaces().unwrap().iter().all(|w| w.name != "Garden"));
    // ambiguity: a third person who can see both foreign "Work"s gets the candidates
    let c = fx.store.lock(Scope::System).auth_add_user("Ciara", now()).unwrap().id;
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.a_work, c, Role::Viewer).unwrap();
    fx.store.lock(Scope::User(fx.b)).share_workspace(fx.b_work, c, Role::Viewer).unwrap();
    let c_pat = crate::auth::create_api_token(&mut fx.store.lock(Scope::System), Some(&c.to_string()), "laptop", None, now())
        .unwrap()
        .1
        .unwrap();
    let (is_err, out) = fx.tool(&c_pat, "search", json!({"query": "work", "workspace": "work"})).await;
    assert!(is_err, "{out}");
    assert!(out.contains("Work · Tom") && out.contains("Work · Aoife"), "{out}");
    assert!(out.contains(&fx.a_work.to_string()) && out.contains(&fx.b_work.to_string()), "{out}");
}

#[tokio::test]
async fn mcp_writes_into_a_shared_workspace_land_flagged() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    // B's own private doc: green as ever
    let (_, own) = fx.tool(&fx.b_pat, "append", json!({"doc_id": fx.b_doc.to_string(), "markdown": "more of mine"})).await;
    assert!(own.starts_with("ok") && !own.contains("yellow"), "{own}");
    // the shared doc: flagged for its members, never green
    let (is_err, out) = fx.tool(&fx.b_pat, "append", json!({"doc_id": fx.shared_doc.to_string(), "markdown": "claude was here"})).await;
    assert!(!is_err && out.contains("yellow"), "{out}");
    let q = fx.store.lock(Scope::User(fx.a)).review_queue(Some(fx.shared_doc)).unwrap();
    assert_eq!(q.len(), 1, "A sees it in her queue");
    // and B's human app writes green there
    let e = fx.store.lock(Scope::System).get_doc(fx.shared_doc).unwrap().current_epoch;
    let (st, out) = fx.b("POST", "/api/propose", Some(json!({"doc_id": fx.shared_doc, "base_epoch": e, "ops": [para("human").1]}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert!(out.contains("\"green\""), "{out}");
}

#[tokio::test]
async fn a_server_request_without_identity_is_refused() {
    let fx = fixture();
    let req = Request::get("/api/docs").header("host", "localhost:7513").body(Body::empty()).unwrap();
    assert_eq!(fx.app.clone().oneshot(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    // and the extractor itself refuses in SERVER mode when no identity got through
    let st = crate::api::ApiState {
        changes: crate::changes::Feed::new(&fx.store),
        store: fx.store.clone(),
        human: fx.a_human,
        server_mode: true,
        db_path: std::env::temp_dir().join("x.db"),
        embedder: None,
        dedupe: crate::mcp::new_dedupe(),
    };
    let bare = crate::api::router(st);
    let req = Request::get("/api/docs").body(Body::empty()).unwrap();
    assert_eq!(bare.oneshot(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
}

/// The public auth routes (discovery, the AASA file, the app's callback
/// page, the OAuth endpoints without credentials) answer without a token
/// and carry nothing of anyone's docs, workspaces or names.
#[tokio::test]
async fn public_auth_routes_carry_no_tenant_data() {
    let fx = fixture();
    let raw = |method: &str, path: &str, body: Option<&str>| {
        let mut b = Request::builder().method(method).uri(path).header("host", "localhost:7513");
        if body.is_some() {
            b = b.header("content-type", "application/x-www-form-urlencoded");
        }
        b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
    };
    for (method, path, body) in [
        ("GET", "/.well-known/oauth-protected-resource", None),
        ("GET", "/.well-known/oauth-protected-resource/mcp", None),
        ("GET", "/.well-known/oauth-authorization-server", None),
        ("GET", "/.well-known/oauth-authorization-server/mcp", None),
        ("GET", "/.well-known/apple-app-site-association", None),
        ("GET", "/.well-known/openid-configuration", None),
        ("GET", "/oauth/app-callback?code=x&state=y", None),
        ("GET", "/oauth/authorize?client_id=taisce-app&redirect_uri=ie.null.taisce:/oauth/callback&response_type=code", None),
        ("POST", "/oauth/token", Some("grant_type=refresh_token&refresh_token=nope&client_id=taisce-app")),
        ("POST", "/oauth/revoke", Some("token=nope&client_id=taisce-app")),
    ] {
        let res = fx.app.clone().oneshot(raw(method, path, body)).await.unwrap();
        let status = res.status();
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{path} is public");
        let text = String::from_utf8_lossy(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).into_owned();
        fx.no_leak(path, &text);
        for name in ["Aoife", "familyshared", "Family plan", "aoifeown"] {
            assert!(!text.contains(name), "{path} names {name}");
        }
    }
}

/// Every `.route("<path>", …)` in the routers' sources, and every MCP tool,
/// must have a row in `COVERAGE` — a new route cannot ship untested.
#[test]
fn every_route_and_tool_is_covered() {
    let sources = [
        include_str!("api.rs"),
        include_str!("home.rs"),
        include_str!("inbox.rs"),
        include_str!("changes.rs"),
        include_str!("workspaces.rs"),
        include_str!("todo.rs"),
        include_str!("push.rs"),
        include_str!("admin.rs"),
        include_str!("auth/oauth.rs"),
        include_str!("auth/web.rs"),
    ];
    let re = regex::Regex::new(r#"\.route\(\s*"([^"]+)""#).unwrap();
    let covered: std::collections::HashSet<&str> = COVERAGE.iter().map(|(p, _)| *p).collect();
    let mut seen = 0;
    for src in sources {
        // the routers only: test modules build throwaway routes
        let src = src.split("#[cfg(test)]").next().unwrap();
        for c in re.captures_iter(src) {
            let path = c.get(1).unwrap().as_str();
            seen += 1;
            assert!(covered.contains(path), "route {path} has no row in isolation_tests::COVERAGE");
        }
    }
    assert!(seen >= 60, "the route scan found only {seen} routes — has the router shape changed?");
    for t in crate::mcp::KsMcp::tool_router().list_all() {
        // sharing is a human surface: no MCP tool may touch membership
        for word in ["share", "member", "user", "invite"] {
            assert!(!t.name.contains(word), "MCP tool {} looks like a sharing surface", t.name);
        }
        let key = format!("mcp:{}", t.name);
        assert!(covered.contains(key.as_str()), "MCP tool {} has no row in isolation_tests::COVERAGE", t.name);
    }
    // every test named in the table exists in this file
    let me = include_str!("isolation_tests.rs");
    for (_, test) in COVERAGE {
        let name = test.split(' ').next().unwrap();
        assert!(me.contains(&format!("async fn {name}()")) || me.contains(&format!("fn {name}()")), "COVERAGE names a missing test {name}");
    }
}

/// Review finding 5: a connector token (Claude over OAuth) on /api is the
/// agent, not the human: it cannot label, move, resolve or manage
/// workspaces there, and its gated writes into a shared workspace land
/// flagged exactly as over MCP.
#[tokio::test]
async fn connector_tokens_on_api_get_the_agent_treatment() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    let doc = fx.b_doc;
    for (m, path, body) in [
        ("PUT", format!("/api/docs/{doc}/workspace"), json!({"workspace_id": fx.family})),
        ("POST", format!("/api/doc/{doc}/move"), json!({"parent_id": fx.shared_doc})),
        ("POST", format!("/api/doc/{doc}/rename"), json!({"title": "x"})),
        ("POST", format!("/api/doc/{doc}/status"), json!({"status": "draft"})),
        ("POST", format!("/api/doc/{doc}/delete"), json!({})),
        ("POST", "/api/resolve".to_string(), json!({"annotation_id": Uuid::now_v7(), "decision": "accept"})),
        ("POST", "/api/resolve_bulk".to_string(), json!({"annotation_ids": [], "decision": "accept"})),
        ("POST", "/api/workspaces".to_string(), json!({"name": "Sneaky"})),
        ("POST", "/api/profile".to_string(), json!({"name": "Claude"})),
        ("POST", "/api/import".to_string(), json!({"files": []})),
    ] {
        let (st, out) = fx.call(&fx.b_conn, m, &path, Some(body)).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "connector {m} {path}: {out}");
    }
    // an agent's flagged write in the shared doc cannot be accepted by the
    // connector on /api, nor by an agent over MCP
    let e = fx.store.lock(Scope::User(fx.b)).get_doc(fx.shared_doc).unwrap().current_epoch;
    let (st, out) = fx
        .call(&fx.b_conn, "POST", "/api/propose", Some(json!({"doc_id": fx.shared_doc, "base_epoch": e, "ops": [para("connector text").1]})))
        .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert!(out.contains("\"yellow\"") && !out.contains("\"green\""), "the share gate flags it: {out}");
    let ann = fx.store.lock(Scope::User(fx.b)).review_queue(Some(fx.shared_doc)).unwrap().last().unwrap().annotation.id;
    let (st, _) = fx.call(&fx.b_conn, "POST", "/api/resolve", Some(json!({"annotation_id": ann, "decision": "accept"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (is_err, out) = fx.tool(&fx.b_pat, "resolve", json!({"annotation_id": ann.to_string(), "decision": "accept"})).await;
    assert!(is_err, "{out}");
    // B's own app (a human surface) still can
    let (st, out) = fx.b("POST", "/api/resolve", Some(json!({"annotation_id": ann, "decision": "accept"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    // reads stay open to the connector, scoped
    let (st, docs) = fx.call(&fx.b_conn, "GET", "/api/docs", None).await;
    assert_eq!(st, StatusCode::OK);
    fx.no_leak("connector GET /api/docs", &docs);
}

/// Probe p6 end to end: after unshare, a comment anchored to a block B once
/// saw never reads it back through /api/flags.
#[tokio::test]
async fn unshare_then_a_stale_anchor_reads_nothing_through_flags() {
    let fx = fixture();
    let remembered = fx.shared_block;
    fx.store.lock(Scope::User(fx.a)).unshare_workspace(fx.family, fx.b).unwrap();
    {
        let mut s = fx.store.lock(Scope::User(fx.a));
        let e = s.get_doc(fx.shared_doc).unwrap().current_epoch;
        s.apply(fx.shared_doc, e, fx.a_human, vec![OpInput { kind: OpKind::Replace { target: remembered, content: "zebrasecret written after unshare".into() }, source_refs: vec![] }])
            .unwrap();
    }
    let e = fx.store.lock(Scope::User(fx.b)).get_doc(fx.b_doc).unwrap().current_epoch;
    let (_, out) = fx
        .tool(&fx.b_pat, "propose", json!({"doc_id": fx.b_doc.to_string(), "base_epoch": e,
            "ops": [{"kind": {"op": "insert", "parent_id": null, "order_key": "", "block_type": "comment", "content": "probe", "refers_to": remembered.to_string()}, "source_refs": []}]}))
        .await;
    assert!(out.starts_with("parked"), "a cross-doc anchor never applies: {out}");
    let (st, flags) = fx.b("GET", "/api/flags", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!flags.contains("written after unshare"), "{flags}");
    fx.no_leak("/api/flags", &flags);
}

/// Review finding 8: agent principals are per person. Tom's and Aoife's
/// `claude-code` PATs write as two principals, an `as` label never lands on
/// someone else's principal, and attribution shown to the other says whose.
#[tokio::test]
async fn agent_principals_are_per_person() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    let a_pat = crate::auth::create_api_token(&mut fx.store.lock(Scope::System), Some(&fx.a.to_string()), "claude-code", None, now())
        .unwrap()
        .1
        .unwrap();
    let shared = fx.shared_doc.to_string();
    let (e1, o1) = fx.tool(&a_pat, "append", json!({"doc_id": shared, "markdown": "from tom's claude", "as": "claude:grimoire-task"})).await;
    let (e2, o2) = fx.tool(&fx.b_pat, "append", json!({"doc_id": shared, "markdown": "from aoife's claude", "as": "claude:grimoire-task"})).await;
    assert!(!e1 && !e2, "{o1} / {o2}");
    let (e3, o3) = fx.tool(&a_pat, "append", json!({"doc_id": shared, "markdown": "tom plain"})).await;
    let (e4, o4) = fx.tool(&fx.b_pat, "append", json!({"doc_id": shared, "markdown": "aoife plain"})).await;
    assert!(!e3 && !e4, "{o3} / {o4}");
    let ops = fx.store.lock(Scope::System).ops_for_doc_limited(fx.shared_doc, 50).unwrap();
    let by = |text: &str| {
        ops.iter()
            .find(|o| matches!(&o.kind, OpKind::Insert { content, .. } if content.contains(text)))
            .map(|o| o.principal)
            .unwrap_or_else(|| panic!("no op for {text}"))
    };
    assert_ne!(by("from tom's claude"), by("from aoife's claude"), "the same `as` label is two principals");
    assert_ne!(by("tom plain"), by("aoife plain"), "two claude-code PATs are two principals");
    // by id, B cannot write as Tom's agent
    let tom_agent = by("from tom's claude").to_string();
    let (is_err, out) = fx.tool(&fx.b_pat, "append", json!({"doc_id": shared, "markdown": "x", "as": tom_agent})).await;
    assert!(is_err, "{out}");
    // attribution Tom sees names Aoife's agent as hers
    let (_, hist) = fx.call(&fx.a_app, "GET", &format!("/api/doc/{}/history", fx.shared_doc), None).await;
    assert!(hist.contains("claude:claude-code (Aoife)"), "{hist}");
    assert!(hist.contains("\"claude:claude-code\""), "Tom's own reads plain: {hist}");
}

/// History says, for the caller, which ops are theirs: their own human
/// principal or one of their own agents (`owner_user`), never another
/// person's agent with the same label. Clients decide from it whether a
/// code block is yours to run without asking.
#[tokio::test]
async fn history_says_which_principals_are_yours() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    let a_pat = crate::auth::create_api_token(&mut fx.store.lock(Scope::System), Some(&fx.a.to_string()), "claude-code", None, now())
        .unwrap()
        .1
        .unwrap();
    let shared = fx.shared_doc.to_string();
    for (pat, md, as_) in [
        (&a_pat, "from tom's claude", Some("claude:grimoire-task")),
        (&fx.b_pat, "from aoife's claude", Some("claude:grimoire-task")),
    ] {
        let mut args = json!({"doc_id": shared, "markdown": md});
        if let Some(a) = as_ {
            args["as"] = json!(a);
        }
        let (is_err, out) = fx.tool(pat, "append", args).await;
        assert!(!is_err, "{out}");
    }
    let yours = |hist: &str, text: &str| -> bool {
        let rows: Value = serde_json::from_str(hist).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["op"]["kind"]["content"].as_str().is_some_and(|c| c.contains(text)))
            .unwrap_or_else(|| panic!("no row for {text}: {hist}"))["principal_is_yours"]
            .as_bool()
            .unwrap()
    };
    let (_, tom) = fx.call(&fx.a_app, "GET", &format!("/api/doc/{shared}/history"), None).await;
    assert!(yours(&tom, "from tom's claude"), "Tom's own agent is his");
    assert!(!yours(&tom, "from aoife's claude"), "Aoife's agent, same label, is not");
    let (_, aoife) = fx.b("GET", &format!("/api/doc/{shared}/history"), None).await;
    assert!(yours(&aoife, "from aoife's claude"));
    assert!(!yours(&aoife, "from tom's claude"));
}

/// Review nit 10: a viewer's GET of a shared workspace's (missing) To-do
/// creates nothing anywhere.
#[tokio::test]
async fn a_viewer_reading_a_shared_todo_creates_nothing() {
    let fx = fixture();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let before = fx.store.lock(Scope::System).list_docs().unwrap().len();
    let (st, out) = fx.b("GET", &format!("/api/todo?date={today}&today={today}&workspace={}", fx.family), None).await;
    // the app is refused the create (403); a browser session never tries
    // one on a GET, so it simply finds no list (404)
    let want = if fx.cookie { StatusCode::NOT_FOUND } else { StatusCode::FORBIDDEN };
    assert_eq!(st, want, "{out}");
    assert_eq!(fx.store.lock(Scope::System).list_docs().unwrap().len(), before, "no stray To-do root");
}

/// Review nit 10: unlabelling a doc whose parent the caller cannot see says
/// nothing about that parent.
#[tokio::test]
async fn unlabel_into_a_hidden_parent_is_a_generic_refusal() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    // a Family doc under A's private (unlabelled) parent; B unlabels it
    let d = {
        let mut s = fx.store.lock(Scope::User(fx.a));
        let p = s.create_doc("A private parent", None, fx.a_human).unwrap().id;
        let d = s.create_doc("Family note", Some(p), fx.a_human).unwrap().id;
        s.set_doc_workspace(d, Some(fx.family), fx.a_human).unwrap();
        d
    };
    let (st, out) = fx.b("PUT", &format!("/api/docs/{d}/workspace"), Some(json!({"workspace_id": null}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{out}");
    assert!(!out.contains("destination") && !out.contains("A private parent"), "{out}");
}

/// Round-2 N4: a GET is read-only for a connector token: GET /api/todo on a
/// shared workspace neither creates its To-do nor carries items forward.
/// (Audit of every GET handler: GET /api/todo was the only one that writes —
/// find-or-create plus carry-forward; /api/todo/due, /api/inbox,
/// /api/home/*, /api/docs and the rest only read.)
#[tokio::test]
async fn a_connectors_get_writes_nothing() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    let today = "2026-10-02";
    let n0 = fx.store.lock(Scope::System).latest_change_seq().unwrap();
    let (st, out) = fx
        .call(&fx.b_conn, "GET", &format!("/api/todo?date={today}&today={today}&utc_offset=%2B00:00&workspace={}", fx.family), None)
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{out}");
    let (st, _) = fx.call(&fx.b_conn, "GET", &format!("/api/todo?date={today}&today={today}"), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(fx.store.lock(Scope::System).latest_change_seq().unwrap(), n0, "nothing was written");
    // B's own app may still create her list on a GET
    let (st, _) = fx.b("GET", &format!("/api/todo?date={today}&today={today}&workspace={}", fx.family), None).await;
    assert_eq!(st, StatusCode::OK);
    // and once it exists the connector reads it, without carrying forward
    let n1 = fx.store.lock(Scope::System).latest_change_seq().unwrap();
    let (st, _) = fx
        .call(&fx.b_conn, "GET", &format!("/api/todo?date=2026-10-05&today=2026-10-05&workspace={}", fx.family), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(fx.store.lock(Scope::System).latest_change_seq().unwrap(), n1, "no carry-forward by a connector");
}

/// Round-2 N5: the person's own app is a pinned client_id, never whatever
/// redirect a client declares. DCR gives the app's exact registration the
/// fixed app client and refuses any other use of its scheme; a client that
/// merely claims the scheme gets no app powers; the app's pre-existing DCR
/// clients are grandfathered once so live sessions keep working.
#[tokio::test]
async fn first_party_is_a_pinned_client_not_a_declared_redirect() {
    let fx = fixture();
    let register = |uris: Value| {
        Request::post("/oauth/register")
            .header("host", "localhost:7513")
            .header("content-type", "application/json")
            .body(Body::from(json!({"client_name": "Taisce", "redirect_uris": uris}).to_string()))
            .unwrap()
    };
    let res = fx.app.clone().oneshot(register(json!([crate::auth::APP_REDIRECT_URI]))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(v["client_id"], crate::auth::FIRST_PARTY_APP_CLIENT, "the shipped app's registration maps to the fixed client");
    for uris in [json!([crate::auth::APP_REDIRECT_URI, "https://claude.ai/api/mcp/auth_callback"]), json!(["ie.null.taisce:/evil"])] {
        let res = fx.app.clone().oneshot(register(uris.clone())).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uris}");
    }
    // a client row that only DECLARES the app redirect (as any DCR client
    // could before) is not first party: no device registration, no /api writes
    let sneaky = {
        let mut s = fx.store.lock(Scope::System);
        let tok = random_token();
        let cid = "dcr_sneaky".to_string();
        s.oauth_upsert_client(&OAuthClient {
            client_id: cid.clone(),
            kind: "dcr".into(),
            client_name: "Totally the app".into(),
            redirect_uris: vec![crate::auth::APP_REDIRECT_URI.into()],
            metadata: "{}".into(),
            created_at: now(),
            refresh_at: None,
        })
        .unwrap();
        let grant = Grant { id: Uuid::now_v7(), client_id: cid, user_id: fx.b, resource: None, scope: crate::auth::SCOPE.into(), created_at: now(), revoked_at: None, revoke_why: None, credential_id: None };
        s.oauth_issue_grant(None, &grant, &hash_secret(&tok), now() + 3600, &hash_secret(&random_token()), now() + 86400).unwrap();
        tok
    };
    // (round 4: an unpinned app-redirect client is a lapsed app client — its
    // tokens are not accepted at all, so the app re-registers as taisce-app)
    let (st, _) = fx.call(&sneaky, "POST", "/api/devices", Some(json!({"token": "ab".repeat(32), "platform": "ios"}))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = fx.call(&sneaky, "POST", &format!("/api/doc/{}/rename", fx.b_doc), Some(json!({"title": "x"}))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // grandfathering (round 3): only the app's exact redirect AND a live
    // grant. Tom's iPhone and Mac (signed in, refresh tokens live) carry
    // over; a registration nobody signed in with, or a revoked one, does not;
    // and the pass never runs again
    let mut raw = SqliteStore::open_in_memory().unwrap();
    let h = raw.create_principal(PrincipalKind::Human, "Tom", None).unwrap().id;
    let tom = raw.auth_ensure_owner(h, "Tom", now()).unwrap().id;
    let client = |id: &str, uri: &str| OAuthClient {
        client_id: id.into(),
        kind: "dcr".into(),
        client_name: "Taisce".into(),
        redirect_uris: vec![uri.into()],
        metadata: "{}".into(),
        created_at: 1,
        refresh_at: None,
    };
    let grant = |raw: &mut SqliteStore, id: &str| {
        let g = Grant { id: Uuid::now_v7(), client_id: id.into(), user_id: tom, resource: None, scope: crate::auth::SCOPE.into(), created_at: now(), revoked_at: None, revoke_why: None, credential_id: None };
        raw.oauth_issue_grant(None, &g, &hash_secret(&random_token()), now() + 3600, &hash_secret(&random_token()), now() + 86400).unwrap();
        g.id
    };
    for id in ["dcr_tom_iphone", "dcr_tom_mac", "dcr_squatter", "dcr_revoked"] {
        raw.oauth_upsert_client(&client(id, crate::auth::APP_REDIRECT_URI)).unwrap();
    }
    grant(&mut raw, "dcr_tom_iphone");
    grant(&mut raw, "dcr_tom_mac");
    let gone = grant(&mut raw, "dcr_revoked");
    raw.oauth_revoke_grant(&gone.to_string(), "test", now()).unwrap();
    crate::auth::ensure_first_party(&mut raw, now(), &format!("{BASE}/oauth/app-callback")).unwrap();
    assert!(raw.oauth_is_first_party("dcr_tom_iphone").unwrap() && raw.oauth_is_first_party("dcr_tom_mac").unwrap(), "Tom's live app sessions carry over");
    assert!(!raw.oauth_is_first_party("dcr_squatter").unwrap(), "a pre-registered client nobody signed in with is not pinned");
    assert!(!raw.oauth_is_first_party("dcr_revoked").unwrap(), "a revoked grant does not pin");
    raw.oauth_upsert_client(&client("dcr_new_claim", crate::auth::APP_REDIRECT_URI)).unwrap();
    grant(&mut raw, "dcr_new_claim");
    crate::auth::ensure_first_party(&mut raw, now(), &format!("{BASE}/oauth/app-callback")).unwrap();
    assert!(!raw.oauth_is_first_party("dcr_new_claim").unwrap(), "a later claim is never grandfathered");
}

/// Round-3 S1, end to end: a block A deleted before B had access never
/// reads back for B — not the anchor id in /api/doc, not read_doc's
/// section/block by full id or ^ref, not related, not add_comment.
#[tokio::test]
async fn a_pre_access_tombstone_is_gone_on_every_block_path() {
    let fx = fixture();
    fx.store.lock(Scope::User(fx.a)).share_workspace(fx.family, fx.b, Role::Editor).unwrap();
    let (d, sid) = {
        let mut s = fx.store.lock(Scope::User(fx.a));
        let (sid, op) = para("zebrasecret TOMBSTONE salary 90k");
        let (d, _) = s.create_doc_with_ops("Plans", None, fx.a_human, vec![op, para("keep").1]).unwrap();
        s.add_comment(sid, fx.a_human, "check this number", None).unwrap();
        let e = s.get_doc(d.id).unwrap().current_epoch;
        s.apply(d.id, e, fx.a_human, vec![OpInput { kind: OpKind::Delete { target: sid }, source_refs: vec![] }]).unwrap();
        s.set_doc_workspace(d.id, Some(fx.family), fx.a_human).unwrap();
        (d.id, sid)
    };
    let (st, out) = fx.b("GET", &format!("/api/doc/{d}"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!out.contains(&sid.to_string()), "the anchor id is masked: {out}");
    let short = taisce_store::locate::short_ref(sid);
    for args in [
        json!({"doc_id": d.to_string(), "section": sid.to_string()}),
        json!({"doc_id": d.to_string(), "section": short}),
        json!({"doc_id": d.to_string(), "block": sid.to_string()}),
        json!({"doc_id": d.to_string(), "comments": true}),
    ] {
        let (_, o) = fx.tool(&fx.b_pat, "read_doc", args.clone()).await;
        // (an error may echo the id she asked about; nothing else of it)
        let echoed = args.to_string().contains(&sid.to_string());
        assert!(!o.contains("90k") && (echoed || !o.contains(&sid.to_string())), "read_doc {args}: {o}");
    }
    let (is_err, o) = fx.tool(&fx.b_pat, "related", json!({"block_id": sid.to_string()})).await;
    assert!(is_err && !o.contains("90k"), "{o}");
    let (is_err, o) = fx.tool(&fx.b_pat, "add_comment", json!({"block_id": sid.to_string(), "text": "x"})).await;
    assert!(is_err && o.contains("not found"), "{o}");
    // the history of the doc starts at her access: no pre-share ops
    let (_, hist) = fx.b("GET", &format!("/api/doc/{d}/history"), None).await;
    assert!(!hist.contains("90k"), "{hist}");
}

/// The isolation suites again, with B signed in through the web UI's session
/// cookie (and the same-origin headers on writes) instead of her app's
/// bearer: a browser session gets exactly the app's Viewer and Scope, so B
/// sees none of A's data on any /api route. (`#[tokio::test]` fns are plain
/// fns that build their own runtime on this thread, so the thread-local
/// switch reaches each fixture.)
#[test]
fn a_web_session_for_b_sees_none_of_as_data() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            B_VIA_COOKIE.with(|c| c.set(false));
        }
    }
    let _reset = Reset;
    B_VIA_COOKIE.with(|c| c.set(true));
    assert!(fixture().b_app.starts_with(COOKIE_TOKEN));
    api_reads_never_show_a_private_doc();
    api_lists_are_per_user();
    api_writes_answer_404_or_403();
    the_feed_shows_only_visible_docs_and_tells_b_to_drop_revoked_ones();
    home_inbox_and_todo_are_each_users_own();
    workspaces_and_membership_routes();
    server_level_routes_are_the_owners();
    admin_routes_are_the_boxs_cli();
    import_and_ask_stay_in_the_callers_tenant();
    unknown_api_paths_are_404();
    unshare_then_a_stale_anchor_reads_nothing_through_flags();
    a_viewer_reading_a_shared_todo_creates_nothing();
    unlabel_into_a_hidden_parent_is_a_generic_refusal();
    a_pre_access_tombstone_is_gone_on_every_block_path();
}

/// The members panel works from the web UI in SERVER mode (a session is a
/// human surface, like the app), sharing stays human-only (a connector and a
/// PAT are still refused), and a session's GETs write nothing — not even
/// GET /api/todo's find-or-create or carry-forward.
#[tokio::test]
async fn the_owner_shares_from_the_web_ui_and_a_session_get_writes_nothing() {
    let fx = fixture();
    let tom = web_session(&mut fx.store.lock(Scope::System), fx.a);
    // the owner lists and changes members of her workspace from the browser
    let (st, members) = fx.call(&tom, "GET", &format!("/api/workspaces/{}/members", fx.family), None).await;
    assert_eq!(st, StatusCode::OK, "{members}");
    assert!(members.contains("Aoife"), "{members}");
    let (st, out) = fx.call(&tom, "POST", &format!("/api/workspaces/{}/members", fx.a_work), Some(json!({"user": fx.b.to_string(), "role": "viewer"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let (st, out) = fx.call(&tom, "DELETE", &format!("/api/workspaces/{}/members/{}", fx.a_work, fx.b), None).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    // without the CSRF header the same share is refused
    let req = Request::post(format!("/api/workspaces/{}/members", fx.a_work))
        .header("host", "localhost:7513")
        .header("origin", BASE)
        .header("content-type", "application/json")
        .header("cookie", format!("{}={}", crate::auth::web::COOKIE, tom.strip_prefix(COOKIE_TOKEN).unwrap()))
        .body(Body::from(json!({"user": fx.b.to_string(), "role": "editor"}).to_string()))
        .unwrap();
    assert_eq!(fx.app.clone().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
    // B's browser is not the owner of Family: 403, as from her app
    let b = web_session(&mut fx.store.lock(Scope::System), fx.b);
    let (st, _) = fx.call(&b, "POST", &format!("/api/workspaces/{}/members", fx.family), Some(json!({"user": fx.b.to_string(), "role": "editor"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // connectors and PATs are still refused on the members routes
    let (st, _) = fx.call(&fx.b_conn, "POST", &format!("/api/workspaces/{}/members", fx.b_work), Some(json!({"user": fx.a.to_string(), "role": "viewer"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = fx.call(&fx.b_pat, "GET", &format!("/api/workspaces/{}/members", fx.b_work), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // GETs from the browser write nothing anywhere
    let today = "2026-10-02";
    let docs0 = fx.store.lock(Scope::System).list_docs().unwrap().len();
    let n0 = fx.store.lock(Scope::System).latest_change_seq().unwrap();
    for path in [
        format!("/api/todo?date={today}&today={today}&utc_offset=%2B00:00"),
        format!("/api/todo/due?today={today}"),
        "/api/docs".into(),
        "/api/inbox".into(),
        "/api/home/visit".into(),
        "/api/home/since".into(),
        "/api/profile".into(),
        "/api/workspaces".into(),
        "/api/queue".into(),
        "/api/flags".into(),
        "/api/changes?since=0".into(),
        format!("/api/doc/{}", fx.b_doc),
        format!("/api/doc/{}/history", fx.b_doc),
    ] {
        let (st, out) = fx.call(&b, "GET", &path, None).await;
        assert!(st == StatusCode::OK || (path.starts_with("/api/todo?") && st == StatusCode::NOT_FOUND), "{path}: {st} {out}");
    }
    assert_eq!(fx.store.lock(Scope::System).latest_change_seq().unwrap(), n0, "a browser session's GETs wrote");
    assert_eq!(fx.store.lock(Scope::System).list_docs().unwrap().len(), docs0, "no To-do was created by a GET");
    // a write from the browser makes the list; a later day's GET does not carry forward
    let (st, out) = fx.call(&b, "POST", "/api/todo", Some(json!({"date": today, "text": "aoife errand", "today": today, "utc_offset": "+00:00"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let n1 = fx.store.lock(Scope::System).latest_change_seq().unwrap();
    let (st, out) = fx.call(&b, "GET", "/api/todo?date=2026-10-05&today=2026-10-05&utc_offset=%2B00:00", None).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(fx.store.lock(Scope::System).latest_change_seq().unwrap(), n1, "no carry-forward by a browser GET");
}

/// The web sign-in routes: the session route names only the caller, and a
/// sign-out ends only the caller's own session.
#[tokio::test]
async fn web_session_routes_answer_only_for_the_caller() {
    let fx = fixture();
    let tom = web_session(&mut fx.store.lock(Scope::System), fx.a);
    let b = web_session(&mut fx.store.lock(Scope::System), fx.b);
    let (st, me) = fx.call(&b, "GET", "/auth/web/session", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(me.contains("Aoife") && !me.contains("Tom"), "{me}");
    fx.no_leak("/auth/web/session", &me);
    let (st, out) = fx.call(&b, "POST", "/auth/web/logout", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(fx.call(&b, "GET", "/api/docs", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(fx.call(&tom, "GET", "/api/docs", None).await.0, StatusCode::OK, "Tom's browser is still signed in");
    // begin is a bare challenge: nothing of anyone's docs
    let req = Request::post("/auth/web/begin").header("host", "localhost:7513").header("origin", BASE).body(Body::empty()).unwrap();
    let res = fx.app.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    fx.no_leak("/auth/web/begin", &String::from_utf8_lossy(&bytes));
}
