//! ai.decide のポート（`ports.rs` から分割・FEAT-1）: エンジンの質問 → gateway の中立形 →
//! `LlmGateway::decide`（判定のチョークポイント）。
//!
//! 組織の許可（判定用のモデルと、元にする chat モデルの両方）・利用上限・会計はチョークポイントの中で行う
//! （ここでは会計しない。冪等キーは step のキーをそのまま渡し、使えなかった判定はチョークポイントが
//! `{key}#f{uuid}` で記録する）。誤りは利用者向けの日本語へ写し、原文はサーバのログだけに残す。
//!
//! ノードに step 属性 `cache` があれば決定キャッシュを通す（`LlmGateway::decide_cached`・範囲はこの実行の
//! テナント＋組織とワークフロー・T10）。当たったかは返す JSON の `cached` に載せる。

use llm_gateway::{
    decide_error_kind, ChoiceOption, DecideCall, DecideErrorKind, DecideRequest, DecisionCache,
    LlmError, Question, QuestionKind, UsageSource,
};
use serde_json::Value;
use workflow_engine::ir::params::{DecideQuestion, DecideQuestionType};
use workflow_engine::nodes::decide::{
    DECISION_INVALID_RESPONSE, DECISION_INVALID_RESPONSE_MESSAGE,
};
use workflow_engine::{ExecCtx, LlmDecideReq, PortError};

use super::auth::auth_ctx;
use super::llm_error::map_llm_error;
use super::ports::ProdNodePorts;

/// モデルが使えない（未登録・判定用でない・組織で無効）ときの code（llm.invoke と同じ分類）。
const MODEL_UNAVAILABLE: &str = "model_unavailable";

// エンジンの上限（保存時の V1・実行時の材料の大きさ）は gateway の上限を超えない（保存を通った判定が
// チョークポイントで弾かれない）。コンパイル時に確かめる（どちらかを変えて食い違えばビルドが止まる）。
const _: () = {
    use llm_gateway::decision::check as g;
    use workflow_engine::ir::params as p;
    assert!(p::MAX_DECIDE_QUESTIONS <= g::MAX_QUESTIONS);
    assert!(p::MIN_DECIDE_OPTIONS >= g::MIN_OPTIONS);
    assert!(p::MAX_DECIDE_OPTIONS <= g::MAX_OPTIONS);
    assert!(p::MIN_DECIDE_LEVELS >= g::MIN_LEVELS);
    assert!(p::MAX_DECIDE_LEVELS <= g::MAX_LEVELS);
    assert!(p::MAX_DECIDE_STATE_BYTES <= g::MAX_STATE_BYTES);
    assert!(p::MAX_DECIDE_INSTRUCTIONS_CHARS <= g::MAX_INSTRUCTIONS_CHARS);
    assert!(p::MAX_DECIDE_DESCRIPTION_CHARS <= g::MAX_DESCRIPTION_CHARS);
    assert!(p::MAX_DECIDE_LEVEL_CHARS <= g::MAX_LEVEL_CHARS);
    assert!(p::MAX_DECIDE_KEY_CHARS <= g::MAX_KEY_CHARS);
};

/// エンジンの質問（params の型）を gateway の中立形へ写す（種類ごとに網羅する）。
pub(super) fn to_question(q: &DecideQuestion) -> Question {
    let kind = match q.kind {
        DecideQuestionType::Choice => QuestionKind::Choice {
            options: q
                .options
                .iter()
                .flatten()
                // 名前（D-87）は説明とつないで AI への手がかりにする（名前の無い定義は説明だけで、今までと
                // 同じ要求＝プロンプトの版と決定キャッシュのキーを変えない）。
                .map(|o| ChoiceOption {
                    key: o.key.clone(),
                    description: o.ai_hint(),
                })
                .collect(),
        },
        DecideQuestionType::Score => QuestionKind::Score {
            levels: q.levels.clone().unwrap_or_default(),
        },
        DecideQuestionType::Boolean => QuestionKind::Boolean {
            yes: q.yes.clone(),
            no: q.no.clone(),
        },
    };
    Question {
        key: q.key.clone(),
        instructions: q.instructions.clone(),
        kind,
    }
}

