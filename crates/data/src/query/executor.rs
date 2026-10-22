//! 読取 SQL の唯一の組み立て点（行述語の無条件合成・Task 9.3）。
//!
//! 各関数はプレースホルダを次の順で採番する:
//! `$1 = tenant_id, $2 = table_id, $3.. = 行述語バインド, その後 = 追加条件（id/filter/limit/offset）`。
//! 値は全てバインド。SQL テキストへ埋め込むのは内部 ID 由来の JSONB キー（`"f{id}"`・
//! [`crate::field_key::is_json_key`] で再検証）と固定エイリアスのみ。

use std::fmt::Write as _;

use authz::{AuthContext, Consistency, Relation};
use sqlx::postgres::PgArguments;
use sqlx::query::QueryAs;
use sqlx::{PgConnection, Postgres};
use uuid::Uuid;

use crate::field_key::is_json_key;
use crate::model::DataTable;
use crate::policy::compile::{compile_expr, compile_read_predicate, Bind, BindSet, RECORD_ALIAS};
use crate::policy::material;
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 一覧の追加フィルタ（record_list が型検証済みで渡す・閉じた形）。
///
/// `field` は保存形の JSONB キー（`"f{id}"`）。参照名（key）からの変換は呼び出し側が行う。
#[derive(Debug, Clone)]
pub(crate) enum ListFilter {
    /// text 系（text/select/date/datetime/user_ref/role_ref/file_ref/record_ref）の等値。
    TextEq { field: String, value: String },
    /// number の等値。
    NumberEq { field: String, value: f64 },
    /// multi_select の包含（GIN `?`）。
    MultiContains { field: String, value: String },
}

/// 一覧のソート（record_list が検証済みで渡す）。
#[derive(Debug, Clone)]
pub(crate) struct ListSort {
    pub field: String,
    pub numeric: bool,
    pub descending: bool,
}

/// 読取述語の解決結果（1 リクエスト分）。
pub(crate) struct ReadPredicate {
    /// `(...)` 自己完結の SQL 断片（エイリアス `r`）。
    sql: String,
    binds: Vec<Bind>,
    pub shares_truncated: bool,
}

impl ReadPredicate {
    /// 述語 SQL 断片（集計クエリ組み立てが参照する）。
    pub(crate) fn sql(&self) -> &str {
        &self.sql
    }
    /// 述語のバインド列（$3 起点で順に bind する）。
    pub(crate) fn binds(&self) -> &[Bind] {
        &self.binds
    }

    /// テスト専用: SQL 断片とバインドから直接組む（集計 SQL 組み立ての純関数検証用）。
    #[cfg(test)]
    pub(crate) fn for_test(sql: impl Into<String>, binds: Vec<Bind>) -> Self {
        ReadPredicate {
            sql: sql.into(),
            binds,
            shares_truncated: false,
        }
    }
}

/// レコード行を DTO へ変換する（宣言的クエリ層から使う）。
pub(crate) fn row_into_record(
    row: RecordRow,
    schema: &crate::model::TableSchema,
) -> crate::model::DataRecord {
    row.into_record(schema)
}

use crate::record::RECORD_COLS as SELECT_COLS;

impl DataStore {
    /// 読取述語を解決する（材料解決込み・プレースホルダは $3 起点）。
    pub(crate) async fn read_predicate(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
    ) -> Result<ReadPredicate, DataError> {
        let exprs: Vec<&crate::policy::PolicyExpr> = table
            .schema
            .row_policy
            .as_ref()
            .map(|p| vec![&p.read])
            .unwrap_or_default();
        let m = material::resolve(ctx, self.authz.as_ref(), &exprs).await?;
        let mut binds = BindSet::new(2);
        let sql = compile_read_predicate(
            &table.schema,
            &m,
            &ctx.principal.id,
            RECORD_ALIAS,
            &mut binds,
        )?;
        Ok(ReadPredicate {
            sql,
            binds: binds.binds,
            shares_truncated: m.shares_truncated,
        })
    }

