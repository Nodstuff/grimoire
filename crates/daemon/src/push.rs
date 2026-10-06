//! APNs push for the Taisce app (SERVER mode).
//!
//! - `POST /api/devices` `{token, platform: "ios", env?: "sandbox"|"production",
//!   app_version?}` registers (or refreshes) a device token for the
//!   authenticated user; `env` defaults to `--apns-env`. `DELETE
//!   /api/devices/{token}` removes it (404 when the user holds no such
//!   token). Only the owner's own app (`auth::APP_REDIRECT_SCHEME`) may
//!   register: a connector's token is refused.
//! - The sender follows the change journal ([`Feed`]): when the head moves,
//!   every active device owes a silent nudge (`{"aps":{"content-available":1},
//!   "seq":N}`, push type `background`, priority 5, collapse id `changes`),
//!   coalesced to at most one per device per [`COALESCE`]. The app answers
//!   with one bounded catch-up and reconciles its local due alerts.
//! - APNs 410, `BadDeviceToken`, `Unregistered` and `DeviceTokenNotForTopic`
//!   disable the token (kept, with its last error) until the app registers
//!   it again; any other failure is recorded and the next change retries.
//! - Auth is token-based: an ES256 JWT (`kid` = key id, `iss` = team id)
//!   signed with the .p8 key via ring, reused for [`JWT_REFRESH`] (Apple
//!   refuses one older than an hour and throttles refreshing more often than
//!   every 20 minutes).
//!
//! Visible due alerts are not sent from here (the app schedules local
//! notifications). The one visible push is `Push::ShareComment`: someone
//! commented on one of the person's share links (`shares.rs`).

use crate::auth::Authenticated;
use crate::changes::Feed;
use crate::store_ext::with_store;
use taisce_store::Scope;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, post};
use axum::{Extension, Json, Router};
use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

/// At most one change nudge per device per window.
pub const COALESCE: Duration = Duration::from_secs(30);
/// How long a provider JWT is reused.
pub const JWT_REFRESH: i64 = 50 * 60;
/// A queued nudge APNs still delivers to a device that comes back online
/// within this long (the collapse id keeps only the newest).
const EXPIRATION: i64 = 3600;
const PRODUCTION_HOST: &str = "https://api.push.apple.com";
const SANDBOX_HOST: &str = "https://api.sandbox.push.apple.com";

/// `serve` flags; APNs is off unless key, key id and team id are all set.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ApnsArgs {
    /// APNs auth key (.p8, PEM) file.
    #[arg(long, env = "TAISCE_APNS_KEY_FILE")]
    pub apns_key_file: Option<std::path::PathBuf>,
    /// APNs auth key PEM content (instead of --apns-key-file; literal `\n`
    /// escapes and bare base64 are accepted).
    #[arg(long, env = "TAISCE_APNS_KEY", hide_env_values = true)]
    pub apns_key: Option<String>,
    #[arg(long, env = "TAISCE_APNS_KEY_ID")]
    pub apns_key_id: Option<String>,
    #[arg(long, env = "TAISCE_APNS_TEAM_ID")]
    pub apns_team_id: Option<String>,
    /// The app's bundle id.
    #[arg(long, env = "TAISCE_APNS_TOPIC", default_value = "ie.null.taisce")]
    pub apns_topic: String,
    /// The env assumed for a registration that names none.
    #[arg(long, env = "TAISCE_APNS_ENV", default_value = "production", value_parser = ["production", "sandbox"])]
    pub apns_env: String,
}

pub struct ApnsConfig {
    pub signer: Signer,
    pub topic: String,
    pub default_env: String,
}

impl ApnsArgs {
    /// None = APNs disabled (nothing configured). A partial or unreadable
    /// configuration is an error the caller logs.
    pub fn config(&self) -> anyhow::Result<Option<ApnsConfig>> {
        let pem = match (&self.apns_key, &self.apns_key_file) {
            (Some(k), _) if !k.trim().is_empty() => Some(k.clone()),
            (_, Some(f)) => Some(
                std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("--apns-key-file {}: {e}", f.display()))?,
            ),
            _ => None,
        };
        let some = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        match (pem, some(&self.apns_key_id), some(&self.apns_team_id)) {
            (None, None, None) => Ok(None),
            (Some(pem), Some(kid), Some(team)) => Ok(Some(ApnsConfig {
                signer: Signer::from_pem(&pem, &kid, &team)?,
                topic: self.apns_topic.clone(),
                default_env: self.apns_env.clone(),
            })),
            _ => anyhow::bail!("APNs needs all of a key (--apns-key-file or TAISCE_APNS_KEY), --apns-key-id and --apns-team-id"),
        }
    }
}

