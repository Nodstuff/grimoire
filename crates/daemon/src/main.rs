//! ksd — the knowledge-system daemon (PROJECT.md §3.2a).
//!
//! One process owns the SQLite file and serves every surface. Tonight:
//! MCP over streamable HTTP at /mcp. Web UI routes come later (M5).

mod admin;
mod api;
mod ask;
mod auth;
mod backup;
mod changes;
mod children;
mod docops;
mod embed;
mod filer;
mod garden;
mod home;
mod inbox;
mod living;
mod local_guard;
mod legacy;
mod mcp;
mod memory;
mod nav;
mod push;
mod retrieval;
mod share_render;
mod shares;
mod store_ext;
mod due;
mod todo;
mod workspaces;
mod viewer;
#[cfg(test)]
mod retrieval_probe;
#[cfg(test)]
mod isolation_tests;

use anyhow::Context;
use clap::{Parser, Subcommand};
use taisce_store::{BlockStore, PrincipalKind, SqliteStore};
use std::path::PathBuf;
use std::sync::Arc;

/// The built frontend, compiled INTO the binary (release) so the app is
/// self-contained — no external `ui/dist` path to go missing on another
/// machine. In debug builds rust-embed reads the folder off disk, which keeps
/// the dev loop live. `ui/dist` must exist at compile time; `release.sh` and
/// `deploy.sh` build it first.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/dist"]
struct EmbeddedUi;

/// Serve the embedded SPA: exact asset by path, else fall back to index.html
/// (client-side routing). Returns 503 only if the binary was built with no
/// frontend at all.
async fn serve_embedded_ui(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (body, name) = match EmbeddedUi::get(path) {
        Some(f) => (f.data, path.to_string()),
        None => match EmbeddedUi::get("index.html") {
            Some(f) => (f.data, "index.html".to_string()),
            None => {
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "frontend not built into this binary",
                )
                    .into_response();
            }
        },
    };
    let ctype = content_type_for(&name);
    (
        [(axum::http::header::CONTENT_TYPE, ctype)],
        body.into_owned(),
    )
        .into_response()
}

/// A stable stamp for the embedded frontend: FNV-1a over the bundled
/// index.html (its asset names carry Vite's content hashes, so any UI change
/// changes this). Computed once.
pub fn ui_build_stamp() -> u64 {
    static STAMP: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *STAMP.get_or_init(|| match EmbeddedUi::get("index.html") {
        Some(f) => fnv1a(&f.data),
        // no embedded UI (a cross-compiled server build): the git sha still
        // distinguishes one binary from the next instead of a flat 0
        None => match GIT_SHA {
            Some(sha) if !sha.is_empty() => fnv1a(sha.as_bytes()),
            _ => 0,
        },
    })
}

/// Short git sha of the checkout this binary was built from (build.rs); None
/// when built outside a git checkout.
pub const GIT_SHA: Option<&str> = option_env!("TAISCE_GIT_SHA");

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Keep a long-lived background loop alive: log its exit or panic with its
/// name and start it again after a backoff (5s doubling to 5 min). Without
/// this a panic in, say, the embed loop silently ends indexing until the
/// next restart.
fn supervise<F, Fut>(name: &'static str, mk: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    supervise_with(name, std::time::Duration::from_secs(5), mk);
}

fn supervise_with<F, Fut>(name: &'static str, initial_backoff: std::time::Duration, mk: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut backoff = initial_backoff;
        loop {
            match tokio::spawn(mk()).await {
                Ok(()) => tracing::warn!(task = name, "background loop exited; restarting in {}s", backoff.as_secs()),
                Err(e) if e.is_panic() => {
                    tracing::error!(task = name, "background loop panicked: {e}; restarting in {}s", backoff.as_secs())
                }
                Err(_) => return, // cancelled: the runtime is shutting down
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(300));
        }
    });
}

