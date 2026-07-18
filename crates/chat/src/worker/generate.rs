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

use super::history::{message_preview, message_text};
use super::sink::WorkerSink;
use super::ChatWorker;
use crate::model::Role;
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
        // skill のピン解決（Task 6.9・fail-closed: 読めないピンは run を失敗させる）。
        let skill = crate::skill::AppliedSkill::load(
            ctx,
            self.skill_artifacts.as_ref(),
            run,
            run.trace_id.as_deref(),
        )
        .await?;

        // 共通ツール（doc_search / code_interpreter / web）。
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        if let Some(search) = &self.search {
            // skill の知識スコープを doc_search に反映する（Task 6.8・絞り込みのみ）。
            let scope = skill
                .as_ref()
                .and_then(crate::skill::AppliedSkill::search_scope);
            tools.push(Arc::new(DocSearchTool::with_scope(search.clone(), scope)));
        }
        if let Some(sandbox) = &self.sandbox {
            tools.push(Arc::new(CodeInterpreterTool::new(
                sandbox.clone(),
                self.artifacts.clone(),
                self.config.sandbox_backend,
            )));
        }
        if let Some(provider) = &self.web_search {
            tools.push(Arc::new(WebSearchTool::new(provider.clone())));
            if let Some(sandbox) = &self.sandbox {
                tools.push(Arc::new(WebFetchTool::new(sandbox.clone())));
            }
        }
        // generative UI（emit_ui・Task 6.4）: 検証層が配線されている時のみ提示する。
        if let Some(validator) = &self.ui_validator {
            tools.push(Arc::new(gui::EmitUiTool::new(validator.clone())));
        }
        // AI ワークフロー編集（emit_workflow / read_workflow・Task 10.13）:
        // ストアとカタログ源（保存 API と同一実装）が両方配線されている時のみ提示する。
        if let (Some(store), Some(catalog)) = (&self.workflow_store, &self.workflow_catalog) {
            tools.push(Arc::new(crate::workflow_tool::EmitWorkflowTool::new(
                store.clone(),
                catalog.clone(),
            )));
            tools.push(Arc::new(crate::workflow_tool::ReadWorkflowTool::new(
                store.clone(),
            )));
        }
        // AI ドキュメント共同編集（ノート/スライド・Task 11P.4/11.3）。
        self.push_collab_tools(&mut tools);
        // AI Office 編集（office.edit・Task 11.8）: office 有効時のみ提示する。
        // 非ロック時=新バージョン／WOPI ロック中=提案バージョン（PIT-44）。
        if let Some(office) = &self.office {
            tools.push(Arc::new(crate::office_tool::OfficeEditTool::new(
                office.clone(),
            )));
        }
        // CSV ツール（csv.query / csv.patch / csv.write・Task 11P.9）: tabular 配線時のみ。
        // 認可は操作別のファイル ReBAC（TabularService が StorageService 経由で強制）。
        if let Some(tabular) = &self.tabular {
            tools.push(Arc::new(crate::csv_tool::CsvQueryTool::new(
                tabular.clone(),
            )));
            tools.push(Arc::new(crate::csv_tool::CsvPatchTool::new(
                tabular.clone(),
            )));
            tools.push(Arc::new(crate::csv_tool::CsvWriteTool::new(
                tabular.clone(),
            )));
        }

        let input_preview = history.last().map(message_preview).unwrap_or_default();
        let run_ctx = RunContext {
            ctx,
            idempotency_prefix: format!("{}:{}", run.run_id, run.fencing_token),
            // run に永続化した trace_id を伝播（Langfuse/OTel/監査を相関・Task 5.9）。
            trace_id: run.trace_id.clone(),
            input_preview,
            app_id: None,
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

        // skill を最後に適用する（system 追記・few-shot・モデル既定・提示ツールの縮小）。
        // ⚠️ opts.approval には触れない（破壊系の明示許可は skill で無効化できない・Task 6.9）。
        let (mut opts, mut history) = (opts, history);
        if let Some(skill) = &skill {
            let base = opts.system.take().unwrap_or_default();
            let mut system = base;
            skill.apply_system(&mut system);
            opts.system = Some(system);
            skill.apply_model_defaults(&mut opts);
            skill.apply_few_shot(&mut history);
            skill.filter_tools(&mut tools);
            skill.audit_apply(&self.db, ctx, run).await;
        }

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
    /// ドキュメント共同編集ツールの配線（ノート=Task 11P.4／スライド=Task 11.3）。
    ///
    /// collab ハブと storage が両方配線されている時のみ提示する。編集は共有 Yjs へ
    /// 適用され、権限は実行主体の editor@file（human と同一経路・昇格しない・排他なし）。
    fn push_collab_tools(&self, tools: &mut Vec<Arc<dyn Tool>>) {
        let (Some(collab), Some(storage)) = (&self.collab, &self.storage) else {
            return;
        };
        tools.push(Arc::new(crate::document_tool::DocumentReadTool::new(
            collab.clone(),
            storage.clone(),
        )));
        tools.push(Arc::new(crate::document_tool::DocumentEditTool::new(
            collab.clone(),
            storage.clone(),
        )));
        tools.push(Arc::new(crate::document_tool::DocumentEmbedTool::new(
            collab.clone(), // 本文への genui 埋め込み（非破壊 append・確認不要・#282）。
            storage.clone(),
        )));
        // 下書きノートを用意（note_draft・下書き確定型・#282・storage 非依存・確定は UI 保存）。
        tools.push(Arc::new(crate::document_tool::SaveNoteTool::new()));
        // AI スライド共同編集（slide.read / slide.edit・Task 11.3）: ノートと同じ
        // 共同編集参加者モデル（排他なし・editor@file・HTML はサーバ側サニタイズ）。
        tools.push(Arc::new(crate::slide_tool::SlideReadTool::new(
            collab.clone(),
            storage.clone(),
        )));
        tools.push(Arc::new(crate::slide_tool::SlideEditTool::new(
            collab.clone(),
            storage.clone(),
        )));
    }

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
                self.config.sandbox_backend,
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
            // 初回自律 run: ワークスペースフォルダを作り thread に紐づける。作成先の親は
            // 利用者が選んだ workspace_parent_folder_id（無ければ Drive 直下＝None）。親フォルダの
            // editor は create_folder 内で本人 ctx により検証される（confused-deputy 防止）。
            // **thread ごとに一意な名前**にする（`node` の (parent,name) unique・別 thread と衝突しない）。
            let parent = self
                .store
                .workspace_parent_folder_id(thread_id, &ctx.tenant_id)
                .await?;
            let name = format!("agent-workspace-{thread_id}");
            match storage.create_folder(ctx, parent, &name, None).await {
                Ok(node) => {
                    self.store
                        .set_workspace_folder_if_absent(thread_id, &ctx.tenant_id, node.id)
                        .await?
                }
                // 作成失敗は 2 種を区別する: ①同一 thread の並行 run が先に作った（unique 衝突）
                // なら workspace_folder_id が既に埋まっている → それを使う。②親フォルダが選択後に
                // 削除された/editor が剥奪された等の**実失敗**なら未設定のまま → 元エラーを伝播して
                // run を失敗させる（黙って別の場所に作らない・fail-closed）。
                Err(e) => match self
                    .store
                    .workspace_folder_id(thread_id, &ctx.tenant_id)
                    .await?
                {
                    Some(id) => id,
                    None => {
                        return Err(match e {
                            storage::StorageError::Forbidden => ChatError::Forbidden,
                            storage::StorageError::NotFound => ChatError::NotFound,
                            other => ChatError::Internal(format!("workspace 作成に失敗: {other}")),
                        });
                    }
                },
            }
        };
        // 共有中の thread editor/owner にワークスペースフォルダの editor を行き渡らせる（Task 5.6(a)・冪等）。
        // 失敗は run を止めない（本人の書込には影響せず、次 run で再同期される）。
        if let Err(e) = self.store.grant_workspace_to_members(ctx, thread_id).await {
            tracing::warn!(thread_id = %thread_id, error = %e, "workspace メンバー同期に失敗（次 run で再試行）");
        }
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

        // skill のピン解決（通常チャットにも適用する・fail-closed・Task 6.9）。
        let skill = crate::skill::AppliedSkill::load(
            ctx,
            self.skill_artifacts.as_ref(),
            run,
            run.trace_id.as_deref(),
        )
        .await?;
        let scope = skill
            .as_ref()
            .and_then(crate::skill::AppliedSkill::search_scope);
        let mut history = history;

        // 直近ユーザー発話で事前検索し、文脈注入＋引用イベント。
        let query = history.last().map(message_preview).unwrap_or_default();
        let mut system = self.config.system_prompt.clone();
        if let Some(skill) = &skill {
            skill.apply_system(&mut system);
            skill.apply_few_shot(&mut history);
            skill.audit_apply(&self.db, ctx, run).await;
        }
        // skill が doc_search を許可していなければ古典事前検索も行わない（Task 6.9 の
        // セッション級ツール制限は agent/classic 両モードで一貫させる）。
        let search_allowed = skill
            .as_ref()
            .is_none_or(|s| s.allows(agent_core::ToolName::DocSearch.as_str()));
        if let (Some(search), true) = (&self.search, search_allowed) {
            match run_doc_search(
                search,
                ctx,
                &query,
                None,
                scope.as_ref(),
                run.trace_id.as_deref(),
            )
            .await
            {
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

        // skill のモデル既定（Task 6.9・指定があるものだけ上書き）。
        let (model, max_tokens, temperature) =
            match skill.as_ref().and_then(|s| s.body.model.as_ref()) {
                Some(defaults) => (
                    defaults.model.clone().or_else(|| self.config.model.clone()),
                    defaults.max_tokens.or(Some(2048)),
                    defaults.temperature,
                ),
                None => (self.config.model.clone(), Some(2048), None),
            };
        let effective_model = model
            .clone()
            .unwrap_or_else(|| self.gateway.default_model().to_string());
        let req = GenerateRequest {
            model,
            system: Some(system),
            messages: history,
            tools: Vec::new(),
            effort: None,
            max_tokens,
            temperature,
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
                    // 会計は実効モデル（skill 既定の上書き込み）で刻む。
                    model: effective_model,
                    usage,
                    trace_id: run.trace_id.clone(),
                    input_preview: query,
                    output_preview: text_acc.chars().take(2000).collect(),
                    app_id: None,
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