// ---- the provider JWT ----

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// Signs APNs provider tokens with the team's ES256 key.
pub struct Signer {
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    rng: SystemRandom,
}

impl Signer {
    /// A PKCS#8 P-256 key: PEM, PEM with literal `\n` escapes, or bare base64.
    pub fn from_pem(pem: &str, key_id: &str, team_id: &str) -> anyhow::Result<Self> {
        let pem = pem.replace("\\n", "\n");
        let b64: String = pem
            .lines()
            .filter(|l| !l.trim_start().starts_with("-----"))
            .flat_map(|l| l.chars().filter(|c| !c.is_whitespace()))
            .collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| anyhow::anyhow!("APNs key: not PEM/base64"))?;
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng)
            .map_err(|e| anyhow::anyhow!("APNs key: not a PKCS#8 P-256 key ({e})"))?;
        Ok(Self { key, key_id: key_id.to_string(), team_id: team_id.to_string(), rng })
    }

    /// `header.claims.signature` issued at `iat` (unix seconds).
    pub fn sign(&self, iat: i64) -> anyhow::Result<String> {
        let header = b64url(json!({"alg": "ES256", "kid": self.key_id}).to_string().as_bytes());
        let claims = b64url(json!({"iss": self.team_id, "iat": iat}).to_string().as_bytes());
        let input = format!("{header}.{claims}");
        let sig = self
            .key
            .sign(&self.rng, input.as_bytes())
            .map_err(|_| anyhow::anyhow!("APNs JWT signing failed"))?;
        Ok(format!("{input}.{}", b64url(sig.as_ref())))
    }
}

/// The current provider token, re-signed once it is [`JWT_REFRESH`] old or
/// APNs refused it.
pub struct TokenCache {
    signer: Signer,
    cur: Mutex<Option<(String, i64)>>,
}

impl TokenCache {
    pub fn new(signer: Signer) -> Self {
        Self { signer, cur: Mutex::new(None) }
    }

    pub fn get(&self, now: i64) -> anyhow::Result<String> {
        let mut cur = self.cur.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((jwt, iat)) = cur.as_ref()
            && now - iat < JWT_REFRESH
            && now >= *iat
        {
            return Ok(jwt.clone());
        }
        let jwt = self.signer.sign(now)?;
        *cur = Some((jwt.clone(), now));
        Ok(jwt)
    }

    pub fn invalidate(&self) {
        *self.cur.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

// ---- what is sent, and what APNs said ----

/// One push: a silent change nudge, or a visible alert that someone
/// commented on one of the person's share links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Push {
    /// Silent: the journal head moved to `seq`.
    Changes { seq: i64 },
    /// Visible: "<author> commented on <title>". The app routes a tap on
    /// `kind: share_comment` to that doc's link comments.
    ShareComment { title: String, author: String, doc_id: uuid::Uuid, share_id: uuid::Uuid },
}

impl Push {
    pub fn payload(&self) -> String {
        match self {
            Push::Changes { seq } => json!({"aps": {"content-available": 1}, "seq": seq}).to_string(),
            Push::ShareComment { title, author, doc_id, share_id } => json!({
                "aps": {
                    "alert": {"title": title, "body": format!("{author} commented on {title}")},
                    "sound": "default",
                    "thread-id": format!("share:{share_id}"),
                },
                "kind": "share_comment",
                "doc_id": doc_id,
                "share_id": share_id,
            })
            .to_string(),
        }
    }
    pub fn push_type(&self) -> &'static str {
        match self {
            Push::Changes { .. } => "background",
            Push::ShareComment { .. } => "alert",
        }
    }
    pub fn priority(&self) -> &'static str {
        match self {
            Push::Changes { .. } => "5",
            Push::ShareComment { .. } => "10",
        }
    }
    /// Change nudges collapse into the newest; every comment alert shows.
    pub fn collapse_id(&self) -> Option<&'static str> {
        match self {
            Push::Changes { .. } => Some("changes"),
            Push::ShareComment { .. } => None,
        }
    }
}

