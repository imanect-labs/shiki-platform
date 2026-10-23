//! 行数カウンタと値頻度表の更新（outbox 消費側・query-optimizer.md §4.1）。
//!
//! 台帳 consumer [`STATS_CONSUMER`] として outbox を claim し、`data.record.changed` の
//! create / update / delete の前後の値（非公開部 `counts.b` / `counts.a`）から差分を集め、
//! テーブルごとに 1 文で `cnt = cnt + excluded.cnt` する。**書込 Tx では更新しない**。
//!
//! - **冪等性**: 反映と `mark_delivered` を同じ Tx で行う（同じ変更を二度足さない）。並行する消費は
//!   台帳 consumer の採番行のロックで直列化する（統計収集の数え直しもこのロックを取る）。
//! - **数え直しとの整合**: 統計収集が索引から数え直した時点で、まだ消費していない変更は
//!   `skip_events` に記録される（数え直しに含まれている）。消費側はそれを足さずに捨てる。
//! - 数え直す前（`counts_ready = false`）の表は変更行数だけ数える（統計収集の起動条件）。
//!   前後の値を持たない古い形式の変更に出会ったら `counts_ready` を倒し、数え直しを積む。
//! - 種類数が上限（[`VALUE_COUNT_MAX_DISTINCT`]）を超えた列は値頻度表から外す。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use super::{analyze_due, STATS_ANALYZE_QUEUE, STATS_CONSUMER, VALUE_COUNT_MAX_DISTINCT};
use crate::index::normalize::prefix_at_char_boundary;
use crate::index::MAX_INDEX_KEY_BYTES;
use crate::model::FieldId;
use crate::store::DataStore;
use crate::stream::outbox::{ChangeKind, RECORD_CHANGED_EVENT};
use crate::{map_db, DataError};

/// 1 バッチで claim するイベント数。
const STATS_BATCH: i64 = 2_000;

/// 1 件の変更の差分（event_id, 行数の増減, (列, 値, 増減)）。
type EventDelta = (i64, i64, Vec<(FieldId, String, i64)>);

/// 1 テーブル分の集計（このバッチ）。
#[derive(Default)]
struct TableDelta {
    tenant_id: String,
    table_id: Option<Uuid>,
    /// 変更のあった行数（統計収集の起動条件）。
    changes: i64,
    /// (event_id, 行数の増減, 値の増減)。
    events: Vec<EventDelta>,
    /// 前後の値を持たない変更があった（数え直しへ倒す）。
    legacy: bool,
}

/// 制御行（自分が書く統計の管理列だけ）。
#[derive(sqlx::FromRow)]
struct Control {
    table_int: i64,
    counts_ready: bool,
    counts_fields: Vec<i16>,
    skip_events: Vec<i64>,
    n_rows: i64,
    changes_since: i64,
}

impl DataStore {
    /// 統計の台帳 consumer を登録する（初回のみ既存のバックログを飛ばす・GC がこの配送を待つ）。
    pub async fn register_stats_consumer(&self) -> Result<(), DataError> {
        for (_, pool) in self.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            storage::event::register_consumer(&mut tx, STATS_CONSUMER)
                .await
                .map_err(|e| DataError::Internal(format!("outbox: {e}")))?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(())
    }

    /// outbox の未消費の変更を 1 バッチ、行数カウンタと値頻度表へ反映する（全プール）。処理した outbox の
    /// 件数（レコード変更以外も含む）を返す。並行に呼んでよい（台帳 consumer の行ロックで直列化）。
    pub async fn consume_stats_once(&self) -> Result<usize, DataError> {
        let mut total = 0;
        for (_, pool) in self.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            total += consume_batch(&mut tx).await?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(total)
    }
}

#[allow(clippy::needless_pass_by_value)] // map_err へそのまま渡す。
fn st(e: storage::StorageError) -> DataError {
    DataError::Internal(format!("outbox: {e}"))
}

