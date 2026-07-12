//! 宣言的クエリ（filter/sort/page/aggregate・Task 9.4）。
//!
//! 生 SQL は非公開。閉じた演算子・宣言フィールドのみを受け、[`executor`](super::executor) の
//! 行述語つき実行へ必ず合成する。フィールドマスク（[`crate::mask`]）と組み合わせ、
//! マスク対象フィールドは filter/sort/group_by/metrics に**使えない**（PIT-19）。
//! 集計は K 未満セルを抑制する（PIT-17）。

use authz::{AuthContext, Relation};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::model::{DataRecord, FieldType};
use crate::query::executor::{ListFilter, ListSort};
use crate::store::DataStore;
use crate::{DataError, DEFAULT_AGGREGATE_MIN_ROWS};

/// フィルタ条件（宣言フィールド・値はバインド）。
///
/// 演算子はフィールド型で決まる（multi_select は「含む」、その他は等値）。呼び出し側で
/// 演算子を選ばせない（型と不整合な組合せを構造的に排除する）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueryFilter {
    pub field: String,
    pub value: serde_json::Value,
}

/// ソート条件（宣言フィールド）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QuerySort {
    pub field: String,
    #[serde(default)]
    pub descending: bool,
}

/// 集計メトリクス。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// 集計指定（group_by ＋ メトリクス）。
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Aggregate {
    /// グループ化フィールド（0 個＝全体集計）。
    #[serde(default)]
    pub group_by: Vec<String>,
    pub metric: Metric,
    /// count 以外で対象にする number フィールド。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// 宣言的クエリ本体。
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct DataQuery {
    #[serde(default)]
    pub filter: Option<QueryFilter>,
    #[serde(default)]
    pub sort: Option<QuerySort>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub aggregate: Option<Aggregate>,
}

/// 集計結果の 1 グループ。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AggregateGroup {
    /// group_by フィールド → 値（全体集計は空）。
    pub key: serde_json::Map<String, serde_json::Value>,
    /// メトリクス値（count は整数、K 未満で抑制された場合は null）。
    pub value: serde_json::Value,
}

/// クエリ実行結果（rows または aggregate のどちらか）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueryResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<DataRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<AggregateGroup>>,
    /// 個別共有集合の切り詰め（PIT-18）。
    pub shares_truncated: bool,
    /// K 未満で抑制されたグループ/全体集計があった（PIT-17）。
    pub suppressed: bool,
}

impl DataStore {
    /// 宣言的クエリを実行する（viewer・行述語＋フィールドマスク合成・Task 9.4）。
    pub async fn run_query(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        query: &DataQuery,
        trace_id: Option<&str>,
    ) -> Result<QueryResult, DataError> {
        self.require(ctx, table_id, Relation::Viewer, "data.query", trace_id)
            .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let masked = self.masked_fields(ctx, &table).await?;

        // 集計クエリは監査に残す（PIT-17: 反復差分の検知可能性）。
        if query.aggregate.is_some() {
            self.record_audit_best_effort(
                ctx,
                "data.query.aggregate",
                &table_id.to_string(),
                trace_id,
                serde_json::json!({ "group_by": query.aggregate.as_ref().map(|a| &a.group_by) }),
            )
            .await;
        }

        // フィルタ/ソートを検証（宣言＋索引＋マスク非対象）→ executor の閉じた型へ。
        let exec_filter = match &query.filter {
            None => None,
            Some(f) => Some(Self::compile_query_filter(&table, &masked, f)?),
        };
        let exec_sort = match &query.sort {
            None => None,
            Some(s) => {
                let field = Self::indexed_queryable(&table, &masked, &s.field)?;
                Some(ListSort {
                    field: field.name.clone(),
                    numeric: field.field_type == FieldType::Number,
                    descending: s.descending,
                })
            }
        };

        if let Some(agg) = &query.aggregate {
            return self
                .aggregate_result(ctx, &table, &masked, agg, exec_filter.as_ref())
                .await;
        }

        let limit = query.limit.unwrap_or(50).clamp(1, 200);
        let offset = query.offset.unwrap_or(0).clamp(0, 10_000);
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
        let mut items: Vec<DataRecord> = rows
            .into_iter()
            .map(super::executor::row_into_record)
            .collect();
        self.resolve_derived_fields(ctx, &table, &mut items).await?;
        Self::apply_mask_records(&masked, &mut items);
        Ok(QueryResult {
            items: Some(items),
            groups: None,
            shares_truncated,
            suppressed: false,
        })
    }

    /// 集計ブランチを実行して QueryResult へ包む（run_query の分割）。
    async fn aggregate_result(
        &self,
        ctx: &AuthContext,
        table: &crate::model::DataTable,
        masked: &std::collections::HashSet<String>,
        agg: &Aggregate,
        filter: Option<&ListFilter>,
    ) -> Result<QueryResult, DataError> {
        let k = table
            .schema
            .aggregate_min_rows
            .unwrap_or(DEFAULT_AGGREGATE_MIN_ROWS);
        let (groups, suppressed, shares_truncated) = self
            .run_aggregate(ctx, table, masked, agg, filter, k)
            .await?;
        Ok(QueryResult {
            items: None,
            groups: Some(groups),
            shares_truncated,
            suppressed,
        })
    }

