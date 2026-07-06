//! 決定的スタブプロバイダ（テスト/CI・外部依存なし）。
//!
//! 実 LLM を持たない環境（CI・オフライン）でパイプライン全体（chat run・SSE・会計・
//! agent-core ループ）をエンドツーエンドで検証するための決定的アダプタ。挙動:
//! - 直近 user メッセージ本文を語単位でストリーミングして返す。
//! - リクエストにツールがあり、かつ最初のターン（tool_result がまだ無い）で本文が
//!   `search:` で始まるとき、最初のツールを 1 回だけ呼び出す（agent ループの決定的検証）。

use futures::stream::{self, StreamExt};

use crate::model::{Block, GenerateRequest, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

/// 決定的スタブ。
#[derive(Debug, Default, Clone)]
pub struct StubProvider;

impl StubProvider {
    pub fn new() -> Self {
        StubProvider
    }
}

/// 直近の user メッセージ本文を取り出す。
fn last_user_text(req: &GenerateRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    Block::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// これまでにツール結果があるか（＝2 ターン目以降）。
fn has_tool_result(req: &GenerateRequest) -> bool {
    req.messages.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, Block::ToolResult { .. }))
    })
}

#[async_trait::async_trait]
impl LlmProvider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let user_text = last_user_text(req);
        let prompt_tokens = req
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.split_whitespace().count() as u64),
                _ => None,
            })
            .sum::<u64>();

        // ツール呼び出し分岐（決定的）: 1 ターン目に `search:` で始まればツールを呼ぶ。
        if !req.tools.is_empty() && !has_tool_result(req) && user_text.starts_with("search:") {
            let tool = &req.tools[0];
            let query = user_text.trim_start_matches("search:").trim().to_string();
            let id = "stubtool_1".to_string();
            let name = tool.name.clone();
            let input = serde_json::json!({ "query": query });
            let events = vec![
                Ok(StreamDelta::ToolUseStart {
                    id: id.clone(),
                    name,
                }),
                Ok(StreamDelta::ToolUseStop { id, input }),
                Ok(StreamDelta::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        prompt_tokens,
                        completion_tokens: 0,
                    },
                }),
            ];
            return Ok(stream::iter(events).boxed());
        }

        // 通常応答: 本文を語単位でストリーミング。
        let reply = if user_text.is_empty() {
            "（空の質問です）".to_string()
        } else {
            format!("回答: {user_text}")
        };
        let words: Vec<String> = reply
            .split_inclusive(char::is_whitespace)
            .map(str::to_string)
            .collect();
        let completion_tokens = words.len() as u64;
        let mut events: Vec<Result<StreamDelta, LlmError>> = words
            .into_iter()
            .map(|w| Ok(StreamDelta::TextDelta { text: w }))
            .collect();
        events.push(Ok(StreamDelta::Done {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                prompt_tokens,
                completion_tokens,
            },
        }));
        Ok(stream::iter(events).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message;

    #[tokio::test]
    async fn stub_streams_reply_and_usage() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "hello world")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut text = String::new();
        let mut done = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                StreamDelta::Done { usage, .. } => done = Some(usage),
                _ => {}
            }
        }
        assert!(text.contains("hello world"));
        let usage = done.unwrap();
        assert_eq!(usage.prompt_tokens, 2);
        assert!(usage.completion_tokens > 0);
    }

    #[tokio::test]
    async fn stub_calls_tool_on_search_prefix() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "search: 経費規程")]);
        req.tools.push(crate::model::ToolDef {
            name: "doc_search".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
        });
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        let mut stop = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { name, .. } => tool_name = Some(name),
                StreamDelta::Done { stop_reason, .. } => stop = Some(stop_reason),
                _ => {}
            }
        }
        assert_eq!(tool_name.as_deref(), Some("doc_search"));
        assert_eq!(stop, Some(StopReason::ToolUse));
    }
}
