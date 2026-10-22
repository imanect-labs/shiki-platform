//! リンク（`data_link`）の宣言と書込（data-platform §3.3・§7.1）。
//!
//! - 1 エッジにつき**順方向行と逆方向行の 2 行**を持つ表形式（両方向とも自分側の `table_int` に載るので
//!   1 パーティションへ枝刈りされる）。逆方向行は対称列（`LinkDef.symmetric_field`・Task 15.4）が
//!   宣言されたリンクだけが持つ。v1 の `record_ref` は対称列を持たない片方向リンクなので、
//!   ここで書くのは順方向行だけ。
//! - **カーディナリティは DB で強制する**。行の `single_valued` は書込側が値を渡さず、
//!   `data_link_field`（スキーマ改訂でだけ書き換わる宣言）から `INSERT … SELECT` で写す。
//!   さらに `data_link_field` への複合 FK があるので、生の SQL で `false` を渡しても宣言が `true` なら
//!   FK 違反で落ちる（部分一意 `WHERE single_valued` を迂回できない・PIT-72）。
//! - v1 の `record_ref` は単一 UUID なので src 側は単一値（`single_valued = true`）。

use std::collections::HashMap;

use sqlx::PgConnection;
use uuid::Uuid;

use super::state::field_id_i16;
use crate::model::{FieldId, FieldType, TableKeys, TableSchema};
use crate::{map_db, DataError};

/// スキーマのリンク列を `data_link_field` へ宣言する（作成・改訂・初期化の Tx 上）。
///
/// v1 の `record_ref` は単一値（`single_valued = true`）。カーディナリティの変更（Task 15.4 の
/// `LinkDef`）はスキーマ改訂経路でのみ・既存行の再検証つきで行う。ここでは既存の宣言を書き換えない。
pub(crate) async fn declare_link_fields(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
) -> Result<(), DataError> {
    for f in &schema.fields {
        if f.field_type != FieldType::RecordRef {
            continue;
        }
        sqlx::query(
            "INSERT INTO data_link_field (tenant_int, table_int, field_id, single_valued) \
             VALUES ($1, $2, $3, true) \
             ON CONFLICT (tenant_int, table_int, field_id) DO NOTHING",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(field_id_i16(f.id)?)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    }
    Ok(())
}

/// 参照先テーブル（uuid）→ `table_int` の解決結果のキャッシュ（1 書込 Tx 内だけ）。
#[derive(Default)]
pub(crate) struct LinkResolver {
    tables: HashMap<Uuid, Option<i64>>,
}

impl LinkResolver {
    /// 参照先レコードの `(dst_table_int, dst_record)` を引く。参照先が無ければ `None`
    /// （宙に浮いた参照は索引に載せない。本体の参照整合は書込検証が担う）。
    pub(crate) async fn resolve(
        &mut self,
        conn: &mut PgConnection,
        tenant_id: &str,
        tenant_int: i32,
        ref_table: Uuid,
        record: Uuid,
    ) -> Result<Option<(i64, i64)>, DataError> {
        let table_int = if let Some(t) = self.tables.get(&ref_table) {
            *t
        } else {
            // 削除済みテーブルも引く（論理削除でレコードは残るため、バックフィルで辿れるように）。
            let t: Option<i64> = sqlx::query_scalar(
                "SELECT table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant_id)
            .bind(ref_table)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db)?;
            self.tables.insert(ref_table, t);
            t
        };
        let Some(dst_table) = table_int else {
            return Ok(None);
        };
        let dst: Option<i64> = sqlx::query_scalar(
            "SELECT record_int FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND id = $3",
        )
        .bind(tenant_int)
        .bind(dst_table)
        .bind(record)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)?;
        Ok(dst.map(|d| (dst_table, d)))
    }
}

/// 1 レコード × 1 リンク列の順方向行を書き直す（既存行を消して入れ直す）。
///
/// `targets` は `(ord, dst_table_int, dst_record)`。`single_valued` は宣言から写す（書込側は渡さない）。
/// 部分一意違反（単一値の列に 2 行）は 409、宣言の無い列（FK 違反）は内部エラー。
pub(crate) async fn replace_forward(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    src_record: i64,
    targets: &[(i16, i64, i64)],
) -> Result<(), DataError> {
    let fid = field_id_i16(field)?;
    sqlx::query(
        "DELETE FROM data_link \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 AND src_record = $4",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fid)
    .bind(src_record)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    if targets.is_empty() {
        return Ok(());
    }
    let ords: Vec<i16> = targets.iter().map(|t| t.0).collect();
    let dst_tables: Vec<i64> = targets.iter().map(|t| t.1).collect();
    let dst_records: Vec<i64> = targets.iter().map(|t| t.2).collect();
    let inserted = sqlx::query(
        "INSERT INTO data_link (tenant_int, table_int, field_id, src_record, ord, \
                                dst_table_int, dst_record, is_reverse, single_valued) \
         SELECT $1, $2, $3, $4, u.ord, u.dst_table, u.dst_record, false, lf.single_valued \
         FROM data_link_field lf \
         CROSS JOIN unnest($5::int2[], $6::int8[], $7::int8[]) AS u(ord, dst_table, dst_record) \
         WHERE lf.tenant_int = $1 AND lf.table_int = $2 AND lf.field_id = $3",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fid)
    .bind(src_record)
    .bind(&ords)
    .bind(&dst_tables)
    .bind(&dst_records)
    .execute(&mut *conn)
    .await
    .map_err(map_link_error)?
    .rows_affected();
    if inserted != targets.len() as u64 {
        // 宣言（data_link_field）が無いリンク列に書こうとした＝台帳と宣言の不整合。黙って落とさない。
        return Err(DataError::Internal(format!(
            "リンク列 {field} の宣言（data_link_field）がありません"
        )));
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn map_link_error(e: sqlx::Error) -> DataError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            DataError::Conflict("リンクのカーディナリティ（単一値）に違反しています".into())
        }
        _ => map_db(e),
    }
}
