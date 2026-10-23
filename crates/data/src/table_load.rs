//! テーブル行の読込（スキーマキャッシュ経由・Task 15.9）と、読取の入口の認可（BatchCheck に束ねる）。
//!
//! - [`DataStore::fetch_live`]: 生存テーブル行を引く。スキーマ本文は [`crate::schema_cache`] の版が DB と
//!   一致すれば転送も解釈もしない（照合は毎回 DB の 1 クエリ）。v1 形式・索引台帳の未初期化は移行してから返す。
//! - [`DataStore::open_for_read`]: 読取の入口。テーブル viewer・1 段目の参照先テーブル viewer・名指しロール
//!   （`has_role(subtree=true)`）の check を **BatchCheck 1 往復**へ束ねてリクエストのメモへ入れ、テーブル
//!   viewer を要求する（data-platform §16.4）。

use std::sync::Arc;

use authz::{AuthContext, FgaObject, Relation};
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use uuid::Uuid;

use crate::key_migration::KEY_FORMAT_LEGACY;
use crate::model::{DataTable, TableKeys, TableSchema};
use crate::policy::ast::PolicyExpr;
use crate::policy::material::{analyze, role_object, Needs};
use crate::policy::memo::AuthzMemo;
use crate::store::DataStore;
use crate::{map_db, DataError};

/// data_table 行（スキーマ本文はキャッシュの版と一致すれば NULL）。
#[derive(sqlx::FromRow)]
struct LiveRow {
    id: Uuid,
    name: String,
    app_id: Option<Uuid>,
    schema_version: i64,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    tenant_int: i32,
    table_int: i64,
    key_format: i16,
    pivot_index_init: bool,
    has_record_shares: bool,
    planner_fixed: bool,
    schema: Option<Json<TableSchema>>,
}

/// 生存行の読込結果。
enum Loaded {
    Ready(Box<DataTable>),
    Missing,
    /// v1 形式（識別子の移行が要る）。
    Legacy,
    /// 索引台帳が未初期化（Task 15.2 より前のテーブル）。
    NoLedger,
}

