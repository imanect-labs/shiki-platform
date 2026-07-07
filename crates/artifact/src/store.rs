//! `ArtifactStore` — アーティファクトの単一チョークポイント（`&AuthContext` 経由・PgPool 内包）。
//!
//! 作成・不変バージョン追記・取得・名前解決・一覧・論理削除。共有は [`share`](crate::share)。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::model::{Artifact, ArtifactKind, ArtifactVersion, VersionMeta};
use crate::{map_db, ArtifactError};

/// 本文 JSON の上限（防御的上限。IR の 1MB 上限（ir.md V7）は保存時検証側が別途課す）。
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// 参照名の上限長。
const MAX_NAME_LEN: usize = 128;

/// 新規アーティファクトの入力。
#[derive(Debug, Clone)]
pub struct NewArtifact {
    pub kind: ArtifactKind,
    pub name: String,
    pub body: serde_json::Value,
}

/// artifact 行。
#[derive(sqlx::FromRow)]
struct ArtifactRow {
    id: Uuid,
    kind: String,
    name: String,
    owner: String,
    current_version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ArtifactRow {
    fn into_artifact(self) -> Result<Artifact, ArtifactError> {
        Ok(Artifact {
            id: self.id,
            kind: ArtifactKind::parse(&self.kind)
                .ok_or_else(|| ArtifactError::Internal(format!("bad kind: {}", self.kind)))?,
            name: self.name,
            owner: self.owner,
            current_version: self.current_version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// アーティファクトのデータチョークポイント。
#[derive(Clone)]
pub struct ArtifactStore {
    pub(crate) db: PgPool,
    pub(crate) authz: Arc<dyn AuthzClient>,
    pub(crate) audit: AuditRecorder,
}

impl ArtifactStore {
    pub fn new(db: PgPool, authz: Arc<dyn AuthzClient>) -> Self {
        let audit = AuditRecorder::new(db.clone());
        ArtifactStore { db, authz, audit }
    }

    /// アーティファクトを作成する（version 1 の本文込み・作成者を owner タプルで付与）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        input: NewArtifact,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        let name = validate_name(&input.name)?;
        validate_body(&input.body)?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        let row: ArtifactRow = sqlx::query_as(
            "INSERT INTO artifact (tenant_id, org, kind, name, owner, current_version) \
             VALUES ($1, $2, $3, $4, $5, 1) \
             RETURNING id, kind, name, owner, current_version, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(input.kind.as_str())
        .bind(name)
        .bind(&ctx.principal.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                ArtifactError::Conflict(format!("name '{name}' は既に存在します"))
            }
            _ => map_db(e),
        })?;
        sqlx::query(
            "INSERT INTO artifact_version (tenant_id, artifact_id, version, body, created_by) \
             VALUES ($1, $2, 1, $3, $4)",
        )
        .bind(&ctx.tenant_id)
        .bind(row.id)
        .bind(Json(&input.body))
        .bind(&ctx.principal.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;

        // 作成者を owner に（FGA）。失敗したら行を補償削除して漏れを残さない。
        let id = row.id;
        let obj = ctx.ns().artifact(&id.to_string());
        if let Err(e) = self
            .authz
            .write_tuple(&ctx.subject(), Relation::Owner, &obj)
            .await
        {
            let _ = sqlx::query("DELETE FROM artifact WHERE tenant_id = $1 AND id = $2")
                .bind(&ctx.tenant_id)
                .bind(id)
                .execute(&self.db)
                .await;
            return Err(ArtifactError::Internal(format!("owner tuple: {e}")));
        }
        self.record_audit(
            ctx,
            "artifact.create",
            &id.to_string(),
            trace_id,
            json!({ "kind": input.kind, "name": name }),
        )
        .await?;
        row.into_artifact()
    }

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
        let row: Option<(Json<serde_json::Value>, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT body, created_by, created_at FROM artifact_version \
             WHERE tenant_id = $1 AND artifact_id = $2 AND version = $3",
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
            "SELECT version, created_by, created_at FROM artifact_version \
             WHERE tenant_id = $1 AND artifact_id = $2 ORDER BY version DESC",
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

    /// 名前でメタデータを解決する（viewer 権限・保存時検証 V4 / ワークフロー起動が使う）。
    pub async fn get_by_name(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM artifact \
             WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(kind.as_str())
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let id = id.ok_or(ArtifactError::NotFound)?;
        self.get(ctx, id, trace_id).await
    }

    /// 自分が owner のアーティファクト一覧（kind 絞り込み・更新日降順・keyset ページング）。
    pub async fn list_mine(
        &self,
        ctx: &AuthContext,
        kind: Option<ArtifactKind>,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> Result<Vec<Artifact>, ArtifactError> {
        let limit = limit.clamp(1, 100);
        let (before_at, before_id) = match before {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let rows: Vec<ArtifactRow> = sqlx::query_as(
            "SELECT id, kind, name, owner, current_version, created_at, updated_at \
             FROM artifact \
             WHERE tenant_id = $1 AND org = $2 AND owner = $3 AND deleted_at IS NULL \
               AND ($4::text IS NULL OR kind = $4) \
               AND ($5::timestamptz IS NULL OR (updated_at, id) < ($5::timestamptz, $6)) \
             ORDER BY updated_at DESC, id DESC LIMIT $7",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(kind.map(ArtifactKind::as_str))
        .bind(before_at)
        .bind(before_id)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        rows.into_iter().map(ArtifactRow::into_artifact).collect()
    }

    /// 論理削除する（owner 権限・バージョン履歴は保持・名前は再利用可能になる）。
    pub async fn delete(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ArtifactError> {
        self.require(ctx, id, Relation::Owner, "artifact.delete", trace_id)
            .await?;
        let updated = sqlx::query(
            "UPDATE artifact SET deleted_at = now(), updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ArtifactError::NotFound);
        }
        self.record_audit(ctx, "artifact.delete", &id.to_string(), trace_id, json!({}))
            .await
    }

    /// 生存行のメタを引く（認可済み前提の内部ヘルパ）。
    pub(crate) async fn fetch_live(
        &self,
        ctx: &AuthContext,
        id: Uuid,
    ) -> Result<Artifact, ArtifactError> {
        let row: Option<ArtifactRow> = sqlx::query_as(
            "SELECT id, kind, name, owner, current_version, created_at, updated_at \
             FROM artifact WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        row.ok_or(ArtifactError::NotFound)?.into_artifact()
    }

    /// アーティファクトへの relation を要求する（不足は監査 deny＋Forbidden・剥奪即時反映）。
    pub(crate) async fn require(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<authz::FgaObject, ArtifactError> {
        let obj = ctx.ns().artifact(&id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                relation,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))?;
        if !ok {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "artifact",
                        object_id: &id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await;
            return Err(ArtifactError::Forbidden);
        }
        Ok(obj)
    }

    pub(crate) async fn record_audit(
        &self,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), ArtifactError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "artifact",
                    object_id,
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(|e| ArtifactError::Internal(format!("audit: {e}")))
    }
}

/// 参照名を検証する（trim 済みを返す）。
fn validate_name(name: &str) -> Result<&str, ArtifactError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ArtifactError::Invalid("name が空です".into()));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(ArtifactError::Invalid(format!(
            "name が長すぎます（最大 {MAX_NAME_LEN} 文字）"
        )));
    }
    Ok(name)
}

/// 本文 JSON のサイズ上限を検証する。
fn validate_body(body: &serde_json::Value) -> Result<(), ArtifactError> {
    // serde_json の直列化長で近似（DB の jsonb サイズとオーダー一致で十分）。
    let size = serde_json::to_vec(body)
        .map_err(|e| ArtifactError::Invalid(format!("body: {e}")))?
        .len();
    if size > MAX_BODY_BYTES {
        return Err(ArtifactError::Invalid(format!(
            "body が大きすぎます（{size} bytes > {MAX_BODY_BYTES} bytes）"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(validate_name("  wf-1  ").is_ok_and(|n| n == "wf-1"));
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(129)).is_err());
    }

    #[test]
    fn body_size_limit() {
        assert!(validate_body(&serde_json::json!({"a": 1})).is_ok());
        let big = serde_json::json!({ "blob": "x".repeat(MAX_BODY_BYTES) });
        assert!(validate_body(&big).is_err());
    }
}
