//! ループから使うツール呼び出しの回収・修復と、結果への注記（[`crate::call_repair`] の配線）。

use std::collections::HashMap;
use std::fmt::Write as _;

use llm_gateway::{Block, ToolDef};

use crate::agent::PendingCall;
use crate::call_repair::{extract_text_calls, normalize_tool_name, repair_args, ArgRepair};
use crate::event::{AgentError, AgentEvent, EventSink};
use crate::loop_detect::{repeat_note, LoopDetector, RepeatTracker, REPEAT_NOTE_AT};

/// 本文に書かれた呼び出しを回収し、`ordered`（assistant メッセージのブロック）を書き換える。
/// 回収できたら呼び出し列（ToolCall イベントは発火済み）。
pub(crate) async fn recover_text_calls(
    tool_defs: &[ToolDef],
    text: &str,
    step: usize,
    ordered: &mut Vec<Block>,
    sink: &mut dyn EventSink,
) -> Result<Option<Vec<PendingCall>>, AgentError> {
    let known: Vec<&str> = tool_defs.iter().map(|t| t.name.as_str()).collect();
    let Some((rest, found)) = extract_text_calls(text, &known) else {
        return Ok(None);
    };
    ordered.retain(|b| !matches!(b, Block::Text { .. }));
    if !rest.is_empty() {
        ordered.insert(0, Block::Text { text: rest });
    }
    let mut calls = Vec::with_capacity(found.len());
    for (i, c) in found.into_iter().enumerate() {
        let id = format!("text_call_{step}_{i}");
        sink.emit(AgentEvent::ToolCall {
            id: id.clone(),
            name: c.name.clone(),
            input: c.input.clone(),
            step: u32::try_from(step).unwrap_or(u32::MAX),
        })
        .await?;
        ordered.push(Block::ToolUse {
            id: id.clone(),
            name: c.name.clone(),
            input: c.input.clone(),
        });
        calls.push(PendingCall {
            id,
            name: c.name,
            input: c.input,
        });
    }
    Ok(Some(calls))
}

/// 修復の記録（呼び出し ID ごとの注記と、実行しない呼び出しのエラー文）。
#[derive(Default)]
pub(crate) struct Repairs {
    notes: HashMap<String, Vec<String>>,
    invalid: HashMap<String, String>,
}

/// 実行しなかった呼び出し（ID・エラー文）。
pub(crate) struct InvalidCall {
    id: String,
    message: String,
}

impl Repairs {
    /// 読めない引数の呼び出しを外す（外したものは結果をこちらで作る）。
    pub(crate) fn split_invalid(
        &self,
        calls: Vec<PendingCall>,
    ) -> (Vec<PendingCall>, Vec<InvalidCall>) {
        let mut run = Vec::with_capacity(calls.len());
        let mut invalid = Vec::new();
        for c in calls {
            match self.invalid.get(&c.id) {
                Some(m) => invalid.push(InvalidCall {
                    id: c.id,
                    message: m.clone(),
                }),
                None => run.push(c),
            }
        }
        (run, invalid)
    }

    /// 出力上限で切れた呼び出しを、実行せず「切れた・書き直して」と返すものにする。
    pub(crate) fn mark_truncated(
        &mut self,
        ids: &[String],
        calls: &[PendingCall],
        max_tokens: Option<u32>,
    ) {
        for id in ids {
            let name = calls
                .iter()
                .find(|c| c.id == *id)
                .map_or("?", |c| c.name.as_str());
            self.invalid.insert(
                id.clone(),
                crate::agent_observation::truncated_call_message(name, max_tokens),
            );
        }
    }

    /// 読み替えた呼び出しの結果に、何を読み替えたかを添える（同じ崩れを繰り返させない）。
    pub(crate) fn annotate(&self, blocks: &mut [Block]) {
        for b in blocks {
            if let Block::ToolResult {
                tool_use_id,
                content,
                ..
            } = b
            {
                if let Some(notes) = self.notes.get(tool_use_id.as_str()) {
                    let _ = write!(
                        content,
                        "\n［注: 呼び出しを読み替えて実行しました: {}。次からはツール定義どおりに呼んでください。］",
                        notes.join("・")
                    );
                }
            }
        }
    }
}

/// ツール名を寄せ、引数をスキーマに合わせる。`ordered` の tool_use も直した形にする。
pub(crate) fn repair_calls(
    tool_defs: &[ToolDef],
    calls: &mut [PendingCall],
    ordered: &mut [Block],
) -> Repairs {
    let known: Vec<&str> = tool_defs.iter().map(|t| t.name.as_str()).collect();
    let mut repairs = Repairs::default();
    for c in calls.iter_mut() {
        let mut notes = Vec::new();
        if !known.contains(&c.name.as_str()) {
            if let Some(n) = normalize_tool_name(&c.name, &known) {
                notes.push(format!("ツール名 `{}` を `{n}` として", c.name));
                c.name = n;
            }
        }
        if let Some(def) = tool_defs.iter().find(|t| t.name == c.name) {
            match repair_args(std::mem::take(&mut c.input), &def.input_schema) {
                ArgRepair::Ok { input, notes: n } => {
                    c.input = input;
                    notes.extend(n);
                }
                ArgRepair::Invalid(msg) => {
                    c.input = serde_json::json!({});
                    repairs.invalid.insert(c.id.clone(), msg);
                }
            }
        }
        for b in ordered.iter_mut() {
            if let Block::ToolUse { id, name, input } = b {
                if *id == c.id {
                    name.clone_from(&c.name);
                    input.clone_from(&c.input);
                }
            }
        }
        if !notes.is_empty() {
            repairs.notes.insert(c.id.clone(), notes);
        }
    }
    repairs
}

