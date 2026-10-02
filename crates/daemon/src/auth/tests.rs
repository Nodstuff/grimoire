//! In-process SERVER-mode tests: the real routers (auth + /mcp + /api)
//! under `require_auth`, driven end to end with webauthn-rs's software
//! passkey, plus a mock CIMD host.

use super::*;
use axum::Router;
use axum::http::Request as HttpRequest;
use taisce_store::{BlockStore, PrincipalKind};
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
    let store = taisce_store::SharedStore::new(store);
    let dir = std::env::temp_dir().join(format!("taisce-auth-test-{}", uuid::Uuid::now_v7()));
    let dedupe = crate::mcp::new_dedupe();
    let st = AuthState::new(cfg, store.clone()).unwrap();
    let hosts = vec![st.cfg.authority(), st.cfg.rp_id.clone()];
    let app = crate::mcp::router_with_hosts(store.clone(), agent, dedupe.clone(), None, Some(hosts))
        .merge(crate::api::router(crate::api::ApiState {
            changes: crate::changes::Feed::new(&store),
            store: store.clone(),
            human,
            server_mode: false,
            db_path: dir.join("ks.db"),
            embedder: None,
            dedupe,
        }))
        .merge(crate::push::router(crate::push::DevicesState { store: store.clone(), default_env: "production".into() }))
        .merge(router(st.clone()))
        .fallback(|| async { "<!doctype html>ui" })
        .layer(axum::middleware::from_fn_with_state(st.clone(), require_auth))
        .layer(axum::middleware::from_fn(web::security_headers))
        .layer(axum::middleware::from_fn(crate::legacy::rename_headers));
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
        self.st.store.lock(taisce_store::Scope::System).auth_add_enrollment(&h, owner, now() + ENROLL_TTL).unwrap();
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
        self.code_for_at(client, REDIRECT).await
    }

    async fn code_for_at(&mut self, client: &str, redirect: &str) -> String {
        let q = format!(
            "response_type=code&client_id={client}&redirect_uri={redirect}&code_challenge={CHALLENGE}&code_challenge_method=S256&state=xyz&resource={BASE}/mcp"
        );
        let (page, req) = self.authorize(&q).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        let back = self.sign_in(&req.unwrap()).await;
        let u = Url::parse(&back).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["state"], "xyz");
        assert_eq!(q["iss"], BASE);
        assert!(back.starts_with(redirect));
        q["code"].clone()
    }

    async fn exchange(&self, client: &str, code: &str, verifier: &str) -> Res {
        self.exchange_at(client, code, verifier, REDIRECT).await
    }

    async fn exchange_at(&self, client: &str, code: &str, verifier: &str, redirect: &str) -> Res {
        send(
            &self.app,
            post_form(
                "/oauth/token",
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", redirect),
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

/// Claude Code's SDK probes more discovery URLs than claude.ai does; every one
/// must answer JSON (the suffixed AS document, or a JSON 404), never the web
/// UI's HTML, or its sign-in fails with "Unrecognized token '<'".
#[tokio::test]
async fn every_well_known_path_answers_json() {
    let h = harness();
    let sfx = send(&h.app, get("/.well-known/oauth-authorization-server/mcp")).await;
    assert_eq!(sfx.status, StatusCode::OK);
    assert_eq!(sfx.json()["issuer"], BASE);
    for p in ["/.well-known/openid-configuration", "/.well-known/openid-configuration/mcp", "/.well-known/anything/else"] {
        let r = send(&h.app, get(p)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{p}");
        assert!(r.headers["content-type"].to_str().unwrap().starts_with("application/json"), "{p}");
    }
}

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
    let ws = HttpRequest::get("/ws/x").header("host", "127.0.0.1").body(Body::empty()).unwrap();
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
                   "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"]}),
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
    // a confidential client is refused plainly, not silently downgraded
    for m in ["client_secret_basic", "client_secret_post", "private_key_jwt"] {
        let r = send(
            &h.app,
            post_json("/oauth/register", json!({"redirect_uris": [REDIRECT], "token_endpoint_auth_method": m})),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{m}");
        let j = r.json();
        assert_eq!(j["error"], "invalid_client_metadata");
        assert!(j["error_description"].as_str().unwrap().contains(m), "{j}");
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
    // rotation: a new pair each time
    let r1 = h.refresh(&client, &refresh).await;
    assert_eq!(r1.status, StatusCode::OK, "{}", r1.body);
    let j = r1.json();
    let (access2, refresh2) = (j["access_token"].as_str().unwrap().to_string(), j["refresh_token"].as_str().unwrap().to_string());
    assert_ne!(refresh2, refresh);
    assert_eq!(mcp_tools(&h.app, &access2).await.status, StatusCode::OK);
    // once the successor refresh token has itself been used, presenting the
    // old one is theft, grace window or not
    let r2 = h.refresh(&client, &refresh2).await;
    assert_eq!(r2.status, StatusCode::OK);
    let refresh2 = r2.json()["refresh_token"].as_str().unwrap().to_string();
    let reuse = h.refresh(&client, &refresh).await;
    assert_eq!(reuse.status, StatusCode::BAD_REQUEST);
    assert_eq!(reuse.json()["error"], "invalid_grant");
    // the whole family is dead: the rotated-in refresh and the live access token
    assert_eq!(h.refresh(&client, &refresh2).await.json()["error"], "invalid_grant");
    let r = send(&h.app, mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), Some(&access2), None)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

/// A client that lost the refresh response retries with the old token at
/// once: it gets a working pair, the grant survives, the lost pair is dead.
#[tokio::test]
async fn immediate_refresh_retry_reissues() {
    let mut h = harness();
    let (client, _, refresh) = h.tokens().await;
    let lost = h.refresh(&client, &refresh).await.json();
    let retry = h.refresh(&client, &refresh).await;
    assert_eq!(retry.status, StatusCode::OK, "{}", retry.body);
    let j = retry.json();
    let access = j["access_token"].as_str().unwrap();
    assert_ne!(access, lost["access_token"].as_str().unwrap());
    assert_eq!(mcp_tools(&h.app, access).await.status, StatusCode::OK);
    let lost_access = lost["access_token"].as_str().unwrap();
    assert_eq!(send(&h.app, mcp(json!({}), Some(lost_access), None)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(h.refresh(&client, lost["refresh_token"].as_str().unwrap()).await.json()["error"], "invalid_grant");
    assert_eq!(h.refresh(&client, j["refresh_token"].as_str().unwrap()).await.status, StatusCode::OK);
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
        h.st.store.lock(taisce_store::Scope::System).list_principals().unwrap().into_iter().map(|p| p.display_name).collect();
    assert!(names.contains(&"claude:claude".to_string()), "{names:?}");
}

// ---- identity pinned to the token ----

const APP_REDIRECT: &str = "ie.null.taisce:/oauth/callback";

impl H {
    /// An access token for another client, the passkey already enrolled.
    async fn token_for(&mut self, name: &str, redirect: &str) -> String {
        let client = self.register(name, redirect).await;
        let code = self.code_for_at(&client, redirect).await;
        let t = self.exchange_at(&client, &code, VERIFIER, redirect).await;
        assert_eq!(t.status, StatusCode::OK, "{}", t.body);
        t.json()["access_token"].as_str().unwrap().to_string()
    }

    fn doc(&self, title: &str) -> uuid::Uuid {
        let human = self.st.store.lock(taisce_store::Scope::System).auth_owner().unwrap().unwrap().principal_id;
        self.st.store.lock(taisce_store::Scope::System).create_doc(title, None, human).unwrap().id
    }

    /// Display name of the principal behind the doc's newest op.
    fn last_writer(&self, doc: uuid::Uuid) -> String {
        let s = self.st.store.lock(taisce_store::Scope::System);
        let op = s.ops_for_doc_limited(doc, 1).unwrap().remove(0);
        s.get_principal(op.principal).unwrap().display_name
    }
}

/// One stateless MCP tools/call on `path` with extra headers: (is_error, text).
async fn tool(app: &Router, token: &str, path: &str, headers: &[(&str, &str)], name: &str, args: Value) -> (bool, String) {
    let at = |body: Value, session: Option<&str>| {
        let mut req = mcp(body, Some(token), session);
        *req.uri_mut() = path.parse().unwrap();
        for (k, v) in headers {
            req.headers_mut().insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        req
    };
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
    let r = send(app, at(init, None)).await;
    let session = r.headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    send(app, at(json!({"jsonrpc":"2.0","method":"notifications/initialized"}), session.as_deref())).await;
    let body = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name": name, "arguments": args}});
    let r = send(app, at(body, session.as_deref())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let v = rpc_body(&r.body);
    let res = &v["result"];
    (res["isError"].as_bool().unwrap_or(false), res["content"][0]["text"].as_str().unwrap_or_default().to_string())
}

#[tokio::test]
async fn a_connector_token_cannot_name_someone_else() {
    let mut h = harness();
    let (_, access, _) = h.tokens().await;
    // a second connector: its principal is not a label this token may take
    h.token_for("Claude Code", REDIRECT).await;
    let doc = h.doc("pinned");
    let tom_id = h.st.store.lock(taisce_store::Scope::System).auth_owner().unwrap().unwrap().principal_id.to_string();
    // header, ?as= and ?cwd= are not identity sources under a token
    let spoof = [("taisce-principal", "tom")];
    let (e, out) = tool(&h.app, &access, "/mcp?as=claude:q&cwd=/x/y", &spoof, "append", json!({"doc_id": doc, "markdown": "a"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "claude:claude");
    // the pre-rename header name is stripped the same way (legacy.rs)
    let (e, out) = tool(&h.app, &access, "/mcp", &[("x-grimoire-principal", "claude:spoof")], "append", json!({"doc_id": doc, "markdown": "b"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "claude:claude");
    // `as` is a label in the token's own namespace
    let (e, out) = tool(&h.app, &access, "/mcp", &[], "append", json!({"doc_id": doc, "markdown": "c", "as": "claude:grimoire-task"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "claude:grimoire-task");
    for bad in ["tom", "claude", "workbox", tom_id.as_str(), "claude:", "claude:claude-code"] {
        let (e, out) = tool(&h.app, &access, "/mcp", &[], "append", json!({"doc_id": doc, "markdown": format!("x {bad}"), "as": bad})).await;
        assert!(e, "as {bad:?} must be refused: {out}");
        assert!(out.starts_with("as: "), "{out}");
    }
    assert_eq!(h.last_writer(doc), "claude:grimoire-task", "no refused write landed");
    let names: Vec<String> = h.st.store.lock(taisce_store::Scope::System).list_principals().unwrap().into_iter().map(|p| p.display_name).collect();
    for n in ["claude:q", "claude:y", "claude:spoof", "workbox"] {
        assert!(!names.contains(&n.to_string()), "{n} must not exist: {names:?}");
    }
    // /api under a connector token: its own principal, never the header's
    let mut req = post_json("/api/propose", json!({"doc_id": doc, "base_epoch": 0, "ops": [{"kind": {"op": "insert",
        "block_id": uuid::Uuid::now_v7(), "parent_id": null, "order_key": "", "block_type": "paragraph", "content": "api"}}]}));
    req.headers_mut().insert("authorization", format!("Bearer {access}").parse().unwrap());
    req.headers_mut().insert("taisce-principal", "tom".parse().unwrap());
    let r = send(&h.app, req).await;
    assert!(r.json()["verdicts"].is_array(), "{}", r.body);
    assert_eq!(h.last_writer(doc), "claude:claude");
}

#[tokio::test]
async fn the_owners_app_writes_as_the_human_and_ignores_as() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let app = h.token_for("Taisce iOS", APP_REDIRECT).await;
    let doc = h.doc("from the phone");
    let (e, out) = tool(&h.app, &app, "/mcp", &[("taisce-principal", "claude:spoof")], "append",
        json!({"doc_id": doc, "markdown": "a", "as": "claude:whoever"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "tom");
    let mut req = post_json("/api/propose", json!({"doc_id": doc, "base_epoch": 0, "ops": [{"kind": {"op": "insert",
        "block_id": uuid::Uuid::now_v7(), "parent_id": null, "order_key": "", "block_type": "paragraph", "content": "api"}}]}));
    req.headers_mut().insert("authorization", format!("Bearer {app}").parse().unwrap());
    req.headers_mut().insert("x-grimoire-principal", "claude:spoof".parse().unwrap());
    let r = send(&h.app, req).await;
    assert!(r.json()["verdicts"].is_array(), "{}", r.body);
    assert_eq!(h.last_writer(doc), "tom");
    let names: Vec<String> = h.st.store.lock(taisce_store::Scope::System).list_principals().unwrap().into_iter().map(|p| p.display_name).collect();
    assert!(!names.iter().any(|n| n == "claude:spoof" || n == "claude:whoever"), "{names:?}");
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
    let s = &mut *h.st.store.lock(taisce_store::Scope::System);
    let ah = hash_secret(&access);
    assert!(s.oauth_access_grant(&ah, now() + 60).unwrap().is_some());
    assert!(s.oauth_access_grant(&ah, now() + ACCESS_TTL + 1).unwrap().is_none());
    let later = now() + REFRESH_TTL + 10;
    let out = s.oauth_rotate_refresh(&hash_secret(&refresh), later, "x", later + 1, "y", later + 2).unwrap();
    assert_eq!(out, taisce_store::auth::RefreshOutcome::Invalid);
}

#[tokio::test]
async fn enrollment_link_is_single_use() {
    let mut h = harness();
    let t = random_token();
    let (hsh, owner) = (hash_secret(&t), h.owner);
    h.st.store.lock(taisce_store::Scope::System).auth_add_enrollment(&hsh, owner, now() + ENROLL_TTL).unwrap();
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
    assert_eq!(h.st.store.lock(taisce_store::Scope::System).auth_credentials(Some(h.owner)).unwrap().len(), 2);
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
    let c = h.st.store.lock(taisce_store::Scope::System).oauth_client(&id).unwrap().unwrap();
    assert_eq!((c.kind.as_str(), c.redirect_uris.clone()), ("cimd", vec![REDIRECT.to_string()]));
    // a redirect the document does not list
    let q2 = q.replace("33418/callback", "33418/elsewhere");
    assert_eq!(h.authorize(&q2).await.0.status, StatusCode::BAD_REQUEST);
    // the code flow works with the document client
    let redirect = h.sign_in(&req.unwrap()).await;
    let code = Url::parse(&redirect).unwrap().query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
    assert_eq!(h.exchange(&id, &code, VERIFIER).await.status, StatusCode::OK);
}

/// A client metadata document may list any redirect it likes; the app's
/// universal link still goes only to the fixed app client.
#[tokio::test]
async fn a_cimd_client_cannot_claim_the_apps_universal_link() {
    let mut cfg = AuthConfig::from_public_url(BASE).unwrap();
    cfg.cimd_allow_insecure = true;
    let mut h = harness_with(cfg);
    assert!(h.enroll().await.status.is_success());
    let link = format!("{BASE}{APP_HTTPS_CALLBACK_PATH}");
    let listed = link.clone();
    let id = serve_doc(move |id| json!({"client_id": id, "client_name": "Taisce", "redirect_uris": [listed, REDIRECT]}).to_string()).await;
    let q = format!("response_type=code&client_id={id}&redirect_uri={link}&code_challenge={CHALLENGE}&code_challenge_method=S256");
    let (r, req) = h.authorize(&q).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
    assert!(req.is_none(), "no sign-in page, no code");
    // its other redirect still works; the app client still gets the link
    let q = format!("response_type=code&client_id={id}&redirect_uri={REDIRECT}&code_challenge={CHALLENGE}&code_challenge_method=S256");
    assert_eq!(h.authorize(&q).await.0.status, StatusCode::OK);
    register_uris(&h, json!([link])).await;
    let code = h.code_for_at(FIRST_PARTY_APP_CLIENT, &link).await;
    assert!(!code.is_empty());
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
    assert!(needs_token(&Method::GET, "/mcp") && needs_token(&Method::GET, "/api") && needs_token(&Method::GET, "/ws/1"));
    assert!(!needs_token(&Method::GET, "/assets/x.js") && !needs_token(&Method::GET, "/apiary"));
    assert!(needs_token(&Method::POST, "/anything"));
}

// ---- push devices behind the token ----

#[tokio::test]
async fn devices_need_the_owners_app_token() {
    let mut h = harness();
    let (_, connector, _) = h.tokens().await;
    let app = h.token_for("Taisce", APP_REDIRECT).await;
    let token = "ab".repeat(32);
    let body = json!({"token": token, "platform": "ios", "env": "sandbox", "app_version": "1.0 (1)"});
    let with = |bearer: Option<&str>| {
        let mut req = post_json("/api/devices", body.clone());
        if let Some(b) = bearer {
            req.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {b}").parse().unwrap());
        }
        req
    };
    assert_eq!(send(&h.app, with(None)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&h.app, with(Some(&connector))).await.status, StatusCode::FORBIDDEN);
    let r = send(&h.app, with(Some(&app))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let d = h.st.store.lock(taisce_store::Scope::System).push_device(&token).unwrap().unwrap();
    assert_eq!((d.user_id, d.env.as_str()), (h.owner, "sandbox"));
}

// ---- personal access tokens ----

impl H {
    fn pat(&self, name: &str) -> (taisce_store::auth::ApiToken, String) {
        let (t, secret) = create_api_token(&mut self.st.store.lock(taisce_store::Scope::System), None, name, None, now()).unwrap();
        (t, secret.unwrap())
    }
}

fn with_bearer(mut req: HttpRequest<Body>, token: &str) -> HttpRequest<Body> {
    req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    req
}

#[tokio::test]
async fn a_pat_opens_mcp_as_its_owner() {
    let h = harness();
    let (t, secret) = h.pat("laptop");
    assert!(secret.starts_with(PAT_PREFIX) && secret.len() == 4 + 43, "{secret}");
    let r = mcp_tools(&h.app, &secret).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let tools = rpc_body(&r.body)["result"]["tools"].as_array().cloned().unwrap_or_default();
    assert!(tools.iter().any(|t| t["name"] == "append"), "{}", r.body);
    // writes: its own claude:<name> by default, `as` picks a label
    let doc = h.doc("pat");
    let (e, out) = tool(&h.app, &secret, "/mcp", &[], "append", json!({"doc_id": doc, "markdown": "a"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "claude:laptop");
    let (e, out) = tool(&h.app, &secret, "/mcp", &[], "append", json!({"doc_id": doc, "markdown": "b", "as": "claude:grimoire-pat"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "claude:grimoire-pat");
    // the pin holds: client-sent identity is dropped, as under OAuth
    for spoof in [("taisce-principal", "tom"), ("x-grimoire-principal", "claude:spoof")] {
        let (e, out) = tool(&h.app, &secret, "/mcp?as=claude:q&cwd=/x/y", &[spoof], "append", json!({"doc_id": doc, "markdown": "c"})).await;
        assert!(!e, "{out}");
        assert_eq!(h.last_writer(doc), "claude:laptop", "{spoof:?}");
    }
    for bad in ["tom", "claude", "workbox"] {
        let (e, out) = tool(&h.app, &secret, "/mcp", &[], "append", json!({"doc_id": doc, "markdown": "x", "as": bad})).await;
        assert!(e && out.starts_with("as: "), "as {bad:?} must be refused: {out}");
    }
    let names: Vec<String> = h.st.store.lock(taisce_store::Scope::System).list_principals().unwrap().into_iter().map(|p| p.display_name).collect();
    for n in ["claude:q", "claude:y", "claude:spoof", "workbox"] {
        assert!(!names.contains(&n.to_string()), "{n} must not exist: {names:?}");
    }
    // the use was recorded
    let row = h.st.store.lock(taisce_store::Scope::System).auth_api_tokens().unwrap().remove(0);
    assert_eq!(row.id, t.id);
    assert!(row.last_used_at.is_some_and(|u| u >= t.created_at), "{row:?}");
}

#[tokio::test]
async fn a_pat_is_refused_off_mcp() {
    let h = harness();
    let (_, secret) = h.pat("laptop");
    let challenge = format!("Bearer resource_metadata=\"{BASE}/.well-known/oauth-protected-resource\", error=\"invalid_token\"");
    let doc = h.doc("x");
    let reqs = [
        get("/api/docs"),
        get(&format!("/api/docs/{doc}")),
        post_json("/api/docs", json!({"title": "nope"})),
        get("/ws"),
        post_json("/api/push/devices", json!({"token": "ab", "env": "production"})),
        post_json("/anything", json!({})),
    ];
    for req in reqs {
        let path = req.uri().path().to_string();
        let r = send(&h.app, with_bearer(req, &secret)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}: {}", r.body);
        assert_eq!(r.headers["www-authenticate"], challenge.as_str(), "{path}");
    }
    // a path that merely starts with /mcp is not the MCP surface
    let r = send(&h.app, with_bearer(post_json("/mcpx", json!({})), &secret)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_and_unknown_pats_are_refused() {
    let h = harness();
    let (t, secret) = h.pat("laptop");
    assert_eq!(mcp_tools(&h.app, &secret).await.status, StatusCode::OK);
    let unknown = new_api_token();
    for bad in [unknown.as_str(), "tsk_", "tsk_short"] {
        let r = send(&h.app, mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), Some(bad), None)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad}");
        assert_eq!(
            r.headers["www-authenticate"],
            format!("Bearer resource_metadata=\"{BASE}/.well-known/oauth-protected-resource/mcp\", error=\"invalid_token\"")
        );
    }
    let revoked = revoke_api_token(&mut h.st.store.lock(taisce_store::Scope::System), &t.id.to_string(), now()).unwrap();
    assert_eq!(revoked.id, t.id);
    let r = send(&h.app, mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), Some(&secret), None)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // the name is free again; the old secret stays dead
    let (_, again) = h.pat("laptop");
    assert_eq!(mcp_tools(&h.app, &again).await.status, StatusCode::OK);
    assert!(revoke_api_token(&mut h.st.store.lock(taisce_store::Scope::System), "laptop", now()).is_ok());
    assert!(revoke_api_token(&mut h.st.store.lock(taisce_store::Scope::System), "laptop", now()).is_err());
}

#[tokio::test]
async fn oauth_tokens_are_unchanged_beside_pats() {
    let mut h = harness();
    let (_, access, _) = h.tokens().await;
    h.pat("laptop");
    assert_eq!(mcp_tools(&h.app, &access).await.status, StatusCode::OK);
    let r = send(&h.app, with_bearer(get("/api/docs"), &access)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let doc = h.doc("oauth");
    let (e, out) = tool(&h.app, &access, "/mcp", &[], "append", json!({"doc_id": doc, "markdown": "a"})).await;
    assert!(!e, "{out}");
    assert_eq!(h.last_writer(doc), "claude:claude");
}

/// Captures every log line (all targets, all levels) written inside `f`.
fn logged<T>(f: impl FnOnce() -> T) -> (T, String) {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    let out = tracing::subscriber::with_default(sub, || {
        // a callsite first hit by a parallel test with no subscriber caches
        // "never"; re-ask every live dispatcher, this one included
        tracing::callsite::rebuild_interest_cache();
        f()
    });
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, text)
}

#[tokio::test(flavor = "multi_thread")]
async fn pat_secrets_never_reach_the_log_or_the_list() {
    let h = harness();
    let ((t, secret), log) = logged(|| create_api_token(&mut h.st.store.lock(taisce_store::Scope::System), None, "laptop", None, now()).unwrap());
    let secret = secret.unwrap();
    assert!(log.contains("pat.create") && log.contains(&t.id.to_string()), "{log}");
    assert!(!log.contains(&secret) && !log.contains(&secret[4..]), "{log}");
    assert!(!log.contains(&hash_secret(&secret)), "{log}");
    let lines = api_token_lines(&h.st.store.lock(taisce_store::Scope::System)).unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("laptop") && lines[0].contains("never") && lines[0].contains("live"), "{lines:?}");
    assert!(!lines[0].contains(&secret[4..]) && !lines[0].contains(&hash_secret(&secret)), "{lines:?}");
    // first use is audited once, without the value
    let use_it = || {
        let (st, s) = (h.st.clone(), secret.clone());
        logged(|| tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(authenticate_pat(&st, &s, "1.2.3.4".into()))))
    };
    let (who, log) = use_it();
    assert_eq!(who.unwrap().user_id, h.owner);
    assert!(log.contains("pat.first_use") && !log.contains(&secret[4..]), "{log}");
    let (who, log) = use_it();
    assert!(who.is_some() && !log.contains("pat.first_use"), "{log}");
    let (_, log) = logged(|| revoke_api_token(&mut h.st.store.lock(taisce_store::Scope::System), "laptop", now()).unwrap());
    assert!(log.contains("pat.revoke") && !log.contains(&secret[4..]), "{log}");
    assert!(api_token_lines(&h.st.store.lock(taisce_store::Scope::System)).unwrap()[0].contains("revoked"));
}

#[test]
fn token_names() {
    for ok in ["laptop", "work-box_2", "a.b"] {
        assert!(valid_token_name(ok).is_ok(), "{ok}");
    }
    for bad in ["", " ", "has space", "tsk_x", "slash/no", &"x".repeat(41)] {
        assert!(valid_token_name(bad).is_err(), "{bad:?}");
    }
}

/// A token minted on the laptop is registered by its hash alone: the box
/// never sees the secret, yet the secret opens /mcp.
#[tokio::test]
async fn a_pat_registered_by_hash_opens_mcp() {
    let h = harness();
    let secret = new_api_token();
    let (t, none) =
        create_api_token(&mut h.st.store.lock(taisce_store::Scope::System), None, "laptop", Some(&hash_secret(&secret)), now()).unwrap();
    assert!(none.is_none());
    let who = authenticate_pat(&h.st, &secret, "1.2.3.4".into()).await.unwrap();
    assert_eq!((who.user_id, who.grant_id), (h.owner, t.id));
    for bad in ["", "abc", &"A".repeat(64), &format!("{}g", "a".repeat(63))] {
        assert!(create_api_token(&mut h.st.store.lock(taisce_store::Scope::System), None, "other", Some(bad), now()).is_err(), "{bad:?}");
    }
}


/// Review round 4: a device whose app grant had lapsed (or been revoked) when
/// the one-time grandfathering ran must not stay a connector forever. Its DCR
/// client looks unknown to the unchanged app's probe (400 → re-register), the
/// register call hands back the pinned `taisce-app`, and the new sign-in is
/// first party. Its old tokens stop working. A live, grandfathered client is
/// unaffected.
#[tokio::test]
async fn a_lapsed_app_client_re_registers_as_the_pinned_app() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let t0 = now();
    let make = |h: &H, id: &str| {
        let mut s = h.st.store.lock(taisce_store::Scope::System);
        s.oauth_upsert_client(&taisce_store::auth::OAuthClient {
            client_id: id.into(),
            kind: "dcr".into(),
            client_name: "Taisce".into(),
            redirect_uris: vec![APP_REDIRECT.into()],
            metadata: "{}".into(),
            created_at: t0,
            refresh_at: None,
        })
        .unwrap();
        let access = random_token();
        let refresh = random_token();
        let g = taisce_store::auth::Grant {
            id: uuid::Uuid::now_v7(),
            client_id: id.into(),
            user_id: h.owner,
            resource: None,
            scope: SCOPE.into(),
            created_at: t0,
            revoked_at: None,
            revoke_why: None,
        };
        s.oauth_issue_grant(None, &g, &hash_secret(&access), t0 + 3600, &hash_secret(&refresh), t0 + 86400).unwrap();
        (access, refresh)
    };
    // the lapsed device: its client exists but was never pinned
    let (lapsed_access, lapsed_refresh) = make(&h, "dcr_lapsed_ipad");
    // a live device grandfathered at first start
    let (live_access, _) = make(&h, "dcr_live_iphone");
    h.st.store.lock(taisce_store::Scope::System).oauth_mark_first_party("dcr_live_iphone", t0).unwrap();
    let probe = |id: &str| get(&format!("/oauth/authorize?client_id={id}&redirect_uri={APP_REDIRECT}&response_type=code"));
    // 1. the app's probe: unknown (400) for the lapsed client, known (a redirect) for the live one
    assert_eq!(send(&h.app, probe("dcr_lapsed_ipad")).await.status, StatusCode::BAD_REQUEST);
    let live = send(&h.app, probe("dcr_live_iphone")).await;
    assert!(live.status.is_redirection(), "{} {}", live.status, live.body);
    assert!(live.headers["location"].to_str().unwrap().starts_with(APP_REDIRECT));
    // its old tokens are dead: no lingering connector session
    let docs = |t: &str| with_bearer(get("/api/docs"), t);
    assert_eq!(send(&h.app, docs(&lapsed_access)).await.status, StatusCode::UNAUTHORIZED);
    let r = h.refresh("dcr_lapsed_ipad", &lapsed_refresh).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
    assert!(r.body.contains("invalid_client"), "{}", r.body);
    // the live one keeps working
    assert_eq!(send(&h.app, docs(&live_access)).await.status, StatusCode::OK);
    // 2. the app re-registers: it gets the pinned client
    let client = h.register("Taisce", APP_REDIRECT).await;
    assert_eq!(client, FIRST_PARTY_APP_CLIENT);
    // 3. authorize + token: the new session is first party
    let code = h.code_for_at(&client, APP_REDIRECT).await;
    let t = h.exchange_at(&client, &code, VERIFIER, APP_REDIRECT).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    let access = t.json()["access_token"].as_str().unwrap().to_string();
    let who = authenticate(&h.st, &access).await.expect("a live token");
    assert!(who.owner_app, "first party again");
    let mut req = post_json("/api/devices", json!({"token": "cd".repeat(32), "platform": "ios"}));
    req.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {access}").parse().unwrap());
    assert_eq!(send(&h.app, req).await.status, StatusCode::OK, "app powers (device registration) are back");
    // and a revoked grant behaves the same: its client is unpinned and unknown
    let (_, _) = make(&h, "dcr_revoked_mac");
    assert_eq!(send(&h.app, probe("dcr_revoked_mac")).await.status, StatusCode::BAD_REQUEST);
}

// ---- the app's universal-link redirect, the AASA file, the sign-in page ----

fn app_https() -> String {
    format!("{BASE}{APP_HTTPS_CALLBACK_PATH}")
}

async fn register_uris(h: &H, uris: Value) -> Res {
    send(&h.app, post_json("/oauth/register", json!({"client_name": "Taisce", "redirect_uris": uris}))).await
}

/// Newer app builds register the universal link (alone or beside the custom
/// scheme) and sign in through it; both map to the pinned app client, which
/// keeps the custom scheme for every older build.
#[tokio::test]
async fn the_app_signs_in_through_its_universal_link() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    for uris in [json!([app_https(), APP_REDIRECT]), json!([app_https()]), json!([APP_REDIRECT, app_https()])] {
        let r = register_uris(&h, uris.clone()).await;
        assert_eq!(r.status, StatusCode::CREATED, "{uris}: {}", r.body);
        let j = r.json();
        assert_eq!(j["client_id"], FIRST_PARTY_APP_CLIENT, "{uris}");
        let got: Vec<&str> = j["redirect_uris"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(got, vec![APP_REDIRECT, app_https().as_str()], "the app learns both redirects");
    }
    // the universal link is the app's alone: never beside anything else, never for another client
    for uris in [
        json!([app_https(), "https://claude.ai/api/mcp/auth_callback"]),
        json!([app_https(), "http://127.0.0.1:9/cb"]),
    ] {
        let r = register_uris(&h, uris.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uris}: {}", r.body);
    }
    // on a real (https) public URL, nothing else on the host is a redirect
    let prod = harness_with(AuthConfig::from_public_url("https://taisce.example").unwrap());
    for uris in [json!(["https://taisce.example/oauth/app-callback/evil"]), json!(["https://taisce.example/oauth/authorize"])] {
        let r = register_uris(&prod, uris.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{uris}: {}", r.body);
    }
    let r = register_uris(&prod, json!(["https://taisce.example/oauth/app-callback"])).await;
    assert_eq!(r.json()["client_id"], FIRST_PARTY_APP_CLIENT);
    let claude = h.register("Claude", REDIRECT).await;
    let q = format!("response_type=code&client_id={claude}&redirect_uri={}&code_challenge={CHALLENGE}&code_challenge_method=S256", app_https());
    let (page, _) = h.authorize(&q).await;
    assert_eq!(page.status, StatusCode::BAD_REQUEST, "a connector cannot borrow the app's link");
    // the code comes back on the universal link and is exchanged there
    let code = h.code_for_at(FIRST_PARTY_APP_CLIENT, &app_https()).await;
    let t = h.exchange_at(FIRST_PARTY_APP_CLIENT, &code, VERIFIER, &app_https()).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    let who = authenticate(&h.st, t.json()["access_token"].as_str().unwrap()).await.expect("a live token");
    assert!(who.owner_app, "the universal-link sign-in is first party");
    // a code minted for one redirect is not redeemable at the other
    let code = h.code_for_at(FIRST_PARTY_APP_CLIENT, &app_https()).await;
    let t = h.exchange_at(FIRST_PARTY_APP_CLIENT, &code, VERIFIER, APP_REDIRECT).await;
    assert_eq!(t.status, StatusCode::BAD_REQUEST, "{}", t.body);
    // the custom scheme keeps working for the builds already installed
    let code = h.code_for_at(FIRST_PARTY_APP_CLIENT, APP_REDIRECT).await;
    let t = h.exchange_at(FIRST_PARTY_APP_CLIENT, &code, VERIFIER, APP_REDIRECT).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.body);
    // the app's probe (`clientIsKnown`) answers for either redirect
    for redirect in [APP_REDIRECT.to_string(), app_https()] {
        let r = send(&h.app, get(&format!("/oauth/authorize?client_id={FIRST_PARTY_APP_CLIENT}&redirect_uri={redirect}&response_type=code"))).await;
        assert!(r.status.is_redirection(), "{redirect}: {}", r.status);
        assert!(r.headers["location"].to_str().unwrap().starts_with(&redirect));
    }
}

/// A `taisce-app` row from the build before (custom scheme only) gains the
/// universal link at the next start and stays pinned; live sessions are untouched.
#[test]
fn the_pinned_app_client_gains_the_universal_link() {
    let mut s = SqliteStore::open_in_memory().unwrap();
    s.oauth_upsert_client(&taisce_store::auth::OAuthClient {
        client_id: FIRST_PARTY_APP_CLIENT.into(),
        kind: "dcr".into(),
        client_name: "Taisce".into(),
        redirect_uris: vec![APP_REDIRECT_URI.into()],
        metadata: "{\"first_party\":true}".into(),
        created_at: 7,
        refresh_at: None,
    })
    .unwrap();
    s.oauth_mark_first_party(FIRST_PARTY_APP_CLIENT, 7).unwrap();
    ensure_first_party(&mut s, now(), "https://taisce.example/oauth/app-callback").unwrap();
    let c = s.oauth_client(FIRST_PARTY_APP_CLIENT).unwrap().unwrap();
    assert_eq!(c.redirect_uris, vec![APP_REDIRECT_URI.to_string(), "https://taisce.example/oauth/app-callback".into()]);
    assert_eq!(c.created_at, 7, "the row is updated, not recreated");
    assert!(s.oauth_is_first_party(FIRST_PARTY_APP_CLIENT).unwrap());
    // the public URL comes from config: a moved server follows it
    ensure_first_party(&mut s, now(), "https://other.example/oauth/app-callback").unwrap();
    let c = s.oauth_client(FIRST_PARTY_APP_CLIENT).unwrap().unwrap();
    assert_eq!(c.redirect_uris[1], "https://other.example/oauth/app-callback");
}

#[test]
fn app_registrations_are_exact() {
    let https = "https://t.example/oauth/app-callback";
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(app_registration(&v(&[APP_REDIRECT_URI]), https), Some(true));
    assert_eq!(app_registration(&v(&[https]), https), Some(true));
    assert_eq!(app_registration(&v(&[https, APP_REDIRECT_URI]), https), Some(true));
    assert_eq!(app_registration(&v(&[https, "https://claude.ai/api/mcp/auth_callback"]), https), Some(false));
    assert_eq!(app_registration(&v(&["ie.null.taisce:/evil"]), https), Some(false));
    assert_eq!(app_registration(&v(&["https://claude.ai/api/mcp/auth_callback"]), https), None);
}

/// The AASA file: JSON at the exact path, no token, no redirect, naming the
/// app by team + bundle id for the callback path and for web credentials.
#[tokio::test]
async fn the_apple_app_site_association_file() {
    let h = harness();
    let r = send(&h.app, get("/.well-known/apple-app-site-association")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.headers["content-type"].to_str().unwrap().starts_with("application/json"));
    assert!(!r.headers.contains_key(header::LOCATION));
    let j = r.json();
    assert_eq!(APPLE_APP_ID, "6UP35L9425.ie.null.taisce", "team id + bundle id from apple/App/project.yml");
    assert_eq!(j["applinks"]["details"][0]["appIDs"], json!([APPLE_APP_ID]));
    assert_eq!(j["applinks"]["details"][0]["components"][0]["/"], APP_HTTPS_CALLBACK_PATH);
    assert_eq!(j["webcredentials"]["apps"], json!([APPLE_APP_ID]));
}

/// The callback path opened in a browser: a plain page, never the code.
#[tokio::test]
async fn the_app_callback_in_a_browser_says_open_the_app() {
    let h = harness();
    let r = send(&h.app, get("/oauth/app-callback?code=zzsecretcode&state=s&iss=x")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.headers["content-type"].to_str().unwrap().starts_with("text/html"));
    assert!(r.body.contains("Open this on a device with the Taisce app"), "{}", r.body);
    assert!(!r.body.contains("zzsecretcode"), "the page never echoes the code");
    assert_eq!(r.headers[header::REFERRER_POLICY], "no-referrer");
}

/// The sign-in page says which app is asking, from which device and when,
/// with every part escaped.
#[tokio::test]
async fn the_sign_in_page_names_the_app_the_device_and_the_time() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let evil = h.register("<script>alert(1)</script>Taisce", REDIRECT).await;
    let page = |client: &str, redirect: &str, ua: &str| {
        HttpRequest::get(format!(
            "/oauth/authorize?response_type=code&client_id={client}&redirect_uri={redirect}&code_challenge={CHALLENGE}&code_challenge_method=S256"
        ))
        .header("host", "localhost:7512")
        .header(header::USER_AGENT, ua)
        .body(Body::empty())
        .unwrap()
    };
    let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1";
    let r = send(&h.app, page(&evil, REDIRECT, iphone)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;Taisce"), "{}", r.body);
    assert!(!r.body.contains("<script>alert"), "the client name is escaped");
    assert!(r.body.contains("a connected app, not the Taisce app"), "a connector calling itself Taisce is still a connector");
    assert!(r.body.contains("iPhone · Safari"), "{}", r.body);
    let year = chrono::Utc::now().format("%Y").to_string();
    assert!(r.body.contains(" UTC") && r.body.contains(&year), "the server time");
    assert!(r.body.contains("Choose Deny"));
    // a UA with markup in it is never copied through
    let r = send(&h.app, page(&evil, REDIRECT, "<img src=x onerror=alert(1)> Windows")).await;
    assert!(!r.body.contains("<img"), "the user agent is summarised, not echoed");
    assert!(r.body.contains("Windows PC"));
    // the person's own app reads as the app
    register_uris(&h, json!([APP_REDIRECT])).await;
    let mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
    let r = send(&h.app, page(FIRST_PARTY_APP_CLIENT, APP_REDIRECT, mac)).await;
    assert!(r.body.contains("(the Taisce app)"), "{}", r.body);
    assert!(r.body.contains("Mac · Safari"));
}

#[test]
fn device_summaries_are_coarse() {
    use passkey::device_summary as d;
    assert_eq!(d(Some("Mozilla/5.0 (iPad; CPU OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/140.0 Mobile/15E148 Safari/604.1")), "iPad · Chrome");
    assert_eq!(d(Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 Edg/140.0")), "Windows PC · Edge");
    assert_eq!(d(Some("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0")), "Linux PC · Firefox");
    assert_eq!(d(Some("Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Mobile Safari/537.36")), "Android · Chrome");
    assert_eq!(d(Some("curl/8.7.1")), "an unknown device");
    assert_eq!(d(None), "an unknown device");
    assert_eq!(passkey::server_time(1_790_000_000), "21 Sep 2026, 14:13 UTC");
}

// ---- the web UI's session cookie ----

/// `__Host-taisce_session=<value>` for a request's Cookie header.
fn with_cookie(mut req: HttpRequest<Body>, value: &str) -> HttpRequest<Body> {
    req.headers_mut().insert(header::COOKIE, format!("theme=dark; {}={value}", web::COOKIE).parse().unwrap());
    req
}

/// A same-origin write from the web UI: Origin, the CSRF header, the cookie.
fn ui_write(method: &str, path: &str, body: Value, cookie: &str) -> HttpRequest<Body> {
    let req = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:7512")
        .header("origin", BASE)
        .header(web::CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    with_cookie(req, cookie)
}

fn set_cookie_of(r: &Res) -> String {
    r.headers.get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
}

impl H {
    /// Enroll the soft passkey (once) and sign the web UI in: the finish
    /// response and the cookie's value.
    async fn web_sign_in(&mut self) -> (Res, String) {
        if self.st.store.lock(taisce_store::Scope::System).auth_credentials(None).unwrap().is_empty() {
            assert!(self.enroll().await.status.is_success());
        }
        let begin = send(&self.app, post_json("/auth/web/begin", json!({}))).await;
        assert_eq!(begin.status, StatusCode::OK, "{}", begin.body);
        let b = begin.json();
        let rcr: RequestChallengeResponse = serde_json::from_value(b["options"].clone()).unwrap();
        let cred = self.passkey.do_authentication(Url::parse(BASE).unwrap(), rcr).unwrap();
        let mut req = post_json("/auth/web/finish", json!({"ceremony": b["ceremony"], "credential": cred}));
        req.headers_mut().insert(
            header::USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Safari/605.1.15".parse().unwrap(),
        );
        let fin = send(&self.app, req).await;
        assert_eq!(fin.status, StatusCode::OK, "{}", fin.body);
        let sc = set_cookie_of(&fin);
        let value = sc.strip_prefix(&format!("{}=", web::COOKIE)).and_then(|r| r.split(';').next()).unwrap().to_string();
        (fin, value)
    }

    fn sessions(&self) -> Vec<taisce_store::auth::WebSession> {
        self.st.store.lock(taisce_store::Scope::System).auth_web_sessions().unwrap()
    }
}

#[tokio::test]
async fn web_sign_in_sets_the_session_cookie_with_the_exact_attributes() {
    let mut h = harness();
    let (fin, value) = h.web_sign_in().await;
    assert_eq!(
        set_cookie_of(&fin),
        format!("__Host-taisce_session={value}; Path=/; Max-Age=7776000; Secure; HttpOnly; SameSite=Strict")
    );
    assert!(!set_cookie_of(&fin).to_lowercase().contains("domain"));
    assert_eq!(value.len(), 43, "256 random bits, base64url");
    assert_eq!(fin.headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(fin.json()["name"], "tom");
    // stored as its hash only, with a coarse device summary
    let s = h.sessions();
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].user_id, s[0].user_agent.as_str()), (h.owner, "Mac · Safari"));
    let st = h.st.store.lock(taisce_store::Scope::System);
    assert!(st.auth_web_session_by_hash(&value, now()).unwrap().is_none(), "the raw value is not a key");
    assert_eq!(st.auth_web_session_by_hash(&hash_secret(&value), now()).unwrap().unwrap().id, s[0].id);
    let audits: Vec<String> = st.audit_events(20).unwrap().into_iter().map(|e| e.event).collect();
    assert!(audits.contains(&"web.signin".to_string()), "{audits:?}");
}

#[tokio::test]
async fn web_sign_in_refuses_cross_origin_replayed_and_mismatched_assertions() {
    let mut h = harness();
    assert!(h.enroll().await.status.is_success());
    let mut evil = post_json("/auth/web/begin", json!({}));
    evil.headers_mut().insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(send(&h.app, evil).await.status, StatusCode::FORBIDDEN);
    let mut none = post_json("/auth/web/begin", json!({}));
    none.headers_mut().remove(header::ORIGIN);
    assert_eq!(send(&h.app, none).await.status, StatusCode::FORBIDDEN, "Origin is required");
    // a ceremony works once
    let begin = send(&h.app, post_json("/auth/web/begin", json!({}))).await.json();
    let rcr: RequestChallengeResponse = serde_json::from_value(begin["options"].clone()).unwrap();
    let cred = h.passkey.do_authentication(Url::parse(BASE).unwrap(), rcr).unwrap();
    let body = json!({"ceremony": begin["ceremony"], "credential": cred});
    assert_eq!(send(&h.app, post_json("/auth/web/finish", body.clone())).await.status, StatusCode::OK);
    let again = send(&h.app, post_json("/auth/web/finish", body)).await;
    assert_eq!(again.status, StatusCode::GONE);
    assert!(set_cookie_of(&again).is_empty());
    // an assertion over another ceremony's challenge is refused
    let b1 = send(&h.app, post_json("/auth/web/begin", json!({}))).await.json();
    let b2 = send(&h.app, post_json("/auth/web/begin", json!({}))).await.json();
    let rcr: RequestChallengeResponse = serde_json::from_value(b1["options"].clone()).unwrap();
    let cred = h.passkey.do_authentication(Url::parse(BASE).unwrap(), rcr).unwrap();
    let r = send(&h.app, post_json("/auth/web/finish", json!({"ceremony": b2["ceremony"], "credential": cred}))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.body);
    assert_eq!(h.sessions().len(), 1);
}

#[tokio::test]
async fn web_sign_in_is_rate_limited_per_ip() {
    let mut h = harness_with(AuthConfig { trusted_proxy: true, ..AuthConfig::from_public_url(BASE).unwrap() });
    assert!(h.enroll().await.status.is_success());
    let from = |ip: &str| {
        let mut r = post_json("/auth/web/begin", json!({}));
        r.headers_mut().insert("x-forwarded-for", ip.parse().unwrap());
        r
    };
    let mut limited = 0;
    for _ in 0..25 {
        if send(&h.app, from("203.0.113.7")).await.status == StatusCode::TOO_MANY_REQUESTS {
            limited += 1;
        }
    }
    assert!(limited >= 5, "the Login bucket (burst 20): {limited}");
    assert_eq!(send(&h.app, from("198.51.100.1")).await.status, StatusCode::OK, "another IP has its own bucket");
}

#[tokio::test]
async fn the_cookie_opens_api_only() {
    let mut h = harness();
    let (_, c) = h.web_sign_in().await;
    // /api: yes
    let r = send(&h.app, with_cookie(get("/api/docs"), &c)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(send(&h.app, get("/api/docs")).await.status, StatusCode::UNAUTHORIZED, "no cookie, no data");
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), "not-a-session")).await.status, StatusCode::UNAUTHORIZED);
    // /mcp: no (the cookie is not a bearer, and it is ignored there)
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}});
    let mut m = with_cookie(mcp(init, None, None), &c);
    m.headers_mut().insert(header::ORIGIN, BASE.parse().unwrap());
    m.headers_mut().insert(web::CSRF_HEADER, "1".parse().unwrap());
    assert_eq!(send(&h.app, m).await.status, StatusCode::UNAUTHORIZED);
    // /ws: no
    assert_eq!(send(&h.app, with_cookie(get("/ws/x"), &c)).await.status, StatusCode::UNAUTHORIZED);
    // /oauth/token: the cookie is no credential there (no token comes back)
    let mut t = with_cookie(post_form("/oauth/token", &[("grant_type", "refresh_token"), ("client_id", FIRST_PARTY_APP_CLIENT)]), &c);
    t.headers_mut().insert(header::ORIGIN, BASE.parse().unwrap());
    let r = send(&h.app, t).await;
    assert_ne!(r.status, StatusCode::OK, "{}", r.body);
    assert!(!r.body.contains("access_token"), "{}", r.body);
    // a bearer token beside a cookie wins
    let (_, connector, _) = h.tokens().await;
    let r = send(&h.app, with_bearer(with_cookie(get("/api/docs"), &c), &connector)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = send(&h.app, with_bearer(with_cookie(get("/api/docs"), &c), "junk")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "an invalid bearer is not rescued by the cookie");
    // the session route says who is signed in
    let r = send(&h.app, with_cookie(get("/auth/web/session"), &c)).await;
    assert_eq!((r.json()["signed_in"].as_bool(), r.json()["name"].as_str()), (Some(true), Some("tom")));
    let r = send(&h.app, get("/auth/web/session")).await;
    assert_eq!(r.json()["signed_in"], false);
}

#[tokio::test]
async fn cookie_writes_need_our_origin_and_the_csrf_header() {
    let mut h = harness();
    let (_, c) = h.web_sign_in().await;
    let docs = |h: &H| h.st.store.lock(taisce_store::Scope::System).list_docs().unwrap().len();
    let before = docs(&h);
    let create = |origin: Option<&str>, csrf: Option<&str>, extra: &[(&str, &str)]| {
        let mut b = HttpRequest::post("/api/docs").header("host", "localhost:7512").header("content-type", "application/json");
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        if let Some(x) = csrf {
            b = b.header(web::CSRF_HEADER, x);
        }
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        with_cookie(b.body(Body::from(json!({"title": "made by the cookie"}).to_string())).unwrap(), &c)
    };
    for (what, req) in [
        ("no Origin", create(None, Some("1"), &[])),
        ("a foreign Origin", create(Some("https://evil.example"), Some("1"), &[])),
        ("a look-alike Origin", create(Some("http://localhost:7512.evil.example"), Some("1"), &[])),
        ("Origin null", create(Some("null"), Some("1"), &[])),
        ("no CSRF header", create(Some(BASE), None, &[])),
        ("a wrong CSRF value", create(Some(BASE), Some("0"), &[])),
        // a cross-site form post: no custom header can ride on it
        ("a cross-site form POST", create(Some("https://evil.example"), None, &[("sec-fetch-site", "cross-site")])),
    ] {
        let r = send(&h.app, req).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{what}: {}", r.body);
        assert_eq!(r.json()["code"], "csrf", "{what}");
    }
    // other methods too
    for m in ["PUT", "PATCH", "DELETE"] {
        let req = HttpRequest::builder().method(m).uri("/api/workspaces/x").header("host", "localhost:7512").body(Body::empty()).unwrap();
        assert_eq!(send(&h.app, with_cookie(req, &c)).await.status, StatusCode::FORBIDDEN, "{m}");
    }
    assert_eq!(docs(&h), before, "no refused write reached the store");
    // the UI's own write: Origin + Taisce-CSRF
    let r = send(&h.app, create(Some(BASE), Some("1"), &[])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(docs(&h), before + 1);
    // written as the person (their human principal), not an agent
    {
        let s = h.st.store.lock(taisce_store::Scope::System);
        let d = s.list_docs().unwrap().into_iter().find(|d| d.title == "made by the cookie").unwrap();
        let p = s.list_principals().unwrap().into_iter().find(|p| p.id == d.created_by).unwrap();
        assert_eq!(p.kind, PrincipalKind::Human);
    }
    // a bearer write needs neither (no ambient credential to forge)
    let (_, connector, _) = h.tokens().await;
    let req = HttpRequest::post("/api/docs")
        .header("host", "localhost:7512")
        .header("content-type", "application/json")
        .body(Body::from(json!({"title": "by a token"}).to_string()))
        .unwrap();
    assert_eq!(send(&h.app, with_bearer(req, &connector)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn web_sessions_expire_idle_and_absolute_and_roll() {
    use taisce_store::auth::{WEB_SESSION_ABSOLUTE, WEB_SESSION_IDLE};
    let h = harness();
    let mk = |created: i64, touched: Option<i64>| {
        let tok = random_token();
        let mut s = h.st.store.lock(taisce_store::Scope::System);
        let w = s.auth_create_web_session(h.owner, &hash_secret(&tok), "", created).unwrap();
        if let Some(t) = touched {
            assert!(s.auth_touch_web_session(w.id, t).unwrap());
        }
        (tok, w.id)
    };
    let last = |id: uuid::Uuid| h.sessions().into_iter().find(|w| w.id == id).unwrap().last_used_at;
    let n = now();
    // idle 14 days: refused, and the cookie is cleared
    let (idle, _) = mk(n - WEB_SESSION_IDLE - 5, None);
    let r = send(&h.app, with_cookie(get("/api/docs"), &idle)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(set_cookie_of(&r), "__Host-taisce_session=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict");
    // idle 13 days: live, and the request rolls last_used forward
    let (busy, busy_id) = mk(n - 13 * 86400, None);
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &busy)).await.status, StatusCode::OK);
    assert!(last(busy_id) >= n, "rolled");
    // used a minute ago but signed in 90 days ago: the absolute end
    let (old, _) = mk(n - WEB_SESSION_ABSOLUTE - 5, Some(n - 60));
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &old)).await.status, StatusCode::UNAUTHORIZED);
    let (young, _) = mk(n - WEB_SESSION_ABSOLUTE + 3600, Some(n - 60));
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &young)).await.status, StatusCode::OK);
    // the roll writes at most once a minute
    let (fresh, fresh_id) = mk(n - 30, None);
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &fresh)).await.status, StatusCode::OK);
    assert_eq!(last(fresh_id), n - 30, "used 30 s ago: no write");
}

#[tokio::test]
async fn logout_revokes_the_session_and_clears_the_cookie() {
    let mut h = harness();
    let (_, c) = h.web_sign_in().await;
    let (_, other) = h.web_sign_in().await;
    // a cross-site logout is refused (and changes nothing)
    let mut evil = with_cookie(post_json("/auth/web/logout", json!({})), &c);
    evil.headers_mut().insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    evil.headers_mut().insert(web::CSRF_HEADER, "1".parse().unwrap());
    assert_eq!(send(&h.app, evil).await.status, StatusCode::FORBIDDEN);
    let no_csrf = with_cookie(post_json("/auth/web/logout", json!({})), &c);
    assert_eq!(send(&h.app, no_csrf).await.status, StatusCode::FORBIDDEN, "no CSRF header");
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &c)).await.status, StatusCode::OK);
    let r = send(&h.app, ui_write("POST", "/auth/web/logout", json!({}), &c)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(set_cookie_of(&r), web::clear_cookie());
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &c)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &other)).await.status, StatusCode::OK, "only this browser signs out");
    // signing out twice is harmless
    assert_eq!(send(&h.app, ui_write("POST", "/auth/web/logout", json!({}), &c)).await.status, StatusCode::OK);
    let audits: Vec<String> =
        h.st.store.lock(taisce_store::Scope::System).audit_events(50).unwrap().into_iter().map(|e| e.event).collect();
    assert_eq!(audits.iter().filter(|e| *e == "web.signout").count(), 1, "{audits:?}");
}

