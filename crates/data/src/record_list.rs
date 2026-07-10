//! レコード一覧（Task 9.2・宣言フィールドの等値フィルタ＋ソート）。
//!
//! フィルタ/ソート対象は `indexed` / `unique` 宣言されたフィールドに限る（未索引フィールド
//! への全走査クエリを API から作れないようにする）。フィールド名はスキーマ検証済みの
//! 宣言名のみ SQL へ埋め込み、値は常にバインドする。宣言的クエリ API（filter/sort/page/
//! aggregate の合成・行述語との無条件 AND 合成）は Task 9.3/9.4 でここに載る。

use authz::{AuthContext, Relation};
use serde_json::Value;
use sqlx::postgres::PgArguments;
use sqlx::query::QueryAs;
use sqlx::Postgres;
use uuid::Uuid;

use crate::model::{DataRecord, FieldType, TableSchema};
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 一覧のフィルタ（PR1 は宣言フィールドへの等値/包含のみ。宣言的クエリ API は Task 9.4）。
#[derive(Debug, Clone)]
pub struct RecordFilter {
    pub field: String,
    pub value: Value,
}

/// 一覧のソート（宣言フィールドのみ）。
#[derive(Debug, Clone)]
pub struct RecordSort {
    pub field: String,
    pub descending: bool,
}

/// 一覧のオプション。
#[derive(Debug, Clone, Default)]
pub struct ListRecordsOptions {
    pub filter: Option<RecordFilter>,
    pub sort: Option<RecordSort>,
    pub limit: i64,
    pub offset: i64,
}

/// 一覧結果。
#[derive(Debug, Clone)]
pub struct ListRecordsPage {
    pub items: Vec<DataRecord>,
}

impl DataStore {
    /// レコード一覧（viewer・宣言フィールドの等値フィルタ＋ソート・式インデックス前提）。
    ///
    /// フィルタ/ソート対象は `indexed` または `unique` 宣言されたフィールドに限る
    /// （未索引フィールドへの全走査クエリを API から作れないようにする）。
    pub async fn list_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        opts: &ListRecordsOptions,
        trace_id: Option<&str>,
    ) -> Result<ListRecordsPage, DataError> {
        let (filter, sort) = (opts.filter.as_ref(), opts.sort.as_ref());
        self.require(
            ctx,
            table_id,
            Relation::Viewer,
            "data.record.list",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let limit = opts.limit.clamp(1, 200);
        let offset = opts.offset.clamp(0, 10_000);

        // フィルタ/ソート句を組み立てる。フィールド名はスキーマ検証済みの宣言名のみを
        // 埋め込む（値は常にバインド）。式インデックスと同形の式にして索引を効かせる。
        use std::fmt::Write as _;
        let mut sql = String::from(
            "SELECT id, table_id, data, rev, owner, created_at, updated_at \
             FROM data_record WHERE tenant_id = $1 AND table_id = $2",
        );
        // バインドは $3 以降（filter 値のみ可変）。
        let mut filter_bind: Option<&Value> = None;
        if let Some(fl) = filter {
            let f = indexed_field(&table.schema, &fl.field)?;
            match f.field_type {
                FieldType::Number => {
                    if !fl.value.is_number() {
                        return Err(DataError::Invalid(format!(
                            "フィルタ '{}' は数値で指定してください",
                            fl.field
                        )));
                    }
                    let _ = write!(sql, " AND ((data ->> '{}'))::numeric = $3::numeric", f.name);
                }
                FieldType::MultiSelect => {
                    if !fl.value.is_string() {
                        return Err(DataError::Invalid(format!(
                            "フィルタ '{}' は文字列で指定してください",
                            fl.field
                        )));
                    }
                    // GIN の存在演算子（配列が値を含むか）。
                    let _ = write!(sql, " AND (data -> '{}') ? $3", f.name);
                }
                _ => {
                    if !fl.value.is_string() {
                        return Err(DataError::Invalid(format!(
                            "フィルタ '{}' は文字列で指定してください",
                            fl.field
                        )));
                    }
                    let _ = write!(sql, " AND (data ->> '{}') = $3", f.name);
                }
            }
            filter_bind = Some(&fl.value);
        }
        if let Some(st) = sort {
            let f = indexed_field(&table.schema, &st.field)?;
            if f.field_type == FieldType::MultiSelect {
                return Err(DataError::Invalid(
                    "multi_select はソートに使えません".into(),
                ));
            }
            let dir = if st.descending { "DESC" } else { "ASC" };
            let expr = if f.field_type == FieldType::Number {
                format!("((data ->> '{}'))::numeric", f.name)
            } else {
                format!("(data ->> '{}')", f.name)
            };
            // NULLS LAST: 値なし行を末尾へ（昇降順で一貫）。id で決定的に並べる。
            let _ = write!(sql, " ORDER BY {expr} {dir} NULLS LAST, id {dir}");
        } else {
            sql.push_str(" ORDER BY updated_at DESC, id DESC");
        }
        let (limit_ph, offset_ph) = if filter_bind.is_some() {
            ("$4", "$5")
        } else {
            ("$3", "$4")
        };
        let _ = write!(sql, " LIMIT {limit_ph} OFFSET {offset_ph}");

        let mut query: QueryAs<'_, Postgres, RecordRow, PgArguments> = sqlx::query_as(&sql);
        query = query.bind(&ctx.tenant_id).bind(table_id);
        if let Some(v) = filter_bind {
            query = match v {
                Value::Number(n) => query.bind(n.as_f64().unwrap_or(f64::NAN)),
                Value::String(s) => query.bind(s.clone()),
                _ => {
                    return Err(DataError::Invalid(
                        "フィルタ値は文字列または数値で指定してください".into(),
                    ))
                }
            };
        }
        let rows: Vec<RecordRow> = query
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        Ok(ListRecordsPage {
            items: rows.into_iter().map(RecordRow::into_record).collect(),
        })
    }
}

/// フィルタ/ソート対象フィールドを解決する（宣言済み・索引付きのみ許可）。
fn indexed_field<'s>(
    schema: &'s TableSchema,
    name: &str,
) -> Result<&'s crate::model::FieldDef, DataError> {
    let f = schema.field(name).ok_or_else(|| {
        DataError::Invalid(format!("フィールド '{name}' はスキーマに存在しません"))
    })?;
    if !(f.indexed || f.unique) {
        return Err(DataError::Invalid(format!(
            "フィールド '{name}' はフィルタ/ソート対象として宣言されていません（indexed）"
        )));
    }
    Ok(f)
}
