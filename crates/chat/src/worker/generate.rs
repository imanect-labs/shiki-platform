//! 生成モード（Task 3.3/3.4/3.9）。claim 済み run を agent-core ループ（agent_mode ON）または
//! 古典 RAG 注入＋gateway 直叩き（OFF）で生成し、イベントを [`WorkerSink`] へ流す。
//!
//! いずれも発話ユーザーの [`AuthContext`] で実行し昇格しない（confused-deputy 防御）。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use agent_core::{
    run_agent, AgentOptions, ApprovalPolicy, CodeInterpreterTool, DocSearchTool, FsDeleteTool,
    FsEditTool, FsListTool, FsReadTool, FsWriteTool, GrepTool, RunContext, ShellTool, Tool,
    WebFetchTool, WebSearchTool, WorkspaceStore,
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

    /// エージェントモード（agent-core ループ）。`run.autonomous` で Chat/Autonomous を切り替える。
    pub(super) async fn run_agent_mode(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        history: Vec<LlmMessage>,
        cancel: Arc<AtomicBool>,
        sink: &mut WorkerSink,
    ) -> Result<(), ChatError> {
        // 共通ツール（doc_search / code_interpreter / web）。
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
            if let Some(sandbox) = &self.sandbox {
                tools.push(Arc::new(WebFetchTool::new(sandbox.clone())));
            }
        }

        let input_preview = history.last().map(message_preview).unwrap_or_default();
        let run_ctx = RunContext {
            ctx,
            idempotency_prefix: format!("{}:{}", run.run_id, run.fencing_token),
            // run に永続化した trace_id を伝播（Langfuse/OTel/監査を相関・Task 5.9）。
            trace_id: run.trace_id.clone(),
            input_preview,
        };

        // 自律プロファイル: フルツール（fs CRUD/grep/shell）＋予算＋計画＋承認ゲート（Task 5.1/5.4/5.6/5.7）。
        let (opts, approver) = if run.autonomous {
            if let Some(storage) = &self.storage {
                let workspace = self.ensure_workspace(ctx, run.thread_id, storage).await?;
                self.push_autonomous_tools(&mut tools, workspace);
                let mut opts = AgentOptions::autonomous(
                    self.config.autonomous_max_steps,
                    None,
                    self.config.autonomous_max_tokens,
                    self.config.autonomous_max_cost_usd_micros,
                );
                opts.system = Some(autonomous_system_prompt(&self.config.system_prompt));
                self.config.model.clone_into(&mut opts.model);
                // 版管理・復元可能な書込は自動承認、shell/削除はユーザー承認（スコープ限定事前許可）。
                opts.approval =
                    ApprovalPolicy::auto(["fs_write".to_string(), "fs_edit".to_string()]);
                let approver = crate::approver::DbApprover::new(
                    self.store.clone(),
                    run.run_id,
                    run.fencing_token,
                    cancel,
                );
                (opts, Some(approver))
            } else {
                // storage 未配線: 自律不能。制約版に落とす（黙って弱くしない・警告）。
                tracing::warn!(run_id = %run.run_id, "autonomous run but storage unwired; falling back to chat profile");
                (chat_opts(self), None)
            }
        } else {
            (chat_opts(self), None)
        };

        let approver_ref = approver.as_ref().map(|a| a as &dyn agent_core::Approver);
        let outcome = run_agent(
            &self.gateway,
            &tools,
            history,
            &run_ctx,
            &opts,
            None,
            approver_ref,
            sink,
        )
        .await
        .map_err(|e| ChatError::Unavailable(format!("agent: {e}")))?;
        let _ = outcome; // Completed / Budget / LoopDetected / Cancelled は content ＋ status で処理
        Ok(())
    }

    /// 自律ツール（file CRUD/grep/shell）を tools へ追加する。
    fn push_autonomous_tools(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        workspace: Arc<dyn WorkspaceStore>,
    ) {
        tools.push(Arc::new(FsListTool::new(workspace.clone())));
        tools.push(Arc::new(FsReadTool::new(workspace.clone())));
        tools.push(Arc::new(GrepTool::new(workspace.clone())));
        tools.push(Arc::new(FsWriteTool::new(workspace.clone())));
        tools.push(Arc::new(FsEditTool::new(workspace.clone())));
        tools.push(Arc::new(FsDeleteTool::new(workspace.clone())));
        // shell はワークスペースを seed→sync する（sandbox 必須）。
        if let Some(sandbox) = &self.sandbox {
            tools.push(Arc::new(ShellTool::new(
                sandbox.clone(),
                workspace,
                self.config.sandbox_software.clone(),
            )));
        }
    }

    /// thread のワークスペースフォルダを解決 or 作成し、`WorkspaceStore` を返す（Durable Workspace）。
    async fn ensure_workspace(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        storage: &Arc<storage::StorageService>,
    ) -> Result<Arc<dyn WorkspaceStore>, ChatError> {
        let folder_id = if let Some(id) = self
            .store
            .workspace_folder_id(thread_id, &ctx.tenant_id)
            .await?
        {
            id
        } else {
            // 初回自律 run: Drive 直下にワークスペースフォルダを作り thread に紐づける。
            let node = storage
                .create_folder(ctx, None, "agent-workspace", None)
                .await
                .map_err(|e| ChatError::Internal(format!("workspace folder: {e}")))?;
            self.store
                .set_workspace_folder_if_absent(thread_id, &ctx.tenant_id, node.id)
                .await?
        };
        Ok(Arc::new(crate::workspace::StorageWorkspaceStore::new(
            storage.clone(),
            folder_id,
        )))
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
            match run_doc_search(search, ctx, &query, None, run.trace_id.as_deref()).await {
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
                    trace_id: run.trace_id.clone(),
                    input_preview: query,
                    output_preview: text_acc.chars().take(2000).collect(),
                },
            )
            .await;
        Ok(())
    }
}

/// Chat プロファイルの実行オプション（制約版・現行挙動）。
fn chat_opts(worker: &ChatWorker) -> AgentOptions {
    let mut opts = AgentOptions::chat(worker.config.max_steps);
    opts.system = Some(worker.config.system_prompt.clone());
    worker.config.model.clone_into(&mut opts.model);
    opts
}

/// 自律プロファイルの system プロンプト（計画・ワークスペース・承認の作法を足す）。
fn autonomous_system_prompt(base: &str) -> String {
    format!(
        "{base}\n\n\
         あなたは自律エージェントです。与えられた目標を達成するため、次の作法で進めてください:\n\
         - まず `plan` ツールで目標を数個のサブタスクに分解し、進捗に応じて計画を更新する。\n\
         - 作業ディレクトリ（ワークスペース）のファイルは fs_list/fs_read/grep で調べ、fs_write/fs_edit で編集する。\n\
         - コマンド実行が必要なら shell を使う（1 コマンドずつ・ネットワークは遮断）。\n\
         - 破壊的な操作（shell・削除）は承認が必要な場合がある。承認待ちで停止したら結果を待つ。\n\
         - 目標を達成したら簡潔に要約して終了する。"
    )
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