/// 判定 1 回（[`workflow_engine::NodePorts::llm_decide`] の本体）。返すのは gateway の `DecideResponse` の JSON。
pub(super) async fn decide(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    req: LlmDecideReq,
) -> Result<Value, PortError> {
    let ctx = auth_ctx(ec)?;
    let dreq = DecideRequest {
        model: req.model.clone(),
        state: req.state,
        questions: req.questions.iter().map(to_question).collect(),
    };
    let call = DecideCall {
        idempotency_key: req.idempotency_key,
        trace_id: ec.trace_id.clone(),
        source: UsageSource::Workflow,
        app_id: None,
    };
    let decided = match req.cache {
        Some(cache) => {
            let scope = DecisionCache {
                workflow_id: cache.workflow_id,
                ttl: std::time::Duration::from_secs(u64::from(cache.ttl_sec)),
            };
            ports
                .gateway
                .decide_cached(&ctx, &dreq, &call, &scope)
                .await
                .map(|d| (d.response, d.cached))
        }
        None => ports
            .gateway
            .decide(&ctx, &dreq, &call)
            .await
            .map(|r| (r, false)),
    };
    match decided {
        Ok((resp, cached)) => {
            let mut out = serde_json::to_value(&resp).map_err(|e| {
                tracing::error!(target: "workflow.decide", run_id = ?ec.run_id, step = ?ec.step_path, error = %e, "判定の結果を直列化できない");
                PortError::unavailable("判定の結果を読めませんでした。もう一度実行してください。")
            })?;
            if let Some(obj) = out.as_object_mut() {
                obj.insert("cached".into(), Value::Bool(cached));
                // 答えた決定器の名前（出力と監査の `provider`・国外処理の TypeSafe か LLM に判定させたものかを
                // 項目で絞れるように）。
                if let Some(name) = ports.gateway.decider_name(&req.model) {
                    obj.insert("provider".into(), Value::from(name));
                }
            }
            Ok(out)
        }
        Err(e) => {
            // 原文はサーバのログにだけ残す（どの run のどの step かで引けるように）。
            tracing::warn!(
                target: "workflow.decide",
                tenant = %ec.tenant_id,
                run_id = ?ec.run_id,
                step = ?ec.step_path,
                trace_id = ?ec.trace_id,
                model = %req.model,
                error = %e,
                "ai.decide の判定に失敗"
            );
            Err(decide_port_error(&req.model, &e))
        }
    }
}