impl DataStore {
    /// 副作用なしで生存行を 1 クエリで読む（スキーマ本文はキャッシュの版と一致すれば省く）。
    async fn load_live(&self, ctx: &AuthContext, id: Uuid) -> Result<Loaded, DataError> {
        let probe = self.schema_cache.probe(&ctx.tenant_id, id);
        let (cached_version, cached_format) = probe.version();
        let row: Option<LiveRow> = sqlx::query_as(
            "SELECT id, name, app_id, schema_version, created_by, created_at, updated_at, \
                    tenant_int, table_int, key_format, pivot_index_init, has_record_shares, planner_fixed, \
                    CASE WHEN schema_version = $4 AND key_format = $5 THEN NULL ELSE schema END \
                      AS schema \
             FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(id)
        .bind(cached_version)
        .bind(cached_format)
        .fetch_optional(self.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(map_db)?;
        let Some(row) = row else {
            return Ok(Loaded::Missing);
        };
        if row.key_format == KEY_FORMAT_LEGACY {
            return Ok(Loaded::Legacy);
        }
        if !row.pivot_index_init {
            return Ok(Loaded::NoLedger);
        }
        let schema = match (probe.hit(row.schema_version, row.key_format), row.schema) {
            (Some(s), _) => (*s).clone(),
            (None, Some(Json(s))) => {
                self.schema_cache.insert(
                    &probe,
                    &ctx.tenant_id,
                    id,
                    row.schema_version,
                    row.key_format,
                    Arc::new(s.clone()),
                );
                s
            }
            // 版が一致したのに手元に本文が無い（並行の世代切替）。本文を引き直させる。
            (None, None) => {
                return Err(DataError::Conflict(
                    "テーブルのスキーマが更新されました。再試行してください".into(),
                ))
            }
        };
        Ok(Loaded::Ready(Box::new(DataTable {
            id: row.id,
            name: row.name,
            app_id: row.app_id,
            schema,
            schema_version: row.schema_version,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
            keys: TableKeys {
                tenant_int: row.tenant_int,
                table_int: row.table_int,
            },
            has_record_shares: row.has_record_shares,
            planner_fixed: row.planner_fixed,
        })))
    }

    /// 生存テーブル行を引く（認可済み前提の内部ヘルパ）。
    ///
    /// v1 形式（`key_format = 1`）のテーブルは**その場で 3 層へ移行してから**返す（移行ジョブと
    /// 同じ関数・テーブル行ロックで直列化・冪等）。返るテーブルは常に `"f{id}"` 形式なので、
    /// 呼び出し側は旧形式を扱わない。
    pub(crate) async fn fetch_live(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<DataTable, DataError> {
        for _ in 0..3 {
            match self.load_live(ctx, id).await? {
                Loaded::Ready(t) => return Ok(*t),
                Loaded::Missing => return Err(DataError::NotFound),
                Loaded::Legacy => {
                    self.migrate_table_field_keys(&ctx.tenant_id, id).await?;
                }
                // Task 15.2 より前に作られたテーブル: 索引台帳を作ってから使う（以降の書込は
                // write-through・既存レコードはバックフィル）。冪等・テーブル行ロックで直列化。
                Loaded::NoLedger => {
                    self.init_pivot_index(&ctx.tenant_id, id).await?;
                }
            }
        }
        Err(DataError::Internal(
            "テーブルの識別子移行が収束しません".into(),
        ))
    }

    /// 移行を伴わずにそのまま使える生存行なら返す（読取の入口の先読み用・書込はしない）。
    async fn peek_live(&self, ctx: &AuthContext, id: Uuid) -> Result<Option<DataTable>, DataError> {
        Ok(match self.load_live(ctx, id).await? {
            Loaded::Ready(t) => Some(*t),
            Loaded::Missing | Loaded::Legacy | Loaded::NoLedger => None,
        })
    }

    /// 読取の入口: テーブル viewer を要求し、生存テーブル行を返す（Task 15.9）。
    ///
    /// テーブル行は認可の**前に**副作用なしで読むが、認可が通るまで何にも使わない（不在・移行要の
    /// テーブルは従来どおり「認可 → 読込」の順。応答は `require` → `fetch_live` と同じ）。読んだ
    /// スキーマから、このリクエストで要る check（参照先テーブルの viewer・名指しロール）を
    /// テーブル viewer と同じ BatchCheck に束ねてメモへ入れる。
    pub(crate) async fn open_for_read(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table_id: Uuid,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let peeked = self.peek_live(ctx, table_id).await?;
        let mut items = vec![(Relation::Viewer, ctx.ns().data_table(&table_id.to_string()))];
        if let Some(t) = &peeked {
            self.read_prefetch(ctx, t, &mut items).await?;
        }
        az.check_all(ctx, &items).await?;
        self.require_in(az, ctx, table_id, Relation::Viewer, action, trace_id)
            .await?;
        match peeked {
            Some(t) => Ok(t),
            None => self.fetch_live(ctx, table_id).await,
        }
    }

    /// 読取で要る check を集める（1 段目の参照先テーブル viewer・自他のテーブルの名指しロール）。
    async fn read_prefetch(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        items: &mut Vec<(Relation, FgaObject)>,
    ) -> Result<(), DataError> {
        let mut needs = Needs::default();
        policy_role_needs(&table.schema, &mut needs);
        for rt in crate::derived::first_hop_tables(table) {
            items.push((Relation::Viewer, ctx.ns().data_table(&rt.to_string())));
            if let Some(r) = self.peek_live(ctx, rt).await? {
                policy_role_needs(&r.schema, &mut needs);
            }
        }
        items.extend(
            needs
                .members
                .iter()
                .filter_map(|role| role_object(ctx, role))
                .map(|o| (Relation::Member, o)),
        );
        Ok(())
    }
}

/// スキーマの行述語（read）とフィールドマスクが名指しするロール。
fn policy_role_needs(schema: &TableSchema, needs: &mut Needs) {
    let exprs: Vec<&PolicyExpr> = schema
        .row_policy
        .iter()
        .map(|p| &p.read)
        .chain(schema.field_policy.iter().map(|p| &p.readable_by))
        .collect();
    for e in exprs {
        analyze(e, needs);
    }
}
