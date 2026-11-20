//! A fake runtime: a small client that signs in to the runtime channel and
//! plays a scripted sequence of events. It stands in for Workbox in the
//! daemon's tests and when driving a scratch daemon by hand (the
//! `fake_runtime` example). It follows the contract in `runtime.proto`; the
//! raw senders ([`Fake::open`], [`Fake::send`]) exist so a test can also
//! break it on purpose.

use crate::v1::runtime_client::RuntimeClient;
use crate::v1::{Account, Accounts, Down, Heartbeat, Hello, SessionState, Snapshot, Status, Up, Welcome, down, up};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug)]
pub enum Error {
    /// The address was not a URL, or nothing answered there.
    Transport(String),
    /// The hub refused the call or ended the stream.
    Status(tonic::Status),
    /// The stream ended without a status, or the hub answered out of order.
    Protocol(String),
    /// A script that does not parse or names an unknown state.
    Script(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(m) => write!(f, "transport: {m}"),
            Error::Status(s) => write!(f, "{:?}: {}", s.code(), s.message()),
            Error::Protocol(m) => write!(f, "protocol: {m}"),
            Error::Script(m) => write!(f, "script: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// The gRPC code the hub answered with, if it did.
    pub fn code(&self) -> Option<tonic::Code> {
        match self {
            Error::Status(s) => Some(s.code()),
            _ => None,
        }
    }
}

/// One step of a script. As JSON: `{"snapshot": [..]}`, `{"state": {..}}`,
/// `{"accounts": [..]}`, `"heartbeat"`, `{"sleep_ms": 500}`, `"disconnect"`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Snapshot(Vec<ScriptSession>),
    State(ScriptSession),
    Accounts(Vec<ScriptAccount>),
    Heartbeat,
    SleepMs(u64),
    Disconnect,
}

/// A session in a script: only `id`, `address` and `state` are needed.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptSession {
    pub id: String,
    pub address: String,
    /// `working`, `needs_you`, `finished`, `stalled`, `waiting_on_peer`,
    /// `idle`, `asleep` or `exited`
    pub state: String,
    pub fabric: String,
    pub project: String,
    pub worktree: String,
    pub branch: String,
    pub account: String,
    pub tool: String,
    pub note: String,
    pub last_line: String,
    pub context_used_pct: Option<u32>,
    /// unix seconds; the moment the step is sent when absent
    pub since: Option<i64>,
}

impl ScriptSession {
    pub fn new(id: impl Into<String>, address: impl Into<String>, state: impl Into<String>) -> Self {
        Self { id: id.into(), address: address.into(), state: state.into(), ..Self::default() }
    }

