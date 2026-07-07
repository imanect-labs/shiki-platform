//! checkpoint＋DAG 前進の単一 TX（engine.md §4.1）。
//!
//! 手順: fencing 検証 → checkpoint（terminal 確定・output/taken_ports 書込）→ 後続 readiness 化＋
//! skip 伝播（fixpoint）→ run 終了判定。checkpoint 書込が「実行済み」の唯一の真実。

use std::collections::HashMap;

use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::super::graph::RunGraph;
use super::super::model::{RunStatus, StepStatus};
use super::super::readiness::Readiness;
use super::super::NodeResult;
use super::{append_event, map_db, node_readiness, RunStoreError};
use crate::vocab::RunEventKind;

/// backoff（指数＋full jitter・engine.md §7.4）。jitter の乱数は (run_id, step_path, attempt) の
/// FNV ハッシュから決定的に導く（Math.random 不使用・リプレイ安全・thundering herd 回避）。
/// **run_id を種に含める**ことで、共有障害・429 storm でも run ごとに遅延が分散する（同一ノードの
/// 全 run が同時に起きない）。
fn next_retry_delay_secs(run_id: Uuid, step_path: &str, attempt: i32) -> i64 {
    let base: i64 = 2;
    let cap: i64 = 300;
    let rand01 = deterministic_rand01(run_id, step_path, attempt);
    crate::retry::backoff_with_jitter(attempt, base, cap, rand01)
}

/// (run_id, step_path, attempt) から `[0, 1)` の決定的乱数を導く（FNV-1a → 正規化）。
fn deterministic_rand01(run_id: Uuid, step_path: &str, attempt: i32) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in run_id
        .as_bytes()
        .iter()
        .copied()
        .chain(step_path.bytes())
        .chain(attempt.to_le_bytes())
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 上位 53 bit を [0,1) へ。
    #[allow(clippy::cast_precision_loss)]
    let v = (h >> 11) as f64 / (1u64 << 53) as f64;
    v
}

