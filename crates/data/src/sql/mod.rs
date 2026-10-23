//! SQL 互換インターフェース（前段言語・data-platform §16.1・Task 15.10・PIT-74）。
//!
//! ```text
//! SQL テキスト
//!   ↓ ① パース（sqlparser）・入口の資源上限          … [`parse`]
//!   ↓ ② 検証: 閉じた部分集合か（allowlist）         … [`parse`]（[`ast`] に写せた形だけが先へ進む）
//!   ↓ ③ 束縛: テーブル名 → テーブル（viewer）、列名 → 参照名（見える列だけ）・JOIN → 宣言済みリンク列
//!   ↓ ④ DataQuery IR へ変換（3 値論理 → 2 値論理は [`translate`] の 1 か所）   … [`resolve`]
//!   ↓ ⑤ 以降は IR 直投入と同一（最適化・pivot 索引 SQL へのコンパイル・行述語の無条件合成・値は全バインド）
//! ```
//!
//! **不変条件: 呼び出し側の SQL テキストを 1 文字もデータベースへ届けない。** SQL テキストはこのモジュールの
//! パーサにしか渡らず、データベースへ届くのは IR から [`crate::query`] の降下（`lower.rs`）が生成した
//! テキストだけ（値はすべてバインド・埋め込むのは固定の表名と別名だけ）。データベースへの第 2 の実行経路は無い。
//!
//! 拒否は 422（[`crate::QueryRejection`]・構造化）。存在しない列・見えない列・存在しないテーブル・
//! 見えないテーブルは同じ応答にする（列/テーブルの存在オラクルを作らない）。

pub(crate) mod ast;
pub(crate) mod lex;
pub(crate) mod names;
pub(crate) mod parse;
pub(crate) mod render;
pub(crate) mod resolve;
pub(crate) mod translate;

use std::collections::HashSet;

use authz::AuthContext;
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use self::ast::TableRef;
use self::resolve::{Compiled, OutCol, OutKind, TableInfo};
use crate::index::SortPrecision;
use crate::model::{DataRecord, DataTable};
use crate::policy::memo::AuthzMemo;
use crate::query::ir::{CountResult, DataQuery, QueryResult};
use crate::query::limits::CompileBudget;
use crate::query::reject::{NameKind, QueryRejection};
use crate::store::{DataStore, TableRow, TABLE_COLS};
use crate::{map_db, DataError};

/// SQL での読取の要求。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlRequest {
    /// SELECT 文（閉じた部分集合・データベースへは渡らない）。
    pub sql: String,
    /// 前ページの `next_cursor`（同じ SQL と組で渡す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 件数も数える（上限付き・集計しない問い合わせだけ）。
    #[serde(default)]
    pub with_count: bool,
}

/// SQL での読取の結果（表形式）。推定値・統計値・戦略名・内部キーは含まない（PIT-75）。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SqlQueryResult {
    /// 列名（SELECT の並び・別名があれば別名）。
    pub columns: Vec<String>,
    /// 行（各行は `columns` と同じ並び）。
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<Value>>,
    /// 次ページのカーソル（無ければ終端）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// `with_count: true` のときだけ件数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<CountResult>,
    /// K 未満で抑制されたグループ/全体集計があった（PIT-17）。
    pub suppressed: bool,
    /// 個別共有集合の切り詰め（PIT-18）。
    pub shares_truncated: bool,
    pub sort_precision: SortPrecision,
    pub sort_degraded: bool,
    /// 実行した IR（同じ問い合わせを IR で投げ直す・保存ビューにするため。FROM のテーブルは `table_id`）。
    pub table_id: Uuid,
    pub query: DataQuery,
}

/// SQL を IR へ変換した結果（検証用に公開）。
#[derive(Debug, Clone)]
pub struct CompiledSql {
    pub table_id: Uuid,
    pub query: DataQuery,
    pub columns: Vec<String>,
}

fn rej(r: QueryRejection) -> DataError {
    DataError::Rejected(Box::new(r))
}

impl DataStore {
    /// SQL（閉じた部分集合）で読む（viewer・行述語・フィールドマスク・集計抑制は IR 直投入と同一）。
    pub async fn run_sql(
        &self,
        ctx: &AuthContext,
        req: &SqlRequest,
        trace_id: Option<&str>,
    ) -> Result<SqlQueryResult, DataError> {
        let az = self.authz_memo();
        let mut budget = CompileBudget::new(&self.limits);
        let compiled = self
            .compile_in(ctx, &az, &mut budget, &req.sql, trace_id)
            .await?;
        let mut q = compiled.query.clone();
        q.cursor.clone_from(&req.cursor);
        q.with_count = req.with_count && q.aggregate.is_empty();
        let tuning = self.tuning.clone();
        let (r, _) = self
            .run_query_in(
                &az,
                &mut budget,
                ctx,
                compiled.table_id,
                &q,
                trace_id,
                "data.query.sql",
                &tuning,
                false,
            )
            .await?;
        Ok(tabulate(&compiled, q, r))
    }

