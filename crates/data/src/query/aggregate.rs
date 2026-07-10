//! 集計実行＋スモールセル抑制（Task 9.4・PIT-17）。
//!
//! 集計にも行述語を無条件合成する（不可視行は件数・合計に混入しない＝permission-aware と
//! 同一保証）。各グループの**実件数が K 未満なら値を返さず抑制**し、少人数セルからの
//! 個人特定・条件を変えた反復差分による逆算を難しくする（DP ではない・監査で補う）。

use std::collections::HashSet;

use authz::AuthContext;
use serde_json::{Map, Value};
use sqlx::postgres::PgArguments;
use sqlx::{Postgres, Row};

use crate::model::{DataTable, FieldType};
use crate::policy::compile::{Bind, RECORD_ALIAS};
use crate::query::declarative::{Aggregate, AggregateGroup, Metric};
use crate::query::executor::ListFilter;
use crate::store::DataStore;
use crate::{map_db, DataError};

impl DataStore {
    /// 集計を実行する。返り値 = (グループ列, 抑制発生, 共有切り詰め)。
    pub(crate) async fn run_aggregate(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        masked: &HashSet<String>,
        agg: &Aggregate,
        filter: Option<&ListFilter>,
        k: i64,
    ) -> Result<(Vec<AggregateGroup>, bool, bool), DataError> {
        // group_by / metric 対象は宣言フィールドかつマスク非対象（PIT-19）。
        for g in &agg.group_by {
            Self::ensure_queryable(masked, g)?;
            Self::require_scalar_field(table, g)?;
        }
        let metric_field = match agg.metric {
            Metric::Count => None,
            Metric::Sum | Metric::Avg | Metric::Min | Metric::Max => {
                let name = agg.field.as_ref().ok_or_else(|| {
                    DataError::Invalid("この集計には field（number）が必要です".into())
                })?;
                Self::ensure_queryable(masked, name)?;
                let f = Self::require_scalar_field(table, name)?;
                if f.field_type != FieldType::Number {
                    return Err(DataError::Invalid(format!(
                        "集計対象 '{name}' は number である必要があります"
                    )));
                }
                Some(name.clone())
            }
        };

        let pred = self.read_predicate(ctx, table).await?;
        let sql = build_aggregate_sql(
            &agg.group_by,
            agg.metric,
            metric_field.as_deref(),
            &pred,
            filter,
        );

        let mut q = sqlx::query::<Postgres>(&sql)
            .bind(&ctx.tenant_id)
            .bind(table.id);
        for b in pred.binds() {
            q = bind_dyn(q, b.clone());
        }
        if let Some(f) = filter {
            q = bind_dyn(q, filter_bind(f));
        }
        let rows = q.fetch_all(&self.db).await.map_err(map_db)?;

        let mut groups = Vec::with_capacity(rows.len());
        let mut suppressed = false;
        for row in rows {
            let cnt: i64 = row.try_get("grp_count").map_err(map_db)?;
            let mut key = Map::new();
            for g in &agg.group_by {
                let v: Option<String> = row.try_get(g.as_str()).map_err(map_db)?;
                key.insert(g.clone(), v.map_or(Value::Null, Value::String));
            }
            // K 未満は値を伏せる（count 含む・全体集計も同様）。
            if cnt < k {
                suppressed = true;
                groups.push(AggregateGroup {
                    key,
                    value: Value::Null,
                });
                continue;
            }
            let value = extract_metric(&row, agg.metric)?;
            groups.push(AggregateGroup { key, value });
        }
        Ok((groups, suppressed, pred.shares_truncated))
    }

    fn require_scalar_field<'s>(
        table: &'s DataTable,
        name: &str,
    ) -> Result<&'s crate::model::FieldDef, DataError> {
        let f = table.schema.field(name).ok_or_else(|| {
            DataError::Invalid(format!("フィールド '{name}' はスキーマに存在しません"))
        })?;
        if matches!(
            f.field_type,
            FieldType::MultiSelect | FieldType::Lookup | FieldType::Computed
        ) {
            return Err(DataError::Invalid(format!(
                "フィールド '{name}' は集計/グループ化に使えません"
            )));
        }
        if !(f.indexed || f.unique) {
            return Err(DataError::Invalid(format!(
                "フィールド '{name}' は集計対象として宣言されていません（indexed）"
            )));
        }
        Ok(f)
    }
}

