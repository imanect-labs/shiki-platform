//! データ変更の wait(event) の候補と起床（GAP-07 の T3・engine.md §5.5）。
//!
//! storage.write の wait（[`super::wait`]）はフォルダの祖先束縛で引くが、データ変更の wait はテーブル
//! （と任意で行）に束縛する。**空の scope をワイルドカードにしない**（テーブルの無い購読は一致しない）。
//! 変更の見え方（run の実行主体に見えるか・マスク後の値）は呼び出し側（api の relay）が data の
//! `change_event_view` で作り、ここは「その変更を待っているか」と起床だけを受け持つ。
//!
//! 登録より前の変更では起こさない: 行に束縛した wait は停止時の rev（`suspend_rev`）より後の変更だけ、
//! テーブル単位の wait は checkpoint の時点の outbox の位置（`after_event_id`）より後のイベントだけを見る。
//!
//! 承認（human.approval・GAP-08）もここで候補になる（その行の `data.transition`・`suspend_rev` より後）。起床は
//! 遷移先の状態を出口へ写し、台帳を同じ Tx で確定する（`approval.rs`）。
//!
//! **受付の突合**（BUG-42）: 停止の時点（行の rev・outbox の位置を読んだ時点）から購読の commit までに配送された
//! 変更は、relay から購読が見えないまま配送済みになる。購読の commit の後に 1 回だけ、その変更を突き合わせ直す
//! 候補を [`RunStore::data_waits_to_reconcile`] が返す（承認は台帳の受付の突合が埋めるので含めない）。照合と
//! 起床は relay と同じ（api の relay が outbox から変更を引き直して配る）。

use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

use super::approval::{wake_approval, ApprovalWait};
use super::wait::{graph_from_snapshot, wake_step_and_advance, EventWaitSubscriber};
use super::{map_db, RunStore, RunStoreError};
use crate::vocab::EventSource;

/// データ変更を待っている wait(event) 1 件。
#[derive(Debug, Clone)]
pub struct DataWaitCandidate {
    pub subscriber: EventWaitSubscriber,
    pub source: EventSource,
    pub table_id: Uuid,
    /// 行に束縛した wait の行（テーブル単位なら `None`）。
    pub record_id: Option<Uuid>,
    suspend_rev: Option<i64>,
    after_event_id: Option<i64>,
    filter: Option<Value>,
    /// 承認の購読（human.approval）なら、その材料。
    approval: Option<ApprovalWait>,
}

impl DataWaitCandidate {
    /// 承認の購読か（human.approval・起床で台帳を確定する）。
    #[must_use]
    pub fn approval(&self) -> Option<&ApprovalWait> {
        self.approval.as_ref()
    }

    /// 行に束縛した wait の停止時の rev（これ以下の変更では起きない・テーブル単位なら `None`）。
    #[must_use]
    pub fn suspend_rev(&self) -> Option<i64> {
        self.suspend_rev
    }

    /// テーブル単位の wait の停止時の outbox の位置（これ以下のイベントでは起きない・行に束縛した wait は `None`）。
    #[must_use]
    pub fn after_event_id(&self) -> Option<i64> {
        self.after_event_id
    }

    /// このイベント（source・テーブル・行・変更後の rev・outbox の id）を待っているか。
    #[must_use]
    pub fn wants(
        &self,
        source: EventSource,
        table_id: Uuid,
        record_id: Uuid,
        rev: i64,
        event_id: i64,
    ) -> bool {
        self.source == source
            && self.table_id == table_id
            && self.record_id.is_none_or(|r| r == record_id)
            && self.suspend_rev.is_none_or(|s| rev > s)
            && self.after_event_id.is_none_or(|a| event_id > a)
    }

    /// filter がペイロードに一致するか（fail-closed: 不正 filter・評価エラーは一致しない）。
    #[must_use]
    pub fn admits(&self, payload: &Value) -> bool {
        crate::scheduler::store::trigger_filter_admits(
            self.filter.as_ref(),
            payload,
            &self.subscriber.tenant_id,
            &self.subscriber.step_path,
        )
    }
}

#[derive(sqlx::FromRow)]
struct DataWaitRow {
    #[sqlx(flatten)]
    subscriber: EventWaitSubscriber,
    source: String,
    spec: Json<Value>,
}

