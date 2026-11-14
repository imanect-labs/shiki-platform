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
    /// 全プロファイルで提示するツール一式（自律専用のものは含まない）。積む順が提示の順になる。
    pub(super) async fn base_tools(
        &self,
        ctx: &AuthContext,
        run: &ClaimedRun,
        skills: &[crate::skill::AppliedSkill],
        attachments: &[AttachmentRef],
    ) -> Result<Vec<Arc<dyn Tool>>, crate::ChatError> {
        let mut tools = self.core_tools(skills, attachments, run).await?;
        // AI ワークフロー編集（説明文に発話ユーザーのモデル一覧を載せる・Task 10.13）と、ワークフローを探すツール。
        self.push_workflow_tools(&mut tools, ctx, run).await;
        // skill ツール（カタログ引き・#344 Task 10.11）。
        self.push_skill_tool(&mut tools, ctx, run, skills).await;
        // AI ドキュメント共同編集（ノート/スライド・Task 11P.4/11.3）＋下書き系＋Office 編集/CSV。
        self.push_collab_tools(&mut tools);
        self.push_office_and_csv_tools(&mut tools);
        // 構造化データ（発話ユーザーに見えるテーブルがあるときだけ・Task 15.7）。
        self.push_data_tools(&mut tools, ctx).await;
        // テーブルを作る・直す（見えるテーブルが 0 件でも出す・D-10 の T05）。
        self.push_data_builder_tools(&mut tools, run);
        Ok(tools)
    }

    /// 全プロファイル共通のツール（検索・コード実行・web・generative UI）。ワークフローは
    /// [`Self::push_workflow_tools`]（発話ユーザーのモデル一覧を引くため ctx が要る）。
    ///
    /// `attachments` は会話の添付で、`code_interpreter` が実行前に `/workspace` へ置く（#379）。
    ///
    /// doc_search / doc_read には skill の知識スコープと会話の検索フォルダを交差前のまま課す
    /// （[`Self::search_bounds`]・絞り込みのみ・#965）。
    pub(super) async fn core_tools(
        &self,
        skills: &[crate::skill::AppliedSkill],
        attachments: &[AttachmentRef],
        run: &ClaimedRun,
    ) -> Result<Vec<Arc<dyn Tool>>, crate::ChatError> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        if let Some(storage) = &self.storage {
            tools.push(Arc::new(
                crate::drive_copy_tool::DriveCopyTool::new(storage.clone(), run.run_id)
                    .with_collab(self.collab.clone()),
            ));
        }

        if let Some(search) = &self.search {
            let bounds = self.search_bounds(run, skills).await?;
            let folder = bounds.folder.clone().unwrap_or_default();
            // この回答のための検索・読み出しとして監査に残す（1 回答の判定と引用を引ける・#829）。
            let doc_search = DocSearchTool::with_scope(search.clone(), bounds.skill.clone())
                .within(folder.clone())
                .for_answer(run.thread_id, run.message_id);
            // 当たった文書を引用の外まで読む（#425）。認可は StorageService の viewer 判定で、
            // 知識スコープは検索と同じものを課す（スコープ ∩ ReBAC）。
            if let Some(storage) = &self.storage {
                tools.push(Arc::new(doc_search.with_doc_read()));
                tools.push(Arc::new(
                    crate::doc_read_tool::DocReadTool::with_scope(
                        search.clone(),
                        storage.clone(),
                        bounds.skill,
                    )
                    .within(folder)
                    .for_answer(run.thread_id, run.message_id),
                ));
            } else {
                tools.push(Arc::new(doc_search));
            }
        }
        if let Some(help) = &self.help_search {
            tools.push(Arc::new(agent_core::HelpSearchTool::new(help.clone())));
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
                    attachments.to_vec(),
                );
            }
            tools.push(Arc::new(code));
        }
        if let Some(provider) = &self.web_search {
            tools.push(Arc::new(WebSearchTool::new(provider.clone())));
            // web_fetch はホスト側で取得する（#348）。sandbox 配線の有無に依存しない。
            // parser があれば PDF/Office も読める（#405）。取得は web_fetch のガード済み経路が行い、
            // worker へはバイト列だけ渡す（URL を渡すと宛先制限を迂回される）。
            // 外へ出るのに中継が要る環境では、運用者が設定した中継だけを通す（GAP-72）。
            tools.push(Arc::new(web_fetch_tool(&self.config, self.parser.as_ref())));
        }
        // ```ui の種類ごとの書き方（早見は system にある・ui_reference で書く前に読む）。
        // 外部への入出力も認可も無い読み取りなので、図を書かせる構成なら常に提示する。
        if self.config.yui {
            tools.push(Arc::new(super::yui_reference::UiReferenceTool));
        }
        Ok(tools)
    }

    /// AI ワークフロー編集（emit_workflow / read_workflow・Task 10.13）と、既存のワークフローを探す
    /// list_workflows（GAP-31）・search_workflows（図のボタンに実在する名前を書く）。
    ///
    /// emit / read / list はストアとカタログ源（保存 API と同一実装）が両方配線されている時のみ提示する。emit_workflow の
    /// 説明文には発話ユーザーの組織で使えるモデルと判定用のモデルを載せる（保存時の照合と同じ集合）。引けなければ
    /// run を落とさず、モデルを載せない説明文にする（候補は検証エラーで返る）。
    ///
    /// 作成の冪等（R1・T02）: 名前が重なったら、本人がこの run の作られた時刻（再試行しても変わらない）以降に作った
    /// 同名と内容を比べる。引くストアは artifact の単一チョークポイント（skill と同じ `ArtifactStore`）。
    pub(super) async fn push_workflow_tools(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        ctx: &AuthContext,
        run: &ClaimedRun,
    ) {
        if let (Some(store), Some(catalog)) = (&self.workflow_store, &self.workflow_catalog) {
            let mut emit =
                crate::workflow_tool::EmitWorkflowTool::new(store.clone(), catalog.clone());
            if let Some(artifacts) = &self.skill_artifacts {
                emit = emit.with_created_since(Arc::clone(artifacts), run.created_at);
            }
            match catalog.models(ctx).await {
                Ok(models) => emit = emit.with_models(&models),
                Err(e) => {
                    tracing::warn!(error = %e, "モデル一覧の取得に失敗（emit_workflow の説明文に載せない）");
                }
            }
            // 省いたときに動く既定がこのユーザーに使えなければ、省略を勧めずに model を書かせる（D-185）。
            match catalog.default_model_unavailable(ctx).await {
                Ok(model) => emit = emit.with_default_model_unavailable(model),
                Err(e) => {
                    tracing::warn!(error = %e, "組織の既定のモデルの確かめに失敗（emit_workflow の説明文に載せない）");
                }
            }
            // 判定用のモデル（ai.decide）も id と表示名を載せる（保存時の誤りの候補で知る往復を減らす・D-79）。
            match catalog.decision_models(ctx).await {
                Ok(models) => emit = emit.with_decision_models(&models),
                Err(e) => {
                    tracing::warn!(error = %e, "判定用のモデルの一覧の取得に失敗（emit_workflow の説明文に載せない）");
                }
            }
            tools.push(Arc::new(emit));
            tools.push(Arc::new(crate::workflow_tool::ReadWorkflowTool::new(
                store.clone(),
            )));
            // 既存のワークフローを名前で探す（新しい会話から直す・本人に見えるものだけ・GAP-31）。
            // 持ち主のものでも、最新の版をほかの人が保存していれば表示名・説明を封筒に入れる（PIT-56・T01 の残り）。
            tools.push(Arc::new(
                crate::workflow_tool::ListWorkflowsTool::new(
                    store.clone(),
                    workflow_engine::WorkflowSummaryStore::new(self.db.clone()),
                )
                .with_version_authors(self.db.clone()),
            ));
        }
        // 本人に見えるワークフローを探す（search_workflows・読み取りのみ）。図のボタンに実在する
        // 名前を書かせる。認可は注入した実装が一覧 API と同じ所有∪共有の経路で行う。
        if let Some(directory) = &self.workflow_directory {
            tools.push(Arc::new(
                crate::workflow_search_tool::SearchWorkflowsTool::new(directory.clone()),
            ));
        }
    }

    /// ページ翻訳（`web_translate`・docs/agent/web-translate.md）を提示ツールに加える。
    ///
    /// `web_fetch` と同じ条件（web 検索が配線されている構成）でだけ提示し、取得は `web_fetch` と
    /// 同じ設定の取得経路を使う。訳は llm-gateway 経由で `model`（会話で決まったモデル）を使い、
    /// run の停止で残りのチャンクを訳さない。計画承認前の段階（門）では調査系と同じく渡さない。
    pub(super) fn push_translate_tool(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        run: &ClaimedRun,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
        model: Option<String>,
        gate_stage: super::gate::GateStage,
    ) {
        if self.web_search.is_none() || gate_stage != super::gate::GateStage::Execute {
            return;
        }
        // 取得は web_fetch と同じ組み立て（中継・文書パーサ・BUG-274）。
        tools.push(Arc::new(
            agent_core::WebTranslateTool::new(
                web_fetch_tool(&self.config, self.parser.as_ref()),
                self.gateway.clone(),
                format!("{}:{}", run.run_id, run.fencing_token),
                Arc::clone(cancel),
            )
            .with_model(model)
            .with_session_id(Some(run.thread_id.to_string())),
        ));
    }

    /// 試運転（`try_workflow`・T04・D-197）を提示ツールに加える。
    ///
    /// ページ翻訳と同じく実行の段階（[`super::gate::GateStage::Execute`]）でだけ提示する（計画の承認の前の門は
    /// 研究系以外を通すため・`base_tools` には入れない）。起動の入口・run の要約・ワークフローのストアのどれかが
    /// 無ければ（`workflow.enabled=false` など）提示しない。委譲の子には渡らない（`CHILD_TOOLS` は閉じた一覧）。
    pub(super) fn push_try_tool(
        &self,
        tools: &mut Vec<Arc<dyn Tool>>,
        run: &ClaimedRun,
        slot: &Arc<super::slot::RunSlot>,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
        gate_stage: super::gate::GateStage,
    ) {
        if gate_stage != super::gate::GateStage::Execute {
            return;
        }
        let (Some(launcher), Some(runs), Some(workflows)) =
            (&self.try_launcher, &self.try_runs, &self.workflow_store)
        else {
            return;
        };
        tools.push(Arc::new(crate::workflow_try_tool::TryWorkflowTool::new(
            crate::workflow_try_tool::TryWorkflowDeps {
                workflows: Arc::clone(workflows),
                launcher: Arc::clone(launcher),
                runs: Arc::clone(runs),
                chat: self.store.clone(),
                db: self.db.clone(),
            },
            crate::workflow_try_tool::TryScope {
                chat_run_id: run.run_id,
                thread_id: run.thread_id,
                chat_trace_id: run.trace_id.clone(),
                slot: Arc::clone(slot),
                cancel: Arc::clone(cancel),
                wait: self.config.try_wait,
            },
        )));
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
        // 下書きスキル（skill_draft・下書き確定型・#919・保存はスキルエディタで人が行う）。
        tools.push(Arc::new(crate::skill_draft_tool::SaveSkillTool::new()));
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
            collab.clone(), // 本文への ```ui の図の追記（非破壊 append・確認不要・#282・#604）。
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
        // Excel・Word のレイアウト編集（office.read / office.excel_edit / office.word_edit・#725）。kit 内の固定エンジンへ
        // 検証済みの JSON だけを渡す（PIT-68）。authz は OfficeEngine が内部で持つ。
        if let Some(engine) = &self.office_engine {
            tools.push(Arc::new(crate::office_engine_tool::OfficeReadTool::new(
                engine.clone(),
            )));
            tools.push(Arc::new(
                crate::office_engine_tool::OfficeExcelEditTool::new(engine.clone()),
            ));
            tools.push(Arc::new(
                crate::office_engine_tool::OfficeWordEditTool::new(engine.clone()),
            ));
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
        // ドライブのファイル・フォルダを探す／移動する（種別を問わない・#942）。認可は
        // StorageService が発話ユーザーの権限で行う（移動は承認ゲート対象）。
        if let Some(storage) = &self.storage {
            let drive: Arc<dyn crate::drive_tool::DriveOps> = storage.clone();
            tools.push(Arc::new(crate::drive_tool::DriveFindTool::new(
                drive.clone(),
            )));
            tools.push(Arc::new(crate::drive_tool::DriveMoveTool::new(drive)));
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

    /// 構造化データのツール（data_query / data_schema / data_record_write・Task 15.7）。
    ///
    /// DataStore が配線され、**発話ユーザーに見えるテーブルが 1 つ以上ある**ときだけ提示する
    /// （見えるテーブルが無ければ使い道が無い・description の一覧は有界。一覧が上限で切れて 0 件のときも提示する）。一覧の取得失敗は run を
    /// 落とさない（warn してツールを出さない）。認可は各呼び出しで data が本人の権限で毎回判定する。
    pub(super) async fn push_data_tools(&self, tools: &mut Vec<Arc<dyn Tool>>, ctx: &AuthContext) {
        use crate::data_tool::{
            DataQueryTool, DataRecordWriteTool, DataSchemaTool, DataTableCatalog,
        };
        let Some(data) = &self.data else {
            return;
        };
        let catalog = match DataTableCatalog::load(data, ctx).await {
            Ok(c) if !c.is_empty() => Arc::new(c),
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "テーブル一覧の取得に失敗（data ツールを提示しない）");
                return;
            }
        };
        tools.push(Arc::new(DataQueryTool::new(
            data.clone(),
            Arc::clone(&catalog),
        )));
        tools.push(Arc::new(DataSchemaTool::new(
            data.clone(),
            Arc::clone(&catalog),
        )));
        tools.push(Arc::new(DataRecordWriteTool::new(data.clone(), catalog)));
    }

    /// テーブルと保存ビューを作る・直す道具（data_table_create / data_table_update・T05、data_view_save・T06）。
    ///
    /// DataStore と FsmStore が配線されていれば、見えるテーブルが 0 件でも提示する（作る道具なので）。どちらも
    /// 遅延ロード（tool_search で読み込むまで定義を載せない）。委譲の子には渡らない（`CHILD_TOOLS` は閉じた一覧）。
    pub(super) fn push_data_builder_tools(&self, tools: &mut Vec<Arc<dyn Tool>>, run: &ClaimedRun) {
        let (Some(data), Some(fsms)) = (&self.data, &self.fsms) else {
            return;
        };
        let deps = crate::data_builder_tool::DataBuilderDeps {
            data: data.clone(),
            fsms: Arc::clone(fsms),
            approvals: Arc::new(workflow_engine::ApprovalStore::new(self.db.clone())),
        };
        let builder_run = crate::data_builder_tool::BuilderRun {
            since: run.created_at,
        };
        tools.push(Arc::new(
            crate::data_builder_tool::DataTableCreateTool::new(deps.clone(), builder_run),
        ));
        tools.push(Arc::new(
            crate::data_builder_tool::DataTableUpdateTool::new(deps),
        ));
        // 保存ビュー（T06）。作成の冪等の照合と作った数は artifact の単一チョークポイントで引く。
        if let (Some(views), Some(artifacts)) = (&self.data_views, &self.skill_artifacts) {
            tools.push(Arc::new(crate::data_builder_tool::DataViewSaveTool::new(
                crate::data_builder_tool::ViewBuilderDeps {
                    data: data.clone(),
                    views: Arc::clone(views),
                    artifacts: Arc::clone(artifacts),
                },
                builder_run,
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
        // 引用の実在照合（#409）: レポートの引用が証拠台帳にあるかを機械で確かめる。
        tools.push(Arc::new(agent_core::QuoteCheckTool::new(workspace.clone())));
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
        const CHILD_TOOLS: [agent_core::ToolName; 9] = [
            agent_core::ToolName::WebSearch,
            agent_core::ToolName::WebFetch,
            agent_core::ToolName::DocSearch,
            agent_core::ToolName::HelpSearch,
            agent_core::ToolName::DocRead,
            agent_core::ToolName::FsRead,
            // 検証ロールが最初に引用の実在を機械で確かめる（#409）。
            agent_core::ToolName::QuoteCheck,
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
                let kind = super::stream_map::to_child_stream_kind(&event);
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
            .with_session_id(Some(run.thread_id.to_string()))
            .with_tool_events(tx)
            // 子は親の system を引き継がないので、今日の日付は明示して渡す（#546）。
            .with_system_note(super::opts::today_note(
                run.created_at,
                self.config.timezone,
            )),
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
                let entries = crate::skill_catalog::merge_entries(pinned, entries);
                // 一覧の上限を超えたら検索も足す（載らない分を探せるように・#520）。
                if let Some(search) = crate::skill_search_tool::SkillSearchTool::build(
                    &entries,
                    self.tool_search_embedder(),
                ) {
                    search.prewarm(ctx);
                    tools.push(Arc::new(search));
                }
                if let Some(tool) = crate::skill_tool::SkillTool::build(
                    artifacts.clone(),
                    self.db.clone(),
                    run,
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

/// `web_fetch` と `web_translate` が共有する取得の道具（運用者が設定した外への送信の中継・文書パーサ）。
///
/// 両方の道具が同じ組み立てを通る（片方だけ中継を付け忘れると、中継でしか外へ出られない環境では取得に失敗し、
/// 直接出られる環境では監査の中継を迂回する・GAP-72・BUG-274）。
pub(super) fn web_fetch_tool(
    config: &super::WorkerConfig,
    parser: Option<&Arc<dyn rag::DocumentParser>>,
) -> WebFetchTool {
    configure_fetch(WebFetchTool::new(), config, parser)
}

/// 取得の道具に、中継と文書パーサを付ける（[`web_fetch_tool`] の中身・テストは名前の引き方を差し替えて通す）。
fn configure_fetch(
    fetch: WebFetchTool,
    config: &super::WorkerConfig,
    parser: Option<&Arc<dyn rag::DocumentParser>>,
) -> WebFetchTool {
    let mut fetch = fetch.with_proxy(config.egress_proxy.clone());
    if let Some(parser) = parser {
        fetch = fetch.with_parser(parser.clone());
    }
    fetch
}

#[cfg(test)]
#[path = "toolset_tests.rs"]
mod tests;
