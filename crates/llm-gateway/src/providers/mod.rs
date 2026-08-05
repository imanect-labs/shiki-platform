//! `LlmProvider` の具体アダプタ群。設定で差し替える。

pub mod anthropic;
pub mod openai;
mod openai_names;
pub mod stub;
mod stub_deep_research;
mod stub_fixtures;
/// stub のストリーム生成ヘルパ（stub.rs から分割）。
mod stub_stream;
mod stub_triggers;
