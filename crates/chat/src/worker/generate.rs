//! 生成モード（Task 3.3/3.4/3.9）。claim 済み run を agent-core ループ（agent_mode ON）または
//! 古典 RAG 注入＋gateway 直叩き（OFF）で生成し、イベントを [`WorkerSink`] へ流す。
//!
//! いずれも発話ユーザーの [`AuthContext`] で実行し昇格しない（confused-deputy 防御）。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use agent_core::{
    run_agent, AgentOptions, CodeInterpreterTool, DocSearchTool, RunContext, Tool, WebFetchTool,
    WebSearchTool, WorkspaceStore,
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
use crate::autonomous::AutonomousMode;
use crate::model::{Role, StreamEventKind};
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
        // AI ドキュメント共同編集（ノート/スライド・Task 11P.4/11.3）＋下書き系＋
        // Office 編集/CSV ツール（worker/toolset.rs に集約）。
        self.push_collab_tools(&mut tools);
        self.push_office_and_csv_tools(&mut tools);

        let input_preview = history.last().map(message_preview).unwrap_or_default();
        let run_ctx = RunContext {
            ctx,
            idempotency_prefix: format!("{}:{}", run.run_id, run.fencing_token),
            // run に永続化した trace_id を伝播（Langfuse/OTel/監査を相関・Task 5.9）。
            trace_id: run.trace_id.clone(),
            input_preview,
            app_id: None,
        };

        // 承認者は自律/通常チャットの双方へ配線する。破壊系ツール（document.edit / slide.edit /
        // csv.patch / csv.write 等・requires_confirmation）は、ノート/スライド/CSV の
        // ドキュメントアシスタント（非自律）でも human-in-the-loop の承認カードを出して実行する。
        // 承認 UI/API/DbApprover は種別非依存で、承認が無ければ実行しない（fail-safe・破壊系を
        // 黙って走らせない）。これを配線しないと非自律チャットでは承認者不在により編集ツールが
        // 常に "requires explicit confirmation" で拒否され、共同編集が機能しない。
        let mut approver = crate::approver::DbApprover::new(
            self.store.clone(),
            run.run_id,
            run.fencing_token,
            cancel,
        );
        // 自律プロファイル: フルツール（fs CRUD/grep/shell）＋予算＋計画＋承認ゲート（Task 5.1/5.4/5.6/5.7）。
        let opts = if run.autonomous {
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
                // 承認 3 モード（#350・既定は承認必須）。実行中のトグルは approver の
                // current_policy が各破壊系呼び出しの直前に反映する。
                let snapshot;
                (snapshot, opts.approval) = self.autonomous_approval(ctx, run).await?;
                approver = approver.with_autonomous_mode(
                    run.thread_id,
                    ctx.tenant_id.clone(),
                    ctx.principal.id.clone(),
                    snapshot,
                );
                opts
            } else {
                // storage 未配線: 自律不能。制約版に落とす（黙って弱くしない・警告）。
                tracing::warn!(run_id = %run.run_id, "autonomous run but storage unwired; falling back to chat profile");
                chat_opts(self)
            }
        } else {
            // 通常チャット: deny_all（既定）のまま。破壊系は都度ユーザー承認が要る（要確認ツールの設計意図）。
            chat_opts(self)
        };
        let approver = Some(approver);

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

        // resume 配線（#351）: 保存済みチェックポイントがあればステップ境界から再開する。
        let resume = match restore_checkpoint(run) {
            Some(envelope) => {
                // takeover 前のイベントログ（チェックポイント境界まで）から content projection を
                // 再構築する（続きだけを生成するため、これ無しでは finalize 時に前半のテキスト/
                // ツール結果が消える。境界より後＝中断ステップの途中イベントは再生成されるので除く）。
                sink.seed_from_log(envelope.event_seq).await?;
                Some(envelope.checkpoint)
            }
            None => None,
        };

        let approver_ref = approver.as_ref().map(|a| a as &dyn agent_core::Approver);
        let outcome = run_agent(
            &self.gateway,
            &tools,
            history,
            &run_ctx,
            &opts,
            resume,
            approver_ref,
            sink,
        )
        .await
        .map_err(|e| ChatError::Unavailable(format!("agent: {e}")))?;
        let _ = outcome; // Completed / Budget / LoopDetected / Cancelled は content ＋ status で処理
        Ok(())
    }

    /// 自律 run の実効承認ポリシを決める（#350）: run スナップショット×thread の現在モード×
    /// org キャップ（写像と実効判定は autonomous.rs に集約）。クランプ時は SSE で明示する
    /// （黙って降格しない・generation_event として replay/監査にも残る）。
    /// 戻り値は（スナップショットモード, 実効ポリシ）。
    async fn autonomous_approval(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
    ) -> Result<(AutonomousMode, agent_core::ApprovalPolicy), ChatError> {
        let snapshot = AutonomousMode::parse(&run.autonomous_mode).unwrap_or_default();
        let (current, set_by) = self
            .store
            .thread_autonomous_mode(run.thread_id, &ctx.tenant_id)
            .await?;
        let bypass_allowed = self.store.autonomous_bypass_allowed(&ctx.tenant_id).await?;
        let (effective, clamp) = crate::autonomous::effective_mode(
            snapshot,
            current,
            set_by.as_deref(),
            &ctx.principal.id,
            bypass_allowed,
        );
        if let Some(clamp) = clamp {
            let _ = self
                .store
                .append_stream_event(
                    run.run_id,
                    run.fencing_token,
                    &StreamEventKind::FailureRecovery {
                        detail: clamp.detail().to_string(),
                        action: "mode_clamped".to_string(),
                    },
                )
                .await;
        }
        Ok((snapshot, effective.approval_policy()))
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

/// 保存済みチェックポイント封筒を復元する（#351・自律 run のみ）。
///
/// 復元できない（旧形式等の）チェックポイントは警告して新規開始へフォールバックする
/// （run を止めない・副作用の収束は版管理と冪等キーが担う）。
pub(super) fn restore_checkpoint(run: &ClaimedRun) -> Option<super::sink::CheckpointEnvelope> {
    if !run.autonomous {
        return None;
    }
    run.checkpoint
        .as_ref()
        .and_then(|j| match serde_json::from_value(j.0.clone()) {
            Ok(envelope) => Some(envelope),
            Err(e) => {
                tracing::warn!(run_id = %run.run_id, error = %e,
                    "checkpoint の復元に失敗（新規開始へフォールバック）");
                None
            }
        })
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
