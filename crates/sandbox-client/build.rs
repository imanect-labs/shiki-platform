//! tonic-build による proto → Rust コード生成（codegen が正・生成物は OUT_DIR に置きコミットしない）。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // システムの protoc に依存せず、同梱バイナリを使う（再現性）。
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
            std::env::set_var("PROTOC", path);
        }
    }
    let proto = "proto/shiki/sandbox/v1/sandbox.proto";
    // server スタブは feature="server"（orchestrator）のときだけ生成する。
    let build_server = std::env::var_os("CARGO_FEATURE_SERVER").is_some();
    tonic_build::configure()
        .build_client(true)
        .build_server(build_server)
        .compile_protos(&[proto], &["proto"])?;
    println!("cargo:rerun-if-changed={proto}");
    Ok(())
}
