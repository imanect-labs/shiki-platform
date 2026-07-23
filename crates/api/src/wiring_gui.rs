//! generative UI／skill／ミニアプリ（Phase 6）の依存配線。
//!
//! 検証層・各ストア・宣言的 UI アクションの実行系（Task 6.3/6.5/6.7/6.10）を束ねる。
//! `wiring.rs` から切り出し（1 ファイル 500 行規約）。

use std::sync::Arc;

use api::config::AppConfig;

use crate::wiring_websearch::wire_websearch;

/// generative UI／skill／ミニアプリのストア束（Phase 6 の配線結果）。
pub(crate) struct GuiWiring {
    pub(crate) validator: Arc<gui::SpecValidator>,
    pub(crate) ui_specs: Arc<gui::UiSpecStore>,
    pub(crate) skills: Arc<gui::SkillStore>,
    pub(crate) mini_apps: Arc<gui::MiniAppStore>,
}

/// generative UI の検証層＋各ストアを配線する（Task 6.3/6.7/6.10）。
///
/// 検証は保存（UiSpecStore）・発話（emit_ui）・解決（ミニアプリ）の全経路が同一実装を共有する。
pub(crate) fn wire_gui(db: &sqlx::PgPool, artifacts: &Arc<artifact::ArtifactStore>) -> GuiWiring {
    let validator = Arc::new(gui::SpecValidator::new(Arc::clone(artifacts), db.clone()));
    let ui_specs = Arc::new(gui::UiSpecStore::new(
        Arc::clone(artifacts),
        Arc::clone(&validator),
    ));
    let skills = Arc::new(gui::SkillStore::new(Arc::clone(artifacts)));
    let mini_apps = Arc::new(gui::MiniAppStore::new(Arc::clone(artifacts), db.clone()));
    GuiWiring {
        validator,
        ui_specs,
        skills,
        mini_apps,
    }
}

/// workflow-engine 対話トリガの UI アクション向けアダプタ（Task 6.5 の③）。
///
/// 検証時にピンした版で起動する。認可は launcher 側（本人 viewer で IR 取得・実行時は
/// scope_ceiling ∩ 本人 ReBAC）に委ねる。IR 取得失敗（不在/権限なし）は存在秘匿で NotFound。
struct LauncherWorkflowStarter(Arc<workflow_engine::WorkflowRunLauncher>);

#[async_trait::async_trait]
impl gui::WorkflowStarter for LauncherWorkflowStarter {
    async fn start_pinned(
        &self,
        ctx: &authz::AuthContext,
        workflow_id: uuid::Uuid,
        version: i64,
        input: &serde_json::Value,
    ) -> Result<Option<uuid::Uuid>, gui::ActionError> {
        self.0
            .start_interactive_version(ctx, workflow_id, version, input)
            .await
            .map_err(|e| match e {
                workflow_engine::run::LauncherError::Ir(_) => gui::ActionError::NotFound,
                other => gui::ActionError::Internal(format!("run 起動: {other}")),
            })
    }

    async fn start_pinned_via_bundle(
        &self,
        ctx: &authz::AuthContext,
        bundle_id: uuid::Uuid,
        workflow_id: uuid::Uuid,
        version: i64,
        input: &serde_json::Value,
    ) -> Result<Option<uuid::Uuid>, gui::ActionError> {
        self.0
            .start_interactive_via_bundle(ctx, bundle_id, workflow_id, version, input)
            .await
            .map_err(|e| match e {
                workflow_engine::run::LauncherError::Ir(_) => gui::ActionError::NotFound,
                other => gui::ActionError::Internal(format!("run 起動: {other}")),
            })
    }
}

/// 宣言的 UI アクションの実行系を配線する（Task 6.5）。
///
/// 利用可能な束縛先（chat.submit ハンドラ・安全ツール・workflow 起動）だけを登録する。
/// 未登録の束縛はディスパッチ時に 503（Unavailable）で fail-closed。
pub(crate) fn wire_ui_actions(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    chat: Option<&Arc<chat::ChatStore>>,
    search: Option<&Arc<rag::SearchService>>,
    workflow_launcher: Option<&Arc<workflow_engine::WorkflowRunLauncher>>,
) -> anyhow::Result<Arc<gui::ActionDispatcher>> {
    let mut dispatcher = gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(db.clone()));
    if let Some(chat) = chat {
        dispatcher.register_handler(Arc::new(chat::ChatSubmitHandler::new((**chat).clone())));
    }
    if let Some(search) = search {
        dispatcher.register_tool(
            agent_core::ToolName::DocSearch,
            Arc::new(agent_core::DocSearchTool::new(Arc::clone(search))),
        );
    }
    if let Some(provider) = wire_websearch(config, http)? {
        dispatcher.register_tool(
            agent_core::ToolName::WebSearch,
            Arc::new(agent_core::WebSearchTool::new(provider)),
        );
    }
    if let Some(launcher) = workflow_launcher {
        dispatcher.set_workflow_starter(Arc::new(LauncherWorkflowStarter(Arc::clone(launcher))));
    }
    Ok(Arc::new(dispatcher))
}