/// Send a share-comment alert to every active device of `owner`, recording
/// each outcome as the change nudges do. Fire and forget (spawned).
pub async fn notify_share_comment<S: Sender>(store: taisce_store::SharedStore, sender: Arc<S>, owner: uuid::Uuid, push: Push) {
    let devices = with_store(&store, Scope::System, move |s| s.push_devices_for_user(owner)).await;
    let devices = match devices {
        Ok(d) => d.into_iter().filter(|d| d.disabled_at.is_none()).collect::<Vec<_>>(),
        Err(e) => return tracing::warn!("push: reading devices failed: {e}"),
    };
    for d in devices {
        let out = sender.send(&d.env, &d.token, &push).await;
        let (err, disable) = match &out {
            Outcome::Delivered => (None, false),
            Outcome::Gone(m) => (Some(m.clone()), true),
            Outcome::Failed(m) => (Some(m.clone()), false),
        };
        let t = d.token.clone();
        if let Err(e) =
            with_store(&store, Scope::System, move |s| s.push_device_result(&t, err.as_deref(), disable, crate::auth::now())).await
        {
            tracing::warn!("push: recording the outcome failed: {e}");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// The token is dead: disable it.
    Gone(String),
    /// Anything else: record it; the next change tries again.
    Failed(String),
}

/// Read an APNs response: the status and the `{"reason"}` body.
pub fn classify(status: u16, body: &str) -> Outcome {
    if status == 200 {
        return Outcome::Delivered;
    }
    let reason = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_default();
    let msg = if reason.is_empty() { format!("{status}") } else { format!("{status} {reason}") };
    let dead = status == 410 || matches!(reason.as_str(), "BadDeviceToken" | "Unregistered" | "DeviceTokenNotForTopic");
    if dead { Outcome::Gone(msg) } else { Outcome::Failed(msg) }
}

/// Where pushes go; the real one is [`ApnsSender`], tests inject their own.
pub trait Sender: Send + Sync + 'static {
    fn send(&self, env: &str, token: &str, push: &Push) -> impl Future<Output = Outcome> + Send;
}

pub struct ApnsSender {
    client: reqwest::Client,
    jwt: TokenCache,
    topic: String,
}

impl ApnsSender {
    pub fn new(cfg: ApnsConfig) -> anyhow::Result<Self> {
        crate::install_crypto_provider();
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("taisce/", env!("CARGO_PKG_VERSION"), " (apns)"))
            .build()?;
        Ok(Self { client, jwt: TokenCache::new(cfg.signer), topic: cfg.topic })
    }
}

impl Sender for ApnsSender {
    async fn send(&self, env: &str, token: &str, push: &Push) -> Outcome {
        let now = crate::auth::now();
        let jwt = match self.jwt.get(now) {
            Ok(j) => j,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        let host = if env == "sandbox" { SANDBOX_HOST } else { PRODUCTION_HOST };
        let mut req = self
            .client
            .post(format!("{host}/3/device/{token}"))
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", &self.topic)
            .header("apns-push-type", push.push_type())
            .header("apns-priority", push.priority())
            .header("apns-expiration", (now + EXPIRATION).to_string())
            .header("content-type", "application/json");
        if let Some(c) = push.collapse_id() {
            req = req.header("apns-collapse-id", c);
        }
        let res = req.body(push.payload()).send().await;
        let out = match res {
            Ok(r) => {
                let status = r.status().as_u16();
                classify(status, &r.text().await.unwrap_or_default())
            }
            Err(e) => Outcome::Failed(format!("transport: {e}")),
        };
        if let Outcome::Failed(m) = &out
            && (m.contains("ExpiredProviderToken") || m.contains("InvalidProviderToken"))
        {
            self.jwt.invalidate();
        }
        out
    }
}

// ---- coalescing ----

#[derive(Debug, Default)]
struct Slot {
    last: Option<Instant>,
    pending: bool,
}

/// Which devices owe a nudge, and when: a change marks every device
/// pending; a pending device is due once its window since the last send
/// has passed. Pure: the caller passes the clock.
pub struct Coalescer {
    window: Duration,
    slots: HashMap<String, Slot>,
}

impl Coalescer {
    pub fn new(window: Duration) -> Self {
        Self { window, slots: HashMap::new() }
    }

    /// A change landed: every token in `active` owes a nudge; tokens no
    /// longer active are forgotten.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn changed(&mut self, active: impl IntoIterator<Item = String>) {
        self.changed_for(active.into_iter().map(|t| (t, true)));
    }

