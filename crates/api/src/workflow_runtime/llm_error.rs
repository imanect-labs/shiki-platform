//! llm.invoke の失敗を、step の誤り（実行履歴に出る文言）と再試行分類へ写す（`ports.rs` から分割）。
//!
//! gateway の誤りは英語の原文（`llm config error: unknown model: X` など）で、プロバイダの応答や設定の
//! キー名を含むことがある。実行履歴には利用者向けの日本語だけを出し、原文はサーバのログにだけ残す
//! （チャットの `chat::error::failure_message` と同じ扱い・GAP-41）。

use llm_gateway::LlmError;
use workflow_engine::{ExecCtx, PortError};

use super::llm_model;

/// 実行履歴に出す定型文（モデル名を差し込むものは [`llm_port_error`] が組み立てる）。
mod message {
    /// 上流がタイムアウトした。
    pub(super) const TIMEOUT: &str =
        "AI の応答がタイムアウトしました。しばらくしてからもう一度実行してください。";
    /// プロバイダに接続できない／一時障害。
    pub(super) const UNAVAILABLE: &str =
        "AI モデルに接続できませんでした。しばらくしてからもう一度実行してください。";
    /// プロバイダが要求を受け付けなかった（4xx）。設定を変えない限り通らない。
    pub(super) const REJECTED: &str = "AI モデルが要求を受け付けませんでした。\
                                       ノードの設定（モデル・答えの長さの上限など）を見直してください。";
    /// 接続先の設定（API キー・接続先 URL など）の不備。利用者には直せない。
    pub(super) const MISCONFIGURED: &str =
        "AI モデルの接続設定に不備があります。管理者にお問い合わせください。";
    /// 上記に当てはまらない失敗。
    pub(super) const GENERIC: &str =
        "AI モデルの呼び出しに失敗しました。もう一度実行してください。\
                                      問題が続く場合は管理者にお問い合わせください。";
}

/// gateway の失敗を step の誤りへ写す。
///
/// 設定・入力の誤り（未知のモデル・組織が許可していないモデル・プロバイダの 4xx）は、設定を
/// 変えない限り何度やっても通らないので再試行しない。到達障害やレート制限だけを再試行する。
pub(super) fn llm_port_error(ec: &ExecCtx, e: &LlmError) -> PortError {
    // 原文はサーバのログにだけ残す（どの run のどの step かで引けるように）。
    tracing::warn!(
        target: "workflow.llm",
        tenant = %ec.tenant_id,
        run_id = ?ec.run_id,
        step = ?ec.step_path,
        trace_id = ?ec.trace_id,
        error = %e,
        "llm.invoke の呼び出しに失敗"
    );
    map_llm_error(e)
}

/// gateway の失敗を step の誤りへ写す（ログは呼び出し側が残す・判定のポートと共用）。
pub(super) fn map_llm_error(e: &LlmError) -> PortError {
    match e {
        LlmError::Config(detail) => config_error(detail),
        LlmError::BadRequest(_) => PortError::invalid(message::REJECTED),
        // gateway が利用者向けに整形した 1 行（上限の種類と再開の目安）をそのまま出す。
        LlmError::RateLimited(line) => PortError::unavailable(line.clone()),
        LlmError::Unavailable(detail) => {
            let d = detail.to_ascii_lowercase();
            if d.contains("timed out") || d.contains("timeout") {
                PortError::unavailable(message::TIMEOUT)
            } else {
                PortError::unavailable(message::UNAVAILABLE)
            }
        }
        LlmError::Internal(_) => PortError::unavailable(message::GENERIC),
    }
}

/// llm.invoke の失敗を step の誤りへ写す。出力の形（output_schema）を求めた呼び出しで、プロバイダが
/// スキーマを受け付けなかったもの（複雑すぎる・使えない書き方）は、出力の形の誤り（`llm_output_invalid`・
/// 再試行しない・直させない）にする。文言は gateway の定型文（プロバイダの本文は含まない）。
pub(super) fn structured_port_error(ec: &ExecCtx, e: &LlmError, structured: bool) -> PortError {
    if structured && llm_gateway::structured::is_schema_rejection(e) {
        tracing::warn!(
            target: "workflow.llm",
            tenant = %ec.tenant_id,
            run_id = ?ec.run_id,
            step = ?ec.step_path,
            error = %e,
            "llm.invoke の出力の形をプロバイダが受け付けなかった"
        );
        if let LlmError::BadRequest(message) = e {
            return PortError::new(workflow_engine::LLM_OUTPUT_INVALID, message.clone(), false);
        }
    }
    llm_port_error(ec, e)
}

