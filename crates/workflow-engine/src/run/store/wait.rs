//! wait ノードの durable 化（中断の永続化＋スケジューラ起床・engine.md §9・§5.1）。
//!
//! - [`handle_suspend`]: checkpoint 時に step を `waiting_timer`/`waiting_event` へ遷移し、event は
//!   `wait_subscription` を登録する（ワーカーを解放）。
//! - 起床は **`ready` に戻さず直接 terminal 化**する（相対 duration の再登録バグを避ける・§9.1）:
//!   - `wake_due_timers`（timer）: `step_execution.wake_at <= now` を out で terminal 化。
//!   - `wake_event_waits`（event）: イベント到来を祖先束縛 scope＋filter で照合し、[`EventWaitGate`]
//!     （実行主体がイベントの対象を読めるか）を通ったものだけ out で terminal 化。
//!   - `expire_due_waits`（timeout）: `wait_subscription.timeout_at <= now` を on_timeout に従い解決。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::PgPool;
use uuid::Uuid;

use super::super::graph::RunGraph;
use super::super::model::StepStatus;
use super::super::{OnTimeout, Suspend};
use super::{append_event, map_db, ClaimedStep, RunStore, RunStoreError};
use crate::control::event_filter_matches;
use crate::ir::expr::Condition;
use crate::ir::WorkflowIr;
use crate::run::failure::WAIT_TIMEOUT;
use crate::vocab::RunEventKind;

/// wait(event) を購読した run（起こす前に、実行主体がイベントを受け取ってよいかを確かめる材料）。
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct EventWaitSubscriber {
    pub tenant_id: String,
    pub org: String,
    pub run_id: Uuid,
    pub step_path: String,
    /// 実行主体の subject local id。
    pub principal: String,
    /// 実行主体の種別（'user' or 'workflow'）。
    pub principal_kind: String,
}

/// wait(event) を起こしてよいかの判定（イベントの対象を、購読した run の実行主体が読めるか）。
///
/// 購読は登録の時点で実行主体の権限を確かめておらず、scope の無い購読はテナント内のどのイベントにも
/// 一致する。ペイロード（対象の id・書いた人など）は step の出力として実行履歴に残るので、起こす前に毎回
/// 実行主体の ReBAC で確かめる（読めない対象のイベントでは起こさず、何も渡さない・fail-closed）。
/// イベントの対象が何か（ファイルなど）はイベント源ごとに違うので、relay がイベントごとに組んで渡す。
#[async_trait]
pub trait EventWaitGate: Send + Sync {
    /// `Ok(true)` のときだけ起こす。`Ok(false)` なら起こさず、購読は残して次のイベントを待つ。
    /// `Err` は確かめられなかった（認可基盤の一時的な障害など）ことを表す。`wake_event_waits` はそのまま
    /// `Err` を返すので、relay はイベントを配送済みにせず次の tick で配り直す。
    async fn admits(&self, subscriber: &EventWaitSubscriber) -> Result<bool, RunStoreError>;
}

