//! 索引の parity 検証（data-platform §13 M2・§16.7「本体と索引の突合」）。
//!
//! 旧方式の式インデックスの内容は「`data_record` の各行に式（`data ->> 'f{id}'` 等）を適用したもの」
//! そのものなので、**本体から SQL で導いた期待集合**と新索引テーブルの行を突き合わせれば、
//! 旧索引と新索引の内容一致を検証したことになる。期待集合は書込側（Rust の [`super::normalize`]）とは
//! **独立に SQL で導く**（同じ関数で作って比べても欠落は見つからないため）。
//!
//! 対象は台帳が `active` の (フィールド, 索引テーブル)。不一致は件数だけを返し、内部キー（record_int）や
//! 値は返さない（運用ログにも値を出さない）。

use sqlx::PgConnection;
use uuid::Uuid;

use super::state::{field_id_i16, load_states};
use super::{IndexState, IndexTable, MAX_INDEX_KEY_BYTES};
use crate::field_key::json_key;
use crate::model::{
    FieldId, FieldType, TableKeys, TableSchema, SYSTEM_FIELD_CREATED_AT, SYSTEM_FIELD_OWNER,
    SYSTEM_FIELD_UPDATED_AT,
};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 1 フィールド × 1 索引テーブルの不一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexParityMismatch {
    /// フィールドの参照名（擬似列は `owner` / `created_at` / `updated_at`）。
    pub field: String,
    /// 索引テーブル名（`data_index_text` 等）。
    pub index_table: &'static str,
    /// 本体にあるのに索引に無い（または値が違う）行数。
    pub missing: i64,
    /// 索引にあるのに本体に無い（または値が違う）行数。
    pub extra: i64,
}

/// parity 検証の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexParityReport {
    /// 検証した (フィールド, 索引テーブル) の数。
    pub checked: u32,
    pub mismatches: Vec<IndexParityMismatch>,
}

