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
    /// プロバイダのレート制限／利用枠超過（429）。
    ///
    /// 4xx だが**呼び出し側の組み立ては正しい**ので [`Self::BadRequest`] とは分ける。
    /// 直せば通るものではなく、待つしかない。文言はそのままユーザーへ出せるものにする
    /// （プロバイダの生 JSON を会話に出さない）。
    #[error("llm rate limited: {0}")]
    RateLimited(String),
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

/// 429 の本文から**ユーザーに出せる 1 行**を作る。
///
/// プロバイダの本文（`{"error":{"message":"…"}}` など）は**利用者へ一切出さない**。
/// `message` にも URL・組織/ワークスペース ID・請求情報が入り得るため、中身を選り分けて
/// 通すより定型文に固定するほうが漏れない。原因の追跡用に本文はログへだけ残す。
pub(crate) fn rate_limit_message(body: &str) -> String {
    tracing::warn!(body = %body, "プロバイダが 429 を返した");
    "利用上限に達しました。しばらく待ってからもう一度お試しください。".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_message_does_not_expose_provider_message() {
        let body = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limit reached for org-abc123. See https://example.com/billing"},"metadata":{"workspace":"wrk_01"}}"#;
        let msg = rate_limit_message(body);
        // message の中身（組織 ID・URL）もワークスペース ID も利用者へ出さない。
        for leaked in ["org-abc123", "https://", "wrk_01", "Rate limit reached"] {
            assert!(!msg.contains(leaked), "{msg}");
        }
        assert!(msg.starts_with("利用上限に達しました"), "{msg}");
    }

    #[test]
    fn rate_limit_message_falls_back_on_unknown_shape() {
        assert!(!rate_limit_message("<html>429</html>").is_empty());
        assert!(!rate_limit_message(r#"{"foo":1}"#).is_empty());
    }
}