/// Content type from a file extension — the handful Vite emits. Kept local to
/// avoid a mime dependency.
fn content_type_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[derive(Parser)]
#[command(name = "taisce", about = "Taisce daemon")]
struct Cli {
    /// Path to the SQLite database.
    #[arg(long, default_value_os_t = default_db())]
    db: PathBuf,
    /// The daemon's port: what `serve` listens on and what every other
    /// command talks to.
    #[arg(long, global = true, default_value_t = 7425)]
    port: u16,
    /// SERVER mode: the public origin clients reach this daemon at, through
    /// a TLS reverse proxy (`https://taisce.example`). Unset = LOCAL mode.
    #[arg(long, global = true, env = "TAISCE_PUBLIC_URL")]
    public_url: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum GardenerCmd {
    Add {
        name: String,
        task_prompt: String,
        /// tagging (default), auditor, scribe, keeper or filer
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        scope_doc: Option<String>,
        /// review (default: everything lands as reviewable yellows) or gate
        #[arg(long)]
        policy: Option<String>,
    },
    List,
}

#[derive(Subcommand)]
enum Cmd {
    /// Import a markdown vault (one-shot).
    Import { dir: PathBuf },
    /// Export all docs to a markdown directory tree.
    Export { dir: PathBuf },
    /// Manage gardeners (talks to the running daemon).
    Gardener {
        #[command(subcommand)]
        cmd: GardenerCmd,
    },
    /// Run gardeners now (talks to the running daemon).
    Garden {
        #[arg(long)]
        name: Option<String>,
    },
    /// Recent gardener runs (talks to the running daemon).
    Runs,
    /// Set a doc's review policy: human-review | agent-review | auto | clear.
    Policy { doc_id: String, policy: String },
    /// Serve MCP over streamable HTTP (on --port, default 7425).
    Serve {
        /// SERVER mode: rate-limit by the reverse proxy's X-Forwarded-For
        /// (its last hop) instead of the socket peer.
        #[arg(long, env = "TAISCE_TRUSTED_PROXY")]
        trusted_proxy: bool,
        /// SERVER mode: an extra OAuth redirect URI to accept (exact match;
        /// repeatable). Claude's callback, loopback and the app's scheme are built in.
        #[arg(long = "allow-redirect", env = "TAISCE_OAUTH_REDIRECTS", value_delimiter = ',')]
        allow_redirect: Vec<String>,
        /// SERVER mode: APNs push to the Taisce app (off unless configured).
        #[command(flatten)]
        apns: push::ApnsArgs,
    },
    /// SERVER mode sign-in: passkeys and OAuth grants (works on the db
    /// directly; run it on the server box).
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Workspaces and who they are shared with (ADR 0004). Works on the db
    /// directly, on the server box: sharing is a human surface, never MCP.
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
}

#[derive(Subcommand)]
enum WorkspaceCmd {
    /// Every workspace: owner, members, shared.
    List,
    /// The members of one workspace (id, unique id prefix, or `Name` when
    /// only one workspace carries it; `Name@owner` picks the owner's).
    Members { workspace: String },
    /// Share a workspace with a user as editor or viewer (or change a role).
    Share {
        workspace: String,
        /// user id, unique prefix, or name
        #[arg(long)]
        user: String,
        /// editor | viewer
        #[arg(long, default_value = "viewer")]
        role: String,
    },
    /// Remove a user from a workspace; their clients drop its docs.
    Unshare {
        workspace: String,
        #[arg(long)]
        user: String,
    },
}

#[derive(Subcommand)]
enum AuthCmd {
    /// Print a one-time link (15 minutes) that registers a passkey for the
    /// owner, or for another user with --user.
    Enroll {
        /// user id, unique prefix, or name (default: the owner)
        #[arg(long)]
        user: Option<String>,
    },
    /// People on this server (ADR 0004).
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    /// Recent audit events: users, membership changes, refused shares.
    Audit {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// List users, passkeys, live OAuth grants and web UI sessions.
    List,
    /// Revoke an OAuth grant or a web UI session, or delete a passkey, by id
    /// (or unique prefix; a web session's id is the prefix `auth list` shows).
    /// Deleting a passkey also revokes the web sessions and grants it opened.
    /// A lost device: `--user <name> --all` deletes every passkey and revokes
    /// every session and grant of that user.
    /// Revocation is always per grant (one sign-in on one device): every
    /// Taisce app sign-in shares the client `taisce-app`, so never revoke "by
    /// client" — that would sign out every person's every device.
    Revoke {
        /// grant, web session or passkey id (or unique prefix)
        id: Option<String>,
        /// with --all: the user whose every sign-in to revoke
        #[arg(long, requires = "all")]
        user: Option<String>,
        /// every passkey, web session and grant of --user
        #[arg(long, requires = "user", conflicts_with = "id")]
        all: bool,
    },
    /// Personal access tokens: static bearers for /mcp only.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Subcommand)]
enum UserCmd {
    /// Add a person (their own Unsorted, workspaces and passkeys); then
    /// `taisce auth enroll --user <name>` for their passkey link.
    Add { name: String },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Mint a token and print it once (it is stored only as a hash).
    Create {
        #[arg(long)]
        name: String,
        /// The user it acts for, by id (or unique prefix); default the owner.
        #[arg(long)]
        user: Option<String>,
        /// Register a token minted elsewhere by its SHA-256 (lowercase hex of
        /// the whole `tsk_…` string): the secret never reaches this box or
        /// its logs, and nothing is printed but the id.
        #[arg(long)]
        hash: Option<String>,
    },
    /// List tokens (never their values).
    List,
    /// Revoke a live token by name or id (or unique prefix).
    Revoke { key: String },
}

/// Still `~/.grimoire` after the rename: the legacy desktop app's data lives
/// there and is not moved. The server passes `--db` explicitly.
fn default_db() -> PathBuf {
    dirs_home().join(".grimoire/ks.db")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The display name a fresh install starts with: the macOS account's full
/// name, else the login name, else "me" — never a hardcoded placeholder.
fn default_human_name() -> String {
    if let Ok(out) = std::process::Command::new("id").arg("-F").output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    std::env::var("USER")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "me".into())
}

/// The two v1 principals, created on first run. The human is found by KIND
/// (there is exactly one per instance), never by name — the name is the
/// user's to change.
fn bootstrap_principals(store: &mut SqliteStore) -> anyhow::Result<(uuid::Uuid, uuid::Uuid)> {
    let existing = store.list_principals()?;
    let find = |name: &str| {
        existing
            .iter()
            .find(|p| p.display_name == name)
            .map(|p| p.id)
    };
    // ADR 0004: with several people there are several human principals; the
    // instance's own is the owner user's
    let owner_principal = store.auth_owner()?.map(|u| u.principal_id);
    let human = match owner_principal.or_else(|| existing.iter().find(|p| p.kind == PrincipalKind::Human).map(|p| p.id)) {
        Some(p) => p,
        None => {
            let name = default_human_name();
            tracing::info!(name, "first run: human principal created (rename it in the app)");
            store.create_principal(PrincipalKind::Human, &name, None)?.id
        }
    };
    let tom = human;
    let claude = match find("claude") {
        Some(id) => id,
        None => {
            store
                .create_principal(PrincipalKind::Agent, "claude", None)?
                .id
        }
    };
    Ok((tom, claude))
}

/// Where the daemon's log files live (the db directory). Set once by
/// `init_logging`; `log_path` and the diagnostics route read it.
static LOG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Log file name parts: `ksd.YYYY-MM-DD.log`, rotated daily, 7 kept.
const LOG_PREFIX: &str = "ksd";
const LOG_SUFFIX: &str = "log";
const LOG_KEEP: usize = 7;

/// The current log file (newest `ksd.*.log` in the log dir), if any.
pub fn log_path() -> Option<PathBuf> {
    let dir = LOG_DIR.get()?;
    // dates sort lexically; the greatest name is the newest file
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| {
                    n.starts_with(&format!("{LOG_PREFIX}.")) && n.ends_with(&format!(".{LOG_SUFFIX}"))
                })
        })
        .max()
}

