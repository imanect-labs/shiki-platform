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

use crate::model::{Artifact, ArtifactKind};
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
pub(crate) fn validate_body(body: &serde_json::Value) -> Result<(), ArtifactError> {
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
