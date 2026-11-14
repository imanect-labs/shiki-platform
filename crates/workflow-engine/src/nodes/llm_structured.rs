//! llm.invoke の構造化出力（`output_schema`・GAP-13）: 出力の検証・1 回の修復・停止理由による失敗。
//!
//! 方針はエンジンに置く（fake ポートで試せる）。ポートは 1 回の呼び出しで `{ text, model, stop_reason? }` を
//! 返すだけで、順序はここが決める:
//!
//! 1. 呼び出す（`output_schema` は閉じた形）。
//! 2. 上限で切れた（`max_tokens`）なら [`LLM_OUTPUT_TRUNCATED`]、拒否（`refusal`）なら [`LLM_REFUSED`] で
//!    失敗する。どちらも直させない・再試行しない（同じ設定では同じ結果になりやすい）。
//! 3. 本文を読んで形で検証する（[`crate::ir::output_schema::validate_output`]）。
//! 4. 合わなければ 1 回だけ直させる（直す点は 5 件・各 200 字まで。会話は user で終える）。呼ぶ前にレートを確かめる。
//! 5. 直しても合わなければ [`LLM_OUTPUT_INVALID`]（permanent・step の retry で作り直し続けない）。
//!
//! 会計のキー（成功は step のキー、修復は [`REPAIR_CALL_SUFFIX`]、使えなかった呼び出しは `#f{uuid}`）は
//! ポートが付ける（[`super::ports::NodePorts::llm_invoke`]）。

use serde_json::Value;

use crate::ir::output_schema::validate_output;

use super::exec::CapabilityNodeExecutor;
use super::ports::{ExecCtx, LlmInvokeReq, LlmRepair, PortError};

/// 失敗のコード: 直させても出力が形に合わない（プロバイダがスキーマを受け付けなかったときも）。
pub const LLM_OUTPUT_INVALID: &str = "llm_output_invalid";
/// 失敗のコード: 構造化出力が上限トークン（max_tokens）で途中で切れた。
pub const LLM_OUTPUT_TRUNCATED: &str = "llm_output_truncated";
/// 失敗のコード: モデルが回答を拒否した。
pub const LLM_REFUSED: &str = "llm_refused";
/// 修復の呼び出しの会計キーの接尾辞（step の冪等キーに足す）。
pub const REPAIR_CALL_SUFFIX: &str = "#c1";

/// 直す点として伝える件数の上限。
const MAX_ISSUES: usize = 5;
/// 直す点 1 件の文字数の上限。
const MAX_ISSUE_CHARS: usize = 200;

/// ポートの出力から停止理由を見て、使えない出力なら失敗にする（無ければ自然終了とみなす）。
fn check_stop(out: &Value) -> Result<(), PortError> {
    match out.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => Err(PortError::new(
            LLM_OUTPUT_TRUNCATED,
            "AI の答えが出力トークンの上限（max_tokens）に達して途中で切れました。\
             ノードの max_tokens を増やしてください（思考の分も上限に数えます）。",
            false,
        )),
        Some("refusal") => Err(PortError::new(
            LLM_REFUSED,
            "AI モデルがこの依頼への回答を拒否しました。プロンプトや入力の内容を見直してください。",
            false,
        )),
        _ => Ok(()),
    }
}

fn text_of(out: &Value) -> String {
    out.get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 直す点を件数と長さで切る（文字の境界で切る）。
fn clip_issues(issues: &[String]) -> Vec<String> {
    issues
        .iter()
        .take(MAX_ISSUES)
        .map(|i| {
            if i.chars().count() > MAX_ISSUE_CHARS {
                let head: String = i.chars().take(MAX_ISSUE_CHARS).collect();
                format!("{head}…")
            } else {
                i.clone()
            }
        })
        .collect()
}

/// 修復の依頼の文。
fn repair_instruction(issues: &[String]) -> String {
    let list = clip_issues(issues)
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "直前の出力は、求めた出力の形（JSON Schema）に次の点で合いません。\n{list}\n\
         形に合う JSON オブジェクトを 1 つだけ、前後に説明やコードフェンス（```）を付けずに出し直してください。"
    )
}

