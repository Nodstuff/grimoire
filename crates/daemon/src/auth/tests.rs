//! In-process SERVER-mode tests: the real routers (auth + /mcp + /api)
//! under `require_auth`, driven end to end with webauthn-rs's software
//! passkey, plus a mock CIMD host.

use super::*;
use axum::Router;
use axum::http::Request as HttpRequest;
use grimoire_store::{BlockStore, PrincipalKind};
use serde_json::{Value, json};
use tower::ServiceExt;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

const BASE: &str = "http://localhost:7512";
const REDIRECT: &str = "http://127.0.0.1:33418/callback";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

struct H {
    app: Router,
    st: AuthState,
    owner: uuid::Uuid,
    passkey: WebauthnAuthenticator<SoftPasskey>,
}

fn harness_with(cfg: AuthConfig) -> H {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let human = store.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
    let agent = store.create_principal(PrincipalKind::Agent, "claude", None).unwrap().id;
    let owner = store.auth_ensure_owner(human, "tom", now()).unwrap().id;
    let store = Arc::new(Mutex::new(store));
    let dir = std::env::temp_dir().join(format!("grimoire-auth-test-{}", uuid::Uuid::now_v7()));
    let hot = crate::hot::HotState::new(dir.clone());
    let dedupe = crate::mcp::new_dedupe();
    let st = AuthState::new(cfg, store.clone()).unwrap();
    let hosts = vec![st.cfg.authority(), st.cfg.rp_id.clone()];
    let app = crate::mcp::router_with_hosts(store.clone(), agent, hot.clone(), dedupe.clone(), None, Some(hosts))
        .merge(crate::api::router(crate::api::ApiState {
            changes: crate::changes::Feed::new(&store),
            store: store.clone(),
            human,
            hot,
            runtime: crate::fed::Runtime::default(),
            db_path: dir.join("ks.db"),
            node_id: None,
            embedder: None,
            dedupe,
        }))
        .merge(router(st.clone()))
        .fallback(|| async { "<!doctype html>ui" })
        .layer(axum::middleware::from_fn_with_state(st.clone(), require_auth));
    H { app, st, owner, passkey: WebauthnAuthenticator::new(SoftPasskey::new(true)) }
}

fn harness() -> H {
    harness_with(AuthConfig::from_public_url(BASE).unwrap())
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not json: {}", self.body))
    }
}

async fn send(app: &Router, req: HttpRequest<Body>) -> Res {
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
    Res { status, headers, body: String::from_utf8_lossy(&body).into_owned() }
}

fn get(path: &str) -> HttpRequest<Body> {
    HttpRequest::get(path).header("host", "localhost:7512").body(Body::empty()).unwrap()
}

fn post_json(path: &str, v: Value) -> HttpRequest<Body> {
    HttpRequest::post(path)
        .header("host", "localhost:7512")
        .header("origin", BASE)
        .header("content-type", "application/json")
        .body(Body::from(v.to_string()))
        .unwrap()
}

