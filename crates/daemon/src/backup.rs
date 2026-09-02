//! Backups: a daily consistent snapshot of the database beside it.
//!
//! The live db is a WAL pair (`ks.db` + `ks.db-wal`); a file-level copy of
//! one without the other (Time Machine catching them seconds apart, a user
//! dragging `ks.db` to a USB stick) is a corrupt-or-behind database. So the
//! daemon writes `backups/ks-YYYY-MM-DD.db` itself with `VACUUM INTO` — one
//! self-contained file — once a day and on demand, keeping the last `KEEP`.
//!
//! Cost: one read transaction and a file the size of the db (29 MB today).
//! It runs when the daemon starts (if today's is missing) and then daily.

use grimoire_store::SqliteStore;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const KEEP: usize = 7;
const PREFIX: &str = "ks-";
const SUFFIX: &str = ".db";

/// Where snapshots live for a db at `db_path`.
pub fn backup_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("backups")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupInfo {
    pub path: String,
    pub date: String,
    pub bytes: u64,
}

/// Existing snapshots, newest first.
pub fn list_backups(db_path: &Path) -> Vec<BackupInfo> {
    list_in(&backup_dir(db_path))
}

fn list_in(dir: &Path) -> Vec<BackupInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<BackupInfo> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let date = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?.to_string();
            let bytes = e.metadata().ok()?.len();
            Some(BackupInfo {
                path: e.path().to_string_lossy().to_string(),
                date,
                bytes,
            })
        })
        .collect();
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out
}

/// Take a snapshot now. Returns the new file, or the existing one if today's
/// snapshot is already there (`force` replaces it). Prunes to `KEEP`.
pub fn backup_now(
    store: &Arc<Mutex<SqliteStore>>,
    db_path: &Path,
    force: bool,
) -> anyhow::Result<BackupInfo> {
    let dir = backup_dir(db_path);
    std::fs::create_dir_all(&dir)?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let target = dir.join(format!("{PREFIX}{today}{SUFFIX}"));
    if target.exists() {
        if !force {
            let bytes = std::fs::metadata(&target)?.len();
            return Ok(BackupInfo {
                path: target.to_string_lossy().to_string(),
                date: today,
                bytes,
            });
        }
        std::fs::remove_file(&target)?;
    }
    // write to a temp name and rename: a crash mid-VACUUM never leaves a
    // half-written file that looks like a backup
    let tmp = dir.join(format!(".{PREFIX}{today}{SUFFIX}.partial"));
    std::fs::remove_file(&tmp).ok();
    {
        let s = store.lock().unwrap_or_else(|p| p.into_inner());
        s.backup_to(&tmp)?;
    }
    std::fs::rename(&tmp, &target)?;
    let bytes = std::fs::metadata(&target)?.len();
    prune(&dir);
    tracing::info!(path = %target.display(), bytes, "database backup written");
    Ok(BackupInfo {
        path: target.to_string_lossy().to_string(),
        date: today,
        bytes,
    })
}

fn prune(dir: &Path) {
    for old in list_in(dir).iter().skip(KEEP) {
        if let Err(e) = std::fs::remove_file(&old.path) {
            tracing::warn!(path = %old.path, "could not prune old backup: {e}");
        }
    }
}

/// Once at start (if today's snapshot is missing), then every 24h. Quiet on
/// success; a failure is a WARN, never a crash — the daemon's job is the notes.
pub async fn backup_loop(store: Arc<Mutex<SqliteStore>>, db_path: PathBuf) {
    // let the daemon settle before the first read transaction
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    loop {
        let s = store.clone();
        let p = db_path.clone();
        let res = tokio::task::spawn_blocking(move || backup_now(&s, &p, false)).await;
        match res {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!("daily backup failed: {e:#}"),
            Err(e) => tracing::warn!("daily backup task panicked: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grimoire_store::{BlockStore, PrincipalKind};

    #[test]
    fn backup_is_a_self_contained_db_and_prunes_to_keep() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ks.db");
        let store = SqliteStore::open(&db_path).unwrap();
        let mut store = store;
        let tom = store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        store.create_doc("kept", None, tom.id).unwrap();
        let store = Arc::new(Mutex::new(store));

        let info = backup_now(&store, &db_path, false).unwrap();
        assert!(Path::new(&info.path).exists());
        // idempotent for the day
        let again = backup_now(&store, &db_path, false).unwrap();
        assert_eq!(again.path, info.path);
        // the copy opens on its own and has the data
        let copy = SqliteStore::open(&info.path).unwrap();
        assert_eq!(copy.list_docs().unwrap()[0].title, "kept");

        // fake KEEP+3 older snapshots; prune keeps the newest KEEP
        let bdir = backup_dir(&db_path);
        for i in 1..=(KEEP + 3) {
            std::fs::write(bdir.join(format!("{PREFIX}2000-01-{i:02}{SUFFIX}")), b"x").unwrap();
        }
        backup_now(&store, &db_path, true).unwrap();
        assert_eq!(list_backups(&db_path).len(), KEEP);
        assert_eq!(list_backups(&db_path)[0].date, info.date);
    }
}