/// 判定の失敗を step の誤りへ写す。
///
/// - 判定の定型文（要求の誤り・拒否・途切れ・形に合わない）は利用者向けの日本語なのでそのまま出し、どれも
///   再試行しない（拒否・途切れ・形の誤りは llm.invoke の構造化出力と同じ code で分類する）。
/// - 決定器の応答が要求と合わない（チョークポイントの突き合わせ・TypeSafe の版の不一致や形の崩れ）は、エンジンの
///   突き合わせと同じ `decision_invalid_response`（再試行しない・送り直しても直らず、試すたびに課金される）。
///   原因はログ用なので出さない。それ以外の内部の誤り（読み取りの失敗など一時的なもの）は従来どおり再試行する。
/// - 設定の誤り（未登録・判定用でない・組織で無効。元の chat モデルが無効なときも）は設定を直すまで通らない。
/// - 到達障害・利用上限は llm.invoke と同じ（再試行・gateway の 1 行）。
pub(super) fn decide_port_error(model: &str, e: &LlmError) -> PortError {
    if decide_error_kind(e) == Some(DecideErrorKind::InvalidResponse) {
        return PortError::new(
            DECISION_INVALID_RESPONSE,
            DECISION_INVALID_RESPONSE_MESSAGE,
            false,
        );
    }
    if let (Some(kind), LlmError::BadRequest(message)) = (decide_error_kind(e), e) {
        let code = match kind {
            DecideErrorKind::InvalidRequest => "invalid",
            DecideErrorKind::Refused => workflow_engine::LLM_REFUSED,
            DecideErrorKind::Truncated => workflow_engine::LLM_OUTPUT_TRUNCATED,
            DecideErrorKind::InvalidOutput => workflow_engine::LLM_OUTPUT_INVALID,
            // 上で扱った（Internal の頭で表す・ここには来ない）。
            DecideErrorKind::InvalidResponse => DECISION_INVALID_RESPONSE,
        };
        return PortError::new(code, message.clone(), false);
    }
    if let LlmError::Config(detail) = e {
        if detail.strip_prefix("unknown model: ") == Some(model) {
            return PortError::new(
                MODEL_UNAVAILABLE,
                format!(
                    "判定用のモデル「{model}」は登録されていません。ノードの設定で判定用のモデルを選び直してください。"
                ),
                false,
            );
        }
        if detail.starts_with("not a decision model: ") {
            return PortError::new(
                MODEL_UNAVAILABLE,
                format!(
                    "「{model}」は判定用のモデルではありません。ノードの設定で判定用のモデルを選び直してください。"
                ),
                false,
            );
        }
        if let Some(disabled) = detail.strip_prefix("model is not enabled for this organization: ")
        {
            let message = if disabled == model {
                format!(
                    "判定用のモデル「{model}」はこの組織では使えません。ノードの設定で選び直すか、管理者に有効にしてもらってください。"
                )
            } else {
                // 元にする chat モデル（decision_via）が無効（判定用のモデルが抜け道にならない・fail-closed）。
                format!(
                    "判定用のモデル「{model}」の元になる AI モデル「{disabled}」が、この組織では使えません。管理者に AI の設定を確かめてもらってください。"
                )
            };
            return PortError::new(MODEL_UNAVAILABLE, message, false);
        }
    }
    map_llm_error(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 質問は種類ごとに中立形へ写り、gateway の要求の検査を通る。
    #[test]
    fn questions_map_to_the_gateway_shape() {
        let qs: Vec<DecideQuestion> = serde_json::from_value(serde_json::json!([
            { "key": "category", "type": "choice", "instructions": "種類",
              "options": [ { "key": "billing", "description": "請求" }, { "key": "bug" } ] },
            { "key": "priority", "type": "score", "instructions": "緊急度", "levels": ["低", "高"] },
            { "key": "angry", "type": "boolean", "instructions": "不満", "yes": "苦情" }
        ]))
        .unwrap();
        let mapped: Vec<Question> = qs.iter().map(to_question).collect();
        assert_eq!(
            mapped[0].kind,
            QuestionKind::Choice {
                options: vec![
                    ChoiceOption {
                        key: "billing".into(),
                        description: "請求".into()
                    },
                    ChoiceOption {
                        key: "bug".into(),
                        description: String::new()
                    },
                ]
            }
        );
        assert_eq!(
            mapped[1].kind,
            QuestionKind::Score {
                levels: vec!["低".into(), "高".into()]
            }
        );
        assert_eq!(
            mapped[2].kind,
            QuestionKind::Boolean {
                yes: Some("苦情".into()),
                no: None
            }
        );
        let req = DecideRequest {
            model: "decide".into(),
            state: serde_json::json!("本文"),
            questions: mapped,
        };
        assert_eq!(llm_gateway::decision::check::check_request(&req), Ok(()));
    }

    /// 選択肢の名前（D-87）は、説明とつないで AI への手がかりにする。名前の無い選択肢は今までと同じ要求になる
    /// （プロンプトの版と決定キャッシュのキーを変えない）。上限いっぱいの名前と説明も gateway の検査を通る。
    #[test]
    fn option_labels_join_the_hint_sent_to_the_decider() {
        let qs: Vec<DecideQuestion> = serde_json::from_value(serde_json::json!([
            { "key": "category", "type": "choice", "instructions": "種類",
              "options": [
                  { "key": "option_1", "label": "請求", "description": "請求書や返金" },
                  { "key": "option_2", "label": "不具合" },
                  { "key": "billing", "description": "請求・返金" },
                  { "key": "x".repeat(10), "label": "y".repeat(98), "description": "z".repeat(400) }
              ] }
        ]))
        .unwrap();
        let mapped: Vec<Question> = qs.iter().map(to_question).collect();
        let QuestionKind::Choice { options } = &mapped[0].kind else {
            panic!("choice");
        };
        let hints: Vec<&str> = options.iter().map(|o| o.description.as_str()).collect();
        assert_eq!(hints[..3], ["請求: 請求書や返金", "不具合", "請求・返金"]);
        let req = DecideRequest {
            model: "decide".into(),
            state: serde_json::json!("本文"),
            questions: mapped,
        };
        assert_eq!(llm_gateway::decision::check::check_request(&req), Ok(()));
    }

    #[test]
    fn errors_are_mapped_to_user_facing_lines() {
        let config = |d: &str| decide_port_error("decide-haiku", &LlmError::Config(d.into()));
        let unknown = config("unknown model: decide-haiku");
        assert_eq!(unknown.code, "model_unavailable");
        assert!(!unknown.retryable);
        assert!(unknown
            .message
            .contains("「decide-haiku」は登録されていません"));
        let kind = config("not a decision model: decide-haiku");
        assert!(
            kind.message.contains("判定用のモデルではありません"),
            "{}",
            kind.message
        );
        let off = config("model is not enabled for this organization: decide-haiku");
        assert!(
            off.message.contains("この組織では使えません"),
            "{}",
            off.message
        );
        // 元にする chat モデルが無効なときは、その名前を出す（判定用のモデルが抜け道にならない）。
        let via = config("model is not enabled for this organization: haiku");
        assert_eq!(via.code, "model_unavailable");
        assert!(
            via.message.contains("元になる AI モデル「haiku」"),
            "{}",
            via.message
        );
        // 判定の定型文はそのまま・再試行しない・構造化出力と同じ code。
        for (msg, want) in [
            (
                "判定の要求が決まりに合いません: 質問は 1〜20 件にしてください",
                "invalid",
            ),
            (
                "判定モデルが判定を拒否しました。判定材料や質問の書き方を見直してください。",
                "llm_refused",
            ),
            (
                "判定モデルの応答が上限の長さで途切れました。質問の数を減らしてください。",
                "llm_output_truncated",
            ),
            (
                "判定モデルの応答が決められた形に合いませんでした（category: 選択肢に無い）",
                "llm_output_invalid",
            ),
        ] {
            let got = decide_port_error("m", &LlmError::BadRequest(msg.into()));
            assert_eq!(got.code, want, "{msg}");
            assert_eq!(got.message, msg);
            assert!(!got.retryable);
        }
        // 決定器の応答が要求と合わない（チョークポイントの突き合わせ・TypeSafe の版の不一致）は再試行しない。
        // 原因（版・キー）は利用者に出さない。
        for raw in [
            "decision provider returned an invalid response: 答えのキーが質問と合わない",
            "decision provider returned an invalid response: typesafe: 設定の版 jev-1.13.0 ではなく jev-1.14.0 が答えた",
        ] {
            let got = decide_port_error("m", &LlmError::Internal(raw.into()));
            assert_eq!(got.code, "decision_invalid_response", "{raw}");
            assert!(!got.retryable, "{raw}");
            assert!(
                !got.message.contains("jev") && !got.message.contains("キー"),
                "{}",
                got.message
            );
        }
        // 頭の無い内部の誤り（読み取りの失敗など一時的なもの）は従来どおり再試行する。
        let internal = decide_port_error("m", &LlmError::Internal("stream read failed".into()));
        assert!(internal.retryable);
        assert_ne!(internal.code, "decision_invalid_response");
        // それ以外は llm.invoke と同じ（到達障害・利用上限は再試行・原文を出さない）。
        let down = decide_port_error("m", &LlmError::Unavailable("anthropic 503 {..}".into()));
        assert!(down.retryable);
        assert!(!down.message.contains("anthropic"));
        let limit = decide_port_error("m", &LlmError::RateLimited("利用上限に達しました".into()));
        assert_eq!(limit.message, "利用上限に達しました");
        let other = decide_port_error("m", &LlmError::BadRequest("anthropic 400 {..}".into()));
        assert!(!other.retryable);
        assert!(!other.message.contains("anthropic"));
    }
}
