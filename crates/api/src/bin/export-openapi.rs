//! OpenAPI 仕様を標準出力へ書き出す（サーバ起動不要で安定して生成）。
//!
//! 使い方: `cargo run -p shiki-api --bin export-openapi > web/src/generated/openapi.json`

// CLI バイナリ: 標準出力/標準エラーへの出力は正当な用途のため print 系 lint を許容する。
#![allow(clippy::print_stdout, clippy::print_stderr)]

fn main() {
    println!("{}", api::openapi::openapi_json());
}
