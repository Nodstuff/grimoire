// ks-shell: a native window over the daemon's UI (PROJECT.md §3.2a — the
// daemon stays the single owner; this is just chrome). Reserved browser
// shortcuts (⌘T/⌘N) belong to the app here.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running knowledge-system shell");
}
