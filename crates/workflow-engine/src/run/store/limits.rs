//! run 多重度の promote と run timeout の強制（Task 10.5・engine.md §8.3/§5.3）。
//!
//! scheduler tick（リーダー）から呼ばれる:
//! - [`RunStore::promote_queued_runs`] — max_parallel_runs 未満（待機中の run は数えない）の workflow の
//!   最古 queued run を running へ昇格（entry step を ready 化・run.started 追記）。バックプレッシャの解放側。
//!   1 つのワークフローの誤りは閉じ込めて次へ進む。ピンした IR が自分の知らない新しい版（`ir_version`）なら、
//!   そのワークフローを飛ばして新しい版のリーダーに任せる。知っている版で読めない run は failed で終わらせる。
//! - [`RunStore::expire_run_timeouts`] — `timeout_at` 超過の running run を fail_reason=run_timeout
//!   でキャンセル・ドレインに載せる（最終 status は failed）。

use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use crate::vocab::RunEventKind;

use super::{append_event, map_db, RunStore, RunStoreError};

/// ピンした IR（`ir_snapshot`）を読めずに failed で終わらせた run の `fail_reason`（内部の失敗・分類は unknown）。
const FAIL_REASON_UNREADABLE_IR: &str = "internal";

/// [`RunStore::promote_queued_runs`] の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromoteReport {
    /// running へ昇格した run の数。
    pub promoted: usize,
    /// ピンした IR を読めず、failed（fail_reason=internal）で終わらせた run の数。
    pub failed: usize,
    /// 誤りで途中までしか進められなかったワークフローの数（warn に残し、次の tick で取り直す）。
    pub skipped_workflows: usize,
    /// 先頭の queued run のピンした IR が、このエンジンの知らない新しい版（`ir_version` が
    /// [`MAX_IR_VERSION`](crate::ir::MAX_IR_VERSION) より大きい）のため、昇格せずに残したワークフローの数
    /// （ローリングデプロイの途中・新しい版のリーダーが拾う・D-65）。
    pub deferred_newer_ir: usize,
}

/// ピンした IR の `ir_version` が、このエンジンの知っている版より新しいか（読めないのは自分が古いから）。
///
/// 版が書かれていない・整数でないものは新しい版とは言えないので偽（知っている版で読めない IR として扱う）。
fn is_newer_ir_version(ir_snapshot: &Value) -> bool {
    ir_snapshot
        .get("ir_version")
        .and_then(Value::as_u64)
        .is_some_and(|v| v > u64::from(crate::ir::MAX_IR_VERSION))
}

impl RunStore {
    /// queued run を（workflow ごとに）多重度上限まで promote する。
    ///
    /// **1 つのワークフローの誤りで、ほかのワークフロー（ほかのテナントを含む）の昇格を止めない。** 誤りは
    /// warn に残して次のワークフローへ進む（そのワークフローは次の tick で取り直す）。ピンした IR を読めない
    /// run は待っても昇格できず先頭に居座るので、failed で終わらせて外す。ただし IR が自分の知らない新しい版
    /// なら失敗させない（ローリングデプロイの途中で古い版のリーダーが新しい版の run を誤って永久に失敗させない・
    /// D-65）。そのワークフローだけを飛ばして warn に残し、新しい版のリーダーに任せる。`Err` は対象の列挙に
    /// 失敗したときだけ。
    pub async fn promote_queued_runs(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<PromoteReport, RunStoreError> {
        // queued を持つ workflow を列挙（少数前提・部分 index 走査）。
        let targets: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT DISTINCT tenant_id, workflow_id FROM workflow_run \
             WHERE status = 'queued' AND (($1::text IS NULL) OR (tenant_id = $1))",
        )
        .bind(tenant_scope)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut report = PromoteReport::default();
        for (tenant, wf) in targets {
            if let Err(e) = self.promote_for_workflow(&tenant, wf, &mut report).await {
                tracing::warn!(
                    tenant_id = %tenant,
                    workflow_id = %wf,
                    error = %e,
                    "queued run の昇格に失敗（ほかのワークフローは続ける・次の tick で取り直す）"
                );
                report.skipped_workflows += 1;
            }
        }
        Ok(report)
    }