/// wait の中断を永続化する（checkpoint TX 内）。terminal 化はしない（起床時に行う）。
pub(super) async fn handle_suspend(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    claimed: &ClaimedStep,
    suspend: &Suspend,
) -> Result<(), RunStoreError> {
    let tenant = &claimed.tenant_id;
    let run_id = claimed.run_id;
    let step_path = &claimed.step_path;
    let kind = match suspend {
        Suspend::Approval(a) => {
            super::approval::suspend(tx, claimed, a).await?;
            "approval"
        }
        Suspend::Timer { wake_at } => {
            sqlx::query(
                "UPDATE step_execution SET status = 'waiting_timer', wake_at = $4, \
                 lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
                 WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
            )
            .bind(tenant)
            .bind(run_id)
            .bind(step_path)
            .bind(wake_at)
            .execute(&mut **tx)
            .await
            .map_err(map_db)?;
            "timer"
        }
        Suspend::Event {
            source,
            scope,
            filter,
            timeout_at,
            on_timeout,
            suspend_rev,
        } => {
            sqlx::query(
                "UPDATE step_execution SET status = 'waiting_event', \
                 lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
                 WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
            )
            .bind(tenant)
            .bind(run_id)
            .bind(step_path)
            .execute(&mut **tx)
            .await
            .map_err(map_db)?;
            // 登録より前のデータ変更では起こさない（GAP-07 の T3）: 行に束縛した wait は停止時の rev
            // （suspend_rev）以下を、テーブル単位の wait はこの Tx の時点の outbox の位置（after_event_id）以下を
            // 無視する。停止の時点からこの commit までに配送された変更は、commit の後の受付の突合が 1 回だけ拾い
            // 直す（BUG-42）。テーブル単位では、並行していた書込（この位置より前に採番され、後で commit したもの）を
            // 取りこぼし得る（docs に明記）。
            let is_data = crate::vocab::EventSource::parse(source)
                .is_some_and(crate::vocab::EventSource::is_data);
            let after_event_id = if suspend_rev.is_none() && is_data {
                let id: i64 =
                    sqlx::query_scalar("SELECT coalesce(max(id), 0) FROM storage_event_outbox")
                        .fetch_one(&mut **tx)
                        .await
                        .map_err(map_db)?;
                Some(id)
            } else {
                None
            };
            let spec = json!({
                "scope": scope,
                "filter": filter,
                "on_timeout": match on_timeout { OnTimeout::Fail => "fail", OnTimeout::Continue => "continue" },
                "suspend_rev": suspend_rev,
                "after_event_id": after_event_id,
            });
            // 再実行（at-least-once）で二重登録しないよう UPSERT（fired をリセットして待ち直す）。データ変更の wait は
            // この commit の後に 1 回だけ受付の突合を受ける（reconciled_at を空にする・BUG-42）。ほかは要らない。
            sqlx::query(
                "INSERT INTO wait_subscription \
                 (tenant_id, run_id, step_path, kind, wake_at, timeout_at, source, spec, fired, \
                  reconciled_at) \
                 VALUES ($1, $2, $3, 'event', NULL, $4, $5, $6, false, \
                         CASE WHEN $7 THEN NULL ELSE now() END) \
                 ON CONFLICT (tenant_id, run_id, step_path) DO UPDATE SET \
                 kind = 'event', wake_at = NULL, timeout_at = $4, source = $5, spec = $6, fired = false, \
                 reconciled_at = CASE WHEN $7 THEN NULL ELSE now() END",
            )
            .bind(tenant)
            .bind(run_id)
            .bind(step_path)
            .bind(*timeout_at)
            .bind(source)
            .bind(Json(&spec))
            .bind(is_data)
            .execute(&mut **tx)
            .await
            .map_err(map_db)?;
            "event"
        }
    };
    append_event(
        tx,
        tenant,
        run_id,
        RunEventKind::StepWaiting,
        &json!({ "step": step_path, "kind": kind }),
    )
    .await?;
    Ok(())
}

/// 待機中 step を直接 terminal 化して前進 TX を実行する（engine.md §9・§5.1）。
///
/// `taken_port=Some(p)` → `succeeded`・そのポート・`output`。`None` → `failed`（on_timeout=fail・run 失敗）。
/// step が既に waiting_* でない（起床済み/キャンセル）なら no-op（冪等）。戻り値 = 起床したか。
pub(super) async fn wake_step_and_advance(
    db: &PgPool,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
    taken_port: Option<&str>,
    output: Value,
    graph: &RunGraph,
) -> Result<bool, RunStoreError> {
    let mut tx = db.begin().await.map_err(map_db)?;
    if !lock_waiting(&mut tx, tenant_id, run_id, step_path).await? {
        tx.rollback().await.map_err(map_db)?;
        return Ok(false); // 既に起床/キャンセル済み（冪等）。
    }
    settle_woken(
        &mut tx, tenant_id, run_id, step_path, taken_port, output, graph,
    )
    .await?;
    tx.commit().await.map_err(map_db)?;
    Ok(true)
}