    /// クエリフィルタを検証して executor の閉じた型へ写す（マスク列は 403）。
    fn compile_query_filter(
        table: &crate::model::DataTable,
        masked: &std::collections::HashSet<String>,
        f: &QueryFilter,
    ) -> Result<ListFilter, DataError> {
        let field = Self::indexed_queryable(table, masked, &f.field)?;
        match field.field_type {
            FieldType::Number => {
                let n = f.value.as_f64().ok_or_else(|| {
                    DataError::Invalid(format!("フィルタ '{}' は数値で指定してください", f.field))
                })?;
                Ok(ListFilter::NumberEq {
                    field: field.name.clone(),
                    value: n,
                })
            }
            FieldType::MultiSelect => {
                let v = f.value.as_str().ok_or_else(|| {
                    DataError::Invalid(format!("フィルタ '{}' は文字列で指定してください", f.field))
                })?;
                Ok(ListFilter::MultiContains {
                    field: field.name.clone(),
                    value: v.to_string(),
                })
            }
            _ => {
                let v = f.value.as_str().ok_or_else(|| {
                    DataError::Invalid(format!("フィルタ '{}' は文字列で指定してください", f.field))
                })?;
                Ok(ListFilter::TextEq {
                    field: field.name.clone(),
                    value: v.to_string(),
                })
            }
        }
    }

    /// 宣言フィールド・索引付き・マスク非対象を確認して FieldDef を返す。
    fn indexed_queryable<'s>(
        table: &'s crate::model::DataTable,
        masked: &std::collections::HashSet<String>,
        name: &str,
    ) -> Result<&'s crate::model::FieldDef, DataError> {
        // マスク対象は filter/sort/group_by/metrics から除外（PIT-19・403）。
        Self::ensure_queryable(masked, name)?;
        let f = table.schema.field(name).ok_or_else(|| {
            DataError::Invalid(format!("フィールド '{name}' はスキーマに存在しません"))
        })?;
        if !(f.indexed || f.unique) {
            return Err(DataError::Invalid(format!(
                "フィールド '{name}' はフィルタ/ソート対象として宣言されていません（indexed）"
            )));
        }
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    //! クエリフィルタのコンパイル（純粋・DB 不要）: 型別 ListFilter 写像・索引/マスク検証。
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::pedantic
    )]

    use super::*;
    use crate::model::{DataTable, FieldDef, TableSchema};
    use crate::store::DataStore;
    use std::collections::HashSet;

    fn field(name: &str, ty: FieldType, indexed: bool) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type: ty,
            required: false,
            unique: false,
            indexed,
            options: if matches!(ty, FieldType::MultiSelect | FieldType::Select) {
                vec!["a".into(), "b".into()]
            } else {
                vec![]
            },
            ref_table: None,
            lookup: None,
            computed: None,
        }
    }

    fn table() -> DataTable {
        let ts = chrono::DateTime::from_timestamp(0, 0).unwrap();
        DataTable {
            id: uuid::Uuid::nil(),
            name: "t".into(),
            app_id: None,
            schema: TableSchema {
                fields: vec![
                    field("title", FieldType::Text, true),
                    field("amount", FieldType::Number, true),
                    field("tags", FieldType::MultiSelect, true),
                    field("memo", FieldType::Text, false),
                ],
                status_field: None,
                row_policy: None,
                field_policy: vec![],
                aggregate_min_rows: None,
                fsm_ref: None,
            },
            schema_version: 1,
            created_by: "alice".into(),
            created_at: ts,
            updated_at: ts,
        }
    }

    fn qf(field: &str, value: serde_json::Value) -> QueryFilter {
        QueryFilter {
            field: field.into(),
            value,
        }
    }

    #[test]
    fn indexed_queryable_enforces_mask_declared_and_indexed() {
        let t = table();
        let mut masked = HashSet::new();
        masked.insert("title".to_string());
        // マスク列は 403（Forbidden）。
        assert!(matches!(
            DataStore::indexed_queryable(&t, &masked, "title"),
            Err(DataError::Forbidden)
        ));
        // 未知フィールド。
        assert!(DataStore::indexed_queryable(&t, &HashSet::new(), "nope").is_err());
        // 非 indexed フィールド。
        assert!(DataStore::indexed_queryable(&t, &HashSet::new(), "memo").is_err());
        // 索引付きは OK。
        assert!(DataStore::indexed_queryable(&t, &HashSet::new(), "title").is_ok());
    }

    #[test]
    fn compile_filter_maps_by_field_type() {
        let t = table();
        let m = HashSet::new();
        // number → NumberEq（非数値は拒否）。
        assert!(matches!(
            DataStore::compile_query_filter(&t, &m, &qf("amount", serde_json::json!(5))).unwrap(),
            ListFilter::NumberEq { .. }
        ));
        assert!(
            DataStore::compile_query_filter(&t, &m, &qf("amount", serde_json::json!("x"))).is_err()
        );
        // multi_select → MultiContains（非文字列は拒否）。
        assert!(matches!(
            DataStore::compile_query_filter(&t, &m, &qf("tags", serde_json::json!("a"))).unwrap(),
            ListFilter::MultiContains { .. }
        ));
        assert!(
            DataStore::compile_query_filter(&t, &m, &qf("tags", serde_json::json!(1))).is_err()
        );
        // text → TextEq（非文字列は拒否）。
        assert!(matches!(
            DataStore::compile_query_filter(&t, &m, &qf("title", serde_json::json!("hi"))).unwrap(),
            ListFilter::TextEq { .. }
        ));
        assert!(
            DataStore::compile_query_filter(&t, &m, &qf("title", serde_json::json!(1))).is_err()
        );
    }

    #[test]
    fn ensure_queryable_flags_masked() {
        let mut m = HashSet::new();
        m.insert("secret".to_string());
        assert!(matches!(
            DataStore::ensure_queryable(&m, "secret"),
            Err(DataError::Forbidden)
        ));
        assert!(DataStore::ensure_queryable(&m, "public").is_ok());
    }
}
