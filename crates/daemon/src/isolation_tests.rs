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
];

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
    };
    s.oauth_issue_grant(None, &grant, &hash_secret(&access), now() + 3600, &hash_secret(&random_token()), now() + 86400)
        .unwrap();
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
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:7513")
            .header("authorization", format!("Bearer {token}"));
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
    let req = Request::get("/api/changes/stream?since=0")
        .header("host", "localhost:7513")
        .header("authorization", format!("Bearer {}", fx.b_app))
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
        assert_eq!(st, StatusCode::OK, "{path}: {body}");
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
    let (st, out) = fx.call(&fx.a_app, "POST", &format!("/api/workspaces/{}/members", fx.a_work), Some(json!({"user": "Aoife", "role": "editor"}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
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
        let req = Request::get(path)
            .header("host", "localhost:7513")
            .header("x-forwarded-for", "203.0.113.9")
            .header("authorization", format!("Bearer {}", fx.b_app))
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
