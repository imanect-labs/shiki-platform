//! 生成モード（Task 3.3/3.4/3.9）。claim 済み run を agent-core ループ（agent_mode ON）または
//! 古典 RAG 注入＋gateway 直叩き（OFF）で生成し、イベントを [`WorkerSink`] へ流す。
//!
//! いずれも発話ユーザーの [`AuthContext`] で実行し昇格しない（confused-deputy 防御）。

use std::sync::Arc;

use agent_core::{
    run_agent, AgentOptions, CodeInterpreterTool, DocSearchTool, RunContext, Tool, WebFetchTool,
    WebSearchTool,
};
use authz::AuthContext;
use futures::stream::StreamExt;
use llm_gateway::{
    GenerateRequest, GenerationRecord, Message as LlmMessage, Role as LlmRole, StreamDelta,
};
use uuid::Uuid;

use super::sink::WorkerSink;
use super::ChatWorker;
use crate::model::{ContentBlock, Role};
use crate::store::ClaimedRun;
use crate::ChatError;

impl ChatWorker {
    /// 直前までのメッセージを LLM 履歴へ写す（テキストのみ・短ホライズン）。
    pub(super) async fn build_history(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        assistant_message_id: Uuid,
    ) -> Result<Vec<LlmMessage>, ChatError> {
        let msgs = self.store.get_messages(ctx, thread_id, None).await?;
        let mut out = Vec::new();
        for m in msgs {
            if m.id == assistant_message_id {
                continue; // 生成対象のプレースホルダは履歴に含めない
            }
            let role = match m.role {
                Role::User => LlmRole::User,
                Role::Assistant => LlmRole::Assistant,
                _ => continue,
            };
            let text = message_text(&m.content);
            if text.trim().is_empty() {
                continue;
            }
            out.push(LlmMessage::text(role, text));
        }
        Ok(out)
    }

    /// エージェントモード（agent-core ループ・doc_search ツール）。
    pub(super) async fn run_agent_mode(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        history: Vec<LlmMessage>,
        sink: &mut WorkerSink,
    ) -> Result<(), ChatError> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        if let Some(search) = &self.search {
            tools.push(Arc::new(DocSearchTool::new(search.clone())));
        }
        if let Some(sandbox) = &self.sandbox {
            tools.push(Arc::new(CodeInterpreterTool::new(
                sandbox.clone(),
                self.artifacts.clone(),
            )));
        }
        if let Some(provider) = &self.web_search {
            tools.push(Arc::new(WebSearchTool::new(provider.clone())));
            // web_fetch は sandbox egress（run 限定 dynamic_allow）を使うため sandbox 必須。
            if let Some(sandbox) = &self.sandbox {
                tools.push(Arc::new(WebFetchTool::new(sandbox.clone())));
            }
        }
        let input_preview = history.last().map(message_preview).unwrap_or_default();
        let run_ctx = RunContext {
            ctx,
            idempotency_prefix: format!("{}:{}", run.run_id, run.fencing_token),
            trace_id: None,
            input_preview,
        };
        let mut opts = AgentOptions::chat(self.config.max_steps);
        opts.system = Some(self.config.system_prompt.clone());
        opts.model = self.config.model.clone();
        // Chat プロファイルは承認要求を出さない（破壊系ツールを提示しない）。
        let outcome = run_agent(
            &self.gateway,
            &tools,
            history,
            &run_ctx,
            &opts,
            None,
            None,
            sink,
        )
        .await
        .map_err(|e| ChatError::Unavailable(format!("agent: {e}")))?;
        let _ = outcome; // Completed / Budget / Cancelled は cancel フラグと content で処理
        Ok(())
    }

    /// 通常チャット（OFF）。古典 RAG 注入＋llm-gateway 直叩き（ツールループ無し）。
    pub(super) async fn run_classic_mode(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        history: Vec<LlmMessage>,
        sink: &mut WorkerSink,
    ) -> Result<(), ChatError> {
        use agent_core::{run_doc_search, AgentEvent, EventSink};

        // 直近ユーザー発話で事前検索し、文脈注入＋引用イベント。
        let query = history.last().map(message_preview).unwrap_or_default();
        let mut system = self.config.system_prompt.clone();
        if let Some(search) = &self.search {
            match run_doc_search(search, ctx, &query, None, None).await {
                Ok(result) => {
                    system.push_str("\n\n# 参考（社内文書検索の結果）\n");
                    system.push_str(&result.context_text);
                    for c in result.citations {
                        // 古典注入でも引用を UI/監査へ流す（post-filter は検索内で済み）。
                        sink.emit(AgentEvent::Citation(agent_core::Citation {
                            node_id: c.node_id,
                            chunk_id: c.chunk_id,
                            snippet: c.snippet,
                            page: c.page,
                            heading_path: c.heading_path,
                            score: c.score,
                        }))
                        .await
                        .map_err(|e| ChatError::Internal(e.to_string()))?;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "classic doc_search failed; continuing without");
                }
            }
        }

        let req = GenerateRequest {
            model: self.config.model.clone(),
            system: Some(system),
            messages: history,
            tools: Vec::new(),
            effort: None,
            max_tokens: Some(2048),
            temperature: None,
        };
        let mut stream = self
            .gateway
            .stream(req)
            .await
            .map_err(|e| ChatError::Unavailable(format!("llm: {e}")))?;

        let mut text_acc = String::new();
        let mut usage = llm_gateway::Usage::default();
        while let Some(delta) = stream.next().await {
            if sink.is_cancelled() {
                break;
            }
            match delta.map_err(|e| ChatError::Unavailable(e.to_string()))? {
                StreamDelta::TextDelta { text } => {
                    text_acc.push_str(&text);
                    sink.emit(AgentEvent::Text(text))
                        .await
                        .map_err(|e| ChatError::Internal(e.to_string()))?;
                }
                StreamDelta::ThinkingDelta { text } => {
                    sink.emit(AgentEvent::Thinking(text))
                        .await
                        .map_err(|e| ChatError::Internal(e.to_string()))?;
                }
                StreamDelta::Done { usage: u, .. } => usage = u,
                _ => {} // 通常チャットはツールを使わない
            }
        }

        self.gateway
            .record_generation(
                ctx,
                &GenerationRecord {
                    idempotency_key: format!("{}:{}:0", run.run_id, run.fencing_token),
                    model: self
                        .config
                        .model
                        .clone()
                        .unwrap_or_else(|| self.gateway.default_model().to_string()),
                    usage,
                    trace_id: None,
                    input_preview: query,
                    output_preview: text_acc.chars().take(2000).collect(),
                },
            )
            .await;
        Ok(())
    }
}

/// content block 列からテキスト（＋添付名）を抽出する（LLM 履歴用）。
fn message_text(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => parts.push(text.clone()),
            ContentBlock::FileRef { name, .. } => parts.push(format!("[添付: {name}]")),
            _ => {}
        }
    }
    parts.join("\n")
}

/// LLM メッセージのテキストプレビュー（Langfuse/検索クエリ用）。
fn message_preview(m: &LlmMessage) -> String {
    m.content
        .iter()
        .filter_map(|b| match b {
            llm_gateway::Block::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}
