//! `LlmProvider` の具体アダプタ群。設定で差し替える。

pub mod anthropic;
pub mod openai;
pub mod stub;
mod stub_fixtures;
mod stub_triggers;
