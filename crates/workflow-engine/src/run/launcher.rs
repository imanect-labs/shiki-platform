//! 具体 `RunLauncher`（委譲チェック＋IR 取得＋run 作成を束ねる・Task 10.4a/10.3 結線・engine.md §6.2）。
//!
//! schedule/event トリガの発火（[`scheduler`](crate::scheduler)）と interactive 起動 API から呼ばれる。
//! run 開始前に **委譲チェック**（registration enabled・委譲有効・declared ⊆ consented）を通し、
//! 不成立なら run を作らない（fail-closed）。schedule/event は workflow プリンシパルで実行する。

use std::sync::Arc;

use async_trait::async_trait;
use authz::AuthContext;
use serde_json::Value;
use uuid::Uuid;

use super::graph::RunGraph;
use super::store::{RunOrigin, RunStore};
use crate::delegation::{DelegationStore, RunAdmission};
use crate::scheduler::{LaunchOutcome, RunLauncher, SkipReason};
use crate::store::WorkflowStore;
use crate::WorkflowIr;

/// 委譲チェック付き run 起動。DelegationStore + WorkflowStore + RunStore を束ねる。
///
/// schedule/event では run の org は **registration.org** を使う（enable 時に固定した org）。
#[derive(Clone)]
pub struct WorkflowRunLauncher {
    delegation: DelegationStore,
    workflows: WorkflowStore,
    runs: RunStore,
}

impl WorkflowRunLauncher {
    pub fn new(delegation: DelegationStore, workflows: WorkflowStore, runs: RunStore) -> Self {
        WorkflowRunLauncher {
            delegation,
            workflows,
            runs,
        }
    }

    /// interactive 起動（本人の AuthContext で・委譲チェックは対話では不要＝本人権限で実行）。
    ///
    /// 本人が読める IR を本人権限で実行するため confused-deputy にならない（engine.md §6.1）。
    pub async fn start_interactive(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        input: &Value,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_latest(ctx, workflow_id, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, None)
            .await
    }

    /// run の中から子のワークフローを起動する（workflow.start・`Shiki.workflow.*`）。
    ///
    /// 権限・入力の検査は [`Self::start_interactive`] と同じ。子の run は親の run の因果の鎖
    /// （親の鎖 ∪ {親のワークフロー}）を引き継ぐ。子の書込が起こすイベントで親（祖先）のトリガが
    /// 起きる循環を、子が別の run でも見つけられるようにする（D-26）。
    pub async fn start_child(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        input: &Value,
        parent_run_id: Option<Uuid>,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_latest(ctx, workflow_id, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, parent_run_id)
            .await
    }

