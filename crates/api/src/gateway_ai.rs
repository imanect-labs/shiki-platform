//! ゲートウェイ agent.invoke の port 実装（Task 9.9）。
//!
//! agent-core `run_agent` を呼出ユーザーの [`AuthContext`] で起動する（昇格しない＝
//! confused-deputy 防御）。提示ツール = **アプリ宣言 ∩ ToolName 閉集合 ∩ 実配線**。
//! doc_search は permission-aware 検索（SearchService・pre/post-filter 二段 authz）を
//! そのまま使うため、アプリ経由でも非可読文書は混入しない。doc_read（#425）も chat と同じ
//! 実装（StorageService の viewer 判定＋可読集合の pre-filter）を通る。
//! 読み取り系ツールは、対応する API と同じ能力スコープがアプリに効いているときだけ提示する
//! （doc_search = `rag.query`・doc_read = `storage.read`）。agent.invoke を scope の迂回路にしない。
//! LLM 呼び出しは llm-gateway を通り `app_id` 付きで `llm_usage` に計上される。

use std::sync::Arc;

use agent_core::{
    run_agent, AgentEvent, AgentOptions, DocSearchTool, EventSink, RunContext, Tool, ToolName,
};
use app_gateway::{AgentInvokeSpec, AgentPort, AiEvent, AiEventStream, GatewayError};
use authz::{AuthContext, CapabilityScope};
use llm_gateway::{LlmGateway, Message as LlmMessage, Role as LlmRole};
use serde_json::json;
use tokio::sync::mpsc;

/// agent.invoke の 1 実行あたりの既定/上限ステップ数。
const DEFAULT_MAX_STEPS: usize = 8;
const MAX_STEPS_CEILING: usize = 24;

pub(crate) struct GatewayAgentPort {
    pub llm: Arc<LlmGateway>,
    pub search: Option<Arc<rag::SearchService>>,
    /// doc_read の認可（viewer 判定）に使う。search と両方そろって doc_read を配線する。
    pub storage: Option<Arc<storage::StorageService>>,
}

/// 宣言ツール名を閉集合（ToolName）で照合し、実配線があるものだけ構築する。
///
/// 未知名・未配線は**黙って落とす**のではなく呼び出し側へ返す（`skipped`・SSE 冒頭で通知）。
fn build_tools(
    declared: &[String],
    scopes: &[CapabilityScope],
    search: Option<&Arc<rag::SearchService>>,
    storage: Option<&Arc<storage::StorageService>>,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut skipped = Vec::new();
    let allowed = |tool: ToolName| required_scope(tool).is_none_or(|s| scopes.contains(&s));
    // doc_read が実際に提示されるときだけ、doc_search の説明で本文の読み方を案内する。
    let doc_read_wired = search.is_some()
        && storage.is_some()
        && allowed(ToolName::DocRead)
        && declared
            .iter()
            .any(|d| ToolName::parse(d) == Some(ToolName::DocRead));
    let mut seen = std::collections::HashSet::new();
    for name in declared {
        // 同じツールの重複宣言は 1 つにまとめる（同名ツールが 2 つあるとプロバイダが拒否する）。
        if !seen.insert(name.as_str()) {
            continue;
        }
        match ToolName::parse(name) {
            // 対応する API のスコープが効いていない読み取り系ツールは提示しない。
            Some(tool) if !allowed(tool) => skipped.push(name.clone()),
            Some(ToolName::DocSearch) => {
                if let Some(s) = search {
                    let tool = DocSearchTool::with_scope(Arc::clone(s), None);
                    tools.push(Arc::new(if doc_read_wired {
                        tool.with_doc_read()
                    } else {
                        tool
                    }));
                } else {
                    skipped.push(name.clone());
                }
            }
            Some(ToolName::DocRead) => {
                if let (Some(s), Some(st)) = (search, storage) {
                    tools.push(Arc::new(chat::doc_read_tool::DocReadTool::new(
                        Arc::clone(s),
                        Arc::clone(st),
                    )));
                } else {
                    skipped.push(name.clone());
                }
            }
            // その他の閉集合ツール（web/code_interpreter/fs/shell 等）はゲートウェイ実行
            // 文脈（ワークスペース/サンドボックス無し）では未配線＝提示しない。
            Some(_) | None => skipped.push(name.clone()),
        }
    }
    (tools, skipped)
}

/// ツールに要る能力スコープ（同じものを読む API と同じ・None はスコープ不要）。
fn required_scope(tool: ToolName) -> Option<CapabilityScope> {
    match tool {
        ToolName::DocSearch => Some(CapabilityScope::RagQuery),
        ToolName::DocRead => Some(CapabilityScope::StorageRead),
        _ => None,
    }
}

/// [`AgentEvent`] → SSE 中立形。
fn to_ai_event(ev: &AgentEvent) -> AiEvent {
    match ev {
        AgentEvent::Text(t) => AiEvent {
            event: "text".into(),
            data: json!({ "text": t }),
        },
        AgentEvent::Thinking(t) => AiEvent {
            event: "thinking".into(),
            data: json!({ "text": t }),
        },
        AgentEvent::ToolCall {
            id,
            name,
            input,
            step,
        } => AiEvent {
            event: "tool_call".into(),
            data: json!({ "id": id, "name": name, "input": input, "step": step }),
        },
        AgentEvent::ToolResult {
            tool_call_id,
            ok,
            content,
        } => AiEvent {
            event: "tool_result".into(),
            data: json!({ "tool_call_id": tool_call_id, "ok": ok, "content": content }),
        },
        AgentEvent::Citation(c) => AiEvent {
            event: "citation".into(),
            data: serde_json::to_value(c).unwrap_or_else(|_| json!({})),
        },
        AgentEvent::BudgetWarning { kind, used, limit } => AiEvent {
            event: "budget_warning".into(),
            data: json!({ "kind": format!("{kind:?}"), "used": used, "limit": limit }),
        },
        // その他（Artifact/Plan/承認系）はゲートウェイ agent 文脈では発生しない構成だが、
        // 将来増えても情報落ちしないよう Debug 表現で流す。
        other => AiEvent {
            event: "event".into(),
            data: json!({ "debug": format!("{other:?}") }),
        },
    }
}