type LogLevelHandle = tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;

/// One log, owned by the daemon: a daily-rolled file beside the db (the
/// shell used to redirect stdout into a second, never-rotating file), plus
/// stdout when run from a terminal. Level: RUST_LOG, else info until the
/// store opens and `apply_log_level` swaps in the `log.level` setting — so
/// a store that fails to open is itself logged. Returns the non-blocking
/// writer's guard (drop it and buffered lines are lost, so `main` holds it
/// until exit) and the reload handle.
fn init_logging(db_dir: &std::path::Path) -> (Option<tracing_appender::non_blocking::WorkerGuard>, LogLevelHandle) {
    use std::io::IsTerminal;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let (filter, handle) = tracing_subscriber::reload::Layer::new(filter);
    let stdout_layer = std::io::stdout()
        .is_terminal()
        .then(tracing_subscriber::fmt::layer);
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .filename_suffix(LOG_SUFFIX)
        .max_log_files(LOG_KEEP)
        .build(db_dir);
    match file {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let _ = LOG_DIR.set(db_dir.to_path_buf());
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer))
                .with(stdout_layer)
                .init();
            (Some(guard), handle)
        }
        Err(e) => {
            // no writable log dir: stderr is better than silence
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
                .init();
            tracing::warn!("log file unavailable ({e}); logging to stderr only");
            (None, handle)
        }
    }
}

/// The `log.level` setting, once the store is open. RUST_LOG still wins.
fn apply_log_level(handle: &LogLevelHandle, level: Option<String>) {
    if std::env::var_os("RUST_LOG").is_some() {
        return;
    }
    let Some(level) = level else { return };
    match tracing_subscriber::EnvFilter::try_new(&level) {
        Ok(f) => {
            if let Err(e) = handle.reload(f) {
                tracing::warn!("could not apply log.level={level}: {e}");
            }
        }
        Err(e) => tracing::warn!("bad log.level setting {level:?}: {e}"),
    }
}