    /// interactive 系共通の run 作成（IR 取得＝認可は呼び出し側で済んでいる前提）。
    async fn create_interactive_run(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        ir: &WorkflowIr,
        input: &Value,
        parent_run_id: Option<Uuid>,
    ) -> Result<Option<Uuid>, LauncherError> {
        // 入力の契約（input_schema）は run を作る前に必ず確かめる。対話起動の全経路
        // （API・会話の図のボタン・ミニアプリ）がここを通るので、画面の検証に頼らない。
        // 保存は通しても空のまま動かせない欄（必須の文字の欄が空・D-43）は、入力より先に断る。
        let blockers = crate::run_blockers(ir);
        if !blockers.is_empty() {
            return Err(LauncherError::NotReady(blockers));
        }
        if let Some(schema) = &ir.input_schema {
            crate::ir::input_schema::validate_input(schema, input)
                .map_err(|errors| LauncherError::Input(errors.join("\n")))?;
        }
        let ir_json = serde_json::to_value(ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(ir);
        // principal_kind は呼び出し主体の種別に従う（ユーザー起動＝user・workflow.start＝workflow）。
        let principal_kind = match ctx.principal.kind {
            authz::PrincipalKind::Workflow => "workflow",
            authz::PrincipalKind::User => "user",
            // ミニアプリ・サービス identity 起動（B2 自動化・Task 9.6）。
            authz::PrincipalKind::MiniApp => "miniapp",
        };
        let run_id = self
            .runs
            .create_run_from(
                &ctx.tenant_id,
                &ctx.org,
                workflow_id,
                version,
                "interactive",
                None,
                &ctx.principal.id,
                principal_kind,
                input,
                &ir_json,
                &graph,
                RunOrigin {
                    trigger_event_id: None,
                    parent_run_id,
                },
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        // None = policies.on_trigger_overflow=skip による多重度あふれ（run 不作成・正直に None）。
        Ok(run_id)
    }

    /// interactive 起動の**バージョンピン版**（generative UI / ミニアプリのアクション束縛・Task 6.5）。
    ///
    /// 検証時にピンした版を実行する（再現性）。認可は [`Self::start_interactive`] と同じく
    /// 本人の viewer 権限で IR を取得し、ノード実行時は scope_ceiling ∩ 本人 ReBAC の二重ゲート。
    pub async fn start_interactive_version(
        &self,
        ctx: &AuthContext,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_version(ctx, workflow_id, version, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, None)
            .await
    }

    /// interactive 起動の**バンドル権限版**（ミニアプリのワークフロー束縛・Task 6.10）。
    ///
    /// ミニアプリ本体（バンドル）だけを共有された利用者でも、部品 workflow を個別共有される
    /// ことなくピン版を起動できる。IR の読取のみバンドル viewer で認可し
    /// （`artifact.read_via_bundle` 監査）、**実行主体は押した本人のまま** — run 内の
    /// データアクセスは本人 ReBAC ∩ 宣言スコープ ∩ ノード設定の二重ゲート（engine.md §6.1）。
    pub async fn start_interactive_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        workflow_id: Uuid,
        version: i64,
        input: &Value,
    ) -> Result<Option<Uuid>, LauncherError> {
        let (version, ir) = self
            .workflows
            .get_version_via_bundle(ctx, bundle_id, workflow_id, version, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;
        self.create_interactive_run(ctx, workflow_id, version, &ir, input, None)
            .await
    }

    /// schedule/event の run を起動する（委譲チェック→workflow プリンシパルで create_run）。
    ///
    /// `Err` は一時的な失敗（IR・DB・OpenFGA）。起動しないと決まったものは `Ok(Skipped)`。
    async fn launch_delegated(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
        event_id: Option<i64>,
    ) -> Result<LaunchOutcome, LauncherError> {
        // registration から **有効化バージョンと org** を取る（enable 時に固定した版・org で実行する）。
        // 未登録/未有効化なら run を作らない。
        let Some((org, enabled_version)) = self
            .delegation
            .registration_info(tenant_id, workflow_id)
            .await
            .map_err(|e| LauncherError::Delegation(format!("{e:?}")))?
        else {
            return Ok(LaunchOutcome::Skipped(SkipReason::NotEnabled));
        };

        // workflow プリンシパルの AuthContext（registration の org）で **enabled_version の IR** を読む。
        // 最新版でなく有効化した版を実行し、未同意の新版が schedule/event で走るのを防ぐ。
        let wf_ctx =
            AuthContext::for_workflow(tenant_id.to_string(), org.clone(), &workflow_id.to_string());
        let (version, ir) = self
            .workflows
            .get_version(&wf_ctx, workflow_id, enabled_version, None)
            .await
            .map_err(|e| LauncherError::Ir(format!("{e:?}")))?;

        // 委譲チェック（fail-closed・不成立なら run を作らない）。
        let declared = ir.declared_scopes.clone();
        match self
            .delegation
            .check_run_start(tenant_id, workflow_id, &declared)
            .await
            .map_err(|e| LauncherError::Delegation(format!("{e:?}")))?
        {
            RunAdmission::Ok => {}
            RunAdmission::DelegationInvalid(reason) => {
                // 委譲失効・未同意スコープ・未登録などの切り分けができるよう理由を残す。
                tracing::warn!(
                    tenant_id,
                    %workflow_id,
                    reason,
                    "委譲チェック不成立のため run を開始しません"
                );
                return Ok(LaunchOutcome::Skipped(SkipReason::DelegationInvalid(
                    reason.clone(),
                )));
            }
        }

        let ir_json = serde_json::to_value(&ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(&ir);
        // event ペイロードを run 入力に載せる（$from trigger/$from input で参照可能・schedule は Null）。
        let run_id = self
            .runs
            .create_run_from(
                tenant_id,
                &org,
                workflow_id,
                version,
                trigger_kind,
                Some(trigger_id),
                &workflow_id.to_string(),
                "workflow",
                payload,
                &ir_json,
                &graph,
                RunOrigin {
                    trigger_event_id: event_id,
                    parent_run_id: None,
                },
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        // None = policies.on_trigger_overflow=skip による多重度あふれ（再試行しても同じ）。
        Ok(run_id.map_or(
            LaunchOutcome::Skipped(SkipReason::Overflow),
            LaunchOutcome::Started,
        ))
    }
}

#[async_trait]
impl RunLauncher for WorkflowRunLauncher {
    async fn launch(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
        event_id: Option<i64>,
    ) -> LaunchOutcome {
        match self
            .launch_delegated(
                tenant_id,
                workflow_id,
                trigger_kind,
                trigger_id,
                payload,
                event_id,
            )
            .await
        {
            Ok(outcome) => outcome,
            // 一時障害（IR 取得/DB/OpenFGA）。イベントの発火は呼び出し側が再試行する（trigger_firing の
            // pending・D-21）。委譲の不成立・多重度あふれは Skipped で返り、再試行しない。
            Err(e) => {
                tracing::warn!(error = %e, tenant = tenant_id, "run 起動に失敗");
                LaunchOutcome::Failed(e.to_string())
            }
        }
    }
}

/// Wrapper を `Arc<dyn RunLauncher>` として scheduler へ渡すためのヘルパ。
#[must_use]
pub fn into_dyn(launcher: WorkflowRunLauncher) -> Arc<dyn RunLauncher> {
    Arc::new(launcher)
}

#[derive(Debug, thiserror::Error)]
pub enum LauncherError {
    #[error("IR 取得エラー: {0}")]
    Ir(String),
    #[error("委譲チェックエラー: {0}")]
    Delegation(String),
    #[error("run 作成エラー: {0}")]
    Run(String),
    /// 入力が `input_schema` に合わない（呼び出し側は 400 に写す）。
    #[error("入力が正しくありません: {0}")]
    Input(String),
    /// IR が実行できる状態にない（必須の文字の欄が空・D-43）。理由は [`crate::run_blockers`] の全件
    /// （呼び出し側は 400 に写し、欄に写せるよう node_id・path を保つ）。
    #[error("実行できない設定があります: {}", LauncherError::messages(.0))]
    NotReady(Vec<crate::ValidationError>),
}

impl LauncherError {
    /// 理由の文言を 1 つにまとめる（構造を持てない呼び出し側・ログ用）。
    #[must_use]
    pub fn messages(errors: &[crate::ValidationError]) -> String {
        errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