/// 起床の前段: run 行と step 行を FOR UPDATE で取り、step が待機中なら購読を消し込んで `true`。
///
/// run 行のロックで前進 TX を run 単位に直列化する（seq 衝突・join 取りこぼし防止）。`false` なら呼び出し側は
/// ロールバックする（既に起床・キャンセル済み）。
pub(super) async fn lock_waiting(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
) -> Result<bool, RunStoreError> {
    let exists: Option<Uuid> = sqlx::query_scalar(
        "SELECT run_id FROM workflow_run WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?;
    if exists.is_none() {
        return Ok(false);
    }
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM step_execution \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3 FOR UPDATE",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(step_path)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_db)?;
    let waiting = matches!(
        status.as_deref().and_then(StepStatus::parse),
        Some(StepStatus::WaitingTimer | StepStatus::WaitingEvent)
    );
    if !waiting {
        return Ok(false);
    }
    // 購読を消し込む（event/timeout 起床の再発火防止・timer は subscription 無しで no-op）。
    sqlx::query(
        "UPDATE wait_subscription SET fired = true \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(step_path)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    Ok(true)
}

/// 起床の後段: step を terminal 化して前進する（`taken_port=None` は run の失敗）。[`lock_waiting`] の後に呼ぶ。
pub(super) async fn settle_woken(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
    taken_port: Option<&str>,
    output: Value,
    graph: &RunGraph,
) -> Result<(), RunStoreError> {
    let (new_status, ports, event_kind) = match taken_port {
        Some(p) => (
            StepStatus::Succeeded,
            vec![p.to_string()],
            RunEventKind::StepSucceeded,
        ),
        None => (StepStatus::Failed, Vec::new(), RunEventKind::StepFailed),
    };
    sqlx::query(
        "UPDATE step_execution SET status = $4, output = $5, taken_ports = $6, wake_at = NULL, \
         lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(step_path)
    .bind(new_status.as_str())
    .bind(Json(&output))
    .bind(&ports)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    append_event(
        tx,
        tenant_id,
        run_id,
        RunEventKind::StepWoken,
        &json!({ "step": step_path, "port": taken_port }),
    )
    .await?;
    append_event(
        tx,
        tenant_id,
        run_id,
        event_kind,
        &json!({ "step": step_path }),
    )
    .await?;

    if taken_port.is_none() {
        // on_timeout=fail: run を失敗させる（即 fail 経路と同じ・残りを cancel）。
        super::advance::cancel_remaining_steps(tx, tenant_id, run_id).await?;
        sqlx::query(
            "UPDATE workflow_run SET status = 'failed', finished_at = now(), updated_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND status = 'running'",
        )
        .bind(tenant_id)
        .bind(run_id)
        .execute(&mut **tx)
        .await
        .map_err(map_db)?;
        append_event(tx, tenant_id, run_id, RunEventKind::RunFailed, &Value::Null).await?;
        return Ok(());
    }

    let run_failed = super::advance::advance_downstream(tx, tenant_id, run_id, graph).await?;
    if !run_failed {
        super::advance::finalize_run_if_done(tx, tenant_id, run_id).await?;
    }
    Ok(())
}

/// 待ちの step の失敗の理由を、step の `error`（engine.md §2.2 の形）と run の `fail_reason` に書く
/// （[`settle_woken`] が run を失敗にした同じ Tx・実行履歴と画面に返す失敗の理由がこの code で分類する）。
///
/// 承認（実行主体の失権・承認の期限切れ）と wait(event) の期限切れ（`on_timeout = fail`）が使う（BUG-70・BUG-112）。
pub(super) async fn record_failure(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
    code: &str,
    message: &str,
) -> Result<(), RunStoreError> {
    sqlx::query(
        "UPDATE step_execution SET error = jsonb_build_object( \
             'code', $4::text, 'message', $5::text, 'retryable', false, \
             'node_id', node_id, 'attempt', attempt) \
         WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(step_path)
    .bind(code)
    .bind(message)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    sqlx::query(
        "UPDATE workflow_run SET fail_reason = $3 \
         WHERE tenant_id = $1 AND run_id = $2 AND status = 'failed'",
    )
    .bind(tenant_id)
    .bind(run_id)
    .bind(code)
    .execute(&mut **tx)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// wait(event) の期限切れ（`on_timeout = fail`）で step を失敗にし、run を [`WAIT_TIMEOUT`] で失敗させる（BUG-112）。
///
/// [`wake_step_and_advance`] の失敗の経路と同じ手順に、理由を書く段を足したもの。既に起床・取り消し済みなら
/// no-op（冪等）。戻り値 = 失敗にしたか。
async fn fail_waiting_step(
    db: &PgPool,
    tenant_id: &str,
    run_id: Uuid,
    step_path: &str,
    graph: &RunGraph,
) -> Result<bool, RunStoreError> {
    let mut tx = db.begin().await.map_err(map_db)?;
    if !lock_waiting(&mut tx, tenant_id, run_id, step_path).await? {
        tx.rollback().await.map_err(map_db)?;
        return Ok(false);
    }
    settle_woken(
        &mut tx,
        tenant_id,
        run_id,
        step_path,
        None,
        Value::Null,
        graph,
    )
    .await?;
    record_failure(
        &mut tx,
        tenant_id,
        run_id,
        step_path,
        WAIT_TIMEOUT,
        "待つ期限までに変更が起きなかったため、止めました",
    )
    .await?;
    tx.commit().await.map_err(map_db)?;
    Ok(true)
}

/// IR スナップショット JSON からグラフを組む（起床経路のヘルパ）。
pub(super) fn graph_from_snapshot(ir_json: Value) -> Option<RunGraph> {
    serde_json::from_value::<WorkflowIr>(ir_json)
        .ok()
        .map(|ir| RunGraph::build(&ir))
}

/// 期限の来た待ち（承認・timer・timeout）の回収で、1 回に読む件数と 1 回の回収で読む回数の上限（超えた分は次の tick）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueSweepLimits {
    /// 1 回に読む件数。
    pub batch: i64,
    /// 1 回の回収で読む回数。
    pub pages: usize,
}

impl DueSweepLimits {
    /// scheduler の tick の上限（256 件 × 16 回）。
    pub const DEFAULT: DueSweepLimits = DueSweepLimits {
        batch: 256,
        pages: 16,
    };
}

/// 期限の来た待ちの走査の続きの位置（期限・テナント・run・step の昇順で、ここより後ろを読む・BUG-76）。
///
/// 起こせずに残った待ち（ピンした IR を読めない run）を、同じ回収の次のページで読み直さないために使う（承認の
/// 回収の [`super::DueApprovalCursor`] と同じ形）。
#[derive(Debug, Clone)]
struct DueWaitCursor {
    due_at: DateTime<Utc>,
    tenant_id: String,
    run_id: Uuid,
    step_path: String,
}

/// 走査の 1 ページの続きの位置（ページが埋まったときだけ・起こせなかった行も数える）。
fn next_due_cursor<T>(
    rows: &[T],
    limit: i64,
    key: impl Fn(&T) -> DueWaitCursor,
) -> Option<DueWaitCursor> {
    let full = i64::try_from(rows.len()).is_ok_and(|n| n >= limit);
    rows.last().filter(|_| full).map(key)
}

impl RunStore {
    /// due な `waiting_timer` step を起床する（out で terminal 化して前進・engine.md §9.1）。起床数を返す。
    pub async fn wake_due_timers(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
    ) -> Result<usize, RunStoreError> {
        self.wake_due_timers_within(now, tenant_scope, DueSweepLimits::DEFAULT)
            .await
    }

    /// [`Self::wake_due_timers`] を、1 回に読む件数と回数の上限を決めて呼ぶ（結合テストが小さなページで呼ぶ）。
    ///
    /// 起こせずに残った timer（ピンした IR を読めない run）は、同じ回収の次のページで読み直さない（起床の時刻・
    /// テナント・run・step の位置から続きを読む・BUG-76）。先頭に居座っても、後ろの timer の起床を止めない。
    #[doc(hidden)]
    pub async fn wake_due_timers_within(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
        limits: DueSweepLimits,
    ) -> Result<usize, RunStoreError> {
        type TimerRow = (String, Uuid, String, Json<Value>, DateTime<Utc>);
        let mut woke = 0;
        let mut after: Option<DueWaitCursor> = None;
        for _ in 0..limits.pages {
            let rows: Vec<TimerRow> = sqlx::query_as(
                "SELECT s.tenant_id, s.run_id, s.step_path, r.ir_snapshot, s.wake_at \
                 FROM step_execution s \
                 JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id \
                 WHERE s.status = 'waiting_timer' AND s.wake_at <= $1 AND r.status = 'running' \
                   AND (($2::text IS NULL) OR (s.tenant_id = $2)) \
                   AND (($3::timestamptz IS NULL) \
                        OR (s.wake_at, s.tenant_id, s.run_id, s.step_path) > ($3, $4, $5, $6)) \
                 ORDER BY s.wake_at, s.tenant_id, s.run_id, s.step_path LIMIT $7",
            )
            .bind(now)
            .bind(tenant_scope)
            .bind(after.as_ref().map(|c| c.due_at))
            .bind(after.as_ref().map_or("", |c| c.tenant_id.as_str()))
            .bind(after.as_ref().map_or(Uuid::nil(), |c| c.run_id))
            .bind(after.as_ref().map_or("", |c| c.step_path.as_str()))
            .bind(limits.batch)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
            let next = next_due_cursor(&rows, limits.batch, |r| DueWaitCursor {
                due_at: r.4,
                tenant_id: r.0.clone(),
                run_id: r.1,
                step_path: r.2.clone(),
            });
            for (tenant, run_id, step_path, ir_json, _) in rows {
                // 固定した IR を読めない run は起こせない（続きの位置で飛ばし、後ろの timer は起こす）。
                let Some(graph) = graph_from_snapshot(ir_json.0) else {
                    continue;
                };
                if wake_step_and_advance(
                    &self.db,
                    &tenant,
                    run_id,
                    &step_path,
                    Some("out"),
                    Value::Null,
                    &graph,
                )
                .await?
                {
                    woke += 1;
                }
            }
            match next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        Ok(woke)
    }

    /// due な `wait_subscription` の timeout を処理する（continue→timeout ポート / fail→run 失敗・§9.2）。
    ///
    /// 承認（`spec.approval`）の購読は扱わない。承認の期限切れは、実行主体がまだテーブルを読めるかを確かめて
    /// から決める（api の `expire_due_approvals`・D-62）。こちらで確かめずに timeout の出口へ進めない。承認の
    /// 確かめが一時障害で滞っても、ほかの wait の timeout の回収はここで毎回進む。
    pub async fn expire_due_waits(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
    ) -> Result<usize, RunStoreError> {
        self.expire_due_waits_within(now, tenant_scope, DueSweepLimits::DEFAULT)
            .await
    }

    /// [`Self::expire_due_waits`] を、1 回に読む件数と回数の上限を決めて呼ぶ（結合テストが小さなページで呼ぶ）。
    ///
    /// 決められずに残った購読（ピンした IR を読めない run）は、同じ回収の次のページで読み直さない（期限・テナント・
    /// run・step の位置から続きを読む・BUG-76）。先頭に居座っても、後ろの timeout の回収を止めない。
    #[doc(hidden)]
    pub async fn expire_due_waits_within(
        &self,
        now: DateTime<Utc>,
        tenant_scope: Option<&str>,
        limits: DueSweepLimits,
    ) -> Result<usize, RunStoreError> {
        type TimeoutRow = (
            String,
            Uuid,
            String,
            Json<Value>,
            Json<Value>,
            DateTime<Utc>,
        );
        let mut woke = 0;
        let mut after: Option<DueWaitCursor> = None;
        for _ in 0..limits.pages {
            let rows: Vec<TimeoutRow> = sqlx::query_as(
                "SELECT w.tenant_id, w.run_id, w.step_path, w.spec, r.ir_snapshot, w.timeout_at \
                 FROM wait_subscription w \
                 JOIN workflow_run r ON r.tenant_id = w.tenant_id AND r.run_id = w.run_id \
                 WHERE w.timeout_at <= $1 AND NOT w.fired AND r.status = 'running' \
                   AND NOT (w.spec ? 'approval') \
                   AND (($2::text IS NULL) OR (w.tenant_id = $2)) \
                   AND (($3::timestamptz IS NULL) \
                        OR (w.timeout_at, w.tenant_id, w.run_id, w.step_path) > ($3, $4, $5, $6)) \
                 ORDER BY w.timeout_at, w.tenant_id, w.run_id, w.step_path LIMIT $7",
            )
            .bind(now)
            .bind(tenant_scope)
            .bind(after.as_ref().map(|c| c.due_at))
            .bind(after.as_ref().map_or("", |c| c.tenant_id.as_str()))
            .bind(after.as_ref().map_or(Uuid::nil(), |c| c.run_id))
            .bind(after.as_ref().map_or("", |c| c.step_path.as_str()))
            .bind(limits.batch)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
            let next = next_due_cursor(&rows, limits.batch, |r| DueWaitCursor {
                due_at: r.5,
                tenant_id: r.0.clone(),
                run_id: r.1,
                step_path: r.2.clone(),
            });
            for (tenant, run_id, step_path, spec, ir_json, _) in rows {
                // 固定した IR を読めない run は決められない（続きの位置で飛ばし、後ろの timeout は回収する）。
                let Some(graph) = graph_from_snapshot(ir_json.0) else {
                    continue;
                };
                // on_timeout=continue → timeout ポート、fail → 時間切れの理由（WAIT_TIMEOUT）で run を失敗させる。
                let woken = if spec.0.get("on_timeout").and_then(Value::as_str) == Some("continue")
                {
                    wake_step_and_advance(
                        &self.db,
                        &tenant,
                        run_id,
                        &step_path,
                        Some("timeout"),
                        Value::Null,
                        &graph,
                    )
                    .await?
                } else {
                    fail_waiting_step(&self.db, &tenant, run_id, &step_path, &graph).await?
                };
                if woken {
                    woke += 1;
                }
            }
            match next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        Ok(woke)
    }

    /// イベント到来で `wait(event)` を起床する（同一マッチャ・祖先束縛 scope＋filter・engine.md §5.5）。
    ///
    /// `event_folder` はイベント発生フォルダ id（祖先束縛の照合対象）。out ポート・ペイロードを output に。
    /// scope と filter に合った購読も、`gate` が実行主体に対象の閲覧を認めたものだけを起こす。
    pub async fn wake_event_waits(
        &self,
        tenant_id: &str,
        source: &str,
        event_folder: Option<Uuid>,
        payload: &Value,
        gate: &dyn EventWaitGate,
    ) -> Result<usize, RunStoreError> {
        #[derive(sqlx::FromRow)]
        struct EventWaitRow {
            #[sqlx(flatten)]
            subscriber: EventWaitSubscriber,
            spec: Json<Value>,
            ir_snapshot: Json<Value>,
        }
        // scope 束縛: folder scope はイベントフォルダの祖先（node_closure・自分自身 depth 0 を含む）に
        // 購読の folder が含まれれば一致（祖先束縛）。**ワイルドカードは scope 空/欠落のみ**（run 内購読の
        // 文書化済み挙動）。folder 以外のキーだけを持つ scope（未対応形状）はワイルドカードに縮退させず
        // 一切マッチしない（fail-closed・誤形状が全購読化する事故を防ぐ）。scope は実行主体の権限を
        // 表さない（購読時に確かめていない）ので、閲覧の可否は下の gate が確かめる。
        let rows: Vec<EventWaitRow> = sqlx::query_as(
            "SELECT w.tenant_id, r.org, w.run_id, w.step_path, r.principal, r.principal_kind, \
                    w.spec, r.ir_snapshot \
             FROM wait_subscription w \
             JOIN workflow_run r ON r.tenant_id = w.tenant_id AND r.run_id = w.run_id \
             WHERE w.tenant_id = $1 AND w.source = $2 AND w.kind = 'event' AND NOT w.fired \
               AND r.status = 'running' \
               AND ( COALESCE(w.spec->'scope', '{}'::jsonb) = '{}'::jsonb \
                     OR ( (w.spec->'scope' ? 'folder') \
                          AND EXISTS ( SELECT 1 FROM node_closure c \
                                       WHERE c.tenant_id = $1 \
                                         AND c.ancestor = (w.spec->'scope'->>'folder')::uuid \
                                         AND c.descendant = $3 ) ) )",
        )
        .bind(tenant_id)
        .bind(source)
        .bind(event_folder)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut woke = 0;
        for row in rows {
            let EventWaitRow {
                subscriber,
                spec,
                ir_snapshot,
            } = row;
            // filter 評価（fail-closed: 不正 filter・不一致は起床しない・engine.md §5.6）。
            if let Some(filter_json) = spec.0.get("filter").filter(|v| !v.is_null()) {
                match serde_json::from_value::<Condition>(filter_json.clone()) {
                    Ok(cond) => match event_filter_matches(&cond, payload) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        // 型不一致などの評価エラーは起床させない（engine.md §5.6）。
                        Err(e) => {
                            tracing::warn!(code = e.code, error = %e.message, tenant = tenant_id, "wait filter の評価に失敗（fail-closed）");
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, tenant = tenant_id, "wait filter が不正（fail-closed）");
                        continue;
                    }
                }
            }
            // 実行主体が対象を読めなければ起こさない（ペイロードを実行履歴に出さない・fail-closed）。
            if !gate.admits(&subscriber).await? {
                continue;
            }
            let Some(graph) = graph_from_snapshot(ir_snapshot.0) else {
                continue;
            };
            if wake_step_and_advance(
                &self.db,
                tenant_id,
                subscriber.run_id,
                &subscriber.step_path,
                Some("out"),
                payload.clone(),
                &graph,
            )
            .await?
            {
                woke += 1;
            }
        }
        Ok(woke)
    }
}
