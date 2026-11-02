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

/// プロバイダが応答の途中でストリームを切ったことを示す [`LlmError::Unavailable`] の接頭辞（#897）。
///
/// gateway はこの印の付いた失敗だけを、まだ何も確定していなければ送り直す。こちらの
/// タイムアウト（全体・無通信）は暴走止めなので送り直さない（接頭辞を付けない）。
pub(crate) const CUT_BY_PROVIDER: &str = "cut by provider: ";

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

/// プロバイダが要求を受け付けなかったことを表す [`LlmError::BadRequest`] の定型文（本文の代わりに入れる）。
pub(crate) const REJECTED_BY_PROVIDER: &str = "プロバイダが要求を受け付けませんでした";

/// 本文から拾う誤りの種類の語の数の上限。
const MAX_ERROR_KINDS: usize = 3;

/// プロバイダの 4xx（429 を除く）を [`LlmError::BadRequest`] にする（構造化出力のスキーマの拒否は
/// [`crate::structured`] が先に定型文にする・BUG-87）。
///
/// プロバイダの本文は**利用者へ出さない**。`message` には要求の ID・組織やワークスペースの ID・URL が入りうり、
/// この文言はアプリのゲートウェイの応答・会話の記録・実行の誤りへそのまま運ばれる（[`rate_limit_message`] と同じ
/// 理由）。本文はログにだけ残し、文言は `{provider} {status}: 定型文（誤りの種類）` にする。誤りの種類は本文の
/// `type`・`code`・`status`・`error`・`detail` にある英数字だけの語（`invalid_request_error` など）で、空白を含む
/// 文や数は拾わない（ChatGPT プランの対象外の判定などが、この語で見分ける）。
pub(crate) fn rejection(provider: &str, status: reqwest::StatusCode, body: &str) -> LlmError {
    let kinds = error_kinds(body);
    tracing::warn!(provider, %status, kinds = ?kinds, body = %body, "プロバイダが要求を受け付けなかった");
    let tail = if kinds.is_empty() {
        String::new()
    } else {
        format!("（{}）", kinds.join(", "))
    };
    LlmError::BadRequest(format!("{provider} {status}: {REJECTED_BY_PROVIDER}{tail}"))
}

/// 本文（JSON）から誤りの種類の語を拾う（上から順に・重ねない・[`MAX_ERROR_KINDS`] 個まで）。
fn error_kinds(body: &str) -> Vec<String> {
    fn is_kind(s: &str) -> bool {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && s.len() <= 64
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            // Anthropic の封筒（`{"type":"error",…}`）は種類ではない。
            && s != "error"
    }
    fn walk(v: &serde_json::Value, depth: usize, out: &mut Vec<String>) {
        let serde_json::Value::Object(map) = v else {
            return;
        };
        for (k, v) in map {
            if out.len() >= MAX_ERROR_KINDS {
                return;
            }
            match v {
                serde_json::Value::String(s)
                    if matches!(k.as_str(), "type" | "code" | "status" | "error" | "detail")
                        && is_kind(s)
                        && !out.iter().any(|o| o == s) =>
                {
                    out.push(s.clone());
                }
                serde_json::Value::Object(_) if depth < 4 => walk(v, depth + 1, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        walk(&v, 0, &mut out);
    }
    out
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

    /// 4xx の本文（要求の ID・組織の ID・URL を含む message）は文言に入れず、誤りの種類の語だけを残す（BUG-87）。
    #[test]
    fn rejection_does_not_expose_the_provider_body() {
        let anthropic = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long for org-abc123: see https://console.example.com/settings"},"request_id":"req_011CUabc"}"#;
        let LlmError::BadRequest(msg) =
            rejection("anthropic", reqwest::StatusCode::BAD_REQUEST, anthropic)
        else {
            panic!("BadRequest になる");
        };
        for leaked in [
            "org-abc123",
            "https://",
            "req_011CUabc",
            "prompt is too long",
            "{",
        ] {
            assert!(!msg.contains(leaked), "{leaked} が漏れた: {msg}");
        }
        assert_eq!(
            msg,
            "anthropic 400 Bad Request: プロバイダが要求を受け付けませんでした（invalid_request_error）"
        );

        let openai = r#"{"error":{"message":"This model's maximum context length is 8192 tokens (org org-xyz)","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#;
        let LlmError::BadRequest(msg) =
            rejection("openai", reqwest::StatusCode::BAD_REQUEST, openai)
        else {
            panic!("BadRequest になる");
        };
        assert!(!msg.contains("org-xyz"), "{msg}");
        assert!(!msg.contains("8192"), "{msg}");
        assert!(msg.contains("context_length_exceeded"), "{msg}");
        assert!(msg.contains("invalid_request_error"), "{msg}");
    }

    /// 本文が JSON でない・種類の語が無いときは定型文だけ。語は空白を含む文や数を拾わない。
    #[test]
    fn rejection_keeps_only_word_like_kinds() {
        let LlmError::BadRequest(msg) = rejection(
            "openai",
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "<html>bad request from 10.0.0.1</html>",
        ) else {
            panic!("BadRequest になる");
        };
        assert_eq!(
            msg,
            "openai 422 Unprocessable Entity: プロバイダが要求を受け付けませんでした"
        );
        let body = r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"x"},"detail":"subscription_sharing_user_not_eligible","type":"has space in it"}"#;
        assert_eq!(
            error_kinds(body),
            ["subscription_sharing_user_not_eligible", "INVALID_ARGUMENT"]
        );
    }
}
