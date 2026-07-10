//! レコード CRUD（Task 9.2）＋楽観ロック（Task 9.5）。
//!
//! # 読取経路について（Task 9.3 への布石）
//!
//! 本モジュールの SELECT は現段階ではテーブル ReBAC（viewer/editor）のみで守られる。
//! Task 9.3 で行レベル述語エンジンが入ると、**全読取はクエリ実行チョークポイント
//! （query/executor）へ集約し、row_policy 述語が無条件 AND 合成される**。
//! 生 SQL は本 crate の外に公開しない（design §4.10・PIT-21）。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::types::Json;
use uuid::Uuid;

use crate::model::{DataRecord, FieldType, TableSchema};
use crate::revision::{diff_fields, insert_revision, RevisionInsert};
use crate::store::DataStore;
use crate::validate::validate_record_data;
use crate::{map_db, DataError};

/// data_record 行。
#[derive(sqlx::FromRow)]
pub(crate) struct RecordRow {
    pub(crate) id: Uuid,
    pub(crate) table_id: Uuid,
    pub(crate) data: Json<Value>,
    pub(crate) rev: i64,
    pub(crate) owner: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

impl RecordRow {
    pub(crate) fn into_record(self) -> DataRecord {
        DataRecord {
            id: self.id,
            table_id: self.table_id,
            data: self.data.0,
            rev: self.rev,
            owner: self.owner,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl DataStore {
    /// レコードを作成する（editor・サーバ検証・rev=1・リビジョン同時記録）。
    pub async fn create_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        data: Value,
        trace_id: Option<&str>,
    ) -> Result<DataRecord, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            "data.record.create",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let normalized =
            validate_record_data(ctx, &table.schema, &data, self.resolver.as_ref()).await?;
        self.check_record_refs(ctx, &table.schema, &normalized)
            .await?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        let row: RecordRow = sqlx::query_as(
            "INSERT INTO data_record (tenant_id, table_id, org, data, owner) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING id, table_id, data, rev, owner, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(&ctx.org)
        .bind(Json(Value::Object(normalized.clone())))
        .bind(&ctx.principal.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
        let patch = diff_fields(&Map::new(), &normalized);
        insert_revision(
            &mut tx,
            ctx,
            RevisionInsert {
                table_id,
                record_id: row.id,
                rev: 1,
                change_kind: "create",
                patch: &patch,
                trace_id,
            },
        )
        .await?;
        // 実データ変更の監査は書込と同一 Tx（原子的・コミット後の監査失敗で成功を偽らない）。
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.record.create",
            &row.id.to_string(),
            trace_id,
            serde_json::json!({ "table_id": table_id }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(row.into_record())
    }

    /// レコードを取得する（viewer・行述語つき）。
    ///
    /// 行述語で不可視の行は存在しない行と**同一応答**（404・存在オラクルなし・PIT-21）。
    pub async fn get_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataRecord, DataError> {
        self.require(ctx, table_id, Relation::Viewer, "data.record.get", trace_id)
            .await?;
        // テーブルの生存チェック込み（削除済みテーブルの残存 FGA タプルで読ませない）。
        let table = self.fetch_live(ctx, table_id).await?;
        let row = self
            .select_visible_by_id(ctx, &table, id)
            .await?
            .ok_or(DataError::NotFound)?;
        let mut records = vec![row.into_record()];
        self.resolve_derived_fields(ctx, &table, &mut records).await?;
        Ok(records.remove(0))
    }

    /// レコードを更新する（editor・merge patch・楽観ロック・リビジョン同時記録）。
    ///
    /// `patch` は変更フィールドのみの部分オブジェクト。`null` はフィールド除去
    /// （required は除去不可＝マージ後検証で拒否）。`expected_rev` 不一致は 409。
    pub async fn update_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        id: Uuid,
        patch: Value,
        expected_rev: i64,
        trace_id: Option<&str>,
    ) -> Result<DataRecord, DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            "data.record.update",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let patch_obj = patch.as_object().ok_or_else(|| {
            DataError::Invalid("patch は JSON オブジェクトである必要があります".into())
        })?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 行述語つきロック: 不可視行は NotFound（rev オラクル封じ・PIT-21）。
        let current = self
            .lock_visible_by_id(ctx, &mut tx, &table, id)
            .await?
            .ok_or(DataError::NotFound)?;
        if !self.write_allowed(ctx, &table, id).await? {
            return Err(DataError::Forbidden);
        }
        if current.rev != expected_rev {
            return Err(DataError::Conflict(format!(
                "rev が一致しません（現在 {}・指定 {expected_rev}）",
                current.rev
            )));
        }
        // merge: null は除去・それ以外は上書き。
        let old_map = current.data.0.as_object().cloned().unwrap_or_default();
        let mut merged = old_map.clone();
        for (k, v) in patch_obj {
            if v.is_null() {
                merged.remove(k);
            } else {
                merged.insert(k.clone(), v.clone());
            }
        }
        let normalized = validate_record_data(
            ctx,
            &table.schema,
            &Value::Object(merged),
            self.resolver.as_ref(),
        )
        .await?;
        self.check_record_refs(ctx, &table.schema, &normalized)
            .await?;
        let patches = diff_fields(&old_map, &normalized);
        if patches.is_empty() {
            tx.rollback().await.map_err(map_db)?;
            return Ok(current.into_record());
        }
        let new_rev = current.rev + 1;
        let row: RecordRow = sqlx::query_as(
            "UPDATE data_record SET data = $4, rev = $5, updated_at = now() \
             WHERE tenant_id = $1 AND table_id = $2 AND id = $3 \
             RETURNING id, table_id, data, rev, owner, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(table_id)
        .bind(id)
        .bind(Json(Value::Object(normalized)))
        .bind(new_rev)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_unique)?;
        insert_revision(
            &mut tx,
            ctx,
            RevisionInsert {
                table_id,
                record_id: id,
                rev: new_rev,
                change_kind: "update",
                patch: &patches,
                trace_id,
            },
        )
        .await?;
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.record.update",
            &id.to_string(),
            trace_id,
            serde_json::json!({ "table_id": table_id, "rev": new_rev }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(row.into_record())
    }

    /// レコードを削除する（editor・楽観ロック・削除リビジョン記録）。
    pub async fn delete_record(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        id: Uuid,
        expected_rev: i64,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        self.require(
            ctx,
            table_id,
            Relation::Editor,
            "data.record.delete",
            trace_id,
        )
        .await?;
        // テーブルの生存チェック込み（削除済みテーブルの残存 FGA タプルで消させない）。
        let table = self.fetch_live(ctx, table_id).await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 行述語つきロック: 不可視行は NotFound（rev オラクル封じ・PIT-21）。
        let current = self
            .lock_visible_by_id(ctx, &mut tx, &table, id)
            .await?
            .ok_or(DataError::NotFound)?;
        if !self.write_allowed(ctx, &table, id).await? {
            return Err(DataError::Forbidden);
        }
        if current.rev != expected_rev {
            return Err(DataError::Conflict(format!(
                "rev が一致しません（現在 {}・指定 {expected_rev}）",
                current.rev
            )));
        }
        sqlx::query("DELETE FROM data_record WHERE tenant_id = $1 AND table_id = $2 AND id = $3")
            .bind(&ctx.tenant_id)
            .bind(table_id)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        let old_map = current.data.0.as_object().cloned().unwrap_or_default();
        let patches = diff_fields(&old_map, &Map::new());
        insert_revision(
            &mut tx,
            ctx,
            RevisionInsert {
                table_id,
                record_id: id,
                rev: current.rev + 1,
                change_kind: "delete",
                patch: &patches,
                trace_id,
            },
        )
        .await?;
        self.record_audit_on(
            &mut tx,
            ctx,
            "data.record.delete",
            &id.to_string(),
            trace_id,
            serde_json::json!({ "table_id": table_id }),
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }

    /// record_ref 値の存在検証（参照先テーブル・同一テナント内）。
    ///
    /// 参照先レコードの**行レベル可視性**の検査は Task 9.3（述語伝播・PIT-20）で行う。
    /// ここでは参照整合（存在すること）のみを担保する。
    async fn check_record_refs(
        &self,
        ctx: &AuthContext,
        schema: &TableSchema,
        data: &Map<String, Value>,
    ) -> Result<(), DataError> {
        for f in &schema.fields {
            if f.field_type != FieldType::RecordRef {
                continue;
            }
            let Some(v) = data.get(&f.name) else { continue };
            let Some(s) = v.as_str() else { continue };
            let Ok(ref_id) = Uuid::parse_str(s) else {
                return Err(DataError::Invalid(format!(
                    "'{}' は UUID 文字列である必要があります",
                    f.name
                )));
            };
            let Some(ref_table) = f.ref_table else {
                continue;
            };
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM data_record WHERE tenant_id = $1 AND table_id = $2 AND id = $3)",
            )
            .bind(&ctx.tenant_id)
            .bind(ref_table)
            .bind(ref_id)
            .fetch_one(&self.db)
            .await
            .map_err(map_db)?;
            if !exists {
                return Err(DataError::Invalid(format!(
                    "'{}' の参照先レコードが見つかりません",
                    f.name
                )));
            }
        }
        Ok(())
    }
}

/// unique 制約違反を 409 へ写す。
#[allow(clippy::needless_pass_by_value)]
fn map_unique(e: sqlx::Error) -> DataError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            DataError::Conflict("unique 制約に違反しています".into())
        }
        _ => map_db(e),
    }
}
