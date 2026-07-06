//! shiki-llm-gateway — 自作 in-process LLM ゲートウェイ（単一チョークポイント）。
//!
//! 設計の正本: docs/design.md §4.5、docs/roadmap/phase-3.md Task 3.2、PIT-9（内部正規形）。
//!
//! - **内部正規形＝中立 content-block**（[`model`]・PIT-9 確定形）。text/thinking/tool_use/
//!   tool_result を一級市民に持ち、OpenAI 互換・Anthropic はアダプタ側で相互変換する。
//! - **プロバイダは OpenAI 互換ファースト**（APIキーで動く openai-compat＝vLLM もこれで賄う）。
//!   `LlmProvider` トレイトで Anthropic / Gemini / 複数 openai-compat を後から差し替え可能。
//! - **チョークポイント責務**（[`gateway::LlmGateway`]）: トークン会計（tenant_id+org・冪等キー・
//!   金額クリティカル）・Langfuse 計装（trace_id 起点）・リトライ/タイムアウト。別プロセス化しない。

// #[cfg(test)] は本番のみ厳格化する lint を許容する。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::pedantic
    )
)]

pub mod accounting;
pub mod config;
pub mod gateway;
pub mod langfuse;
pub mod model;
pub mod provider;
pub mod providers;

pub use config::{
    GatewayConfig, LangfuseConfig, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
pub use gateway::{GenerationRecord, LlmGateway};
pub use model::{
    Block, Effort, GenerateRequest, Message, Role, StopReason, StreamDelta, ToolDef, Usage,
};
pub use provider::{DeltaStream, LlmError, LlmProvider};