async fn consume_batch(tx: &mut PgConnection) -> Result<usize, DataError> {
    // 消費と数え直しの排他（コミットまで保持）。
    storage::event::lock_consumer_seq(&mut *tx, STATS_CONSUMER, None)
        .await
        .map_err(st)?;
    let events = storage::event::claim_undelivered(&mut *tx, STATS_CONSUMER, STATS_BATCH)
        .await
        .map_err(st)?;
    if events.is_empty() {
        return Ok(0);
    }
    let change_ids: Vec<i64> = events
        .iter()
        .filter(|e| {
            e.payload.get("event_type").and_then(Value::as_str) == Some(RECORD_CHANGED_EVENT)
        })
        .map(|e| e.id)
        .collect();
    let privates = storage::event::private_payloads(&mut *tx, &change_ids)
        .await
        .map_err(st)?;
    // (tenant_int, table_int) → 集計。BTreeMap でロック順を固定する。
    let mut tables: BTreeMap<(i32, i64), TableDelta> = BTreeMap::new();
    for e in &events {
        let Some(p) = privates.get(&e.id) else {
            continue;
        };
        let kind = e
            .payload
            .get("change")
            .and_then(Value::as_str)
            .and_then(ChangeKind::parse);
        let Some(kind) = kind else { continue };
        if !matches!(
            kind,
            ChangeKind::Create | ChangeKind::Update | ChangeKind::Delete | ChangeKind::Derived
        ) {
            continue;
        }
        let Some(table_int) = p.get("table_int").and_then(Value::as_i64) else {
            continue;
        };
        let counts = p.get("counts");
        let Some(tenant_int) = counts
            .and_then(|c| c.get("tenant_int"))
            .and_then(Value::as_i64)
            .and_then(|t| i32::try_from(t).ok())
        else {
            continue;
        };
        let d = tables.entry((tenant_int, table_int)).or_default();
        d.tenant_id.clone_from(&e.tenant_id);
        d.table_id = d.table_id.or_else(|| {
            e.payload
                .get("table_id")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        });
        d.changes += 1;
        if kind == ChangeKind::Derived {
            // 材料化した派生列の値だけが変わった（行数・分類列の値頻度は変わらない）。
            continue;
        }
        let Some(c) = counts else {
            d.legacy = true;
            continue;
        };
        let rows = match kind {
            ChangeKind::Create => 1,
            ChangeKind::Delete => -1,
            _ => 0,
        };
        let mut vals = Vec::new();
        add_image(c.get("b"), -1, &mut vals);
        add_image(c.get("a"), 1, &mut vals);
        d.events.push((e.id, rows, vals));
    }
    for ((tenant_int, table_int), d) in &tables {
        apply_table(tx, *tenant_int, *table_int, d).await?;
    }
    let all: Vec<i64> = events.iter().map(|e| e.id).collect();
    storage::event::mark_delivered(&mut *tx, STATS_CONSUMER, &all)
        .await
        .map_err(st)?;
    Ok(events.len())
}

/// 前後の値の材料（`{owner, data: {"f{id}": 値}}`）を (列, 値, 符号) へ。多値は要素ごと（重複は 1 回）。
fn add_image(img: Option<&Value>, sign: i64, out: &mut Vec<(FieldId, String, i64)>) {
    let Some(img) = img.filter(|v| v.is_object()) else {
        return;
    };
    let norm = |s: &str| {
        prefix_at_char_boundary(s, MAX_INDEX_KEY_BYTES)
            .0
            .to_string()
    };
    if let Some(o) = img.get("owner").and_then(Value::as_str) {
        out.push((crate::model::SYSTEM_FIELD_OWNER, norm(o), sign));
    }
    let Some(data) = img.get("data").and_then(Value::as_object) else {
        return;
    };
    for (k, v) in data {
        let Some(id) = k.strip_prefix('f').and_then(|n| n.parse::<FieldId>().ok()) else {
            continue;
        };
        let mut seen = BTreeSet::new();
        match v {
            Value::String(s) => {
                seen.insert(norm(s));
            }
            Value::Array(a) => {
                for x in a.iter().filter_map(Value::as_str) {
                    seen.insert(norm(x));
                }
            }
            _ => {}
        }
        out.extend(seen.into_iter().map(|s| (id, s, sign)));
    }
}

async fn apply_table(
    tx: &mut PgConnection,
    tenant_int: i32,
    table_int: i64,
    d: &TableDelta,
) -> Result<(), DataError> {
    // 制御行を確保してロックする（無ければ作る・数え直し前の状態）。
    sqlx::query(
        "INSERT INTO data_table_stats (tenant_int, table_int) VALUES ($1, $2) \
         ON CONFLICT (tenant_int, table_int) DO NOTHING",
    )
    .bind(tenant_int)
    .bind(table_int)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    let c: Control = sqlx::query_as(
        "SELECT table_int, counts_ready, counts_fields, skip_events, n_rows, changes_since \
         FROM data_table_stats WHERE tenant_int = $1 AND table_int = $2 FOR UPDATE",
    )
    .bind(tenant_int)
    .bind(table_int)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db)?;
    debug_assert_eq!(c.table_int, table_int);
    let mut skip: BTreeSet<i64> = c.skip_events.iter().copied().collect();
    let fields: BTreeSet<FieldId> = c
        .counts_fields
        .iter()
        .filter_map(|f| FieldId::try_from(*f).ok())
        .collect();
    let ready = c.counts_ready && !d.legacy;
    let mut rows = 0i64;
    let mut agg: HashMap<(FieldId, String), i64> = HashMap::new();
    for (id, r, vals) in &d.events {
        // 数え直しの時点で索引に反映済みだった変更は足さない（消費したので外す）。
        if skip.remove(id) {
            continue;
        }
        rows += r;
        for (f, v, s) in vals {
            if fields.contains(f) {
                *agg.entry((*f, v.clone())).or_default() += s;
            }
        }
    }
    let skip_left: Vec<i64> = skip.into_iter().collect();
    sqlx::query(
        "UPDATE data_table_stats SET n_rows = CASE WHEN $3 THEN greatest(n_rows + $4, 0) ELSE n_rows END, \
           changes_since = changes_since + $5, counts_ready = $3, skip_events = $6, updated_at = now() \
         WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(tenant_int)
    .bind(table_int)
    .bind(ready)
    .bind(rows)
    .bind(d.changes)
    .bind(&skip_left)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    if ready {
        apply_counts(tx, tenant_int, table_int, &agg).await?;
    }
    // 統計収集の起動（変更行数が閾値を超えた・数え直しが要る）。未着手のジョブがあれば積まない。
    let due = analyze_due(c.changes_since + d.changes, c.n_rows + rows) || !ready;
    if let (true, Some(table_id)) = (due, d.table_id) {
        enqueue_analyze(tx, &d.tenant_id, table_id).await?;
    }
    Ok(())
}

