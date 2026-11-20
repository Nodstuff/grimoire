// Compile proto/taisce/runtime/v1/runtime.proto with protox (pure Rust, so
// no `protoc` on the build machine) and hand the descriptors to tonic's
// generator. The service trait and the client stub are each generated only
// when their feature is on.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let root = manifest.join("../../proto");
    let file = "taisce/runtime/v1/runtime.proto";
    println!("cargo:rerun-if-changed={}", root.join(file).display());
    let descriptors = protox::compile([file], [&root])?;
    tonic_prost_build::configure()
        .build_server(cfg!(feature = "server"))
        .build_client(cfg!(feature = "client"))
        .build_transport(false)
        .compile_fds(descriptors)?;
    Ok(())
}
