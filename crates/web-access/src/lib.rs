//! 公開 Web への取得の共通部品（docs/agent/browser.md §2）。
//!
//! - [`public_fetch`] — URL 検証・解決後 IP の検証・検証済みアドレスへの接続固定（PIT-48）。
//!   外部へ HTTP を撃つ経路はここを通す。
//! - [`decode`] — 取得したバイト列の文字コード判定とデコード（#405）。
//!
//! web_fetch（agent-core）・回答画像の代理取得（api）・エージェントブラウザ（crates/browser）が共有する。
//! 関門を複製しないため、どの利用側にも依存しない位置に置く。

pub mod decode;
pub mod public_fetch;
