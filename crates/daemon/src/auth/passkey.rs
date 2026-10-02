//! Passkeys (webauthn-rs) and the two server-rendered pages that use them:
//! the sign-in/consent page behind `/oauth/authorize` and the one-time
//! enrollment page `grimoire auth enroll` links to. No framework, no CDN:
//! one inline script and stylesheet, each carrying the response's CSP nonce.
//!
//! Ceremony state (the challenge) lives in memory only, keyed by a random
//! id, for ten (sign-in) or fifteen (enrollment) minutes; a restart simply
//! means reloading the page.

use super::ratelimit::Class;
use super::{AUDIT, AuthState, hash_secret, now, random_token};
use crate::store_ext::with_store;
use axum::Json;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use taisce_store::auth::AuthCredential;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use webauthn_rs::prelude::{
    Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
};

const AUTHZ_TTL: i64 = 600;
const MAX_AUTHZ: usize = 1000;
const MAX_ENROLL: usize = 100;

/// An authorization request waiting for its passkey.
#[derive(Clone)]
pub struct AuthzRequest {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uri: String,
    pub state: Option<String>,
    pub code_challenge: String,
    pub resource: Option<String>,
    pub scope: String,
    pub created: i64,
    pub ceremony: Option<PasskeyAuthentication>,
}

pub struct Enrollment {
    token_hash: String,
    user_id: uuid::Uuid,
    label: String,
    created: i64,
    ceremony: PasskeyRegistration,
}

#[derive(Default)]
pub struct Pending {
    authz: HashMap<String, AuthzRequest>,
    enroll: HashMap<String, Enrollment>,
}

impl Pending {
    pub fn add_authz(&mut self, r: AuthzRequest) -> String {
        self.sweep(r.created);
        if self.authz.len() >= MAX_AUTHZ
            && let Some(oldest) = self.authz.iter().min_by_key(|(_, v)| v.created).map(|(k, _)| k.clone())
        {
            self.authz.remove(&oldest);
        }
        let id = random_token();
        self.authz.insert(id.clone(), r);
        id
    }

    fn authz_live(&mut self, id: &str, now: i64) -> Option<&mut AuthzRequest> {
        self.authz.get_mut(id).filter(|r| now - r.created < AUTHZ_TTL)
    }

    pub fn sweep(&mut self, now: i64) {
        self.authz.retain(|_, r| now - r.created < AUTHZ_TTL);
        self.enroll.retain(|_, e| now - e.created < super::ENROLL_TTL);
    }
}

pub fn router(st: AuthState) -> axum::Router {
    axum::Router::new()
        .route("/oauth/authorize/begin", post(login_begin))
        .route("/oauth/authorize/finish", post(login_finish))
        .route("/oauth/authorize/deny", post(login_deny))
        .route("/auth/enroll", get(enroll_page))
        .route("/auth/enroll/begin", post(enroll_begin))
        .route("/auth/enroll/finish", post(enroll_finish))
        .with_state(st)
}

/// WebAuthn credential ids are compared as base64url text.
pub fn cred_id_text(pk: &Passkey) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pk.cred_id().as_ref())
}

fn json_err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"error": msg}))).into_response()
}

/// A POST from these pages must come from the public origin (WebAuthn
/// checks the origin in clientData too; this refuses earlier and plainer).
fn same_origin(st: &AuthState, headers: &HeaderMap) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => o.trim_end_matches('/') == st.cfg.base,
    }
}

fn limited(st: &AuthState, req: &Request) -> bool {
    let ip = super::client_ip(&st.cfg, req.headers(), req.extensions());
    !st.limiter.allow(Class::Login, &ip)
}

async fn json_body<T: serde::de::DeserializeOwned>(req: Request) -> Result<T, Response> {
    let Json(v) = <Json<T> as axum::extract::FromRequest<()>>::from_request(req, &())
        .await
        .map_err(|_| json_err(StatusCode::BAD_REQUEST, "malformed request"))?;
    Ok(v)
}

fn passkeys_of(creds: &[AuthCredential]) -> Vec<Passkey> {
    creds.iter().filter_map(|c| serde_json::from_str(&c.passkey).ok()).collect()
}

// ---- sign-in ----

#[derive(Deserialize)]
struct ReqId {
    req: String,
}