async fn apply_counts(
    tx: &mut PgConnection,
    tenant_int: i32,
    table_int: i64,
    agg: &HashMap<(FieldId, String), i64>,
) -> Result<(), DataError> {
    let mut f = Vec::with_capacity(agg.len());
    let mut v = Vec::with_capacity(agg.len());
    let mut n = Vec::with_capacity(agg.len());
    for ((field, val), d) in agg {
        if *d == 0 {
            continue;
        }
        f.push(i16::try_from(*field).unwrap_or(i16::MAX));
        v.push(val.clone());
        n.push(*d);
    }
    if f.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO data_value_count (tenant_int, table_int, field_id, val, cnt) \
         SELECT $1, $2, u.f, u.v, u.n FROM unnest($3::int2[], $4::text[], $5::int8[]) AS u(f, v, n) \
         ON CONFLICT (tenant_int, table_int, field_id, val) \
         DO UPDATE SET cnt = data_value_count.cnt + excluded.cnt",
    )
    .bind(tenant_int)
    .bind(table_int)
    .bind(&f)
    .bind(&v)
    .bind(&n)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    let touched: Vec<i16> = f
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    sqlx::query(
        "DELETE FROM data_value_count \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = ANY($3) AND cnt <= 0",
    )
    .bind(tenant_int)
    .bind(table_int)
    .bind(&touched)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    // 種類数が上限を超えた列は値頻度表から外す（推定はフィールド統計へ）。
    let over: Vec<i16> = sqlx::query_scalar(
        "SELECT field_id FROM data_value_count \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = ANY($3) \
         GROUP BY field_id HAVING count(*) > $4",
    )
    .bind(tenant_int)
    .bind(table_int)
    .bind(&touched)
    .bind(VALUE_COUNT_MAX_DISTINCT)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db)?;
    if !over.is_empty() {
        sqlx::query(
            "DELETE FROM data_value_count \
             WHERE tenant_int = $1 AND table_int = $2 AND field_id = ANY($3)",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(&over)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "UPDATE data_table_stats SET counts_fields = ARRAY(SELECT unnest(counts_fields) \
               EXCEPT SELECT unnest($3::int2[])) \
             WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(&over)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
    }
    Ok(())
}

/// 統計収集ジョブを積む（未着手の同じジョブがあれば積まない）。
pub(crate) async fn enqueue_analyze(
    conn: &mut PgConnection,
    tenant_id: &str,
    table_id: Uuid,
) -> Result<(), DataError> {
    let payload = serde_json::json!({ "table_id": table_id });
    jobq::enqueue_unless_pending_on(
        conn,
        jobq::NewJob {
            queue: STATS_ANALYZE_QUEUE,
            tenant_id,
            payload: &payload,
            trace_id: None,
            max_attempts: 5,
        },
    )
    .await
    .map(|_| ())
    .map_err(|e| DataError::Internal(format!("jobq: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn images_become_signed_counts_with_multi_dedup() {
        let mut out = Vec::new();
        add_image(
            Some(&json!({"owner": "alice", "data": {"f16": "open", "f17": ["a", "b", "a"]}})),
            -1,
            &mut out,
        );
        add_image(
            Some(&json!({"owner": "alice", "data": {"f16": "done"}})),
            1,
            &mut out,
        );
        add_image(None, 1, &mut out);
        out.sort();
        assert_eq!(
            out,
            vec![
                (0, "alice".into(), -1),
                (0, "alice".into(), 1),
                (16, "done".into(), 1),
                (16, "open".into(), -1),
                (17, "a".into(), -1),
                (17, "b".into(), -1),
            ]
        );
    }
}
