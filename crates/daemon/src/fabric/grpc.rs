//! The runtime channel's service (`taisce.runtime.v1.Runtime`, ADR 0007):
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
//! The order of a stream, its limits, its flow control and how it ends are
//! stated in `proto/taisce/runtime/v1/runtime.proto`; this file enforces
//! them. Flow control is the part forced on the design: the proxy ends an
//! HTTP/2 connection once about 145 small DATA frames are buffered on it,
//! so neither side may have more than 64 unacknowledged events in flight.
//! The hub acknowledges what it takes ([`Receipts`]: every 16 events or
//! 250 ms) and everything it sends goes through [`DownSender`], which holds
//! the same window for the hub's own events.

use super::hub::{Connected, Hub, Incoming, Kill, Refused};
use crate::auth::ratelimit::{Class, Limiter};
use crate::store_ext::with_store;
use std::time::Duration;
use taisce_runtime_proto::CREDENTIAL_PREFIX;
use taisce_runtime_proto::v1::runtime_server::{Runtime, RuntimeServer};
use taisce_runtime_proto::v1::{self as pb, down, up};
use taisce_runtime_proto::window::{Receipts, SendWindow};
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
pub const RECHECK: Duration = Duration::from_secs(60);
/// The largest message taken (a snapshot of 200 full sessions fits).
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Accounts in one headroom report.
const MAX_ACCOUNTS: usize = 32;
/// Messages outside the send window (Snapshot, Heartbeat, Ack, a body this
/// build does not know) a stream may send: a burst, then a steady rate.
/// Events are not counted: the window paces them.
const CONTROL_BURST: f64 = 30.0;
const CONTROL_PER_SEC: f64 = 1.0;
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
    pub pace: Pace,
}

/// A stream's two clocks. Fields, not constants, so a test can run a stream
/// whose silence deadline is longer than its recheck, as production's is.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    /// What the Welcome asks for; three of these in silence end the stream.
    pub heartbeat: Duration,
    /// How often the runtime is checked to be still enrolled.
    pub recheck: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Pace { heartbeat: HEARTBEAT, recheck: RECHECK }
    }
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
                looked_up(with_store(&self.store, Scope::System, move |s| s.fabric_runtime_by_credential(&hash)).await)?
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

/// What a credential lookup found. A store that cannot answer (busy under
/// the box CLI, say) is not a wrong credential: UNAVAILABLE, and no failed
/// guess is counted against the caller's address.
fn looked_up(found: taisce_store::Result<Option<FabricRuntime>>) -> Result<Option<FabricRuntime>, Status> {
    found.map_err(|e| {
        tracing::warn!("fabric: sign-in could not read the store: {e}");
        Status::unavailable("the hub could not check that credential; try again")
    })
}

#[tonic::async_trait]
impl Runtime for RuntimeService {
    type AttachStream = ReceiverStream<Result<pb::Down, Status>>;