    /// id 指定の可視行を引く（不可視は None＝存在と同型・存在オラクルなし）。
    pub(crate) async fn select_visible_by_id(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        id: Uuid,
    ) -> Result<Option<RecordRow>, DataError> {
        let pred = self.read_predicate(ctx, table).await?;
        let id_ph = 3 + pred.binds.len();
        let sql = format!(
            "SELECT {SELECT_COLS} FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({}) AND r.id = ${id_ph}",
            pred.sql
        );
        let q = bind_all(
            sqlx::query_as::<Postgres, RecordRow>(&sql),
            ctx,
            table.id,
            &pred.binds,
        )
        .bind(id);
        q.fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
            .await
            .map_err(map_db)
    }

    /// id 指定の可視行を `FOR UPDATE` でロックして引く（update/delete/遷移の入口）。
    ///
    /// 呼び出し側のトランザクション上で実行する。不可視は None（rev オラクル封じ）。
    pub(crate) async fn lock_visible_by_id(
        &self,
        ctx: &AuthContext,
        conn: &mut PgConnection,
        table: &DataTable,
        id: Uuid,
    ) -> Result<Option<RecordRow>, DataError> {
        let pred = self.read_predicate(ctx, table).await?;
        let id_ph = 3 + pred.binds.len();
        let sql = format!(
            "SELECT {SELECT_COLS} FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({}) AND r.id = ${id_ph} FOR UPDATE",
            pred.sql
        );
        let q = bind_all(
            sqlx::query_as::<Postgres, RecordRow>(&sql),
            ctx,
            table.id,
            &pred.binds,
        )
        .bind(id);
        q.fetch_optional(&mut *conn).await.map_err(map_db)
    }

    /// 可視行の一覧（フィルタ/ソートは検証済み・述語と AND 合成）。
    pub(crate) async fn select_visible_rows(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        filter: Option<&ListFilter>,
        sort: Option<&ListSort>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<RecordRow>, bool), DataError> {
        let pred = self.read_predicate(ctx, table).await?;
        let mut sql = format!(
            "SELECT {SELECT_COLS} FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({})",
            pred.sql
        );
        let mut next_ph = 3 + pred.binds.len();
        let filter_bind = push_filter_sql(&mut sql, filter, &mut next_ph)?;
        match sort {
            Some(s) => {
                let dir = if s.descending { "DESC" } else { "ASC" };
                let expr = sort_expr(s)?;
                let _ = write!(sql, " ORDER BY {expr} {dir} NULLS LAST, r.id {dir}");
            }
            None => sql.push_str(" ORDER BY r.updated_at DESC, r.id DESC"),
        }
        let _ = write!(sql, " LIMIT ${next_ph} OFFSET ${}", next_ph + 1);

        let mut q = bind_all(
            sqlx::query_as::<Postgres, RecordRow>(&sql),
            ctx,
            table.id,
            &pred.binds,
        );
        if let Some(b) = filter_bind {
            q = push_bind(q, b);
        }
        let rows = q
            .bind(limit)
            .bind(offset)
            .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
            .await
            .map_err(map_db)?;
        Ok((rows, pred.shares_truncated))
    }

    /// 可視行の件数（集計にも述語を適用する・Task 9.3 受け入れ条件）。
    pub(crate) async fn count_visible_rows(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        filter: Option<&ListFilter>,
    ) -> Result<i64, DataError> {
        let pred = self.read_predicate(ctx, table).await?;
        let mut sql = format!(
            "SELECT count(*) FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({})",
            pred.sql
        );
        let mut next_ph = 3 + pred.binds.len();
        let filter_bind = push_filter_sql(&mut sql, filter, &mut next_ph)?;
        let mut q = sqlx::query_scalar::<Postgres, i64>(&sql)
            .bind(&ctx.tenant_id)
            .bind(table.id);
        for b in &pred.binds {
            q = push_bind_scalar(q, b.clone());
        }
        if let Some(b) = filter_bind {
            q = push_bind_scalar(q, b);
        }
        q.fetch_one(self.pools.for_tenant(&ctx.tenant_id))
            .await
            .map_err(map_db)
    }