fn post_form(path: &str, pairs: &[(&str, &str)]) -> HttpRequest<Body> {
    let body: String = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
    HttpRequest::post(path)
        .header("host", "localhost:7512")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

impl H {
    /// Mint an enrollment link and register the soft passkey through it.
    async fn enroll(&mut self) -> Res {
        let t = random_token();
        let (h, owner) = (hash_secret(&t), self.owner);
        self.st.store.lock().unwrap().auth_add_enrollment(&h, owner, now() + ENROLL_TTL).unwrap();
        let page = send(&self.app, get(&format!("/auth/enroll?t={t}"))).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        let begin = send(&self.app, post_json("/auth/enroll/begin", json!({"t": t, "label": "soft"}))).await;
        assert_eq!(begin.status, StatusCode::OK, "{}", begin.body);
        let b = begin.json();
        let ccr: CreationChallengeResponse = serde_json::from_value(b["options"].clone()).unwrap();
        let cred = self.passkey.do_registration(Url::parse(BASE).unwrap(), ccr).unwrap();
        send(&self.app, post_json("/auth/enroll/finish", json!({"ceremony": b["ceremony"], "credential": cred}))).await
    }

    async fn register(&self, name: &str, redirect: &str) -> String {
        let r = send(&self.app, post_json("/oauth/register", json!({"client_name": name, "redirect_uris": [redirect]}))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.json()["client_id"].as_str().unwrap().to_string()
    }

    /// GET /oauth/authorize; on the login page, the pending request id.
    async fn authorize(&self, query: &str) -> (Res, Option<String>) {
        let r = send(&self.app, get(&format!("/oauth/authorize?{query}"))).await;
        let req = regex::Regex::new(r#"data-req="([^"]+)""#)
            .unwrap()
            .captures(&r.body)
            .map(|c| c[1].to_string());
        (r, req)
    }

    /// Sign in with the soft passkey; the redirect URL carrying the code.
    async fn sign_in(&mut self, req: &str) -> String {
        let begin = send(&self.app, post_json("/oauth/authorize/begin", json!({"req": req}))).await;
        assert_eq!(begin.status, StatusCode::OK, "{}", begin.body);
        let rcr: RequestChallengeResponse = serde_json::from_value(begin.json()).unwrap();
        let cred = self.passkey.do_authentication(Url::parse(BASE).unwrap(), rcr).unwrap();
        let fin = send(&self.app, post_json("/oauth/authorize/finish", json!({"req": req, "credential": cred}))).await;
        assert_eq!(fin.status, StatusCode::OK, "{}", fin.body);
        fin.json()["redirect"].as_str().unwrap().to_string()
    }

    /// Enroll, register a DCR client and run the code flow to its redirect.
    async fn code_for(&mut self, client: &str) -> String {
        let q = format!(
            "response_type=code&client_id={client}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=S256&state=xyz&resource={BASE}/mcp"
        );
        let (page, req) = self.authorize(&q).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        let redirect = self.sign_in(&req.unwrap()).await;
        let u = Url::parse(&redirect).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["state"], "xyz");
        assert_eq!(q["iss"], BASE);
        assert!(redirect.starts_with(REDIRECT));
        q["code"].clone()
    }

    async fn exchange(&self, client: &str, code: &str, verifier: &str) -> Res {
        send(
            &self.app,
            post_form(
                "/oauth/token",
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", REDIRECT),
                    ("client_id", client),
                    ("code_verifier", verifier),
                    ("resource", &format!("{BASE}/mcp")),
                ],
            ),
        )
        .await
    }

    async fn refresh(&self, client: &str, rt: &str) -> Res {
        send(&self.app, post_form("/oauth/token", &[("grant_type", "refresh_token"), ("refresh_token", rt), ("client_id", client)]))
            .await
    }

    /// Full flow to a token pair.
    async fn tokens(&mut self) -> (String, String, String) {
        assert!(self.enroll().await.status.is_success());
        let client = self.register("Claude", REDIRECT).await;
        let code = self.code_for(&client).await;
        let t = self.exchange(&client, &code, VERIFIER).await;
        assert_eq!(t.status, StatusCode::OK, "{}", t.body);
        let j = t.json();
        (client, j["access_token"].as_str().unwrap().into(), j["refresh_token"].as_str().unwrap().into())
    }
}

fn mcp(body: Value, bearer: Option<&str>, session: Option<&str>) -> HttpRequest<Body> {
    let mut b = HttpRequest::post("/mcp")
        .header("host", "localhost:7512")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    if let Some(s) = session {
        b = b.header("mcp-session-id", s);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// The JSON-RPC message in a JSON or SSE response body.
fn rpc_body(body: &str) -> Value {
    if let Ok(v) = serde_json::from_str(body) {
        return v;
    }
    body.lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .last()
        .unwrap_or_else(|| panic!("no JSON-RPC message in {body}"))
}

async fn mcp_tools(app: &Router, token: &str) -> Res {
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
    let r = send(app, mcp(init, Some(token), None)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let session = r.headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    send(app, mcp(note, Some(token), session.as_deref())).await;
    send(app, mcp(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), Some(token), session.as_deref())).await
}

// ---- metadata ----

#[tokio::test]
async fn metadata_documents() {
    let h = harness();
    let prm = send(&h.app, get("/.well-known/oauth-protected-resource/mcp")).await;
    assert_eq!(prm.status, StatusCode::OK);
    let j = prm.json();
    assert_eq!(j["resource"], format!("{BASE}/mcp"));
    assert_eq!(j["authorization_servers"], json!([BASE]));
    let root = send(&h.app, get("/.well-known/oauth-protected-resource")).await.json();
    assert_eq!(root["resource"], BASE);
    let asm = send(&h.app, get("/.well-known/oauth-authorization-server")).await;
    assert_eq!(asm.headers["access-control-allow-origin"], "*");
    let a = asm.json();
    assert_eq!(a["issuer"], BASE);
    assert_eq!(a["authorization_endpoint"], format!("{BASE}/oauth/authorize"));
    assert_eq!(a["token_endpoint"], format!("{BASE}/oauth/token"));
    assert_eq!(a["registration_endpoint"], format!("{BASE}/oauth/register"));
    assert_eq!(a["code_challenge_methods_supported"], json!(["S256"]));
    assert!(a["token_endpoint_auth_methods_supported"].as_array().unwrap().contains(&json!("none")));
    assert_eq!(a["grant_types_supported"], json!(["authorization_code", "refresh_token"]));
    assert_eq!(a["response_types_supported"], json!(["code"]));
    assert_eq!(a["client_id_metadata_document_supported"], true);
    assert_eq!(send(&h.app, get("/healthz")).await.body, "ok");
}

// ---- 401s ----

#[tokio::test]
async fn unauthenticated_requests_get_401_with_resource_metadata() {
    let h = harness();
    let r = send(&h.app, mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), None, None)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        r.headers["www-authenticate"],
        format!("Bearer resource_metadata=\"{BASE}/.well-known/oauth-protected-resource/mcp\"")
    );
    let r = send(&h.app, get("/api/docs")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        r.headers["www-authenticate"],
        format!("Bearer resource_metadata=\"{BASE}/.well-known/oauth-protected-resource\"")
    );
    let r = send(&h.app, mcp(json!({}), Some("not-a-token"), None)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.headers["www-authenticate"].to_str().unwrap().contains("error=\"invalid_token\""));
    // the static UI is not data
    assert_eq!(send(&h.app, get("/some/page")).await.status, StatusCode::OK);
}

/// What LOCAL mode trusts — a loopback Host, a loopback peer — buys nothing
/// in SERVER mode: the reverse proxy makes every request look like that.
#[tokio::test]
async fn server_mode_rejects_unauthenticated_loopback() {
    let h = harness();
    let mut req = HttpRequest::get("/api/docs").header("host", "127.0.0.1:7425").body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo::<std::net::SocketAddr>("127.0.0.1:50000".parse().unwrap()));
    assert_eq!(send(&h.app, req).await.status, StatusCode::UNAUTHORIZED);
    let req = HttpRequest::post("/api/propose").header("host", "localhost").body(Body::empty()).unwrap();
    assert_eq!(send(&h.app, req).await.status, StatusCode::UNAUTHORIZED);
    let ws = HttpRequest::get("/ws/hot/x").header("host", "127.0.0.1").body(Body::empty()).unwrap();
    assert_eq!(send(&h.app, ws).await.status, StatusCode::UNAUTHORIZED);
    // /admin through the proxy (forwarded headers) is refused outright
    let adm = HttpRequest::get("/admin/runs")
        .header("host", "localhost:7512")
        .header("x-forwarded-for", "203.0.113.9")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&h.app, adm).await.status, StatusCode::FORBIDDEN);
}

