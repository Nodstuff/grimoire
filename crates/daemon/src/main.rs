//! ksd — the knowledge-system daemon (PROJECT.md §3.2a).
//!
//! One process owns the SQLite file and serves every surface. Tonight:
//! MCP over streamable HTTP at /mcp. Web UI routes come later (M5).

mod admin;
mod api;
mod ask;
mod backup;
mod children;
mod embed;
mod fed;
mod garden;
mod hot;
mod identity;
mod local_guard;
mod yrender;
mod mcp;
mod memory;
mod room;
mod store_ext;
#[cfg(test)]
mod retrieval_probe;

use anyhow::Context;
use clap::{Parser, Subcommand};
use grimoire_store::{BlockStore, PrincipalKind, SqliteStore};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
        // no embedded UI (a cross-compiled hub build): the git sha still
        // distinguishes one binary from the next instead of a flat 0
        None => match GIT_SHA {
            Some(sha) if !sha.is_empty() => fnv1a(sha.as_bytes()),
            _ => 0,
        },
    })
}

/// Short git sha of the checkout this binary was built from (build.rs); None
/// when built outside a git checkout.
pub const GIT_SHA: Option<&str> = option_env!("GRIMOIRE_GIT_SHA");

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
/// this a panic in, say, the pull loop silently ends federation until the
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
#[command(name = "grimoire", about = "Grimoire daemon")]
struct Cli {
    /// Path to the SQLite database.
    #[arg(long, default_value_os_t = default_db())]
    db: PathBuf,
    /// The daemon's port: what `serve` listens on and what every other
    /// command talks to.
    #[arg(long, global = true, default_value_t = 7425)]
    port: u16,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum GardenerCmd {
    Add {
        name: String,
        task_prompt: String,
        /// tagging (default) or reviewer
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
        /// Run as a hub: a team Grimoire members join, publish to, and read
        /// from. Persisted — later plain `serve` runs stay a hub.
        #[arg(long)]
        hub: bool,
        /// The hub's name (its root folder and the name members see).
        #[arg(long, requires = "hub")]
        name: Option<String>,
    },
    /// Hub administration on the hub box (talks to the running daemon).
    Hub {
        #[command(subcommand)]
        cmd: HubCmd,
    },
    /// Show the instance's federation identity (ADR 0002); export/import
    /// move it between machines.
    Identity {
        #[command(subcommand)]
        cmd: Option<IdentityCmd>,
    },
    /// Manage shares (talks to the running daemon).
    Share {
        #[command(subcommand)]
        cmd: ShareCmd,
    },
    /// Join a share from a grimoire://join/… invite link.
    Join { link: String },
    /// Pull all shared mirrors from their owners now.
    Pull,
    /// List paired contacts.
    Contacts,
}

#[derive(Subcommand)]
enum ShareCmd {
    /// Share a doc's subtree: mints a one-time invite link (7-day validity).
    Invite {
        doc_id: String,
        /// view (default) or propose
        #[arg(long, default_value = "view")]
        permission: String,
    },
    List,
    Revoke {
        share_id: String,
    },
    /// Set a share's trust tier: review (park, default), yellow (trusted:
    /// applies flagged) or green (maintainer: applies directly, you're notified).
    Trust {
        share_id: String,
        trust: String,
    },
}

#[derive(Subcommand)]
enum HubCmd {
    /// List members and pending requests.
    Members,
    /// Approve a pending member (offers them the hub folder).
    Approve { contact_id: String },
    /// Remove a member: their publications and access go, and they are blocked.
    Eject { contact_id: String },
    /// Set a member's role: member | admin.
    Role { contact_id: String, role: String },
    /// Mint a one-time invite link to the hub (7-day validity).
    Invite,
}

#[derive(Subcommand)]
enum IdentityCmd {
    /// Write the identity key to a file (0600) for machine migration.
    Export { path: PathBuf },
    /// Adopt an exported identity, replacing this machine's key.
    Import { path: PathBuf },
}

fn default_db() -> PathBuf {
    dirs_home().join(".grimoire/ks.db")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The display name a fresh install starts with: the macOS account's full
/// name, else the login name, else "me". It is the petname others see when
/// this instance pairs with them, so it must never be a hardcoded placeholder
/// — a network of instances all called "tom" is indistinguishable.
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
    let human = match existing.iter().find(|p| p.kind == PrincipalKind::Human) {
        Some(p) => p.id,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db_dir = cli
        .db
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&db_dir).context("creating db directory")?;
    // logging first: a store that will not open must say so in the log
    let (_log_guard, log_level) = init_logging(&db_dir);
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
            let report = grimoire_store::import::import_vault(&mut store, &dir, tom)?;
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
            let report = grimoire_store::export::export_vault(&store, &dir)?;
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
        Cmd::Share { cmd } => {
            let client = admin_client(&cli.db, None)?;
            let base = format!("http://127.0.0.1:{}", cli.port);
            match cmd {
                ShareCmd::Invite { doc_id, permission } => {
                    let r = client
                        .post(format!("{base}/admin/shares"))
                        .json(&serde_json::json!({"root_doc": doc_id, "permission": permission}))
                        .send()
                        .await?;
                    let v: serde_json::Value = r.json().await?;
                    match v.get("link").and_then(|l| l.as_str()) {
                        Some(link) => {
                            println!("{link}");
                            println!("(one-time, expires in 7 days — send it over a private channel)");
                        }
                        None => println!("{v}"),
                    }
                }
                ShareCmd::List => {
                    let r = client.get(format!("{base}/admin/shares")).send().await?;
                    println!("{}", r.text().await?);
                }
                ShareCmd::Revoke { share_id } => {
                    let r = client
                        .post(format!("{base}/admin/shares/revoke"))
                        .json(&serde_json::json!({"id": share_id}))
                        .send()
                        .await?;
                    println!("{}", r.text().await?);
                }
                ShareCmd::Trust { share_id, trust } => {
                    let r = client
                        .post(format!("{base}/admin/shares/trust"))
                        .json(&serde_json::json!({"id": share_id, "trust": trust}))
                        .send()
                        .await?;
                    println!("{}", r.text().await?);
                }
            }
        }
        Cmd::Join { link } => {
            let client = admin_client(&cli.db, Some(std::time::Duration::from_secs(30)))?;
            let r = client
                .post(format!("http://127.0.0.1:{}/admin/join", cli.port))
                .json(&serde_json::json!({"link": link}))
                .send()
                .await?;
            let v: serde_json::Value = r.json().await?;
            if let Some(j) = v.get("joined") {
                println!(
                    "joined \"{}\" from {} ({})",
                    j["root_title"].as_str().unwrap_or("?"),
                    j["owner_name"].as_str().unwrap_or("?"),
                    j["permission"].as_str().unwrap_or("?"),
                );
            } else if v.get("queued").is_some() {
                println!("owner unreachable — join queued, will retry in the background");
            } else {
                println!("{v}");
            }
        }
        Cmd::Contacts => {
            let r = admin_client(&cli.db, None)?.get(format!("http://127.0.0.1:{}/admin/contacts", cli.port)).send().await?;
            println!("{}", r.text().await?);
        }
        Cmd::Pull => {
            let client = admin_client(&cli.db, Some(std::time::Duration::from_secs(120)))?;
            let r = client
                .post(format!("http://127.0.0.1:{}/admin/pull", cli.port))
                .send()
                .await?;
            println!("{}", r.text().await?);
        }
        Cmd::Identity { cmd } => {
            let db_dir = cli.db.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
            match cmd {
                None => {
                    let id = identity::Identity::load_or_create(&db_dir)?;
                    println!("node id:     {}", id.node_id());
                    println!("fingerprint: {}", id.fingerprint());
                }
                Some(IdentityCmd::Export { path }) => {
                    let id = identity::Identity::load_or_create(&db_dir)?;
                    id.export(&path)?;
                    println!("identity exported to {} (0600)", path.display());
                }
                Some(IdentityCmd::Import { path }) => {
                    let id = identity::Identity::import(&path, &db_dir)?;
                    println!("identity imported; node id: {}", id.node_id());
                }
            }
        }
        Cmd::Hub { cmd } => {
            let client = admin_client(&cli.db, Some(std::time::Duration::from_secs(30)))?;
            let base = format!("http://127.0.0.1:{}", cli.port);
            let text = match cmd {
                HubCmd::Members => client.get(format!("{base}/admin/hub/members")).send().await?.text().await?,
                HubCmd::Approve { contact_id } => {
                    client
                        .post(format!("{base}/admin/hub/approve"))
                        .json(&serde_json::json!({"contact_id": contact_id}))
                        .send()
                        .await?
                        .text()
                        .await?
                }
                HubCmd::Eject { contact_id } => {
                    client
                        .post(format!("{base}/admin/hub/eject"))
                        .json(&serde_json::json!({"contact_id": contact_id}))
                        .send()
                        .await?
                        .text()
                        .await?
                }
                HubCmd::Role { contact_id, role } => {
                    client
                        .post(format!("{base}/admin/hub/role"))
                        .json(&serde_json::json!({"contact_id": contact_id, "role": role}))
                        .send()
                        .await?
                        .text()
                        .await?
                }
                HubCmd::Invite => {
                    let v: serde_json::Value = client
                        .post(format!("{base}/admin/hub/invite"))
                        .send()
                        .await?
                        .json()
                        .await?;
                    match v.get("link").and_then(|l| l.as_str()) {
                        Some(link) => format!("{link}\n(one-time, expires in 7 days — the first person to join becomes admin)"),
                        None => v.to_string(),
                    }
                }
            };
            println!("{text}");
        }
        Cmd::Serve { hub, name } => {
            let port = cli.port;
            let mut store = store;
            // hub mode (slice 1): persisted; `--hub` turns it on (and renames)
            if hub {
                let cfg = fed::hub::enable(&mut store, name.as_deref(), tom).context("enabling hub mode")?;
                tracing::info!(name = cfg.name, root = %cfg.root_doc, "hub mode enabled");
            }
            let hub_mode = fed::hub::config(&store);
            if let Some(h) = &hub_mode {
                tracing::info!(name = h.name, "serving as a hub: gardener schedule and memory sync are off");
            }
            // federation identity: minted silently on first serve, linked to
            // the human principal so provenance and pubkey agree (#54)
            let db_dir = cli.db.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
            let fed_identity = match identity::Identity::load_or_create(&db_dir) {
                Ok(id) => {
                    store.set_principal_pubkey(tom, &id.node_id())?;
                    tracing::info!("federation identity: {}", id.fingerprint());
                    Some(id)
                }
                Err(e) => {
                    tracing::warn!("no federation identity; federation disabled: {e:#}");
                    None
                }
            };
            if let Ok(n) = store.mark_orphaned_runs()
                && n > 0
            {
                tracing::warn!("marked {n} orphaned gardener runs (daemon restarted mid-run)");
            }
            let store = Arc::new(Mutex::new(store));
            // hot sessions (#65): journal-backed live co-editing state, created
            // first because every write surface (fed, gardeners, api, mcp)
            // consults it for the freeze. Journals live beside the db so
            // multiple instances never share.
            let hot = hot::HotState::new(
                cli.db.parent().unwrap_or(std::path::Path::new(".")).join("hot"),
            );
            hot.recover(&store);
            {
                let (hot, store) = (hot.clone(), store.clone());
                supervise("hot.idle", move || hot::idle_loop(hot.clone(), store.clone()));
            }
            // federation listener: separate iroh surface, deny-by-default
            // (ADR 0002 decision 7); the HTTP router below never sees it —
            // the admin routes only get the endpoint handle for outbound
            // joins and the node id for minting links
            // federation runtime state: focus heartbeats + received nudges
            let runtime = fed::Runtime::default();
            let mut fed_ctx = admin::FedCtx {
                node_id: None,
                endpoint: None,
            };
            if let Some(id) = fed_identity {
                match fed::bind(id.secret_bytes()).await {
                    Ok((ep, mdns)) => {
                        fed_ctx.node_id = Some(id.node_id());
                        fed_ctx.endpoint = Some(ep.clone());
                        // advertise our profile name on the LAN so neighbours
                        // read "Tom's MacBook", not a key
                        {
                            let name = store_ext::with_store(&store, |s| {
                                s.list_principals()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .find(|p| p.kind == PrincipalKind::Human)
                                    .map(|p| p.display_name)
                                    .unwrap_or_default()
                            })
                            .await;
                            if let Ok(ud) = name.parse::<iroh::address_lookup::UserData>() {
                                ep.set_user_data_for_address_lookup(Some(ud));
                            }
                        }
                        tokio::spawn(fed::neighbour_loop(mdns, runtime.clone()));
                        {
                            let (ep, store, hot, runtime) = (ep.clone(), store.clone(), hot.clone(), runtime.clone());
                            supervise("fed.serve", move || {
                                fed::serve(ep.clone(), store.clone(), hot.clone(), runtime.clone())
                            });
                        }
                        {
                            let (ep, store) = (ep.clone(), store.clone());
                            supervise("fed.join_retry", move || fed::join_retry_loop(ep.clone(), store.clone()));
                        }
                        // grantee side: adaptive pull; owner side: nudge grantees on change
                        {
                            let (ep, store, runtime) = (ep.clone(), store.clone(), runtime.clone());
                            supervise("fed.pull", move || fed::pull_loop(ep.clone(), store.clone(), runtime.clone()));
                        }
                        {
                            let (store, hot) = (store.clone(), hot.clone());
                            supervise("fed.notify", move || fed::notify_loop(ep.clone(), store.clone(), hot.clone()));
                        }
                    }
                    Err(e) => tracing::warn!("federation endpoint failed to bind: {e:#}"),
                }
            }
            // the local trust boundary for /admin/*: a per-boot token beside the
            // db; the shell and CLI read it, any other local process is refused
            // Win the port BEFORE minting the admin token: a second daemon
            // (a double launch, a stale sidecar racing a new one) must die at
            // bind, not overwrite the live daemon's token file on its way out.
            let addr = format!("127.0.0.1:{port}");
            let listener = match tokio::net::TcpListener::bind(&addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("port {port} is taken ({e}); another Grimoire is already serving — exiting");
                    return Err(anyhow::anyhow!("port {port} in use: {e}"));
                }
            };
            let admin_token = admin::AdminToken::mint(&db_dir)
                .context("minting admin token")?;
            // a hub has no gardeners to schedule and no Claude memory to mirror
            if hub_mode.is_none() {
                {
                    let (store, hot) = (store.clone(), hot.clone());
                    supervise("gardener.daily", move || admin::daily_loop(store.clone(), hot.clone()));
                }
                // Claude Code's per-project memory → `Claude Memory` docs, kept in
                // sync through the gate (changed memories arrive as reviewable)
                {
                    let store = store.clone();
                    supervise("memory.sync", move || memory::memory_loop(store.clone(), tom));
                }
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
                        store_ext::with_store(&store, move |s| match e.load_index(s) {
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
            let fed_ctx_node_id: Option<String>;
            // one idempotency cache for MCP and HTTP proposes (request_id)
            let dedupe = mcp::new_dedupe();
            let app = mcp::router(store.clone(), claude, hot.clone(), dedupe.clone())
                .merge(hot::router(hot::HotCtx {
                    hot: hot.clone(),
                    store: store.clone(),
                    endpoint: fed_ctx.endpoint.clone(),
                }))
                .merge({
                    fed_ctx_node_id = fed_ctx.node_id.clone();
                    admin::router(store.clone(), fed_ctx, hot.clone(), runtime.clone(), admin_token)
                })
                .merge(api::router(api::ApiState {
                    store,
                    human: tom,
                    hot,
                    runtime,
                    db_path: cli.db.clone(),
                    node_id: fed_ctx_node_id,
                    embedder,
                    dedupe,
                }));
            // The frontend is EMBEDDED in this binary (rust-embed over ui/dist),
            // so the app is self-contained on any machine. GRIMOIRE_UI_DIST is a
            // dev override: set it to serve a live build off disk instead.
            let app = match std::env::var("GRIMOIRE_UI_DIST") {
                Ok(dir) => app.fallback_service(
                    tower_http::services::ServeDir::new(&dir)
                        .fallback(tower_http::services::ServeFile::new(format!("{dir}/index.html"))),
                ),
                Err(_) => app.fallback(serve_embedded_ui),
            };
            // DNS-rebinding guard over EVERY surface (api, admin, mcp, ws, ui):
            // a request whose Host/Origin is not a loopback name is refused
            let app = app.layer(axum::middleware::from_fn(local_guard::require_loopback));
            tracing::info!("ksd serving MCP (streamable HTTP) at http://{addr}/mcp");
            axum::serve(listener, app)
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