/// CLI → daemon: every `/admin/*` call carries the per-boot admin token the
/// daemon wrote beside the db (see `admin::AdminToken`). Missing file = the
/// daemon is not running; the request fails with a clear 401 either way.
fn admin_client(db: &std::path::Path, timeout: Option<std::time::Duration>) -> anyhow::Result<reqwest::Client> {
    let db_dir = db.parent().unwrap_or(std::path::Path::new("."));
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(tok) = admin::AdminToken::read_from(db_dir) {
        headers.insert(admin::ADMIN_HEADER, reqwest::header::HeaderValue::from_str(&tok)?);
    }
    let mut b = reqwest::Client::builder().default_headers(headers);
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    Ok(b.build()?)
}

/// When spawned by the shell (`TAISCE_PARENT_PID`), exit when that shell
/// is gone. macOS has no PDEATHSIG, and a shell that crashes or is replaced
/// by the updater leaves its child running; the next app version then
/// attaches to a stale daemon (0.7.2 shipped this way). Raising SIGTERM on
/// ourselves takes the normal graceful path, children included.
#[cfg(unix)]
async fn watch_parent() {
    let Some(pid) = std::env::var("TAISCE_PARENT_PID").ok().and_then(|p| p.parse::<i32>().ok()) else {
        return;
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        // SAFETY: kill(pid, 0) sends no signal; it only reports whether pid exists
        let alive = unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
        if !alive {
            tracing::info!(parent = pid, "shell is gone: shutting down");
            // SAFETY: raise(3) on our own process
            unsafe {
                libc::raise(libc::SIGTERM);
            }
            return;
        }
    }
}

/// Ctrl-C from a terminal, or SIGTERM from the shell / launchd / `kill`.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("no SIGTERM handler ({e}); ctrl-c only");
                tokio::signal::ctrl_c().await.ok();
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("ctrl-c: shutting down"),
            _ = term.recv() => tracing::info!("SIGTERM: shutting down"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}

/// rustls (reqwest's https, for client metadata documents and APNs) needs one
/// process crypto provider: ring. Idempotent.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn human_name(store: &SqliteStore) -> String {
    if let Ok(Some(u)) = store.auth_owner() {
        return u.name;
    }
    store
        .list_principals()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.kind == PrincipalKind::Human)
        .map(|p| p.display_name)
        .unwrap_or_else(|| "owner".into())
}