async fn login_begin(State(st): State<AuthState>, req: Request) -> Response {
    if limited(&st, &req) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    if !same_origin(&st, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let body: ReqId = match json_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // every user's passkeys: the assertion names the credential, and the
    // credential names the user (no username field to type)
    let creds = with_store(&st.store, |s| s.auth_credentials(None).unwrap_or_default()).await;
    let keys = passkeys_of(&creds);
    if keys.is_empty() {
        return json_err(StatusCode::SERVICE_UNAVAILABLE, "no passkey enrolled");
    }
    let (options, ceremony) = match st.webauthn.start_passkey_authentication(&keys) {
        Ok(x) => x,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("webauthn: {e}")),
    };
    let mut p = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(r) = p.authz_live(&body.req, now()) else {
        return json_err(StatusCode::GONE, "this sign-in request expired; start again from the app");
    };
    r.ceremony = Some(ceremony);
    Json(options).into_response()
}

#[derive(Deserialize)]
struct LoginFinish {
    req: String,
    credential: PublicKeyCredential,
}

async fn login_finish(State(st): State<AuthState>, req: Request) -> Response {
    if limited(&st, &req) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    if !same_origin(&st, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let body: LoginFinish = match json_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // the ceremony is single-shot: taken out whether it verifies or not
    let (authz, ceremony) = {
        let mut p = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(r) = p.authz_live(&body.req, now()) else {
            return json_err(StatusCode::GONE, "this sign-in request expired; start again from the app");
        };
        let Some(c) = r.ceremony.take() else {
            return json_err(StatusCode::BAD_REQUEST, "no passkey challenge outstanding; press the button again");
        };
        (r.clone(), c)
    };
    let result = match st.webauthn.finish_passkey_authentication(&body.credential, &ceremony) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: AUDIT, event = "login.fail", client = authz.client_id, "passkey assertion rejected: {e}");
            return json_err(StatusCode::UNAUTHORIZED, "passkey not accepted");
        }
    };
    let cred_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(result.cred_id().as_ref());
    let now = now();
    let user = with_store(&st.store, move |s| {
        let c = s.auth_credential_by_cred_id(&cred_id).ok()??;
        if let Ok(mut pk) = serde_json::from_str::<Passkey>(&c.passkey) {
            pk.update_credential(&result);
            if let Ok(js) = serde_json::to_string(&pk) {
                let _ = s.auth_touch_credential(&cred_id, &js, now);
            }
        }
        Some(c.user_id)
    })
    .await;
    let Some(user) = user else {
        return json_err(StatusCode::UNAUTHORIZED, "passkey not recognised");
    };
    st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).authz.remove(&body.req);
    tracing::info!(target: AUDIT, event = "login.ok", client = authz.client_id, name = authz.client_name, user = %user);
    match super::oauth::issue_code(&st, &authz, user).await {
        Ok(redirect) => Json(json!({"redirect": redirect})).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

async fn login_deny(State(st): State<AuthState>, req: Request) -> Response {
    if !same_origin(&st, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let body: ReqId = match json_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let r = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).authz.remove(&body.req);
    match r {
        Some(r) => Json(json!({"redirect": super::oauth::denied_redirect(&st, &r)})).into_response(),
        None => json_err(StatusCode::GONE, "this sign-in request expired"),
    }
}

// ---- enrollment ----

#[derive(Deserialize)]
struct EnrollQuery {
    t: Option<String>,
}

async fn enroll_page(State(st): State<AuthState>, Query(q): Query<EnrollQuery>, req: Request) -> Response {
    if limited(&st, &req) {
        return error_page(StatusCode::TOO_MANY_REQUESTS, "Too many attempts. Wait a minute and try again.");
    }
    let Some(t) = q.t.filter(|t| !t.is_empty()) else {
        return error_page(StatusCode::BAD_REQUEST, "This enrollment link is incomplete.");
    };
    let h = hash_secret(&t);
    let ok = with_store(&st.store, move |s| s.auth_peek_enrollment(&h, now()).ok().flatten().is_some()).await;
    if !ok {
        return error_page(
            StatusCode::GONE,
            "This enrollment link has expired or was already used. Run `grimoire auth enroll` again.",
        );
    }
    page(
        "Add a passkey",
        &format!(
            r#"<h1>Add a passkey</h1>
<p>This registers a passkey for the owner of <b>{host}</b>. The link works once.</p>
<label>Name for this passkey <input id="label" value="passkey" maxlength="60"></label>
<button id="go">Create passkey</button>
<p id="msg" role="status"></p>"#,
            host = esc(&st.cfg.rp_id)
        ),
        &[("mode", "enroll"), ("token", &t)],
    )
}

#[derive(Deserialize)]
struct EnrollBegin {
    t: String,
    #[serde(default)]
    label: Option<String>,
}

async fn enroll_begin(State(st): State<AuthState>, req: Request) -> Response {
    if limited(&st, &req) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    if !same_origin(&st, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let body: EnrollBegin = match json_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let token_hash = hash_secret(&body.t);
    let h = token_hash.clone();
    let found = with_store(&st.store, move |s| {
        let uid = s.auth_peek_enrollment(&h, now()).ok()??;
        let user = s.auth_user(uid).ok()??;
        let creds = s.auth_credentials(Some(uid)).unwrap_or_default();
        Some((user, creds))
    })
    .await;
    let Some((user, creds)) = found else {
        return json_err(StatusCode::GONE, "enrollment link expired or used");
    };
    let exclude: Vec<_> = passkeys_of(&creds).iter().map(|p| p.cred_id().clone()).collect();
    let (options, ceremony) = match st.webauthn.start_passkey_registration(
        user.id,
        &user.name,
        &user.name,
        (!exclude.is_empty()).then_some(exclude),
    ) {
        Ok(x) => x,
        Err(e) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("webauthn: {e}")),
    };
    let label = super::oauth::clean_name(body.label.as_deref().unwrap_or("passkey"));
    let id = random_token();
    {
        let mut p = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        p.sweep(now());
        if p.enroll.len() >= MAX_ENROLL {
            return json_err(StatusCode::TOO_MANY_REQUESTS, "too many enrollments in progress");
        }
        p.enroll.insert(
            id.clone(),
            Enrollment {
                token_hash,
                user_id: user.id,
                label: if label.is_empty() { "passkey".into() } else { label },
                created: now(),
                ceremony,
            },
        );
    }
    Json(json!({"ceremony": id, "options": options})).into_response()
}

#[derive(Deserialize)]
struct EnrollFinish {
    ceremony: String,
    credential: RegisterPublicKeyCredential,
}

async fn enroll_finish(State(st): State<AuthState>, req: Request) -> Response {
    if limited(&st, &req) {
        return json_err(StatusCode::TOO_MANY_REQUESTS, "too many attempts");
    }
    if !same_origin(&st, req.headers()) {
        return json_err(StatusCode::FORBIDDEN, "cross-origin request");
    }
    let body: EnrollFinish = match json_body(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Some(e) = st.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).enroll.remove(&body.ceremony) else {
        return json_err(StatusCode::GONE, "enrollment expired; reload the link");
    };
    let pk = match st.webauthn.finish_passkey_registration(&body.credential, &e.ceremony) {
        Ok(pk) => pk,
        Err(err) => return json_err(StatusCode::BAD_REQUEST, &format!("passkey not accepted: {err}")),
    };
    let cred = AuthCredential {
        id: uuid::Uuid::now_v7(),
        user_id: e.user_id,
        cred_id: cred_id_text(&pk),
        passkey: match serde_json::to_string(&pk) {
            Ok(s) => s,
            Err(err) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        },
        label: e.label.clone(),
        created_at: now(),
        last_used_at: None,
    };
    let (c, h) = (cred.clone(), e.token_hash.clone());
    let res = with_store(&st.store, move |s| {
        if s.auth_credential_by_cred_id(&c.cred_id).ok().flatten().is_some() {
            return Err("this passkey is already registered".to_string());
        }
        s.auth_enroll_credential(&h, &c, now()).map_err(|e| e.to_string())
    })
    .await;
    match res {
        Ok(true) => {
            tracing::info!(target: AUDIT, event = "passkey.enroll", user = %cred.user_id, credential = %cred.id, label = cred.label);
            Json(json!({"ok": true, "credential": cred.id})).into_response()
        }
        Ok(false) => json_err(StatusCode::GONE, "enrollment link expired or already used"),
        Err(m) => json_err(StatusCode::CONFLICT, &m),
    }
}

// ---- pages ----

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const STYLE: &str = "body{font:16px/1.5 -apple-system,system-ui,sans-serif;max-width:28rem;margin:12vh auto;padding:0 16px;color:#1c1c1e;background:#fff}\
h1{font-size:1.4rem}button{font:inherit;padding:.6rem 1.2rem;border-radius:.5rem;border:0;background:#1c1c1e;color:#fff;cursor:pointer}\
button.alt{background:none;color:#555;text-decoration:underline}input{font:inherit;padding:.3rem;margin:.3rem 0 1rem;width:100%;box-sizing:border-box}\
.client{font-weight:600}.muted{color:#666;font-size:.9rem;word-break:break-all}#msg{min-height:1.5rem}\
@media (prefers-color-scheme:dark){body{background:#111;color:#eee}button{background:#eee;color:#111}button.alt{color:#aaa}.muted{color:#999}}";

/// The page script: base64url ⇄ ArrayBuffer around navigator.credentials.
const SCRIPT: &str = r#"
const d=document.body.dataset,msg=document.getElementById('msg');
const dec=s=>Uint8Array.from(atob(s.replace(/-/g,'+').replace(/_/g,'/')+'==='.slice((s.length+3)%4)),c=>c.charCodeAt(0)).buffer;
const enc=b=>btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g,'-').replace(/\//g,'_').replace(/=+$/,'');
async function post(u,b){const r=await fetch(u,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(b)});
 const j=await r.json().catch(()=>({error:'HTTP '+r.status}));if(!r.ok)throw new Error(j.error||('HTTP '+r.status));return j;}