/// agent.invoke のループの失敗を step の誤りへ写す（BUG-6）。
///
/// agent-core は LLM の誤りを `AgentError::Llm(<LlmError の文>)` に潰すので、接頭辞から種別を戻して
/// llm.invoke と同じ利用者向けの文言・再試行の分類にする。利用枠の超過（`RateLimited`）は gateway が
/// 利用者向けに整形した文言をそのまま出し、code は `rate_limited` にしない（attempt を消費しない再試行で
/// 回り続けないように・retry.rs）。
pub(super) fn agent_port_error(ec: &ExecCtx, e: &agent_core::AgentError) -> PortError {
    use agent_core::AgentError as A;
    match e {
        A::Llm(raw) => llm_port_error(ec, &llm_error_from_text(raw)),
        A::RateLimited(line) => llm_port_error(ec, &LlmError::RateLimited(line.clone())),
        A::Cancelled => PortError::new("cancelled", "実行が取り消されました", false),
        A::Sink(detail) => {
            tracing::warn!(target: "workflow.llm", run_id = ?ec.run_id, step = ?ec.step_path, error = %detail, "agent.invoke の記録に失敗");
            PortError::unavailable(message::GENERIC)
        }
    }
}

/// `LlmError` の Display（`llm config error: …` など）から種別を戻す（知らない形は内部の誤り）。
fn llm_error_from_text(raw: &str) -> LlmError {
    let detail = |prefix: &str| raw.strip_prefix(prefix).map(str::to_string);
    if let Some(d) = detail("llm config error: ") {
        LlmError::Config(d)
    } else if let Some(d) = detail("llm provider unavailable: ") {
        LlmError::Unavailable(d)
    } else if let Some(d) = detail("llm rate limited: ") {
        LlmError::RateLimited(d)
    } else if let Some(d) = detail("llm bad request: ") {
        LlmError::BadRequest(d)
    } else {
        LlmError::Internal(
            raw.strip_prefix("llm internal error: ")
                .unwrap_or(raw)
                .to_string(),
        )
    }
}

