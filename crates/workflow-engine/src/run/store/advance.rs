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
use super::map_region::{self, MapOutcome};
use super::{append_event, map_db, RunStoreError};
use crate::ir::OnError;
use crate::vocab::RunEventKind;

use super::backoff::next_retry_delay_secs;

/// checkpoint＋前進の本体。
///
/// `on_error` はノードの失敗時方針（既定 `fail_run`）。`continue` の失敗 step は run を落とさず
/// `error` ポートのデータフローへ変換する（engine.md §4.1 手順 2・§4.5）。
#[allow(clippy::too_many_lines)]
pub(super) async fn checkpoint_and_advance(
    db: &PgPool,
    claimed: &super::ClaimedStep,
    result: &NodeResult,
    graph: &RunGraph,
    max_attempts: i32,
    on_error: OnError,
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

    // wait の中断（timer/event）: terminal 化せず待機状態にしてワーカーを解放する（engine.md §9）。
    if let Some(suspend) = &result.suspend {
        super::wait::handle_suspend(&mut tx, claimed, suspend).await?;
        tx.commit().await.map_err(map_db)?;
        return Ok(true);
    }

    // map の動的 fan-out: 要素を挿入し map step を waiting_map にする（engine.md §4.5）。
    if let Some(fanout) = &result.fanout {
        let count = map_region::insert_fanout(
            &mut tx,
            tenant_id,
            run_id,
            graph,
            &claimed.step_path,
            on_error,
            fanout,
        )
        .await?;
        if count == 0 {
            // 空 map は即集約して後続を前進させる（要素が無いので必ず成功・run 失敗しない）。
            let _ =
                map_region::aggregate_map(&mut tx, tenant_id, run_id, &claimed.step_path).await?;
            advance_downstream(&mut tx, tenant_id, run_id, graph).await?;
            finalize_run_if_done(&mut tx, tenant_id, run_id).await?;
        }
        tx.commit().await.map_err(map_db)?;
        return Ok(true);
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

    // error 列は engine.md §2.2 どおり `{code,message,retryable,node_id,attempt}` に統一する
    // （後続ノードが `$from nodes.<id>.output.error.*` で参照する形と一致させる）。
    let error_obj = result.error.as_ref().map(|e| {
        json!({
            "code": e.code,
            "message": e.message,
            "retryable": e.retryable,
            "node_id": claimed.node_id,
            "attempt": claimed.attempt,
        })
    });

    // on_error=continue かつ失敗（リトライ枯渇後）は「処理済み失敗」: run を落とさず error ポートへ流す。
    // ただし **error 出エッジが実際に繋がっている場合のみ** continue 扱いにする。エラーの行き先が無い
    // （error ポート未接続）なら握り潰さず fail_run と同じく run を失敗させる（#179 受け入れ条件）。
    let has_error_edge = graph
        .out_edges(&claimed.node_id)
        .iter()
        .any(|(from_port, _)| from_port == "error");
    let continue_on_error = !result.ok && on_error == OnError::Continue && has_error_edge;

    // checkpoint: terminal 状態＋output/taken_ports/error を確定する。
    let (status, ports, output_json, event_kind) = if result.ok {
        (
            StepStatus::Succeeded,
            result.taken_ports.clone(),
            result.output.clone(),
            RunEventKind::StepSucceeded,
        )
    } else if continue_on_error {
        // 失敗をデータフローに変換する。output に error オブジェクトを載せ、taken_ports=error で
        // error 出エッジのみ live（out 出エッジは dead）にして後続を前進させる（§4.1 手順 2）。
        (
            StepStatus::Failed,
            vec!["error".to_string()],
            json!({ "error": error_obj }),
            RunEventKind::StepFailed,
        )
    } else {
        // fail_run（既定）: 未処理失敗。taken_ports は空（全出エッジ dead）で run を failed へ。
        (
            StepStatus::Failed,
            Vec::new(),
            Value::Null,
            RunEventKind::StepFailed,
        )
    };
    sqlx::query(
        "UPDATE step_execution SET status = $4, output = $5, taken_ports = $6, error = $7, \
         lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(&claimed.step_path)
    .bind(status.as_str())
    .bind(Json(&output_json))
    .bind(&ports)
    .bind(error_obj.as_ref().map(Json))
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    append_event(
        &mut tx,
        tenant_id,
        run_id,
        event_kind,
        // continue 経路は「失敗→error ポート遷移」を監査で辿れるよう明示する（#179 受け入れ）。
        &json!({
            "step": claimed.step_path,
            "on_error": if continue_on_error { Some("continue") } else { None::<&str> },
            "taken_ports": ports,
        }),
    )
    .await?;

    // fail_run（既定）で step が失敗したら run を即 failed 化し、残る非 terminal step を cancelled に
    // する。こうしないと ready/running の兄弟が run 失敗後も claim され副作用を起こし得る（P1）。
    // 例外: ①on_error=continue は run を落とさず下の前進へ ②map 領域要素（step_path に `[`）の失敗は
    // 要素内 skip に留め、他要素を完走させてから map 集約で扱う（engine.md §4.5・ir.md §5.3）。
    let is_region_element = claimed.step_path.contains('[');
    if !result.ok && !continue_on_error && !is_region_element {
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

    // 後続 readiness 化＋skip 伝播＋map 集約（fixpoint）。map が fail_run で失敗したら run は失敗確定済み。
    let run_failed = advance_downstream(&mut tx, tenant_id, run_id, graph).await?;
    if !run_failed {
        // run 終了判定（全 step terminal で run terminal）。
        finalize_run_if_done(&mut tx, tenant_id, run_id).await?;
    }

    tx.commit().await.map_err(map_db)?;
    Ok(true)
}

/// run 失敗時に残る非 terminal step を cancelled 化する（以後 claim されない）。
pub(super) async fn cancel_remaining_steps(
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

/// pending の step を fixpoint で ready/skipped 化し（要素スコープ考慮の skip 伝播）、待機中の map を
/// 集約する。map が fail_run で失敗したら run を failed 化し `true` を返す（呼び出し側は finalize を飛ばす）。
pub(super) async fn advance_downstream(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    graph: &RunGraph,
) -> Result<bool, RunStoreError> {
    loop {
        // 現在の全 step 状態と terminal ports を読む（要素を混同しないよう step_path でキーする）。
        let rows: Vec<(String, String, Vec<String>)> = sqlx::query_as(
            "SELECT step_path, status, taken_ports FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_db)?;

        // step_path → terminal 時の taken_ports。pending / waiting_map を分けて集める。
        let mut terminal_by_path: HashMap<String, Vec<String>> = HashMap::new();
        let mut pending: Vec<String> = Vec::new();
        let mut waiting_maps: Vec<String> = Vec::new();
        for (path, status, ports) in &rows {
            match StepStatus::parse(status) {
                Some(s) if s.is_terminal() => {
                    terminal_by_path.insert(path.clone(), ports.clone());
                }
                Some(StepStatus::Pending) => pending.push(path.clone()),
                Some(StepStatus::WaitingMap) => waiting_maps.push(path.clone()),
                _ => {}
            }
        }

        let mut changed = false;
        for path in &pending {
            let (scope, node) = map_region::split_step_path(path);
            match map_region::scoped_readiness(scope, node, graph, &terminal_by_path) {
                Readiness::Ready => {
                    set_step_status(tx, tenant_id, run_id, path, StepStatus::Ready, true).await?;
                    // ready 遷移を run_event に記録する（SSE/replay が step 状態を正しく追える）。
                    append_event(
                        tx,
                        tenant_id,
                        run_id,
                        RunEventKind::StepReady,
                        &json!({ "step": path }),
                    )
                    .await?;
                    changed = true;
                }
                Readiness::Skip => {
                    set_step_status(tx, tenant_id, run_id, path, StepStatus::Skipped, false)
                        .await?;
                    append_event(
                        tx,
                        tenant_id,
                        run_id,
                        RunEventKind::StepSkipped,
                        &json!({ "step": path }),
                    )
                    .await?;
                    changed = true;
                }
                Readiness::NotYet => {}
            }
        }

        // 待機中の map を集約する（全要素の出口が terminal なら terminal 化して後続を前進）。
        for map_path in &waiting_maps {
            match map_region::aggregate_map(tx, tenant_id, run_id, map_path).await? {
                MapOutcome::Pending => {}
                MapOutcome::Completed => changed = true,
                MapOutcome::RunFailed => {
                    // map が fail_run で失敗 → run を failed 化し残りを cancel（即 fail 経路と同じ）。
                    cancel_remaining_steps(tx, tenant_id, run_id).await?;
                    sqlx::query(
                        "UPDATE workflow_run SET status = 'failed', finished_at = now(), \
                         updated_at = now() WHERE tenant_id = $1 AND run_id = $2 AND status = 'running'",
                    )
                    .bind(tenant_id)
                    .bind(run_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(map_db)?;
                    append_event(tx, tenant_id, run_id, RunEventKind::RunFailed, &Value::Null)
                        .await?;
                    return Ok(true);
                }
            }
        }
        if !changed {
            break;
        }
    }
    Ok(false)
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
pub(super) async fn finalize_run_if_done(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
) -> Result<(), RunStoreError> {
    let rows: Vec<(String, String, Vec<String>)> = sqlx::query_as(
        "SELECT step_path, status, taken_ports FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_db)?;
    let steps: Vec<(&String, StepStatus, &Vec<String>)> = rows
        .iter()
        .filter_map(|(path, s, ports)| StepStatus::parse(s).map(|st| (path, st, ports)))
        .collect();
    if !steps.iter().all(|(_, s, _)| s.is_terminal()) {
        return Ok(()); // まだ実行中 step がある。
    }
    // 未処理失敗（Failed かつ taken_ports 空）だけが run を failed にする。ただし:
    // ①on_error=continue で error ポートを取った failed（taken_ports 非空）は「処理済み失敗」
    // ②map 領域要素（step_path に `[`）の失敗は map ノードが集約するため run 成否に直接数えない
    //   （fail_map なら map step 自身が未処理失敗として run を落とす・engine.md §4.5）。
    let any_unhandled_failed = steps.iter().any(|(path, s, ports)| {
        *s == StepStatus::Failed && ports.is_empty() && !path.contains('[')
    });
    let (run_status, kind) = if any_unhandled_failed {
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
