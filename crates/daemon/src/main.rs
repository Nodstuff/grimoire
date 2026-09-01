//! ksd — the knowledge-system daemon (PROJECT.md §3.2a).
//!
//! One process owns the SQLite file and serves every surface. Tonight:
//! MCP over streamable HTTP at /mcp. Web UI routes come later (M5).

mod admin;
mod api;
mod garden;
mod identity;
mod mcp;

use anyhow::Context;
use clap::{Parser, Subcommand};
use grimoire_store::{BlockStore, PrincipalKind, SqliteStore};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(name = "grimoire", about = "knowledge-system daemon")]
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

/// The two v1 principals, created on first run.
fn bootstrap_principals(store: &mut SqliteStore) -> anyhow::Result<(uuid::Uuid, uuid::Uuid)> {
    let existing = store.list_principals()?;
    let find = |name: &str| {
        existing
            .iter()
            .find(|p| p.display_name == name)
            .map(|p| p.id)
    };
    let tom = match find("tom") {
        Some(id) => id,
        None => {
            store
                .create_principal(PrincipalKind::Human, "tom", None)?
                .id
        }
    };
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    if let Some(parent) = cli.db.parent() {
        std::fs::create_dir_all(parent).context("creating db directory")?;
    }
    let mut store = SqliteStore::open(&cli.db).context("opening store")?;
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
            let client = reqwest::Client::new();
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
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()?;
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
            let client = reqwest::Client::new();
            let r = client
                .post("http://127.0.0.1:7425/admin/policy")
                .json(&body)
                .send()
                .await?;
            println!("{}", r.text().await?);
        }
        Cmd::Runs => {
            let r = reqwest::get("http://127.0.0.1:7425/admin/runs").await?;
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
            match identity::Identity::load_or_create(&db_dir) {
                Ok(id) => {
                    store.set_principal_pubkey(tom, &id.node_id())?;
                    tracing::info!("federation identity: {}", id.fingerprint());
                }
                Err(e) => tracing::warn!("no federation identity available: {e:#}"),
            }
            if let Ok(n) = store.mark_orphaned_runs()
                && n > 0
            {
                tracing::warn!("marked {n} orphaned gardener runs (daemon restarted mid-run)");
            }
            let store = Arc::new(Mutex::new(store));
            tokio::spawn(admin::daily_loop(store.clone()));
            // ui/dist next to the binary's repo root; fall back to cwd
            let ui_dist = std::env::var("GRIMOIRE_UI_DIST")
                .unwrap_or_else(|_| "/Users/tmeaney/personal/knowledge-system/ui/dist".into());
            let app = mcp::router(store.clone(), claude)
                .merge(admin::router(store.clone()))
                .merge(api::router(api::ApiState { store, human: tom }))
                .fallback_service(tower_http::services::ServeDir::new(&ui_dist).fallback(
                    tower_http::services::ServeFile::new(format!("{ui_dist}/index.html")),
                ));
            let addr = format!("127.0.0.1:{port}");
            tracing::info!("ksd serving MCP (streamable HTTP) at http://{addr}/mcp");
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    tokio::signal::ctrl_c().await.ok();
                })
                .await?;
        }
    }
    Ok(())
}
