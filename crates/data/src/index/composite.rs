//! 複数ソート用の複合索引（data-platform §5.3・オンデマンド材料化）。
//!
//! 第 1 キーの同値群がページの K 倍を超える複数ソートは、同値群の先読み（ページ内ソート）では一定コストに
//! ならない。その組み合わせを `data_sort_set` に登録し、レコードごとに全順序を保つバイト列
//! （[`encode_pos`]）を `data_index_composite` に 1 行ずつ持つ。生成は索引の自動昇格（§10）と同じ機構:
//!
//! ```text
//! 登録（building・data_table 行を FOR UPDATE）→ 書込は write-through → jobq でバックフィル → active
//! ```
//!
//! - 書込は `data_table` 行を `FOR KEY SHARE` で押さえてから組み合わせを読む（[`super::state::lock_plan`]）。
//!   登録は同じ行を `FOR UPDATE` で押さえるので、登録前に計画を読んだ書込は登録より先に必ず終わる
//! - バックフィルは登録時点の最大 `record_int`（high_water）までをレコード行 `FOR SHARE` で読んで作る
//! - 値は**索引テーブルの値**（text は格納プレフィクス）から作る。全順序とカーソルの定義と一致する
//! - **ランタイム DDL は打たない**（行の追記・削除だけ）。組み合わせの数はテーブルあたり [`MAX_SORT_SETS`] まで

use std::collections::HashMap;

use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::state::field_id_i16;
use crate::model::{FieldId, FieldType, TableKeys, TableSchema};
use crate::query::bound::FieldKind;
use crate::query::planner::keys::{encode_pos, Pos, SortVal};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// テーブルあたりの複合索引（ソートの組み合わせ）の上限（PIT-72: 書込増幅と肥大化）。
pub(crate) const MAX_SORT_SETS: i64 = 8;
/// バックフィル 1 チャンク（1 Tx）のレコード数。
pub(crate) const COMPOSITE_CHUNK: i64 = 500;

/// 登録済みのソートの組み合わせ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SortSetDef {
    pub id: i16,
    /// (field_id, 降順か)。
    pub keys: Vec<(FieldId, bool)>,
    pub active: bool,
}

/// 組み合わせの正準表記（`"16:a,17:d"`）。
pub(crate) fn spec_of(keys: &[(FieldId, bool)]) -> String {
    keys.iter()
        .map(|(f, d)| format!("{f}:{}", if *d { "d" } else { "a" }))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_spec(s: &str) -> Option<Vec<(FieldId, bool)>> {
    s.split(',')
        .map(|p| {
            let (f, d) = p.split_once(':')?;
            Some((f.parse().ok()?, d == "d"))
        })
        .collect()
}

/// フィールドの値索引の持ち方（number は numeric・それ以外と擬似列は text）。
fn kind_of(schema: &TableSchema, f: FieldId) -> FieldKind {
    match schema.field_by_id(f).map(|d| d.field_type) {
        Some(FieldType::Number) => FieldKind::Num,
        _ => FieldKind::Text,
    }
}

/// テーブルの組み合わせ（building / active）。
pub(crate) async fn load_sets(
    conn: &mut PgConnection,
    keys: TableKeys,
) -> Result<Vec<SortSetDef>, DataError> {
    let rows: Vec<(i16, String, String)> = sqlx::query_as(
        "SELECT sort_set_id, spec, state FROM data_sort_set \
         WHERE tenant_int = $1 AND table_int = $2 ORDER BY sort_set_id",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, spec, state)| {
            Some(SortSetDef {
                id,
                keys: parse_spec(&spec)?,
                active: state == "active",
            })
        })
        .collect())
}

/// 1 レコード × 1 組み合わせの複合キーを索引テーブルの値から作り直す（書込 Tx）。
pub(crate) async fn refresh(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
    set: &SortSetDef,
    record_int: i64,
) -> Result<(), DataError> {
    refresh_many(conn, keys, schema, set, &[record_int]).await
}

