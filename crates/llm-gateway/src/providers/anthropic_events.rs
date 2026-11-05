//! Anthropic Messages API の SSE の読み取り（anthropic.rs から分割・BUG-214）。
//!
//! 1 行ずつ受け取り、中立形の差分にして返す。終わりの合図は `message_stop` と、`message_delta` の `stop_reason`
//! （OpenAI 互換の `[DONE]` と `finish_reason` にあたる）で、次の 2 つは正常な終わりにしない（OpenAI 互換の
//! `cut_short` と同じ扱い）:
//!
//! - 流し始めた後の `event: error`（`overloaded_error` など）。本文（`message` には要求の ID などが入る）は文言に
//!   入れず、誤りの種類の語だけを載せる（BUG-210 と同じ）。
//! - 終わりの合図が無いまま閉じたストリームと、書きかけの `tool_use`（`content_block_stop` の前）で閉じたストリーム。
//!   途中までの回答やツール呼び出しを完了として渡さない。`stop_reason` の後に `message_stop` だけが欠けたものは、
//!   中身が揃っているので完了にする（`stop_reason` は全部のブロックを閉じた後に来る）。
//!
//! どちらも [`CUT_BY_PROVIDER`] の付いた [`LlmError::Unavailable`] にする。送り直すかは gateway が決める
//! （まだ何も確定していないときだけ・`retry::resume_unsettled`）。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::tool_names::ToolNameMap;
use super::usage::AnthropicUsage;
use crate::model::{StopReason, StreamDelta};
use crate::provider::{LlmError, CUT_BY_PROVIDER};

/// 終わりの合図が来ないまま閉じたストリームの理由（本文の代わりに入れる）。
const ENDED_BEFORE_STOP: &str = "anthropic: プロバイダが応答を最後まで返さずに接続を切りました";

/// 送る差分か誤り。
type Item = Result<StreamDelta, LlmError>;

/// 1 回の応答の SSE を読む状態。
pub(super) struct EventReader {
    names: ToolNameMap,
    /// content block の index → (id, name, 積んだ引数の JSON)。
    tools: BTreeMap<i64, (String, String, String)>,
    usage: AnthropicUsage,
    stop: StopReason,
    /// `message_delta` の `stop_reason` を受け取った（応答の中身は揃った）。
    stop_seen: bool,
    /// `message_stop` か `error` まで読んだ（以後の行は読まない）。
    finished: bool,
}

impl EventReader {
    pub(super) fn new(names: ToolNameMap) -> Self {
        EventReader {
            names,
            tools: BTreeMap::new(),
            usage: AnthropicUsage::default(),
            stop: StopReason::EndTurn,
            stop_seen: false,
            finished: false,
        }
    }

