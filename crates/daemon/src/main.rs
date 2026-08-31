//! ksd — the knowledge-system daemon (PROJECT.md §3.2a).
//!
//! One process owns the SQLite file and serves every surface. Tonight:
//! MCP over streamable HTTP at /mcp. Web UI routes come later (M5).

mod mcp;

use anyhow::Context;
use clap::{Parser, Subcommand};
use ks_store::{BlockStore, PrincipalKind, SqliteStore};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(name = "ksd", about = "knowledge-system daemon")]
struct Cli {
    /// Path to the SQLite database.
    #[arg(long, default_value_os_t = default_db())]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Import a markdown vault (one-shot).
    Import { dir: PathBuf },
    /// Export all docs to a markdown directory tree.
    Export { dir: PathBuf },
    /// Serve MCP over streamable HTTP.
    Serve {
        #[arg(long, default_value_t = 7425)]
        port: u16,
    },
}

fn default_db() -> PathBuf {
    dirs_home().join(".knowledge-system/ks.db")
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
            let report = ks_store::import::import_vault(&mut store, &dir, tom)?;
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
            let report = ks_store::export::export_vault(&store, &dir)?;
            println!("exported {} files to {}", report.files, dir.display());
        }
        Cmd::Serve { port } => {
            let store = Arc::new(Mutex::new(store));
            let app = mcp::router(store, claude);
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
