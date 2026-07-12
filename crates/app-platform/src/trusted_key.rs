//! 信頼鍵台帳（Task 9.13b・`app_trusted_key`）。
//!
//! first-party publish／オフライン import の署名検証に使う ed25519 公開鍵。
//! 登録/失効は /admin 面（provisioner Bearer・api 層でゲート）のみ。失効は行を残す
//! （どの鍵で何が入ったかの監査可能性）。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{map_db, AppPlatformError};

/// 信頼鍵 1 件（public_key は API 応答では hex）。
#[derive(Debug, Clone, Serialize)]
pub struct TrustedKey {
    pub id: Uuid,
    pub key_id: String,
    /// ed25519 公開鍵（hex 64 文字）。
    pub public_key_hex: String,
    pub note: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    key_id: String,
    public_key: Vec<u8>,
    note: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl From<Row> for TrustedKey {
    fn from(r: Row) -> Self {
        TrustedKey {
            id: r.id,
            key_id: r.key_id,
            public_key_hex: hex::encode(r.public_key),
            note: r.note,
            created_by: r.created_by,
            created_at: r.created_at,
            revoked_at: r.revoked_at,
        }
    }
}

/// `app_trusted_key` への単一チョークポイント。
#[derive(Clone)]
pub struct TrustedKeyStore {
    db: PgPool,
}

impl TrustedKeyStore {
    pub fn new(db: PgPool) -> Self {
        TrustedKeyStore { db }
    }

    /// 鍵を登録する（key_id はテナント内一意・公開鍵は 32 バイト raw）。
    pub async fn add(
        &self,
        ctx: &AuthContext,
        key_id: &str,
        public_key: &[u8],
        note: Option<&str>,
    ) -> Result<TrustedKey, AppPlatformError> {
        let key_id = key_id.trim();
        if key_id.is_empty() || key_id.len() > 100 {
            return Err(AppPlatformError::Invalid(
                "key_id は 1〜100 文字で指定してください".into(),
            ));
        }
        if public_key.len() != 32 {
            return Err(AppPlatformError::Invalid(
                "public_key は ed25519 の 32 バイト（hex 64 文字）です".into(),
            ));
        }
        let row: Row = sqlx::query_as(
            "INSERT INTO app_trusted_key (tenant_id, org, key_id, public_key, note, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, key_id, public_key, note, created_by, created_at, revoked_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(key_id)
        .bind(public_key)
        .bind(note)
        .bind(&ctx.principal.id)
        .fetch_one(&self.db)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                AppPlatformError::Conflict(format!("key_id '{key_id}' は既に存在します"))
            }
            _ => map_db(e),
        })?;
        Ok(row.into())
    }

    /// 有効な鍵を key_id で引く（失効済みは対象外・fail-closed）。
    pub async fn find_active(
        &self,
        ctx: &AuthContext,
        key_id: &str,
    ) -> Result<Option<Vec<u8>>, AppPlatformError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(
            "SELECT public_key FROM app_trusted_key \
             WHERE tenant_id = $1 AND key_id = $2 AND revoked_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(key_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.map(|(k,)| k))
    }

    /// 有効な鍵の一覧（first-party インストール検証は全鍵と突合する）。
    pub async fn list_active(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<TrustedKey>, AppPlatformError> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, key_id, public_key, note, created_by, created_at, revoked_at \
             FROM app_trusted_key \
             WHERE tenant_id = $1 AND revoked_at IS NULL ORDER BY created_at",
        )
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// 有効な鍵の生バイト一覧（検証ループ用）。
    pub async fn active_key_bytes(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<Vec<u8>>, AppPlatformError> {
        let rows: Vec<(Vec<u8>,)> = sqlx::query_as(
            "SELECT public_key FROM app_trusted_key \
             WHERE tenant_id = $1 AND revoked_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(|(k,)| k).collect())
    }

    /// 鍵を失効させる（行は残す）。
    pub async fn revoke(&self, ctx: &AuthContext, key_id: &str) -> Result<(), AppPlatformError> {
        let updated = sqlx::query(
            "UPDATE app_trusted_key SET revoked_at = now() \
             WHERE tenant_id = $1 AND key_id = $2 AND revoked_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(key_id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(AppPlatformError::NotFound);
        }
        Ok(())
    }
}
