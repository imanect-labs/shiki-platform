//! モデルが返したツールの引数（JSON）の読み取り（全アダプタ共通・BUG-225）。
//!
//! 読めない引数を `{}` にして [`StreamDelta::ToolUseStop`](crate::model::StreamDelta::ToolUseStop) を渡さない。
//! `{}` にすると、必須の項目の無いツール（読むだけのもの）は既定の値で動き、副作用のあるツールには「項目が無い」
//! としか伝わらず、ミニアプリの AI の能力は空の引数と壊れた引数を見分けられない。壊れた呼び出しは渡さずに数え、
//! 応答の終わりで次のように扱う（[`refuse`]）。
//!
//! - 応答が出力の上限で止まった（[`StopReason::MaxTokens`]）なら、上限で切れた呼び出しとして渡さないだけにする。
//!   呼ぶ側は打ち切りとして扱う（エージェントはツールを動かさずに止まる）。
//! - それ以外は、プロバイダが切った印（[`CUT_BY_PROVIDER`]）の付いた一時障害にする。送り直すかは gateway が決める
//!   （まだ何も確定していないときだけ）。呼び出しの始まり（`ToolUseStart`）を渡した後なので、ふつうは送り直さず、
//!   その生成の失敗として呼ぶ側（チャットの生成のやり直し・ワークフローの再試行）に任せる。
//!
//! 空の引数は、引数の無い呼び出し（`{}`）として読む（Anthropic は引数の無いツールで差分を送らない）。

use serde_json::{json, Value};

use crate::model::StopReason;
use crate::provider::{LlmError, CUT_BY_PROVIDER};

/// 読めない引数の呼び出しがあった応答の誤りの文（`{provider}: ` の後ろに付く・誤りの種類だけを言う）。
pub(super) const BROKEN_TOOL_INPUT: &str = "モデルがツールに渡す値を読める形で返しませんでした";

/// 引数の JSON を読む。読めなければ `None`（中身はログにも残さない。利用者の入力が入りうるため、長さだけを残す）。
pub(super) fn parse(provider: &str, tool_use_id: &str, raw: &str) -> Option<Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(json!({}));
    }
    match serde_json::from_str(raw) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                provider,
                tool_use_id,
                error = %e,
                len = raw.len(),
                "モデルが返したツールの引数が JSON として読めない（呼び出しを渡さない）"
            );
            None
        }
    }
}

/// 応答の終わりに、読めない引数の呼び出しが `broken` 個あったときの誤り（上限で止まった応答なら誤りにしない）。
pub(super) fn refuse(provider: &str, broken: usize, stop: StopReason) -> Option<LlmError> {
    (broken > 0 && stop != StopReason::MaxTokens)
        .then(|| LlmError::Unavailable(format!("{CUT_BY_PROVIDER}{provider}: {BROKEN_TOOL_INPUT}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arguments_are_a_call_without_arguments() {
        assert_eq!(parse("p", "t", ""), Some(json!({})));
        assert_eq!(parse("p", "t", "  \n"), Some(json!({})));
        assert_eq!(parse("p", "t", r#" {"q": 1} "#), Some(json!({"q": 1})));
        assert_eq!(parse("p", "t", r#"{"q": "#), None);
    }

    #[test]
    fn broken_calls_fail_the_answer_unless_it_stopped_at_the_output_limit() {
        assert!(refuse("p", 0, StopReason::ToolUse).is_none());
        assert!(refuse("p", 1, StopReason::MaxTokens).is_none());
        for stop in [StopReason::ToolUse, StopReason::EndTurn, StopReason::Other] {
            match refuse("p", 1, stop) {
                Some(LlmError::Unavailable(m)) => {
                    assert_eq!(m, format!("{CUT_BY_PROVIDER}p: {BROKEN_TOOL_INPUT}"));
                }
                other => panic!("{stop:?}: {other:?}"),
            }
        }
    }
}