    /// A change landed that only some devices may hear about (ADR 0004: a
    /// device's user must be able to see a changed doc): `(token, owes)`
    /// for every active token; tokens no longer active are forgotten.
    pub fn changed_for(&mut self, active: impl IntoIterator<Item = (String, bool)>) {
        let mut next = HashMap::new();
        for (t, owes) in active {
            let mut slot = self.slots.remove(&t).unwrap_or_default();
            slot.pending |= owes;
            next.insert(t, slot);
        }
        self.slots = next;
    }

    /// The tokens due at `now`, marked sent.
    pub fn take_due(&mut self, now: Instant) -> Vec<String> {
        let mut due: Vec<String> = Vec::new();
        for (t, s) in self.slots.iter_mut() {
            if s.pending && s.last.is_none_or(|l| now >= l + self.window) {
                s.pending = false;
                s.last = Some(now);
                due.push(t.clone());
            }
        }
        due.sort();
        due
    }

    /// When the next pending device comes due.
    pub fn next_due(&self) -> Option<Instant> {
        self.slots
            .values()
            .filter(|s| s.pending)
            .map(|s| s.last.map_or_else(Instant::now, |l| l + self.window))
            .min()
    }
}

/// Follow the journal head and nudge every active device, coalesced.
pub async fn push_loop<S: Sender>(store: taisce_store::SharedStore, feed: Feed, sender: Arc<S>, window: Duration) {
    let mut head = feed.subscribe();
    // a boot is not a change: start from the journal as it stands
    // the fan-out sees every user's rows: System, then per-user filtering
    let mut seen = with_store(&store, Scope::System, |s| s.latest_change_seq()).await.unwrap_or(0);
    let mut co = Coalescer::new(window);
    let mut envs: HashMap<String, String> = HashMap::new();
    loop {
        let next = co.next_due();
        tokio::select! {
            r = head.changed() => {
                if r.is_err() {
                    return;
                }
                let seq = *head.borrow_and_update();
                if seq != seen {
                    let since = seen;
                    seen = seq;
                    // a device hears about a change only if its user can see
                    // one of the changed docs (or a row addressed to them)
                    let got = with_store(&store, Scope::System, move |s| {
                        Ok::<_, taisce_store::StoreError>((s.push_devices_active()?, s.change_audience(since)?))
                    })
                    .await;
                    match got {
                        Ok((ds, audience)) => {
                            envs = ds.iter().map(|d| (d.token.clone(), d.env.clone())).collect();
                            co.changed_for(ds.into_iter().map(|d| {
                                let owes = audience.contains(&d.user_id);
                                (d.token, owes)
                            }));
                        }
                        Err(e) => tracing::warn!("push: reading devices failed: {e}"),
                    }
                }
            }
            _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {}
        }
        for token in co.take_due(Instant::now()) {
            let env = envs.get(&token).cloned().unwrap_or_else(|| "production".into());
            let out = sender.send(&env, &token, &Push::Changes { seq: seen }).await;
            let (err, disable) = match &out {
                Outcome::Delivered => (None, false),
                Outcome::Gone(m) => (Some(m.clone()), true),
                Outcome::Failed(m) => (Some(m.clone()), false),
            };
            match &out {
                Outcome::Delivered => tracing::debug!(seq = seen, "push: nudge delivered"),
                Outcome::Gone(m) => tracing::info!("push: token disabled: {m}"),
                Outcome::Failed(m) => tracing::warn!("push: send failed: {m}"),
            }
            let t = token.clone();
            if let Err(e) =
                with_store(&store, Scope::System, move |s| s.push_device_result(&t, err.as_deref(), disable, crate::auth::now())).await
            {
                tracing::warn!("push: recording the outcome failed: {e}");
            }
        }
    }
}

// ---- the device routes ----

#[derive(Clone)]
pub struct DevicesState {
    pub store: taisce_store::SharedStore,
    /// `--apns-env`: the env of a registration that names none.
    pub default_env: String,
}

fn error(status: StatusCode, msg: impl std::fmt::Display) -> Response {
    (status, Json(json!({"error": msg.to_string()}))).into_response()
}

#[derive(Deserialize)]
struct Register {
    token: String,
    #[serde(default = "ios")]
    platform: String,
    env: Option<String>,
    #[serde(default)]
    app_version: String,
}

fn ios() -> String {
    "ios".into()
}

/// A token as APNs addresses it: hex, lowercased.
fn normalise_token(t: &str) -> Option<String> {
    let t = t.trim();
    (t.len() >= 16 && t.len() <= 200 && t.len() % 2 == 0 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| t.to_ascii_lowercase())
}