    fn message(&self) -> Result<SessionState, Error> {
        let state = Status::from_name(&self.state).ok_or_else(|| Error::Script(format!("unknown state {:?}", self.state)))?;
        Ok(SessionState {
            session_id: self.id.clone(),
            address: self.address.clone(),
            fabric: self.fabric.clone(),
            project: self.project.clone(),
            worktree: self.worktree.clone(),
            branch: self.branch.clone(),
            account: self.account.clone(),
            state: state as i32,
            tool: self.tool.clone(),
            note: self.note.clone(),
            last_line: self.last_line.clone(),
            context_used_pct: self.context_used_pct,
            since: self.since.unwrap_or_else(now),
        })
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScriptAccount {
    pub name: String,
    pub session_used_pct: Option<u32>,
    pub session_resets_at: i64,
    pub week_used_pct: Option<u32>,
    pub week_resets_at: i64,
}

impl ScriptAccount {
    fn message(&self) -> Account {
        Account {
            name: self.name.clone(),
            session_used_pct: self.session_used_pct,
            session_resets_at: self.session_resets_at,
            week_used_pct: self.week_used_pct,
            week_resets_at: self.week_resets_at,
        }
    }
}

/// A script from its JSON: an array of steps.
pub fn parse_script(json: &str) -> Result<Vec<Step>, Error> {
    serde_json::from_str(json).map_err(|e| Error::Script(e.to_string()))
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// An open stream that has been welcomed.
pub struct Fake {
    tx: mpsc::Sender<Up>,
    down: tonic::Streaming<Down>,
    /// The last event number this instance assigned.
    seq: u64,
    pub welcome: Welcome,
}

impl Fake {
    /// Open the stream and nothing more: no Hello is sent. For tests of
    /// what the hub does with a stream that breaks the contract.
    pub async fn open(url: &str, credential: Option<&str>) -> Result<(mpsc::Sender<Up>, tonic::Streaming<Down>), Error> {
        let channel = tonic::transport::Endpoint::from_shared(url.to_string())
            .map_err(|e| Error::Transport(e.to_string()))?
            .connect()
            .await
            .map_err(|e| Error::Transport(format!("{e:?}")))?;
        let (tx, rx) = mpsc::channel::<Up>(64);
        let mut req = tonic::Request::new(ReceiverStream::new(rx));
        if let Some(c) = credential {
            let value = format!("Bearer {c}").parse().map_err(|_| Error::Transport("the credential is not a header value".into()))?;
            req.metadata_mut().insert("authorization", value);
        }
        let down = RuntimeClient::new(channel).connect(req).await.map_err(Error::Status)?.into_inner();
        Ok((tx, down))
    }

    /// Sign in as `instance_id`, say Hello and wait for the Welcome.
    /// `resume_seq` is the last event number this instance had assigned
    /// before (0 for a fresh launch): numbering continues after it.
    pub async fn connect(url: &str, credential: &str, instance_id: &str, resume_seq: u64) -> Result<Fake, Error> {
        let (tx, mut down) = Self::open(url, Some(credential)).await?;
        let hello = Hello {
            runtime_id: String::new(),
            instance_id: instance_id.to_string(),
            version: concat!("fake/", env!("CARGO_PKG_VERSION")).to_string(),
            last_down_seq: 0,
        };
        tx.send(Up { seq: 0, body: Some(up::Body::Hello(hello)) }).await.map_err(|_| Error::Protocol("the stream closed".into()))?;
        let welcome = match down.message().await.map_err(Error::Status)? {
            Some(Down { body: Some(down::Body::Welcome(w)), .. }) => w,
            Some(other) => return Err(Error::Protocol(format!("expected a Welcome, got {other:?}"))),
            None => return Err(Error::Protocol("the stream ended before a Welcome".into())),
        };
        Ok(Fake { tx, down, seq: resume_seq, welcome })
    }

    /// The last event number assigned.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Send one message exactly as given.
    pub async fn send(&mut self, up: Up) -> Result<(), Error> {
        self.tx.send(up).await.map_err(|_| Error::Protocol("the stream closed".into()))
    }

    async fn event(&mut self, body: up::Body) -> Result<(), Error> {
        self.seq += 1;
        self.send(Up { seq: self.seq, body: Some(body) }).await
    }

    /// Every session, as of the last event assigned.
    pub async fn snapshot(&mut self, sessions: &[ScriptSession]) -> Result<(), Error> {
        let sessions = sessions.iter().map(ScriptSession::message).collect::<Result<Vec<_>, _>>()?;
        self.send(Up { seq: 0, body: Some(up::Body::Snapshot(Snapshot { replaces_seq: self.seq, sessions })) }).await
    }

    pub async fn state(&mut self, session: &ScriptSession) -> Result<(), Error> {
        self.event(up::Body::State(session.message()?)).await
    }

    pub async fn accounts(&mut self, accounts: &[ScriptAccount]) -> Result<(), Error> {
        self.event(up::Body::Accounts(Accounts { accounts: accounts.iter().map(ScriptAccount::message).collect() })).await
    }

    pub async fn heartbeat(&mut self) -> Result<(), Error> {
        self.send(Up { seq: 0, body: Some(up::Body::Heartbeat(Heartbeat {})) }).await
    }

    /// Play `steps` in order. True when the script disconnected (the rest
    /// of it, if any, is not played).
    pub async fn play(&mut self, steps: &[Step]) -> Result<bool, Error> {
        for step in steps {
            match step {
                Step::Snapshot(sessions) => self.snapshot(sessions).await?,
                Step::State(session) => self.state(session).await?,
                Step::Accounts(accounts) => self.accounts(accounts).await?,
                Step::Heartbeat => self.heartbeat().await?,
                Step::SleepMs(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
                Step::Disconnect => return Ok(true),
            }
        }
        Ok(false)
    }

    /// Wait for the hub to end the stream and say how: the status it ended
    /// with, or None for a clean end.
    pub async fn ended(&mut self) -> Option<tonic::Status> {
        loop {
            match self.down.message().await {
                Ok(Some(_)) => continue,
                Ok(None) => return None,
                Err(status) => return Some(status),
            }
        }
    }

    /// Hang up: the hub sees the stream end.
    pub fn disconnect(self) {}
}
