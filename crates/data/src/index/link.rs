//! リンク（`data_link`）の宣言と書込（data-platform §3.3・§7.1）。
//!
//! - 1 エッジにつき**順方向行と逆方向行の 2 行**を持つ表形式（両方向とも自分側の `table_int` に載るので
//!   1 パーティションへ枝刈りされる）。逆方向行は対称列（`LinkDef.symmetric_field`・Task 15.4）が
//!   宣言されたリンクだけが持ち、**順方向行と同じ Tx で**書く（[`replace_link`]）。対称列の値は本体に
//!   持たないので、両方向が食い違う状態は構造的に生じない。v1 の `record_ref` は片方向（順方向行だけ）。
//! - 逆方向行の `ord` は相手（参照先レコード）ごとの採番。同じ参照先へ並行してリンクを張る書込は、
//!   参照先ごとの advisory lock（参照先 id の昇順に取る＝デッドロックしない）で直列化する。
//! - **カーディナリティは DB で強制する**。行の `single_valued` は書込側が値を渡さず、
//!   `data_link_field`（スキーマ改訂でだけ書き換わる宣言）から `INSERT … SELECT` で写す。
//!   さらに `data_link_field` への複合 FK があるので、生の SQL で `false` を渡しても宣言が `true` なら
//!   FK 違反で落ちる（部分一意 `WHERE single_valued` を迂回できない・PIT-72）。
//! - v1 の `record_ref` は単一 UUID なので src 側は単一値（`single_valued = true`）。リンク定義
//!   （`LinkDef`）つきの列はカーディナリティから決まる（[`crate::derived::types::LinkCardinality`]）。

use std::collections::{BTreeSet, HashMap};

use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use super::state::field_id_i16;
use super::MAX_MULTI_VALUES_CEILING;
use crate::model::{FieldDef, FieldId, FieldType, TableKeys, TableSchema};
use crate::{map_db, DataError};

/// リンク列の行の `single_valued`（この列の src 側が単一値か）。v1 の `record_ref` は単一値。
pub(crate) fn single_valued(f: &FieldDef) -> bool {
    f.link.as_ref().is_none_or(|l| l.cardinality.src_single())
}

/// スキーマのリンク列（対称列を含む）を `data_link_field` へ宣言する（作成・改訂・初期化の Tx 上）。
///
/// 宣言はカーディナリティから決まり、**レコード書込では触らない**。カーディナリティはスキーマ改訂で
/// 変えられない（[`crate::schema::validate_schema_update`]）ので、既存の宣言は書き換えない。
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
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_int, table_int, field_id) DO NOTHING",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(field_id_i16(f.id)?)
        .bind(single_valued(f))
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
        let Some(dst_table) = self.table_int(conn, tenant_id, ref_table).await? else {
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

    /// 参照先テーブルの `table_int`（削除済みテーブルも引く。論理削除でレコードは残るため、
    /// バックフィルで辿れるように）。
    pub(crate) async fn table_int(
        &mut self,
        conn: &mut PgConnection,
        tenant_id: &str,
        ref_table: Uuid,
    ) -> Result<Option<i64>, DataError> {
        if let Some(t) = self.tables.get(&ref_table) {
            return Ok(*t);
        }
        let t: Option<i64> =
            sqlx::query_scalar("SELECT table_int FROM data_table WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id)
                .bind(ref_table)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db)?;
        self.tables.insert(ref_table, t);
        Ok(t)
    }
}

/// 1 レコード × 1 リンク列の順方向行を書き直す（既存行を消して入れ直す）。
///
/// `targets` は `(ord, dst_table_int, dst_record)`。`single_valued` は宣言から写す（書込側は渡さない）。
/// 部分一意違反（単一値の列に 2 行）は 422、宣言の無い列（FK 違反）は内部エラー。
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
    insert_forward(conn, keys, field, src_record, targets).await
}