#[tokio::test]
async fn the_cli_lists_and_revokes_web_sessions() {
    let mut h = harness();
    let (_, c) = h.web_sign_in().await;
    let id = h.sessions()[0].id.to_string();
    let lines = web::session_lines(&h.st.store.lock(taisce_store::Scope::System), now()).unwrap();
    assert_eq!(lines.len(), 1);
    let l = &lines[0];
    assert!(l.contains(&id[..18]) && l.contains("tom") && l.contains("Mac · Safari") && l.ends_with("live"), "{l}");
    assert!(!l.contains(&c), "never the secret");
    // the exact path `taisce auth revoke <prefix>` runs
    let out = cli_revoke(&mut h.st.store.lock(taisce_store::Scope::System), &id[..18], now()).unwrap();
    assert_eq!(out, format!("revoked web session {id}"));
    assert_eq!(send(&h.app, with_cookie(get("/api/docs"), &c)).await.status, StatusCode::UNAUTHORIZED);
    {
        let s = h.st.store.lock(taisce_store::Scope::System);
        assert!(web::session_lines(&s, now()).unwrap()[0].contains("revoked"));
        let audits: Vec<String> = s.audit_events(50).unwrap().into_iter().map(|e| e.event).collect();
        assert!(audits.contains(&"web.revoke".to_string()), "{audits:?}");
    }
    assert!(cli_revoke(&mut h.st.store.lock(taisce_store::Scope::System), &id, now()).is_err(), "already revoked");
}