/// `LlmError::Config` を利用者向けの 1 行の誤りにする（どれも設定を直すまで通らないので再試行しない）。
///
/// モデル名はノードの設定に書いた値なので文言に含める（どの設定を直すかが分かる）。モデルは実行の前に
/// 解決しているので（`llm_model.rs`）、未登録・無効がここに来るのは解決と生成の間に管理者が設定を
/// 変えたときだけで、解決の失敗と同じ code・文言にする。gateway が最初から日本語で書いた文言
/// （ChatGPT プランの本人限定など）は利用者向けなので、そのまま出す。
fn config_error(detail: &str) -> PortError {
    if let Some(model) = detail.strip_prefix("unknown model: ") {
        return llm_model::unknown_model(model);
    }
    if let Some(model) = detail.strip_prefix("model is not enabled for this organization: ") {
        return llm_model::disabled_model(model);
    }
    if let Some(model) = detail.strip_prefix("unknown provider for model: ") {
        return PortError::invalid(format!(
            "AI モデル「{model}」の接続先が設定されていません。管理者にお問い合わせください。"
        ));
    }
    if detail.is_ascii() {
        PortError::invalid(message::MISCONFIGURED)
    } else {
        PortError::invalid(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ec() -> ExecCtx {
        ExecCtx {
            tenant_id: "t1".into(),
            org: "acme".into(),
            principal: "alice".into(),
            principal_kind: "user".into(),
            trace_id: None,
            run_id: None,
            step_path: Some("ai".into()),
        }
    }

    #[test]
    fn llm_config_errors_are_not_retried() {
        // 組織が許可していないモデルは設定を変えるまで通らない → 再試行しない。
        let e = llm_port_error(&ec(), &LlmError::Config("model is not enabled".into()));
        assert!(!e.retryable);
        assert!(!llm_port_error(&ec(), &LlmError::BadRequest("bad".into())).retryable);
        // 到達障害・レート制限は再試行する。
        assert!(llm_port_error(&ec(), &LlmError::Unavailable("down".into())).retryable);
        assert!(llm_port_error(&ec(), &LlmError::RateLimited("429".into())).retryable);
        assert!(llm_port_error(&ec(), &LlmError::Internal("decode".into())).retryable);
    }

    /// モデルの誤りは、どのモデルをどう直すかが分かる日本語になる（原文の英語を出さない）。
    #[test]
    fn model_errors_name_the_model_in_japanese() {
        let unknown = llm_port_error(&ec(), &LlmError::Config("unknown model: gpt-x-mini".into()));
        assert_eq!(
            unknown.message,
            "AI モデル「gpt-x-mini」は登録されていません。\
             ノードの設定でモデルを選び直すか、「組織の既定に従う」にしてください。"
        );
        // 実行前の解決の失敗と同じ code（設定を直すまで通らない）。
        assert_eq!(unknown.code, "model_unavailable");
        assert!(!unknown.retryable);
        let disabled = llm_port_error(
            &ec(),
            &LlmError::Config("model is not enabled for this organization: big".into()),
        );
        assert!(
            disabled
                .message
                .starts_with("AI モデル「big」はこの組織では使えません。"),
            "{}",
            disabled.message
        );
        for e in [&unknown, &disabled] {
            for leak in ["llm", "config", "unknown", "enabled"] {
                assert!(!e.message.contains(leak), "{leak} が漏れた: {}", e.message);
            }
        }
    }

    /// 上流の詳細（プロバイダの応答・URL・設定のキー名）は文言に出さない。
    #[test]
    fn upstream_details_stay_in_the_log() {
        let raw = "openai 400: {\"error\":{\"message\":\"max_tokens too large\"}} https://api.example.com/v1";
        for (e, want) in [
            (LlmError::BadRequest(raw.into()), message::REJECTED),
            (LlmError::Unavailable(raw.into()), message::UNAVAILABLE),
            (
                LlmError::Unavailable("openai request failed: operation timed out".into()),
                message::TIMEOUT,
            ),
            (LlmError::Internal(raw.into()), message::GENERIC),
            (
                LlmError::Config("anthropic api_key required".into()),
                message::MISCONFIGURED,
            ),
        ] {
            let got = llm_port_error(&ec(), &e);
            assert_eq!(got.message, want, "{e}");
            for leak in ["https://", "{", "openai", "api_key", "400"] {
                assert!(
                    !got.message.contains(leak),
                    "{leak} が漏れた: {}",
                    got.message
                );
            }
        }
    }

    /// 出力の形をプロバイダが拒んだ誤りは、出力の形の誤り（再試行しない）として定型文で出す。
    /// 形を求めていない呼び出し・ほかの 4xx は従来どおり。
    #[test]
    fn schema_rejections_become_output_invalid() {
        let rejected = LlmError::BadRequest(
            "出力の形（JSON Schema）が複雑すぎて、モデルが受け付けませんでした。".into(),
        );
        let got = structured_port_error(&ec(), &rejected, true);
        assert_eq!(got.code, "llm_output_invalid");
        assert!(!got.retryable);
        assert!(
            got.message.starts_with("出力の形（JSON Schema）"),
            "{}",
            got.message
        );
        assert_eq!(
            structured_port_error(&ec(), &rejected, false).message,
            message::REJECTED
        );
        let other = LlmError::BadRequest("openai 400 prompt too long".into());
        assert_eq!(
            structured_port_error(&ec(), &other, true).message,
            message::REJECTED
        );
        let down = structured_port_error(&ec(), &LlmError::Unavailable("x".into()), true);
        assert!(down.retryable);
    }

    /// エージェントのループの誤りも llm.invoke と同じ文言・再試行の分類になる（BUG-6）。
    #[test]
    fn agent_errors_follow_the_llm_mapping() {
        use agent_core::AgentError as A;
        let unknown =
            agent_port_error(&ec(), &A::Llm("llm config error: unknown model: m9".into()));
        assert_eq!(unknown.code, "model_unavailable");
        assert!(!unknown.retryable);
        let down = agent_port_error(
            &ec(),
            &A::Llm("llm provider unavailable: openai request failed: operation timed out".into()),
        );
        assert_eq!(down.message, message::TIMEOUT);
        assert!(down.retryable);
        let bad = agent_port_error(&ec(), &A::Llm("llm bad request: openai 400 {..}".into()));
        assert_eq!(bad.message, message::REJECTED);
        // 利用枠の超過は文言をそのまま出し、attempt を消費しない再試行（rate_limited）にしない。
        let limit = agent_port_error(&ec(), &A::RateLimited("利用上限に達しました".into()));
        assert_eq!(limit.message, "利用上限に達しました");
        assert_ne!(limit.code, "rate_limited");
        assert_eq!(agent_port_error(&ec(), &A::Cancelled).code, "cancelled");
        let odd = agent_port_error(&ec(), &A::Llm("something odd".into()));
        assert_eq!(odd.message, message::GENERIC);
    }

    /// gateway が利用者向けに書いた文言（利用上限・本人限定のモデル）はそのまま出す。
    #[test]
    fn user_facing_gateway_lines_pass_through() {
        let line = "利用上限に達しました（5-hour usage limit reached. Resets in 37min.）";
        assert_eq!(
            llm_port_error(&ec(), &LlmError::RateLimited(line.into())).message,
            line
        );
        let owner = "このモデルは ChatGPT プランを連携した本人だけが使えます。";
        let got = llm_port_error(&ec(), &LlmError::Config(owner.into()));
        assert_eq!(got.message, owner);
        assert!(!got.retryable);
    }
}
