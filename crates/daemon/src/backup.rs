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

use std::path::{Path, PathBuf};

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
///
/// Reads through a SECOND, read-only connection to the same file: the
/// store's mutex is never held for the seconds a 30 MB VACUUM takes, so the
/// UI and MCP keep answering. WAL gives the copy a consistent snapshot.
pub fn backup_now(db_path: &Path, force: bool) -> anyhow::Result<BackupInfo> {
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
    let bytes = snapshot_to(db_path, &target)?;
    prune(&dir);
    tracing::info!(path = %target.display(), bytes, "database backup written");
    Ok(BackupInfo {
        path: target.to_string_lossy().to_string(),
        date: today,
        bytes,
    })
}

/// A snapshot to a place of the user's choosing (⌘K → *Back up database
/// to…*, a native Save sheet): a USB stick, iCloud Drive, a sync folder —
/// off this Mac, which the daily snapshot beside the db never is. Nothing is
/// pruned there; the file is theirs. Refused: a relative path, a name not
/// ending in `.db` (Finder would show a mystery file), a folder that does
/// not exist, and the data directory itself (nothing may land beside the
/// live WAL pair).
pub fn backup_to(db_path: &Path, target: &Path) -> anyhow::Result<BackupInfo> {
    anyhow::ensure!(target.is_absolute(), "backup path must be absolute: {}", target.display());
    let ext_ok = target
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("db"));
    anyhow::ensure!(ext_ok, "backup file name must end in .db: {}", target.display());
    let dir = target
        .parent()
        .filter(|d| d.is_dir())
        .ok_or_else(|| anyhow::anyhow!("backup folder does not exist: {}", target.display()))?;
    let data_dir = db_path.parent().unwrap_or(Path::new("."));
    let same_dir = match (std::fs::canonicalize(dir), std::fs::canonicalize(data_dir)) {
        (Ok(a), Ok(b)) => a == b,
        _ => dir == data_dir,
    };
    anyhow::ensure!(
        !same_dir,
        "choose a folder other than {} — nothing may land beside the live database",
        data_dir.display()
    );
    let bytes = snapshot_to(db_path, target)?;
    tracing::info!(path = %target.display(), bytes, "database backup written (chosen location)");
    Ok(BackupInfo {
        path: target.to_string_lossy().to_string(),
        date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        bytes,
    })
}

/// `VACUUM INTO` the live db as one self-contained file at `target`,
/// replacing whatever is there. Reads through a SECOND read-only connection
/// so the store's mutex is never held; writes `.<name>.partial` beside the
/// target and renames, so a crash mid-copy never leaves a half-written file
/// that looks like a backup. Returns the size written.
fn snapshot_to(db_path: &Path, target: &Path) -> anyhow::Result<u64> {
    let dir = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("backup target has no folder: {}", target.display()))?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("backup target has no file name: {}", target.display()))?;
    let tmp = dir.join(format!(".{name}.partial"));
    std::fs::remove_file(&tmp).ok();
    {
        use rusqlite::OpenFlags;
        let ro = rusqlite::Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        ro.busy_timeout(std::time::Duration::from_secs(10))?;
        ro.execute("VACUUM INTO ?1", rusqlite::params![tmp.to_string_lossy()])?;
    }
    std::fs::rename(&tmp, target)?;
    Ok(std::fs::metadata(target)?.len())
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
pub async fn backup_loop(db_path: PathBuf) {
    // let the daemon settle before the first read transaction
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    loop {
        let p = db_path.clone();
        let res = tokio::task::spawn_blocking(move || backup_now(&p, false)).await;
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
    use grimoire_store::{BlockStore, PrincipalKind, SqliteStore};
    use std::sync::{Arc, Mutex};

    #[test]
    fn backup_is_a_self_contained_db_and_prunes_to_keep() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ks.db");
        let store = SqliteStore::open(&db_path).unwrap();
        let mut store = store;
        let tom = store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        store.create_doc("kept", None, tom.id).unwrap();
        let store = Arc::new(Mutex::new(store));

        // the store's lock is HELD throughout: the backup must not need it
        let held = store.lock().unwrap();
        let info = backup_now(&db_path, false).unwrap();
        assert!(Path::new(&info.path).exists());
        // idempotent for the day
        let again = backup_now(&db_path, false).unwrap();
        assert_eq!(again.path, info.path);
        drop(held);
        // the copy opens on its own and has the data (written via the WAL,
        // never checkpointed — the snapshot still sees it)
        let copy = SqliteStore::open(&info.path).unwrap();
        assert_eq!(copy.list_docs().unwrap()[0].title, "kept");

        // fake KEEP+3 older snapshots; prune keeps the newest KEEP
        let bdir = backup_dir(&db_path);
        for i in 1..=(KEEP + 3) {
            std::fs::write(bdir.join(format!("{PREFIX}2000-01-{i:02}{SUFFIX}")), b"x").unwrap();
        }
        backup_now(&db_path, true).unwrap();
        assert_eq!(list_backups(&db_path).len(), KEEP);
        assert_eq!(list_backups(&db_path)[0].date, info.date);
    }

    #[test]
    fn backup_to_writes_where_asked_and_refuses_the_unsafe_places() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ks.db");
        let mut store = SqliteStore::open(&db_path).unwrap();
        let tom = store.create_principal(PrincipalKind::Human, "tom", None).unwrap();
        store.create_doc("kept", None, tom.id).unwrap();

        let dest = tempfile::tempdir().unwrap();
        let target = dest.path().join("grimoire-2026-09-07.db");
        let info = backup_to(&db_path, &target).unwrap();
        assert_eq!(Path::new(&info.path), target);
        let copy = SqliteStore::open(&target).unwrap();
        assert_eq!(copy.list_docs().unwrap()[0].title, "kept");
        // a second run replaces the file rather than failing on "exists"
        backup_to(&db_path, &target).unwrap();
        // no temp file left behind
        assert!(!dest.path().join(".grimoire-2026-09-07.db.partial").exists());

        // the chosen backup never counts as a daily snapshot
        assert!(list_backups(&db_path).is_empty());

        // refusals: relative, wrong extension, missing folder, the data dir itself
        assert!(backup_to(&db_path, Path::new("relative.db")).is_err());
        assert!(backup_to(&db_path, &dest.path().join("notes.sqlite")).is_err());
        assert!(backup_to(&db_path, &dest.path().join("nope").join("x.db")).is_err());
        let beside = dir.path().join("copy.db");
        let err = backup_to(&db_path, &beside).unwrap_err().to_string();
        assert!(err.contains("beside the live database"), "{err}");
        assert!(!beside.exists());
    }
}
