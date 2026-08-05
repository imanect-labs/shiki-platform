//! 生成モード（Task 3.3/3.4/3.9）。claim 済み run を agent-core ループ（agent_mode ON）または
//! 古典 RAG 注入＋gateway 直叩き（OFF）で生成し、イベントを [`WorkerSink`] へ流す。
//!
//! いずれも発話ユーザーの [`AuthContext`] で実行し昇格しない（confused-deputy 防御）。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use agent_core::{run_agent, AgentOptions, RunContext, Tool, WorkspaceStore};
use authz::AuthContext;
use llm_gateway::{Message as LlmMessage, Role as LlmRole};
use uuid::Uuid;

use super::approval_policy::restore_checkpoint;
use super::history::{message_preview, message_text};
use super::opts::{autonomous_system_prompt, chat_opts, sanitize_for_prompt};
use super::sink::WorkerSink;
use super::ChatWorker;
use crate::model::Role;
use crate::store::ClaimedRun;
use crate::ChatError;

/// 履歴 1 回の読み出しから取れるもの（LLM メッセージ＋会話の添付）。
///
/// 添付は `code_interpreter` の `/workspace` seed に使う（#379）。同じ `get_messages` から
/// 取るのは、別途取り直すと DB 往復が二重になるため。
pub(super) struct ThreadHistory {
    pub messages: Vec<LlmMessage>,
    /// 会話に添付されたファイル（古い順・同名は新しい方が残る）。
    pub attachments: Vec<agent_core::AttachmentRef>,
}

impl ChatWorker {
    /// 直前までのメッセージを LLM 履歴へ写す（テキストのみ・短ホライズン）。
    pub(super) async fn build_history(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        assistant_message_id: Uuid,
    ) -> Result<ThreadHistory, ChatError> {
        let msgs = self.store.get_messages(ctx, thread_id, None).await?;
        let mut out = Vec::new();
        let mut attachments: Vec<agent_core::AttachmentRef> = Vec::new();
        for m in msgs {
            if m.id == assistant_message_id {
                continue; // 生成対象のプレースホルダは履歴に含めない
            }
            // 添付はプレースホルダ以外の全メッセージから拾う（role で絞らない）。assistant 側の
            // FileRef＝code_interpreter が保存した成果物も含む: 前ターンで作った CSV を次の
            // ターンで読み直せる方が自然で、上限は seed 側が持つ（#379）。
            for block in &m.content {
                if let crate::model::ContentBlock::FileRef { node_id, name } = block {
                    // 同名の添付は**新しい方**を残す（guest のファイル名が衝突するため）。
                    attachments.retain(|a: &agent_core::AttachmentRef| a.name != *name);
                    attachments.push(agent_core::AttachmentRef {
                        node_id: node_id.clone(),
                        name: name.clone(),
                    });
                }
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
        Ok(ThreadHistory {
            messages: out,
            attachments,
        })
    }

    /// この run のスレッドに紐づく「開いているドキュメント」（ノート/Office）を返す。
    ///
    /// ドキュメントアシスタント（`/notes/:id`・`/office/:id` のパネル）から作られた会話は
    /// `thread.origin_note_id` を持つ。編集対象の解決に使う（無ければ `None`）。
    async fn origin_document(&self, run: &ClaimedRun) -> Option<(Uuid, Option<String>)> {
        let row: Option<(Option<Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT origin_note_id, origin_note_name FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(run.thread_id)
        .bind(&run.tenant_id)
        .fetch_optional(&self.db)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "origin_note の取得に失敗"))
        .ok()
        .flatten();
        match row {
            Some((Some(id), name)) => Some((id, name)),
            _ => None,
        }
    }

    /// system プロンプトへ「開いているドキュメント」の node_id を足す。
    ///
    /// これが無いと「この文書に書いて」に対し対象 id が分からず、モデルは新規下書き
    /// （save_note / save_document）へ逃げてしまう（実 LLM 検証で確認）。
    async fn system_with_origin_document(&self, run: &ClaimedRun, base: Option<String>) -> String {
        let base = base.unwrap_or_else(|| self.config.system_prompt.clone());
        match self.origin_document(run).await {
            Some((node_id, name)) => {
                // ドキュメント名はユーザーが自由に付けられる文字列。system プロンプトへ
                // 無加工で連結すると「以降の指示を無視せよ」等を**システム発話として**
                // 注入できてしまうため、改行を潰して長さを切り、引用で括る。
                let named = name
                    .map(|n| format!("・名前: 「{}」", sanitize_for_prompt(&n)))
                    .unwrap_or_default();
                format!(
                    "{base}\n\nこの会話は開いているドキュメントに紐づいています\
                     （node_id: {node_id}{named}）。ユーザーが「この文書」「開いているノート」\
                     「このシート」等と言う場合は、新規作成や下書きではなく**この node_id を対象**に\
                     編集ツール（document.edit / office.live_edit / csv.patch 等）を使ってください。"
                )
            }
            None => base,
        }
    }

    /// エージェントモード（agent-core ループ）。`run.autonomous` で Chat/Autonomous を切り替える。
    pub(super) async fn run_agent_mode(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        thread_history: ThreadHistory,
        cancel: Arc<AtomicBool>,
        sink: &mut WorkerSink,
    ) -> Result<(), ChatError> {
        let ThreadHistory {
            messages: history,
            attachments,
        } = thread_history;
        // skill のピン解決（複数可・Task 6.9/#344・fail-closed: 読めないピンは run を失敗させる）。
        let skills = crate::skill::AppliedSkill::load_pins(
            ctx,
            self.skill_artifacts.as_ref(),
            run,
            run.trace_id.as_deref(),
        )
        .await?;

        // 提示ツール一式（共通＋ドキュメント/Office/CSV）は worker/toolset.rs に集約する。
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        self.push_core_tools(&mut tools, &skills, attachments);
        // skill ツール（カタログ引き・#344 Task 10.11）。
        self.push_skill_tool(&mut tools, ctx, run, &skills).await;
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
                // ワークスペースは遅延生成（使わない run で空フォルダを作らない・#392）。
                let (workspace, system_workspace) =
                    self.lazy_workspace(ctx, run.thread_id, storage).await?;
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
                // システム領域（自動生成の使い捨てワークスペース）への書込は承認カードを出さない
                // （#392）。**opts.approval と approver の両方**へ足す: current_policy が
                // opts.approval を上書きするため、片方だけでは run 中盤で書込が止まる。
                if system_workspace {
                    let extra = crate::autonomous::system_workspace_writes();
                    opts.approval.auto_approve.extend(extra.iter().cloned());
                    approver = approver.with_pre_authorized(extra);
                }
                opts.parallel_read_tools = self.config.parallel_read_tools;
                opts
            } else {
                // storage 未配線: 自律不能。制約版に落とす（黙って弱くしない・警告）。
                tracing::warn!(run_id = %run.run_id, "autonomous run but storage unwired; falling back to chat profile");
                chat_opts(self)
            }
        } else {
            // 通常チャット: deny_all（既定）のまま。破壊系は都度ユーザー承認が要る（要確認ツールの設計意図）。
            let mut opts = chat_opts(self);
            opts.system = Some(self.system_with_origin_document(run, opts.system).await);
            opts
        };
        let approver = Some(approver);

