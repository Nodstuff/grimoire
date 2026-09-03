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
    // Re-run when the sha could have changed. `.git/HEAD` alone is not enough:
    // it only moves on a checkout, so a commit on the SAME branch left the
    // stamp stale. Watch the ref HEAD points at, and packed-refs (a gc packs
    // loose refs away). Resolved through git rather than hard-coded, because
    // in a worktree `.git` is a file and the refs live in the common dir.
    let git = |args: &[&str]| -> Option<String> {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&manifest)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let git_dir = git(&["rev-parse", "--absolute-git-dir"]).map(std::path::PathBuf::from);
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map(std::path::PathBuf::from)
        .or_else(|| git_dir.clone());
    let mut watch: Vec<std::path::PathBuf> = Vec::new();
    if let Some(d) = &git_dir {
        watch.push(d.join("HEAD"));
    }
    if let Some(c) = &common {
        watch.push(c.join("packed-refs"));
        // detached HEAD has no symbolic ref: the sha is then pinned by HEAD itself
        if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch.push(c.join(r));
        }
    }
    for path in watch {
        // a path that does not exist would make cargo re-run this every time
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    println!("cargo:rerun-if-changed=models/potion-base-8M");
    println!("cargo:rerun-if-changed=../../scripts/fetch-model.sh");
}
