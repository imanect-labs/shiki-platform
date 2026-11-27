//! 同じ対象の続く保存を 1 回にまとめるイベントトリガの発火（ファイルの新しい版・BUG-28・D-38）。
//!
//! Office やノートの自動保存は、同じファイルの新しい版を数秒〜数分おきに保存する。版ごとに起動すると、
//! 編集のあいだ run が積み上がる。そこで新しい版の保存ではすぐに起動せず、`trigger_firing` の `pending`
//! （`next_attempt_at` = 起動する時刻）として待たせ、同じトリガ・同じファイルの後の保存が来たら前の発火を
//! `coalesced` にして後の発火へ寄せる（後縁のまとめ）。
//!
//! - 起動する時刻 = 最後の保存から [`COALESCE_QUIET_SECS`] 秒。保存が途切れなくても、まとめ始めた保存から
//!   [`COALESCE_MAX_SECS`] 秒で起動する（長い編集のあいだ一度も起動しない、を避ける）。
//! - 起動は起動の再試行と同じ経路（[`SchedulerStore::retry_pending_firings`]）。因果の鎖・queued の上限・
//!   run 開始時の委譲の確認は起動の時点でも効き、run の入力は最後の保存のペイロードになる。filter と
//!   workflow プリンシパルの閲覧の確認（D-50）は、待たせる前に呼び出し側（relay）がイベントごとに行う。
//! - 因果の鎖による抑止は**まとめる前**に判定する。run 自身の保存が、待っている人の保存を押し流して
//!   一緒に抑止される（人の保存で起動しない）のを防ぐ。

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use super::firing::{cause_of, chain_suppression, finish, EventTriggerRef, FireOutcome};
use super::store::{map_db, SchedulerStore, SchedulerStoreError};

/// 同じ対象の保存が途切れてから起動するまでの秒数。ノートの保存（編集中は最大 30 秒おき・`collab::saver`）を
/// 1 回にまとめられるよう、その間隔より長くする。
pub const COALESCE_QUIET_SECS: i64 = 60;
/// まとめ始めた保存から起動するまでの上限（保存が途切れなくても、この間隔で 1 回は起動する）。
pub const COALESCE_MAX_SECS: i64 = 600;

/// まとめる単位と、そのイベントの時刻。
#[derive(Debug, Clone)]
pub struct Coalesce {
    /// まとめる単位（ファイルの id など）。同じトリガ・同じ key の発火を 1 回にまとめる。
    pub key: String,
    /// イベントの起きた時刻（outbox の作成時刻・relay の遅れに左右されない）。
    pub occurred_at: DateTime<Utc>,
}

/// 起動する時刻（最後の保存から静かな時間・まとめ始めから上限のうち早い方）。
fn due_at(occurred_at: DateTime<Utc>, since: DateTime<Utc>) -> DateTime<Utc> {
    (occurred_at + TimeDelta::seconds(COALESCE_QUIET_SECS))
        .min(since + TimeDelta::seconds(COALESCE_MAX_SECS))
}

impl SchedulerStore {
    /// イベント 1 件で 1 つのトリガの発火を、同じ対象の続くイベントとまとめるために待たせる。
    ///
    /// 結果は [`FireOutcome::Deferred`]（待たせた）・[`FireOutcome::Coalesced`]（後のイベントが先に待っている
    /// のでそちらへまとめた）・抑止（因果の鎖）・[`FireOutcome::AlreadyHandled`]（再配送）のどれか。
    /// 待たせた発火は時刻が来たら [`Self::retry_pending_firings`] が起動する。
    pub async fn defer_event_trigger(
        &self,
        t: &EventTriggerRef,
        event_id: i64,
        payload: &Value,
        cause_run_id: Option<Uuid>,
        coalesce: &Coalesce,
    ) -> Result<FireOutcome, SchedulerStoreError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 同じトリガ・同じ対象のまとめを直列化する（並行する relay が両方とも待たせて 2 回起動しない）。
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2 || ':' || $3, 77))",
        )
        .bind(&t.tenant_id)
        .bind(&t.trigger_id)
        .bind(&coalesce.key)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO trigger_firing (tenant_id, trigger_id, event_id, outcome, coalesce_key) \
             VALUES ($1, $2, $3, 'pending', $4) ON CONFLICT DO NOTHING RETURNING event_id",
        )
        .bind(&t.tenant_id)
        .bind(&t.trigger_id)
        .bind(event_id)
        .bind(&coalesce.key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        if inserted.is_none() {
            // 再配送（待たせた・まとめた・起動した、のどれかで既に処理した）。
            tx.commit().await.map_err(map_db)?;
            return Ok(FireOutcome::AlreadyHandled);
        }
        let cause = cause_of(&mut tx, &t.tenant_id, cause_run_id).await?;
        if let Some((outcome, out)) = chain_suppression(cause.as_ref(), t.workflow_id) {
            let out = finish(&mut tx, t, event_id, outcome, out).await?;
            tx.commit().await.map_err(map_db)?;
            return Ok(out);
        }
        // 同じ対象で待っている発火（起動中のものは行ロックで待ち、起動し終えたものは外れる）。
        let waiting: Vec<(i64, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT event_id, coalesce_since FROM trigger_firing \
             WHERE tenant_id = $1 AND trigger_id = $2 AND coalesce_key = $3 AND outcome = 'pending' \
               AND event_id <> $4 \
             FOR UPDATE",
        )
        .bind(&t.tenant_id)
        .bind(&t.trigger_id)
        .bind(&coalesce.key)
        .bind(event_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        if waiting.iter().any(|(id, _)| *id > event_id) {
            // 後のイベントが先に届いて待っている（配る順が前後した）。こちらはそちらへまとめる。
            let out = finish(&mut tx, t, event_id, "coalesced", FireOutcome::Coalesced).await?;
            tx.commit().await.map_err(map_db)?;
            return Ok(out);
        }
        let since = waiting
            .iter()
            .filter_map(|(_, s)| *s)
            .chain([coalesce.occurred_at])
            .min()
            .unwrap_or(coalesce.occurred_at);
        let older: Vec<i64> = waiting.iter().map(|(id, _)| *id).collect();
        if !older.is_empty() {
            sqlx::query(
                "UPDATE trigger_firing SET outcome = 'coalesced', payload = NULL, next_attempt_at = NULL \
                 WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = ANY($3)",
            )
            .bind(&t.tenant_id)
            .bind(&t.trigger_id)
            .bind(&older)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        sqlx::query(
            "UPDATE trigger_firing SET next_attempt_at = $4, payload = $5, cause_run_id = $6, \
                 coalesce_since = $7 \
             WHERE tenant_id = $1 AND trigger_id = $2 AND event_id = $3",
        )
        .bind(&t.tenant_id)
        .bind(&t.trigger_id)
        .bind(event_id)
        .bind(due_at(coalesce.occurred_at, since))
        .bind(Json(payload))
        .bind(cause_run_id)
        .bind(since)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(FireOutcome::Deferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最後の保存から静かな時間で起動し、保存が続いてもまとめ始めから上限で起動する。
    #[test]
    fn due_is_quiet_after_the_last_save_capped_from_the_first() {
        let t0 = Utc::now();
        assert_eq!(due_at(t0, t0), t0 + TimeDelta::seconds(COALESCE_QUIET_SECS));
        let late = t0 + TimeDelta::seconds(COALESCE_MAX_SECS - 10);
        assert_eq!(due_at(late, t0), t0 + TimeDelta::seconds(COALESCE_MAX_SECS));
    }
}
