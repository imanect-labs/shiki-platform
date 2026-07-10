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
            // 補償: 物理インデックス → 行の順で片付ける（インデックスは行に従属しないため
            // 明示 DROP が要る。registry 行は data_table への CASCADE で消える）。
            if let Err(cleanup) = self.drop_table_indexes(&ctx.tenant_id, id).await {
                tracing::error!(error = %cleanup, table_id = %id, "補償時のインデックス削除に失敗（孤立インデックスが残存）");
            }
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
        // テーブルは確定済み。以降の監査失敗は結果に影響させない（重複再試行を防ぐ）。
        self.record_audit_best_effort(
            ctx,
            "data.table.create",
            &id.to_string(),
            trace_id,
            json!({ "name": name }),
        )
        .await;
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
        self.list_tables_filtered(ctx, None, limit).await
    }

    /// アプリ所有 ∩ 自分が viewer のテーブル一覧（app-gateway の所有束縛・Task 9.8）。
    ///
    /// `app_id` の絞り込みを **LIMIT より前に SQL で**行う（可視テーブルが上限を超えても
    /// アプリ所有分が一覧から欠落しない）。
    pub async fn list_app_tables(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        self.list_tables_filtered(ctx, Some(app_id), limit).await
    }

    async fn list_tables_filtered(
        &self,
        ctx: &AuthContext,
        app_id: Option<Uuid>,
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
               AND ($5::uuid IS NULL OR app_id = $5) \
             ORDER BY updated_at DESC, id DESC LIMIT $4",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .bind(limit)
        .bind(app_id)
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
        // 実データ変更のためスキーマ更新と同一 Tx で監査を残す（原子的・Chain=Yes）。
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.table.update_schema",
            &id.to_string(),
            trace_id,
            json!({ "schema_version": row.schema_version }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
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
        self.record_audit_best_effort(
            ctx,
            "data.table.delete",
            &id.to_string(),
            trace_id,
            json!({}),
        )
        .await;
        Ok(())
    }

    /// テナント消去（SAAS.2）: data_table（CASCADE で record/registry/revision も）と
    /// 物理式インデックス・FGA タプルを撤去する。監査ログは保持（purge 方針は storage と同一）。
    pub async fn purge_tenant(&self, tenant_id: &str) -> Result<u32, DataError> {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM data_table WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_all(&self.db)
            .await
            .map_err(map_db)?;
        let ns = authz::Namespace::for_tenant(tenant_id);
        let mut purged = 0u32;
        for id in &ids {
            // 物理インデックス（partial・テーブル行に従属しない）→ FGA タプルの順で撤去。
            self.drop_table_indexes(tenant_id, *id).await?;
            self.authz
                .delete_object_tuples(&ns.data_table(&id.to_string()))
                .await
                .map_err(|e| DataError::Internal(format!("fga purge: {e}")))?;
            purged += 1;
        }
        sqlx::query("DELETE FROM data_table WHERE tenant_id = $1")
            .bind(tenant_id)
            .execute(&self.db)
            .await
            .map_err(map_db)?;
        Ok(purged)
    }

    /// テーブルの物理式インデックスを台帳経由で DROP する（補償・purge 用）。
    pub(crate) async fn drop_table_indexes(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<(), DataError> {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT index_name FROM data_index_registry WHERE tenant_id = $1 AND table_id = $2",
        )
        .bind(tenant_id)
        .bind(table_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        for name in names {
            // 台帳由来（本 crate が決定的命名で作った名前）のみ。防御的に識別子検証する。
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            sqlx::query(&format!("DROP INDEX IF EXISTS {name}"))
                .execute(&self.db)
                .await
                .map_err(map_db)?;
        }
        Ok(())
    }

    /// 生存テーブル行を引く（認可済み前提の内部ヘルパ）。
    pub(crate) async fn fetch_live(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<DataTable, DataError> {
        let row: Option<TableRow> = sqlx::query_as(
            "SELECT id, name, app_id, schema, schema_version, created_by, created_at, updated_at \
             FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = $3 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
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

    /// 既存 Tx 上で監査を残す（実データ変更用・Chain=Yes・書込と原子的）。
    pub(crate) async fn record_audit_on(
        &self,
        conn: &mut sqlx::PgConnection,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), DataError> {
        storage::audit::record_on(
            conn,
            ctx,
            AuditEntry {
                action,
                object_type: "data_table",
                object_id,
                decision: Decision::Allow,
                trace_id,
                metadata,
            },
            storage::audit::Chain::Yes,
        )
        .await
        .map_err(|e| DataError::Internal(format!("audit: {e}")))
    }

    /// コミット済み操作の事後監査（失敗しても結果を覆さない・ログのみ）。
    pub(crate) async fn record_audit_best_effort(
        &self,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) {
        if let Err(e) = self
            .record_audit(ctx, action, object_id, trace_id, metadata)
            .await
        {
            tracing::error!(error = %e, action, object_id, "監査ログの書込に失敗（操作自体は成功済み）");
        }
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
