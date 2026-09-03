// The embedding model (potion-base-8M, ~30 MB, MIT) is compiled into the
// daemon with rust-embed so the app never fetches anything at runtime. The
// files are not in git; this fetches them once (sha256-pinned) when missing.
fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest.join("models/potion-base-8M");
    let needed = ["model.safetensors", "tokenizer.json", "config.json"];
    if !needed.iter().all(|f| dir.join(f).is_file()) {
        let script = manifest.join("../../scripts/fetch-model.sh");
        let status = std::process::Command::new("zsh").arg(&script).status();
        match status {
            Ok(s) if s.success() => {}
            other => panic!(
                "embedding model missing under {} and fetch failed ({other:?}); run scripts/fetch-model.sh",
                dir.display()
            ),
        }
    }
    // Short git sha for the build stamp fallback: a cross-compiled binary
    // without an embedded ui/dist would otherwise report `build: 0`.
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=GRIMOIRE_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=models/potion-base-8M");
    println!("cargo:rerun-if-changed=../../scripts/fetch-model.sh");
}
