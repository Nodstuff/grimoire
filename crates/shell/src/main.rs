// grimoire-shell: native chrome over the daemon's UI (PROJECT.md §3.2a).
//
// The sidecar model: the daemon (`grimoire`) is bundled inside the .app and
// the shell owns it. On launch the shell attaches to a daemon already
// answering on 127.0.0.1:7425 (another shell instance, or one started by
// hand) or spawns the bundled binary as a child, which dies with the app on
// Quit. No launchd, no install step: download, open, done. Closing the
// window keeps the ◈ in the menu bar; only the tray's Quit exits.
//
// The daemon owns its log (~/.grimoire/ksd.<date>.log, rotated daily) — the
// shell no longer redirects stdout into a second file.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

const DAEMON_ADDR: &str = "127.0.0.1:7425";
const DAEMON_URL: &str = "http://127.0.0.1:7425/";

fn daemon_up() -> bool {
    TcpStream::connect_timeout(&DAEMON_ADDR.parse().unwrap(), Duration::from_millis(300)).is_ok()
}

/// The daemon we spawned — killed on quit. None when we attached to one
/// that was already running.
static SPAWNED: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| ".".into())
}

fn data_dir() -> String {
    format!("{}/.grimoire", home())
}

/// The daemon's newest log file (`ksd.<date>.log`, rotated daily), or the
/// pattern when none exists yet — for error pages and dialogs.
fn log_path() -> String {
    let dir = data_dir();
    let newest = std::fs::read_dir(&dir).ok().and_then(|rd| {
        rd.flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("ksd.") && n.ends_with(".log"))
            .max()
    });
    match newest {
        Some(n) => format!("{dir}/{n}"),
        None => format!("{dir}/ksd.<date>.log"),
    }
}

/// GET /api/stamp on the running daemon and read its `version`. None when
/// nothing answers or the daemon predates 0.7.2 (no version in the stamp).
fn daemon_version() -> Option<String> {
    let mut s = TcpStream::connect_timeout(&DAEMON_ADDR.parse().unwrap(), Duration::from_millis(500)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(b"GET /api/stamp HTTP/1.0\r\nHost: 127.0.0.1:7425\r\nConnection: close\r\n\r\n").ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let body = raw.split("\r\n\r\n").nth(1)?;
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    v.get("version")?.as_str().map(str::to_string)
}

/// "0.7.2" → (0, 7, 2); anything unparseable sorts as (0, 0, 0), i.e. older.
fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// The pid of a `grimoire` daemon listening on our port that is NOT the
/// child we spawned — a leftover from a previous app version, or one started
/// by hand. Anything else on the port is left alone.
#[cfg(unix)]
fn foreign_daemon_pid() -> Option<i32> {
    let out = Command::new("lsof").args(["-ti", "tcp:7425", "-sTCP:LISTEN"]).output().ok()?;
    let ours = SPAWNED.lock().unwrap().as_ref().map(|c| c.id() as i32);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .filter(|pid| Some(*pid) != ours)
        .find(|pid| {
            Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("grimoire"))
                .unwrap_or(false)
        })
}