#[tokio::test]
async fn a_web_session_is_not_a_push_device() {
    let mut h = harness();
    let (_, c) = h.web_sign_in().await;
    let body = json!({"token": "cd".repeat(32), "platform": "ios", "env": "sandbox", "app_version": "1"});
    let r = send(&h.app, ui_write("POST", "/api/devices", body, &c)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
}

#[tokio::test]
async fn server_mode_ui_responses_carry_the_security_headers() {
    let h = harness();
    let r = send(&h.app, get("/")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers.get(header::CONTENT_SECURITY_POLICY).unwrap(), web::UI_CSP);
    assert!(web::UI_CSP.contains("frame-ancestors 'none'") && web::UI_CSP.contains("script-src 'self';"));
    assert_eq!(r.headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    assert_eq!(r.headers.get(header::REFERRER_POLICY).unwrap(), "same-origin");
    assert_eq!(r.headers.get("x-content-type-options").unwrap(), "nosniff");
    // the 401 the UI turns into its sign-in screen carries them too
    assert!(send(&h.app, get("/api/docs")).await.headers.contains_key(header::CONTENT_SECURITY_POLICY));
    // a passkey page keeps its own nonce'd policy
    let t = random_token();
    h.st.store.lock(taisce_store::Scope::System).auth_add_enrollment(&hash_secret(&t), h.owner, now() + ENROLL_TTL).unwrap();
    let page = send(&h.app, get(&format!("/auth/enroll?t={t}"))).await;
    assert!(page.headers.get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap().contains("'nonce-"));
}
