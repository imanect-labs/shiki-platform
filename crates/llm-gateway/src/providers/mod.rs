//! `LlmProvider` の具体アダプタ群。設定で差し替える。

pub mod anthropic;
pub mod openai;
pub mod stub;
mod stub_fixtures;
mod stub_triggers;
/// OpenAI 互換 function 名の写像（openai.rs から分割）。
mod tool_names;