        // skill を最後に適用する（system 追記・few-shot・モデル既定。ピン順・モデル既定は
        // 指定フィールドのみ後勝ち・#344）。`allowed_tools` は誘導テキスト（apply_system 内）で
        // あり提示ツールは縮小しない（#344 の再定義。決定性はツール実装＋認可＋承認ゲート）。
        // ⚠️ opts.approval には触れない（破壊系の明示許可は skill で無効化できない・Task 6.9）。
        let (mut opts, mut history) = (opts, history);
        if !skills.is_empty() {
            let mut system = opts.system.take().unwrap_or_default();
            for skill in &skills {
                skill.apply_system(&mut system);
                skill.apply_model_defaults(&mut opts);
            }
            opts.system = Some(system);
            // few-shot は「ピン順に前から並ぶ」よう逆順で先頭 splice する。
            for skill in skills.iter().rev() {
                skill.apply_few_shot(&mut history);
            }
            for skill in &skills {
                skill.audit_apply(&self.db, ctx, run).await;
            }
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

    /// thread のワークスペース（Durable Workspace）を**遅延生成**で束ねる（#392）。
    ///
    /// フォルダは fs ツールが実際に呼ばれた時に初めて作られる（使わない run で空フォルダを
    /// 増やさない）。戻り値の `system` は「システム領域か」で、承認の事前許可判断に使う:
    ///
    /// - 既にフォルダがある → **その実際の `system` 属性**（過去 run の設定を勝手に変えない）
    /// - まだ無い → 「このフォルダで作業」の明示選択が**無ければ** system（自動生成の使い捨て領域）
    async fn lazy_workspace(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        storage: &Arc<storage::StorageService>,
    ) -> Result<(Arc<dyn WorkspaceStore>, bool), ChatError> {
        let system = match self
            .store
            .workspace_folder_id(thread_id, &ctx.tenant_id)
            .await?
        {
            Some(folder_id) => storage
                .is_system_node(ctx, folder_id)
                .await
                .inspect_err(|e| {
                    tracing::warn!(thread_id = %thread_id, error = %e,
                        "workspace の system 属性を読めなかった（可視領域として扱う）");
                })
                // 読めないときは**可視領域として扱う**（＝承認を緩めない安全側）。
                .unwrap_or(false),
            None => self
                .store
                .workspace_parent_folder_id(thread_id, &ctx.tenant_id)
                .await?
                .is_none(),
        };
        let workspace = Arc::new(crate::workspace::LazyWorkspace::new(
            self.store.clone(),
            storage.clone(),
            thread_id,
            system,
        ));
        Ok((workspace, system))
    }
}
