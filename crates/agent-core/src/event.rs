//! エージェントループが外へ流すイベント（[`AgentEvent`]）と受け口（[`EventSink`]）。
//!
//! chat ワーカーが [`EventSink`] を実装し、各イベントを `generation_event` へ append
//! （真実のソース）＋ Redis pub/sub 配信する。ループはツール実行/トークンをこのイベントで
//! 逐次外部化し、chat 側で SSE の [`StreamEventKind`](../../chat) へ写す。

use crate::tool::Citation;

/// エージェントループが外へ流すイベント（プロバイダ非依存・chat 非依存）。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// 本文テキストの差分。
    Text(String),
    /// 思考テキストの差分。
    Thinking(String),
    /// ツール呼び出し（id/name/入力確定）。
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// ツール結果。
    ToolResult {
        tool_call_id: String,
        ok: bool,
        content: String,
    },
    /// 引用（doc_search）。
    Citation(Citation),
}

/// エージェントループのエラー。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// LLM ゲートウェイ側の障害。
    #[error("llm error: {0}")]
    Llm(String),
    /// イベント永続化（sink）側の障害。
    #[error("sink error: {0}")]
    Sink(String),
    /// キャンセル要求で停止した（ユーザー明示停止）。
    #[error("cancelled")]
    Cancelled,
}

/// ループ外へイベントを流す受け口。chat ワーカーが実装する。
///
/// `emit` は append-only 永続化（＋pub/sub）を行うため async。`is_cancelled` はステップ境界と
/// ストリーム読取ループでの協調キャンセル検知に使う（ユーザー明示停止のみ・ページ離脱≠キャンセル）。
#[async_trait::async_trait]
pub trait EventSink: Send {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError>;

    /// キャンセル要求が来ているか（協調キャンセル）。
    fn is_cancelled(&self) -> bool;
}
