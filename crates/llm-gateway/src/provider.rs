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

/// 文言に載せる誤りの種類の語の数の上限。
const MAX_ERROR_KINDS: usize = 3;

/// 本文から誤りの種類の語を探す数の上限（見分ける語を探す範囲。文言に載せるのは [`MAX_ERROR_KINDS`] 個まで）。
const MAX_SCANNED_KINDS: usize = 32;

/// ChatGPT プランの対象外のアカウント（`chatgpt::provider` が文言で見分ける）。
pub(crate) const PLAN_NOT_ELIGIBLE: &str = "subscription_sharing_user_not_eligible";

/// 呼ぶ側が文言で見分ける誤りの種類の語。本文のどこにあっても、ほかの語より先に文言へ載せる（本文の並びと
/// [`MAX_ERROR_KINDS`] で落とさない）。
const DISTINGUISHED_KINDS: &[&str] = &[PLAN_NOT_ELIGIBLE];

/// プロバイダの 4xx（429 を除く）を [`LlmError::BadRequest`] にする（構造化出力のスキーマの拒否は
/// [`crate::structured`] が先に定型文にする・BUG-87）。
///
/// プロバイダの本文は**利用者へ出さない**。`message` には要求の ID・組織やワークスペースの ID・URL が入りうり、
/// この文言はアプリのゲートウェイの応答・会話の記録・実行の誤りへそのまま運ばれる（[`rate_limit_message`] と同じ
/// 理由）。本文はログにだけ残し、文言は `{provider} {status}: 定型文（誤りの種類）` にする。誤りの種類は本文の
/// `type`・`code`・`status`・`error`・`detail` にある英数字だけの語（`invalid_request_error` など）で、空白を含む
/// 文や数、数字を含む識別子（`org-abc123`・`req_011…`）は拾わない。呼ぶ側が見分ける語（[`DISTINGUISHED_KINDS`]・
/// ChatGPT プランの対象外の判定など）は、本文のどこにあっても先に載せる。
pub(crate) fn rejection(provider: &str, status: reqwest::StatusCode, body: &str) -> LlmError {
    let kinds = error_kinds(body);
    tracing::warn!(provider, %status, kinds = ?kinds, body = %body, "プロバイダが要求を受け付けなかった");
    LlmError::BadRequest(format!(
        "{provider} {}: {REJECTED_BY_PROVIDER}{}",
        status_line(status),
        kinds_tail(&kinds)
    ))
}

/// プロバイダが一時的に応答できなかったことを表す [`LlmError::Unavailable`] の定型文（本文の代わりに入れる）。
pub(crate) const UNAVAILABLE_AT_PROVIDER: &str = "プロバイダが一時的に応答できませんでした";

/// プロバイダが流し始めた後に誤りを返したことを表す [`LlmError::Unavailable`] の定型文（本文の代わりに入れる）。
pub(crate) const FAILED_MIDSTREAM: &str = "プロバイダが応答の途中で失敗しました";

/// プロバイダの 5xx（Anthropic の 529 を含む）を [`LlmError::Unavailable`] にする（BUG-210）。
///
/// 本文は [`rejection`] と同じ理由で**利用者へ出さない**（5xx の本文にも要求の ID・組織の ID・URL が入りうり、
/// ミニアプリの AI の能力の応答・ストリームの error イベントへそのまま運ばれる）。本文はログにだけ残し、文言は
/// `{provider} {status}: 定型文（誤りの種類）` にする（誤りの種類の語は [`rejection`] と同じ選び方・`overloaded_error`
/// など）。状態の行は残すので、呼ぶ側の見分け（504 の `Gateway Timeout` をタイムアウトの文言にするなど）は変わらない。
pub(crate) fn unavailable(provider: &str, status: reqwest::StatusCode, body: &str) -> LlmError {
    let kinds = error_kinds(body);
    tracing::warn!(provider, %status, kinds = ?kinds, body = %body, "プロバイダが一時的に応答できなかった");
    LlmError::Unavailable(format!(
        "{provider} {}: {UNAVAILABLE_AT_PROVIDER}{}",
        status_line(status),
        kinds_tail(&kinds)
    ))
}