    /// actor 述語（FSM 遷移認可・Task 9.10）を**当該行に対し**評価する。
    ///
    /// 読取述語ではなく遷移固有の actor を使う。行は可視前提（ロック済み）で、述語を満たす
    /// 行が 1 件あるか（＝この主体が遷移を実行してよいか）を返す。
    pub(crate) async fn actor_allows(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        record_id: Uuid,
        actor: &crate::policy::PolicyExpr,
    ) -> Result<bool, DataError> {
        let m = material::resolve(ctx, self.authz.as_ref(), &[actor]).await?;
        let mut binds = BindSet::new(2);
        let pred = compile_expr(
            actor,
            &table.schema,
            &m,
            &ctx.principal.id,
            RECORD_ALIAS,
            &mut binds,
        )?;
        let id_ph = 3 + binds.binds.len();
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({pred}) AND r.id = ${id_ph})"
        );
        let mut q = sqlx::query_scalar::<Postgres, bool>(&sql)
            .bind(&ctx.tenant_id)
            .bind(table.id);
        for b in &binds.binds {
            q = push_bind_scalar(q, b.clone());
        }
        q.bind(record_id)
            .fetch_one(self.pools.for_tenant(&ctx.tenant_id))
            .await
            .map_err(map_db)
    }

    /// 対象行への**書込可否**（write 述語 or 個別共有 editor・Task 9.3）。
    ///
    /// 読取述語を通ってロック済みの行に対して呼ぶ（可視は前提）。row_policy 未定義なら
    /// テーブル editor だけで書ける（従来どおり）。
    pub(crate) async fn write_allowed(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        record_id: Uuid,
    ) -> Result<bool, DataError> {
        let Some(policy) = &table.schema.row_policy else {
            return Ok(true);
        };
        let write_expr = policy.write_expr();
        let m = material::resolve(ctx, self.authz.as_ref(), &[write_expr]).await?;
        let mut binds = BindSet::new(2);
        let pred = compile_expr(
            write_expr,
            &table.schema,
            &m,
            &ctx.principal.id,
            RECORD_ALIAS,
            &mut binds,
        )?;
        let id_ph = 3 + binds.binds.len();
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({pred}) AND r.id = ${id_ph})"
        );
        let mut q = sqlx::query_scalar::<Postgres, bool>(&sql)
            .bind(&ctx.tenant_id)
            .bind(table.id);
        for b in &binds.binds {
            q = push_bind_scalar(q, b.clone());
        }
        let by_policy = q
            .bind(record_id)
            .fetch_one(self.pools.for_tenant(&ctx.tenant_id))
            .await
            .map_err(map_db)?;
        if by_policy {
            return Ok(true);
        }
        // 個別共有 editor（スパースタプル）による書込許可。
        self.authz
            .check(
                &ctx.subject(),
                Relation::Editor,
                &ctx.ns().data_record(&record_id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| DataError::Internal(e.to_string()))
    }
}

impl DataStore {
    /// lookup 射影のバッチ取得（参照先テーブルの行述語つき・Task 9.3 / PIT-20）。
    ///
    /// `target_json_key` は参照先フィールドの保存形キー（`"f{id}"`）。
    /// 述語で不可視の参照先は結果に含まれない（呼び出し側で null になる）。
    pub(crate) async fn select_lookup_values(
        &self,
        ctx: &AuthContext,
        ref_table: &DataTable,
        target_json_key: &str,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, serde_json::Value>, DataError> {
        // 呼び出し側（derived）で参照先スキーマ照合済み。埋め込み直前に形式を再検証（二重化）。
        if !is_json_key(target_json_key) {
            return Err(DataError::Internal(format!(
                "lookup 対象の JSONB キー '{target_json_key}' が内部 ID 形式ではありません"
            )));
        }
        let pred = self.read_predicate(ctx, ref_table).await?;
        let ids_ph = 3 + pred.binds.len();
        let sql = format!(
            "SELECT r.id, r.data -> '{target_json_key}' FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 AND ({}) AND r.id = ANY(${ids_ph}::uuid[])",
            pred.sql
        );
        let mut q = sqlx::query_as::<Postgres, (Uuid, Option<serde_json::Value>)>(&sql)
            .bind(&ctx.tenant_id)
            .bind(ref_table.id);
        for b in &pred.binds {
            q = match b.clone() {
                Bind::Text(v) => q.bind(v),
                Bind::TextArray(v) => q.bind(v),
                Bind::Number(v) => q.bind(v),
                Bind::UuidArray(v) => q.bind(v),
            };
        }
        let rows = q
            .bind(ids.to_vec())
            .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
            .await
            .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(id, v)| (id, v.unwrap_or(serde_json::Value::Null)))
            .collect())
    }

    /// 外部 ID（uuid）を内部キー `record_int` へ**1 本のクエリで**引き直す（data-platform §3.2）。
    ///
    /// 個別共有（OpenFGA の `data_record` タプルは uuid）を pivot 索引側の述語へ渡すための入口。
    /// 呼び出し側は材料の上限（`MAX_SHARED_IDS`）で切り詰め済みの集合を渡すこと。結果は
    /// リクエストスコープでのみ使い、**外へ出さない**（PIT-75）。Task 15.3 の材料解決が使う。
    #[allow(dead_code)] // Task 15.3 の材料解決が使う（継ぎ目として先に置く）。
    pub(crate) async fn record_ints_for(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        ids: &[Uuid],
    ) -> Result<Vec<i64>, DataError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ints: Vec<i64> = sqlx::query_scalar(
            "SELECT record_int FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND tenant_id = $3 AND id = ANY($4::uuid[]) \
             ORDER BY record_int",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(&ctx.tenant_id)
        .bind(ids.to_vec())
        .fetch_all(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        Ok(ints)
    }
}

