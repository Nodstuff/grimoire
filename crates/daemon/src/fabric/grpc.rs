//! The runtime channel's service (`taisce.runtime.v1.Runtime`, ADR 0006):
//! what a runtime such as Workbox dials. It listens on its own loopback
//! port (`--runtime-grpc-port`), cleartext HTTP/2 behind the proxy, apart
//! from the axum app and its `require_auth` layer.
//!
//! Sign-in is this service's own: every call carries `authorization: Bearer
//! trt_…`, a runtime credential, resolved to the runtime and through it to
//! one person. Nothing else opens the service (a personal access token or
//! an OAuth token is just an unknown credential here), and the credential
//! opens nothing else (`auth::require_auth` never looks one up). Everything
//! a stream then does runs for that person: the hub writes in their
//! `Scope::User`, keyed to the runtime the credential named, so no message
//! can reach another person's fleet whatever ids it carries.
//!
//! The order of a stream, its limits and how it ends are stated in
//! `proto/taisce/runtime/v1/runtime.proto`; this file enforces them.

use super::hub::{Connected, Hub, Incoming, Kill, Refused};
use crate::auth::ratelimit::{Class, Limiter};
use crate::store_ext::with_store;
use std::time::Duration;
use taisce_runtime_proto::CREDENTIAL_PREFIX;
use taisce_runtime_proto::v1::runtime_server::{Runtime, RuntimeServer};
use taisce_runtime_proto::v1::{self as pb, down, up};
use taisce_store::fabric::{FabricRuntime, FabricSession, MAX_SESSIONS_PER_RUNTIME, SessionStatus};
use taisce_store::{Scope, SharedStore};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

/// What the Welcome asks for; a stream silent for three of these is ended.
pub const HEARTBEAT: Duration = Duration::from_secs(20);
/// How long a stream may take to say Hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How often an open stream's runtime is checked to be still enrolled.
const RECHECK: Duration = Duration::from_secs(60);
/// The largest message taken (a snapshot of 200 full sessions fits).
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Accounts in one headroom report.
const MAX_ACCOUNTS: usize = 32;
/// Messages a stream may send: a burst, then a steady rate.
const RATE_BURST: f64 = 400.0;
const RATE_PER_SEC: f64 = 50.0;
/// The latest `since` taken (2100-01-01): a wild clock is refused.
const MAX_SINCE: i64 = 4_102_444_800;

#[derive(Clone)]
pub struct RuntimeService {
    pub hub: Hub,
    pub store: SharedStore,
    /// Failed sign-ins are counted per address here.
    pub limiter: Limiter,
    /// Take the caller's address from the proxy's `x-forwarded-for`.
    pub trusted_proxy: bool,
    pub heartbeat: Duration,
}

impl RuntimeService {
    pub fn server(self) -> RuntimeServer<Self> {
        RuntimeServer::new(self).max_decoding_message_size(MAX_MESSAGE_BYTES)
    }

    fn client_ip<T>(&self, req: &Request<T>) -> String {
        if self.trusted_proxy
            && let Some(xff) = req.metadata().get("x-forwarded-for").and_then(|v| v.to_str().ok())
            && let Some(last) = xff.rsplit(',').next().map(str::trim).filter(|s| !s.is_empty())
        {
            return last.to_string();
        }
        req.remote_addr().map(|a| a.ip().to_string()).unwrap_or_else(|| "unknown".into())
    }

    /// The runtime behind the call's credential.
    async fn authenticate<T>(&self, req: &Request<T>) -> Result<FabricRuntime, Status> {
        let credential = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, c)| c.trim())
            .filter(|c| c.starts_with(CREDENTIAL_PREFIX));
        let found = match credential {
            Some(c) => {
                let hash = crate::auth::hash_secret(c);
                // before a user is known: the hash is the credential
                with_store(&self.store, Scope::System, move |s| s.fabric_runtime_by_credential(&hash)).await.ok().flatten()
            }
            None => None,
        };
        match found {
            Some(runtime) => Ok(runtime),
            None => {
                let ip = crate::shares::ip_key(&self.client_ip(req));
                tracing::warn!(target: crate::auth::AUDIT, event = "runtime.auth_failed", ip);
                if !self.limiter.allow(Class::RuntimeAuth, &ip) {
                    return Err(Status::resource_exhausted("too many requests; try again shortly"));
                }
                Err(Status::unauthenticated("a runtime credential is required"))
            }
        }
    }
}

