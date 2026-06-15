//! OpenAPI 仕様を標準出力へ書き出す（サーバ起動不要で安定して生成）。
//!
//! 使い方: `cargo run -p shiki-api --bin export-openapi > web/src/generated/openapi.json`

fn main() {
    println!("{}", api::openapi::openapi_json());
}