/// The app's user, or the refusal: SERVER mode only, the owner's app only.
fn device_user(who: Option<Extension<Authenticated>>) -> Result<uuid::Uuid, Response> {
    match who {
        None => Err(error(StatusCode::FORBIDDEN, "push devices need a signed-in app (SERVER mode)")),
        Some(Extension(w)) if !w.owner_app || w.web_session => {
            Err(error(StatusCode::FORBIDDEN, "only the Taisce app registers push devices"))
        }
        Some(Extension(w)) => Ok(w.user_id),
    }
}

async fn register(
    State(st): State<DevicesState>,
    who: Option<Extension<Authenticated>>,
    body: Result<Json<Register>, JsonRejection>,
) -> Response {
    let user = match device_user(who) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Json(b) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.body_text()),
    };
    let Some(token) = normalise_token(&b.token) else {
        return error(StatusCode::BAD_REQUEST, "token: expected the APNs device token in hex");
    };
    if b.platform != "ios" {
        return error(StatusCode::BAD_REQUEST, "platform: only \"ios\"");
    }
    let env = b.env.unwrap_or(st.default_env);
    if env != "sandbox" && env != "production" {
        return error(StatusCode::BAD_REQUEST, "env: \"sandbox\" or \"production\"");
    }
    if b.app_version.len() > 64 {
        return error(StatusCode::BAD_REQUEST, "app_version: at most 64 bytes");
    }
    let now = crate::auth::now();
    with_store(&st.store, Scope::User(user), move |s| match s.push_device_upsert(user, &token, &b.platform, &env, &b.app_version, now) {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    })
    .await
}

async fn unregister(
    State(st): State<DevicesState>,
    who: Option<Extension<Authenticated>>,
    Path(token): Path<String>,
) -> Response {
    let user = match device_user(who) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(token) = normalise_token(&token) else {
        return error(StatusCode::BAD_REQUEST, "token: expected the APNs device token in hex");
    };
    with_store(&st.store, Scope::User(user), move |s| match s.push_device_delete(user, &token) {
        Ok(true) => Json(json!({"ok": true})).into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "no such device"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
    })
    .await
}

