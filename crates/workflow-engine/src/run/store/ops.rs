//! run のキャンセル・再開操作（Task 10.14・engine.md §9.3/§11.4）。
//!
//! - **キャンセル**（v1 = step 境界検知）: `cancel_requested` を立て、実行中でない step を即
//!   cancelled 化（ドレイン）。実行中 step は完走後の checkpoint（finalize）で run が
//!   `cancelled` に落ちる。長時間オペの即時中断は v1 非対応（UI に注記・過約束しない）。
//! - **再開**（resume）: failed run の未処理失敗 step を ready に戻し、失敗ドレインで
//!   cancelled になった未実行 step を pending に復元して再前進する。成功済み checkpoint は
//!   そのまま（再実行しない・冪等キーは attempt 非依存＝engine.md §7）。

use serde_json::Value;
use uuid::Uuid;

use crate::vocab::RunEventKind;

use super::advance::advance_downstream;
use super::{append_event, map_db, RunStore, RunStoreError};

/// キャンセル要求の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelOutcome {
    /// 受理（ドレインは即時または後続 tick で完了）。
    Requested,
    /// 既に terminal（キャンセル不要）。
    AlreadyTerminal(String),
    /// run が存在しない（存在秘匿は呼び出し側）。
    NotFound,
}

/// 再開の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// 再開した（worker が未完 step から続行する）。
    Resumed,
    /// failed でない run は再開できない。
    NotFailed(String),
    /// run が存在しない。
    NotFound,
}

impl RunStore {
    /// キャンセルを要求し、実行中でない step を即ドレインする（workflow_id 束縛・存在秘匿）。
    pub async fn request_cancel(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
    ) -> Result<CancelOutcome, RunStoreError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND run_id = $3 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((status,)) = row else {
            return Ok(CancelOutcome::NotFound);
        };
        if !matches!(status.as_str(), "queued" | "running") {
            return Ok(CancelOutcome::AlreadyTerminal(status));
        }
        sqlx::query(
            "UPDATE workflow_run SET cancel_requested = true, updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        drain_one(&mut tx, tenant_id, run_id).await?;
        tx.commit().await.map_err(map_db)?;
        Ok(CancelOutcome::Requested)
    }

    /// cancel_requested な run のドレインを進める（scheduler tick から・リーダーのみ）。
    ///
    /// 実行中 step が完走/リース失効した後の取りこぼし（API 直後のドレインで running が
    /// 残ったケース）をここで回収する。戻り値 = terminal 化した run 数。
    pub async fn drain_cancel_requested(
        &self,
        tenant_scope: Option<&str>,
    ) -> Result<usize, RunStoreError> {
        let runs: Vec<(String, Uuid)> = sqlx::query_as(
            "SELECT tenant_id, run_id FROM workflow_run \
             WHERE cancel_requested AND status IN ('queued', 'running') \
               AND (($1::text IS NULL) OR (tenant_id = $1)) LIMIT 64",
        )
        .bind(tenant_scope)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut drained = 0usize;
        for (tenant, run_id) in runs {
            let mut tx = self.db.begin().await.map_err(map_db)?;
            if drain_one(&mut tx, &tenant, run_id).await? {
                drained += 1;
            }
            tx.commit().await.map_err(map_db)?;
        }
        Ok(drained)
    }