/// 集計 SQL を組む（group_by・metric・行述語・任意フィルタ）。
///
/// フィールド名は呼び出し側で宣言＋索引＋マスク検証済み。ここでも識別子検証で二重化する。
fn build_aggregate_sql(
    group_by: &[String],
    metric: Metric,
    metric_field: Option<&str>,
    pred: &super::executor::ReadPredicate,
    filter: Option<&ListFilter>,
) -> String {
    use std::fmt::Write as _;
    let alias = RECORD_ALIAS;
    // フィールド名は呼び出し側で宣言＋索引＋マスク検証済みだが、DDL/JSONB アクセサに
    // 埋め込む前に識別子規約で再検証する（PIT-21・二重化）。規約外は空集計へ倒す。
    let safe = |f: &str| crate::schema::is_valid_field_name(f);
    if group_by.iter().any(|g| !safe(g)) || metric_field.is_some_and(|f| !safe(f)) {
        return "SELECT 0 AS grp_count WHERE false".to_string();
    }
    let mut select = String::from("count(*) AS grp_count");
    // group キーを text で射影。
    for g in group_by {
        let _ = write!(select, ", ({alias}.data ->> '{g}') AS \"{g}\"");
    }
    // メトリクス列。
    match metric {
        Metric::Count => {}
        Metric::Sum | Metric::Avg | Metric::Min | Metric::Max => {
            let field = metric_field.unwrap_or("");
            let func = match metric {
                Metric::Sum => "sum",
                Metric::Avg => "avg",
                Metric::Min => "min",
                Metric::Max => "max",
                Metric::Count => unreachable!(),
            };
            // ::float8 で受ける（Rust 側 f64 デコードと一致。avg/sum の NUMERIC を回避）。
            let _ = write!(
                select,
                ", ({func}((({alias}.data ->> '{field}'))::numeric))::float8 AS metric_value"
            );
        }
    }
    let mut sql = format!(
        "SELECT {select} FROM data_record {alias} \
         WHERE {alias}.tenant_id = $1 AND {alias}.table_id = $2 AND ({})",
        pred.sql()
    );
    let mut next_ph = 3 + pred.binds().len();
    if let Some(f) = filter {
        let frag = filter_fragment(f, next_ph);
        sql.push_str(&frag);
        next_ph += 1;
    }
    let _ = next_ph;
    if !group_by.is_empty() {
        let cols: Vec<String> = group_by
            .iter()
            .map(|g| format!("({alias}.data ->> '{g}')"))
            .collect();
        let _ = write!(sql, " GROUP BY {}", cols.join(", "));
    }
    sql
}

fn filter_fragment(f: &ListFilter, ph: usize) -> String {
    let alias = RECORD_ALIAS;
    match f {
        ListFilter::TextEq { field, .. } => {
            format!(" AND ({alias}.data ->> '{field}') = ${ph}")
        }
        ListFilter::NumberEq { field, .. } => {
            format!(" AND (({alias}.data ->> '{field}'))::numeric = ${ph}::numeric")
        }
        ListFilter::MultiContains { field, .. } => {
            format!(" AND ({alias}.data -> '{field}') ? ${ph}")
        }
    }
}

fn filter_bind(f: &ListFilter) -> Bind {
    match f {
        ListFilter::TextEq { value, .. } | ListFilter::MultiContains { value, .. } => {
            Bind::Text(value.clone())
        }
        ListFilter::NumberEq { value, .. } => Bind::Number(*value),
    }
}

fn extract_metric(row: &sqlx::postgres::PgRow, metric: Metric) -> Result<Value, DataError> {
    if metric == Metric::Count {
        let c: i64 = row.try_get("grp_count").map_err(map_db)?;
        return Ok(Value::Number(c.into()));
    }
    // numeric 集計は f64 で受ける（min/max/sum/avg）。NULL はグループなしなら発生しない。
    let v: Option<f64> = row.try_get("metric_value").map_err(map_db)?;
    Ok(v.and_then(serde_json::Number::from_f64)
        .map_or(Value::Null, Value::Number))
}

fn bind_dyn(
    q: sqlx::query::Query<'_, Postgres, PgArguments>,
    b: Bind,
) -> sqlx::query::Query<'_, Postgres, PgArguments> {
    match b {
        Bind::Text(v) => q.bind(v),
        Bind::TextArray(v) => q.bind(v),
        Bind::Number(v) => q.bind(v),
        Bind::UuidArray(v) => q.bind(v),
    }
}
