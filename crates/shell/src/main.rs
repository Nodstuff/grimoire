// ks-shell: native chrome over the daemon's UI (PROJECT.md §3.2a).
//
// The shell ensures the daemon by any means available: already up (a
// launchd agent on power-user machines — always-on for MCP sessions and
// the 16:00 cut even when this app is quit) or a bundled sidecar spawned
// as a child for install-and-run users (dies on Quit). Closing the window
// keeps the ◈ in the menu bar either way.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const DAEMON_ADDR: &str = "127.0.0.1:7425";
const DAEMON_URL: &str = "http://127.0.0.1:7425/";
const LAUNCHD_LABEL: &str = "ie.null.knowledge-system";

fn daemon_up() -> bool {
    TcpStream::connect_timeout(&DAEMON_ADDR.parse().unwrap(), Duration::from_millis(300)).is_ok()
}

/// A daemon we spawned ourselves (no launchd on this machine) — killed on quit.
static SPAWNED: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn launchd_agent_present(uid: &str) -> bool {
    Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Ensure the daemon by any means available, in order of preference:
/// already up (whoever owns it) → kick the launchd agent (power users) →
/// spawn the bundled ksd as a child (install-and-run users).
fn ensure_daemon() {
    if daemon_up() {
        return;
    }
    let uid = uid();
    if !uid.is_empty() && launchd_agent_present(&uid) {
        let _ = Command::new("launchctl")
            .args(["kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
            .status();
    } else {
        spawn_sidecar();
    }
    for _ in 0..20 {
        if daemon_up() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn spawn_sidecar() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let ksd = dir.join("ksd");
    if !ksd.exists() {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let _ = std::fs::create_dir_all(format!("{home}/.knowledge-system"));
    let log = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{home}/.knowledge-system/ksd.log"))
            .ok()
    };
    let mut cmd = Command::new(&ksd);
    cmd.args(["serve", "--port", "7425"]);
    // gardeners shell out to `claude`; GUI apps get a bare PATH
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env(
        "PATH",
        format!("{path}:{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin"),
    );
    if let Some(f) = log() {
        cmd.stdout(f);
    }
    if let Some(f) = log() {
        cmd.stderr(f);
    }
    if let Ok(child) = cmd.spawn() {
        *SPAWNED.lock().unwrap() = Some(child);
    }
}

fn restart_daemon() {
    if let Some(mut child) = SPAWNED.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let uid = uid();
    if !uid.is_empty() && launchd_agent_present(&uid) {
        let _ = Command::new("launchctl")
            .args(["kickstart", "-k", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
            .status();
    } else {
        spawn_sidecar();
    }
}

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(
        app,
        "main",
        WebviewUrl::External(DAEMON_URL.parse().unwrap()),
    )
    .title("Grimoire")
    .inner_size(1240.0, 860.0)
    .hidden_title(true)
    .title_bar_style(tauri::TitleBarStyle::Overlay)
    .build();
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            ensure_daemon();

            let open = MenuItemBuilder::with_id("open", "Open Grimoire").build(app)?;
            let garden = MenuItemBuilder::with_id("garden", "Run gardeners now").build(app)?;
            let restart = MenuItemBuilder::with_id("restart", "Restart daemon").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&open, &garden, &restart, &quit])
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
                        std::thread::spawn(|| {
                            let _ = Command::new("curl")
                                .args([
                                    "-s",
                                    "-X",
                                    "POST",
                                    &format!("http://{DAEMON_ADDR}/admin/garden"),
                                    "-H",
                                    "Content-Type: application/json",
                                    "-d",
                                    "{}",
                                ])
                                .status();
                        });
                    }
                    "restart" => {
                        std::thread::spawn(restart_daemon);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

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
        .expect("error building knowledge-system shell")
        .run(|_app, event| {
            match event {
                // keep running with zero windows; only the tray Quit exits
                tauri::RunEvent::ExitRequested { api, code, .. } => {
                    if code.is_none() {
                        api.prevent_exit();
                    }
                }
                // a daemon we spawned dies with us; a launchd daemon does not
                tauri::RunEvent::Exit => {
                    if let Some(mut child) = SPAWNED.lock().unwrap().take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
                _ => {}
            }
        });
}
