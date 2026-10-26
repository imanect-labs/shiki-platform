//! データ変更の wait(event) の受付の突合（BUG-42・承認の受付の突合と同じ形）。
//!
//! 停止の時点（行の rev・outbox の位置を読んだ時点）から購読の commit までに配送された変更は、relay から購読が
//! 見えないまま配送済みになり、wait は timeout まで起きなかった。購読の commit の後に 1 回だけ、取りこぼし得た
//! 変更を outbox から引き直し、relay と同じ照合（実行主体の見え方・filter・停止より後か）で配る。
//!
//! - **行に束縛した wait**: 実行主体の権限で行を読み直し、いまの rev が停止時の rev（`suspend_rev`）より大きい
//!   ときだけ、その行の `suspend_rev` より後の変更を引き直す（動いていなければ outbox を読まない）。
//! - **テーブル単位の wait**: 停止時の outbox の位置（`after_event_id`）より後のそのテーブルの変更を、
//!   [`TABLE_RECHECK_LIMIT`] 件まで引き直す。購読の commit の後に配送された変更は relay が照合済みで、照合し
//!   直しても結果は変わらない（起こしたなら wait はもう待っていない）。
//!
//! 引き直せるのは outbox に残っている変更だけ（保持期間の GC で消えた変更は拾えない）。テーブル単位の wait の
//! 並行の書込（位置より前に採番され、後で commit したもの）は、この突合でも拾わない（engine.md §9.2）。
//! 一時障害は印を付けずに次の tick で突き合わせ直す。

use workflow_engine::DataWaitCandidate;

use super::super::relay::RelayDeps;
use super::super::relay_origin::{cause_runs, Causes};
use super::{data_event, relay_tenant_waits, BatchCtx, DataEvent, PrincipalKey};

/// 1 tick で突き合わせる wait の上限（超えた分は次の tick）。
const RECONCILE_BATCH: i64 = 100;
/// テーブル単位の wait で引き直す変更の上限。
const TABLE_RECHECK_LIMIT: i64 = 500;
/// 行に束縛した wait で引き直す変更の上限。
const RECORD_RECHECK_LIMIT: i64 = 100;

/// 受付の突合がまだのデータ変更の wait を突き合わせる（relay の tick・結合テストも同じ口）。
pub(in super::super) async fn reconcile_data_waits(
    deps: &RelayDeps<'_>,
) -> Result<(), anyhow::Error> {
    for w in deps.runs.data_waits_to_reconcile(RECONCILE_BATCH).await? {
        match reconcile_one(deps, &w).await {
            Ok(()) => deps.runs.mark_data_wait_reconciled(&w).await?,
            Err(e) => {
                tracing::warn!(error = %e, run_id = %w.subscriber.run_id, step = %w.subscriber.step_path,
                    "データ変更の wait の受付の突合に失敗（次の tick で再試行）");
            }
        }
    }
    Ok(())
}

/// 1 つの wait を突き合わせる（取りこぼし得た変更が無ければ何もしない）。
async fn reconcile_one(deps: &RelayDeps<'_>, w: &DataWaitCandidate) -> Result<(), anyhow::Error> {
    let tenant = w.subscriber.tenant_id.as_str();
    let (record, after_id, after_version, limit) = if let Some(record) = w.record_id {
        let Some(suspend_rev) = w.suspend_rev() else {
            return Ok(());
        };
        match current_rev(deps, w, record).await? {
            Some(rev) if rev > suspend_rev => (Some(record), 0, suspend_rev, RECORD_RECHECK_LIMIT),
            // 動いていない・見えない・消えた（relay も起こさない）。
            _ => return Ok(()),
        }
    } else {
        let Some(after) = w.after_event_id() else {
            return Ok(());
        };
        (None, after, i64::MIN, TABLE_RECHECK_LIMIT)
    };
    let events = {
        let mut conn = deps.db.acquire().await?;
        storage::event::data_change_events_after(
            &mut conn,
            tenant,
            w.table_id,
            record,
            after_id,
            after_version,
            limit,
        )
        .await?
    };
    let evs: Vec<DataEvent<'_>> = events.iter().filter_map(data_event).collect();
    if evs.is_empty() {
        return Ok(());
    }
    let refs: Vec<&storage::event::OutboxEvent> = evs.iter().map(|e| e.event).collect();
    let causes = cause_runs(deps.db, &refs).await?;
    let mut cx = BatchCtx::new(deps, tenant, &causes);
    relay_tenant_waits(&mut cx, &evs, std::slice::from_ref(w)).await
}

/// 実行主体の権限で読んだ行のいまの rev（委譲・権限が無い・行が見えない・消えたなら `None`・一時障害は `Err`）。
async fn current_rev(
    deps: &RelayDeps<'_>,
    w: &DataWaitCandidate,
    record: uuid::Uuid,
) -> Result<Option<i64>, anyhow::Error> {
    let none = Causes::new();
    let cx = BatchCtx::new(deps, &w.subscriber.tenant_id, &none);
    let Some(actor) = cx
        .actor(&PrincipalKey::of(&w.subscriber), w.table_id)
        .await?
    else {
        return Ok(None);
    };
    match actor
        .data
        .get_record(&actor.ctx, w.table_id, record, None)
        .await
    {
        Ok(r) => Ok(Some(r.rev)),
        Err(e) if e.is_transient() => Err(anyhow::anyhow!("行の読み直しに失敗: {e}")),
        Err(_) => Ok(None),
    }
}