/// AgentEvent を mpsc へ写す sink（受信側が消えたら協調キャンセル）。
struct ChannelSink {
    tx: mpsc::Sender<AiEvent>,
}

#[async_trait::async_trait]
impl EventSink for ChannelSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), agent_core::AgentError> {
        // 受信側 drop（クライアント切断）は is_cancelled が拾う。ここでは無視してよい。
        let _ = self.tx.send(to_ai_event(&event)).await;
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.tx.is_closed()
    }
}

#[async_trait::async_trait]
impl AgentPort for GatewayAgentPort {
    async fn invoke(
        &self,
        ctx: &AuthContext,
        spec: AgentInvokeSpec,
    ) -> Result<AiEventStream, GatewayError> {
        let (tools, skipped) = build_tools(
            &spec.declared_tools,
            &spec.scopes,
            self.search.as_ref(),
            self.storage.as_ref(),
        );

        let max_steps = spec
            .max_steps
            .unwrap_or(DEFAULT_MAX_STEPS)
            .clamp(1, MAX_STEPS_CEILING);
        let mut opts = AgentOptions::chat(max_steps);
        opts.model = spec.model.clone();
        if let Some(cap) = spec.max_tokens.and_then(|t| u32::try_from(t).ok()) {
            opts.max_tokens = Some(opts.max_tokens.map_or(cap, |d| d.min(cap)));
        }
        // 日次残額をループ予算にも渡す（実行中の超過もステップ境界で停止）。
        opts.budget.max_cost_usd_micros = Some(spec.max_cost_usd_micros);

        let (tx, rx) = mpsc::channel::<AiEvent>(64);
        if !skipped.is_empty() {
            // 宣言されたが提示できないツールを冒頭で通知する（黙って弱くしない）。
            let _ = tx
                .send(AiEvent {
                    event: "tools_unavailable".into(),
                    data: json!({ "tools": skipped }),
                })
                .await;
        }

        let llm = Arc::clone(&self.llm);
        let ctx_owned = ctx.clone();
        let prompt = spec.prompt.clone();
        let app_id = spec.app_id;
        let trace_id = spec.trace_id.clone();
        tokio::spawn(async move {
            let run_ctx = RunContext {
                ctx: &ctx_owned,
                // ゲートウェイ agent.invoke の 1 回が trace の起点。
                owns_trace: true,
                idempotency_prefix: format!("gw-agent:{app_id}:{}", uuid::Uuid::new_v4()),
                trace_id,
                input_preview: prompt.chars().take(200).collect(),
                app_id: Some(app_id),
            };
            let history = vec![LlmMessage::text(LlmRole::User, prompt)];
            let mut sink = ChannelSink { tx: tx.clone() };
            match run_agent(
                &llm, &tools, history, &run_ctx, &opts, None, None, &mut sink,
            )
            .await
            {
                Ok(outcome) => {
                    let _ = tx
                        .send(AiEvent {
                            event: "done".into(),
                            data: json!({ "stop": format!("{:?}", outcome.stop) }),
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AiEvent {
                            event: "error".into(),
                            data: json!({ "error": e.to_string() }),
                        })
                        .await;
                }
            }
        });

        use futures::StreamExt;
        Ok(ReceiverStream::new(rx).boxed())
    }
}

/// tokio mpsc → Stream の薄いアダプタ。
struct ReceiverStream<T>(mpsc::Receiver<T>);

impl<T> ReceiverStream<T> {
    fn new(rx: mpsc::Receiver<T>) -> Self {
        ReceiverStream(rx)
    }
}

impl<T> futures::Stream for ReceiverStream<T> {
    type Item = T;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        self.0.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_declared_intersect_closed_set_and_wiring() {
        // search 未配線: doc_search も skipped。
        let all = [CapabilityScope::RagQuery, CapabilityScope::StorageRead];
        let (tools, skipped) = build_tools(
            &[
                "doc_search".into(),
                "doc_read".into(),
                "doc_read".into(),
                "shell".into(),
                "bogus".into(),
            ],
            &all,
            None,
            None,
        );
        assert!(tools.is_empty());
        // 重複宣言は 1 つにまとまる。
        assert_eq!(skipped, vec!["doc_search", "doc_read", "shell", "bogus"]);
        // 宣言なし → 提示なし。
        let (tools, skipped) = build_tools(&[], &all, None, None);
        assert!(tools.is_empty() && skipped.is_empty());
    }

    #[test]
    fn read_tools_need_the_same_scope_as_their_apis() {
        assert_eq!(
            required_scope(ToolName::DocSearch),
            Some(CapabilityScope::RagQuery)
        );
        assert_eq!(
            required_scope(ToolName::DocRead),
            Some(CapabilityScope::StorageRead)
        );
        // スコープが無ければ配線の有無に関わらず skipped（配線前に弾く）。
        let (tools, skipped) = build_tools(
            &["doc_search".into(), "doc_read".into()],
            &[CapabilityScope::AgentInvoke],
            None,
            None,
        );
        assert!(tools.is_empty());
        assert_eq!(skipped, vec!["doc_search", "doc_read"]);
    }
}