function say(t){msg.textContent=t;}
async function login(){
 say('Waiting for your passkey…');
 const o=(await post('/oauth/authorize/begin',{req:d.req})).publicKey;
 o.challenge=dec(o.challenge);(o.allowCredentials||[]).forEach(c=>c.id=dec(c.id));
 const a=await navigator.credentials.get({publicKey:o});
 const credential={id:a.id,rawId:enc(a.rawId),type:a.type,extensions:a.getClientExtensionResults(),response:{
  authenticatorData:enc(a.response.authenticatorData),clientDataJSON:enc(a.response.clientDataJSON),
  signature:enc(a.response.signature),userHandle:a.response.userHandle?enc(a.response.userHandle):null}};
 const r=await post('/oauth/authorize/finish',{req:d.req,credential});
 say('Signed in. Returning to the app…');location.href=r.redirect;}
async function deny(){const r=await post('/oauth/authorize/deny',{req:d.req});location.href=r.redirect;}
async function enroll(){
 say('Follow your device’s prompt…');
 const label=document.getElementById('label').value;
 const b=await post('/auth/enroll/begin',{t:d.token,label});const o=b.options.publicKey;
 o.challenge=dec(o.challenge);o.user.id=dec(o.user.id);(o.excludeCredentials||[]).forEach(c=>c.id=dec(c.id));
 const c=await navigator.credentials.create({publicKey:o});
 const credential={id:c.id,rawId:enc(c.rawId),type:c.type,extensions:c.getClientExtensionResults(),response:{
  attestationObject:enc(c.response.attestationObject),clientDataJSON:enc(c.response.clientDataJSON),
  transports:c.response.getTransports?c.response.getTransports():undefined}};
 await post('/auth/enroll/finish',{ceremony:b.ceremony,credential});
 document.getElementById('go').disabled=true;say('Passkey added. You can close this page.');}
