//! チャットドメインのエラー。

/// チャットの操作エラー。api 層で `ApiError` へ写す（`From` 実装は api 側）。
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// スレッド/メッセージが見つからない。
    #[error("not found")]
    NotFound,
    /// 認可拒否（閲覧/編集/共有権限なし）。
    #[error("forbidden")]
    Forbidden,
    /// 入力不正（空メッセージ・不正な role 等）。
    #[error("invalid request: {0}")]
    Invalid(String),
    /// LLM プロバイダ等の一時障害（再試行可能・503 相当）。
    #[error("service unavailable: {0}")]
    Unavailable(String),
    /// プロバイダの利用枠超過（429）。**この文字列はそのまま会話へ表示される**ので、
    /// Display に内部語の接頭辞を付けない（`llm-gateway` が整形済みの 1 行を渡す）。
    #[error("{0}")]
    RateLimited(String),
    /// 内部エラー（DB/authz/シリアライズ等）。
    #[error("internal error: {0}")]
    Internal(String),
}

/// 生成 run の失敗をユーザーへ出すときの定型文（利用上限以外）。
///
/// 上流のエラー文（プロバイダの生 JSON・URL・`agent:` 等の内部語）は**会話へ出さない**。
/// 全文はサーバ側（tracing・`generation_run.last_error`）にだけ残す。
pub mod failure_message {
    /// 上流がタイムアウトした。
    pub const TIMEOUT: &str =
        "AI の応答がタイムアウトしました。しばらくしてからもう一度お試しください。";
    /// LLM プロバイダに接続できない／一時障害。
    pub const PROVIDER_UNAVAILABLE: &str =
        "AI モデルに接続できませんでした。しばらくしてからもう一度お試しください。";
    /// 生成がリース失効で中断された（孤児回収）。
    pub const INTERRUPTED: &str = "応答の生成が中断されました。もう一度お試しください。";
    /// 上記に当てはまらない失敗。
    pub const GENERIC: &str = "応答の生成に失敗しました。もう一度お試しください。\
                               問題が続く場合は管理者にお問い合わせください。";
}

impl ChatError {
    /// 生成 run の失敗としてユーザーへ出す 1 行を返す（**上流の詳細を含めない**）。
    ///
    /// 利用上限（[`Self::RateLimited`]）だけは llm-gateway が整形済みの文言をそのまま使う
    /// （「待てば直る」ことと再開目安を伝えるため）。それ以外は定型文へ落とす。分類は
    /// 文字列の手掛かりによる安価なもので、外れても定型文になるだけで詳細は漏れない。
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::RateLimited(msg) => msg.clone(),
            Self::Unavailable(detail) => {
                let d = detail.to_ascii_lowercase();
                if d.contains("timed out") || d.contains("timeout") {
                    failure_message::TIMEOUT.to_string()
                } else if d.contains("llm provider unavailable") {
                    failure_message::PROVIDER_UNAVAILABLE.to_string()
                } else {
                    failure_message::GENERIC.to_string()
                }
            }
            Self::NotFound | Self::Forbidden | Self::Invalid(_) | Self::Internal(_) => {
                failure_message::GENERIC.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実測で会話に出ていた形（プロバイダの生 JSON と URL を含む）。
    const RAW: &str = "agent: llm error: llm provider unavailable: openai 502 Bad Gateway: \
        {\"error\":{\"message\":\"upstream connect error\",\"workspace\":\"wrk_01\"}} \
        https://api.example.com/v1/chat/completions";

    #[test]
    fn provider_failure_maps_to_fixed_message_without_details() {
        let msg = ChatError::Unavailable(RAW.into()).user_message();
        assert_eq!(msg, failure_message::PROVIDER_UNAVAILABLE);
        for leak in ["wrk_01", "https://", "{", "agent:", "llm error", "502"] {
            assert!(!msg.contains(leak), "{leak} が漏れた: {msg}");
        }
    }

    #[test]
    fn timeout_is_categorized() {
        let e = ChatError::Unavailable(
            "agent: llm error: llm provider unavailable: openai request failed: \
             error sending request: operation timed out"
                .into(),
        );
        assert_eq!(e.user_message(), failure_message::TIMEOUT);
        let e =
            ChatError::Unavailable("agent: llm error: llm provider unavailable: Timeout".into());
        assert_eq!(e.user_message(), failure_message::TIMEOUT);
    }

    #[test]
    fn rate_limited_keeps_gateway_formatted_line() {
        let line = "利用上限に達しました（5-hour usage limit reached. Resets in 37min.）";
        assert_eq!(ChatError::RateLimited(line.into()).user_message(), line);
    }

    #[test]
    fn other_failures_fall_back_to_generic() {
        for e in [
            ChatError::Unavailable("agent: llm error: llm bad request: secret detail".into()),
            ChatError::Internal("db: connection refused at 10.0.0.1".into()),
            ChatError::Invalid("x".into()),
            ChatError::NotFound,
            ChatError::Forbidden,
        ] {
            assert_eq!(e.user_message(), failure_message::GENERIC, "{e}");
        }
    }
}
