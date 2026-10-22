//! 読取 SQL の唯一の組み立て点（行述語の無条件合成・Task 9.3）。
//!
//! 各関数はプレースホルダを次の順で採番する:
//! `$1 = tenant_id, $2 = table_id, $3 = tenant_int, $4 = table_int, $5.. = 行述語バインド,
//! その後 = 追加条件（id/filter/limit/offset）`。`tenant_int` / `table_int` は `data_record` の
//! ハッシュパーティションを 1 つへ枝刈りし主キーで引くための条件（Task 15.2）。外へは出さない。
//! 値は全てバインド。SQL テキストへ埋め込むのは内部 ID 由来の JSONB キー（`"f{id}"`・
//! [`crate::field_key::is_json_key`] で再検証）と固定エイリアスのみ。

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

/// 全読取 SQL の先頭条件（`r.tenant_id = $1 AND r.table_id = $2 AND r.tenant_int = $3 AND
/// r.table_int = $4`・tenant スコープ＋パーティション枝刈り）が占有するプレースホルダ数
/// （行述語は `$5` から）。
pub(crate) const SCOPE_BINDS: usize = 4;

/// 読取述語の解決結果（1 リクエスト分）。
pub(crate) struct ReadPredicate {
    /// `(...)` 自己完結の SQL 断片（エイリアス `r`）。
    sql: String,
    binds: Vec<Bind>,
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
        let mut binds = BindSet::new(SCOPE_BINDS);
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
        let id_ph = SCOPE_BINDS + 1 + pred.binds.len();
        let sql = format!(
            "SELECT {SELECT_COLS} FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 \
               AND r.tenant_int = $3 AND r.table_int = $4 AND ({}) AND r.id = ${id_ph}",
            pred.sql
        );
        let q = bind_all(
            sqlx::query_as::<Postgres, RecordRow>(&sql),
            ctx,
            table,
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
        let id_ph = SCOPE_BINDS + 1 + pred.binds.len();
        let sql = format!(
            "SELECT {SELECT_COLS} FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 \
               AND r.tenant_int = $3 AND r.table_int = $4 AND ({}) AND r.id = ${id_ph} FOR UPDATE",
            pred.sql
        );
        let q = bind_all(
            sqlx::query_as::<Postgres, RecordRow>(&sql),
            ctx,
            table,
            &pred.binds,
        )
        .bind(id);
        q.fetch_optional(&mut *conn).await.map_err(map_db)
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
        let mut binds = BindSet::new(SCOPE_BINDS);
        let pred = compile_expr(
            actor,
            &table.schema,
            &m,
            &ctx.principal.id,
            RECORD_ALIAS,
            &mut binds,
        )?;
        let id_ph = SCOPE_BINDS + 1 + binds.binds.len();
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 \
               AND r.tenant_int = $3 AND r.table_int = $4 AND ({pred}) AND r.id = ${id_ph})"
        );
        let mut q = sqlx::query_scalar::<Postgres, bool>(&sql)
            .bind(&ctx.tenant_id)
            .bind(table.id)
            .bind(table.keys.tenant_int)
            .bind(table.keys.table_int);
        for b in &binds.binds {
            q = push_bind_scalar(q, b);
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
        let mut binds = BindSet::new(SCOPE_BINDS);
        let pred = compile_expr(
            write_expr,
            &table.schema,
            &m,
            &ctx.principal.id,
            RECORD_ALIAS,
            &mut binds,
        )?;
        let id_ph = SCOPE_BINDS + 1 + binds.binds.len();
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 \
               AND r.tenant_int = $3 AND r.table_int = $4 AND ({pred}) AND r.id = ${id_ph})"
        );
        let mut q = sqlx::query_scalar::<Postgres, bool>(&sql)
            .bind(&ctx.tenant_id)
            .bind(table.id)
            .bind(table.keys.tenant_int)
            .bind(table.keys.table_int);
        for b in &binds.binds {
            q = push_bind_scalar(q, b);
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
        let ids_ph = SCOPE_BINDS + 1 + pred.binds.len();
        let sql = format!(
            "SELECT r.id, r.data -> '{target_json_key}' FROM data_record {RECORD_ALIAS} \
             WHERE r.tenant_id = $1 AND r.table_id = $2 \
               AND r.tenant_int = $3 AND r.table_int = $4 AND ({}) AND r.id = ANY(${ids_ph}::uuid[])",
            pred.sql
        );
        let mut q = sqlx::query_as::<Postgres, (Uuid, Option<serde_json::Value>)>(&sql)
            .bind(&ctx.tenant_id)
            .bind(ref_table.id)
            .bind(ref_table.keys.tenant_int)
            .bind(ref_table.keys.table_int);
        q = crate::query::sql::bind_query_as(q, &pred.binds);
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
    /// 個別共有（OpenFGA の `data_record` タプルは uuid）と record_ref の比較値を pivot 索引側の述語へ
    /// 渡すための入口。呼び出し側は材料の上限（`MAX_SHARED_IDS`）で切り詰め済みの集合を渡すこと。
    /// 結果はリクエストスコープでのみ使い、**外へ出さない**（PIT-75）。存在しない uuid は結果に含まれない。
    pub(crate) async fn record_ints_for(
        conn: &mut PgConnection,
        tenant_id: &str,
        keys: crate::model::TableKeys,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, i64>, DataError> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT id, record_int FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 AND tenant_id = $3 AND id = ANY($4::uuid[])",
        )
        .bind(keys.tenant_int)
        .bind(keys.table_int)
        .bind(tenant_id)
        .bind(ids.to_vec())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().collect())
    }
}

/// 共通バインド（tenant, table, 述語バインド）を適用する。
fn bind_all<'q>(
    q: QueryAs<'q, Postgres, RecordRow, PgArguments>,
    ctx: &'q AuthContext,
    table: &DataTable,
    binds: &[Bind],
) -> QueryAs<'q, Postgres, RecordRow, PgArguments> {
    let mut q = q
        .bind(&ctx.tenant_id)
        .bind(table.id)
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int);
    for b in binds {
        q = push_bind(q, b);
    }
    q
}

fn push_bind<'q>(
    q: QueryAs<'q, Postgres, RecordRow, PgArguments>,
    b: &Bind,
) -> QueryAs<'q, Postgres, RecordRow, PgArguments> {
    crate::query::sql::bind_query_as(q, std::slice::from_ref(b))
}

fn push_bind_scalar<'q, T>(
    q: sqlx::query::QueryScalar<'q, Postgres, T, PgArguments>,
    b: &Bind,
) -> sqlx::query::QueryScalar<'q, Postgres, T, PgArguments> {
    crate::query::sql::bind_scalar(q, std::slice::from_ref(b))
}