const run=f=>()=>f().catch(e=>say(e.name==='NotAllowedError'?'Cancelled.':('Failed: '+e.message)));
if(!window.PublicKeyCredential)say('This browser does not support passkeys.');
document.getElementById('go').addEventListener('click',run(d.mode==='enroll'?enroll:login));
const n=document.getElementById('deny');if(n)n.addEventListener('click',run(deny));
"#;

/// A complete page under a strict CSP: nothing loads but this document's
/// own nonce'd script and style; fetches go to this origin only.
fn page(title: &str, body: &str, data: &[(&str, &str)]) -> Response {
    let nonce = random_token();
    let attrs: String = data.iter().map(|(k, v)| format!(" data-{k}=\"{}\"", esc(v))).collect();
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{t}</title>\
<style nonce=\"{nonce}\">{STYLE}</style></head><body{attrs}>{body}<script nonce=\"{nonce}\">{SCRIPT}</script></body></html>",
        t = esc(title)
    );
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; \
img-src 'none'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'"
    );
    let mut r = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    r
}

/// The sign-in + consent page: names the client, then one passkey tap
/// both authenticates and approves.
pub fn login_page(req_id: &str, client_name: &str, client_id: &str) -> Response {
    page(
        "Sign in to Taisce",
        &format!(
            r#"<h1>Sign in to Taisce</h1>
<p><span class="client">{name}</span> wants full access to your Taisce: reading and editing your docs.</p>
<p class="muted">Client: {id}</p>
<button id="go">Continue with passkey</button> <button id="deny" class="alt">Deny</button>
<p id="msg" role="status"></p>"#,
            name = esc(client_name),
            id = esc(client_id)
        ),
        &[("mode", "login"), ("req", req_id)],
    )
}

pub fn error_page(status: StatusCode, message: &str) -> Response {
    let mut r = page(
        "Taisce sign-in",
        &format!("<h1>Can’t sign in</h1><p>{}</p><p id=\"msg\"></p><button id=\"go\" hidden></button>", esc(message)),
        &[("mode", "error")],
    );
    *r.status_mut() = status;
    r
}
