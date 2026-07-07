//! アーティファクトのバージョン操作（不変追記・楽観ロック・履歴・版取得）。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use uuid::Uuid;

use crate::model::{Artifact, ArtifactVersion, VersionMeta};
use crate::store::{validate_body, ArtifactStore};
use crate::{map_db, ArtifactError};

impl ArtifactStore {
    /// 新バージョンを追記する（editor 権限・不変追記・楽観ロック）。
    ///
    /// `expected_version` を渡すと現行バージョンと一致する場合のみ追記する
    /// （dnd/AI 編集の lost-update 防止）。`None` は無条件追記。
    pub async fn append_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        body: serde_json::Value,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        validate_body(&body)?;
        self.require(
            ctx,
            id,
            Relation::Editor,
            "artifact.version.append",
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        // current_version の加算が単一の正（同時追記は行ロックで直列化される）。
        let new_version: Option<i64> = sqlx::query_scalar(
            "UPDATE artifact SET current_version = current_version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL \
               AND ($3::bigint IS NULL OR current_version = $3) \
             RETURNING current_version",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(expected_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some(new_version) = new_version else {
            tx.rollback().await.map_err(map_db)?;
            // 行が無い（削除済み）か expected_version 不一致かを区別して返す。
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM artifact \
                 WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL)",
            )
            .bind(&ctx.tenant_id)
            .bind(id)
            .fetch_one(&self.db)
            .await
            .map_err(map_db)?;
            return Err(if exists {
                ArtifactError::Conflict("バージョンが競合しました（再取得して下さい）".into())
            } else {
                ArtifactError::NotFound
            });
        };
        let created_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO artifact_version (tenant_id, artifact_id, version, body, created_by) \
             VALUES ($1, $2, $3, $4, $5) RETURNING created_at",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(new_version)
        .bind(Json(&body))
        .bind(&ctx.principal.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;

        self.record_audit(
            ctx,
            "artifact.version.append",
            &id.to_string(),
            trace_id,
            json!({ "version": new_version }),
        )
        .await?;
        Ok(ArtifactVersion {
            artifact_id: id,
            version: new_version,
            body,
            created_by: ctx.principal.id.clone(),
            created_at,
        })
    }

    /// メタデータを取得する（viewer 権限）。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        self.require(ctx, id, Relation::Viewer, "artifact.get", trace_id)
            .await?;
        self.fetch_live(ctx, id).await
    }

    /// 指定バージョンの本文を取得する（viewer 権限・過去バージョンも不変で取得できる）。
    pub async fn get_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        self.require(ctx, id, Relation::Viewer, "artifact.version.get", trace_id)
            .await?;
        // 論理削除済みアーティファクトのバージョン本文は読めない（tuple 残存でも body を返さない）。
        let row: Option<(Json<serde_json::Value>, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT v.body, v.created_by, v.created_at FROM artifact_version v \
             WHERE v.tenant_id = $1 AND v.artifact_id = $2 AND v.version = $3 \
               AND EXISTS (SELECT 1 FROM artifact a \
                           WHERE a.tenant_id = $1 AND a.id = $2 AND a.deleted_at IS NULL)",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (body, created_by, created_at) = row.ok_or(ArtifactError::NotFound)?;
        Ok(ArtifactVersion {
            artifact_id: id,
            version,
            body: body.0,
            created_by,
            created_at,
        })
    }

    /// バージョン履歴（メタのみ・新しい順）。viewer 権限。
    pub async fn list_versions(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<VersionMeta>, ArtifactError> {
        self.require(
            ctx,
            id,
            Relation::Viewer,
            "artifact.versions.list",
            trace_id,
        )
        .await?;
        let rows: Vec<(i64, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT v.version, v.created_by, v.created_at FROM artifact_version v \
             WHERE v.tenant_id = $1 AND v.artifact_id = $2 \
               AND EXISTS (SELECT 1 FROM artifact a \
                           WHERE a.tenant_id = $1 AND a.id = $2 AND a.deleted_at IS NULL) \
             ORDER BY v.version DESC",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(version, created_by, created_at)| VersionMeta {
                version,
                created_by,
                created_at,
            })
            .collect())
    }
}
