//! ゲートウェイ agent.invoke の port 実装（Task 9.9）。
//!
//! agent-core `run_agent` を呼出ユーザーの [`AuthContext`] で起動する（昇格しない＝
//! confused-deputy 防御）。提示ツール = **アプリ宣言 ∩ ToolName 閉集合 ∩ 実配線**。
//! doc_search は permission-aware 検索（SearchService・pre/post-filter 二段 authz）を
//! そのまま使うため、アプリ経由でも非可読文書は混入しない。
//! LLM 呼び出しは llm-gateway を通り `app_id` 付きで `llm_usage` に計上される。

use std::sync::Arc;

use agent_core::{
    run_agent, AgentEvent, AgentOptions, DocSearchTool, EventSink, RunContext, Tool, ToolName,
};
use app_gateway::{AgentInvokeSpec, AgentPort, AiEvent, AiEventStream, GatewayError};
use authz::AuthContext;
use llm_gateway::{LlmGateway, Message as LlmMessage, Role as LlmRole};
use serde_json::json;
use tokio::sync::mpsc;

/// agent.invoke の 1 実行あたりの既定/上限ステップ数。
const DEFAULT_MAX_STEPS: usize = 8;
const MAX_STEPS_CEILING: usize = 24;

pub(crate) struct GatewayAgentPort {
    pub llm: Arc<LlmGateway>,
    pub search: Option<Arc<rag::SearchService>>,
}

/// 宣言ツール名を閉集合（ToolName）で照合し、実配線があるものだけ構築する。
///
/// 未知名・未配線は**黙って落とす**のではなく呼び出し側へ返す（`skipped`・SSE 冒頭で通知）。
fn build_tools(
    declared: &[String],
    search: Option<&Arc<rag::SearchService>>,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut skipped = Vec::new();
    for name in declared {
        match ToolName::parse(name) {
            Some(ToolName::DocSearch) => {
                if let Some(s) = search {
                    tools.push(Arc::new(DocSearchTool::with_scope(Arc::clone(s), None)));
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
        AgentEvent::ToolCall { id, name, input } => AiEvent {
            event: "tool_call".into(),
            data: json!({ "id": id, "name": name, "input": input }),
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
        let (tools, skipped) = build_tools(&spec.declared_tools, self.search.as_ref());

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
        let (tools, skipped) =
            build_tools(&["doc_search".into(), "shell".into(), "bogus".into()], None);
        assert!(tools.is_empty());
        assert_eq!(skipped, vec!["doc_search", "shell", "bogus"]);
        // 宣言なし → 提示なし。
        let (tools, skipped) = build_tools(&[], None);
        assert!(tools.is_empty() && skipped.is_empty());
    }
}