/// 出力上限で切れた応答の、**切れた末尾の呼び出し**の ID。
///
/// 呼び出しは順に書かれるので、切れるのは最後の 1 件。引数がオブジェクトとして読めないもの
/// （生の文字列で届いたもの）と、最後の呼び出しが `{}`（途中で切れたプロバイダの表現）のものを
/// 返す。それより前の呼び出しは書き終えている。
pub(crate) fn truncated_tail(calls: &[PendingCall]) -> Vec<String> {
    let mut cut: Vec<String> = calls
        .iter()
        .filter(|c| !c.input.is_object())
        .map(|c| c.id.clone())
        .collect();
    if let Some(last) = calls.last() {
        if !crate::agent_observation::has_args(&last.input) && !cut.contains(&last.id) {
            cut.push(last.id.clone());
        }
    }
    cut
}

/// 実行しなかった呼び出しのエラー結果を、呼び出し順の位置へ差し込む。ループ検出にも数える。
/// 戻り値は「ループに陥った」か。
pub(crate) async fn merge_invalid(
    blocks: &mut Vec<Block>,
    call_info: &[(String, String, serde_json::Value, bool)],
    invalid: Vec<InvalidCall>,
    detector: &mut LoopDetector,
    sink: &mut dyn EventSink,
) -> Result<bool, AgentError> {
    if invalid.is_empty() {
        return Ok(false);
    }
    let mut looping = false;
    for inv in invalid {
        let pos = call_info
            .iter()
            .position(|c| c.0 == inv.id)
            .unwrap_or(blocks.len());
        let (_, name, input, _) = &call_info[pos.min(call_info.len().saturating_sub(1))];
        looping |= detector.observe(name, input, true);
        sink.emit(AgentEvent::ToolResult {
            tool_call_id: inv.id.clone(),
            ok: false,
            content: inv.message.clone(),
        })
        .await?;
        blocks.insert(
            pos.min(blocks.len()),
            Block::tool_result(inv.id, inv.message, true),
        );
    }
    Ok(looping)
}

/// 同じ呼び出しが同じ結果を返し続けていたら、その結果に注記する。
pub(crate) fn note_repeats(
    blocks: &mut [Block],
    call_info: &[(String, String, serde_json::Value, bool)],
    tracker: &mut RepeatTracker,
) {
    for b in blocks {
        let Block::ToolResult {
            tool_use_id,
            content,
            ..
        } = b
        else {
            continue;
        };
        // 未表示（呼び直せば読める）・畳んだ結果は、同じ本文でも停滞ではない。
        if content.starts_with(crate::result_budget::DEFERRED_MARK) {
            continue;
        }
        let Some((_, name, input, _)) = call_info.iter().find(|c| c.0 == *tool_use_id) else {
            continue;
        };
        let n = tracker.observe(name, input, content);
        if n >= REPEAT_NOTE_AT {
            content.push_str(&repeat_note(n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, input: serde_json::Value) -> PendingCall {
        PendingCall {
            id: id.into(),
            name: "resolve".into(),
            input,
        }
    }

    #[test]
    fn only_the_cut_tail_of_a_truncated_response_is_held_back() {
        // 書き終えた 2 件と、引数の途中で切れた末尾（生の文字列）。
        let calls = vec![
            call("a", json!({"id": "T-1"})),
            call("b", json!({"id": "T-2"})),
            call("c", json!("{\"id\": \"T-3\", \"act")),
        ];
        assert_eq!(truncated_tail(&calls), vec!["c".to_string()]);
        // 末尾が `{}`（プロバイダが切れた引数を空にした）も切れた扱い。
        let calls = vec![call("a", json!({"id": "T-1"})), call("b", json!({}))];
        assert_eq!(truncated_tail(&calls), vec!["b".to_string()]);
        // 全部書き終えていれば何も返さない。
        assert!(truncated_tail(&[call("a", json!({"id": "T-1"}))]).is_empty());
    }

    #[test]
    fn truncated_calls_are_split_out_with_a_rewrite_message() {
        let calls = vec![call("a", json!({"id": "T-1"})), call("b", json!("{\"id"))];
        let mut r = Repairs::default();
        r.mark_truncated(&["b".to_string()], &calls, Some(4096));
        let (run, invalid) = r.split_invalid(calls);
        assert_eq!(run.len(), 1);
        assert_eq!(run[0].id, "a");
        assert_eq!(invalid.len(), 1);
        assert!(invalid[0].message.contains("切れた") && invalid[0].message.contains("4096"));
    }

    #[test]
    fn repaired_names_and_args_are_written_back_to_history() {
        let defs = vec![ToolDef::new(
            "read_file",
            "",
            json!({"type": "object", "properties": {"start_line": {"type": "integer"}}}),
        )];
        let mut calls = vec![PendingCall {
            id: "x".into(),
            name: "functions.read_file".into(),
            input: json!({"start_line": "3"}),
        }];
        let mut ordered = vec![Block::ToolUse {
            id: "x".into(),
            name: "functions.read_file".into(),
            input: json!({"start_line": "3"}),
        }];
        let r = repair_calls(&defs, &mut calls, &mut ordered);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].input, json!({"start_line": 3}));
        assert!(
            matches!(&ordered[0], Block::ToolUse { name, input, .. } if name == "read_file" && *input == json!({"start_line": 3}))
        );
        let mut blocks = vec![Block::tool_result("x", "ok", false)];
        r.annotate(&mut blocks);
        assert!(
            matches!(&blocks[0], Block::ToolResult { content, .. } if content.contains("読み替えて実行"))
        );
    }
}
