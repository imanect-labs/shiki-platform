//! 内容ハッシュは論理的な識別子。保存表現と、署名付き GET 用 raw キャッシュは別に管理する。
//! 公開ユーザー操作は StorageService が認可した後にだけ本モジュールへ渡す。
use std::{sync::Arc, time::Duration};

use authz::TenantId;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};

use crate::{content_address::is_valid_sha256_hex, ObjectStore, StorageError};

mod backfill;
mod codec;
mod gc;
mod namespace;
mod optimize;
mod read;
mod worker;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod concurrency_tests;

pub use namespace::NamespaceMove;
pub use optimize::CompactionBatch;
pub use worker::spawn_version_storage_worker;

pub(crate) const QUEUE: &str = "storage_version_optimize";
static IO_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static PLAN_LIMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

pub(crate) struct DownloadSpec<'a> {
    pub ttl: Duration,
    pub filename: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub internal: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_reject_unbounded_memory_and_invalid_lease() {
        let valid = VersionStorageOptions::default();
        assert!(valid.validate().is_ok());
        for option in [
            VersionStorageOptions {
                raw_grace_secs: 0,
                ..valid
            },
            VersionStorageOptions {
                raw_grace_secs: 31_536_001,
                ..valid
            },
            VersionStorageOptions {
                max_optimize_bytes: 0,
                ..valid
            },
            VersionStorageOptions {
                max_optimize_bytes: codec::MAX_BYTES + 1,
                ..valid
            },
            VersionStorageOptions {
                read_cost_bytes: 1024 * 1024 + 1,
                ..valid
            },
        ] {
            assert!(option.validate().is_err());
        }
    }

    #[test]
    fn physical_keys_are_scoped_and_reject_path_input() {
        let a = TenantId::parse("tenant-a").unwrap();
        let b = TenantId::parse("tenant-b").unwrap();
        let sha = "a".repeat(64);
        assert_ne!(
            BlobStorage::object_key(&a, "org", &sha).unwrap(),
            BlobStorage::object_key(&b, "org", &sha).unwrap()
        );
        assert_ne!(
            BlobStorage::object_key(&a, "org", &sha).unwrap(),
            BlobStorage::object_key(&a, "other", &sha).unwrap()
        );
        assert!(BlobStorage::object_key(&a, "org", "../other-tenant").is_err());
    }
}

/// 既定で有効。旧 reader と混在するローリング更新中は明示的に無効化する。
/// 無効でも保存済み表現は常に読める。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct VersionStorageOptions {
    pub enabled: bool,
    /// 発行済み URL と旧 reader の猶予。実際の GET TTL より短くはしない。
    pub raw_grace_secs: u64,
    /// 旧版最適化のメモリ予算。超えるファイルは raw を保持する。
    pub max_optimize_bytes: usize,
    /// 復元時の1実体読出しを何byteの保存コストとして評価するか。0は容量優先。
    pub read_cost_bytes: usize,
}

impl Default for VersionStorageOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            raw_grace_secs: 86400,
            max_optimize_bytes: codec::MAX_BYTES,
            read_cost_bytes: 0,
        }
    }
}

impl VersionStorageOptions {
    pub fn validate(self) -> Result<(), StorageError> {
        if !(60..=31_536_000).contains(&self.raw_grace_secs) {
            return Err(StorageError::Invalid(
                "version_storage.raw_grace_secs は60秒〜365日の範囲が必要です".into(),
            ));
        }
        if self.max_optimize_bytes == 0 || self.max_optimize_bytes > codec::MAX_BYTES {
            return Err(StorageError::Invalid(
                "version_storage.max_optimize_bytes は1バイト〜64 MiBの範囲が必要です".into(),
            ));
        }
        if self.read_cost_bytes > 1024 * 1024 {
            return Err(StorageError::Invalid(
                "version_storage.read_cost_bytes は0〜1 MiBの範囲が必要です".into(),
            ));
        }
        Ok(())
    }
}

/// システム保守用 capability（ユーザー API に直接公開しない）。全操作は検証済み tenant に閉じる。
#[derive(Clone)]
pub struct BlobStorage {
    pool: PgPool,
    store: Arc<dyn ObjectStore>,
}

#[derive(sqlx::FromRow)]
struct BlobRow {
    size_bytes: i64,
    content_type: String,
    storage_manifest: Option<sqlx::types::Json<codec::Manifest>>,
}

impl BlobStorage {
    pub fn new(pool: PgPool, store: Arc<dyn ObjectStore>) -> Self {
        Self { pool, store }
    }

