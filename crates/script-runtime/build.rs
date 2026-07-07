//! tonic-build による proto → Rust コード生成（codegen が正・生成物は OUT_DIR・非コミット）。

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // システムの protoc に依存せず同梱バイナリを使う（再現性）。
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(path) = protoc_bin_vendored::protoc_bin_path() {
            std::env::set_var("PROTOC", path);
        }
    }
    let proto = "proto/script_runtime.proto";
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&[proto], &["proto"])?;
    println!("cargo:rerun-if-changed={proto}");
    Ok(())
}
