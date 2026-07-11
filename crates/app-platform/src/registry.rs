//! 汎用アーティファクト・レジストリ（Task 9.13a・不変 publish）。
//!
//! ミニアプリ（mini_app_code）を内部レジストリへ**不変 publish** する。`artifact_kind` 列を
//! 持つ汎用設計で、skill ストア等へ将来流用できる（新しい配布機構は作らない）。
//! 同一 (tenant, kind, name, version) の再 publish は 409。取り下げは yank（不変性は保つ）。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{map_db, AppPlatformError};

/// レジストリ登録の 1 行。
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct RegistryEntry {
    pub id: Uuid,
    pub artifact_kind: String,
    pub name: String,
    pub version: String,
    pub artifact_id: Uuid,
    pub artifact_version: i64,
    pub manifest_digest: String,
    pub publisher: String,
    pub trust_tier: String,
    pub yanked: bool,
    pub created_at: DateTime<Utc>,
}

/// publish の入力。
#[derive(Debug, Clone)]
pub struct NewRegistryEntry<'a> {
    pub artifact_kind: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub artifact_id: Uuid,
    pub artifact_version: i64,
    pub manifest_digest: &'a str,
    pub trust_tier: &'a str,
    pub signature: Option<&'a [u8]>,
}

/// 汎用レジストリ（Postgres 上・不変 publish）。
#[derive(Clone)]
pub struct Registry {
    db: PgPool,
}

impl Registry {
    pub fn new(db: PgPool) -> Self {
        Registry { db }
    }

    /// レジストリへ不変 publish する（同一 name+version は 409）。
    ///
    /// artifact 本体（不変バージョン）は呼び出し側が既に作成済みで、その id/version と
    /// digest を受け取る。authz は artifact 側の owner タプルで担保される（レジストリ行
    /// 自体はテナントスコープの公開台帳）。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        entry: NewRegistryEntry<'_>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        let row: RegistryEntry = sqlx::query_as(
            "INSERT INTO registry_entry \
             (tenant_id, org, artifact_kind, name, version, artifact_id, artifact_version, \
              manifest_digest, publisher, trust_tier, signature) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             RETURNING id, artifact_kind, name, version, artifact_id, artifact_version, \
                       manifest_digest, publisher, trust_tier, yanked, created_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(entry.artifact_kind)
        .bind(entry.name)
        .bind(entry.version)
        .bind(entry.artifact_id)
        .bind(entry.artifact_version)
        .bind(entry.manifest_digest)
        .bind(&ctx.principal.id)
        .bind(entry.trust_tier)
        .bind(entry.signature)
        .fetch_one(&self.db)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                AppPlatformError::Conflict(format!(
                    "{}@{} は既に publish 済みです（不変）",
                    entry.name, entry.version
                ))
            }
            _ => map_db(e),
        })?;
        Ok(row)
    }

    /// name の最新（未 yank）エントリを引く（インストール時の解決）。
    pub async fn latest(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        name: &str,
    ) -> Result<Option<RegistryEntry>, AppPlatformError> {
        let row: Option<RegistryEntry> = sqlx::query_as(
            "SELECT id, artifact_kind, name, version, artifact_id, artifact_version, \
                    manifest_digest, publisher, trust_tier, yanked, created_at \
             FROM registry_entry \
             WHERE tenant_id = $1 AND artifact_kind = $2 AND name = $3 AND NOT yanked \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&ctx.tenant_id)
        .bind(artifact_kind)
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row)
    }

    /// 特定 version のエントリを引く。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<RegistryEntry>, AppPlatformError> {
        let row: Option<RegistryEntry> = sqlx::query_as(
            "SELECT id, artifact_kind, name, version, artifact_id, artifact_version, \
                    manifest_digest, publisher, trust_tier, yanked, created_at \
             FROM registry_entry \
             WHERE tenant_id = $1 AND artifact_kind = $2 AND name = $3 AND version = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(artifact_kind)
        .bind(name)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row)
    }

    /// エントリの署名（publish 時添付・first-party インストール検証用）。
    pub async fn signature_of(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<Vec<u8>>, AppPlatformError> {
        let row: Option<(Option<Vec<u8>>,)> = sqlx::query_as(
            "SELECT signature FROM registry_entry \
             WHERE tenant_id = $1 AND artifact_kind = $2 AND name = $3 AND version = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(artifact_kind)
        .bind(name)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.and_then(|(s,)| s))
    }

    /// エントリを yank する（新規インストールを止める・行は不変で残す）。
    pub async fn yank(&self, ctx: &AuthContext, id: Uuid) -> Result<(), AppPlatformError> {
        let updated =
            sqlx::query("UPDATE registry_entry SET yanked = true WHERE tenant_id = $1 AND id = $2")
                .bind(&ctx.tenant_id)
                .bind(id)
                .execute(&self.db)
                .await
                .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(AppPlatformError::NotFound);
        }
        Ok(())
    }
}
