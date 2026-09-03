//! Registry of the long-running child processes the daemon spawns (`claude
//! -p` for gardeners, the room and ask-the-vault; `d2` renders are sub-second
//! and stay out of it).
//!
//! Each child runs in its OWN process group so a budget overrun kills the
//! whole tree (claude spawns its own helpers), and every live pid is
//! registered here so shutdown — ctrl-c, SIGTERM from the shell, the tray
//! Quit — takes them down instead of orphaning them onto launchd.

use std::collections::HashSet;
use std::sync::Mutex;

static LIVE: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

fn with_set<R>(f: impl FnOnce(&mut HashSet<u32>) -> R) -> R {
    let mut g = LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(g.get_or_insert_with(HashSet::new))
}

/// A registered child pid; unregisters on drop (normal exit, kill, or the
/// awaiting task being cancelled).
pub struct Registered(u32);

impl Registered {
    pub fn pid(&self) -> u32 {
        self.0
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        with_set(|s| {
            s.remove(&self.0);
        });
    }
}

pub fn register(pid: u32) -> Registered {
    with_set(|s| {
        s.insert(pid);
    });
    Registered(pid)
}

pub fn live_count() -> usize {
    with_set(|s| s.len())
}

/// Signal a child's whole process group (the child was spawned with
/// `process_group(0)`, so its pgid is its pid). `sig` is a libc signal.
#[cfg(unix)]
pub fn kill_group(pid: u32, sig: i32) {
    let Ok(pid) = i32::try_from(pid) else { return };
    // SAFETY: kill(2) with a negative pid signals a process group; it has no
    // memory-safety preconditions and simply fails with ESRCH if the group
    // is already gone.
    unsafe {
        libc::kill(-pid, sig);
    }
}

#[cfg(not(unix))]
pub fn kill_group(_pid: u32, _sig: i32) {}

/// Shutdown: SIGTERM every registered group, give them a moment, SIGKILL
/// whatever is still registered. Called from the serve shutdown path before
/// the listener drains, so a hung HTTP connection never delays it.
pub async fn kill_all() {
    let pids: Vec<u32> = with_set(|s| s.iter().copied().collect());
    if pids.is_empty() {
        return;
    }
    tracing::info!(children = pids.len(), "shutdown: stopping child processes");
    #[cfg(unix)]
    for &p in &pids {
        kill_group(p, libc::SIGTERM);
    }
    // the owning tasks reap and unregister as their children exit
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while live_count() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    #[cfg(unix)]
    for p in with_set(|s| s.iter().copied().collect::<Vec<_>>()) {
        kill_group(p, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_is_scoped_to_the_guard() {
        let before = live_count();
        let r = register(999_999);
        assert_eq!(live_count(), before + 1);
        assert_eq!(r.pid(), 999_999);
        drop(r);
        assert_eq!(live_count(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_all_takes_down_a_registered_process_group() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let reg = register(child.id());
        // the process must still be up before we ask for the kill
        assert!(child.try_wait().unwrap().is_none());
        // reap concurrently, the way the owning task would
        let pid = child.id();
        let reaper = std::thread::spawn(move || child.wait());
        kill_all().await;
        let status = reaper.join().unwrap().unwrap();
        assert!(!status.success(), "sleep should have died to a signal");
        drop(reg);
        // the group is gone: a second signal has nobody to hit
        kill_group(pid, libc::SIGTERM);
    }
}