/// 順方向行を入れる（既存行は消してある前提・`single_valued` は宣言から写す）。
async fn insert_forward(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    src_record: i64,
    targets: &[(i16, i64, i64)],
) -> Result<(), DataError> {
    if targets.is_empty() {
        return Ok(());
    }
    let fid = field_id_i16(field)?;
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

/// 対称列を持つリンクの 1 レコード分を書き直す（順方向行＋逆方向行を同じ Tx で）。
///
/// 順方向行は消して入れ直す（`ord` = 配列内の位置）。逆方向行は**差分だけ**を触る（外れた相手の行を消し、
/// 増えた相手へ行を足す。残る相手の逆方向の並び順は動かさない）。`reverse` は (対称列の field_id,
/// 参照先の `table_int`)。
pub(crate) async fn replace_link(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    src_record: i64,
    targets: &[(i16, i64, i64)],
    reverse: (FieldId, i64),
) -> Result<(), DataError> {
    let fid = field_id_i16(field)?;
    let old: Vec<i64> = sqlx::query_scalar(
        "DELETE FROM data_link \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 AND src_record = $4 \
         RETURNING dst_record",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fid)
    .bind(src_record)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    insert_forward(conn, keys, field, src_record, targets).await?;

    let (sym, ref_table_int) = reverse;
    let rkeys = TableKeys {
        tenant_int: keys.tenant_int,
        table_int: ref_table_int,
    };
    let sym_i16 = field_id_i16(sym)?;
    let old: BTreeSet<i64> = old.into_iter().collect();
    let new: BTreeSet<i64> = targets
        .iter()
        .filter(|t| t.1 == ref_table_int)
        .map(|t| t.2)
        .collect();
    let removed: Vec<i64> = old.difference(&new).copied().collect();
    if !removed.is_empty() {
        sqlx::query(
            "DELETE FROM data_link \
             WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
               AND src_record = ANY($4::int8[]) AND dst_record = $5",
        )
        .bind(rkeys.tenant_int)
        .bind(rkeys.table_int)
        .bind(sym_i16)
        .bind(&removed)
        .bind(src_record)
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    }
    // 参照先 id の昇順（BTreeSet）にロックを取るので、並行する書込どうしがデッドロックしない。
    for dst in new.difference(&old) {
        lock_reverse_slot(conn, rkeys, sym, *dst).await?;
        let ord = next_reverse_ord(conn, rkeys, sym_i16, *dst).await?;
        let n = sqlx::query(
            "INSERT INTO data_link (tenant_int, table_int, field_id, src_record, ord, \
                                    dst_table_int, dst_record, is_reverse, single_valued) \
             SELECT $1, $2, $3, $4, $5, $6, $7, true, lf.single_valued \
             FROM data_link_field lf \
             WHERE lf.tenant_int = $1 AND lf.table_int = $2 AND lf.field_id = $3",
        )
        .bind(rkeys.tenant_int)
        .bind(rkeys.table_int)
        .bind(sym_i16)
        .bind(*dst)
        .bind(ord)
        .bind(keys.table_int)
        .bind(src_record)
        .execute(&mut *conn)
        .await
        .map_err(map_link_error)?
        .rows_affected();
        if n != 1 {
            return Err(DataError::Internal(format!(
                "対称列 {sym} の宣言（data_link_field）がありません"
            )));
        }
    }
    Ok(())
}

/// 参照先レコードごとの逆方向 `ord` 採番を直列化する（Tx 終了で解放される advisory lock）。
async fn lock_reverse_slot(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    record: i64,
) -> Result<(), DataError> {
    let digest = Sha256::digest(
        format!(
            "data_link.reverse:{}:{}:{field}:{record}",
            keys.tenant_int, keys.table_int
        )
        .as_bytes(),
    );
    let mut b = [0u8; 8];
    b.copy_from_slice(&digest[..8]);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(i64::from_be_bytes(b))
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok(())
}

/// 逆方向行の次の `ord`（末尾の次。`int2` の上限に達したら空き番号を探す・無ければ 422）。
async fn next_reverse_ord(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: i16,
    record: i64,
) -> Result<i16, DataError> {
    let next: i32 = sqlx::query_scalar(
        "SELECT coalesce(max(ord)::int4 + 1, 0) FROM data_link \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 AND src_record = $4",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(field)
    .bind(record)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db)?;
    if let Ok(ord) = i16::try_from(next) {
        return Ok(ord);
    }
    let gap: Option<i32> = sqlx::query_scalar(
        "SELECT g FROM generate_series(0, $5::int4) AS g \
         WHERE NOT EXISTS (SELECT 1 FROM data_link WHERE tenant_int = $1 AND table_int = $2 \
                             AND field_id = $3 AND src_record = $4 AND ord = g) \
         LIMIT 1",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(field)
    .bind(record)
    .bind(i32::try_from(MAX_MULTI_VALUES_CEILING).unwrap_or(i32::from(i16::MAX)) - 1)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    gap.and_then(|g| i16::try_from(g).ok()).ok_or_else(|| {
        DataError::Invalid("参照先のレコードへのリンクが多すぎます（逆方向の上限）".into())
    })
}

/// 対称列の逆方向行を、参照先レコード側から消す（参照先レコードの削除時）。
pub(crate) async fn delete_reverse_rows(
    conn: &mut PgConnection,
    keys: TableKeys,
    fields: &[i16],
    record: i64,
) -> Result<(), DataError> {
    if fields.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "DELETE FROM data_link \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = ANY($3::int2[]) AND src_record = $4",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(fields)
    .bind(record)
    .execute(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(())
}

/// カーディナリティ（部分一意）違反は 422（宣言した一意性への入力違反）。
#[allow(clippy::needless_pass_by_value)]
fn map_link_error(e: sqlx::Error) -> DataError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => DataError::Invalid(
            "リンクのカーディナリティ（一対一・一対多の一意性）に違反しています".into(),
        ),
        _ => map_db(e),
    }
}
