//! run/step の永続化と DAG 前進 TX（engine.md §2/§4）。
//!
//! claim/リース/fencing は `crates/durable` のプリミティブに載る。前進は checkpoint（terminal
//! への確定＝実行済みの唯一の真実）と後続 readiness 化を**単一 TX**で行う。

use durable::{Key, KeyValue, RunTableSpec};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::graph::RunGraph;
use super::model::{idempotency_key, RunStatus, StepStatus};
use super::readiness::{
    readiness_join, readiness_non_join, EdgeState, InEdge, Readiness,
};
use crate::vocab::{NodeType, RunEventKind};

mod advance;

/// step の durable テーブル記述子（複合キー・attempt は claim で増やさない・engine.md §9.5）。
const STEP_SPEC: RunTableSpec = RunTableSpec {
    table: "step_execution",
    status_column: "status",
    fencing_column: "fencing_token",
    lease_column: "lease_expires_at",
    worker_column: "lease_owner",
    attempt_column: None,
    updated_at_column: Some("updated_at"),
    queued_status: "ready",
    running_status: "running",
};

const STEP_KEY: &[&str] = &["tenant_id", "run_id", "step_path"];

/// run/step 操作のエラー。
#[derive(Debug, thiserror::Error)]
pub enum RunStoreError {
    #[error("対象が見つかりません")]
    NotFound,
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> RunStoreError {
    RunStoreError::Internal(format!("db: {e}"))
}

/// claim した step（実行に必要な材料）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedStep {
    pub run_id: Uuid,
    pub step_path: String,
    pub node_id: String,
    pub tenant_id: String,
    pub org: String,
    pub principal: String,
    pub attempt: i32,
    pub fencing_token: i64,
    pub idempotency_key: String,
    pub input: Json<Value>,
    /// 開始時にピンした IR（ワーカーがノード params/retry を引く）。
    pub ir_snapshot: Json<Value>,
}

/// run/step のデータチョークポイント。
#[derive(Clone)]
pub struct RunStore {
    db: PgPool,
}

impl RunStore {
    pub fn new(db: PgPool) -> Self {
        RunStore { db }
    }

    /// run を作成し本体ノードを一括実体化する（root=ready・他=pending・run.started 追記）。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_run(
        &self,
        tenant_id: &str,
        org: &str,
        workflow_id: Uuid,
        version: i64,
        trigger_kind: &str,
        principal: &str,
        input: &Value,
        ir_snapshot: &Value,
        graph: &RunGraph,
    ) -> Result<Uuid, RunStoreError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workflow_run \
             (tenant_id, org, workflow_id, version, trigger_kind, principal, input, ir_snapshot, \
              status, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'running', now()) RETURNING run_id",
        )
        .bind(tenant_id)
        .bind(org)
        .bind(workflow_id)
        .bind(version)
        .bind(trigger_kind)
        .bind(principal)
        .bind(Json(input))
        .bind(Json(ir_snapshot))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        for node_id in graph.root_body_nodes() {
            // 入エッジ 0 本の本体ノードは ready、それ以外は pending。
            let status = if graph.is_root_source(node_id) {
                StepStatus::Ready
            } else {
                StepStatus::Pending
            };
            let idem = idempotency_key(tenant_id, run_id, node_id);
            sqlx::query(
                "INSERT INTO step_execution \
                 (tenant_id, run_id, step_path, node_id, status, idempotency_key) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(tenant_id)
            .bind(run_id)
            .bind(node_id)
            .bind(node_id)
            .bind(status.as_str())
            .bind(&idem)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }

        append_event(
            &mut tx,
            tenant_id,
            run_id,
            RunEventKind::RunStarted,
            &Value::Null,
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(run_id)
    }

    /// ready な step を 1 つ claim する（`FOR UPDATE SKIP LOCKED`・fencing +1・lease）。
    ///
    /// `tenant_scope` を渡すとそのテナントの step のみ claim する（ワーカーの tenant シャーディング・
    /// テスト分離）。`None` は全テナント横断（既定のワーカー動作）。
    pub async fn claim_ready_step(
        &self,
        worker_id: &str,
        lease_secs: i64,
        tenant_scope: Option<&str>,
    ) -> Result<Option<ClaimedStep>, RunStoreError> {
        // ready（次実行時刻到来）か、リース失効した running（takeover）を 1 件 claim する
        // （SKIP LOCKED・at-least-once）。takeover でも attempt は増やさない
        // （engine.md §9.5・冪等キーは attempt 非依存）。
        let claimed: Option<ClaimedStep> = sqlx::query_as(
            "UPDATE step_execution s SET status = 'running', lease_owner = $1, \
                 lease_expires_at = now() + ($2 || ' seconds')::interval, \
                 fencing_token = s.fencing_token + 1, \
                 attempt = s.attempt + (CASE WHEN s.status = 'ready' THEN 1 ELSE 0 END), \
                 updated_at = now() \
             FROM ( \
                 SELECT tenant_id, run_id, step_path FROM step_execution \
                 WHERE (($3::text IS NULL) OR (tenant_id = $3)) \
                   AND ((status = 'ready' AND next_retry_at <= now()) \
                        OR (status = 'running' AND lease_expires_at < now())) \
                 ORDER BY next_retry_at FOR UPDATE SKIP LOCKED LIMIT 1 \
             ) picked \
             JOIN workflow_run r ON r.tenant_id = picked.tenant_id AND r.run_id = picked.run_id \
             WHERE s.tenant_id = picked.tenant_id AND s.run_id = picked.run_id \
               AND s.step_path = picked.step_path \
             RETURNING s.run_id, s.step_path, s.node_id, s.tenant_id, r.org, r.principal, \
                       s.attempt, s.fencing_token, s.idempotency_key, r.input, r.ir_snapshot",
        )
        .bind(worker_id)
        .bind(lease_secs)
        .bind(tenant_scope)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(claimed)
    }

    /// step のリースを延長し cancel_requested を返す（heartbeat）。
    pub async fn heartbeat(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        step_path: &str,
        fencing_token: i64,
        lease_secs: i64,
    ) -> Result<Option<bool>, RunStoreError> {
        let kv = [
            KeyValue::Text(tenant_id),
            KeyValue::Uuid(run_id),
            KeyValue::Text(step_path),
        ];
        durable::heartbeat(
            &self.db,
            &STEP_SPEC,
            &Key::new(STEP_KEY, &kv),
            fencing_token,
            lease_secs,
            "(SELECT cancel_requested FROM workflow_run r \
                 WHERE r.tenant_id = step_execution.tenant_id AND r.run_id = step_execution.run_id)",
        )
        .await
        .map_err(map_db)
    }

    /// step の実行結果を checkpoint し DAG を一段前進させる（単一 TX・engine.md §4.1）。
    ///
    /// `taken_ports`/`output`/`error` を書いて terminal 化 → 後続 readiness 化 → run 終了判定。
    /// fencing 不一致（ゾンビ）は false を返し no-op。
    pub async fn checkpoint_and_advance(
        &self,
        claimed: &ClaimedStep,
        result: &super::NodeResult,
        graph: &RunGraph,
        max_attempts: i32,
    ) -> Result<bool, RunStoreError> {
        advance::checkpoint_and_advance(&self.db, claimed, result, graph, max_attempts).await
    }

    /// run の状態を取得する。
    pub async fn run_status(
        &self,
        tenant_id: &str,
        run_id: Uuid,
    ) -> Result<Option<RunStatus>, RunStoreError> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT status FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(s.and_then(|s| RunStatus::parse(&s)))
    }

    /// run の全 step 状態を取得する（実行履歴・テスト検証）。
    pub async fn step_statuses(
        &self,
        tenant_id: &str,
        run_id: Uuid,
    ) -> Result<Vec<(String, StepStatus)>, RunStoreError> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT step_path, status FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 ORDER BY step_path",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|(p, s)| StepStatus::parse(&s).map(|st| (p, st)))
            .collect())
    }
}

