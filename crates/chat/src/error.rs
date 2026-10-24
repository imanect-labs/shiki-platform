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
    /// LLM プロバイダが要求を受け付けなかった（400・設定の誤り等）。**再試行しても同じ結果**に
    /// なるので、ジョブを再試行せずに 1 回目で失敗を確定する（#877）。
    #[error("llm rejected: {0}")]
    Rejected(String),
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
    /// モデル（プロバイダ）が要求を受け付けなかった。待っても直らないので別のモデルを促す。
    pub const MODEL_REJECTED: &str = "選んだ AI モデルでは応答を生成できませんでした。\
                                      別のモデルを選んでもう一度お試しください。";
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
            Self::Rejected(_) => failure_message::MODEL_REJECTED.to_string(),
            Self::NotFound | Self::Forbidden | Self::Invalid(_) | Self::Internal(_) => {
                failure_message::GENERIC.to_string()
            }
        }
    }

    /// 生成ジョブを再試行してよいか（一時障害か）。
    ///
    /// プロバイダが要求を受け付けなかった（[`Self::Rejected`]）ときは何度送っても同じなので、
    /// 再試行せずに失敗を確定する。再試行の間は「生成中」に見え続けるため（#877）。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::Rejected(_))
    }

    /// agent ループの LLM エラー（`AgentError::Llm` の文字列）を写す。
    ///
    /// agent-core は LLM エラーを [`llm_gateway::LlmError`] の `Display` で文字列にして渡す
    /// （利用枠超過だけは種別を保つ）。要求の拒否（`BadRequest`）と設定の誤り（`Config`）は
    /// 先頭の語で見分け、再試行しない [`Self::Rejected`] にする。それ以外は一時障害として扱う。
    #[must_use]
    pub fn from_agent_llm(msg: &str) -> Self {
        if is_rejection(msg) {
            Self::Rejected(format!("agent: llm error: {msg}"))
        } else {
            Self::Unavailable(format!("agent: llm error: {msg}"))
        }
    }

    /// gateway のエラーを写す（agent を通らない経路）。利用枠超過は種別を保つ。
    #[must_use]
    pub fn from_llm(e: llm_gateway::LlmError) -> Self {
        match e {
            llm_gateway::LlmError::RateLimited(msg) => Self::RateLimited(msg),
            e @ (llm_gateway::LlmError::BadRequest(_) | llm_gateway::LlmError::Config(_)) => {
                Self::Rejected(format!("llm: {e}"))
            }
            other => Self::Unavailable(format!("llm: {other}")),
        }
    }
}

/// `LlmError` の `Display` が要求の拒否・設定の誤りを表すか。
fn is_rejection(msg: &str) -> bool {
    msg.starts_with("llm bad request:") || msg.starts_with("llm config error:")
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

    #[test]
    fn provider_rejection_is_not_retried_and_points_to_another_model() {
        // agent-core は LlmError を Display で文字列にして AgentError::Llm へ入れる。
        for e in [
            llm_gateway::LlmError::BadRequest("responses 400: model is not supported".into()),
            llm_gateway::LlmError::Config("unknown model: x".into()),
        ] {
            let agent = ChatError::from_agent_llm(&e.to_string());
            assert!(matches!(agent, ChatError::Rejected(_)), "{agent:?}");
            assert!(!agent.is_retryable());
            assert_eq!(agent.user_message(), failure_message::MODEL_REJECTED);
            // 上流の詳細は会話へ出さない（サーバ側の Display にだけ残る）。
            assert!(!agent.user_message().contains("400"));
            assert!(agent.to_string().contains("llm error"), "{agent}");

            let direct = ChatError::from_llm(e);
            assert!(matches!(direct, ChatError::Rejected(_)), "{direct:?}");
        }
    }

    #[test]
    fn transient_llm_failures_stay_retryable() {
        let e = llm_gateway::LlmError::Unavailable("openai 502".into());
        let agent = ChatError::from_agent_llm(&e.to_string());
        assert!(matches!(agent, ChatError::Unavailable(_)));
        assert!(agent.is_retryable());
        assert!(ChatError::from_llm(e).is_retryable());
        // 利用枠超過は種別を保つ（文言はそのまま出す）。
        let rl = ChatError::from_llm(llm_gateway::LlmError::RateLimited("上限".into()));
        assert!(matches!(rl, ChatError::RateLimited(ref m) if m == "上限"));
        assert!(ChatError::Internal("db".into()).is_retryable());
    }
}