#[tonic::async_trait]
impl Runtime for RuntimeService {
    type ConnectStream = ReceiverStream<Result<pb::Down, Status>>;

    async fn connect(&self, req: Request<Streaming<pb::Up>>) -> Result<Response<Self::ConnectStream>, Status> {
        let runtime = self.authenticate(&req).await?;
        let ip = self.client_ip(&req);
        let (tx, rx) = mpsc::channel(8);
        let (hub, heartbeat) = (self.hub.clone(), self.heartbeat);
        tokio::spawn(async move {
            if let Err(status) = stream(&hub, &runtime, req.into_inner(), &tx, heartbeat, &ip).await {
                tracing::info!(runtime = %runtime.id, code = ?status.code(), "fabric: stream ended: {}", status.message());
                let _ = tx.send(Err(status)).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// One stream, from its Hello to its end. Err is the status it ends with;
/// Ok is the runtime hanging up.
async fn stream(
    hub: &Hub,
    runtime: &FabricRuntime,
    mut inbound: Streaming<pb::Up>,
    tx: &mpsc::Sender<Result<pb::Down, Status>>,
    heartbeat: Duration,
    ip: &str,
) -> Result<(), Status> {
    let first = tokio::time::timeout(HELLO_TIMEOUT, inbound.message())
        .await
        .map_err(|_| Status::deadline_exceeded("no Hello within 10 seconds"))?;
    let hello = match first {
        Ok(Some(pb::Up { body: Some(up::Body::Hello(h)), .. })) => h,
        Ok(Some(_)) => return Err(Status::failed_precondition("the first message must be a Hello")),
        Ok(None) | Err(_) => return Ok(()),
    };
    if !hello.runtime_id.is_empty() && hello.runtime_id != runtime.id.to_string() {
        return Err(Status::invalid_argument("hello.runtime_id: not the runtime this credential names"));
    }
    let instance_ok = (1..=64).contains(&hello.instance_id.len())
        && hello.instance_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !instance_ok {
        return Err(Status::invalid_argument("hello.instance_id: 1-64 of [A-Za-z0-9-]"));
    }
    text("hello.version", &hello.version, 0, 64)?;
    let Connected { conn, mut kill, last_up_seq } =
        hub.connect(runtime, &hello.instance_id, &hello.version, crate::auth::now()).await.map_err(refused)?;
    tracing::info!(target: crate::auth::AUDIT, event = "runtime.connect", runtime = %runtime.id, name = runtime.name,
        user = %runtime.user_id, instance = hello.instance_id, version = hello.version, ip);
    let welcome = pb::Welcome { last_up_seq, runtime_id: runtime.id.to_string(), heartbeat_secs: heartbeat.as_secs().max(1) as u32 };
    let ended = match tx.send(Ok(pb::Down { seq: 0, body: Some(down::Body::Welcome(welcome)) })).await {
        Ok(()) => events(hub, runtime, conn, &mut inbound, &mut kill, heartbeat).await,
        // the runtime hung up before its Welcome
        Err(_) => Ok(()),
    };
    hub.disconnect(runtime.user_id, runtime.id, conn, crate::auth::now()).await;
    tracing::info!(target: crate::auth::AUDIT, event = "runtime.disconnect", runtime = %runtime.id, user = %runtime.user_id);
    ended
}

/// Everything after the Welcome.
async fn events(
    hub: &Hub,
    runtime: &FabricRuntime,
    conn: u64,
    inbound: &mut Streaming<pb::Up>,
    kill: &mut tokio::sync::oneshot::Receiver<Kill>,
    heartbeat: Duration,
) -> Result<(), Status> {
    let (user, id) = (runtime.user_id, runtime.id);
    let mut budget = Budget::new(tokio::time::Instant::now());
    let mut recheck = tokio::time::interval_at(tokio::time::Instant::now() + RECHECK, RECHECK);
    let mut snapshot_seen = false;
    loop {
        let up = tokio::select! {
            biased;
            why = &mut *kill => {
                return Err(match why {
                    Ok(Kill::Superseded) => Status::aborted("a newer stream for this runtime took over"),
                    Ok(Kill::Removed) | Err(_) => Status::unauthenticated("this runtime was removed"),
                });
            }
            _ = recheck.tick() => {
                if !hub.still_enrolled(user, id).await {
                    return Err(Status::unauthenticated("this runtime was removed"));
                }
                continue;
            }
            got = tokio::time::timeout(heartbeat * 3, inbound.message()) => match got {
                Err(_) => return Err(Status::deadline_exceeded("no message for three heartbeats")),
                // the runtime hung up, cleanly or not
                Ok(Ok(None)) | Ok(Err(_)) => return Ok(()),
                Ok(Ok(Some(up))) => up,
            },
        };
        if !budget.take(tokio::time::Instant::now()) {
            return Err(Status::resource_exhausted("messages are arriving faster than the hub accepts"));
        }
        let now = crate::auth::now();
        let incoming = match up.body {
            Some(up::Body::Hello(_)) => return Err(Status::failed_precondition("a second Hello on one stream")),
            Some(up::Body::Snapshot(s)) => {
                snapshot_seen = true;
                if s.sessions.len() > MAX_SESSIONS_PER_RUNTIME {
                    return Err(Status::invalid_argument(format!("snapshot.sessions: at most {MAX_SESSIONS_PER_RUNTIME}")));
                }
                let sessions = s.sessions.iter().map(|m| session(m, runtime, now)).collect::<Result<Vec<_>, _>>()?;
                Incoming::Snapshot { replaces_seq: s.replaces_seq, sessions }
            }
            Some(_) if !snapshot_seen => return Err(Status::failed_precondition("the first message after the Hello must be a Snapshot")),
            Some(up::Body::State(s)) => Incoming::State { seq: event_seq(up.seq)?, session: Box::new(session(&s, runtime, now)?) },
            Some(up::Body::Accounts(a)) => Incoming::Accounts { seq: event_seq(up.seq)?, accounts: accounts(&a)? },
            Some(up::Body::Heartbeat(_)) => continue,
            // a body this build does not know (a newer runtime): ignored
            None => continue,
        };
        hub.apply(user, id, conn, incoming, now).await.map_err(refused)?;
    }
}

fn refused(r: Refused) -> Status {
    match r {
        Refused::NotLive => Status::aborted("this stream is no longer the runtime's live one"),
        Refused::TooManySessions => Status::resource_exhausted(format!("a runtime holds at most {MAX_SESSIONS_PER_RUNTIME} sessions")),
        Refused::DuplicateSession(id) => Status::invalid_argument(format!("snapshot.sessions: {id} appears twice")),
        Refused::Store(e) => {
            tracing::warn!("fabric: store error on the runtime channel: {e}");
            Status::unavailable("the hub could not take that; reconnect and try again")
        }
    }
}

/// A token bucket over one stream's messages.
struct Budget {
    tokens: f64,
    at: tokio::time::Instant,
}

impl Budget {
    fn new(at: tokio::time::Instant) -> Self {
        Self { tokens: RATE_BURST, at }
    }

    fn take(&mut self, now: tokio::time::Instant) -> bool {
        self.tokens = (self.tokens + now.saturating_duration_since(self.at).as_secs_f64() * RATE_PER_SEC).min(RATE_BURST);
        self.at = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

// ---- validation: a message becomes what the hub holds, or a refusal ----

/// `field`'s text within its byte limits.
fn text(field: &str, value: &str, min: usize, max: usize) -> Result<String, Status> {
    if value.len() < min || value.len() > max {
        let what = if min > 0 { format!("{min}-{max} bytes") } else { format!("at most {max} bytes") };
        return Err(Status::invalid_argument(format!("{field}: {what}")));
    }
    Ok(value.to_string())
}

/// An event's number: never 0.
fn event_seq(seq: u64) -> Result<u64, Status> {
    if seq == 0 {
        return Err(Status::invalid_argument("seq: an event carries its number (1 or more)"));
    }
    Ok(seq)
}

fn percent(field: &str, value: Option<u32>) -> Result<Option<u8>, Status> {
    match value {
        None => Ok(None),
        Some(p) if p <= 100 => Ok(Some(p as u8)),
        Some(_) => Err(Status::invalid_argument(format!("{field}: 0-100"))),
    }
}

fn status(value: i32) -> Result<SessionStatus, Status> {
    pb::Status::try_from(value)
        .ok()
        .and_then(pb::Status::name)
        .and_then(SessionStatus::parse)
        .ok_or_else(|| Status::invalid_argument("state: one of the eight session states"))
}

/// A reported session as the hub holds it: `runtime`'s, its owner's. The
/// hub stamps `updated_at` again when it takes it.
fn session(m: &pb::SessionState, runtime: &FabricRuntime, now: i64) -> Result<FabricSession, Status> {
    let id = uuid::Uuid::parse_str(&m.session_id).map_err(|_| Status::invalid_argument("session_id: a UUID"))?;
    if !(0..=MAX_SINCE).contains(&m.since) {
        return Err(Status::invalid_argument("since: unix seconds"));
    }
    Ok(FabricSession {
        runtime_id: runtime.id,
        id,
        user_id: runtime.user_id,
        address: text("address", &m.address, 1, 200)?,
        fabric: text("fabric", &m.fabric, 0, 200)?,
        project: text("project", &m.project, 0, 200)?,
        worktree: text("worktree", &m.worktree, 0, 200)?,
        branch: text("branch", &m.branch, 0, 200)?,
        account: text("account", &m.account, 0, 200)?,
        state: status(m.state)?,
        tool: text("tool", &m.tool, 0, 100)?,
        note: text("note", &m.note, 0, 500)?,
        last_line: text("last_line", &m.last_line, 0, 2000)?,
        context_used_pct: percent("context_used_pct", m.context_used_pct)?,
        since: m.since,
        updated_at: now,
    })
}

/// A headroom report as the JSON the viewer routes serve.
fn accounts(m: &pb::Accounts) -> Result<serde_json::Value, Status> {
    if m.accounts.len() > MAX_ACCOUNTS {
        return Err(Status::invalid_argument(format!("accounts: at most {MAX_ACCOUNTS}")));
    }
    let resets = |field: &str, at: i64| match at {
        0 => Ok(None),
        at if (1..=MAX_SINCE).contains(&at) => Ok(Some(at)),
        _ => Err(Status::invalid_argument(format!("{field}: unix seconds, or 0 when unknown"))),
    };
    let mut out = Vec::with_capacity(m.accounts.len());
    for a in &m.accounts {
        out.push(serde_json::json!({
            "name": text("accounts.name", &a.name, 1, 200)?,
            "session_used_pct": percent("accounts.session_used_pct", a.session_used_pct)?,
            "session_resets_at": resets("accounts.session_resets_at", a.session_resets_at)?,
            "week_used_pct": percent("accounts.week_used_pct", a.week_used_pct)?,
            "week_resets_at": resets("accounts.week_resets_at", a.week_resets_at)?,
        }));
    }
    Ok(serde_json::Value::Array(out))
}

/// Serve the runtime channel on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    service: RuntimeService,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<(), tonic::transport::Error> {
    tonic::transport::Server::builder()
        // notice a dead proxy connection; the runtime's own liveness is its
        // Heartbeat, which crosses the proxy
        .http2_keepalive_interval(Some(Duration::from_secs(30)))
        .http2_keepalive_timeout(Some(Duration::from_secs(20)))
        .add_service(service.server())
        .serve_with_incoming_shutdown(tonic::transport::server::TcpIncoming::from(listener), shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> FabricRuntime {
        FabricRuntime {
            id: uuid::Uuid::now_v7(),
            user_id: uuid::Uuid::now_v7(),
            name: "mbp".into(),
            created_at: 1,
            revoked_at: None,
            last_seen_at: None,
            instance_id: None,
            last_up_seq: 0,
            version: String::new(),
            accounts: "[]".into(),
            accounts_at: None,
        }
    }

    fn message() -> pb::SessionState {
        pb::SessionState {
            session_id: uuid::Uuid::now_v7().to_string(),
            address: "taisce/main".into(),
            state: pb::Status::Working as i32,
            since: 1_800_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn a_session_is_keyed_to_the_credentials_runtime_and_owner() {
        let rt = runtime();
        let s = session(&message(), &rt, 77).unwrap();
        assert_eq!((s.runtime_id, s.user_id, s.state, s.updated_at), (rt.id, rt.user_id, SessionStatus::Working, 77));
    }

    #[test]
    fn every_limit_is_enforced_and_names_its_field() {
        let rt = runtime();
        let bad = |edit: &dyn Fn(&mut pb::SessionState), field: &str| {
            let mut m = message();
            edit(&mut m);
            let e = session(&m, &rt, 1).unwrap_err();
            assert_eq!(e.code(), tonic::Code::InvalidArgument, "{field}");
            assert!(e.message().starts_with(field), "{field}: {}", e.message());
        };
        bad(&|m| m.session_id = "pane-1".into(), "session_id");
        bad(&|m| m.address.clear(), "address");
        bad(&|m| m.address = "a".repeat(201), "address");
        bad(&|m| m.fabric = "f".repeat(201), "fabric");
        bad(&|m| m.project = "p".repeat(201), "project");
        bad(&|m| m.worktree = "w".repeat(201), "worktree");
        bad(&|m| m.branch = "b".repeat(201), "branch");
        bad(&|m| m.account = "a".repeat(201), "account");
        bad(&|m| m.tool = "t".repeat(101), "tool");
        bad(&|m| m.note = "n".repeat(501), "note");
        bad(&|m| m.last_line = "l".repeat(2001), "last_line");
        bad(&|m| m.state = 0, "state");
        bad(&|m| m.state = 9, "state");
        bad(&|m| m.context_used_pct = Some(101), "context_used_pct");
        bad(&|m| m.since = -1, "since");
        bad(&|m| m.since = MAX_SINCE + 1, "since");
        // at the limits: taken
        let mut m = message();
        m.address = "a".repeat(200);
        m.last_line = "l".repeat(2000);
        m.context_used_pct = Some(100);
        assert!(session(&m, &rt, 1).is_ok());
        assert_eq!(event_seq(0).unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn every_proto_state_has_a_store_state() {
        for n in 1..=8 {
            let s = status(n).unwrap();
            assert_eq!(Some(s.as_str()), pb::Status::try_from(n).unwrap().name());
        }
    }

    #[test]
    fn accounts_become_the_json_the_routes_serve() {
        let a = pb::Accounts {
            accounts: vec![pb::Account {
                name: "personal".into(),
                session_used_pct: Some(40),
                session_resets_at: 1_800_000_000,
                week_used_pct: None,
                week_resets_at: 0,
            }],
        };
        assert_eq!(
            accounts(&a).unwrap(),
            serde_json::json!([{"name": "personal", "session_used_pct": 40, "session_resets_at": 1_800_000_000,
                "week_used_pct": null, "week_resets_at": null}])
        );
        let many = pb::Accounts { accounts: vec![pb::Account { name: "x".into(), ..Default::default() }; MAX_ACCOUNTS + 1] };
        assert_eq!(accounts(&many).unwrap_err().code(), tonic::Code::InvalidArgument);
        let over = pb::Accounts { accounts: vec![pb::Account { name: "x".into(), week_used_pct: Some(101), ..Default::default() }] };
        assert!(accounts(&over).unwrap_err().message().starts_with("accounts.week_used_pct"));
    }

    #[test]
    fn a_stream_has_a_burst_and_a_steady_rate() {
        let t0 = tokio::time::Instant::now();
        let mut b = Budget::new(t0);
        for _ in 0..RATE_BURST as usize {
            assert!(b.take(t0));
        }
        assert!(!b.take(t0), "burst spent");
        assert!(b.take(t0 + Duration::from_millis(40)), "two more after 40 ms");
        assert!(b.take(t0 + Duration::from_millis(40)));
        assert!(!b.take(t0 + Duration::from_millis(40)));
    }
}
