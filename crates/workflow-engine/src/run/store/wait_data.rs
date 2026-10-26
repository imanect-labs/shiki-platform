//! データ変更の wait(event) の候補と起床（GAP-07 の T3・engine.md §5.5）。
//!
//! storage.write の wait（[`super::wait`]）はフォルダの祖先束縛で引くが、データ変更の wait はテーブル
//! （と任意で行）に束縛する。**空の scope をワイルドカードにしない**（テーブルの無い購読は一致しない）。
//! 変更の見え方（run の実行主体に見えるか・マスク後の値）は呼び出し側（api の relay）が data の
//! `change_event_view` で作り、ここは「その変更を待っているか」と起床だけを受け持つ。
//!
//! 登録より前の変更では起こさない: 行に束縛した wait は停止時の rev（`suspend_rev`）より後の変更だけ、
//! テーブル単位の wait は checkpoint の時点の outbox の位置（`after_event_id`）より後のイベントだけを見る。

use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;

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
}

impl DataWaitCandidate {
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
        let sources: Vec<&str> = EventSource::ALL
            .iter()
            .filter(|s| s.is_data())
            .map(|s| s.as_str())
            .collect();
        let keys: Vec<String> = tables.iter().map(|t| t.hyphenated().to_string()).collect();
        let rows: Vec<DataWaitRow> = sqlx::query_as(
            "SELECT w.tenant_id, r.org, w.run_id, w.step_path, r.principal, r.principal_kind, \
                    w.source, w.spec \
             FROM wait_subscription w \
             JOIN workflow_run r ON r.tenant_id = w.tenant_id AND r.run_id = w.run_id \
             WHERE w.tenant_id = $1 AND w.kind = 'event' AND NOT w.fired \
               AND w.source = ANY($2) AND r.status = 'running' \
               AND (w.spec->'scope'->>'table') = ANY($3)",
        )
        .bind(tenant_id)
        .bind(&sources)
        .bind(&keys)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let spec = r.spec.0;
                let scope = spec.get("scope")?;
                let id = |k: &str| {
                    scope
                        .get(k)
                        .and_then(Value::as_str)
                        .and_then(|s| Uuid::parse_str(s).ok())
                };
                Some(DataWaitCandidate {
                    source: EventSource::parse(&r.source)?,
                    table_id: id("table")?,
                    record_id: id("record"),
                    suspend_rev: spec.get("suspend_rev").and_then(Value::as_i64),
                    after_event_id: spec.get("after_event_id").and_then(Value::as_i64),
                    filter: spec.get("filter").filter(|f| !f.is_null()).cloned(),
                    subscriber: r.subscriber,
                })
            })
            .collect())
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