/// SIGTERM a daemon we did not spawn (it shuts down cleanly and takes its
/// gardener children with it), wait for the port to free, SIGKILL as a
/// last resort. Returns whether the port is free afterwards.
#[cfg(unix)]
fn stop_foreign_daemon() -> bool {
    let Some(pid) = foreign_daemon_pid() else { return !daemon_up() };
    // SAFETY: kill(2) on a pid we just confirmed is a grimoire daemon
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    if wait_port_free(Duration::from_secs(5)) {
        return true;
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    wait_port_free(Duration::from_secs(2))
}

#[cfg(not(unix))]
fn stop_foreign_daemon() -> bool {
    !daemon_up()
}

fn wait_port_free(max: Duration) -> bool {
    let deadline = std::time::Instant::now() + max;
    while std::time::Instant::now() < deadline {
        if !daemon_up() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !daemon_up()
}

fn wait_daemon_up(max: Duration) -> bool {
    let deadline = std::time::Instant::now() + max;
    while std::time::Instant::now() < deadline {
        if daemon_up() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    daemon_up()
}

/// Ensure a daemon at least as new as this app is answering. A daemon is
/// already up → if it is this version (another shell, or a hand-started
/// one) attach to it; if it is OLDER, or too old to say (no `version` in
/// the stamp), it is a leftover from before an update — stop it and spawn
/// the bundled binary. Before 0.7.3 the shell attached to whatever held the
/// port, so an in-app update could leave a 0.7.1 daemon serving a 0.7.2
/// window indefinitely, and "Restart background service" could not replace
/// it because it only knew how to stop its own child.
fn ensure_daemon() -> bool {
    if daemon_up() {
        let mine = parse_version(env!("CARGO_PKG_VERSION"));
        let theirs = daemon_version().map(|v| parse_version(&v));
        match theirs {
            Some(v) if v >= mine => return true,
            _ => {
                if !stop_foreign_daemon() {
                    // could not free the port; better an old daemon than none
                    return true;
                }
            }
        }
    }
    spawn_sidecar();
    wait_daemon_up(Duration::from_secs(5))
}

fn spawn_sidecar() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let ksd = dir.join("grimoire");
    if !ksd.exists() {
        return;
    }
    let home = home();
    let _ = std::fs::create_dir_all(data_dir());
    let mut cmd = Command::new(&ksd);
    cmd.args(["serve", "--port", "7425"]);
    // gardeners shell out to `claude`; GUI apps get a bare PATH
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env(
        "PATH",
        format!("{path}:{home}/.claude/local/bin:{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin"),
    );
    // the daemon exits on its own when this shell is gone (a crash, or the
    // updater swapping the app out from under it), so it can never outlive
    // the app and greet the next version as a stale attach target
    cmd.env("GRIMOIRE_PARENT_PID", std::process::id().to_string());
    // the daemon writes its own rotating log; a GUI child has no terminal
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    if let Ok(child) = cmd.spawn() {
        *SPAWNED.lock().unwrap() = Some(child);
    }
}

/// Stop the sidecar we spawned: SIGTERM so the daemon takes its own
/// children (`claude -p`) down with it, up to 3s for it to exit, then
/// SIGKILL. A bare kill() is SIGKILL and orphans every gardener.
fn stop_sidecar() {
    let Some(mut child) = SPAWNED.lock().unwrap().take() else { return };
    #[cfg(unix)]
    {
        // SAFETY: kill(2) on our own child's pid; no memory preconditions
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Restart whatever daemon is serving the port: our own child, or a foreign
/// one we attached to. The port must actually be free before the spawn, or
/// the new daemon dies on bind and the old one carries on unnoticed.
fn restart_daemon() {
    if SPAWNED.lock().unwrap().is_some() {
        stop_sidecar();
    } else {
        stop_foreign_daemon();
    }
    wait_port_free(Duration::from_secs(5));
    spawn_sidecar();
}

/// The daemon mints a per-boot admin token (0600, `<data_dir>/admin.token`)
/// that gates the /admin surface; the shell hands it to the UI on the URL so
/// the webview — and only the webview — can use it. Absent on older daemons.
fn admin_token() -> Option<String> {
    let raw = std::fs::read_to_string(format!("{}/admin.token", data_dir())).ok()?;
    let tok = raw.trim();
    (!tok.is_empty() && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .then(|| tok.to_string())
}

/// The URL the window loads: the daemon UI, with the admin token and any
/// extra query params (`join=`) attached.
fn ui_url(extra: &[(&str, &str)]) -> String {
    let mut params: Vec<String> = Vec::new();
    if let Some(tok) = admin_token() {
        params.push(format!("admin_token={tok}"));
    }
    for (k, v) in extra {
        params.push(format!("{k}={v}"));
    }
    if params.is_empty() {
        DAEMON_URL.to_string()
    } else {
        format!("{DAEMON_URL}?{}", params.join("&"))
    }
}

/// What the window shows when the daemon is not answering: no white screen.
/// Self-contained (data: URL); the button probes the daemon from the page
/// and the shell keeps retrying `ensure_daemon` behind it.
fn error_page() -> String {
    let log_path = log_path();
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><title>Grimoire</title>
<style>
:root{{color-scheme:light dark}}
body{{margin:0;min-height:100vh;display:grid;place-items:center;font:14px -apple-system,system-ui,sans-serif;background:#111;color:#ddd}}
main{{max-width:460px;padding:32px;text-align:center}}
h1{{font-size:16px;margin:0 0 12px;font-weight:600}}
p{{margin:8px 0;color:#aaa;line-height:1.5}}
code{{font:12px ui-monospace,Menlo,monospace;color:#ddd;background:#222;padding:2px 6px;border-radius:6px;word-break:break-all}}
button{{margin-top:18px;padding:8px 18px;border-radius:10px;border:1px solid #444;background:#1c1c1c;color:#eee;font-size:13px;cursor:pointer}}
button:hover{{background:#262626}}
#s{{min-height:1.4em;margin-top:10px;font-size:12px;color:#c9a35a}}
</style>
<main>
<div style="font-size:36px;opacity:.5;margin-bottom:16px">◈</div>
<h1>Grimoire’s background service did not start</h1>
<p>Your notes are safe. The service that stores and serves them is not answering on port 7425.</p>
<p>Its log is in <code>{log_path}</code></p>
<button onclick="retry()">Try again</button>
<div id="s"></div>
</main>
<script>
const url='{DAEMON_URL}';
const s=document.getElementById('s');
function probe(){{return fetch(url+'api/stamp',{{mode:'no-cors',cache:'no-store'}})}}
function go(){{location.replace('grimoire-shell://ui')}}
function retry(){{
  s.textContent='checking…';
  probe().then(go).catch(()=>{{s.textContent='still not running — quit Grimoire from the ◈ menu and open it again, or check the log'}});
}}
setInterval(()=>probe().then(go).catch(()=>{{}}),3000);
</script>"#
    );
    format!("data:text/html;charset=utf-8,{}", urlencode(&html))
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn navigate(app: &AppHandle, target: &str) {
    if let Some(w) = app.get_webview_window("main") {
        if let Ok(url) = target.parse() {
            let _ = w.navigate(url);
        }
    }
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let target = if daemon_up() { ui_url(&[]) } else { error_page() };
    let _ = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(target.parse().unwrap()))
        .title("Grimoire")
        .inner_size(1240.0, 860.0)
        .hidden_title(true)
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .on_navigation(|url| {
            // the error page asks to be replaced by the UI once the daemon
            // answers; everything else navigates normally
            url.scheme() != "grimoire-shell"
        })
        .build();
    if !daemon_up() {
        // keep trying behind the error page; swap in the UI the moment it answers
        let handle = app.clone();
        std::thread::spawn(move || {
            // a failed connect returns in 300ms; this costs nothing while idle
            loop {
                if ensure_daemon() {
                    navigate(&handle, &ui_url(&[]));
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
    }
}

/// A clicked grimoire://join/… link lands here (#57): route it into the UI
/// as a query param — App.tsx prefills the join box on the sharing screen.
fn handle_deep_link(app: &AppHandle, urls: Vec<tauri::Url>) {
    let Some(link) = urls.first() else { return };
    let link = link.as_str();
    // only the payload travels as the query value — base64url is query-safe
    let Some(payload) = link.strip_prefix("grimoire://join/") else {
        return;
    };
    show_window(app);
    navigate(app, &ui_url(&[("join", payload)]));
}

/// One HTTP/1.1 POST to the daemon without pulling in an HTTP client: the
/// tray's "Run gardeners now". Returns the status code.
fn post_admin(path: &str) -> Result<u16, String> {
    let mut s = TcpStream::connect_timeout(&DAEMON_ADDR.parse().unwrap(), Duration::from_secs(2))
        .map_err(|e| format!("not running ({e})"))?;
    // gardener runs take minutes; wait for the reply so failures surface
    s.set_read_timeout(Some(Duration::from_secs(30 * 60))).ok();
    let token = admin_token().map(|t| format!("X-Grimoire-Admin: {t}\r\n")).unwrap_or_default();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {DAEMON_ADDR}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n{token}Connection: close\r\n\r\n{{}}"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&buf);
    let code = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| "bad reply".to_string())?;
    // the daemon answers 200 with {"error": …} on failure
    if head.contains("\"error\"") {
        let msg = head.split("\"error\"").nth(1).unwrap_or("").chars().take(160).collect::<String>();
        return Err(format!("the daemon reported an error: {msg}"));
    }
    Ok(code)
}

fn run_gardeners_now(app: AppHandle) {
    match post_admin("/admin/garden") {
        Ok(200..=299) => {
            // one line, no dialog: the run happened; results are in the app
            let _ = app
                .notification()
                .builder()
                .title("Gardeners ran")
                .body("Proposals, if any, are in the review queue.")
                .show();
        }
        Ok(code) => app
            .dialog()
            .message(format!("Grimoire refused the request (HTTP {code}). Open the app and check Gardeners."))
            .kind(MessageDialogKind::Warning)
            .title("Gardeners did not run")
            .show(|_| {}),
        Err(e) => app
            .dialog()
            .message(format!("{e}\n\nLog: {}", log_path()))
            .kind(MessageDialogKind::Error)
            .title("Gardeners did not run")
            .show(|_| {}),
    }
}

/// Check the GitHub release feed (`latest.json`, minisign-verified against
/// the pubkey in tauri.conf.json). `interactive` = the user asked from the
/// tray, so "you're up to date" and errors get a dialog; the background check
/// only speaks when there is something to install. Installing replaces the
/// .app and relaunches; the spawned daemon dies with us and the new one
/// starts with the new app (RunEvent::Exit kills the child).
fn check_for_updates(app: AppHandle, interactive: bool) {
    let result = tauri::async_runtime::block_on(async {
        let updater = app.updater().map_err(|e| e.to_string())?;
        updater.check().await.map_err(|e| e.to_string())
    });
    let update = match result {
        Ok(Some(u)) => u,
        Ok(None) => {
            if interactive {
                app.dialog()
                    .message(format!("Grimoire {} is the latest version.", app.package_info().version))
                    .kind(MessageDialogKind::Info)
                    .title("Up to date")
                    .blocking_show();
            }
            return;
        }
        Err(e) => {
            eprintln!("update check failed: {e}");
            if interactive {
                app.dialog()
                    .message(format!("Could not check for updates.\n\n{e}"))
                    .kind(MessageDialogKind::Warning)
                    .title("Update check failed")
                    .blocking_show();
            }
            return;
        }
    };
    let notes = update
        .body
        .as_deref()
        .map(|b| b.trim())
        .filter(|b| !b.is_empty())
        .map(|b| format!("\n\n{}", b.chars().take(600).collect::<String>()))
        .unwrap_or_default();
    let install = app
        .dialog()
        .message(format!(
            "Grimoire {} is available (you have {}).{notes}\n\nInstall and relaunch now? Your notes are untouched.",
            update.version, update.current_version
        ))
        .kind(MessageDialogKind::Info)
        .title("Update available")
        .buttons(MessageDialogButtons::OkCancelCustom("Install".into(), "Later".into()))
        .blocking_show();
    if !install {
        return;
    }
    let res = tauri::async_runtime::block_on(async {
        update
            .download_and_install(|_chunk, _total| {}, || {})
            .await
            .map_err(|e| e.to_string())
    });
    if let Err(e) = res {
        app.dialog()
            .message(format!("The update could not be installed.\n\n{e}\n\nDownload it from github.com/Nodstuff/grimoire/releases instead."))
            .kind(MessageDialogKind::Error)
            .title("Update failed")
            .blocking_show();
        return;
    }
    app.restart()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            ensure_daemon();

            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    handle_deep_link(&handle, event.urls());
                });
            }

            let open = MenuItemBuilder::with_id("open", "Open Grimoire").build(app)?;
            let garden = MenuItemBuilder::with_id("garden", "Run gardeners now").build(app)?;
            let restart = MenuItemBuilder::with_id("restart", "Restart background service").build(app)?;
            let update = MenuItemBuilder::with_id("update", "Check for updates…").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&open, &garden, &restart, &update, &quit])
                .build()?;

            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("main-tray")
                .icon(tray_icon)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => show_window(app),
                    "garden" => {
                        let handle = app.clone();
                        std::thread::spawn(move || run_gardeners_now(handle));
                    }
                    "restart" => {
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            restart_daemon();
                            if ensure_daemon() {
                                navigate(&handle, &ui_url(&[]));
                            } else {
                                navigate(&handle, &error_page());
                            }
                        });
                    }
                    "update" => {
                        let handle = app.clone();
                        std::thread::spawn(move || check_for_updates(handle, true));
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // quiet update check: a minute after launch, then daily. Only an
            // available update ever produces UI; failures go to stderr.
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(60));
                    check_for_updates(handle.clone(), false);
                    std::thread::sleep(Duration::from_secs(24 * 60 * 60 - 60));
                });
            }

            show_window(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // close-to-tray: the ◈ stays in the menu bar
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error building Grimoire shell")
        .run(|_app, event| {
            match event {
                // keep running with zero windows; only the tray Quit exits
                tauri::RunEvent::ExitRequested { api, code, .. } => {
                    if code.is_none() {
                        api.prevent_exit();
                    }
                }
                // the daemon we spawned dies with us
                tauri::RunEvent::Exit => stop_sidecar(),
                _ => {}
            }
        });
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn versions_compare_numerically_and_unparseable_sorts_oldest() {
        assert!(parse_version("0.7.2") > parse_version("0.7.1"));
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
        assert_eq!(parse_version("v0.7.2"), parse_version("0.7.2"));
        assert_eq!(parse_version("garbage"), (0, 0, 0));
        // a daemon with no version in its stamp (pre-0.7.2) must read as older
        assert!(parse_version("0.7.3") > parse_version(""));
    }
}