    async fn attach(&self, req: Request<Streaming<pb::Up>>) -> Result<Response<Self::AttachStream>, Status> {
        let runtime = self.authenticate(&req).await?;
        let ip = self.client_ip(&req);
        let (tx, rx) = mpsc::channel(8);
        let (hub, pace) = (self.hub.clone(), self.pace);
        tokio::spawn(async move {
            if let Err(status) = stream(&hub, &runtime, req.into_inner(), &tx, pace, &ip).await {
                tracing::info!(runtime = %runtime.id, code = ?status.code(), "fabric: stream ended: {}", status.message());
                let _ = tx.send(Err(status)).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    /// Fabric tool calls (Slice 5): declared in the contract, not served yet.
    async fn call(&self, req: Request<pb::CallRequest>) -> Result<Response<pb::CallReply>, Status> {
        self.authenticate(&req).await?;
        Err(Status::unimplemented("fabric tool calls are not served yet"))
    }
}

/// Why something was not sent down a stream.
#[derive(Debug)]
pub enum Unsent {
    /// 64 events are unacknowledged: the caller keeps the body and tries
    /// again after the next Ack.
    WindowFull,
    /// The runtime has hung up.
    Gone,
}

/// The one way anything is sent down a stream. Events are numbered and
/// held to the send window; the Welcome and Acks are outside it.
pub struct DownSender {
    tx: mpsc::Sender<Result<pb::Down, Status>>,
    window: SendWindow,
    /// The last event number assigned on this stream.
    seq: u64,
}

impl DownSender {
    /// A sender whose events are numbered after `last_seq` (the last Down
    /// event the runtime's Hello said it had received).
    pub fn new(tx: mpsc::Sender<Result<pb::Down, Status>>, last_seq: u64) -> Self {
        Self { tx, window: SendWindow::default(), seq: last_seq }
    }

    /// Send a message that is not an event (Welcome, Ack): unnumbered,
    /// outside the window.
    async fn control(&self, body: down::Body) -> Result<(), Unsent> {
        self.tx.send(Ok(pb::Down { seq: 0, body: Some(body) })).await.map_err(|_| Unsent::Gone)
    }

    /// Send the next event, if the window has room. Nothing in Slice 1
    /// sends one: orders and nudges will.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn event(&mut self, body: &down::Body) -> Result<u64, Unsent> {
        let seq = self.seq + 1;
        if !self.window.sent(seq) {
            return Err(Unsent::WindowFull);
        }
        self.seq = seq;
        self.tx.send(Ok(pb::Down { seq, body: Some(body.clone()) })).await.map_err(|_| Unsent::Gone)?;
        Ok(seq)
    }

    /// The runtime has received every event up to `seq`.
    fn acked(&mut self, seq: u64) {
        self.window.acked(seq);
    }

    async fn ack(&self, receipts: &mut Receipts) -> Result<(), Unsent> {
        self.control(down::Body::Ack(pb::Ack { seq: receipts.ack() })).await
    }
}

/// What reading the stream failed with, as the status the stream ends
/// with. tonic refuses an oversize message itself (OUT_OF_RANGE): the
/// contract calls every broken limit INVALID_ARGUMENT. Anything else (a
/// message that does not decode, a connection that broke) keeps its status:
/// only a clean end of the stream is a hang-up.
fn read_failed(status: Status) -> Status {
    if status.code() == tonic::Code::OutOfRange {
        return Status::invalid_argument(format!("a message is at most {MAX_MESSAGE_BYTES} bytes"));
    }
    status
}

/// One stream, from its Hello to its end. Err is the status it ends with;
/// Ok is the runtime hanging up.
async fn stream(
    hub: &Hub,
    runtime: &FabricRuntime,
    mut inbound: Streaming<pb::Up>,
    tx: &mpsc::Sender<Result<pb::Down, Status>>,
    pace: Pace,
    ip: &str,
) -> Result<(), Status> {
    let first = tokio::time::timeout(HELLO_TIMEOUT, inbound.message())
        .await
        .map_err(|_| Status::deadline_exceeded("no Hello within 10 seconds"))?;
    let hello = match first.map_err(read_failed)? {
        Some(pb::Up { body: Some(up::Body::Hello(h)), .. }) => h,
        Some(_) => return Err(Status::failed_precondition("the first message must be a Hello")),
        None => return Ok(()),
    };
    // a UUID compares as a UUID: Swift prints one in capitals
    if !hello.runtime_id.is_empty() && uuid::Uuid::parse_str(&hello.runtime_id).ok() != Some(runtime.id) {
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
    let welcome = pb::Welcome { last_up_seq, runtime_id: runtime.id.to_string(), heartbeat_secs: pace.heartbeat.as_secs().max(1) as u32 };
    let mut down = DownSender::new(tx.clone(), hello.last_down_seq);
    let ended = match down.control(down::Body::Welcome(welcome)).await {
        Ok(()) => {
            let mut open = Open { hub, runtime, conn, pace, down: &mut down, receipts: Receipts::starting_at(last_up_seq) };
            open.run(&mut inbound, &mut kill).await
        }
        // the runtime hung up before its Welcome
        Err(_) => Ok(()),
    };
    hub.disconnect(runtime.user_id, runtime.id, conn, crate::auth::now()).await;
    tracing::info!(target: crate::auth::AUDIT, event = "runtime.disconnect", runtime = %runtime.id, user = %runtime.user_id);
    ended
}

/// A stream after its Welcome.
struct Open<'a> {
    hub: &'a Hub,
    runtime: &'a FabricRuntime,
    conn: u64,
    pace: Pace,
    down: &'a mut DownSender,
    receipts: Receipts,
}

impl Open<'_> {
    async fn run(&mut self, inbound: &mut Streaming<pb::Up>, kill: &mut tokio::sync::oneshot::Receiver<Kill>) -> Result<(), Status> {
        use tokio::time::Instant;
        let (user, id) = (self.runtime.user_id, self.runtime.id);
        let silence = self.pace.heartbeat * 3;
        let mut budget = Budget::new(Instant::now());
        let mut recheck = tokio::time::interval_at(Instant::now() + self.pace.recheck, self.pace.recheck);
        // When the stream will be called silent. Moved ONLY by a message
        // arriving: the recheck and the ack timer below wake this loop too,
        // and must not buy a sleeping Mac more time.
        let mut silent_at = Instant::now() + silence;
        let mut snapshot_seen = false;
        loop {
            let ack_due = self.receipts.due_at().map(Instant::from_std);
            let up = tokio::select! {
                biased;
                why = &mut *kill => {
                    return Err(match why {
                        Ok(Kill::Superseded) => Status::aborted("a newer stream for this runtime took over"),
                        Ok(Kill::Removed) | Err(_) => Status::unauthenticated("this runtime was removed"),
                    });
                }
                _ = tokio::time::sleep_until(silent_at) => {
                    return Err(Status::deadline_exceeded("no message for three heartbeats"));
                }
                _ = recheck.tick() => {
                    if !self.hub.still_enrolled(user, id).await {
                        return Err(Status::unauthenticated("this runtime was removed"));
                    }
                    continue;
                }
                // what has been taken is acknowledged within 250 ms
                _ = tokio::time::sleep_until(ack_due.unwrap_or_else(Instant::now)), if ack_due.is_some() => {
                    if self.down.ack(&mut self.receipts).await.is_err() {
                        return Ok(());
                    }
                    continue;
                }
                got = inbound.message() => match got.map_err(read_failed)? {
                    Some(up) => up,
                    // the runtime hung up
                    None => return Ok(()),
                },
            };
            silent_at = Instant::now() + silence;
            let now = crate::auth::now();
            let incoming = match up.body {
                Some(up::Body::State(_) | up::Body::Accounts(_)) if !snapshot_seen => {
                    return Err(Status::failed_precondition("the first message after the Hello must be a Snapshot"));
                }
                // events: paced by the send window, not by the budget
                Some(up::Body::State(s)) => Incoming::State { seq: event_seq(up.seq)?, session: Box::new(session(&s, self.runtime, now)?) },
                Some(up::Body::Accounts(a)) => Incoming::Accounts { seq: event_seq(up.seq)?, accounts: accounts(&a)? },
                // everything else is outside the window, so it is counted
                other => {
                    if !budget.take(Instant::now()) {
                        return Err(Status::resource_exhausted("too many messages outside the send window"));
                    }
                    match other {
                        Some(up::Body::Hello(_)) => return Err(Status::failed_precondition("a second Hello on one stream")),
                        Some(up::Body::Ack(a)) => {
                            self.down.acked(a.seq);
                            continue;
                        }
                        Some(up::Body::Snapshot(s)) => {
                            snapshot_seen = true;
                            if s.sessions.len() > MAX_SESSIONS_PER_RUNTIME {
                                return Err(Status::invalid_argument(format!("snapshot.sessions: at most {MAX_SESSIONS_PER_RUNTIME}")));
                            }
                            let sessions = s.sessions.iter().map(|m| session(m, self.runtime, now)).collect::<Result<Vec<_>, _>>()?;
                            Incoming::Snapshot { replaces_seq: s.replaces_seq, sessions }
                        }
                        Some(_) if !snapshot_seen => {
                            return Err(Status::failed_precondition("the first message after the Hello must be a Snapshot"));
                        }
                        // a Heartbeat, or a body this build does not know
                        // (a newer runtime): nothing to do
                        Some(_) | None => continue,
                    }
                }
            };
            // a Snapshot is not an event: it has no number to acknowledge
            let seq = match &incoming {
                Incoming::Snapshot { .. } => None,
                Incoming::State { seq, .. } | Incoming::Accounts { seq, .. } => Some(*seq),
            };
            self.hub.apply(user, id, self.conn, incoming, now).await.map_err(refused)?;
            if let Some(seq) = seq {
                let at = std::time::Instant::now();
                self.receipts.received(seq, at);
                // sixteen taken and unacknowledged: say so now
                if self.receipts.is_due(at) && self.down.ack(&mut self.receipts).await.is_err() {
                    return Ok(());
                }
            }
        }
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

/// A token bucket over one stream's messages outside the send window.
struct Budget {
    tokens: f64,
    at: tokio::time::Instant,
}

impl Budget {
    fn new(at: tokio::time::Instant) -> Self {
        Self { tokens: CONTROL_BURST, at }
    }

    fn take(&mut self, now: tokio::time::Instant) -> bool {
        self.tokens = (self.tokens + now.saturating_duration_since(self.at).as_secs_f64() * CONTROL_PER_SEC).min(CONTROL_BURST);
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

/// A reported state. One the hub does not know (a newer runtime's, or 0) is
/// `Unknown`: shown, never fatal, never an alert.
fn status(value: i32) -> SessionStatus {
    match pb::SessionStatus::try_from(value) {
        Ok(pb::SessionStatus::Working) => SessionStatus::Working,
        Ok(pb::SessionStatus::NeedsYou) => SessionStatus::NeedsYou,
        Ok(pb::SessionStatus::Finished) => SessionStatus::Finished,
        Ok(pb::SessionStatus::Stalled) => SessionStatus::Stalled,
        Ok(pb::SessionStatus::WaitingOnPeer) => SessionStatus::WaitingOnPeer,
        Ok(pb::SessionStatus::Idle) => SessionStatus::Idle,
        Ok(pb::SessionStatus::Asleep) => SessionStatus::Asleep,
        Ok(pb::SessionStatus::Exited) => SessionStatus::Exited,
        Ok(pb::SessionStatus::Unspecified) | Err(_) => SessionStatus::Unknown,
    }
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
        state: status(m.state),
        tool: text("tool", &m.tool, 0, 100)?,
        note: text("note", &m.note, 0, 500)?,
        last_line: text("last_line", &m.last_line, 0, 2000)?,
        context_used_pct: percent("context_used_pct", m.context_used_pct)?,
        since: m.since,
        updated_at: now,
        // the person's, never the runtime's: the hub keeps what it holds
        quiet_until: None,
    })
}

/// A headroom report as the JSON the viewer routes serve.
fn accounts(m: &pb::Accounts) -> Result<serde_json::Value, Status> {
    if m.accounts.len() > MAX_ACCOUNTS {
        return Err(Status::invalid_argument(format!("accounts: at most {MAX_ACCOUNTS}")));
    }
    let resets = |field: &str, at: Option<i64>| match at {
        None => Ok(None),
        Some(at) if (1..=MAX_SINCE).contains(&at) => Ok(Some(at)),
        Some(_) => Err(Status::invalid_argument(format!("{field}: unix seconds, or unset when unknown"))),
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

/// [`serve`] on a clone of `listener`, for `supervise`: when the server
/// stops (it should not), the next start listens on the same socket.
pub async fn serve_supervised(listener: std::sync::Arc<std::net::TcpListener>, service: RuntimeService) {
    let served = match listener.try_clone().and_then(tokio::net::TcpListener::from_std) {
        Ok(l) => serve(l, service, std::future::pending()).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = served {
        tracing::error!("the runtime channel stopped: {e}");
    }
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
            state: pb::SessionStatus::Working as i32,
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
    fn every_proto_state_has_a_store_state_and_an_unknown_one_is_unknown() {
        for n in 1..=8 {
            assert_eq!(Some(status(n).as_str()), pb::SessionStatus::try_from(n).unwrap().name());
        }
        // a state a newer runtime knows, and an unset one: shown, not fatal
        for n in [0, 9, 42, -1] {
            assert_eq!(status(n), SessionStatus::Unknown);
        }
        let mut m = message();
        m.state = 42;
        assert_eq!(session(&m, &runtime(), 1).unwrap().state, SessionStatus::Unknown);
    }

    #[test]
    fn accounts_become_the_json_the_routes_serve() {
        let a = pb::Accounts {
            accounts: vec![pb::Account {
                name: "personal".into(),
                session_used_pct: Some(40),
                session_resets_at: Some(1_800_000_000),
                week_used_pct: None,
                week_resets_at: None,
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
        let zero = pb::Accounts { accounts: vec![pb::Account { name: "x".into(), week_resets_at: Some(0), ..Default::default() }] };
        assert!(accounts(&zero).unwrap_err().message().starts_with("accounts.week_resets_at"), "unknown is unset, not 0");
    }

    /// The hub's own events obey the window the runtime's do. (Nothing in
    /// Slice 1 sends one; an Ack body stands in for an order here.)
    #[tokio::test]
    async fn the_hub_holds_its_own_events_to_the_window() {
        use taisce_runtime_proto::window::WINDOW;
        let (tx, mut rx) = mpsc::channel(WINDOW * 2);
        // the runtime's Hello said it had received Down event 10
        let mut down = DownSender::new(tx, 10);
        let body = down::Body::Ack(pb::Ack { seq: 0 });
        for i in 1..=WINDOW as u64 {
            assert_eq!(down.event(&body).await.unwrap(), 10 + i);
        }
        assert!(matches!(down.event(&body).await, Err(Unsent::WindowFull)), "the 65th waits for an Ack");
        // a Welcome or an Ack is outside the window, and unnumbered
        down.control(body.clone()).await.unwrap();
        // the runtime acknowledges sixteen: sixteen more may go, numbered on
        down.acked(26);
        for i in 1..=16 {
            assert_eq!(down.event(&body).await.unwrap(), 10 + WINDOW as u64 + i);
        }
        assert!(matches!(down.event(&body).await, Err(Unsent::WindowFull)));
        let mut seqs = vec![];
        while let Ok(Ok(m)) = rx.try_recv() {
            seqs.push(m.seq);
        }
        assert_eq!(seqs.iter().filter(|s| **s == 0).count(), 1);
        assert_eq!(seqs.iter().filter(|s| **s > 0).copied().collect::<Vec<_>>(), (11..=10 + WINDOW as u64 + 16).collect::<Vec<_>>());
        // a runtime that has hung up
        drop(rx);
        down.acked(u64::MAX);
        assert!(matches!(down.event(&body).await, Err(Unsent::Gone)));
    }

    #[test]
    fn messages_outside_the_window_have_a_burst_and_a_steady_rate() {
        let t0 = tokio::time::Instant::now();
        let mut b = Budget::new(t0);
        for _ in 0..CONTROL_BURST as usize {
            assert!(b.take(t0));
        }
        assert!(!b.take(t0), "burst spent");
        assert!(b.take(t0 + Duration::from_secs(2)), "two more after 2 s");
        assert!(b.take(t0 + Duration::from_secs(2)));
        assert!(!b.take(t0 + Duration::from_secs(2)));
    }

    #[test]
    fn a_store_that_cannot_answer_at_sign_in_is_unavailable_not_a_wrong_credential() {
        let busy = taisce_store::StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        ));
        assert_eq!(looked_up(Err(busy)).unwrap_err().code(), tonic::Code::Unavailable);
        assert!(looked_up(Ok(None)).unwrap().is_none(), "an unknown credential is still just unknown");
    }

    #[test]
    fn an_oversize_message_is_a_broken_limit_and_other_read_failures_keep_their_status() {
        let s = read_failed(Status::out_of_range("Error, decoded message length too large"));
        assert_eq!(s.code(), tonic::Code::InvalidArgument);
        assert!(s.message().contains("1048576"));
        assert_eq!(read_failed(Status::internal("failed to decode")).code(), tonic::Code::Internal);
    }
}
