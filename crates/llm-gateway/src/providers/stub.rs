//! 決定的スタブプロバイダ（テスト/CI・外部依存なし）。
//!
//! 実 LLM を持たない環境（CI・オフライン）でパイプライン全体（chat run・SSE・会計・
//! agent-core ループ）をエンドツーエンドで検証するための決定的アダプタ。挙動:
//! - 直近 user メッセージ本文を語単位でストリーミングして返す。
//! - `slow:<秒> <本文>` は本文を出したあと指定秒だけ生成を続ける（生成中の UI＝順番待ち・停止の検証用）。
//! - リクエストにツールがあり、かつ最初のターン（tool_result がまだ無い）で本文が
//!   既知のプレフィックス（`search:` / `python:` / `websearch:` / `webfetch:`）で始まるとき、
//!   対応するツールを 1 回だけ呼び出す（agent ループ・各ツールの決定的検証）。プレフィックス
//!   に対応するツールが提示されていなければ最初のツールへフォールバックする（後方互換）。
//! - generative UI（Phase 6）検証用の駆動プレフィックス `genui:`:
//!   `genui:form|table|chart|stat` は検証を通る固定スペック（`genui:chart:<kind>` で
//!   scatter/radar/... を種別指定）、`genui:bad` は不正スペック、
//!   `genui:workflow <name>` は名前参照のワークフロー起動ボタンで `emit_ui` を呼ぶ。
//! - 自律プロファイル（Phase 5）検証用の駆動プレフィックス:
//!   - `plan: A, B, C` … 1 ターン目に `plan` メタツールをカンマ区切りのサブタスクで呼ぶ（計画分解の検証）。
//!   - `loop:` … tool_result の有無に関わらず**毎ターン** tools[0] を空入力で呼び続ける
//!     （ループ検出・ステップ/予算上限・長ホライズンの決定的駆動）。

use std::time::Duration;

use futures::stream::{self, StreamExt};

use super::stub_deep_research::deep_research_call;
use super::stub_fixtures::genui_spec;
use super::stub_stream::{slow_text_stream, text_stream, tool_call_stream, tool_calls_stream};
use super::stub_triggers::note_tool_call;
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

/// 履歴の語数を prompt トークンとみなす。**ツール結果も数える**（実プロバイダと同じく観測は
/// prompt に載る）。本文だけにすると、ツールを回し続けるループで prompt が伸びず、
/// `Spent::fresh_tokens`（#404 で上限判定に使う軸）が永久に増えないため、トークン予算が
/// 「ステップ上限で先に止まる」ことでしか終われなくなる。
fn prompt_tokens(req: &GenerateRequest) -> u64 {
    req.messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            Block::Text { text } | Block::ToolResult { content: text, .. } => {
                Some(text.split_whitespace().count() as u64)
            }
            _ => None,
        })
        .sum()
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

/// `emitwf:` 駆動の固定ワークフロー IR（Task 10.13 の決定的検証）。
/// `emitwf:ok <name>` は保存パイプライン（V1〜V7）を通る IR、`emitwf:bad <name>` は
/// 語彙外ノード type を含む不正 IR（検証エラー全件のフォールバック検証用）。
fn emitwf_ir(kind: &str) -> serde_json::Value {
    let (variant, name) = kind.split_once(' ').unwrap_or((kind, "stub-flow"));
    let node_type = if variant == "bad" {
        "bogus.node"
    } else {
        "script.run"
    };
    serde_json::json!({
        "ir_version": 1,
        "name": name.trim(),
        "display_name": "スタブフロー",
        "declared_scopes": [],
        "triggers": [{ "kind": "interactive" }],
        "nodes": [{
            "id": "compute",
            "type": node_type,
            "params": { "source": { "inline": "function main(i){ return { ok: true }; }" } }
        }],
        "edges": []
    })
}

/// `subagent:<テーマ>` — 1 ステップで `subagent` を**3 体**呼ぶ（境界を重複なく割った形）。
///
/// 委譲の並列（#391）と UI の「並行して N 件」表示を決定的に再現する唯一の入口。提示ツールに
/// `subagent` が無ければ `None`（呼び出し側が通常経路へ落ちる）。
fn subagent_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let topic = user_text.strip_prefix("subagent:")?.trim();
    let tool = req.tools.iter().find(|t| t.name == "subagent")?;
    let calls = [
        ("国内・直近 3 年の実数", "国内"),
        ("同期間の海外比較", "北米・欧州"),
        ("規制・政策の動き", "法改正のみ"),
    ]
    .into_iter()
    .map(|(objective, boundary)| {
        (
            tool.name.clone(),
            serde_json::json!({
                "objective": format!("{topic}: {objective}"),
                "boundary": boundary,
            }),
        )
    })
    .collect();
    Some(tool_calls_stream(calls, prompt_tokens))
}

