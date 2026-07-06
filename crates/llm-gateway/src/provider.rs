//! `LlmProvider` トレイト（プロバイダ差の吸収点）と関連型。
//!
//! チョークポイント（会計・認可・監査・Langfuse）は [`gateway`](crate::gateway) が持ち、
//! ここは純粋にプロバイダの生成/ストリーミングだけを担う。将来 Anthropic / Gemini /
//! 複数 OpenAI 互換を **設定で差し替え**できるよう、gateway はトレイトオブジェクト
//! `Arc<dyn LlmProvider>` を保持する。

use futures::stream::BoxStream;

use crate::model::{GenerateRequest, StreamDelta};

/// LLM 呼び出しのエラー。
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// 設定不備（API キー欠落・未知プロバイダ等）。
    #[error("llm config error: {0}")]
    Config(String),
    /// プロバイダの一時障害（タイムアウト・5xx・レート制限）。呼び出し側で 503 相当に写す。
    #[error("llm provider unavailable: {0}")]
    Unavailable(String),
    /// リクエスト不正（プロバイダ 4xx）。
    #[error("llm bad request: {0}")]
    BadRequest(String),
    /// ストリーム/デコード等の内部エラー。
    #[error("llm internal error: {0}")]
    Internal(String),
}

/// ストリーミング生成の結果ストリーム。
pub type DeltaStream = BoxStream<'static, Result<StreamDelta, LlmError>>;

/// LLM プロバイダアダプタ。設定で差し替える（vLLM/OpenAI 互換・Anthropic・stub …）。
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// プロバイダ名（会計 `provider` 列・ログ用）。
    fn name(&self) -> &'static str;

    /// リクエストを**ストリーミング**生成する。中立 [`StreamDelta`] 列を返す。
    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError>;
}
