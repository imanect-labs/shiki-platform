//! 索引上の走査の基本形（Task 15.3 のクエリコンパイラが組み込む入口・data-platform §3.3・§3.5）。
//!
//! ここは**索引だけを見る**下回りで、行述語（Guard）・マスク・テーブル ReBAC は掛けない。
//! 呼び出し側（クエリコンパイラ）が必ず認可と合成すること。結果の `record_int` は外へ出さない（PIT-75）。
//!
//! # 切り詰め値でのソートの契約（§3.3）
//!
//! - ソートは**プレフィクス順**で走査する。同一プレフィクス群の中の順序は完全値順を保証しない
//! - keyset は `(val_prefix, record_int)` のタプル比較で進むので、**静的な集合に対して**ページ間の
//!   重複・欠落は起きない（`record_int` が常にタイブレーカに入る）
//! - カーソルに載せるのは**索引に格納されているプレフィクス値**（完全値を載せると走査位置とずれて欠落する）
//! - 切り詰めが起こり得る列（text 型）をソートキーにしたら、応答に `sort_precision: "prefix"` を含める

use serde::Serialize;
use sqlx::PgConnection;
use utoipa::ToSchema;

use super::normalize::prefix_at_char_boundary;
use super::state::field_id_i16;
use super::MAX_INDEX_KEY_BYTES;
use crate::model::{FieldDef, FieldId, FieldType, TableKeys};
use crate::{map_db, DataError};

/// ソート結果の精度（応答に含めて呼び出し側へ明示する・data-platform §3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortPrecision {
    /// 完全値順。
    Exact,
    /// 索引のプレフィクス（先頭 `MAX_INDEX_KEY_BYTES` バイト）順。同一プレフィクス内は完全値順ではない。
    Prefix,
}

/// ソートキーにしたときの精度。切り詰めが起こり得る列（`MAX_INDEX_KEY_BYTES` を超える値を
/// 書ける text 型）は `Prefix`。選択肢・ID・日付・擬似列（`None`）は上限より短いので `Exact`。
pub fn sort_precision(field: Option<&FieldDef>) -> SortPrecision {
    match field {
        Some(f) if f.field_type == FieldType::Text => SortPrecision::Prefix,
        _ => SortPrecision::Exact,
    }
}

/// text 索引のソート走査の 1 ページ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSortedPage {
    /// 索引の順（`(val_prefix, record_int)`）に並んだレコード。
    pub record_ints: Vec<i64>,
    /// 次ページの keyset（索引に入っているプレフィクス値と record_int）。終端なら `None`。
    pub next: Option<(String, i64)>,
}

/// `data_index_text` を `(val, record_int)` の keyset で歩く（昇順／降順）。
///
/// `(tenant_int, table_int, field_id, val, record_int)` の値索引をそのまま範囲走査する形。
#[allow(dead_code)] // Task 15.3 のクエリコンパイラ（ソート駆動）が使う。走査契約は IT で固定済み。
pub(crate) async fn text_sorted_page(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    after: Option<(&str, i64)>,
    descending: bool,
    limit: i64,
) -> Result<TextSortedPage, DataError> {
    let (cmp, dir) = if descending {
        ("<", "DESC")
    } else {
        (">", "ASC")
    };
    let keyset = if after.is_some() {
        format!(" AND (val, record_int) {cmp} ($4::text COLLATE \"C\", $5)")
    } else {
        " AND $4::text IS NULL AND $5::int8 IS NULL".to_string()
    };
    let sql = format!(
        "SELECT val, record_int FROM data_index_text \
         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3{keyset} \
         ORDER BY val {dir}, record_int {dir} LIMIT $6"
    );
    let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(field_id_i16(field)?)
        .bind(after.map(|a| a.0))
        .bind(after.map(|a| a.1))
        .bind(limit.max(1))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
    let next = if rows.len() as i64 == limit.max(1) {
        rows.last().cloned()
    } else {
        None
    };
    Ok(TextSortedPage {
        record_ints: rows.into_iter().map(|(_, r)| r).collect(),
        next,
    })
}

/// text 列の等値の候補（`record_int` 昇順）。プレフィクスで索引を引き、`truncated` の候補だけを
/// **本体で再確認**する（data-platform §3.3）。`json_key` は `"f{id}"`（バインドで渡す）。
#[allow(dead_code)] // Task 15.3 のクエリコンパイラ（フィルタ駆動・検査）が使う。挙動は IT で固定済み。
pub(crate) async fn text_eq_candidates(
    conn: &mut PgConnection,
    keys: TableKeys,
    field: FieldId,
    json_key: &str,
    value: &str,
) -> Result<Vec<i64>, DataError> {
    let (prefix, truncated) = prefix_at_char_boundary(value, MAX_INDEX_KEY_BYTES);
    sqlx::query_scalar(
        "SELECT i.record_int FROM data_index_text i \
         WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = $3 AND i.val = $4 \
           AND i.truncated = $5 \
           AND (NOT i.truncated OR EXISTS ( \
                 SELECT 1 FROM data_record r \
                 WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = i.record_int \
                   AND (r.data ->> $6) = $7)) \
         ORDER BY i.record_int",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(field_id_i16(field)?)
    .bind(prefix)
    .bind(truncated)
    .bind(json_key)
    .bind(value)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