impl DataWaitRow {
    /// 購読の行を候補にする（spec が読めない・テーブルの無い購読は `None`＝一致しない）。
    fn into_candidate(self) -> Option<DataWaitCandidate> {
        let spec = self.spec.0;
        let scope = spec.get("scope")?;
        let id = |k: &str| {
            scope
                .get(k)
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
        };
        Some(DataWaitCandidate {
            source: EventSource::parse(&self.source)?,
            table_id: id("table")?,
            record_id: id("record"),
            suspend_rev: spec.get("suspend_rev").and_then(Value::as_i64),
            after_event_id: spec.get("after_event_id").and_then(Value::as_i64),
            filter: spec.get("filter").filter(|f| !f.is_null()).cloned(),
            approval: ApprovalWait::from_spec(&spec),
            subscriber: self.subscriber,
        })
    }
}

/// 受付の突合の走査の 1 行（run が実行中か・購読の鍵）。
#[derive(sqlx::FromRow)]
struct ReconcileRow {
    #[sqlx(flatten)]
    wait: DataWaitRow,
    live: bool,
}

fn data_sources() -> Vec<&'static str> {
    EventSource::ALL
        .iter()
        .filter(|s| s.is_data())
        .map(|s| s.as_str())
        .collect()
}

impl RunStore {
    /// データ変更の wait(event) の候補を、テーブルの集合でまとめて引く（未発火・run が running のもの）。
    pub async fn data_wait_candidates(
        &self,
        tenant_id: &str,
        tables: &[Uuid],
    ) -> Result<Vec<DataWaitCandidate>, RunStoreError> {
        if tables.is_empty() {
            return Ok(Vec::new());
        }
        let sources = data_sources();
        let keys: Vec<String> = tables.iter().map(|t| t.hyphenated().to_string()).collect();
        let rows: Vec<DataWaitRow> = sqlx::query_as(
            "SELECT w.tenant_id, r.org, w.run_id, w.step_path, r.principal, r.principal_kind, \
                    w.source, w.spec \
             FROM wait_subscription w \
             JOIN workflow_run r ON r.tenant_id = w.tenant_id AND r.run_id = w.run_id \
             WHERE w.tenant_id = $1 AND w.kind = 'event' AND NOT w.fired \
               AND w.source = ANY($2) AND r.status = 'running' \
               AND w.scope_table = ANY($3)",
        )
        .bind(tenant_id)
        .bind(&sources)
        .bind(&keys)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(DataWaitRow::into_candidate)
            .collect())
    }

    /// 受付の突合がまだのデータ変更の wait（承認を除く・作成の古い順・テナントを問わない・BUG-42）。
    ///
    /// 突き合わせようのない購読（run が実行中でない・spec が読めない）は、ここで突合を済ませた印を付けて外す
    /// （次の tick で引き直さない）。返した候補は、呼び出し側が突き合わせ終えたら
    /// [`Self::mark_data_wait_reconciled`] で印を付ける（一時障害なら付けずに次の tick で突き合わせ直す）。
    pub async fn data_waits_to_reconcile(
        &self,
        limit: i64,
    ) -> Result<Vec<DataWaitCandidate>, RunStoreError> {
        let rows: Vec<ReconcileRow> = sqlx::query_as(
            "SELECT w.tenant_id, r.org, w.run_id, w.step_path, r.principal, r.principal_kind, \
                    w.source, w.spec, (r.status = 'running') AS live \
             FROM wait_subscription w \
             JOIN workflow_run r ON r.tenant_id = w.tenant_id AND r.run_id = w.run_id \
             WHERE w.kind = 'event' AND NOT w.fired AND w.reconciled_at IS NULL \
               AND w.source = ANY($1) AND NOT (w.spec ? 'approval') \
             ORDER BY w.created_at LIMIT $2",
        )
        .bind(data_sources())
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut out = Vec::with_capacity(rows.len());
        let mut moot: Vec<EventWaitSubscriber> = Vec::new();
        for r in rows {
            let subscriber = r.wait.subscriber.clone();
            match (r.live, r.wait.into_candidate()) {
                (true, Some(c)) => out.push(c),
                _ => moot.push(subscriber),
            }
        }
        for s in &moot {
            self.mark_reconciled(s).await?;
        }
        Ok(out)
    }

    /// データ変更の wait の受付の突合を済ませた印を付ける（起こした・待ち続ける、のどちらでも）。
    pub async fn mark_data_wait_reconciled(
        &self,
        c: &DataWaitCandidate,
    ) -> Result<(), RunStoreError> {
        self.mark_reconciled(&c.subscriber).await
    }

    async fn mark_reconciled(&self, s: &EventWaitSubscriber) -> Result<(), RunStoreError> {
        sqlx::query(
            "UPDATE wait_subscription SET reconciled_at = now() \
             WHERE tenant_id = $1 AND run_id = $2 AND step_path = $3 AND reconciled_at IS NULL",
        )
        .bind(&s.tenant_id)
        .bind(s.run_id)
        .bind(&s.step_path)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// データ変更の wait(event) を起こす（out ポート・ペイロードを出力に）。既に起きた・取り消された wait は
    /// 何もしない（冪等）。戻り値 = 起こしたか。
    pub async fn wake_data_wait(
        &self,
        c: &DataWaitCandidate,
        payload: &Value,
    ) -> Result<bool, RunStoreError> {
        let snapshot: Option<Json<Value>> = sqlx::query_scalar(
            "SELECT ir_snapshot FROM workflow_run WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(&c.subscriber.tenant_id)
        .bind(c.subscriber.run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some(graph) = snapshot.and_then(|s| graph_from_snapshot(s.0)) else {
            return Ok(false);
        };
        if let Some(wait) = &c.approval {
            // 遷移先の状態を出口へ（判断の外の状態は取り下げ）。判断した人はリビジョンの changed_by。
            let res = crate::approval::ApprovalResolution::transition(
                &wait.choices,
                payload
                    .get("to")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                payload
                    .get("actor")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                payload
                    .get("occurred_at")
                    .and_then(Value::as_str)
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&chrono::Utc)),
                payload.get("rev").and_then(Value::as_i64),
            );
            return wake_approval(
                &self.db,
                &c.subscriber.tenant_id,
                c.subscriber.run_id,
                &c.subscriber.step_path,
                wait,
                &res,
                &graph,
            )
            .await
            .map(|status| status.is_some());
        }
        wake_step_and_advance(
            &self.db,
            &c.subscriber.tenant_id,
            c.subscriber.run_id,
            &c.subscriber.step_path,
            Some("out"),
            payload.clone(),
            &graph,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(
        record: Option<Uuid>,
        suspend_rev: Option<i64>,
        after: Option<i64>,
    ) -> DataWaitCandidate {
        DataWaitCandidate {
            subscriber: EventWaitSubscriber {
                tenant_id: "t".into(),
                org: "o".into(),
                run_id: Uuid::nil(),
                step_path: "w".into(),
                principal: "alice".into(),
                principal_kind: "user".into(),
            },
            source: EventSource::DataRecordUpdated,
            table_id: Uuid::nil(),
            record_id: record,
            suspend_rev,
            after_event_id: after,
            filter: None,
            approval: None,
        }
    }

    /// 登録より前の変更（停止時の rev 以下・outbox の位置以下）と、別の行・別の source では起きない。
    #[test]
    fn waits_ignore_changes_from_before_the_suspend() {
        let row = Uuid::new_v4();
        let upd = EventSource::DataRecordUpdated;
        let c = cand(Some(row), Some(3), None);
        assert!(!c.wants(upd, Uuid::nil(), row, 3, 100), "停止時の rev");
        assert!(c.wants(upd, Uuid::nil(), row, 4, 100));
        assert!(!c.wants(upd, Uuid::nil(), Uuid::new_v4(), 4, 100), "別の行");
        assert!(!c.wants(EventSource::DataTransition, Uuid::nil(), row, 4, 100));
        assert!(!c.wants(upd, Uuid::new_v4(), row, 4, 100), "別のテーブル");
        let t = cand(None, None, Some(50));
        assert!(
            !t.wants(upd, Uuid::nil(), row, 1, 50),
            "停止時の outbox の位置"
        );
        assert!(t.wants(upd, Uuid::nil(), row, 1, 51));
    }
}