impl IndexParityReport {
    /// 全て一致したか。
    pub fn is_consistent(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// 本体側の値（期待集合の元）。SQL に埋め込むのはこの固定集合の式だけ（PIT-21）。
#[derive(Clone, Copy)]
enum Source {
    /// JSONB の文字列（`$3` = `"f{id}"`）と文字列配列（multi_select・要素ごとに ord）。
    Json,
    Owner,
    CreatedAt,
    UpdatedAt,
}

impl Source {
    fn rows_sql(self) -> &'static str {
        match self {
            Source::Json => {
                "SELECT r.record_int, 0::int2 AS ord, (r.data ->> $3) COLLATE \"C\" AS full \
                 FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 \
                   AND jsonb_typeof(r.data -> $3) = 'string' \
                 UNION ALL \
                 SELECT r.record_int, (x.n - 1)::int2, x.v COLLATE \"C\" \
                 FROM data_record r CROSS JOIN LATERAL jsonb_array_elements_text( \
                     CASE WHEN jsonb_typeof(r.data -> $3) = 'array' THEN r.data -> $3 \
                          ELSE '[]'::jsonb END) WITH ORDINALITY AS x(v, n) \
                 WHERE r.tenant_int = $1 AND r.table_int = $2"
            }
            Source::Owner => {
                "SELECT r.record_int, 0::int2 AS ord, r.owner COLLATE \"C\" AS full \
                 FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 AND $3::text IS NOT NULL"
            }
            Source::CreatedAt => {
                "SELECT r.record_int, 0::int2 AS ord, \
                   to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                     COLLATE \"C\" AS full \
                 FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 AND $3::text IS NOT NULL"
            }
            Source::UpdatedAt => {
                "SELECT r.record_int, 0::int2 AS ord, \
                   to_char(r.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                     COLLATE \"C\" AS full \
                 FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 AND $3::text IS NOT NULL"
            }
        }
    }
}

impl DataStore {
    /// テーブルの新索引と本体（＝旧索引の内容）の一致を検証し、結果を進捗表へ記録する。
    pub async fn verify_index_parity(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<IndexParityReport, DataError> {
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .begin()
            .await
            .map_err(map_db)?;
        // 突合は本体と索引の全行どうしの反結合。統計が取れる前（大量の取込・バックフィルの直後）は
        // 件数を過小に見積もって CTE どうしの入れ子ループ（行数の 2 乗）を選び得るので、この Tx に限って
        // 入れ子ループを止めてハッシュ反結合に固定する（10 万行で数百秒 → 1 秒未満・Task 15.11 の計測）。
        sqlx::query("SET LOCAL enable_nestloop = off")
            .execute(&mut *conn)
            .await
            .map_err(map_db)?;
        let meta: Option<(sqlx::types::Json<TableSchema>, i32, i64)> = sqlx::query_as(
            "SELECT schema, tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db)?;
        let (schema, tenant_int, table_int) = meta.ok_or(DataError::NotFound)?;
        let keys = TableKeys {
            tenant_int,
            table_int,
        };
        let mut report = IndexParityReport::default();
        for (field, table, state) in load_states(&mut conn, keys).await? {
            if state != IndexState::Active {
                continue;
            }
            let Some((label, counts)) =
                check_one(&mut conn, keys, &schema.0, tenant_id, field, table).await?
            else {
                continue;
            };
            report.checked += 1;
            if counts != (0, 0) {
                report.mismatches.push(IndexParityMismatch {
                    field: label,
                    index_table: table.relation(),
                    missing: counts.0,
                    extra: counts.1,
                });
            }
        }
        let total: i64 = report.mismatches.iter().map(|m| m.missing + m.extra).sum();
        sqlx::query(
            "UPDATE data_index_backfill SET parity_checked_at = now(), parity_mismatches = $3, \
               updated_at = now() \
             WHERE tenant_int = $1 AND table_int = $2",
        )
        .bind(tenant_int)
        .bind(table_int)
        .bind(i32::try_from(total).unwrap_or(i32::MAX))
        .execute(&mut *conn)
        .await
        .map_err(map_db)?;
        conn.commit().await.map_err(map_db)?;
        Ok(report)
    }
}

/// 指定フィールドの active な索引だけを検証する（リンク値の移行の完了時・[`crate::link_values`]）。
pub(crate) async fn verify_fields(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
    tenant_id: &str,
    fields: &std::collections::BTreeSet<FieldId>,
) -> Result<IndexParityReport, DataError> {
    let mut report = IndexParityReport::default();
    for (field, table, state) in load_states(&mut *conn, keys).await? {
        if state != IndexState::Active || !fields.contains(&field) {
            continue;
        }
        let Some((label, counts)) =
            check_one(&mut *conn, keys, schema, tenant_id, field, table).await?
        else {
            continue;
        };
        report.checked += 1;
        if counts != (0, 0) {
            report.mismatches.push(IndexParityMismatch {
                field: label,
                index_table: table.relation(),
                missing: counts.0,
                extra: counts.1,
            });
        }
    }
    Ok(report)
}

/// 1 組を検証して `(ラベル, (missing, extra))` を返す。検証対象外（定義が消えた等）は `None`。
async fn check_one(
    conn: &mut PgConnection,
    keys: TableKeys,
    schema: &TableSchema,
    tenant_id: &str,
    field: FieldId,
    table: IndexTable,
) -> Result<Option<(String, (i64, i64))>, DataError> {
    let (label, source, def) = match field {
        SYSTEM_FIELD_OWNER => ("owner".to_string(), Source::Owner, None),
        SYSTEM_FIELD_CREATED_AT => ("created_at".to_string(), Source::CreatedAt, None),
        SYSTEM_FIELD_UPDATED_AT => ("updated_at".to_string(), Source::UpdatedAt, None),
        _ => match schema.field_by_id(field) {
            // 材料化した派生列の索引行は本体ではなく再計算が書く（本体に値を持たない・Task 15.4）。
            Some(d) if d.field_type.is_derived() => return Ok(None),
            Some(d) => (d.key.clone(), Source::Json, Some(d)),
            None => return Ok(None),
        },
    };
    let jk = json_key(field);
    let fid = field_id_i16(field)?;
    let counts = match table {
        IndexTable::Text => text_counts(conn, keys, &jk, fid, source).await?,
        IndexTable::Num => num_counts(conn, keys, &jk, fid).await?,
        IndexTable::Search => search_counts(conn, keys, &jk, fid).await?,
        IndexTable::Unique => {
            let numeric = def.is_some_and(|d| d.field_type == FieldType::Number);
            let uuid = def
                .is_some_and(|d| matches!(d.field_type, FieldType::RecordRef | FieldType::FileRef));
            unique_counts(conn, keys, &jk, fid, numeric, uuid).await?
        }
        IndexTable::Link => {
            let Some(ref_table) = def.and_then(|d| d.ref_table) else {
                return Ok(None);
            };
            let dst: Option<i64> = sqlx::query_scalar(
                "SELECT table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant_id)
            .bind(ref_table)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db)?;
            let Some(dst) = dst else { return Ok(None) };
            link_counts(conn, keys, &jk, fid, dst).await?
        }
    };
    Ok(Some((label, counts)))
}

async fn text_counts(
    conn: &mut PgConnection,
    keys: TableKeys,
    jk: &str,
    fid: i16,
    source: Source,
) -> Result<(i64, i64), DataError> {
    // 切り詰め行は「本体の値の、MAX_INDEX_KEY_BYTES 以内で最長の文字境界プレフィクス」と一致すること。
    let max = MAX_INDEX_KEY_BYTES;
    let matches = format!(
        "a.record_int = e.record_int AND a.ord = e.ord AND ( \
           (NOT a.truncated AND a.val = e.full) OR \
           (a.truncated AND octet_length(e.full) > {max} AND starts_with(e.full, a.val) \
            AND octet_length(a.val) <= {max} \
            AND octet_length(a.val) + octet_length(substr(e.full, char_length(a.val) + 1, 1)) > {max}))"
    );
    let sql = format!(
        "WITH e AS ({src}), \
         a AS (SELECT record_int, ord, val, truncated FROM data_index_text \
               WHERE tenant_int = $1 AND table_int = $2 AND field_id = $4) \
         SELECT (SELECT count(*) FROM e WHERE NOT EXISTS (SELECT 1 FROM a WHERE {matches})), \
                (SELECT count(*) FROM a WHERE NOT EXISTS (SELECT 1 FROM e WHERE {matches}))",
        src = source.rows_sql()
    );
    counts(conn, &sql, keys, jk, fid).await
}

async fn num_counts(
    conn: &mut PgConnection,
    keys: TableKeys,
    jk: &str,
    fid: i16,
) -> Result<(i64, i64), DataError> {
    let sql = "WITH e AS (SELECT r.record_int, (r.data ->> $3)::numeric AS v FROM data_record r \
                         WHERE r.tenant_int = $1 AND r.table_int = $2 \
                           AND jsonb_typeof(r.data -> $3) = 'number'), \
                    a AS (SELECT record_int, ord, val FROM data_index_num \
                         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $4) \
               SELECT (SELECT count(*) FROM e WHERE NOT EXISTS (SELECT 1 FROM a \
                         WHERE a.record_int = e.record_int AND a.ord = 0 AND a.val = e.v)), \
                      (SELECT count(*) FROM a WHERE NOT EXISTS (SELECT 1 FROM e \
                         WHERE a.record_int = e.record_int AND a.ord = 0 AND a.val = e.v))";
    counts(conn, sql, keys, jk, fid).await
}

async fn search_counts(
    conn: &mut PgConnection,
    keys: TableKeys,
    jk: &str,
    fid: i16,
) -> Result<(i64, i64), DataError> {
    let sql = "WITH e AS (SELECT r.record_int, r.data ->> $3 AS v FROM data_record r \
                         WHERE r.tenant_int = $1 AND r.table_int = $2 \
                           AND jsonb_typeof(r.data -> $3) = 'string'), \
                    a AS (SELECT record_int, ord, val FROM data_index_search \
                         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $4) \
               SELECT (SELECT count(*) FROM e WHERE NOT EXISTS (SELECT 1 FROM a \
                         WHERE a.record_int = e.record_int AND a.ord = 0 AND a.val = e.v)), \
                      (SELECT count(*) FROM a WHERE NOT EXISTS (SELECT 1 FROM e \
                         WHERE a.record_int = e.record_int AND a.ord = 0 AND a.val = e.v))";
    counts(conn, sql, keys, jk, fid).await
}

/// unique の期待集合は本体の単値（要素 1 件の配列＝リンクの保存形はその要素）から導く。
async fn unique_counts(
    conn: &mut PgConnection,
    keys: TableKeys,
    jk: &str,
    fid: i16,
    numeric: bool,
    uuid: bool,
) -> Result<(i64, i64), DataError> {
    let max = MAX_INDEX_KEY_BYTES;
    // 型ごとの一致条件（固定の 3 通りだけを埋め込む）。
    let match_cond = if numeric {
        "jsonb_typeof(e.raw) = 'number' AND a.val_norm::numeric = (e.raw #>> '{}')::numeric"
            .to_string()
    } else if uuid {
        "jsonb_typeof(e.raw) = 'string' AND a.val_norm = ((e.raw #>> '{}')::uuid)::text".to_string()
    } else {
        format!(
            "jsonb_typeof(e.raw) = 'string' AND ( \
               (octet_length(e.raw #>> '{{}}') <= {max} AND a.val_norm = (e.raw #>> '{{}}')) OR \
               (octet_length(e.raw #>> '{{}}') > {max} \
                AND right(a.val_norm, 65) = ':' || encode(sha256(convert_to(e.raw #>> '{{}}', 'UTF8')), 'hex') \
                AND starts_with(e.raw #>> '{{}}', left(a.val_norm, -65))))"
        )
    };
    let sql = format!(
        "WITH e AS (SELECT r.record_int, x.raw FROM data_record r \
                    CROSS JOIN LATERAL (SELECT CASE \
                        WHEN jsonb_typeof(r.data -> $3) = 'array' \
                         AND jsonb_array_length(r.data -> $3) = 1 THEN r.data -> $3 -> 0 \
                        ELSE r.data -> $3 END AS raw) x \
                    WHERE r.tenant_int = $1 AND r.table_int = $2 \
                      AND jsonb_typeof(x.raw) IN ('string', 'number')), \
              a AS (SELECT record_int, val_norm FROM data_unique \
                    WHERE tenant_int = $1 AND table_int = $2 AND field_id = $4) \
         SELECT (SELECT count(*) FROM e WHERE NOT EXISTS (SELECT 1 FROM a \
                   WHERE a.record_int = e.record_int AND {match_cond})), \
                (SELECT count(*) FROM a WHERE NOT EXISTS (SELECT 1 FROM e \
                   WHERE a.record_int = e.record_int AND {match_cond}))"
    );
    counts(conn, &sql, keys, jk, fid).await
}

async fn link_counts(
    conn: &mut PgConnection,
    keys: TableKeys,
    jk: &str,
    fid: i16,
    dst_table: i64,
) -> Result<(i64, i64), DataError> {
    // 参照先が存在するエッジだけを比べる（宙に浮いた参照は索引に載せない・参照先の削除後に残る
    // 順方向行は本体側の参照も宙に浮いているので数えない）。
    // 保存形は配列（ord = 配列内の位置）。移行前の v1 の単一 UUID 文字列は ord 0（同じ索引行）。
    let sql = "WITH e AS (SELECT r.record_int AS src, 0::int2 AS ord, \
                            (SELECT d.record_int FROM data_record d \
                              WHERE d.tenant_int = $1 AND d.table_int = $5 \
                                AND d.id = (r.data ->> $3)::uuid) AS dst \
                         FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 \
                           AND jsonb_typeof(r.data -> $3) = 'string' \
                         UNION ALL \
                         SELECT r.record_int, (x.n - 1)::int2, \
                            (SELECT d.record_int FROM data_record d \
                              WHERE d.tenant_int = $1 AND d.table_int = $5 AND d.id = x.v::uuid) \
                         FROM data_record r CROSS JOIN LATERAL jsonb_array_elements_text( \
                             CASE WHEN jsonb_typeof(r.data -> $3) = 'array' THEN r.data -> $3 \
                                  ELSE '[]'::jsonb END) WITH ORDINALITY AS x(v, n) \
                         WHERE r.tenant_int = $1 AND r.table_int = $2), \
                    a AS (SELECT l.src_record, l.ord, l.dst_table_int, l.dst_record FROM data_link l \
                         WHERE l.tenant_int = $1 AND l.table_int = $2 AND l.field_id = $4 \
                           AND NOT l.is_reverse \
                           AND EXISTS (SELECT 1 FROM data_record d WHERE d.tenant_int = $1 \
                                AND d.table_int = l.dst_table_int AND d.record_int = l.dst_record)) \
               SELECT (SELECT count(*) FROM e WHERE e.dst IS NOT NULL AND NOT EXISTS ( \
                         SELECT 1 FROM a WHERE a.src_record = e.src AND a.ord = e.ord \
                           AND a.dst_table_int = $5 AND a.dst_record = e.dst)), \
                      (SELECT count(*) FROM a WHERE NOT EXISTS (SELECT 1 FROM e \
                         WHERE a.src_record = e.src AND a.ord = e.ord \
                           AND a.dst_table_int = $5 AND a.dst_record = e.dst))";
    let (m, x): (i64, i64) = sqlx::query_as(sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(jk)
        .bind(fid)
        .bind(dst_table)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db)?;
    Ok((m, x))
}

async fn counts(
    conn: &mut PgConnection,
    sql: &str,
    keys: TableKeys,
    jk: &str,
    fid: i16,
) -> Result<(i64, i64), DataError> {
    sqlx::query_as(sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(jk)
        .bind(fid)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db)
}