/// 源 step の taken_ports から入エッジ状態を導出する（純関数・readiness の入力を組む）。
pub(crate) fn edge_state(
    from_port: &str,
    source_terminal: bool,
    source_taken_ports: &[String],
) -> EdgeState {
    if !source_terminal {
        EdgeState::Unresolved
    } else if source_taken_ports.iter().any(|p| p == from_port) {
        EdgeState::Live
    } else {
        EdgeState::Dead
    }
}

/// ノードの readiness をグラフ＋源 step 状態から判定する。
pub(crate) fn node_readiness(
    node_id: &str,
    graph: &RunGraph,
    terminal_ports: &std::collections::HashMap<String, Vec<String>>,
) -> Readiness {
    let edges: Vec<InEdge> = graph
        .in_edges(node_id)
        .iter()
        .map(|(from, from_port)| {
            let terminal = terminal_ports.contains_key(from);
            let taken = terminal_ports.get(from).cloned().unwrap_or_default();
            InEdge {
                from: from.clone(),
                state: edge_state(from_port, terminal, &taken),
            }
        })
        .collect();
    match graph.node_type(node_id) {
        // join の待ち合わせモードは IR の params.mode（"any"=初回 live で発火・既定 "all"）。
        Some(NodeType::ControlJoin) => readiness_join(graph.join_mode(node_id), &edges),
        _ => readiness_non_join(&edges),
    }
}

/// run_event を追記する（tenant 複合キー・(tenant,run,seq) exactly-once）。
pub(crate) async fn append_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    kind: RunEventKind,
    payload: &Value,
) -> Result<(), RunStoreError> {
    // run_event は fencing を持たない追記（fencing 検証は step 側・ここは直接 INSERT）。
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO run_event (tenant_id, run_id, seq, kind, payload) \
         SELECT $1, $2, coalesce((SELECT max(seq) FROM run_event WHERE tenant_id = $1 AND run_id = $2), 0) + 1, \
                $3, $4 RETURNING seq",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(kind.as_str())
    .bind(Json(payload))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_db)?;
    let _ = seq;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn edge_state_derivation() {
        assert_eq!(edge_state("out", false, &[]), EdgeState::Unresolved);
        assert_eq!(edge_state("out", true, &["out".into()]), EdgeState::Live);
        assert_eq!(edge_state("out", true, &["true".into()]), EdgeState::Dead);
    }

    #[test]
    fn node_readiness_linear() {
        use crate::ir::WorkflowIr;
        use serde_json::json;
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "a", "type": "storage.read", "params": {} },
                { "id": "b", "type": "storage.write", "params": {} }
            ],
            "edges": [{ "from": "a", "to": "b" }]
        }))
        .unwrap();
        let graph = RunGraph::build(&ir);
        // a 未 terminal → b は NotYet。
        let mut ports: HashMap<String, Vec<String>> = HashMap::new();
        assert_eq!(node_readiness("b", &graph, &ports), Readiness::NotYet);
        // a が out を出した → b は Ready。
        ports.insert("a".into(), vec!["out".into()]);
        assert_eq!(node_readiness("b", &graph, &ports), Readiness::Ready);
        // a が別ポート（out 以外）→ b は Skip。
        ports.insert("a".into(), vec!["error".into()]);
        assert_eq!(node_readiness("b", &graph, &ports), Readiness::Skip);
    }
}