    /// 検証専用: SQL を IR へ変換する（実行しない）。名前の解決は閲覧者本人の権限で行う。
    #[doc(hidden)]
    pub async fn compile_sql(
        &self,
        ctx: &AuthContext,
        sql: &str,
    ) -> Result<CompiledSql, DataError> {
        let az = self.authz_memo();
        let mut budget = CompileBudget::new(&self.limits);
        let c = self.compile_in(ctx, &az, &mut budget, sql, None).await?;
        Ok(CompiledSql {
            table_id: c.table_id,
            query: c.query,
            columns: c.columns.into_iter().map(|c| c.name).collect(),
        })
    }

    async fn compile_in(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        budget: &mut CompileBudget,
        sql: &str,
        trace_id: Option<&str>,
    ) -> Result<Compiled, DataError> {
        let limits = self.limits.clone();
        let sel = budget
            .stage(&limits, || parse::parse(sql, &limits))
            .map_err(rej)?
            .map_err(rej)?;
        let mut opened: Vec<(DataTable, HashSet<String>)> = Vec::with_capacity(1 + sel.joins.len());
        for t in std::iter::once(&sel.from).chain(sel.joins.iter().map(|j| &j.table)) {
            opened.push(self.open_by_name(ctx, az, t, trace_id).await?);
        }
        let infos: Vec<TableInfo<'_>> = opened
            .iter()
            .map(|(t, m)| TableInfo {
                id: t.id,
                schema: &t.schema,
                masked: m,
            })
            .collect();
        budget
            .stage(&limits, || {
                resolve::compile(&sel, &infos, limits.max_join_depth)
            })
            .map_err(rej)?
            .map_err(rej)
    }

    /// テーブル名で開く（viewer でないテーブルは存在しないテーブルと同じ応答）。
    async fn open_by_name(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        t: &TableRef,
        trace_id: Option<&str>,
    ) -> Result<(DataTable, HashSet<String>), DataError> {
        let rows: Vec<TableRow> = sqlx::query_as(&format!(
            "SELECT {TABLE_COLS} FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND deleted_at IS NULL AND lower(name) = lower($3) \
             LIMIT 8"
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&t.name)
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let pick = rows
            .iter()
            .position(|r| r.name() == t.name)
            .or_else(|| (!t.quoted && rows.len() == 1).then_some(0));
        let found = match pick {
            Some(i) => {
                let id = rows[i].id;
                match self
                    .open_for_read(ctx, az, id, "data.query.sql", trace_id)
                    .await
                {
                    Ok(table) => Some(table),
                    Err(DataError::NotFound | DataError::Forbidden) => None,
                    Err(e) => return Err(e),
                }
            }
            None => None,
        };
        let Some(table) = found else {
            // 存在しない・見えないを区別しない。候補は閲覧者に見えるテーブルの名前だけ。
            let mut names: Vec<String> = self
                .list_tables(ctx, 50)
                .await
                .map(|v| v.into_iter().map(|t| t.name).collect())
                .unwrap_or_default();
            names.sort();
            return Err(rej(QueryRejection::unknown_name(
                NameKind::Table,
                &t.name,
                names,
            )
            .at(t.loc)));
        };
        let masked = self.masked_fields(ctx, az, &table).await?;
        Ok((table, masked))
    }
}

/// 擬似列の値（FROM の表）。
fn pseudo(r: &DataRecord, key: &str) -> Option<Value> {
    Some(match key {
        "_id" => Value::String(r.id.to_string()),
        "_owner" => Value::String(r.owner.clone()),
        "_created_at" => Value::String(r.created_at.to_rfc3339_opts(SecondsFormat::Micros, true)),
        "_updated_at" => Value::String(r.updated_at.to_rfc3339_opts(SecondsFormat::Micros, true)),
        _ => return None,
    })
}

/// 結果を表にする（列の並びは SELECT の並び）。
fn tabulate(c: &Compiled, query: DataQuery, r: QueryResult) -> SqlQueryResult {
    let columns: Vec<String> = c.columns.iter().map(|x| x.name.clone()).collect();
    let mut rows = Vec::new();
    if let Some(groups) = &r.groups {
        for g in groups {
            rows.push(
                c.columns
                    .iter()
                    .map(|col| match &col.kind {
                        OutKind::Group(i) => query
                            .group_by
                            .get(*i)
                            .and_then(|k| g.key.get(k))
                            .cloned()
                            .unwrap_or(Value::Null),
                        OutKind::Metric(i) => g.values.get(*i).cloned().unwrap_or(Value::Null),
                        OutKind::Field { .. } => Value::Null,
                    })
                    .collect(),
            );
        }
    } else if let Some(items) = &r.items {
        for it in items {
            rows.push(c.columns.iter().map(|col| cell(c, col, it)).collect());
        }
    }
    SqlQueryResult {
        columns,
        rows,
        next_cursor: r.next_cursor,
        count: r.count,
        suppressed: r.suppressed,
        shares_truncated: r.shares_truncated,
        sort_precision: r.sort_precision,
        sort_degraded: r.sort_degraded,
        table_id: c.table_id,
        query,
    }
}

fn cell(c: &Compiled, col: &OutCol, it: &DataRecord) -> Value {
    match &col.kind {
        OutKind::Field { src: 0, key } => pseudo(it, key)
            .or_else(|| it.data.get(key).cloned())
            .unwrap_or(Value::Null),
        OutKind::Field { src, key } => c
            .join_aliases
            .get(src - 1)
            .and_then(|a| it.data.get(format!("{a}.{key}")))
            .cloned()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}