/// プロバイダが流し始めた後に返した誤りの本文（SSE の error イベント・素の誤りの JSON）を、利用者へ出せる 1 行に
/// する（`{provider}: 定型文（誤りの種類）`。本文はログにだけ残す・[`unavailable`] と同じ理由・BUG-210）。
pub(crate) fn midstream_failure(provider: &str, body: &str) -> String {
    let kinds = error_kinds(body);
    tracing::warn!(provider, kinds = ?kinds, body = %body, "プロバイダが応答の途中で誤りを返した");
    format!("{provider}: {FAILED_MIDSTREAM}{}", kinds_tail(&kinds))
}

/// 状態の行（`503 Service Unavailable`。名前の無い状態（529 など）は数だけ）。
///
/// 標準に名前の無い中継の時間切れ（Cloudflare の 522・524 など）は、時間切れと分かる名前を付ける。本文を文言に
/// 入れないので、呼ぶ側はこの行の「Timeout」でタイムアウトを見分ける（504・408 は標準の名前に入っている）。
fn status_line(status: reqwest::StatusCode) -> String {
    match status
        .canonical_reason()
        .or_else(|| proxy_timeout_reason(status.as_u16()))
    {
        Some(reason) => format!("{} {reason}", status.as_u16()),
        None => status.as_u16().to_string(),
    }
}

/// 標準に名前の無い、中継（CDN・プロキシ）の時間切れの状態の名前。
fn proxy_timeout_reason(code: u16) -> Option<&'static str> {
    match code {
        522 => Some("Connection Timed Out"),
        524 => Some("A Timeout Occurred"),
        598 => Some("Network Read Timeout"),
        _ => None,
    }
}

/// 誤りの種類の語を文言の後ろに付ける形（`（a, b）`。無ければ空）。
fn kinds_tail(kinds: &[String]) -> String {
    if kinds.is_empty() {
        String::new()
    } else {
        format!("（{}）", kinds.join(", "))
    }
}