    pub(crate) async fn io_permit() -> Result<tokio::sync::SemaphorePermit<'static>, StorageError> {
        IO_LIMIT
            .acquire()
            .await
            .map_err(|e| StorageError::Integrity(e.to_string()))
    }

    pub(crate) async fn protect_lifecycle_on(
        conn: &mut PgConnection,
        tenant: &TenantId,
    ) -> Result<(), StorageError> {
        // purge 待ちで pool を占有しない。node 行を固定済みの書込でも lock 順序を逆転させない。
        let acquired: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock_shared(hashtextextended($1, 118488))",
        )
        .bind(tenant.as_str())
        .fetch_one(&mut *conn)
        .await?;
        if !acquired {
            return Err(StorageError::NotFound);
        }
        let active: bool = sqlx::query_scalar("SELECT NOT EXISTS (SELECT 1 FROM tenant WHERE tenant_id = $1 AND status IN ('deleting', 'deleted'))")
            .bind(tenant.as_str()).fetch_one(conn).await?;
        if !active {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    /// バックアップ/旧 reader への切戻し用に、元バイトのオブジェクトを用意する。
    /// システム保守専用。バックアップでは先に hold_backup を commit しておく。
    pub async fn materialize_for_export(
        &self,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        ttl: Duration,
    ) -> Result<String, StorageError> {
        if ttl.is_zero() {
            return Err(StorageError::Invalid("export lease が空です".into()));
        }
        let mut tx = self.maintenance_guard(tenant).await?;
        let key = self.materialize_on(&mut tx, tenant, org, sha, ttl).await?;
        tx.commit().await?;
        Ok(key)
    }

    /// purge と競合するバックグラウンド書込を、テナント単位の共有ロックで保護する。
    async fn maintenance_guard(
        &self,
        tenant: &TenantId,
    ) -> Result<crate::db_tenant::TenantTx, StorageError> {
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        Self::protect_lifecycle_on(&mut tx, tenant).await?;
        Ok(tx)
    }

    fn object_key(tenant: &TenantId, org: &str, sha: &str) -> Result<String, StorageError> {
        if !is_valid_sha256_hex(sha) {
            return Err(StorageError::Integrity("物理ハッシュが不正です".into()));
        }
        Ok(format!("{}/{org}/storage-v1/{sha}", tenant.as_str()))
    }

    async fn lock_blob(
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        exclusive: bool,
    ) -> Result<BlobRow, StorageError> {
        if !is_valid_sha256_hex(sha) {
            return Err(StorageError::NotFound);
        }
        let lock = if exclusive { "UPDATE" } else { "SHARE" };
        let sql = format!(
            "SELECT size_bytes, content_type, storage_manifest FROM blob \
            WHERE tenant_id = $1 AND org = $2 AND sha256 = $3 FOR {lock}"
        );
        sqlx::query_as(&sql)
            .bind(tenant.as_str())
            .bind(org)
            .bind(sha)
            .fetch_optional(conn)
            .await?
            .ok_or(StorageError::NotFound)
    }

    pub(crate) async fn enqueue(
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
        node_id: uuid::Uuid,
    ) -> Result<(), StorageError> {
        Self::enqueue_after(conn, tenant, org, node_id, 0).await
    }

    pub(crate) async fn enqueue_after(
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
        node_id: uuid::Uuid,
        after_version: i64,
    ) -> Result<(), StorageError> {
        Self::protect_lifecycle_on(conn, tenant).await?;
        let payload =
            serde_json::json!({ "org": org, "node_id": node_id, "after_version": after_version });
        // queue はリテラルにし、このキューだけの有界なJSONに張った部分indexを使う。
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM job_queue WHERE queue = 'storage_version_optimize' \
            AND tenant_id = $1 AND storage_coalesce_key = md5(($2::jsonb - 'after_version')::text) \
            AND (payload - 'after_version') = ($2::jsonb - 'after_version') \
            AND attempts = 0 LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .bind(tenant.as_str())
        .bind(&payload)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(id) = pending {
            sqlx::query(
                "UPDATE job_queue SET payload = jsonb_set(payload, '{after_version}', \
                to_jsonb(LEAST(COALESCE((payload->>'after_version')::bigint, 0), $3))) \
                WHERE tenant_id = $1 AND id = $2",
            )
            .bind(tenant.as_str())
            .bind(id)
            .bind(after_version)
            .execute(&mut *conn)
            .await?;
            return Ok(());
        }
        jobq::enqueue_on(
            conn,
            jobq::NewJob {
                queue: QUEUE,
                tenant_id: tenant.as_str(),
                payload: &payload,
                trace_id: None,
                max_attempts: 12,
            },
        )
        .await
        .map_err(|e| StorageError::Integrity(e.to_string()))?;
        Ok(())
    }

    /// バックアップ開始前に取得する。TTL 内に完了させるか同じ id で延長する。
    /// この lease の commit 後に DB/オブジェクトの整合点を撮る。
    pub async fn hold_backup(
        &self,
        tenant: &TenantId,
        org: &str,
        id: uuid::Uuid,
        ttl: Duration,
    ) -> Result<(), StorageError> {
        if ttl.is_zero() {
            return Err(StorageError::Invalid("バックアップ lease が空です".into()));
        }
        let mut tx = self.maintenance_guard(tenant).await?;
        Self::lock_gc(&mut tx, tenant, org).await?;
        sqlx::query(
            "INSERT INTO blob_storage_hold (tenant_id, org, id, expires_at) \
            VALUES ($1, $2, $3, clock_timestamp() + $4 * interval '1 second') \
            ON CONFLICT (tenant_id, org, id) DO UPDATE SET expires_at = \
            GREATEST(blob_storage_hold.expires_at, EXCLUDED.expires_at)",
        )
        .bind(tenant.as_str())
        .bind(org)
        .bind(id)
        .bind(ttl.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn release_backup(
        &self,
        tenant: &TenantId,
        org: &str,
        id: uuid::Uuid,
    ) -> Result<(), StorageError> {
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        sqlx::query("DELETE FROM blob_storage_hold WHERE tenant_id = $1 AND org = $2 AND id = $3")
            .bind(tenant.as_str())
            .bind(org)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn lock_gc(
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
    ) -> Result<(), StorageError> {
        // JSON 配列で区切りを符号化し、tenant/org の文字列連結による曖昧さを避ける。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(json_build_array($1::text, $2::text)::text, 488))")
            .bind(tenant.as_str()).bind(org).execute(conn).await?;
        Ok(())
    }
}
