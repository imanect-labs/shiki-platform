//! ツール束の構築（generate.rs から分割）。配線状況に応じて提示ツールを組み立てる。
//!
//! 提示可否のポリシー（未配線なら提示しない・下書き系は無条件）はここに集約する。

use std::sync::Arc;

use agent_core::{
    AttachmentRef, CodeInterpreterTool, DocSearchTool, FsAppendTool, FsDeleteTool, FsEditTool,
    FsListTool, FsReadTool, FsWriteTool, GrepTool, ShellTool, Tool, WebFetchTool, WebSearchTool,
    WorkspaceStore,
};

use authz::AuthContext;

use super::ChatWorker;
use crate::store::ClaimedRun;

impl ChatWorker {
    /// 全プロファイル共通のツール（検索・コード実行・web・generative UI・ワークフロー）。
    ///
    /// `attachments` は会話の添付で、`code_interpreter` が実行前に `/workspace` へ置く（#379）。
    pub(super) fn push_core_tools(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        skills: &[crate::skill::AppliedSkill],
        attachments: Vec<AttachmentRef>,
    ) {
        if let Some(search) = &self.search {
            // skill の知識スコープを doc_search に反映する（Task 6.8・絞り込みのみ・
            // 複数ピンは全ピンが scope を持つ時のみ union・#344）。
            let scope = crate::skill::combined_scope(skills);
            tools.push(Arc::new(DocSearchTool::with_scope(search.clone(), scope)));
        }
        if let Some(sandbox) = &self.sandbox {
            let mut code = CodeInterpreterTool::new(
                sandbox.clone(),
                self.artifacts.clone(),
                self.config.sandbox_backend,
            );
            // 会話の添付を実行前に /workspace へ置く（#379）。storage が無い構成では
            // 従来どおり seed しない（description の注記どおり結果に理由が付く）。
            if let (Some(storage), false) = (&self.storage, attachments.is_empty()) {
                code = code.with_attachments(
                    Arc::new(crate::attachments::StorageAttachmentStore::new(
                        storage.clone(),
                    )),
                    attachments,
                );
            }
            tools.push(Arc::new(code));
        }
        if let Some(provider) = &self.web_search {
            tools.push(Arc::new(WebSearchTool::new(provider.clone())));
            // web_fetch はホスト側で取得する（#348）。sandbox 配線の有無に依存しない。
            tools.push(Arc::new(WebFetchTool::new()));
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
    }

    /// ドキュメント共同編集ツールの配線（ノート=Task 11P.4／スライド=Task 11.3）。
    ///
    /// collab ハブと storage が両方配線されている時のみ提示する。編集は共有 Yjs へ
    /// 適用され、権限は実行主体の editor@file（human と同一経路・昇格しない・排他なし）。
    pub(super) fn push_collab_tools(&self, tools: &mut Vec<Arc<dyn Tool>>) {
        // 下書きツールは保存も共同編集もしない（確定は UI 保存）ため、collab/storage の
        // 配線に依存させない（下書き生成フローを任意配線構成でも使えるようにする）。
        tools.push(Arc::new(crate::document_tool::SaveNoteTool::new()));
        tools.push(Arc::new(crate::slide_tool::SaveSlideTool::new()));
        // 下書き CSV（csv_draft・下書き確定型・Task 11.11・storage 非依存・確定は UI 保存）。
        tools.push(Arc::new(crate::csv_tool::SaveCsvTool::new()));
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

    /// AI Office 編集＋CSV ツールの配線。
    ///
    /// office.edit（ファイル単位・非ロック=新版/ロック中=提案・PIT-44・Task 11.8）は office
    /// 有効時のみ。office.live_edit（CoolWSD headless 参加・#352）は LiveEditor 配線時のみ。
    /// CSV（csv.query / csv.patch / csv.write・Task 11P.9）は tabular 配線時のみで、
    /// 認可は操作別のファイル ReBAC（TabularService が StorageService 経由で強制）。
    pub(super) fn push_office_and_csv_tools(&self, tools: &mut Vec<Arc<dyn Tool>>) {
        if let Some(office) = &self.office {
            tools.push(Arc::new(crate::office_tool::OfficeEditTool::new(
                office.clone(),
            )));
        }
        // office.live_edit は CoolWSD の headless 参加者として編集する（#352）。authz は
        // LiveEditor が内部で持つ（ツール側は昇格経路を持たない）。
        if let Some(live) = &self.office_live {
            tools.push(Arc::new(crate::office_live_tool::OfficeLiveEditTool::new(
                live.clone(),
            )));
        }
        // Office の**新規作成**（save_document / save_sheet・#381）。実体は「空テンプレを
        // 作成 → Collabora へ paste」なので Collabora が要る＝office_creator 配線時のみ提示する
        // （md 下書き画面は廃止。Collabora 無しで「Word を作る」導線だけ残す方が嘘になる）。
        if let Some(creator) = &self.office_creator {
            tools.push(Arc::new(crate::office_create_tool::SaveDocumentTool::new(
                creator.clone(),
            )));
            tools.push(Arc::new(crate::office_create_tool::SaveSheetTool::new(
                creator.clone(),
            )));
        }
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
    }

    /// 自律ツール（file CRUD/grep/shell）を tools へ追加する。
    pub(super) fn push_autonomous_tools(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        workspace: Arc<dyn WorkspaceStore>,
    ) {
        tools.push(Arc::new(FsListTool::new(workspace.clone())));
        tools.push(Arc::new(FsReadTool::new(workspace.clone())));
        tools.push(Arc::new(GrepTool::new(workspace.clone())));
        tools.push(Arc::new(FsWriteTool::new(workspace.clone())));
        // 追記（#392）: 証拠台帳のような append-only メモを全文再送なしに伸ばす。
        tools.push(Arc::new(FsAppendTool::new(workspace.clone())));
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

    /// 委譲ツール（`subagent`・#391）を提示ツールに加える（自律プロファイルのみ）。
    ///
    /// 子へ渡すのは**明示 allowlist の read-only ツールだけ**を、いま提示している中から拾う
    /// （配線されていないツールは子にも無い）。`subagent` 自身は渡さない＝**入れ子の入れ子が
    /// 構造的に起きない**。破壊系を渡さないので入れ子の承認問題も生じない。
    /// 委譲ツールを提示し、**子のツール実行を親の run のイベント列へ中継する**タスクを起こす。
    ///
    /// 中継しないと、調査を委譲した瞬間に画面から「何を調べているか」が消える（`subagent` の
    /// 行が数本出るだけになる）。中継先は `generation_event`＝イベント経路で、親の LLM
    /// コンテキストにも確定メッセージにも入らない（`SkillInvoked` と同じ扱い）。
    pub(super) fn push_subagent_tool(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        run: &ClaimedRun,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    ) {
        /// 子が使えるツール（調査に必要な読み取りだけ）。ここに破壊系を足さないこと。
        const CHILD_TOOLS: [agent_core::ToolName; 6] = [
            agent_core::ToolName::WebSearch,
            agent_core::ToolName::WebFetch,
            agent_core::ToolName::DocSearch,
            agent_core::ToolName::FsRead,
            agent_core::ToolName::Grep,
            agent_core::ToolName::FsList,
        ];
        let child: Vec<Arc<dyn Tool>> = tools
            .iter()
            .filter(|t| {
                agent_core::ToolName::parse(t.name()).is_some_and(|n| CHILD_TOOLS.contains(&n))
            })
            .map(Arc::clone)
            .collect();
        // 調査できる道具が 1 つも無ければ委譲は無意味（提示しない）。
        if child.is_empty() {
            return;
        }
        // 子のツール実行を UI へ中継する。イベント列への追記は `append_stream_event` が
        // 単調 seq で原子的に行うので、親の sink と並行に書いても順序は壊れない。
        // fencing 不一致（リース喪失）は Ok(None) が返るだけ＝ゾンビ書込にならない。
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<agent_core::AgentEvent>();
        let store = self.store.clone();
        let (run_id, fencing) = (run.run_id, run.fencing_token);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let kind = super::sink::to_stream_kind(&event);
                if store
                    .append_stream_event(run_id, fencing, &kind)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        tools.push(Arc::new(
            agent_core::SubagentTool::new(
                self.gateway.clone(),
                child,
                self.config.subagent,
                format!("{}:{}", run.run_id, run.fencing_token),
                cancel,
            )
            .with_model(self.config.model.clone())
            .with_tool_events(tx),
        ));
    }

    /// skill ツール（カタログ引き・#344 Task 10.11）を提示ツールに加える。
    ///
    /// artifact ストアとカタログ源が配線されている時のみ。カタログはピン済み ∪ 本人 owner
    /// （PR2 でインストール済みを追加）。掲載一覧の取得失敗は run を落とさない
    /// （ピンの fail-closed とは別・warn してツールを出さない）。
    pub(super) async fn push_skill_tool(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        ctx: &AuthContext,
        run: &ClaimedRun,
        skills: &[crate::skill::AppliedSkill],
    ) {
        let (Some(artifacts), Some(catalog)) = (&self.skill_artifacts, &self.skill_catalog) else {
            return;
        };
        match catalog.entries(ctx, run.trace_id.as_deref()).await {
            Ok(entries) => {
                let pinned = skills
                    .iter()
                    .map(|s| crate::skill_catalog::SkillCatalogEntry {
                        id: s.id,
                        version: s.version,
                        name: s.name.clone(),
                        description: s.body.description.clone(),
                        pinned: true,
                        command: s.body.command.clone(),
                    })
                    .collect();
                if let Some(tool) = crate::skill_tool::SkillTool::build(
                    artifacts.clone(),
                    self.db.clone(),
                    run,
                    pinned,
                    entries,
                ) {
                    tools.push(Arc::new(tool));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, run_id = %run.run_id, "skill カタログ取得に失敗（skill ツールを提示しない）");
            }
        }
    }
}