// ---- DCR ----

#[tokio::test]
async fn dynamic_client_registration() {
    let h = harness();
    let r = send(
        &h.app,
        post_json(
            "/oauth/register",
            json!({"client_name": "Claude", "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
                   "token_endpoint_auth_method": "client_secret_basic", "grant_types": ["authorization_code", "refresh_token"]}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let j = r.json();
    assert!(j["client_id"].as_str().unwrap().starts_with("dcr_"));
    assert_eq!(j["token_endpoint_auth_method"], "none");
    assert_eq!(j["client_name"], "Claude");
    for bad in [
        json!({"redirect_uris": ["https://evil.example/cb"]}),
        json!({"redirect_uris": []}),
        json!({"client_name": "x"}),
        json!({"redirect_uris": ["http://127.0.0.1:1/cb"], "grant_types": ["client_credentials"]}),
    ] {
        let r = send(&h.app, post_json("/oauth/register", bad.clone())).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

// ---- the whole flow ----

#[tokio::test]
async fn passkey_code_flow_tokens_and_mcp_tools_list() {
    let mut h = harness();
    let (client, access, refresh) = h.tokens().await;
    let r = mcp_tools(&h.app, &access).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let tools = rpc_body(&r.body);
    let names: Vec<_> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"find_doc"), "{names:?}");
    // /api with the bearer works too (as the owner human)
    let api = HttpRequest::get("/api/docs")
        .header("host", "localhost:7512")
        .header("authorization", format!("Bearer {access}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&h.app, api).await.status, StatusCode::OK);
    // rotation: new pair; the old refresh token is then a reuse → family revoked
    let r1 = h.refresh(&client, &refresh).await;
    assert_eq!(r1.status, StatusCode::OK, "{}", r1.body);
    let j = r1.json();
    let (access2, refresh2) = (j["access_token"].as_str().unwrap().to_string(), j["refresh_token"].as_str().unwrap().to_string());
    assert_ne!(refresh2, refresh);
    assert_eq!(mcp_tools(&h.app, &access2).await.status, StatusCode::OK);
    let reuse = h.refresh(&client, &refresh).await;
    assert_eq!(reuse.status, StatusCode::BAD_REQUEST);
    assert_eq!(reuse.json()["error"], "invalid_grant");
    // the whole family is dead: the rotated-in refresh and the live access token
    assert_eq!(h.refresh(&client, &refresh2).await.json()["error"], "invalid_grant");
    let r = send(&h.app, mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), Some(&access2), None)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_connector_writes_as_its_own_principal() {
    let mut h = harness();
    let (_, access, _) = h.tokens().await;
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
    let r = send(&h.app, mcp(init, Some(&access), None)).await;
    let session = r.headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    send(&h.app, mcp(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), Some(&access), session.as_deref())).await;
    let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_doc","arguments":{"title":"From phone"}}});
    let r = send(&h.app, mcp(call, Some(&access), session.as_deref())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let names: Vec<String> =
        h.st.store.lock().unwrap().list_principals().unwrap().into_iter().map(|p| p.display_name).collect();
    assert!(names.contains(&"claude:claude".to_string()), "{names:?}");
}

#[tokio::test]
async fn pkce_failures() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let client = h.register("Claude Code", REDIRECT).await;
    // missing / plain challenge: redirected back with invalid_request
    for q in [
        format!("response_type=code&client_id={client}&redirect_uri={REDIRECT}&state=s"),
        format!("response_type=code&client_id={client}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=plain&state=s"),
    ] {
        let (r, req) = h.authorize(&q).await;
        assert_eq!(r.status, StatusCode::FOUND, "{q}");
        assert!(req.is_none());
        let loc = r.headers["location"].to_str().unwrap();
        assert!(loc.starts_with(REDIRECT) && loc.contains("error=invalid_request") && loc.contains("state=s"), "{loc}");
    }
    // wrong verifier: invalid_grant, and the code is spent
    let code = h.code_for(&client).await;
    let bad = h.exchange(&client, &code, &"a".repeat(43)).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.json()["error"], "invalid_grant");
    assert_eq!(h.exchange(&client, &code, VERIFIER).await.json()["error"], "invalid_grant");
}

#[tokio::test]
async fn code_is_single_use_and_replay_revokes() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let client = h.register("Claude", REDIRECT).await;
    let code = h.code_for(&client).await;
    let ok = h.exchange(&client, &code, VERIFIER).await;
    assert_eq!(ok.status, StatusCode::OK);
    let access = ok.json()["access_token"].as_str().unwrap().to_string();
    let replay = h.exchange(&client, &code, VERIFIER).await;
    assert_eq!(replay.json()["error"], "invalid_grant");
    // the replay killed the grant the first exchange produced
    assert_eq!(send(&h.app, mcp(json!({}), Some(&access), None)).await.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn redirect_mismatch_is_a_page_not_a_redirect() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let client = h.register("Claude", REDIRECT).await;
    for (redirect, status) in [
        ("https://evil.example/cb", StatusCode::BAD_REQUEST),
        ("http://127.0.0.1:33418/other", StatusCode::BAD_REQUEST),
    ] {
        let q = format!(
            "response_type=code&client_id={client}&redirect_uri={redirect}&code_challenge={CHALLENGE}&code_challenge_method=S256"
        );
        let (r, req) = h.authorize(&q).await;
        assert_eq!(r.status, status, "{redirect}");
        assert!(r.headers.get("location").is_none());
        assert!(req.is_none());
    }
    // loopback may change port (RFC 8252)
    let q = format!(
        "response_type=code&client_id={client}&redirect_uri=http://127.0.0.1:9999/callback&code_challenge={CHALLENGE}&code_challenge_method=S256"
    );
    assert!(h.authorize(&q).await.1.is_some());
    // unknown client
    let (r, _) = h.authorize("response_type=code&client_id=dcr_nope").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // and the code is bound to the redirect it was issued for
    let code = h.code_for(&client).await;
    let r = send(
        &h.app,
        post_form(
            "/oauth/token",
            &[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", "http://127.0.0.1:1/x"), ("client_id", &client), ("code_verifier", VERIFIER)],
        ),
    )
    .await;
    assert_eq!(r.json()["error"], "invalid_grant");
}

#[tokio::test]
async fn wrong_resource_is_invalid_target() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let client = h.register("Claude", REDIRECT).await;
    let q = format!(
        "response_type=code&client_id={client}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=S256&resource=https://other.example/mcp"
    );
    let (r, _) = h.authorize(&q).await;
    assert!(r.headers["location"].to_str().unwrap().contains("error=invalid_target"));
}

/// Expiry is enforced by the reading query itself: the tokens issued by a
/// real flow die at their TTL (the store takes the clock as an argument).
#[tokio::test]
async fn expired_tokens_are_rejected() {
    let mut h = harness();
    let (_, access, refresh) = h.tokens().await;
    let s = &mut *h.st.store.lock().unwrap();
    let ah = hash_secret(&access);
    assert!(s.oauth_access_grant(&ah, now() + 60).unwrap().is_some());
    assert!(s.oauth_access_grant(&ah, now() + ACCESS_TTL + 1).unwrap().is_none());
    let later = now() + REFRESH_TTL + 10;
    let out = s.oauth_rotate_refresh(&hash_secret(&refresh), later, "x", later + 1, "y", later + 2).unwrap();
    assert_eq!(out, grimoire_store::auth::RefreshOutcome::Invalid);
}

#[tokio::test]
async fn enrollment_link_is_single_use() {
    let mut h = harness();
    let t = random_token();
    let (hsh, owner) = (hash_secret(&t), h.owner);
    h.st.store.lock().unwrap().auth_add_enrollment(&hsh, owner, now() + ENROLL_TTL).unwrap();
    let begin = send(&h.app, post_json("/auth/enroll/begin", json!({"t": t}))).await.json();
    let ccr: CreationChallengeResponse = serde_json::from_value(begin["options"].clone()).unwrap();
    let cred = h.passkey.do_registration(Url::parse(BASE).unwrap(), ccr).unwrap();
    let fin = send(&h.app, post_json("/auth/enroll/finish", json!({"ceremony": begin["ceremony"], "credential": cred}))).await;
    assert_eq!(fin.status, StatusCode::OK, "{}", fin.body);
    assert_eq!(send(&h.app, get(&format!("/auth/enroll?t={t}"))).await.status, StatusCode::GONE);
    let again = send(&h.app, post_json("/auth/enroll/begin", json!({"t": t}))).await;
    assert_eq!(again.status, StatusCode::GONE);
    // a cross-origin POST is refused before any ceremony
    let mut x = post_json("/oauth/authorize/begin", json!({"req": "x"}));
    x.headers_mut().insert("origin", HeaderValue::from_static("https://evil.example"));
    assert_eq!(send(&h.app, x).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_second_passkey_signs_in_too() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    // a second device: its own soft passkey, its own link
    let first = std::mem::replace(&mut h.passkey, WebauthnAuthenticator::new(SoftPasskey::new(true)));
    assert!(h.enroll().await.status.is_success());
    assert_eq!(h.st.store.lock().unwrap().auth_credentials(Some(h.owner)).unwrap().len(), 2);
    let client = h.register("Claude", REDIRECT).await;
    let _ = h.code_for(&client).await;
    h.passkey = first;
    let _ = h.code_for(&client).await;
}

// ---- CIMD ----

async fn serve_doc(doc: impl Fn(String) -> String + Send + Sync + 'static) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let id = format!("{base}/client.json");
    let body = doc(id.clone());
    let app = Router::new().route("/client.json", axum::routing::get(move || async move { body }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    id
}

#[tokio::test]
async fn cimd_client_is_fetched_validated_and_cached() {
    let mut cfg = AuthConfig::from_public_url(BASE).unwrap();
    cfg.cimd_allow_insecure = true;
    let mut h = harness_with(cfg);
    assert!(h.enroll().await.status.is_success());
    let id = serve_doc(|id| json!({"client_id": id, "client_name": "Doc Client", "redirect_uris": [REDIRECT]}).to_string()).await;
    let q = format!(
        "response_type=code&client_id={id}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=S256"
    );
    let (r, req) = h.authorize(&q).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body.contains("Doc Client"));
    let c = h.st.store.lock().unwrap().oauth_client(&id).unwrap().unwrap();
    assert_eq!((c.kind.as_str(), c.redirect_uris.clone()), ("cimd", vec![REDIRECT.to_string()]));
    // a redirect the document does not list
    let q2 = q.replace("33418/callback", "33418/elsewhere");
    assert_eq!(h.authorize(&q2).await.0.status, StatusCode::BAD_REQUEST);
    // the code flow works with the document client
    let redirect = h.sign_in(&req.unwrap()).await;
    let code = Url::parse(&redirect).unwrap().query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
    assert_eq!(h.exchange(&id, &code, VERIFIER).await.status, StatusCode::OK);
}

#[tokio::test]
async fn cimd_document_must_name_itself() {
    let mut cfg = AuthConfig::from_public_url(BASE).unwrap();
    cfg.cimd_allow_insecure = true;
    let mut h = harness_with(cfg);
    assert!(h.enroll().await.status.is_success());
    let id = serve_doc(|_| json!({"client_id": "https://someone-else.example/c.json", "redirect_uris": [REDIRECT]}).to_string()).await;
    let q = format!("response_type=code&client_id={id}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=S256");
    let (r, _) = h.authorize(&q).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("does not match"), "{}", r.body);
}

#[tokio::test]
async fn cimd_refuses_loopback_documents_in_production() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let id = serve_doc(|id| json!({"client_id": id, "redirect_uris": [REDIRECT]}).to_string()).await;
    let q = format!("response_type=code&client_id={id}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=S256");
    let (r, _) = h.authorize(&q).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("https"), "{}", r.body);
}

// ---- pure pieces ----

#[test]
fn public_url_parsing() {
    let c = AuthConfig::from_public_url("https://taisce.example/").unwrap();
    assert_eq!((c.base.as_str(), c.rp_id.as_str()), ("https://taisce.example", "taisce.example"));
    assert_eq!(c.mcp_url(), "https://taisce.example/mcp");
    assert!(c.resource_ok("https://taisce.example/mcp/") && c.resource_ok("https://taisce.example"));
    assert!(!c.resource_ok("https://taisce.example/api"));
    assert!(AuthConfig::from_public_url("http://taisce.example").is_err());
    assert!(AuthConfig::from_public_url("https://taisce.example/prefix").is_err());
    assert!(AuthConfig::from_public_url("http://localhost:7512").is_ok());
}

#[test]
fn connector_principals() {
    assert_eq!(client_principal("https://claude.ai/oauth/mcp-client.json", "Claude"), "claude:claude-ai");
    assert_eq!(client_principal("dcr_abc", "Claude Code"), "claude:claude-code");
    assert_eq!(client_principal("dcr_abc", "claude.ai"), "claude:claude-ai");
    assert_eq!(client_principal("dcr_abc123", "!!!"), "claude:oauth-dcrabc12");
}

#[test]
fn path_classes() {
    use axum::http::Method;
    assert!(is_public("/.well-known/oauth-authorization-server") && is_public("/oauth/token") && is_public("/healthz"));
    assert!(!is_public("/api/docs") && !is_public("/oauthx"));
    assert!(needs_token(&Method::GET, "/mcp") && needs_token(&Method::GET, "/api") && needs_token(&Method::GET, "/ws/hot/1"));
    assert!(!needs_token(&Method::GET, "/assets/x.js") && !needs_token(&Method::GET, "/apiary"));
    assert!(needs_token(&Method::POST, "/anything"));
}
