//! `LlmProvider` トレイト（プロバイダ差の吸収点）と関連型。
//!
//! チョークポイント（会計・認可・監査・Langfuse）は [`gateway`](crate::gateway) が持ち、
//! ここは純粋にプロバイダの生成/ストリーミングだけを担う。将来 Anthropic / Gemini /
//! 複数 OpenAI 互換を **設定で差し替え**できるよう、gateway はトレイトオブジェクト
//! `Arc<dyn LlmProvider>` を保持する。

use futures::stream::BoxStream;
use serde_json::Value;

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
/// プロバイダの本文は `{"error":{"message":"5-hour usage limit reached. Resets in 37min. …"}}`
/// のような JSON で、そのまま会話に出すと URL やワークスペース ID まで晒したうえで読めない
/// （実測でその状態になっていた）。message だけを取り出し、無ければ定型文にする。
///
/// 形が違っても壊れない（定型文に落ちるだけ）ことを優先する — プロバイダごとに
/// パーサを書き分けると、増やすたびにここが腐る。
pub(crate) fn rate_limit_message(body: &str) -> String {
    const FALLBACK: &str = "利用上限に達しました。しばらく待ってからもう一度お試しください。";
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return FALLBACK.to_string();
    };
    // `error.message`（OpenAI 互換）→ `message`（素朴な実装）の順に見る。
    let msg = v
        .pointer("/error/message")
        .or_else(|| v.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match msg {
        Some(m) => format!("利用上限に達しました（{m}）"),
        None => FALLBACK.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_message_extracts_provider_reason() {
        let body = r#"{"type":"error","error":{"type":"GoUsageLimitError","message":"5-hour usage limit reached. Resets in 37min."},"metadata":{"workspace":"wrk_01"}}"#;
        let msg = rate_limit_message(body);
        assert!(msg.contains("5-hour usage limit reached"), "{msg}");
        // ワークスペース ID や URL を会話へ漏らさない。
        assert!(!msg.contains("wrk_01"), "{msg}");
    }

    #[test]
    fn rate_limit_message_falls_back_on_unknown_shape() {
        assert!(!rate_limit_message("<html>429</html>").is_empty());
        assert!(!rate_limit_message(r#"{"foo":1}"#).is_empty());
    }
}