/// フィルタ断片を SQL へ追記し、必要なバインドを返す（プレースホルダは呼び出し側採番）。
fn push_filter_sql(
    sql: &mut String,
    filter: Option<&ListFilter>,
    next_ph: &mut usize,
) -> Result<Option<Bind>, DataError> {
    let Some(f) = filter else { return Ok(None) };
    let (fragment, bind) = match f {
        ListFilter::TextEq { field, value } => (
            format!(" AND ({RECORD_ALIAS}.data ->> '{field}') = ${next_ph}"),
            Bind::Text(value.clone()),
        ),
        ListFilter::NumberEq { field, value } => (
            format!(" AND (({RECORD_ALIAS}.data ->> '{field}'))::numeric = ${next_ph}::numeric"),
            Bind::Number(*value),
        ),
        ListFilter::MultiContains { field, value } => (
            format!(" AND ({RECORD_ALIAS}.data -> '{field}') ? ${next_ph}"),
            Bind::Text(value.clone()),
        ),
    };
    // JSONB キーは呼び出し側が宣言フィールドから生成済み。埋め込み直前に形式を再検証する。
    let field = match f {
        ListFilter::TextEq { field, .. }
        | ListFilter::NumberEq { field, .. }
        | ListFilter::MultiContains { field, .. } => field,
    };
    if !is_json_key(field) {
        return Err(DataError::Internal(format!(
            "フィルタの JSONB キー '{field}' が内部 ID 形式ではありません"
        )));
    }
    sql.push_str(&fragment);
    *next_ph += 1;
    Ok(Some(bind))
}

fn sort_expr(s: &ListSort) -> Result<String, DataError> {
    if !is_json_key(&s.field) {
        return Err(DataError::Internal(format!(
            "ソートの JSONB キー '{}' が内部 ID 形式ではありません",
            s.field
        )));
    }
    Ok(if s.numeric {
        format!("(({RECORD_ALIAS}.data ->> '{}'))::numeric", s.field)
    } else {
        format!("({RECORD_ALIAS}.data ->> '{}')", s.field)
    })
}

/// 共通バインド（tenant, table, 述語バインド）を適用する。
fn bind_all<'q>(
    q: QueryAs<'q, Postgres, RecordRow, PgArguments>,
    ctx: &'q AuthContext,
    table_id: Uuid,
    binds: &[Bind],
) -> QueryAs<'q, Postgres, RecordRow, PgArguments> {
    let mut q = q.bind(&ctx.tenant_id).bind(table_id);
    for b in binds {
        q = push_bind(q, b.clone());
    }
    q
}

fn push_bind(
    q: QueryAs<'_, Postgres, RecordRow, PgArguments>,
    b: Bind,
) -> QueryAs<'_, Postgres, RecordRow, PgArguments> {
    match b {
        Bind::Text(v) => q.bind(v),
        Bind::TextArray(v) => q.bind(v),
        Bind::Number(v) => q.bind(v),
        Bind::UuidArray(v) => q.bind(v),
    }
}

fn push_bind_scalar<T>(
    q: sqlx::query::QueryScalar<'_, Postgres, T, PgArguments>,
    b: Bind,
) -> sqlx::query::QueryScalar<'_, Postgres, T, PgArguments> {
    match b {
        Bind::Text(v) => q.bind(v),
        Bind::TextArray(v) => q.bind(v),
        Bind::Number(v) => q.bind(v),
        Bind::UuidArray(v) => q.bind(v),
    }
}