/// 本文（JSON）から誤りの種類の語を拾う（見分ける語を先に、ほかは上から順に・重ねない・[`MAX_ERROR_KINDS`] 個まで）。
fn error_kinds(body: &str) -> Vec<String> {
    fn is_kind(s: &str) -> bool {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && s.len() <= 64
            && chars.all(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '.' | '-'))
            // Anthropic の封筒（`{"type":"error",…}`）は種類ではない。
            && s != "error"
    }
    fn walk(v: &serde_json::Value, depth: usize, out: &mut Vec<String>) {
        let map = match v {
            serde_json::Value::Object(map) => map,
            // Gemini API は誤りを配列（`[{"error": {…}}]`）で返す。
            serde_json::Value::Array(items) if depth < 4 => {
                for item in items {
                    walk(item, depth + 1, out);
                }
                return;
            }
            _ => return,
        };
        for (k, v) in map {
            if out.len() >= MAX_SCANNED_KINDS {
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
                serde_json::Value::Object(_) | serde_json::Value::Array(_) if depth < 4 => {
                    walk(v, depth + 1, out);
                }
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        walk(&v, 0, &mut found);
    }
    let (mut out, rest): (Vec<String>, Vec<String>) = found
        .into_iter()
        .partition(|k| DISTINGUISHED_KINDS.contains(&k.as_str()));
    out.extend(rest);
    out.truncate(MAX_ERROR_KINDS);
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
        // 数字を含む識別子（組織・要求の ID）は、種類の語の形でも拾わない。
        let body = r#"{"error":{"code":"org-abc123","type":"invalid_request_error","status":"req_011CUabc"}}"#;
        assert_eq!(error_kinds(body), ["invalid_request_error"]);
    }

    /// 本文を文言に入れなくても、中継の時間切れ（524 など）は状態の行で時間切れと分かる。
    #[test]
    fn proxy_timeouts_are_named_in_the_status_line() {
        for code in [524u16, 522, 504, 408] {
            let status = reqwest::StatusCode::from_u16(code).unwrap();
            let LlmError::Unavailable(msg) = unavailable("openai", status, "A timeout occurred")
            else {
                panic!("unavailable");
            };
            let lower = msg.to_ascii_lowercase();
            assert!(
                lower.contains("timeout") || lower.contains("timed out"),
                "{code}: {msg}"
            );
        }
        let status = reqwest::StatusCode::from_u16(529).unwrap();
        let LlmError::Unavailable(msg) = unavailable("anthropic", status, "") else {
            panic!("unavailable");
        };
        assert!(!msg.to_ascii_lowercase().contains("timeout"), "{msg}");
    }

    /// 5xx（529 を含む）の本文（要求の ID・組織の ID・URL）は文言に入れず、誤りの種類の語だけを残す（BUG-210）。
    #[test]
    fn unavailable_does_not_expose_the_provider_body() {
        let overloaded = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded for org-abc123, see https://status.example.com"},"request_id":"req_011CUabc"}"#;
        let status = reqwest::StatusCode::from_u16(529).unwrap();
        let LlmError::Unavailable(msg) = unavailable("anthropic", status, overloaded) else {
            panic!("Unavailable になる");
        };
        for leaked in [
            "org-abc123",
            "https://",
            "req_011CUabc",
            "Overloaded for",
            "{",
        ] {
            assert!(!msg.contains(leaked), "{leaked} が漏れた: {msg}");
        }
        assert_eq!(
            msg,
            "anthropic 529: プロバイダが一時的に応答できませんでした（overloaded_error）"
        );
        // HTML の本文は定型文だけ。状態の行は残す（504 はタイムアウトの文言に見分けられる）。
        let LlmError::Unavailable(msg) = unavailable(
            "openai",
            reqwest::StatusCode::GATEWAY_TIMEOUT,
            "<html>upstream 10.0.0.1 timed out</html>",
        ) else {
            panic!("Unavailable になる");
        };
        assert_eq!(
            msg,
            "openai 504 Gateway Timeout: プロバイダが一時的に応答できませんでした"
        );
    }

    /// 流し始めた後の誤りの本文（Gemini API の配列の誤り・Responses API の error イベント）も同じく本文を出さない。
    #[test]
    fn midstream_failure_keeps_only_kinds() {
        let gemini = r#"[{"error":{"code":500,"message":"Internal error for project 1234567 (req abc)","status":"INTERNAL"}}]"#;
        assert_eq!(
            midstream_failure("openai", gemini),
            "openai: プロバイダが応答の途中で失敗しました（INTERNAL）"
        );
        assert_eq!(
            midstream_failure("responses", "not json"),
            "responses: プロバイダが応答の途中で失敗しました"
        );
    }

    /// 呼ぶ側が見分ける語は、ほかの種類の語が先に 3 つ並んでいても文言に残す（本文の並びと上限で落とさない）。
    #[test]
    fn distinguished_kinds_survive_the_limit() {
        let body = r#"{"code":"a_kind","detail":"b_kind","error":{"code":"c_kind","type":"subscription_sharing_user_not_eligible"}}"#;
        assert_eq!(error_kinds(body), [PLAN_NOT_ELIGIBLE, "a_kind", "b_kind"]);
        let LlmError::BadRequest(msg) =
            rejection("responses", reqwest::StatusCode::FORBIDDEN, body)
        else {
            panic!("BadRequest になる");
        };
        assert!(msg.contains(PLAN_NOT_ELIGIBLE), "{msg}");
    }
}
