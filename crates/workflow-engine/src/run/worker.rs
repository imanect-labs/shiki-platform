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

        // $from nodes.<id>.output の源として、先行成功 step の出力を読み込む。
        let node_outputs = self
            .store
            .step_outputs(&claimed.tenant_id, claimed.run_id)
            .await
            .map_or(Value::Null, |pairs| {
                Value::Object(pairs.into_iter().collect())
            });

        // ノードを実行する。制御ノード（branch/switch/join）も executor 経由で taken_ports を決める。
        // trace_id は run_id（16 バイト UUID）を 32-hex 化して OTel/監査/Langfuse を束ねる。
        let ctx = NodeContext {
            tenant_id: claimed.tenant_id.clone(),
            org: claimed.org.clone(),
            run_id: claimed.run_id,
            step_path: claimed.step_path.clone(),
            idempotency_key: claimed.idempotency_key.clone(),
            attempt: claimed.attempt,
            principal: claimed.principal.clone(),
            input: claimed.input.0.clone(),
            // Stage A: interactive のトリガペイロードは run 入力と同一（schedule/event は Null）。
            trigger: claimed.input.0.clone(),
            node_outputs,
            trace_id: Some(claimed.run_id.simple().to_string()),
            // 実効スコープ = declared_scopes（run 開始時に declared ⊆ 委譲 が保証済み）。
            scope_ceiling: ir.declared_scopes.clone(),
        };
        // 実行中はリース失効を防ぐため定期 heartbeat を並走させる。これが無いと lease_secs を
        // 超える長いノードで別ワーカーが同 step を再 claim して**二重に副作用**を起こし得る（P1）。
        // 実行完了で heartbeat タスクを止める。
        let hb_store = self.store.clone();
        let hb_tenant = claimed.tenant_id.clone();
        let hb_step = claimed.step_path.clone();
        let hb_run = claimed.run_id;
        let hb_fencing = claimed.fencing_token;
        let hb_lease = self.config.lease_secs;
        // リース期間の 1/3 間隔で延長（最低 1 秒）。
        let hb_interval =
            std::time::Duration::from_secs(u64::try_from(hb_lease.max(3) / 3).unwrap_or(1));
        let heartbeat = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(hb_interval);
            ticker.tick().await; // 初回即時分を消費。
            loop {
                ticker.tick().await;
                if hb_store
                    .heartbeat(&hb_tenant, hb_run, &hb_step, hb_fencing, hb_lease)
                    .await
                    .ok()
                    .flatten()
                    .is_none()
                {
                    // fencing 不一致（別ワーカーが奪取）等はこれ以上延長しない。
                    break;
                }
            }
        });

        let result: NodeResult = self
            .executor
            .execute(&node.node_type, &node.params, &ctx)
            .await;
        heartbeat.abort();

        self.store
            .checkpoint_and_advance(claimed, &result, &graph, max_attempts)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
