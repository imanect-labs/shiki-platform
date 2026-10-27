//! `LlmProvider` の具体アダプタ群。設定で差し替える。

pub mod anthropic;
/// 識別ヘッダ（User-Agent・会話 ID）の共通処理。
mod identity;
/// chat モデルに決定を真似させる決定器（FEAT-1）。
pub mod llm_decider;
/// llm_decider のプロンプト・出力の形・読み取り（llm_decider.rs から分割）。
mod llm_decider_prompt;
/// llm_decider の確からしさの出し方（llm_decider.rs から分割）。
mod llm_decider_vote;
pub mod openai;
pub mod openai_responses;
pub mod stub;
/// stub の決定器（FEAT-1）。
pub mod stub_decider;
mod stub_deep_research;
mod stub_deep_research_specs;
mod stub_fixtures;
/// stub のストリーム生成ヘルパ（stub.rs から分割）。
mod stub_stream;
/// stub の構造化出力（GAP-13・stub.rs から分割）。
mod stub_structured;
mod stub_triggers;
/// 本文先頭の `<think>…</think>` を思考へ振り分ける（Chat Completions・#584）。
mod think_tags;
/// 最初の応答・無通信・全体のタイムアウト（全アダプタ共通・#554）。
mod timeouts;
/// ツール名の wire 写像（全アダプタ共通・#582）。
mod tool_names;
/// TypeSafe System One 互換の決定器（FEAT-1 の T9・実験的・既定は無効）。
pub mod typesafe;
/// プロバイダの `usage` を中立形（キャッシュ内訳つき）へ写す（#581）。
mod usage;