    /// 終わり（`message_stop`・`error`）まで読んだか。
    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    /// SSE の 1 行を読み、送るものを返す（`data:` で始まらない行・読めない JSON は読み捨てる）。
    pub(super) fn line(&mut self, line: &str) -> Vec<Item> {
        if self.finished {
            return Vec::new();
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            return Vec::new();
        };
        let data = data.trim();
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Vec::new();
        };
        match v.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.usage.absorb(u);
                }
                Vec::new()
            }
            Some("content_block_start") => self.block_start(&v),
            Some("content_block_delta") => self.block_delta(&v),
            Some("content_block_stop") => {
                let idx = index_of(&v);
                let Some((id, _name, args)) = self.tools.remove(&idx) else {
                    return Vec::new();
                };
                let input: Value = serde_json::from_str(args.trim()).unwrap_or(json!({}));
                vec![Ok(StreamDelta::ToolUseStop { id, input })]
            }
            Some("message_delta") => {
                if let Some(sr) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = map_stop_reason(Some(sr));
                    self.stop_seen = true;
                }
                if let Some(u) = v.get("usage") {
                    self.usage.absorb(u);
                }
                Vec::new()
            }
            Some("message_stop") => {
                self.finished = true;
                vec![Ok(StreamDelta::Done {
                    stop_reason: self.stop,
                    usage: self.usage.to_usage(),
                })]
            }
            Some("error") => {
                self.finished = true;
                let why = crate::provider::midstream_failure("anthropic", data);
                vec![Err(LlmError::Unavailable(format!(
                    "{CUT_BY_PROVIDER}{why}"
                )))]
            }
            _ => Vec::new(),
        }
    }

    /// 本文が尽きた（接続が閉じた）ときに送るもの。終わりの合図（`message_stop`・`stop_reason`）を読んでいないか、
    /// 書きかけの `tool_use` があれば誤り。
    pub(super) fn end(&self) -> Option<Item> {
        if self.finished {
            return None;
        }
        if self.stop_seen && self.tools.is_empty() {
            return Some(Ok(StreamDelta::Done {
                stop_reason: self.stop,
                usage: self.usage.to_usage(),
            }));
        }
        tracing::warn!(
            open_tool_uses = self.tools.len(),
            stop_seen = self.stop_seen,
            "anthropic のストリームが終わりの合図の前に閉じた"
        );
        Some(Err(LlmError::Unavailable(format!(
            "{CUT_BY_PROVIDER}{ENDED_BEFORE_STOP}"
        ))))
    }

    fn block_start(&mut self, v: &Value) -> Vec<Item> {
        let Some(cb) = v
            .get("content_block")
            .filter(|cb| cb.get("type").and_then(Value::as_str) == Some("tool_use"))
        else {
            return Vec::new();
        };
        let text = |key: &str| {
            cb.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let id = text("id");
        let name = self.names.local(&text("name"));
        self.tools
            .insert(index_of(v), (id.clone(), name.clone(), String::new()));
        vec![Ok(StreamDelta::ToolUseStart { id, name })]
    }

    fn block_delta(&mut self, v: &Value) -> Vec<Item> {
        let Some(delta) = v.get("delta") else {
            return Vec::new();
        };
        let text = |key: &str| delta.get(key).and_then(Value::as_str).map(str::to_string);
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => text("text")
                .map(|text| Ok(StreamDelta::TextDelta { text }))
                .into_iter()
                .collect(),
            Some("thinking_delta") => text("thinking")
                .map(|text| Ok(StreamDelta::ThinkingDelta { text }))
                .into_iter()
                .collect(),
            Some("input_json_delta") => {
                if let (Some(pj), Some(entry)) =
                    (text("partial_json"), self.tools.get_mut(&index_of(v)))
                {
                    entry.2.push_str(&pj);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

fn index_of(v: &Value) -> i64 {
    v.get("index").and_then(Value::as_i64).unwrap_or(0)
}

pub(super) fn map_stop_reason(sr: Option<&str>) -> StopReason {
    match sr {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::Refusal,
        Some("end_turn" | "stop_sequence") => StopReason::EndTurn,
        _ => StopReason::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader() -> EventReader {
        EventReader::new(ToolNameMap::new(std::iter::empty::<&str>()))
    }

    /// 行を順に読み、最後に本文の終わりを読ませた結果（差分は短い名前、誤りは文言）。
    fn read(lines: &[&str]) -> Vec<String> {
        let mut r = reader();
        let mut out: Vec<Item> = lines.iter().flat_map(|l| r.line(l)).collect();
        out.extend(r.end());
        out.into_iter()
            .map(|item| match item {
                Ok(StreamDelta::TextDelta { text }) => format!("text:{text}"),
                Ok(StreamDelta::ThinkingDelta { text }) => format!("think:{text}"),
                Ok(StreamDelta::ToolUseStart { id, .. }) => format!("start:{id}"),
                Ok(StreamDelta::ToolUseStop { id, input }) => format!("stop:{id}:{input}"),
                Ok(StreamDelta::Done { stop_reason, .. }) => format!("done:{stop_reason:?}"),
                Ok(other) => format!("{other:?}"),
                Err(LlmError::Unavailable(m)) => format!("unavailable:{m}"),
                Err(e) => format!("err:{e}"),
            })
            .collect()
    }

    const TEXT: &str = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"途中"}}"#;
    const TOOL_START: &str = r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"doc_search"}}"#;
    const TOOL_ARGS: &str = r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#;
    const STOP: &str = r#"data: {"type":"message_stop"}"#;

    #[test]
    fn message_stop_ends_the_answer() {
        assert_eq!(read(&[TEXT, STOP]), ["text:途中", "done:EndTurn"]);
        // message_stop の後の行は読まない。
        assert_eq!(read(&[STOP, TEXT]), ["done:EndTurn"]);
    }

    /// message_stop の無いまま閉じたストリームは、途中までの回答を完了にせず、送り直しの印の付いた誤りにする。
    #[test]
    fn a_stream_closed_before_message_stop_is_cut_by_the_provider() {
        let out = read(&[TEXT]);
        assert_eq!(out[0], "text:途中");
        assert_eq!(
            out[1],
            format!("unavailable:{CUT_BY_PROVIDER}{ENDED_BEFORE_STOP}")
        );
        assert_eq!(out.len(), 2, "{out:?}");
        // 何も流さずに閉じても同じ。
        assert_eq!(
            read(&[]),
            [format!("unavailable:{CUT_BY_PROVIDER}{ENDED_BEFORE_STOP}")]
        );
    }

    /// stop_reason の後に message_stop だけが欠けたものは完了にする（中身は揃っている・OpenAI 互換の finish_reason と
    /// 同じ）。stop_reason が来ても書きかけの tool_use があれば完了にしない。
    #[test]
    fn a_stop_reason_without_message_stop_is_complete_only_with_every_block_closed() {
        let end_turn = r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#;
        assert_eq!(read(&[TEXT, end_turn]), ["text:途中", "done:EndTurn"]);
        let tool_use = r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#;
        let out = read(&[TOOL_START, TOOL_ARGS, tool_use]);
        assert_eq!(
            out.last().unwrap(),
            &format!("unavailable:{CUT_BY_PROVIDER}{ENDED_BEFORE_STOP}"),
            "{out:?}"
        );
    }

    /// 流し始めた後の error イベントは誤りにする。本文の message（組織の ID・要求の ID）は入れず、種類の語だけ。
    #[test]
    fn an_error_event_is_a_failure_with_only_its_kind() {
        let error = r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded for org-secret"},"request_id":"req_secret_3"}"#;
        let out = read(&["event: error", error, STOP]);
        assert_eq!(
            out,
            [format!(
                "unavailable:{CUT_BY_PROVIDER}anthropic: プロバイダが応答の途中で失敗しました（overloaded_error）"
            )],
            "error の後の行（message_stop）は読まず、end も重ねない"
        );
    }

    /// 書きかけの tool_use（content_block_stop の前）で切れたら、壊れた引数でツールを止めて渡さない。
    #[test]
    fn a_tool_use_cut_midway_is_not_handed_over() {
        let out = read(&[TOOL_START, TOOL_ARGS]);
        assert_eq!(out[0], "start:toolu_1");
        assert_eq!(
            out[1],
            format!("unavailable:{CUT_BY_PROVIDER}{ENDED_BEFORE_STOP}")
        );
        assert!(!out.iter().any(|o| o.starts_with("stop:")), "{out:?}");
        // 最後まで来れば引数を読んで止める。
        let done = r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#;
        let block_stop = r#"data: {"type":"content_block_stop","index":1}"#;
        let delta = r#"data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#;
        assert_eq!(
            read(&[TOOL_START, TOOL_ARGS, done, block_stop, delta, STOP]),
            ["start:toolu_1", r#"stop:toolu_1:{"q":1}"#, "done:ToolUse"]
        );
    }

    #[test]
    fn refusal_stop_reason_is_kept() {
        assert_eq!(map_stop_reason(Some("refusal")), StopReason::Refusal);
        assert_eq!(map_stop_reason(Some("pause_turn")), StopReason::Other);
    }
}
