//! スレッドの記録を、コンテキスト剪定の再生用ステップ列へ写す（#532）。
//!
//! 入力は `GET /threads/{id}/messages` の応答（`Message` / `ContentBlock`）。**認可済み API から
//! エクスポートした記録だけ**を受け、DB を直接読む経路は作らない（アンビエント権限の禁止）。
//!
//! 再生の対象は 1 つの assistant メッセージ（＝1 run）。ステップの区切りは content の並びから
//! 復元する: 本文/ツール呼び出し → そのツール結果 → 次の本文/呼び出し…。それより前のターンは、
//! 本番の `build_history` と同じくテキストだけの履歴にする（前ターンのツール出力は再送されない）。

use agent_core::context_replay::ReplayStep;
use llm_gateway::{Block, Message as LlmMessage, Role as LlmRole};
use uuid::Uuid;

use super::history::message_text;
use crate::model::{ContentBlock, Message, Role};

/// 再生入力（run 開始時の履歴とステップ列）。
#[derive(Debug, Clone, PartialEq)]
pub struct Trajectory {
    /// 対象 run の id（assistant メッセージ id）。
    pub message_id: Uuid,
    pub initial: Vec<LlmMessage>,
    pub steps: Vec<ReplayStep>,
}

/// 変換の失敗。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrajectoryError {
    #[error("assistant メッセージ {0} が見つからない")]
    NotFound(Uuid),
    #[error("ツール呼び出しを含む assistant メッセージが無い")]
    NoAgentRun,
}

/// メッセージ列から再生入力を作る。`message_id` 省略時は、ツール呼び出しを含む最後の assistant
/// メッセージを対象にする。
pub fn from_messages(
    messages: &[Message],
    message_id: Option<Uuid>,
) -> Result<Trajectory, TrajectoryError> {
    let pos = match message_id {
        Some(id) => messages
            .iter()
            .position(|m| m.id == id && m.role == Role::Assistant)
            .ok_or(TrajectoryError::NotFound(id))?,
        None => messages
            .iter()
            .rposition(|m| m.role == Role::Assistant && has_tool_call(&m.content))
            .ok_or(TrajectoryError::NoAgentRun)?,
    };
    let initial = messages[..pos]
        .iter()
        .filter_map(|m| {
            let role = match m.role {
                Role::User => LlmRole::User,
                Role::Assistant => LlmRole::Assistant,
                _ => return None,
            };
            let text = message_text(&m.content);
            (!text.trim().is_empty()).then(|| LlmMessage::text(role, text))
        })
        .collect();
    Ok(Trajectory {
        message_id: messages[pos].id,
        initial,
        steps: steps_of(&messages[pos].content),
    })
}

fn has_tool_call(blocks: &[ContentBlock]) -> bool {
    blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall { .. }))
}

/// content の並びをステップ列に分ける。
///
/// サブエージェントが中継した子の呼び出し（`step` が無い）は親のコンテキストに載らないので除く。
/// ただし `step` 自体が無い旧い記録（全呼び出しが `None`）は、全て親の呼び出しとして扱う。
fn steps_of(blocks: &[ContentBlock]) -> Vec<ReplayStep> {
    let has_steps = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolCall { step: Some(_), .. }));
    let child_ids: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolCall { id, step: None, .. } if has_steps => Some(id.as_str()),
            _ => None,
        })
        .collect();

    let mut steps = Vec::new();
    let mut assistant: Vec<Block> = Vec::new();
    let mut results: Vec<Block> = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => {
                flush(&mut steps, &mut assistant, &mut results);
                assistant.push(Block::Text { text: text.clone() });
            }
            ContentBlock::ToolCall {
                id, name, input, ..
            } if !child_ids.contains(&id.as_str()) => {
                flush(&mut steps, &mut assistant, &mut results);
                assistant.push(Block::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                ok,
            } if !child_ids.contains(&tool_call_id.as_str()) => {
                results.push(Block::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
                    is_error: !ok,
                });
            }
            // 思考は履歴に積まれない（本番ループも assistant には本文と tool_use だけを積む）。
            // 引用・参照カード等は UI 用の投影で、モデルの入力ではない。
            _ => {}
        }
    }
    if !results.is_empty() {
        flush(&mut steps, &mut assistant, &mut results);
    } else if !assistant.is_empty() {
        steps.push(ReplayStep {
            assistant: LlmMessage {
                role: LlmRole::Assistant,
                content: assistant,
            },
            observation: None,
        });
    }
    steps
}

