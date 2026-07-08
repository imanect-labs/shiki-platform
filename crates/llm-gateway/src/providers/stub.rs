//! 決定的スタブプロバイダ（テスト/CI・外部依存なし）。
//!
//! 実 LLM を持たない環境（CI・オフライン）でパイプライン全体（chat run・SSE・会計・
//! agent-core ループ）をエンドツーエンドで検証するための決定的アダプタ。挙動:
//! - 直近 user メッセージ本文を語単位でストリーミングして返す。
//! - リクエストにツールがあり、かつ最初のターン（tool_result がまだ無い）で本文が
//!   既知のプレフィックス（`search:` / `python:` / `websearch:` / `webfetch:`）で始まるとき、
//!   対応するツールを 1 回だけ呼び出す（agent ループ・各ツールの決定的検証）。プレフィックス
//!   に対応するツールが提示されていなければ最初のツールへフォールバックする（後方互換）。
//! - 自律プロファイル（Phase 5）検証用の駆動プレフィックス:
//!   - `plan: A, B, C` … 1 ターン目に `plan` メタツールをカンマ区切りのサブタスクで呼ぶ（計画分解の検証）。
//!   - `loop:` … tool_result の有無に関わらず**毎ターン** tools[0] を空入力で呼び続ける
//!     （ループ検出・ステップ/予算上限・長ホライズンの決定的駆動）。

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

/// プレフィックス → (対応ツール名, 入力 JSON のキー)。入力値は本文の残り。
/// 対応ツールが提示に無ければ `None` を返し、呼び出し側が tools[0] にフォールバックする。
fn tool_trigger(user_text: &str) -> Option<(&'static str, &'static str, String)> {
    const MAP: &[(&str, &str, &str)] = &[
        ("search:", "doc_search", "query"),
        ("websearch:", "web_search", "query"),
        ("python:", "code_interpreter", "code"),
        ("webfetch:", "web_fetch", "url"),
    ];
    MAP.iter().find_map(|(prefix, tool, key)| {
        user_text
            .strip_prefix(prefix)
            .map(|rest| (*tool, *key, rest.trim().to_string()))
    })
}

/// 単一ツール呼び出し（ToolUse で停止）のストリームを組む決定的ヘルパ。
fn tool_call_stream(name: String, input: serde_json::Value, prompt_tokens: u64) -> DeltaStream {
    let id = "stubtool_1".to_string();
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
    stream::iter(events).boxed()
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

        // --- 自律駆動 `loop:`: 毎ターン tools[0] を空入力で呼び続ける（ループ/上限の決定的駆動）。 ---
        if !req.tools.is_empty() && user_text.starts_with("loop:") {
            return Ok(tool_call_stream(
                req.tools[0].name.clone(),
                serde_json::json!({}),
                prompt_tokens,
            ));
        }
        // --- 自律駆動 `fswrite:`: 1 ターン目に fs_write を固定名で呼ぶ（ワークスペース書込の e2e）。 ---
        if !has_tool_result(req) {
            if let Some(rest) = user_text.strip_prefix("fswrite:") {
                if let Some(t) = req.tools.iter().find(|t| t.name == "fs_write") {
                    return Ok(tool_call_stream(
                        t.name.clone(),
                        serde_json::json!({ "name": "agent-note.txt", "content": rest.trim() }),
                        prompt_tokens,
                    ));
                }
            }
        }
        // --- 自律駆動 `plan:`: 1 ターン目に plan メタツールをカンマ区切りサブタスクで呼ぶ。 ---
        if !has_tool_result(req) {
            if let Some(rest) = user_text.strip_prefix("plan:") {
                if let Some(plan_tool) = req.tools.iter().find(|t| t.name == "plan") {
                    let subtasks: Vec<serde_json::Value> = rest
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|title| serde_json::json!({ "title": title }))
                        .collect();
                    return Ok(tool_call_stream(
                        plan_tool.name.clone(),
                        serde_json::json!({ "subtasks": subtasks }),
                        prompt_tokens,
                    ));
                }
            }
        }

        // ツール呼び出し分岐（決定的）: 1 ターン目に既知プレフィックスで始まれば対応ツールを呼ぶ。
        if !req.tools.is_empty() && !has_tool_result(req) {
            if let Some((tool_name, key, value)) = tool_trigger(&user_text) {
                // 対応ツールが提示にあればそれを、無ければ tools[0] を呼ぶ（後方互換）。
                let name = req
                    .tools
                    .iter()
                    .find(|t| t.name == tool_name)
                    .map_or_else(|| req.tools[0].name.clone(), |t| t.name.clone());
                let id = "stubtool_1".to_string();
                let input = serde_json::json!({ key: value });
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

    #[tokio::test]
    async fn stub_selects_tool_by_prefix() {
        // websearch: プレフィックスは提示ツールの中から web_search を選ぶ（順序非依存）。
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "websearch: rust")]);
        for name in ["code_interpreter", "web_search", "web_fetch"] {
            req.tools.push(crate::model::ToolDef {
                name: name.into(),
                description: "d".into(),
                input_schema: serde_json::json!({}),
            });
        }
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        let mut input = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { name, .. } => tool_name = Some(name),
                StreamDelta::ToolUseStop { input: i, .. } => input = Some(i),
                _ => {}
            }
        }
        assert_eq!(tool_name.as_deref(), Some("web_search"));
        assert_eq!(input.unwrap()["query"], "rust");
    }

    #[tokio::test]
    async fn stub_python_prefix_selects_code_interpreter() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "python: print(1)")]);
        req.tools.push(crate::model::ToolDef {
            name: "code_interpreter".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
        });
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        let mut input = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { name, .. } => tool_name = Some(name),
                StreamDelta::ToolUseStop { input: i, .. } => input = Some(i),
                _ => {}
            }
        }
        assert_eq!(tool_name.as_deref(), Some("code_interpreter"));
        assert_eq!(input.unwrap()["code"], "print(1)");
    }

    #[tokio::test]
    async fn stub_falls_back_to_first_tool_when_named_absent() {
        // websearch: だが web_search が提示に無い → tools[0]（doc_search）にフォールバック。
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "websearch: x")]);
        req.tools.push(crate::model::ToolDef {
            name: "doc_search".into(),
            description: "d".into(),
            input_schema: serde_json::json!({}),
        });
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        while let Some(ev) = s.next().await {
            if let StreamDelta::ToolUseStart { name, .. } = ev.unwrap() {
                tool_name = Some(name);
            }
        }
        assert_eq!(tool_name.as_deref(), Some("doc_search"));
    }
}