    /// 1 workflow の queued を古い順に、running が上限未満の間だけ promote する（数は `report` に足す）。
    async fn promote_for_workflow(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        report: &mut PromoteReport,
    ) -> Result<(), RunStoreError> {
        loop {
            let mut tx = self.db.begin().await.map_err(map_db)?;
            // create_run と同じ advisory lock で多重度判定を直列化する。
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2, 42))")
                .bind(tenant_id)
                .bind(workflow_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(map_db)?;
            // 最古の queued を 1 件ロック（無ければ終了）。
            let row: Option<(Uuid, Json<Value>)> = sqlx::query_as(
                "SELECT run_id, ir_snapshot FROM workflow_run \
                 WHERE tenant_id = $1 AND workflow_id = $2 AND status = 'queued' \
                 ORDER BY created_at, run_id FOR UPDATE SKIP LOCKED LIMIT 1",
            )
            .bind(tenant_id)
            .bind(workflow_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db)?;
            let Some((run_id, ir_snapshot)) = row else {
                tx.rollback().await.map_err(map_db)?;
                break;
            };
            // ピンした IR を読めない run は、待っても昇格できない（先頭に居座って後ろの run も止める）。
            let ir = match crate::ir::WorkflowIr::from_json(&ir_snapshot.0) {
                Ok(ir) => ir,
                // 自分より新しい版の IR は、新しい版のリーダーなら読める。失敗させずにこのワークフローだけを
                // 飛ばす（古い順を守るため、後ろの run も昇格しない・D-65）。
                Err(e) if is_newer_ir_version(&ir_snapshot.0) => {
                    tx.rollback().await.map_err(map_db)?;
                    tracing::warn!(
                        tenant_id,
                        %workflow_id,
                        %run_id,
                        ir_version = ?ir_snapshot.0.get("ir_version"),
                        max_ir_version = crate::ir::MAX_IR_VERSION,
                        error = %e,
                        "ピンした IR が新しい版のため、このワークフローの昇格を見送りました（新しい版のリーダーが拾う）"
                    );
                    report.deferred_newer_ir += 1;
                    break;
                }
                Err(e) => {
                    fail_unreadable_run(&mut tx, tenant_id, run_id).await?;
                    tx.commit().await.map_err(map_db)?;
                    tracing::warn!(
                        tenant_id,
                        %workflow_id,
                        %run_id,
                        error = %e,
                        "ピンした IR を読めない queued run を failed で終わらせました"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            let policies = &ir.policies;
            // 待機中の run は数えない（create_run と同じ述語・D-26）。
            let running: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM workflow_run r \
                 WHERE r.tenant_id = $1 AND r.workflow_id = $2 AND r.status = 'running' \
                   AND {}",
                super::ACTIVE_RUN
            ))
            .bind(tenant_id)
            .bind(workflow_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db)?;
            if running >= i64::from(policies.max_parallel_runs.max(1)) {
                tx.rollback().await.map_err(map_db)?;
                break;
            }
            sqlx::query(
                "UPDATE workflow_run SET status = 'running', started_at = now(), \
                     timeout_at = now() + ($3 || ' seconds')::interval, updated_at = now() \
                 WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(tenant_id)
            .bind(run_id)
            .bind(i64::from(policies.run_timeout_sec))
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            // entry（入エッジ 0 本の本体ノード）を ready 化する。step_path = node_id（静的ノード）で、
            // create_run が queued では pending 留め置きにしたものだけが対象（他はそもそも pending）。
            let graph = crate::run::graph::RunGraph::build(&ir);
            for node_id in graph.root_body_nodes() {
                if graph.is_root_source(node_id) {
                    sqlx::query(
                        "UPDATE step_execution SET status = 'ready', next_retry_at = now(), \
                             updated_at = now() \
                         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3 \
                           AND status = 'pending'",
                    )
                    .bind(tenant_id)
                    .bind(run_id)
                    .bind(node_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db)?;
                }
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
            report.promoted += 1;
        }
        Ok(())
    }

    /// timeout_at 超過の running run を run_timeout 失敗としてキャンセル経路に載せる。
    ///
    /// fail_reason=run_timeout を刻んで cancel_requested を立てる（ドレインの最終 status は
    /// failed・[`drain_one`](super::ops) が fail_reason で分岐）。戻り値 = 対象 run 数。
    pub async fn expire_run_timeouts(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        tenant_scope: Option<&str>,
    ) -> Result<usize, RunStoreError> {
        let expired: Vec<(String, Uuid)> = sqlx::query_as(
            "UPDATE workflow_run SET cancel_requested = true, fail_reason = 'run_timeout', \
                 updated_at = now() \
             WHERE status = 'running' AND timeout_at IS NOT NULL AND timeout_at < $1 \
               AND fail_reason IS DISTINCT FROM 'run_timeout' \
               AND (($2::text IS NULL) OR (tenant_id = $2)) \
             RETURNING tenant_id, run_id",
        )
        .bind(now)
        .bind(tenant_scope)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        // 即時ドレイン（running step が残る run は tick の drain_cancel_requested が回収）。
        let n = expired.len();
        for (tenant, run_id) in expired {
            let mut tx = self.db.begin().await.map_err(map_db)?;
            super::ops::drain_one(&mut tx, &tenant, run_id).await?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(n)
    }
}

/// ピンした IR を読めない queued run を failed で終わらせる（run の失敗の経路と同じ: 残る step を cancelled に
/// して run.failed を積む）。`fail_reason` は内部の失敗（分類は unknown・押し直しを勧めない文言になる）。
async fn fail_unreadable_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
) -> Result<(), RunStoreError> {
    super::advance::cancel_remaining_steps(tx, tenant_id, run_id).await?;
    sqlx::query(
        "UPDATE workflow_run SET status = 'failed', fail_reason = $3, finished_at = now(), \
             updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND status = 'queued'",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(FAIL_REASON_UNREADABLE_IR)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    append_event(tx, tenant_id, run_id, RunEventKind::RunFailed, &Value::Null).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 新しい版と言えるのは、整数の `ir_version` が知っている最大の版より大きいときだけ（D-65）。
    #[test]
    fn only_a_larger_integer_ir_version_is_newer() {
        let max = crate::ir::MAX_IR_VERSION;
        assert!(is_newer_ir_version(
            &json!({ "ir_version": max + 1, "x": 1 })
        ));
        assert!(is_newer_ir_version(&json!({ "ir_version": u64::MAX })));
        assert!(!is_newer_ir_version(&json!({ "ir_version": max, "x": 1 })));
        assert!(!is_newer_ir_version(&json!({ "ir_version": 0 })));
        assert!(!is_newer_ir_version(&json!({})));
        assert!(!is_newer_ir_version(&json!({ "ir_version": "2" })));
        assert!(!is_newer_ir_version(&json!({ "ir_version": -1 })));
        assert!(!is_newer_ir_version(&json!(null)));
    }
}