/// 複数レコード × 1 組み合わせの複合キーを作り直す（値の読取 1 文・書込 1 文。バックフィルのチャンク）。
pub(crate) async fn refresh_many(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
    set: &SortSetDef,
    record_ints: &[i64],
) -> Result<(), DataError> {
    if record_ints.is_empty() {
        return Ok(());
    }
    let fields: Vec<i16> = set
        .keys
        .iter()
        .map(|(f, _)| field_id_i16(*f))
        .collect::<Result<_, _>>()?;
    let vals: Vec<(i64, i16, String)> = sqlx::query_as(
        "SELECT record_int, field_id, val FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int = ANY($3) AND field_id = ANY($4) AND ord = 0 \
         UNION ALL \
         SELECT record_int, field_id, val::text FROM data_index_num \
         WHERE tenant_int = $1 AND table_int = $2 AND record_int = ANY($3) AND field_id = ANY($4) AND ord = 0",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(record_ints)
    .bind(&fields)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    let mut by: HashMap<(i64, i16), String> = HashMap::with_capacity(vals.len());
    for (r, f, v) in vals {
        by.insert((r, f), v);
    }
    let kinds: Vec<(FieldKind, bool)> = set
        .keys
        .iter()
        .map(|(f, d)| (kind_of(schema, *f), *d))
        .collect();
    let encoded: Vec<Vec<u8>> = record_ints
        .iter()
        .map(|r| {
            let pos = Pos {
                vals: fields
                    .iter()
                    .zip(&kinds)
                    .map(|(f, (k, _))| SortVal::from_db(*k, by.get(&(*r, *f)).cloned()))
                    .collect(),
                rint: *r,
            };
            encode_pos(&kinds, &pos)
        })
        .collect();
    sqlx::query(
        "INSERT INTO data_index_composite (tenant_int, table_int, sort_set_id, record_int, val) \
         SELECT $1, $2, $3, u.r, u.v FROM unnest($4::bigint[], $5::bytea[]) AS u(r, v) \
         ON CONFLICT (tenant_int, table_int, sort_set_id, record_int) DO UPDATE SET val = excluded.val",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(set.id)
    .bind(record_ints)
    .bind(&encoded)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// レコードの複合キーを全て消す（delete）。
pub(crate) async fn delete_record(
    conn: &mut PgConnection,
    keys: TableKeys,
    record_int: i64,
) -> Result<(), DataError> {
    sqlx::query(
        "DELETE FROM data_index_composite WHERE tenant_int = $1 AND table_int = $2 AND record_int = $3",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(record_int)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// 値索引が保守されなくなった列を含む組み合わせを捨てる（スキーマ改訂の Tx 上・`data_table` 行 FOR UPDATE 済み）。
pub(crate) async fn prune(
    conn: &mut PgConnection,
    keys: TableKeys,
    still_indexed: impl Fn(FieldId) -> bool,
) -> Result<(), DataError> {
    for set in load_sets(conn, keys).await? {
        if set.keys.iter().all(|(f, _)| still_indexed(*f)) {
            continue;
        }
        sqlx::query(
            "DELETE FROM data_index_composite \
             WHERE tenant_int = $1 AND table_int = $2 AND sort_set_id = $3",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(set.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
        sqlx::query(
            "DELETE FROM data_sort_set WHERE tenant_int = $1 AND table_int = $2 AND sort_set_id = $3",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(set.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    }
    Ok(())
}

impl DataStore {
    /// ソートの組み合わせの複合索引を要求する（オンデマンド材料化・登録済み・上限超過なら何もしない）。
    ///
    /// `data_table` 行を `FOR UPDATE` で押さえて building で登録し、バックフィルのジョブを積む
    /// （既存レコードが無ければ即 active）。
    pub(crate) async fn request_sort_set(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        keys: TableKeys,
        set: &[(FieldId, bool)],
    ) -> Result<(), DataError> {
        let spec = spec_of(set);
        // 登録済み・上限到達なら data_table 行を押さえない（読取のたびに書込を待たせない）。
        let (count, exists): (i64, bool) = sqlx::query_as(
            "SELECT count(*), coalesce(bool_or(spec = $3), false) \
             FROM data_sort_set WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(&spec)
        .fetch_one(self.pools.for_tenant(tenant_id))
        .await
        .map_err(map_db)?;
        if exists || count >= MAX_SORT_SETS {
            return Ok(());
        }
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let live: Option<i64> = sqlx::query_scalar(
            "SELECT table_int FROM data_table \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        if live.is_none() {
            return Ok(());
        }
        let (count, exists, next): (i64, bool, i32) = sqlx::query_as(
            "SELECT count(*), coalesce(bool_or(spec = $3), false), coalesce(max(sort_set_id), 0)::int + 1 \
             FROM data_sort_set WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(&spec)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        if exists || count >= MAX_SORT_SETS {
            return Ok(());
        }
        let high: i64 = sqlx::query_scalar(
            "SELECT coalesce(max(record_int), 0) FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        let state = if high == 0 { "active" } else { "building" };
        sqlx::query(
            "INSERT INTO data_sort_set (tenant_int, table_int, sort_set_id, spec, state, high_water) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(i16::try_from(next).unwrap_or(i16::MAX))
        .bind(&spec)
        .bind(state)
        .bind(high)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if high > 0 {
            jobq::enqueue_on(
                &mut tx,
                jobq::NewJob {
                    queue: super::INDEX_BACKFILL_QUEUE,
                    tenant_id,
                    payload: &json!({ "op": "composite", "table_id": table_id, "v": 1 }),
                    trace_id: None,
                    max_attempts: 10,
                },
            )
            .await
            .map_err(|e| DataError::Internal(format!("jobq: {e}")))?;
        }
        tx.commit().await.map_err(map_db)?;
        tracing::info!(%table_id, spec, "複合索引の材料化を開始しました");
        Ok(())
    }

    /// 複合索引のバックフィルを 1 チャンク進める（冪等・中断再開可）。全て active なら `true`。
    pub async fn sort_set_backfill_step(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        max_records: i64,
    ) -> Result<bool, DataError> {
        let mut tx = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        let meta: Option<(i32, i64, sqlx::types::Json<TableSchema>)> = sqlx::query_as(
            "SELECT tenant_int, table_int, schema FROM data_table WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((tenant_int, table_int, schema)) = meta else {
            return Ok(true);
        };
        let keys = TableKeys {
            tenant_int,
            table_int,
        };
        let row: Option<(i16, String, i64, i64)> = sqlx::query_as(
            "SELECT sort_set_id, spec, cursor_record, high_water FROM data_sort_set \
             WHERE tenant_int = $1 AND table_int = $2 AND state = 'building' \
             ORDER BY sort_set_id LIMIT 1 FOR UPDATE",
        )
        .bind(tenant_int)
        .bind(table_int)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some((id, spec, cursor, high)) = row else {
            return Ok(true);
        };
        let set = SortSetDef {
            id,
            keys: parse_spec(&spec).unwrap_or_default(),
            active: false,
        };
        // FOR SHARE: 並行する書込（FOR UPDATE）と直列化し、古い値から作らない。
        let ints: Vec<i64> = sqlx::query_scalar(
            "SELECT record_int FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND record_int > $3 AND record_int <= $4 \
             ORDER BY record_int LIMIT $5 FOR SHARE",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(cursor)
        .bind(high)
        .bind(max_records.max(1))
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?;
        refresh_many(&mut tx, keys, &schema.0, &set, &ints).await?;
        let done = (ints.len() as i64) < max_records.max(1);
        sqlx::query(
            "UPDATE data_sort_set SET cursor_record = $4, state = $5, updated_at = now() \
             WHERE tenant_int = $1 AND table_int = $2 AND sort_set_id = $3",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(id)
        .bind(ints.last().copied().unwrap_or(cursor))
        .bind(if done { "active" } else { "building" })
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        if !done {
            return Ok(false);
        }
        // 次の building が残っていれば続ける。
        Box::pin(self.sort_set_backfill_step(tenant_id, table_id, max_records)).await
    }

    /// 複合索引のバックフィルを最後まで進める（同期実行・小さいテーブル向け／テスト用）。
    pub async fn backfill_sort_sets(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<(), DataError> {
        while !self
            .sort_set_backfill_step(tenant_id, table_id, COMPOSITE_CHUNK)
            .await?
        {}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_roundtrips() {
        let k = vec![(16, false), (2, true)];
        assert_eq!(spec_of(&k), "16:a,2:d");
        assert_eq!(parse_spec(&spec_of(&k)), Some(k));
        assert_eq!(parse_spec("x"), None);
    }
}
