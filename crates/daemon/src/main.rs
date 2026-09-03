//! ksd — the knowledge-system daemon (PROJECT.md §3.2a).
//!
//! One process owns the SQLite file and serves every surface. Tonight:
//! MCP over streamable HTTP at /mcp. Web UI routes come later (M5).

mod admin;
mod api;
mod ask;
mod backup;
mod embed;
mod fed;
mod garden;
mod hot;
mod identity;
mod yrender;
mod mcp;
mod memory;
mod room;
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
    *STAMP.get_or_init(|| {
        let Some(f) = EmbeddedUi::get("index.html") else { return 0 };
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in f.data.iter() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    })
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
    /// Serve MCP over streamable HTTP.
    Serve {
        #[arg(long, default_value_t = 7425)]
        port: u16,
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

/// One log, owned by the daemon: a daily-rolled file beside the db (the
/// shell used to redirect stdout into a second, never-rotating file), plus
/// stdout when run from a terminal. Level: RUST_LOG, else the `log.level`
/// setting, else info. Returns the non-blocking writer's guard — drop it and
/// buffered lines are lost, so `main` holds it until exit.
fn init_logging(db_dir: &std::path::Path, level: Option<String>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use std::io::IsTerminal;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new(level.as_deref().unwrap_or("info")))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
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
            Some(guard)
        }
        Err(e) => {
            // no writable log dir: stderr is better than silence
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
                .init();
            tracing::warn!("log file unavailable ({e}); logging to stderr only");
            None
        }
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let db_dir = cli
        .db
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&db_dir).context("creating db directory")?;
    let mut store = SqliteStore::open(&cli.db).context("opening store")?;
    // logging waits for the store: the level may live in settings (log.level)
    let level = store.get_setting("log.level").ok().flatten();
    let _log_guard = init_logging(&db_dir, level);
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
            let base = "http://127.0.0.1:7425";
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
                .post("http://127.0.0.1:7425/admin/garden")
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
                .post("http://127.0.0.1:7425/admin/policy")
                .json(&body)
                .send()
                .await?;
            println!("{}", r.text().await?);
        }
        Cmd::Runs => {
            let r = admin_client(&cli.db, None)?.get("http://127.0.0.1:7425/admin/runs").send().await?;
            println!("{}", r.text().await?);
        }
        Cmd::Share { cmd } => {
            let client = admin_client(&cli.db, None)?;
            let base = "http://127.0.0.1:7425";
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
                .post("http://127.0.0.1:7425/admin/join")
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
            let r = admin_client(&cli.db, None)?.get("http://127.0.0.1:7425/admin/contacts").send().await?;
            println!("{}", r.text().await?);
        }
        Cmd::Pull => {
            let client = admin_client(&cli.db, Some(std::time::Duration::from_secs(120)))?;
            let r = client
                .post("http://127.0.0.1:7425/admin/pull")
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
        Cmd::Serve { port } => {
            let mut store = store;
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
            tokio::spawn(hot::idle_loop(hot.clone(), store.clone()));
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
                    Ok(ep) => {
                        fed_ctx.node_id = Some(id.node_id());
                        fed_ctx.endpoint = Some(ep.clone());
                        tokio::spawn(fed::serve(ep.clone(), store.clone(), hot.clone(), runtime.clone()));
                        tokio::spawn(fed::join_retry_loop(ep.clone(), store.clone()));
                        // grantee side: adaptive pull; owner side: nudge grantees on change
                        tokio::spawn(fed::pull_loop(ep.clone(), store.clone(), runtime.clone()));
                        tokio::spawn(fed::notify_loop(ep, store.clone(), hot.clone()));
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
            tokio::spawn(admin::daily_loop(store.clone(), hot.clone()));
            // daily self-contained db snapshot beside the db (backups/), keep 7
            tokio::spawn(backup::backup_loop(store.clone(), cli.db.clone()));
            // Claude Code's per-project memory → `Claude Memory` docs, kept in
            // sync through the gate (changed memories arrive as reviewable)
            tokio::spawn(memory::memory_loop(store.clone(), tom));
            // block embeddings (ask the vault): model compiled in, index kept
            // current block-by-block; a load failure degrades to keyword search
            let embedder = match embed::Embedder::load() {
                Ok(e) => {
                    let e = Arc::new(e);
                    {
                        let s = store.lock().unwrap_or_else(|p| p.into_inner());
                        match e.load_index(&s) {
                            Ok(n) => tracing::info!(vectors = n, dim = e.dim, "embedding index loaded"),
                            Err(err) => tracing::warn!("embedding index load failed: {err}"),
                        }
                    }
                    tokio::spawn(embed::embed_loop(e.clone(), store.clone()));
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
                    admin::router(store.clone(), fed_ctx, hot.clone(), admin_token)
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
            tracing::info!("ksd serving MCP (streamable HTTP) at http://{addr}/mcp");
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::signal::ctrl_c().await.ok();
                })
                .await?;
        }
    }
    Ok(())
}