pub fn router(state: DevicesState) -> Router {
    Router::new()
        .route("/api/devices", post(register))
        .route("/api/devices/{token}", delete(unregister))
        .layer(axum::extract::DefaultBodyLimit::max(4 * 1024))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use taisce_store::{BlockStore, PrincipalKind};
    use serde_json::Value;
    use tower::ServiceExt;

    const TOKEN: &str = "00fc13adff785122b4ad28809a3420982341241421348097878e577c991de8f0";

    fn throwaway_pem() -> (String, Vec<u8>) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref());
        let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap()).collect();
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", lines.join("\n"));
        (pem, ring::signature::KeyPair::public_key(&kp).as_ref().to_vec())
    }

    fn decode(part: &str) -> Value {
        serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn jwt_header_claims_and_signature() {
        let (pem, public) = throwaway_pem();
        // every accepted spelling of the key loads
        for k in [pem.clone(), pem.replace('\n', "\\n"), pem.lines().filter(|l| !l.starts_with("-----")).collect()] {
            Signer::from_pem(&k, "KEYID12345", "TEAMID1234").unwrap();
        }
        assert!(Signer::from_pem("not a key", "k", "t").is_err());
        let s = Signer::from_pem(&pem, "KEYID12345", "TEAMID1234").unwrap();
        let jwt = s.sign(1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(decode(parts[0]), json!({"alg": "ES256", "kid": "KEYID12345"}));
        assert_eq!(decode(parts[1]), json!({"iss": "TEAMID1234", "iat": 1_700_000_000}));
        // JWS ES256: the raw 64-byte r||s over `header.claims`
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        assert_eq!(sig.len(), 64);
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, &public)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .expect("verifies with the key's public half");
    }

    #[test]
    fn jwt_is_reused_for_fifty_minutes() {
        let (pem, _) = throwaway_pem();
        let c = TokenCache::new(Signer::from_pem(&pem, "k", "t").unwrap());
        let iat = |j: &str| decode(j.split('.').nth(1).unwrap())["iat"].as_i64().unwrap();
        let a = c.get(1000).unwrap();
        assert_eq!(c.get(1000 + JWT_REFRESH - 1).unwrap(), a);
        assert_eq!(iat(&c.get(1000 + JWT_REFRESH).unwrap()), 1000 + JWT_REFRESH);
        c.invalidate();
        assert_eq!(iat(&c.get(1000 + JWT_REFRESH + 5).unwrap()), 1000 + JWT_REFRESH + 5);
    }

    #[test]
    fn apns_responses_classify() {
        assert_eq!(classify(200, ""), Outcome::Delivered);
        assert_eq!(classify(410, r#"{"reason":"Unregistered","timestamp":1}"#), Outcome::Gone("410 Unregistered".into()));
        assert_eq!(classify(400, r#"{"reason":"BadDeviceToken"}"#), Outcome::Gone("400 BadDeviceToken".into()));
        assert_eq!(classify(400, r#"{"reason":"DeviceTokenNotForTopic"}"#), Outcome::Gone("400 DeviceTokenNotForTopic".into()));
        assert_eq!(classify(403, r#"{"reason":"ExpiredProviderToken"}"#), Outcome::Failed("403 ExpiredProviderToken".into()));
        assert_eq!(classify(429, r#"{"reason":"TooManyRequests"}"#), Outcome::Failed("429 TooManyRequests".into()));
        assert_eq!(classify(503, "junk"), Outcome::Failed("503".into()));
        let p = Push::Changes { seq: 42 };
        assert_eq!(serde_json::from_str::<Value>(&p.payload()).unwrap(), json!({"aps": {"content-available": 1}, "seq": 42}));
        assert_eq!((p.push_type(), p.priority(), p.collapse_id()), ("background", "5", Some("changes")));
    }

    #[test]
    fn coalescing_is_one_nudge_per_device_per_window() {
        let w = Duration::from_secs(30);
        let t0 = Instant::now();
        let mut c = Coalescer::new(w);
        assert_eq!(c.next_due(), None, "nothing pending");
        c.changed(["a".to_string(), "b".to_string()]);
        assert_eq!(c.take_due(t0), vec!["a", "b"], "first change: immediately");
        assert_eq!(c.next_due(), None);
        // a burst inside the window: one nudge each, at the window's end
        c.changed(["a".to_string(), "b".to_string()]);
        c.changed(["a".to_string(), "b".to_string()]);
        assert!(c.take_due(t0 + Duration::from_secs(10)).is_empty());
        assert_eq!(c.next_due(), Some(t0 + w));
        assert_eq!(c.take_due(t0 + w), vec!["a", "b"]);
        assert!(c.take_due(t0 + w * 2).is_empty(), "no change, no nudge");
        // a device registered mid-window is due at once; a gone one is forgotten
        c.changed(["a".to_string(), "c".to_string()]);
        assert_eq!(c.take_due(t0 + w + Duration::from_secs(1)), vec!["c"]);
        assert_eq!(c.next_due(), Some(t0 + w * 2));
        assert_eq!(c.take_due(t0 + w * 2), vec!["a"]);
        c.changed(["a".to_string()]);
        assert!(!c.slots.contains_key("b"));
    }

    #[test]
    fn tokens_are_hex_and_lowercased() {
        assert_eq!(normalise_token(&TOKEN.to_uppercase()).as_deref(), Some(TOKEN));
        for bad in ["", "abc", "zz00zz00zz00zz00", &"a".repeat(202), "00fc13adff785122b"] {
            assert!(normalise_token(bad).is_none(), "{bad:?}");
        }
    }

    struct T {
        app: Router,
        store: taisce_store::SharedStore,
        tom: uuid::Uuid,
        ann: uuid::Uuid,
    }

    fn who(user: uuid::Uuid, owner_app: bool) -> Authenticated {
        Authenticated {
            user_id: user,
            grant_id: uuid::Uuid::now_v7(),
            client_id: "c".into(),
            principal: "claude:test".into(),
            owner_app,
            human: uuid::Uuid::now_v7(),
            instance_owner: false,
            web_session: false,
        }
    }

    fn setup() -> T {
        let mut s = taisce_store::SqliteStore::open_in_memory().unwrap();
        let p = s.create_principal(PrincipalKind::Human, "tom", None).unwrap().id;
        let tom = s.auth_ensure_owner(p, "tom", 1).unwrap().id;
        // another user: deleting needs no row of theirs, only a different id
        let ann = uuid::Uuid::now_v7();
        let store = taisce_store::SharedStore::new(s);
        let app = router(DevicesState { store: store.clone(), default_env: "production".into() });
        T { app, store, tom, ann }
    }

    async fn call(t: &T, method: &str, uri: &str, as_: Option<Authenticated>, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        if let Some(w) = as_ {
            req = req.extension(w);
        }
        let req = req.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string()))).unwrap();
        let res = t.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn devices_register_refresh_delete_scoped_to_the_user() {
        let t = setup();
        let body = json!({"token": TOKEN.to_uppercase(), "platform": "ios", "env": "sandbox", "app_version": "1.0 (3)"});
        // no sign-in (LOCAL mode) and a connector's token are refused
        assert_eq!(call(&t, "POST", "/api/devices", None, Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&t, "POST", "/api/devices", Some(who(t.tom, false)), Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        let (status, v) = call(&t, "POST", "/api/devices", Some(who(t.tom, true)), Some(body.clone())).await;
        assert_eq!((status, v), (StatusCode::OK, json!({"ok": true})));
        {
            let s = t.store.lock(taisce_store::Scope::System);
            let d = s.push_devices_for_user(t.tom).unwrap();
            assert_eq!(d.len(), 1);
            assert_eq!((d[0].token.as_str(), d[0].env.as_str(), d[0].app_version.as_str()), (TOKEN, "sandbox", "1.0 (3)"));
        }
        // env defaults to --apns-env; a re-send refreshes in place
        let (status, _) = call(&t, "POST", "/api/devices", Some(who(t.tom, true)), Some(json!({"token": TOKEN}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(t.store.lock(taisce_store::Scope::System).push_device(TOKEN).unwrap().unwrap().env, "production");
        // bad input is a 400 with an error
        for b in [
            json!({"token": "nothex!"}),
            json!({"token": TOKEN, "platform": "android"}),
            json!({"token": TOKEN, "env": "staging"}),
            json!({"nope": 1}),
        ] {
            let (status, v) = call(&t, "POST", "/api/devices", Some(who(t.tom, true)), Some(b.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{b}");
            assert!(v["error"].is_string());
        }
        // another user cannot delete it; its owner can, once
        let uri = format!("/api/devices/{TOKEN}");
        assert_eq!(call(&t, "DELETE", &uri, Some(who(t.ann, true)), None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&t, "DELETE", &uri, None, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&t, "DELETE", &uri, Some(who(t.tom, true)), None).await.0, StatusCode::OK);
        assert_eq!(call(&t, "DELETE", &uri, Some(who(t.tom, true)), None).await.0, StatusCode::NOT_FOUND);
        assert!(t.store.lock(taisce_store::Scope::System).push_device(TOKEN).unwrap().is_none());
    }

    /// Records sends; answers `Gone` for tokens starting `dead`.
    #[derive(Default)]
    struct Mock {
        sent: Mutex<Vec<(String, String, i64)>>,
    }

    impl Sender for Mock {
        async fn send(&self, env: &str, token: &str, push: &Push) -> Outcome {
            // a share-comment alert records as seq -1
            let seq = match push {
                Push::Changes { seq } => *seq,
                Push::ShareComment { .. } => -1,
            };
            self.sent.lock().unwrap().push((env.to_string(), token.to_string(), seq));
            if token.starts_with("dead") {
                Outcome::Gone("410 Unregistered".into())
            } else {
                Outcome::Delivered
            }
        }
    }

    async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A link comment is a visible alert to the share owner's active devices
    /// only, in the payload the app routes on (`kind: share_comment`).
    #[tokio::test]
    async fn a_share_comment_alerts_only_the_owners_devices() {
        let t = setup();
        let other = t.store.lock(taisce_store::Scope::System).auth_add_user("Aoife", 1).unwrap().id;
        let (mine, off, theirs) = ("aa".repeat(32), "bb".repeat(32), "cc".repeat(32));
        {
            let mut s = t.store.lock(taisce_store::Scope::System);
            s.push_device_upsert(t.tom, &mine, "ios", "sandbox", "", 1).unwrap();
            s.push_device_upsert(t.tom, &off, "ios", "production", "", 1).unwrap();
            s.push_device_result(&off, Some("410"), true, 2).unwrap();
            s.push_device_upsert(other, &theirs, "ios", "production", "", 1).unwrap();
        }
        let (doc, share) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
        let push = Push::ShareComment { title: "Plan".into(), author: "Aoife".into(), doc_id: doc, share_id: share };
        let v: serde_json::Value = serde_json::from_str(&push.payload()).unwrap();
        assert_eq!(v["aps"]["alert"]["body"], "Aoife commented on Plan");
        assert_eq!(v["aps"]["alert"]["title"], "Plan");
        assert_eq!(v["aps"]["thread-id"], format!("share:{share}"));
        assert_eq!((v["kind"].as_str(), v["doc_id"].as_str(), v["share_id"].as_str()), (Some("share_comment"), Some(doc.to_string().as_str()), Some(share.to_string().as_str())));
        assert_eq!((push.push_type(), push.priority(), push.collapse_id()), ("alert", "10", None));
        let mock = Arc::new(Mock::default());
        notify_share_comment(t.store.clone(), mock.clone(), t.tom, push).await;
        assert_eq!(*mock.sent.lock().unwrap(), vec![("sandbox".to_string(), mine, -1)]);
    }

    #[tokio::test]
    async fn the_loop_nudges_on_change_coalesces_and_disables_gone_tokens() {
        let t = setup();
        let live = "aa".repeat(32);
        let dead = format!("dead{}", "00".repeat(30));
        {
            let mut s = t.store.lock(taisce_store::Scope::System);
            s.push_device_upsert(t.tom, &live, "ios", "sandbox", "", 1).unwrap();
            s.push_device_upsert(t.tom, &dead, "ios", "production", "", 1).unwrap();
            let author = s.list_principals().unwrap()[0].id;
            s.create_doc("before boot", None, author).unwrap();
        }
        let feed = Feed::new(&t.store);
        let mock = Arc::new(Mock::default());
        let window = Duration::from_millis(300);
        let task = tokio::spawn(push_loop(t.store.clone(), feed, mock.clone(), window));
        let author = t.store.lock(taisce_store::Scope::System).list_principals().unwrap()[0].id;
        // booting is not a change
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(mock.sent.lock().unwrap().is_empty(), "no nudge at boot");
        t.store.lock(taisce_store::Scope::System).create_doc("one", None, author).unwrap();
        wait_for("the first nudges", || mock.sent.lock().unwrap().len() == 2).await;
        {
            let sent = mock.sent.lock().unwrap();
            assert!(sent.contains(&("sandbox".into(), live.clone(), 2)));
            assert!(sent.contains(&("production".into(), dead.clone(), 2)));
        }
        wait_for("the gone token disabled", || {
            t.store.lock(taisce_store::Scope::System).push_device(&dead).unwrap().unwrap().disabled_at.is_some()
        })
        .await;
        let d = t.store.lock(taisce_store::Scope::System).push_device(&dead).unwrap().unwrap();
        assert_eq!(d.last_error.as_deref(), Some("410 Unregistered"));
        // a burst inside the window: one more nudge, to the live token only, carrying the latest seq
        for i in 0..3 {
            t.store.lock(taisce_store::Scope::System).create_doc(&format!("burst {i}"), None, author).unwrap();
        }
        wait_for("the coalesced nudge", || mock.sent.lock().unwrap().len() == 3).await;
        tokio::time::sleep(window + Duration::from_millis(200)).await;
        let sent = mock.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert_eq!(sent[2].1, live);
        assert_eq!(sent[2].2, 5, "the newest head");
        assert!(t.store.lock(taisce_store::Scope::System).push_device(&live).unwrap().unwrap().last_sent_at.is_some());
        task.abort();
    }
}

/// Live check against the APNs sandbox with a real key and a bogus token:
/// `400 BadDeviceToken` proves TLS, HTTP/2 and the JWT were accepted (a bad
/// JWT answers 403). Run by hand:
/// `TAISCE_APNS_TEST_KEY_FILE=… TAISCE_APNS_KEY_ID=… TAISCE_APNS_TEAM_ID=… cargo test -p taisce live_sandbox -- --ignored`
#[cfg(test)]
#[tokio::test]
#[ignore]
async fn live_sandbox_accepts_the_jwt() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} unset"));
    let args = ApnsArgs {
        apns_key_file: Some(var("TAISCE_APNS_TEST_KEY_FILE").into()),
        apns_key_id: Some(var("TAISCE_APNS_KEY_ID")),
        apns_team_id: Some(var("TAISCE_APNS_TEAM_ID")),
        apns_topic: "ie.null.taisce".into(),
        apns_env: "sandbox".into(),
        ..Default::default()
    };
    let sender = ApnsSender::new(args.config().unwrap().unwrap()).unwrap();
    let out = sender.send("sandbox", &"00".repeat(32), &Push::Changes { seq: 0 }).await;
    assert_eq!(out, Outcome::Gone("400 BadDeviceToken".into()));
}
