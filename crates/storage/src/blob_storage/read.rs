use crate::{content_address::blob_object_key, StorageError};
use authz::TenantId;
use futures::{stream, StreamExt, TryStreamExt};
use sqlx::PgConnection;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::{
    codec::{self, Body, Manifest},
    BlobStorage,
};
use crate::content_address::sha256_hex;

impl BlobStorage {
    pub(super) async fn decode(
        &self,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        size: usize,
        manifest: Manifest,
    ) -> Result<Vec<u8>, StorageError> {
        // permit の保持中に DB 接続・行ロックを取りに行かない。
        // 全取得/展開が完了してから呼出元の DB 更新へ戻る。
        let _io = Self::io_permit().await?;
        self.decode_inner(tenant, org, sha, size, manifest).await
    }

    async fn decode_inner(
        &self,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        size: usize,
        manifest: Manifest,
    ) -> Result<Vec<u8>, StorageError> {
        manifest.validate(size)?;
        let refs: BTreeSet<String> = manifest.refs().into_iter().map(str::to_owned).collect();
        let objects: BTreeMap<String, Vec<u8>> =
            stream::iter(refs.into_iter().map(|hash| async move {
                let key = Self::object_key(tenant, org, &hash)?;
                let bytes = self.store.get_object(&key).await?;
                if bytes.len() > codec::MAX_BYTES + 1 || sha256_hex(&bytes) != hash {
                    return Err(StorageError::Integrity(
                        "物理実体のハッシュ/サイズ不一致".into(),
                    ));
                }
                Ok((hash, bytes))
            }))
            .buffered(8)
            .try_collect()
            .await?;
        let expected_sha = sha.to_owned();
        tokio::task::spawn_blocking(move || {
            let get = |s: &str| {
                objects
                    .get(s)
                    .map(Vec::as_slice)
                    .ok_or_else(|| StorageError::Integrity("実体の参照がありません".into()))
            };
            let bytes = match manifest.body {
                Body::Full { object } => codec::unpack(get(&object)?, size)?,
                Body::Dictionary {
                    object,
                    base,
                    base_size,
                } => {
                    let dictionary = codec::unpack(get(&base)?, base_size)?;
                    codec::dictionary_decode(get(&object)?, &dictionary, size)?
                }
                Body::Chunks { objects } => {
                    let mut out = Vec::with_capacity(size);
                    for chunk in objects {
                        out.extend_from_slice(&codec::unpack(get(&chunk.object)?, chunk.size)?);
                    }
                    out
                }
            };
            if bytes.len() != size || sha256_hex(&bytes) != expected_sha {
                return Err(StorageError::Integrity(
                    "復元内容のハッシュ/サイズ不一致".into(),
                ));
            }
            Ok(bytes)
        })
        .await
        .map_err(|e| StorageError::Integrity(e.to_string()))?
    }

    pub(crate) async fn read_on(
        &self,
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
        sha: &str,
    ) -> Result<Vec<u8>, StorageError> {
        let row = Self::lock_blob(conn, tenant, org, sha, false).await?;
        self.read_row(tenant, org, sha, row).await
    }

    /// 呼出側が node の現在版を FOR UPDATE で固定している場合。GC は現在版の raw を消さない。
    /// 旧 blob と新 blob の二重行ロックを避け、別ファイル間の追記の lock 順序を保つ。
    pub(crate) async fn read_current_on(
        &self,
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
        sha: &str,
    ) -> Result<Vec<u8>, StorageError> {
        let row = sqlx::query_as("SELECT size_bytes, content_type, storage_manifest FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
            .bind(tenant.as_str()).bind(org).bind(sha).fetch_optional(conn).await?.ok_or(StorageError::NotFound)?;
        self.read_row(tenant, org, sha, row).await
    }

    async fn read_row(
        &self,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        row: super::BlobRow,
    ) -> Result<Vec<u8>, StorageError> {
        let size = usize::try_from(row.size_bytes)
            .map_err(|_| StorageError::Integrity("blob サイズが不正".into()))?;
        if let Some(manifest) = row.storage_manifest {
            return self.decode(tenant, org, sha, size, manifest.0).await;
        }
        let _io = Self::io_permit().await?;
        let bytes = self
            .store
            .get_object(&blob_object_key(tenant.as_str(), org, sha))
            .await?;
        let expected = sha.to_owned();
        tokio::task::spawn_blocking(move || {
            if bytes.len() != size || sha256_hex(&bytes) != expected {
                return Err(StorageError::Integrity(
                    "raw のハッシュ/サイズ不一致".into(),
                ));
            }
            Ok(bytes)
        })
        .await
        .map_err(|e| StorageError::Integrity(e.to_string()))?
    }

    pub(crate) async fn materialize_on(
        &self,
        conn: &mut PgConnection,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        ttl: Duration,
    ) -> Result<String, StorageError> {
        let row = Self::lock_blob(conn, tenant, org, sha, true).await?;
        let key = blob_object_key(tenant.as_str(), org, sha);
        let mut recreated = false;
        if let Some(manifest) = row.storage_manifest {
            if !self.store.exists(&key).await? {
                recreated = true;
                let size = usize::try_from(row.size_bytes)
                    .map_err(|_| StorageError::Integrity("blob サイズが不正".into()))?;
                // DB行は固定済み。PUTがバッファを解放するまで同じpermitで保護する。
                let _io = Self::io_permit().await?;
                let bytes = self
                    .decode_inner(tenant, org, sha, size, manifest.0)
                    .await?;
                self.store
                    .put_object(&key, bytes, &row.content_type)
                    .await?;
            }
        }
        sqlx::query(
            "UPDATE blob SET raw_available = true, raw_expires_at = \
            GREATEST(raw_expires_at, clock_timestamp() + $4 * interval '1 second') \
            WHERE tenant_id = $1 AND org = $2 AND sha256 = $3",
        )
        .bind(tenant.as_str())
        .bind(org)
        .bind(sha)
        .bind(ttl.as_secs_f64() + 60.0)
        .execute(&mut *conn)
        .await?;
        if recreated {
            sqlx::query("UPDATE blob_storage_scope SET next_check_at = LEAST(next_check_at, clock_timestamp() + $3 * interval '1 second') WHERE tenant_id = $1 AND org = $2")
                .bind(tenant.as_str()).bind(org).bind(ttl.as_secs_f64() + 60.0).execute(conn).await?;
        }
        Ok(key)
    }

    pub(crate) async fn presign(
        &self,
        tenant: &TenantId,
        org: &str,
        sha: &str,
        spec: super::DownloadSpec<'_>,
    ) -> Result<String, StorageError> {
        let mut tx = self.maintenance_guard(tenant).await?;
        let key = self
            .materialize_on(&mut tx, tenant, org, sha, spec.ttl)
            .await?;
        let url = if spec.internal {
            self.store.presign_get_internal(&key, spec.ttl).await?
        } else {
            self.store
                .presign_get(&key, spec.ttl, spec.filename, spec.content_type)
                .await?
        };
        // 署名が遅れても URL 発行後の TTL を確保する。GC は commit まで行ロックで待つ。
        sqlx::query("UPDATE blob SET raw_expires_at = GREATEST(raw_expires_at, \
            clock_timestamp() + $4 * interval '1 second') WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
            .bind(tenant.as_str()).bind(org).bind(sha).bind(spec.ttl.as_secs_f64() + 60.0).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(url)
    }
}
