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
use super::store::RunStore;
use crate::delegation::{DelegationStore, RunAdmission};
use crate::scheduler::RunLauncher;
use crate::store::WorkflowStore;

/// 委譲チェック付き run 起動。DelegationStore + WorkflowStore + RunStore を束ねる。
#[derive(Clone)]
pub struct WorkflowRunLauncher {
    delegation: DelegationStore,
    workflows: WorkflowStore,
    runs: RunStore,
    /// tenant → org の解決（run のスコープ最上位）。schedule/event では registration.org を使う。
    org_default: String,
}

impl WorkflowRunLauncher {
    pub fn new(
        delegation: DelegationStore,
        workflows: WorkflowStore,
        runs: RunStore,
        org_default: impl Into<String>,
    ) -> Self {
        WorkflowRunLauncher {
            delegation,
            workflows,
            runs,
            org_default: org_default.into(),
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
        let ir_json = serde_json::to_value(&ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(&ir);
        let run_id = self
            .runs
            .create_run(
                &ctx.tenant_id,
                &ctx.org,
                workflow_id,
                version,
                "interactive",
                &ctx.principal.id,
                input,
                &ir_json,
                &graph,
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        Ok(Some(run_id))
    }

    /// schedule/event の run を起動する（委譲チェック→workflow プリンシパルで create_run）。
    async fn launch_delegated(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
    ) -> Result<Option<Uuid>, LauncherError> {
        // workflow プリンシパルの AuthContext で IR を読む（enable 時に self-viewer 付与済み）。
        let wf_ctx = AuthContext::for_workflow(
            tenant_id.to_string(),
            self.org_default.clone(),
            &workflow_id.to_string(),
        );
        let (version, ir) = self
            .workflows
            .get_latest(&wf_ctx, workflow_id, None)
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
            RunAdmission::DelegationInvalid(_) => return Ok(None),
        }

        let ir_json = serde_json::to_value(&ir).map_err(|e| LauncherError::Ir(e.to_string()))?;
        let graph = RunGraph::build(&ir);
        let run_id = self
            .runs
            .create_run(
                tenant_id,
                &self.org_default,
                workflow_id,
                version,
                trigger_kind,
                &workflow_id.to_string(),
                &Value::Null,
                &ir_json,
                &graph,
            )
            .await
            .map_err(|e| LauncherError::Run(e.to_string()))?;
        Ok(Some(run_id))
    }
}

#[async_trait]
impl RunLauncher for WorkflowRunLauncher {
    async fn launch(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        _trigger_id: &str,
    ) -> Option<Uuid> {
        match self
            .launch_delegated(tenant_id, workflow_id, trigger_kind)
            .await
        {
            Ok(run_id) => run_id,
            Err(e) => {
                tracing::warn!(error = %e, tenant = tenant_id, "run 起動に失敗");
                None
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
}
