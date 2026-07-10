//! `DataStore` — 構造化データの単一チョークポイント（`&AuthContext` 経由・PgPool 内包）。
//!
//! テーブルの作成・取得・一覧・スキーマ改訂・論理削除。レコード操作は
//! [`crate::record`]、リビジョンは [`crate::revision`]（同じ struct の impl 分割）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, ObjectType, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::index::ensure_indexes;
use crate::model::{DataTable, TableSchema};
use crate::schema::{validate_schema_update, validate_table_name, validate_table_schema};
use crate::validate::RefResolver;
use crate::{map_db, DataError};

/// 新規テーブルの入力。
#[derive(Debug, Clone)]
pub struct NewDataTable {
    pub name: String,
    pub schema: TableSchema,
}

/// data_table 行。
#[derive(sqlx::FromRow)]
struct TableRow {
    id: Uuid,
    name: String,
    app_id: Option<Uuid>,
    schema: Json<TableSchema>,
    schema_version: i64,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TableRow {
    fn into_table(self) -> DataTable {
        DataTable {
            id: self.id,
            name: self.name,
            app_id: self.app_id,
            schema: self.schema.0,
            schema_version: self.schema_version,
            created_by: self.created_by,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// 構造化データのチョークポイント。
#[derive(Clone)]
pub struct DataStore {
    pub(crate) db: PgPool,
    pub(crate) authz: Arc<dyn AuthzClient>,
    pub(crate) audit: AuditRecorder,
    pub(crate) resolver: Arc<dyn RefResolver>,
}

impl DataStore {
    pub fn new(db: PgPool, authz: Arc<dyn AuthzClient>, resolver: Arc<dyn RefResolver>) -> Self {
        let audit = AuditRecorder::new(db.clone());
        DataStore {
            db,
            authz,
            audit,
            resolver,
        }
    }

    /// テーブルを作成する（スキーマ検証＋式インデックス適用＋作成者 owner タプル）。
    pub async fn create_table(
        &self,
        ctx: &AuthContext,
        input: NewDataTable,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let name = validate_table_name(&input.name)?;
        validate_table_schema(&input.schema)?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        let row: TableRow = sqlx::query_as(
            "INSERT INTO data_table (tenant_id, org, name, schema, created_by) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING id, name, app_id, schema, schema_version, created_by, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(name)
        .bind(Json(&input.schema))
        .bind(&ctx.principal.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                DataError::Conflict(format!("テーブル名 '{name}' は既に存在します"))
            }
            _ => map_db(e),
        })?;
        // 作成直後（空テーブル）に宣言インデックスを張る（同一 Tx・即時完了）。
        ensure_indexes(&mut tx, &ctx.tenant_id, row.id, &input.schema).await?;
        tx.commit().await.map_err(map_db)?;

        // 作成者を owner に（FGA）。失敗したら行を補償削除して孤立行を残さない。
        let id = row.id;
        let obj = ctx.ns().data_table(&id.to_string());
        if let Err(e) = self
            .authz
            .write_tuple(&ctx.subject(), Relation::Owner, &obj)
            .await
        {
            if let Err(cleanup) =
                sqlx::query("DELETE FROM data_table WHERE tenant_id = $1 AND id = $2")
                    .bind(&ctx.tenant_id)
                    .bind(id)
                    .execute(&self.db)
                    .await
            {
                tracing::error!(error = %cleanup, table_id = %id, "owner タプル書込失敗後の補償削除にも失敗（孤立行が残存）");
            }
            return Err(DataError::Internal(format!("owner tuple: {e}")));
        }
        self.record_audit(
            ctx,
            "data.table.create",
            &id.to_string(),
            trace_id,
            json!({ "name": name }),
        )
        .await?;
        Ok(row.into_table())
    }

    /// テーブルのメタ＋スキーマを取得する（viewer）。
    pub async fn get_table(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.require(ctx, id, Relation::Viewer, "data.table.get", trace_id)
            .await?;
        self.fetch_live(ctx, id).await
    }

    /// 自分が使えるテーブル一覧（FGA viewer 実効集合 → DB 突合の二段・owner 含む）。
    pub async fn list_tables(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        let limit = limit.clamp(1, 200);
        let objs = self
            .authz
            .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::DataTable)
            .await
            .map_err(|e| DataError::Internal(e.to_string()))?;
        let mut ids: Vec<Uuid> = Vec::new();
        for o in objs {
            let Some((_, id_part)) = o.split_once(':') else {
                continue;
            };
            if let Some(local) = ctx.ns().strip_object_id(id_part) {
                if let Ok(id) = Uuid::parse_str(local) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<TableRow> = sqlx::query_as(
            "SELECT id, name, app_id, schema, schema_version, created_by, created_at, updated_at \
             FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL \
             ORDER BY updated_at DESC, id DESC LIMIT $4",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(TableRow::into_table).collect())
    }

    /// スキーマを改訂する（owner・additive のみ・式インデックス差分適用・楽観ロック）。
    pub async fn update_table_schema(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        next: TableSchema,
        expected_schema_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.require(
            ctx,
            id,
            Relation::Owner,
            "data.table.update_schema",
            trace_id,
        )
        .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let current: Option<TableRow> = sqlx::query_as(
            "SELECT id, name, app_id, schema, schema_version, created_by, created_at, updated_at \
             FROM data_table \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let current = current.ok_or(DataError::NotFound)?;
        if let Some(expected) = expected_schema_version {
            if expected != current.schema_version {
                return Err(DataError::Conflict(format!(
                    "schema_version が一致しません（現在 {}）",
                    current.schema_version
                )));
            }
        }
        validate_schema_update(&current.schema.0, &next)?;
        let row: TableRow = sqlx::query_as(
            "UPDATE data_table \
             SET schema = $3, schema_version = schema_version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 \
             RETURNING id, name, app_id, schema, schema_version, created_by, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(Json(&next))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        ensure_indexes(&mut tx, &ctx.tenant_id, id, &next).await?;
        tx.commit().await.map_err(map_db)?;
        self.record_audit(
            ctx,
            "data.table.update_schema",
            &id.to_string(),
            trace_id,
            json!({ "schema_version": row.schema_version }),
        )
        .await?;
        Ok(row.into_table())
    }

    /// テーブルを論理削除する（owner・レコード/履歴は保持・名前は再利用可能になる）。
    pub async fn delete_table(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require(ctx, id, Relation::Owner, "data.table.delete", trace_id)
            .await?;
        let updated = sqlx::query(
            "UPDATE data_table SET deleted_at = now(), updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(DataError::NotFound);
        }
        self.record_audit(
            ctx,
            "data.table.delete",
            &id.to_string(),
            trace_id,
            json!({}),
        )
        .await
    }

    /// 生存テーブル行を引く（認可済み前提の内部ヘルパ）。
    pub(crate) async fn fetch_live(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<DataTable, DataError> {
        let row: Option<TableRow> = sqlx::query_as(
            "SELECT id, name, app_id, schema, schema_version, created_by, created_at, updated_at \
             FROM data_table WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.ok_or(DataError::NotFound)?.into_table())
    }

    /// テーブルへの relation を要求する（不足は監査 deny＋Forbidden・剥奪即時反映）。
    pub(crate) async fn require(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        let obj = ctx.ns().data_table(&id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                relation,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| DataError::Internal(e.to_string()))?;
        if !ok {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "data_table",
                        object_id: &id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await;
            return Err(DataError::Forbidden);
        }
        Ok(())
    }

    pub(crate) async fn record_audit(
        &self,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), DataError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "data_table",
                    object_id,
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(|e| DataError::Internal(format!("audit: {e}")))
    }
}
