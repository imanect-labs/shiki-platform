//! ワークフローワーカー（ready step を claim → 実行 → 前進・engine.md §4）。
//!
//! 複数インスタンス・複数タスクで同時に走り、`FOR UPDATE SKIP LOCKED` で step を分け合う。
//! リース失効した running step は別ワーカーが再 claim して未完のみ再実行する（at-least-once・
//! terminal step は checkpoint が正のため再実行しない）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::graph::RunGraph;
use super::store::{ClaimedStep, RunStore};
use super::{NodeContext, NodeExecutor, NodeResult};
use crate::ir::WorkflowIr;

/// ワーカー設定（数値は engine 初期値・運用で調整）。
#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    /// リース期間（秒）。
    pub lease_secs: i64,
    /// claim が空振りしたときのポーリング間隔。
    pub idle_poll: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        WorkerConfig {
            lease_secs: 30,
            idle_poll: Duration::from_millis(200),
        }
    }
}

/// ワーカー。`spawn` で並行タスクを起動する（各タスクが claim ループを回す）。
#[derive(Clone)]
pub struct WorkflowWorker {
    store: RunStore,
    executor: Arc<dyn NodeExecutor>,
    config: WorkerConfig,
    /// テナントシャーディング（None=全テナント横断・Some=そのテナントのみ）。
    tenant_scope: Option<String>,
}

impl WorkflowWorker {
    pub fn new(store: RunStore, executor: Arc<dyn NodeExecutor>, config: WorkerConfig) -> Self {
        WorkflowWorker {
            store,
            executor,
            config,
            tenant_scope: None,
        }
    }

    /// 特定テナントの step のみ処理するワーカーにする（tenant シャーディング・テスト分離）。
    #[must_use]
    pub fn scoped_to_tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_scope = Some(tenant_id.into());
        self
    }

    /// 並行ワーカータスクを起動する（プロセス生存中は走り続ける・detach）。
    pub fn spawn(&self, concurrency: usize, worker_id: &str) -> Vec<tokio::task::JoinHandle<()>> {
        (0..concurrency.max(1))
            .map(|i| {
                let me = self.clone();
                let wid = format!("{worker_id}-{i}");
                tokio::spawn(async move { me.run_loop(&wid).await })
            })
            .collect()
    }

    async fn run_loop(&self, worker_id: &str) {
        loop {
            match self.claim_and_run_once(worker_id).await {
                Ok(true) => {} // 1 件処理した。すぐ次を試す。
                Ok(false) => tokio::time::sleep(self.config.idle_poll).await,
                Err(e) => {
                    tracing::warn!(error = %e, "worker step 処理でエラー");
                    tokio::time::sleep(self.config.idle_poll).await;
                }
            }
        }
    }

    /// 1 件だけ claim して実行・前進する（テストからも直接呼べる）。戻り値は「処理したか」。
    pub async fn claim_and_run_once(&self, worker_id: &str) -> Result<bool, String> {
        let Some(claimed) = self
            .store
            .claim_ready_step(
                worker_id,
                self.config.lease_secs,
                self.tenant_scope.as_deref(),
            )
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(false);
        };
        self.execute_and_advance(&claimed).await?;
        Ok(true)
    }

    async fn execute_and_advance(&self, claimed: &ClaimedStep) -> Result<(), String> {
        // IR スナップショットからグラフとノード情報を得る。
        let ir: WorkflowIr =
            serde_json::from_value(claimed.ir_snapshot.0.clone()).map_err(|e| e.to_string())?;
        let graph = RunGraph::build(&ir);
        let node = ir
            .nodes
            .iter()
            .find(|n| n.id == claimed.node_id)
            .ok_or_else(|| format!("node {} が IR に無い", claimed.node_id))?;
        let max_attempts = node.retry.max_attempts as i32;

        // ノードを実行する（制御ノードも Stage A の本 PR では executor 経由で pass-through）。
        let ctx = NodeContext {
            tenant_id: claimed.tenant_id.clone(),
            org: claimed.org.clone(),
            run_id: claimed.run_id,
            step_path: claimed.step_path.clone(),
            idempotency_key: claimed.idempotency_key.clone(),
            attempt: claimed.attempt,
            principal: claimed.principal.clone(),
            input: resolve_node_input(node, &claimed.input.0),
        };
        let result: NodeResult = self
            .executor
            .execute(&node.node_type, &node.params, &ctx)
            .await;

        self.store
            .checkpoint_and_advance(claimed, &result, &graph, max_attempts)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// ノードへ渡す入力を組む（Stage A は run 入力をそのまま渡す。$from 実行時解決は Task 10.5/10.6a）。
fn resolve_node_input(_node: &crate::ir::Node, run_input: &Value) -> Value {
    run_input.clone()
}