/// 直しても形に合わなかったときの失敗。
fn invalid(issues: &[String]) -> PortError {
    PortError::new(
        LLM_OUTPUT_INVALID,
        format!(
            "AI の出力が、ノードで決めた出力の形（output_schema）に合いませんでした（1 回直させても合いませんでした）。\
             プロンプトか出力の形を見直してください: {}",
            clip_issues(issues).join(" / ")
        ),
        false,
    )
}

/// node の出力（`{ text, model, data }`・停止理由は載せない）。
fn structured_output(out: &Value, data: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("text".into(), Value::String(text_of(out)));
    map.insert(
        "model".into(),
        out.get("model").cloned().unwrap_or(Value::Null),
    );
    map.insert("data".into(), data);
    Value::Object(map)
}

impl CapabilityNodeExecutor {
    /// 構造化出力の llm.invoke（`req.output_schema` は閉じた形）。返すのは出力と、修復したか。
    pub(super) async fn invoke_structured(
        &self,
        ec: &ExecCtx,
        req: LlmInvokeReq,
        schema: &Value,
    ) -> Result<(Value, bool), PortError> {
        let first = self.ports.llm_invoke(ec, req.clone()).await?;
        check_stop(&first)?;
        let text = text_of(&first);
        let issues = match validate_output(schema, &text) {
            Ok(data) => return Ok((structured_output(&first, data), false)),
            Err(issues) => issues,
        };
        // 直しの呼び出しも外部 API の呼び出し（レート制御を通す）。
        self.rate_check(ec, "llm.invoke").await?;
        let second = self
            .ports
            .llm_invoke(
                ec,
                LlmInvokeReq {
                    repair: Some(LlmRepair {
                        previous_output: text,
                        instruction: repair_instruction(&issues),
                    }),
                    call_suffix: Some(REPAIR_CALL_SUFFIX.to_string()),
                    ..req
                },
            )
            .await?;
        check_stop(&second)?;
        match validate_output(schema, &text_of(&second)) {
            Ok(data) => Ok((structured_output(&second, data), true)),
            Err(issues) => Err(invalid(&issues)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stop_reasons_fail_permanently() {
        assert!(check_stop(&json!({ "text": "{}" })).is_ok());
        assert!(check_stop(&json!({ "text": "{}", "stop_reason": "end_turn" })).is_ok());
        let t = check_stop(&json!({ "stop_reason": "max_tokens" })).unwrap_err();
        assert_eq!(t.code, LLM_OUTPUT_TRUNCATED);
        assert!(!t.retryable);
        assert!(t.message.contains("max_tokens"), "{}", t.message);
        let r = check_stop(&json!({ "stop_reason": "refusal" })).unwrap_err();
        assert_eq!(r.code, LLM_REFUSED);
        assert!(!r.retryable);
    }

    /// 直す点は 5 件・各 200 字までに切って伝える（長い出力の誤りで依頼を膨らませない）。
    #[test]
    fn repair_issues_are_clipped() {
        let issues: Vec<String> = (0..8)
            .map(|i| format!("k{i}: {}", "あ".repeat(300)))
            .collect();
        let text = repair_instruction(&issues);
        assert_eq!(text.matches("\n- ").count(), MAX_ISSUES, "{text}");
        assert!(!text.contains("k5:"), "{text}");
        let first = text.lines().nth(1).unwrap();
        assert_eq!(first.chars().count(), 2 + MAX_ISSUE_CHARS + 1, "{first}");
        let e = invalid(&issues);
        assert_eq!(e.code, LLM_OUTPUT_INVALID);
        assert!(!e.retryable);
        assert_eq!(e.message.matches(" / ").count(), MAX_ISSUES - 1);
    }
}