/// 観測まで揃ったステップを確定する（観測がまだ無ければ何もしない＝同じステップの続き）。
fn flush(steps: &mut Vec<ReplayStep>, assistant: &mut Vec<Block>, results: &mut Vec<Block>) {
    if results.is_empty() {
        return;
    }
    steps.push(ReplayStep {
        assistant: LlmMessage {
            role: LlmRole::Assistant,
            content: std::mem::take(assistant),
        },
        observation: Some(LlmMessage {
            role: LlmRole::Tool,
            content: std::mem::take(results),
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: Vec<ContentBlock>) -> Message {
        Message {
            id: Uuid::new_v4(),
            role,
            content,
            agent_mode: true,
            parent_id: None,
            invoked_actions: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }

    fn call(id: &str, step: Option<u32>) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: "web_fetch".into(),
            input: serde_json::json!({"url": id}),
            step,
        }
    }

    fn result(id: &str, ok: bool) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_call_id: id.into(),
            content: format!("body of {id}"),
            ok,
        }
    }

    fn text(t: &str) -> ContentBlock {
        ContentBlock::Text { text: t.into() }
    }

    #[test]
    fn splits_steps_and_keeps_parallel_calls_together() {
        let run = msg(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    text: "考え中".into(),
                },
                text("調べます"),
                call("a", Some(0)),
                call("b", Some(0)),
                result("a", true),
                result("b", false),
                call("c", Some(1)),
                result("c", true),
                text("まとめ"),
            ],
        );
        let thread = vec![
            msg(Role::User, vec![text("前の質問")]),
            msg(Role::Assistant, vec![text("前の回答")]),
            msg(Role::User, vec![text("調べて")]),
            run.clone(),
        ];
        let t = from_messages(&thread, None).unwrap();
        assert_eq!(t.message_id, run.id);
        assert_eq!(t.initial.len(), 3);
        assert_eq!(t.steps.len(), 3);
        // 1 ステップ目: 本文＋並列 2 呼び出し → 結果 2 件（失敗は is_error）。
        assert_eq!(t.steps[0].assistant.content.len(), 3);
        let obs = t.steps[0].observation.as_ref().map(|m| m.content.clone());
        assert!(matches!(
            obs.as_deref(),
            Some([
                Block::ToolResult {
                    is_error: false,
                    ..
                },
                Block::ToolResult { is_error: true, .. }
            ])
        ));
        // 最終ステップは観測なし。
        assert!(t.steps[2].observation.is_none());
        assert_eq!(
            t.steps[2].assistant.content,
            vec![Block::Text {
                text: "まとめ".into()
            }]
        );
    }

    #[test]
    fn drops_subagent_relayed_calls() {
        let run = msg(
            Role::Assistant,
            vec![
                call("parent", Some(0)),
                call("child", None), // 子の中継（step なし）
                result("child", true),
                result("parent", true),
            ],
        );
        let t = from_messages(&[run], None).unwrap();
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].assistant.content.len(), 1);
        assert_eq!(
            t.steps[0].observation.as_ref().map(|m| m.content.len()),
            Some(1)
        );
    }

    #[test]
    fn legacy_rows_without_steps_keep_every_call() {
        let run = msg(
            Role::Assistant,
            vec![
                call("a", None),
                result("a", true),
                call("b", None),
                result("b", true),
            ],
        );
        let t = from_messages(&[run], None).unwrap();
        assert_eq!(t.steps.len(), 2);
    }

    #[test]
    fn picks_requested_message_or_reports_missing() {
        let first = msg(Role::Assistant, vec![call("a", Some(0)), result("a", true)]);
        let last = msg(Role::Assistant, vec![call("b", Some(0)), result("b", true)]);
        let thread = vec![first.clone(), last];
        let t = from_messages(&thread, Some(first.id)).unwrap();
        assert_eq!(t.message_id, first.id);
        assert!(t.initial.is_empty());

        let missing = Uuid::new_v4();
        assert_eq!(
            from_messages(&thread, Some(missing)),
            Err(TrajectoryError::NotFound(missing))
        );
        let chat_only = vec![msg(Role::Assistant, vec![text("hi")])];
        assert_eq!(
            from_messages(&chat_only, None),
            Err(TrajectoryError::NoAgentRun)
        );
    }
}
