//! レコード一覧（Task 9.2・宣言フィールドの等値フィルタ＋ソート）。
//!
//! フィルタ/ソート対象は `indexed` / `unique` 宣言されたフィールドに限る（未索引フィールド
//! への全走査クエリを API から作れないようにする）。フィールド名はスキーマ検証済みの
//! 宣言名のみ SQL へ埋め込み、値は常にバインドする。宣言的クエリ API（filter/sort/page/
//! aggregate の合成・行述語との無条件 AND 合成）は Task 9.3/9.4 でここに載る。

use authz::{AuthContext, Relation};
use serde_json::Value;
use uuid::Uuid;

use crate::model::{DataRecord, FieldType, TableSchema};
use crate::query::executor::{ListFilter, ListSort};
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::DataError;

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
    /// 個別共有集合が上限（PIT-18）で切り詰められ、共有経由の一部行が
    /// 表示されていない可能性がある（fail-closed・可視減方向）。
    pub shares_truncated: bool,
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

        // フィルタ/ソートを検証し、閉じた形（executor の型）へ写す。SQL の組み立ては
        // クエリ実行チョークポイント（query::executor）だけが行い、行述語が無条件に
        // AND 合成される（Task 9.3・PIT-21）。
        let exec_filter = match filter {
            None => None,
            Some(fl) => {
                let f = indexed_field(&table.schema, &fl.field)?;
                Some(match f.field_type {
                    FieldType::Number => {
                        let Some(n) = fl.value.as_f64() else {
                            return Err(DataError::Invalid(format!(
                                "フィルタ '{}' は数値で指定してください",
                                fl.field
                            )));
                        };
                        ListFilter::NumberEq {
                            field: f.name.clone(),
                            value: n,
                        }
                    }
                    FieldType::MultiSelect => {
                        let Some(v) = fl.value.as_str() else {
                            return Err(DataError::Invalid(format!(
                                "フィルタ '{}' は文字列で指定してください",
                                fl.field
                            )));
                        };
                        ListFilter::MultiContains {
                            field: f.name.clone(),
                            value: v.to_string(),
                        }
                    }
                    _ => {
                        let Some(v) = fl.value.as_str() else {
                            return Err(DataError::Invalid(format!(
                                "フィルタ '{}' は文字列で指定してください",
                                fl.field
                            )));
                        };
                        ListFilter::TextEq {
                            field: f.name.clone(),
                            value: v.to_string(),
                        }
                    }
                })
            }
        };
        let exec_sort = match sort {
            None => None,
            Some(st) => {
                let f = indexed_field(&table.schema, &st.field)?;
                if f.field_type == FieldType::MultiSelect {
                    return Err(DataError::Invalid(
                        "multi_select はソートに使えません".into(),
                    ));
                }
                Some(ListSort {
                    field: f.name.clone(),
                    numeric: f.field_type == FieldType::Number,
                    descending: st.descending,
                })
            }
        };
        let (rows, shares_truncated) = self
            .select_visible_rows(
                ctx,
                &table,
                exec_filter.as_ref(),
                exec_sort.as_ref(),
                limit,
                offset,
            )
            .await?;
        let mut items: Vec<DataRecord> = rows.into_iter().map(RecordRow::into_record).collect();
        self.resolve_derived_fields(ctx, &table, &mut items).await?;
        let masked = self.masked_fields(ctx, &table).await?;
        Self::apply_mask_records(&masked, &mut items);
        Ok(ListRecordsPage {
            items,
            shares_truncated,
        })
    }
}

impl DataStore {
    /// 可視行の件数（Task 9.3: 集計にも行述語を適用する・不可視行は件数に混入しない）。
    pub async fn count_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        filter: Option<&RecordFilter>,
        trace_id: Option<&str>,
    ) -> Result<i64, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Viewer,
            "data.record.count",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let exec_filter = match filter {
            None => None,
            Some(fl) => {
                let f = indexed_field(&table.schema, &fl.field)?;
                Some(match f.field_type {
                    FieldType::Number => {
                        let Some(n) = fl.value.as_f64() else {
                            return Err(DataError::Invalid(format!(
                                "フィルタ '{}' は数値で指定してください",
                                fl.field
                            )));
                        };
                        ListFilter::NumberEq {
                            field: f.name.clone(),
                            value: n,
                        }
                    }
                    FieldType::MultiSelect => {
                        let Some(v) = fl.value.as_str() else {
                            return Err(DataError::Invalid(format!(
                                "フィルタ '{}' は文字列で指定してください",
                                fl.field
                            )));
                        };
                        ListFilter::MultiContains {
                            field: f.name.clone(),
                            value: v.to_string(),
                        }
                    }
                    _ => {
                        let Some(v) = fl.value.as_str() else {
                            return Err(DataError::Invalid(format!(
                                "フィルタ '{}' は文字列で指定してください",
                                fl.field
                            )));
                        };
                        ListFilter::TextEq {
                            field: f.name.clone(),
                            value: v.to_string(),
                        }
                    }
                })
            }
        };
        self.count_visible_rows(ctx, &table, exec_filter.as_ref())
            .await
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