    /// run の実行主体（principal, principal_kind, trigger_kind）を返す（操作権限判定用の軽量クエリ）。
    pub async fn run_principal(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<(String, String, String)>, RunStoreError> {
        sqlx::query_as(
            "SELECT principal, principal_kind, trigger_kind FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND run_id = $3",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)
    }

    /// run にピンされた IR の declared_scopes を返す（resume の委譲再検査用・fail-closed 材料）。
    pub async fn run_declared_scopes(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
    ) -> Result<Option<Vec<String>>, RunStoreError> {
        let row: Option<(Value,)> = sqlx::query_as(
            "SELECT COALESCE(ir_snapshot->'declared_scopes', '[]'::jsonb) FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND run_id = $3",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.map(|(v,)| {
            v.as_array().map_or_else(Vec::new, |a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(ToString::to_string))
                    .collect()
            })
        }))
    }

    /// failed run を失敗 step から再開する（成功済み checkpoint は再利用・engine.md §11.4）。
    pub async fn resume_failed(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        run_id: Uuid,
    ) -> Result<ResumeOutcome, RunStoreError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let row: Option<(String, sqlx::types::Json<Value>)> = sqlx::query_as(
            "SELECT status, ir_snapshot FROM workflow_run \
             WHERE tenant_id = $1 AND workflow_id = $2 AND run_id = $3 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(workflow_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((status, ir_snapshot)) = row else {
            return Ok(ResumeOutcome::NotFound);
        };
        if status != "failed" {
            return Ok(ResumeOutcome::NotFailed(status));
        }
        // readiness 再計算は run 開始時と同じくピン済み ir_snapshot のグラフで行う。
        let ir = crate::ir::WorkflowIr::from_json(&ir_snapshot.0)
            .map_err(|e| RunStoreError::Internal(format!("ir_snapshot: {e}")))?;
        let graph = crate::run::graph::RunGraph::build(&ir);
        // 未処理失敗 step（error ポート解決済みは除外 = taken_ports 空）を ready に戻す。
        // attempt は据え置き（リトライ上限の消費分は履歴として残す）。
        sqlx::query(
            "UPDATE step_execution SET status = 'ready', next_retry_at = now(), \
                 lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND status = 'failed' \
               AND taken_ports = '{}'",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // 失敗ドレインで cancelled になった**未 claim** step（attempt=0）を pending に復元
        //（readiness は下で再計算）。
        sqlx::query(
            "UPDATE step_execution SET status = 'pending', updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND status = 'cancelled' AND attempt = 0",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // claim 済みで中断された step（attempt>0）は ready に戻すが、**旧リース分の猶予**を置く
        //（失敗直後の resume で、まだ実行中かもしれない元 worker と二重実行しない・Codex P1。
        // 元 worker の checkpoint は fencing で無害化されるが、外部副作用の同時併走を避ける）。
        sqlx::query(
            "UPDATE step_execution SET status = 'ready', \
                 next_retry_at = now() + interval '35 seconds', \
                 lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND status = 'cancelled' AND attempt > 0",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "UPDATE workflow_run SET status = 'running', finished_at = NULL, \
                 fail_reason = NULL, cancel_requested = false, updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        // pending → ready の再計算（成功済み checkpoint を前提に前進）。
        advance_downstream(&mut tx, tenant_id, run_id, &graph).await?;
        append_event(
            &mut tx,
            tenant_id,
            run_id,
            RunEventKind::RunResumed,
            &Value::Null,
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(ResumeOutcome::Resumed)
    }
}

/// 1 run のドレイン: 実行中でない step を cancelled 化し、running が残らなければ run を
/// terminal（cancelled）化する。戻り値 = run を terminal 化したか。
pub(super) async fn drain_one(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
) -> Result<bool, RunStoreError> {
    // 候補選定〜本 TX の間に resume 等で状態が変わり得るため、行ロックの上で
    // cancel_requested を**再検証**する（stale ドレインが再開済み run を殺さない・Codex P1）。
    let still_requested: bool = sqlx::query_scalar(
        "SELECT cancel_requested FROM workflow_run \
         WHERE tenant_id = $1 AND run_id = $2 AND status IN ('queued', 'running') FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?
    .unwrap_or(false);
    if !still_requested {
        return Ok(false);
    }
    // 実行中（リース有効）以外を cancelled 化（v1 = step 境界検知・engine.md §9.3）。
    // **リース失効した running**（worker 死亡）も対象にする — claim は cancel_requested run を
    // 除外するため takeover は起きず、ここで回収しないと run が永久に cancelling で残る（Codex P1）。
    sqlx::query(
        "UPDATE step_execution SET status = 'cancelled', lease_owner = NULL, \
             lease_expires_at = NULL, wake_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 \
           AND (status IN ('pending', 'ready', 'waiting_timer', 'waiting_event', 'waiting_map') \
                OR (status = 'running' AND lease_expires_at < now()))",
    )
    .bind(tenant_id)
    .bind(run_id)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    // wait 購読の消し込み（イベント/timeout 起床の再発火防止・run 失敗時ドレインと同じ）。
    sqlx::query(
        "UPDATE wait_subscription SET fired = true \
         WHERE tenant_id = $1 AND run_id = $2 AND NOT fired",
    )
    .bind(tenant_id)
    .bind(run_id)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    // running（リース有効）が残っていれば完走待ち（checkpoint 側 finalize が cancelled 化する）。
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND status = 'running' \
           AND lease_expires_at >= now()",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_db)?;
    if running > 0 {
        return Ok(false);
    }
    // run_timeout 起因のドレインは failed（ユーザーキャンセルと区別する・engine.md §5.3）。
    let timeout: bool = sqlx::query_scalar(
        "SELECT COALESCE(fail_reason = 'run_timeout', false) FROM workflow_run \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?
    .unwrap_or(false);
    let (final_status, kind) = if timeout {
        ("failed", RunEventKind::RunFailed)
    } else {
        ("cancelled", RunEventKind::RunCancelled)
    };
    let updated = sqlx::query(
        "UPDATE workflow_run SET status = $3, finished_at = now(), updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND status IN ('queued', 'running')",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(final_status)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    if updated.rows_affected() > 0 {
        append_event(tx, tenant_id, run_id, kind, &Value::Null).await?;
        return Ok(true);
    }
    Ok(false)
}