/// `taisce auth …`: straight against the db (WAL lets the daemon run on).
fn auth_cli(store: &mut SqliteStore, cmd: AuthCmd, public_url: Option<String>, human: uuid::Uuid) -> anyhow::Result<()> {
    let now = auth::now();
    let fmt_time = |t: i64| {
        chrono::DateTime::from_timestamp(t, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default()
    };
    match cmd {
        AuthCmd::Enroll { user } => {
            let base = match public_url {
                Some(u) => auth::AuthConfig::from_public_url(&u)?.base,
                None => store.get_setting("auth.public_url")?.ok_or_else(|| {
                    anyhow::anyhow!("no public URL: pass --public-url (or serve once in server mode)")
                })?,
            };
            let name = human_name(store);
            let owner = store.auth_ensure_owner(human, &name, now)?;
            let target = match user.as_deref() {
                None => owner,
                Some(key) => one_user(store, key)?,
            };
            let token = auth::random_token();
            store.auth_add_enrollment(&auth::hash_secret(&token), target.id, now + auth::ENROLL_TTL)?;
            tracing::info!(target: auth::AUDIT, event = "enroll.mint", user = %target.id);
            if target.role != "owner" {
                println!("passkey link for {}:", target.name);
            }
            println!("{base}/auth/enroll?t={token}");
            println!("(one-time, expires in 15 minutes — open it on the device that will hold the passkey)");
        }
        AuthCmd::User { cmd: UserCmd::Add { name } } => {
            let owner_name = human_name(store);
            store.auth_ensure_owner(human, &owner_name, now)?;
            let u = store.auth_add_user(&name, now)?;
            tracing::info!(target: auth::AUDIT, event = "user.create", user = %u.id, name = u.name);
            println!("added user {}  {}  (member)", u.id, u.name);
            println!("next: taisce auth enroll --user {:?}   (their passkey link)", u.name);
        }
        AuthCmd::Audit { limit } => {
            for e in store.audit_events(limit)? {
                println!("{}  {:<26} {}  actor {}  {}", e.at, e.event, e.subject, e.actor.unwrap_or_else(|| "cli".into()), e.detail);
            }
        }
        AuthCmd::List => {
            for u in store.auth_users()? {
                println!("user {}  {}  ({})", u.id, u.name, u.role);
                for c in store.auth_credentials(Some(u.id))? {
                    let used = c.last_used_at.map(fmt_time).unwrap_or_else(|| "never".into());
                    println!("  passkey {}  {:<20}  added {}  last used {}", c.id, c.label, fmt_time(c.created_at), used);
                }
            }
            for g in store.oauth_grants(false)? {
                let name = store.oauth_client(&g.client_id)?.map(|c| c.client_name).unwrap_or_default();
                println!("grant {}  {}  [{}]  since {}", g.id, name, g.client_id, fmt_time(g.created_at));
            }
            for line in auth::web::session_lines(store, now)? {
                println!("{line}");
            }
        }
        AuthCmd::Token { cmd: TokenCmd::Create { name, user, hash } } => {
            if user.is_none() {
                // as `enroll`: the owner exists before the box ever served
                let owner_name = human_name(store);
                store.auth_ensure_owner(human, &owner_name, now)?;
            }
            let (t, secret) = auth::create_api_token(store, user.as_deref(), &name, hash.as_deref(), now)?;
            match secret {
                Some(secret) => {
                    println!("{secret}");
                    eprintln!("(token {:?} {} — shown once; it opens /mcp only)", t.name, t.id);
                }
                None => println!("registered token {} {} (hash only; it opens /mcp only)", t.name, t.id),
            }
        }
        AuthCmd::Token { cmd: TokenCmd::List } => {
            for line in auth::api_token_lines(store)? {
                println!("{line}");
            }
        }
        AuthCmd::Token { cmd: TokenCmd::Revoke { key } } => {
            let t = auth::revoke_api_token(store, &key, now)?;
            println!("revoked token {} {}", t.name, t.id);
        }
        AuthCmd::Revoke { id, user, all } => match (id, user) {
            (None, Some(key)) if all => {
                let u = one_user(store, &key)?;
                println!("{}", auth::cli_revoke_user_all(store, &u, now)?);
            }
            (Some(id), None) => println!("{}", auth::cli_revoke(store, &id, now)?),
            _ => anyhow::bail!("give an id, or --user <name> --all"),
        },
    }
    Ok(())
}

/// A workspace by id, unique id prefix, `Name@owner` (owner name or its
/// start: `Work@aoife`), or a name only one
/// workspace carries (the CLI is System scope: it sees every person's).
fn cli_workspace(store: &SqliteStore, key: &str) -> anyhow::Result<taisce_store::Workspace> {
    let all = store.list_workspaces()?;
    let key = key.trim();
    let (name, owner) = match key.rsplit_once('@') {
        Some((n, o)) => (n.trim(), Some(o.trim().to_lowercase())),
        None => (key, None),
    };
    let hits: Vec<&taisce_store::Workspace> = all
        .iter()
        .filter(|w| {
            w.id.to_string().starts_with(&key.to_lowercase())
                || (w.name.eq_ignore_ascii_case(name)
                    && owner.as_ref().is_none_or(|o| w.owner_name.as_deref().is_some_and(|n| n.to_lowercase().starts_with(o.as_str()))))
        })
        .collect();
    match hits.as_slice() {
        [w] => Ok((*w).clone()),
        [] => anyhow::bail!("no workspace matches {key:?}"),
        many => anyhow::bail!(
            "{key:?} is ambiguous: {} — use the id or Name@owner",
            many.iter().map(|w| format!("{}@{} ({})", w.name, w.owner_name.as_deref().unwrap_or("?"), w.id)).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Exactly one user for a CLI `--user`: an unknown or ambiguous name is
/// refused, and an ambiguous one lists the candidates' ids.
fn one_user(store: &SqliteStore, key: &str) -> anyhow::Result<taisce_store::auth::AuthUser> {
    let mut hits = store.auth_users_matching(key)?;
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => anyhow::bail!("no user matches {key:?} (see `taisce auth list`)"),
        _ => anyhow::bail!(
            "{key:?} matches several users — pass an id: {}",
            hits.iter().map(|u| format!("{} ({})", u.id, u.name)).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `taisce workspace …`: straight against the db, as System (the box).
fn workspace_cli(store: &mut SqliteStore, cmd: WorkspaceCmd) -> anyhow::Result<()> {
    let user_of = |store: &SqliteStore, key: &str| one_user(store, key);
    match cmd {
        WorkspaceCmd::List => {
            for w in store.list_workspaces()? {
                let members = store.workspace_members(w.id)?;
                println!(
                    "workspace {}  {:<24} owner {:<12} {} member{}{}",
                    w.id,
                    w.name,
                    w.owner_name.as_deref().unwrap_or("?"),
                    members.len(),
                    if members.len() == 1 { "" } else { "s" },
                    if w.shared { "  (shared)" } else { "" }
                );
            }
        }
        WorkspaceCmd::Members { workspace } => {
            let w = cli_workspace(store, &workspace)?;
            for m in store.workspace_members(w.id)? {
                println!("{}  {:<20} {:<7} since {}", m.user_id, m.name, m.role.as_str(), m.added_at);
            }
        }
        WorkspaceCmd::Share { workspace, user, role } => {
            let w = cli_workspace(store, &workspace)?;
            let u = user_of(store, &user)?;
            let role = taisce_store::Role::parse(&role).ok_or_else(|| anyhow::anyhow!("--role: editor | viewer"))?;
            store.share_workspace(w.id, u.id, role)?;
            tracing::info!(target: auth::AUDIT, event = "workspace.share", workspace = %w.id, user = %u.id, role = role.as_str(), by = "cli");
            println!("shared {} with {} as {} (history before today stays private)", w.name, u.name, role.as_str());
        }
        WorkspaceCmd::Unshare { workspace, user } => {
            let w = cli_workspace(store, &workspace)?;
            let u = user_of(store, &user)?;
            let removed = store.unshare_workspace(w.id, u.id)?;
            tracing::info!(target: auth::AUDIT, event = "workspace.unshare", workspace = %w.id, user = %u.id, by = "cli");
            println!("{} {} from {}", if removed { "removed" } else { "was not a member:" }, u.name, w.name);
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // before the runtime's threads exist: the env shim calls set_var
    let legacy_env = legacy::adopt_env();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the tokio runtime")?
        .block_on(run(legacy_env))
}

async fn run(legacy_env: Vec<String>) -> anyhow::Result<()> {
    install_crypto_provider();
    let cli = Cli::parse();
    let db_dir = cli
        .db
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&db_dir).context("creating db directory")?;
    // logging first: a store that will not open must say so in the log
    let (_log_guard, log_level) = init_logging(&db_dir);
    legacy::log_adopted(&legacy_env);
    let mut store = match SqliteStore::open(&cli.db) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(db = %cli.db.display(), "could not open the store: {e}");
            return Err(anyhow::Error::from(e).context(format!("opening store {}", cli.db.display())));
        }
    };
    apply_log_level(&log_level, store.get_setting("log.level").ok().flatten());
    let (tom, claude) = bootstrap_principals(&mut store)?;

    match cli.cmd {
        Cmd::Import { dir } => {
            let report = taisce_store::import::import_vault(&mut store, &dir, tom)?;
            println!(
                "imported {} docs, {} blocks; skipped {} files",
                report.docs,
                report.blocks,
                report.skipped.len()
            );
            for p in report.skipped {
                println!("  skipped: {}", p.display());
            }
        }
        Cmd::Export { dir } => {
            let report = taisce_store::export::export_vault(&store, &dir)?;
            println!("exported {} files to {}", report.files, dir.display());
        }
        Cmd::Gardener { cmd } => {
            let client = admin_client(&cli.db, None)?;
            let base = format!("http://127.0.0.1:{}", cli.port);
            match cmd {
                GardenerCmd::Add {
                    name,
                    task_prompt,
                    kind,
                    scope_doc,
                    policy,
                } => {
                    let body = serde_json::json!({
                        "name": name,
                        "kind": kind,
                        "task_prompt": task_prompt,
                        "scope_doc": scope_doc,
                        "confidence_policy": policy,
                    });
                    let r = client
                        .post(format!("{base}/admin/gardeners"))
                        .json(&body)
                        .send()
                        .await?;
                    println!("{}", r.text().await?);
                }
                GardenerCmd::List => {
                    let r = client.get(format!("{base}/admin/gardeners")).send().await?;
                    println!("{}", r.text().await?);
                }
            }
        }
        Cmd::Garden { name } => {
            let client = admin_client(&cli.db, Some(std::time::Duration::from_secs(600)))?;
            let r = client
                .post(format!("http://127.0.0.1:{}/admin/garden", cli.port))
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await?;
            println!("{}", r.text().await?);
        }
        Cmd::Policy { doc_id, policy } => {
            let body = serde_json::json!({
                "doc_id": doc_id,
                "policy": if policy == "clear" { serde_json::Value::Null } else { policy.clone().into() },
            });
            let client = admin_client(&cli.db, None)?;
            let r = client
                .post(format!("http://127.0.0.1:{}/admin/policy", cli.port))
                .json(&body)
                .send()
                .await?;
            println!("{}", r.text().await?);
        }
        Cmd::Runs => {
            let r = admin_client(&cli.db, None)?.get(format!("http://127.0.0.1:{}/admin/runs", cli.port)).send().await?;
            println!("{}", r.text().await?);
        }
        Cmd::Auth { cmd } => {
            let mut store = store;
            auth_cli(&mut store, cmd, cli.public_url.clone(), tom)?;
        }
        Cmd::Workspace { cmd } => {
            let mut store = store;
            workspace_cli(&mut store, cmd)?;
        }
        Cmd::Serve { trusted_proxy, allow_redirect, apns } => {
            let port = cli.port;
            let mut store = store;
            // SERVER mode: OAuth + passkeys replace loopback trust entirely
            let auth_cfg = match cli.public_url.as_deref() {
                Some(u) => {
                    let mut cfg = auth::AuthConfig::from_public_url(u)?;
                    cfg.trusted_proxy = trusted_proxy;
                    cfg.extra_redirects = allow_redirect.into_iter().filter(|r| !r.trim().is_empty()).collect();
                    store.set_setting("auth.public_url", &cfg.base)?;
                    let name = human_name(&store);
                    store.auth_ensure_owner(tom, &name, auth::now())?;
                    tracing::info!(public_url = cfg.base, trusted_proxy, "SERVER mode: every data route needs an OAuth bearer token");
                    Some(cfg)
                }
                None => None,
            };
            if let Ok(n) = store.mark_orphaned_runs()
                && n > 0
            {
                tracing::warn!("marked {n} orphaned gardener runs (daemon restarted mid-run)");
            }
            let store = taisce_store::SharedStore::new(store);
            #[cfg(unix)]
            tokio::spawn(watch_parent());
            // the local trust boundary for /admin/*: a per-boot token beside the
            // db; the shell and CLI read it, any other local process is refused
            // Win the port BEFORE minting the admin token: a second daemon
            // (a double launch, a stale sidecar racing a new one) must die at
            // bind, not overwrite the live daemon's token file on its way out.
            let addr = format!("127.0.0.1:{port}");
            let listener = match tokio::net::TcpListener::bind(&addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("port {port} is taken ({e}); another Taisce is already serving — exiting");
                    return Err(anyhow::anyhow!("port {port} in use: {e}"));
                }
            };
            let admin_token = admin::AdminToken::mint(&db_dir)
                .context("minting admin token")?;
            {
                let store = store.clone();
                supervise("gardener.daily", move || admin::daily_loop(store.clone()));
            }
            // Claude Code's per-project memory → `Claude Memory` docs, kept in
            // sync through the gate (changed memories arrive as reviewable)
            {
                let store = store.clone();
                supervise("memory.sync", move || memory::memory_loop(store.clone(), tom));
            }
            // daily self-contained db snapshot beside the db (backups/), keep 7
            {
                let db = cli.db.clone();
                supervise("backup.daily", move || backup::backup_loop(db.clone()));
            }
            // block embeddings (ask the vault): model compiled in, index kept
            // current block-by-block; a load failure degrades to keyword search
            let embedder = match embed::Embedder::load() {
                Ok(e) => {
                    let e = Arc::new(e);
                    {
                        let e = e.clone();
                        // the index holds every block (System); searches cut it per viewer
                        store_ext::with_store(&store, taisce_store::Scope::System, move |s| match e.load_index(s) {
                            Ok(n) => tracing::info!(vectors = n, dim = e.dim, "embedding index loaded"),
                            Err(err) => tracing::warn!("embedding index load failed: {err}"),
                        })
                        .await;
                    }
                    {
                        let (e, store) = (e.clone(), store.clone());
                        supervise("embed", move || embed::embed_loop(e.clone(), store.clone()));
                    }
                    Some(e)
                }
                Err(err) => {
                    tracing::warn!("embedding model unavailable; ask-the-vault uses keywords only: {err:#}");
                    None
                }
            };
            // the living-answers refresher retrieves the way a fresh ask does
            living::set_embedder(embedder.clone());
            // one idempotency cache for MCP and HTTP proposes (request_id)
            let dedupe = mcp::new_dedupe();
            {
                let store = store.clone();
                supervise("idempotency.cleanup", move || mcp::idempotency_cleanup_loop(store.clone()));
            }
            let auth_state = match auth_cfg {
                Some(cfg) => {
                    let st = auth::AuthState::new(cfg, store.clone())?;
                    {
                        let st = st.clone();
                        supervise("auth.cleanup", move || auth::cleanup_loop(st.clone()));
                    }
                    Some(st)
                }
                None => None,
            };
            // rmcp checks Host itself; behind the proxy it is the public name
            let mcp_hosts = auth_state.as_ref().map(|st| {
                let host = st.cfg.rp_id.clone();
                vec![st.cfg.authority(), host, "localhost".into(), "127.0.0.1".into(), "::1".into()]
            });
            // one feed: the store has a single commit hook
            let feed = changes::Feed::new(&store);
            // APNs (push.rs): device routes always answer; the sender runs
            // only in SERVER mode with a key configured
            let apns_cfg = match apns.config() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("APNs disabled: {e:#}");
                    None
                }
            };
            let default_env = apns_cfg.as_ref().map_or_else(|| apns.apns_env.clone(), |c| c.default_env.clone());
            let mut apns_sender: Option<Arc<push::ApnsSender>> = None;
            match (apns_cfg, auth_state.is_some()) {
                (Some(cfg), true) => {
                    tracing::info!(topic = cfg.topic, "APNs enabled: change nudges to registered devices");
                    match push::ApnsSender::new(cfg) {
                        Ok(sender) => {
                            let sender = Arc::new(sender);
                            apns_sender = Some(sender.clone());
                            let (store, feed) = (store.clone(), feed.clone());
                            supervise("push", move || {
                                push::push_loop(store.clone(), feed.clone(), sender.clone(), push::COALESCE)
                            });
                        }
                        Err(e) => tracing::error!("APNs disabled: {e:#}"),
                    }
                }
                (Some(_), false) => tracing::warn!("APNs configured but this is LOCAL mode: push needs --public-url"),
                (None, _) => {}
            }
            // ADR 0004: SERVER mode is multi-user; a request without a token's
            // identity is refused, never served as the local user
            let server_mode = auth_state.is_some();
            // share links: the owner routes (/api/shares) and the public
            // page (/s/{token}); every one answers 404 in LOCAL mode
            let shares_state = shares::SharesState {
                store: store.clone(),
                local_human: tom,
                server: auth_state.as_ref().map(|st| shares::ServerSide {
                    cfg: st.cfg.clone(),
                    limiter: st.limiter.clone(),
                    notify: apns_sender.clone().map(|sender| {
                        let store = store.clone();
                        Arc::new(move |a: shares::CommentAlert| {
                            let (store, sender) = (store.clone(), sender.clone());
                            let push = push::Push::ShareComment { title: a.title, author: a.author, doc_id: a.doc_id, share_id: a.share_id };
                            tokio::spawn(push::notify_share_comment(store, sender, a.owner, push));
                        }) as shares::Notifier
                    }),
                }),
            };
            let app = mcp::router_with_hosts(store.clone(), claude, dedupe.clone(), embedder.clone(), mcp_hosts)
                .merge(admin::router(store.clone(), admin_token, server_mode))
                .merge(push::router(push::DevicesState { store: store.clone(), default_env }))
                .merge(shares::router(shares_state))
                .merge(api::router(api::ApiState {
                    changes: feed,
                    store,
                    human: tom,
                    server_mode,
                    db_path: cli.db.clone(),
                    embedder,
                    dedupe,
                }));
            let app = match &auth_state {
                Some(st) => app.merge(auth::router(st.clone())),
                None => app,
            };
            // The frontend is EMBEDDED in this binary (rust-embed over ui/dist),
            // so the app is self-contained on any machine. TAISCE_UI_DIST is a
            // dev override: set it to serve a live build off disk instead.
            let app = match std::env::var("TAISCE_UI_DIST") {
                Ok(dir) => app.fallback_service(
                    tower_http::services::ServeDir::new(&dir)
                        .fallback(tower_http::services::ServeFile::new(format!("{dir}/index.html"))),
                ),
                Err(_) => app.fallback(serve_embedded_ui),
            };
            let app = match auth_state {
                // SERVER mode: the proxy makes every request loopback, so no
                // loopback trust; a bearer token on every data surface instead
                // (and the web UI's session cookie on /api); the UI's
                // security headers (CSP, no framing) over every response
                Some(st) => app
                    .layer(axum::middleware::from_fn_with_state(st, auth::require_auth))
                    .layer(axum::middleware::from_fn(auth::web::security_headers)),
                // DNS-rebinding guard over EVERY surface (api, admin, mcp, ws, ui):
                // a request whose Host/Origin is not a loopback name is refused
                None => app.layer(axum::middleware::from_fn(local_guard::require_loopback)),
            };
            // outermost: the pre-rename X-Grimoire-* headers, read as Taisce-*
            let app = app.layer(axum::middleware::from_fn(legacy::rename_headers));
            tracing::info!("ksd serving MCP (streamable HTTP) at http://{addr}/mcp");
            axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
                .with_graceful_shutdown(async {
                    shutdown_signal().await;
                    // children first: a slow connection drain must never
                    // leave a `claude -p` running past the daemon
                    children::kill_all().await;
                })
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod supervise_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn a_panicking_loop_is_restarted_after_backoff() {
        static STARTS: AtomicUsize = AtomicUsize::new(0);
        supervise_with("test.loop", std::time::Duration::from_millis(10), || async {
            let n = STARTS.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                panic!("first run dies");
            }
            std::future::pending::<()>().await;
        });
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while STARTS.load(Ordering::SeqCst) < 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(STARTS.load(Ordering::SeqCst), 2, "restarted exactly once, then kept running");
    }
}
