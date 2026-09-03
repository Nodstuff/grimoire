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
    println!("cargo:rerun-if-changed=models/potion-base-8M");
    println!("cargo:rerun-if-changed=../../scripts/fetch-model.sh");
}
