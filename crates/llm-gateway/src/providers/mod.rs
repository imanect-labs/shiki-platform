//! `LlmProvider` の具体アダプタ群。設定で差し替える。

pub mod anthropic;
/// 識別ヘッダ（User-Agent・会話 ID）の共通処理。
mod identity;
pub mod openai;
pub mod openai_responses;
pub mod stub;
mod stub_deep_research;
mod stub_deep_research_specs;
mod stub_fixtures;
/// stub のストリーム生成ヘルパ（stub.rs から分割）。
mod stub_stream;
mod stub_triggers;
/// 本文先頭の `<think>…</think>` を思考へ振り分ける（Chat Completions・#584）。
mod think_tags;
/// ツール名の wire 写像（全アダプタ共通・#582）。
mod tool_names;
/// プロバイダの `usage` を中立形（キャッシュ内訳つき）へ写す（#581）。
mod usage;
