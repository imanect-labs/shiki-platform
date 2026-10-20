//! skill／ミニアプリと図のボタン（Phase 6・#604）の依存配線。
//!
//! 各ストア・ワークフロー名の解決・図のボタンの実行系（Task 6.5/6.7/6.10）を束ねる。
//! `wiring.rs` から切り出し（1 ファイル行数規約）。

use std::sync::Arc;

/// skill／ミニアプリのストア束（Phase 6 の配線結果）。
pub(crate) struct GuiWiring {
    /// 図のボタンの `-> workflow 名前` を版に固定する（会話の回答の確定時）。
    pub(crate) workflow_resolver: Arc<gui::WorkflowResolver>,
    pub(crate) skills: Arc<gui::SkillStore>,
    pub(crate) mini_apps: Arc<gui::MiniAppStore>,
}

/// 各ストアとワークフロー名の解決を配線する（Task 6.7/6.10・#604）。
pub(crate) fn wire_gui(db: &sqlx::PgPool, artifacts: &Arc<artifact::ArtifactStore>) -> GuiWiring {
    let workflow_resolver = Arc::new(gui::WorkflowResolver::new(Arc::clone(artifacts)));
    let skills = Arc::new(gui::SkillStore::new(Arc::clone(artifacts)));
    let mini_apps = Arc::new(gui::MiniAppStore::new(Arc::clone(artifacts), db.clone()));
    GuiWiring {
        workflow_resolver,
        skills,
        mini_apps,
    }
}

/// workflow-engine 対話トリガの図のボタン向けアダプタ（Task 6.5）。
///
/// 回答の確定時・ミニアプリの保存時にピンした版で起動する。認可は launcher 側（本人 viewer で IR 取得・実行時は
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
                workflow_engine::run::LauncherError::Input(m) => gui::ActionError::Invalid(m),
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
                workflow_engine::run::LauncherError::Input(m) => gui::ActionError::Invalid(m),
                other => gui::ActionError::Internal(format!("run 起動: {other}")),
            })
    }
}

/// 図のボタンの実行系を配線する（Task 6.5・#604）。
///
/// 利用可能な束縛先（chat.submit ハンドラ・workflow 起動）だけを登録する。
/// 未登録の束縛はディスパッチ時に 503（Unavailable）で fail-closed。
pub(crate) fn wire_ui_actions(
    db: &sqlx::PgPool,
    chat: Option<&Arc<chat::ChatStore>>,
    workflow_launcher: Option<&Arc<workflow_engine::WorkflowRunLauncher>>,
) -> Arc<gui::ActionDispatcher> {
    let mut dispatcher = gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(db.clone()));
    if let Some(chat) = chat {
        dispatcher.register_handler(Arc::new(chat::ChatSubmitHandler::new((**chat).clone())));
        // 単発アクション（chat.submit）の実行台帳。ハンドラと**必ず対で**配線する
        // （台帳が無いと二重送信を抑止できないので、その場合は実行しない・#410）。
        dispatcher.set_ledger(Arc::new(chat::ChatActionLedger::new((**chat).clone())));
    }
    if let Some(launcher) = workflow_launcher {
        dispatcher.set_workflow_starter(Arc::new(LauncherWorkflowStarter(Arc::clone(launcher))));
    }
    Arc::new(dispatcher)
}