/// `parallel:<クエリ>` — 1 ステップで `web_search` ＋ **別ホスト**の `web_fetch` ×3 を呼ぶ。
///
/// agent-core の有界並列（#349・同一ホストは politeness_key で直列化される）と、
/// UI の「並行して N 件」表示・step グルーピング（#386）を決定的に再現する唯一の入口。
/// 提示ツールに web 系が無ければ `None`（呼び出し側が通常経路へ落ちる）。
fn parallel_read_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let query = user_text.strip_prefix("parallel:")?.trim();
    let mut calls: Vec<(String, serde_json::Value)> = Vec::new();
    if let Some(t) = req.tools.iter().find(|t| t.name == "web_search") {
        calls.push((t.name.clone(), serde_json::json!({ "query": query })));
    }
    if let Some(t) = req.tools.iter().find(|t| t.name == "web_fetch") {
        // ホストを分けないと同一レーンへ入り、並列にならない。
        for url in [
            "https://example.com/stub-1",
            "https://example.org/stub-2",
            "https://example.net/stub-3",
        ] {
            calls.push((t.name.clone(), serde_json::json!({ "url": url })));
        }
    }
    (!calls.is_empty()).then(|| tool_calls_stream(calls, prompt_tokens))
}

#[async_trait::async_trait]
impl LlmProvider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        let user_text = last_user_text(req);
        let prompt_tokens = prompt_tokens(req);

        // --- deep research（#387）: `/deep-research` 起動はフェーズを跨いで進むため最初に見る。 ---
        if !req.tools.is_empty() {
            if let Some(s) = deep_research_call(req, prompt_tokens) {
                return Ok(s);
            }
        }
        // --- 遅い生成 `slow:<秒> <本文>`: 生成中の挙動（順番待ち・停止）を決定的に試す。 ---
        if let Some(rest) = user_text.strip_prefix("slow:") {
            let (secs, body) = rest.split_once(' ').unwrap_or((rest, ""));
            let secs: u64 = secs.trim().parse().unwrap_or(3);
            return Ok(slow_text_stream(
                &format!("回答: {}", body.trim()),
                prompt_tokens,
                Duration::from_secs(secs),
            ));
        }
        // --- 自律駆動 `loop:`: 毎ターン tools[0] を空入力で呼び続ける（ループ/上限の決定的駆動）。 ---
        if !req.tools.is_empty() && user_text.starts_with("loop:") {
            return Ok(tool_call_stream(
                req.tools[0].name.clone(),
                serde_json::json!({}),
                prompt_tokens,
            ));
        }
        // --- 並行 read 駆動 `parallel:`（#386）。 ---
        if !has_tool_result(req) {
            if let Some(s) = parallel_read_call(req, &user_text, prompt_tokens) {
                return Ok(s);
            }
        }
        // --- 委譲の並列駆動 `subagent:`（#391）。 ---
        if !has_tool_result(req) {
            if let Some(s) = subagent_call(req, &user_text, prompt_tokens) {
                return Ok(s);
            }
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

        // --- generative UI 駆動 `genui:`（Phase 6 Task 6.4 の決定的検証）。 ---
        // `genui:form|table|chart` は検証を通る固定スペックで emit_ui を呼び、
        // `genui:bad` はカタログ外コンポーネントを含む不正スペック（フォールバック検証用）、
        // `genui:workflow <name>` は名前参照のワークフロー起動ボタンを出す。
        if !has_tool_result(req) {
            if let Some(rest) = user_text.strip_prefix("genui:") {
                if let Some(t) = req.tools.iter().find(|t| t.name == "emit_ui") {
                    let spec = genui_spec(rest.trim());
                    return Ok(tool_call_stream(
                        t.name.clone(),
                        serde_json::json!({ "spec": spec }),
                        prompt_tokens,
                    ));
                }
            }
        }

        // --- ノートツール駆動（issue #282 の e2e）: savenote:<name>→save_note / docembed:<id>→document.embed。 ---
        if !has_tool_result(req) {
            if let Some(s) = note_tool_call(req, &user_text, prompt_tokens) {
                return Ok(s);
            }
        }

        // --- AI ワークフロー編集駆動 `emitwf:`（Task 10.13 の決定的検証）。 ---
        if !has_tool_result(req) {
            if let Some(rest) = user_text.strip_prefix("emitwf:") {
                if let Some(t) = req.tools.iter().find(|t| t.name == "emit_workflow") {
                    let ir = emitwf_ir(rest.trim());
                    return Ok(tool_call_stream(
                        t.name.clone(),
                        serde_json::json!({ "ir": ir }),
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
        Ok(text_stream(&reply, prompt_tokens))
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