/// checkpoint＋前進の本体。
#[allow(clippy::too_many_lines)]
pub(super) async fn checkpoint_and_advance(
    db: &PgPool,
    claimed: &super::ClaimedStep,
    result: &NodeResult,
    graph: &RunGraph,
    max_attempts: i32,
) -> Result<bool, RunStoreError> {
    let mut tx = db.begin().await.map_err(map_db)?;
    let tenant_id = &claimed.tenant_id;
    let run_id = claimed.run_id;

    // **run 行を FOR UPDATE で確保して checkpoint を run 単位で直列化する。** 並行 fan-out/fan-in の
    // checkpoint が同時に走ると (a) run_event の seq 採番が衝突して PK エラーで丸ごと巻き戻り
    // step 再実行を招く、(b) join の両前段が互いの terminal を見られず join を pending のまま
    // 取り残す、という不整合が起きる。run 行ロックで前進 TX を直列化し双方を防ぐ（engine.md §4.1）。
    let run_exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT run_id FROM workflow_run WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db)?;
    if run_exists.is_none() {
        tx.rollback().await.map_err(map_db)?;
        return Ok(false);
    }

    // fencing 検証（ゾンビ書込拒否）。現在の fencing_token が claim 時と一致するか。
    let current_fencing: Option<i64> = sqlx::query_scalar(
        "SELECT fencing_token FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(&claimed.step_path)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db)?;
    if current_fencing != Some(claimed.fencing_token) {
        tx.rollback().await.map_err(map_db)?;
        return Ok(false); // ゾンビ（別ワーカーが再 claim 済み）。
    }

    // リトライ判定（engine.md §7.4）: エラーを分類し ready へ戻すか terminal 化するか決める。
    // - RateLimited: attempt を消費せず再試行（次 claim の +1 を打ち消すため attempt-1）。
    // - Retryable: attempt 未枯渇なら backoff 再試行。
    // - Permanent / 枯渇: 下の checkpoint で terminal（失敗）化。
    if !result.ok {
        use crate::retry::RetryClass;
        let class = result.error.as_ref().map_or(RetryClass::Permanent, |e| {
            crate::retry::classify(&e.code, e.retryable)
        });
        let retry = match class {
            RetryClass::RateLimited => true,
            RetryClass::Retryable => claimed.attempt < max_attempts,
            RetryClass::Permanent => false,
        };
        if retry {
            let delay = next_retry_delay_secs(run_id, &claimed.step_path, claimed.attempt);
            // rate_limited は attempt を消費しない（次 claim で +1 されるぶんを相殺）。
            let attempt_delta: i32 = if class == RetryClass::RateLimited {
                -1
            } else {
                0
            };
            sqlx::query(
                "UPDATE step_execution SET status = 'ready', lease_owner = NULL, \
                 lease_expires_at = NULL, next_retry_at = now() + ($4 || ' seconds')::interval, \
                 attempt = attempt + $5, updated_at = now() \
                 WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
            )
            .bind(tenant_id)
            .bind(run_id)
            .bind(&claimed.step_path)
            .bind(delay)
            .bind(attempt_delta)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            append_event(
                &mut tx,
                tenant_id,
                run_id,
                RunEventKind::StepRetrying,
                &json!({
                    "step": claimed.step_path,
                    "attempt": claimed.attempt,
                    "class": format!("{class:?}"),
                }),
            )
            .await?;
            tx.commit().await.map_err(map_db)?;
            return Ok(true);
        }
    }

    // checkpoint: terminal 状態＋output/taken_ports/error を確定する。
    let (status, ports, event_kind) = if result.ok {
        (
            StepStatus::Succeeded,
            result.taken_ports.clone(),
            RunEventKind::StepSucceeded,
        )
    } else {
        // Stage A は fail_run 既定（on_error=continue の error ポートは Task 10.5）。
        (StepStatus::Failed, Vec::new(), RunEventKind::StepFailed)
    };
    let error_json = result
        .error
        .as_ref()
        .map(|e| json!({ "code": e.code, "message": e.message, "retryable": e.retryable }));
    sqlx::query(
        "UPDATE step_execution SET status = $4, output = $5, taken_ports = $6, error = $7, \
         lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(&claimed.step_path)
    .bind(status.as_str())
    .bind(Json(&result.output))
    .bind(&ports)
    .bind(error_json.as_ref().map(Json))
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    append_event(
        &mut tx,
        tenant_id,
        run_id,
        event_kind,
        &json!({ "step": claimed.step_path }),
    )
    .await?;

    // fail_run（既定）で step が失敗したら run を即 failed 化し、残る非 terminal step を cancelled に
    // する。こうしないと ready/running の兄弟が run 失敗後も claim され副作用を起こし得る（P1）。
    if !result.ok {
        cancel_remaining_steps(&mut tx, tenant_id, run_id).await?;
        sqlx::query(
            "UPDATE workflow_run SET status = 'failed', finished_at = now(), updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND status = 'running'",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        append_event(
            &mut tx,
            tenant_id,
            run_id,
            RunEventKind::RunFailed,
            &Value::Null,
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        return Ok(true);
    }

    // 後続 readiness 化＋skip 伝播（fixpoint・全 step を読んでメモリで判定し書き戻す）。
    advance_downstream(&mut tx, tenant_id, run_id, graph).await?;

    // run 終了判定（全 step terminal で run terminal）。
    finalize_run_if_done(&mut tx, tenant_id, run_id).await?;

    tx.commit().await.map_err(map_db)?;
    Ok(true)
}

/// run 失敗時に残る非 terminal step を cancelled 化する（以後 claim されない）。
async fn cancel_remaining_steps(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
) -> Result<(), RunStoreError> {
    sqlx::query(
        "UPDATE step_execution SET status = 'cancelled', lease_owner = NULL, \
         lease_expires_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 \
           AND status IN ('pending', 'ready', 'running')",
    )
    .bind(tenant_id)
    .bind(run_id)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// pending の step を fixpoint で ready/skipped 化する（skip 伝播）。
async fn advance_downstream(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    graph: &RunGraph,
) -> Result<(), RunStoreError> {
    loop {
        // 現在の全 step 状態と terminal ports を読む。
        let rows: Vec<(String, String, Vec<String>)> = sqlx::query_as(
            "SELECT node_id, status, taken_ports FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_db)?;

        // node_id → terminal 時の taken_ports（terminal でない node は不在）。
        let mut terminal_ports: HashMap<String, Vec<String>> = HashMap::new();
        let mut pending: Vec<String> = Vec::new();
        for (node_id, status, ports) in &rows {
            match StepStatus::parse(status) {
                Some(s) if s.is_terminal() => {
                    terminal_ports.insert(node_id.clone(), ports.clone());
                }
                Some(StepStatus::Pending) => pending.push(node_id.clone()),
                _ => {}
            }
        }

        let mut changed = false;
        for node_id in &pending {
            match node_readiness(node_id, graph, &terminal_ports) {
                Readiness::Ready => {
                    set_step_status(tx, tenant_id, run_id, node_id, StepStatus::Ready, true)
                        .await?;
                    // ready 遷移を run_event に記録する（SSE/replay が step 状態を正しく追える）。
                    append_event(
                        tx,
                        tenant_id,
                        run_id,
                        RunEventKind::StepReady,
                        &json!({ "step": node_id }),
                    )
                    .await?;
                    changed = true;
                }
                Readiness::Skip => {
                    set_step_status(tx, tenant_id, run_id, node_id, StepStatus::Skipped, false)
                        .await?;
                    append_event(
                        tx,
                        tenant_id,
                        run_id,
                        RunEventKind::StepSkipped,
                        &json!({ "step": node_id }),
                    )
                    .await?;
                    changed = true;
                }
                Readiness::NotYet => {}
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

/// step の status を更新する（ready 化時は next_retry_at を now に）。
async fn set_step_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
    status: StepStatus,
    reset_retry: bool,
) -> Result<(), RunStoreError> {
    let retry_clause = if reset_retry {
        ", next_retry_at = now()"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE step_execution SET status = $4{retry_clause}, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3"
    );
    sqlx::query(&sql)
        .bind(tenant_id)
        .bind(run_id)
        .bind(step_path)
        .bind(status.as_str())
        .execute(&mut **tx)
        .await
        .map_err(map_db)?;
    Ok(())
}

/// 全 step が terminal なら run を terminal 化する（失敗があれば failed）。
async fn finalize_run_if_done(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
) -> Result<(), RunStoreError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT status FROM step_execution WHERE tenant_id = $1 AND run_id = $2")
            .bind(tenant_id)
            .bind(run_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_db)?;
    let statuses: Vec<StepStatus> = rows
        .iter()
        .filter_map(|(s,)| StepStatus::parse(s))
        .collect();
    if !statuses.iter().all(|s| s.is_terminal()) {
        return Ok(()); // まだ実行中 step がある。
    }
    let any_failed = statuses.contains(&StepStatus::Failed);
    let (run_status, kind) = if any_failed {
        (RunStatus::Failed, RunEventKind::RunFailed)
    } else {
        (RunStatus::Succeeded, RunEventKind::RunSucceeded)
    };
    sqlx::query(
        "UPDATE workflow_run SET status = $3, finished_at = now(), updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND status = 'running'",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(run_status.as_str())
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    append_event(tx, tenant_id, run_id, kind, &Value::Null).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_bounded_and_deterministic() {
        // full jitter: [1, ceiling]。ceiling は base*2^attempt を cap=300 で頭打ち。
        let rid = Uuid::nil();
        assert!((1..=2).contains(&next_retry_delay_secs(rid, "a", 0)));
        assert!(next_retry_delay_secs(rid, "a", 20) <= 300);
        // 同じ (step, attempt) は同じ遅延（リプレイ安全）。
        assert_eq!(
            next_retry_delay_secs(rid, "a", 3),
            next_retry_delay_secs(rid, "a", 3)
        );
    }
}
