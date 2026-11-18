use crate::StorageError;
use authz::TenantId;
use futures::{stream, StreamExt, TryStreamExt};
use std::collections::{BTreeSet, HashSet};
use std::time::Duration;
use uuid::Uuid;

use super::{
    codec::{self, Body, Manifest, Plan},
    BlobStorage, VersionStorageOptions,
};
use crate::content_address::sha256_hex;

#[derive(sqlx::FromRow)]
struct Candidate {
    blob_sha256: String,
    version: i64,
    eligible: bool,
}

/// 次のバッチへ渡す版番号。論理版を16件ずつ走査し、処理済みの長い接頭部を読み直さない。
pub struct CompactionBatch {
    pub checked: usize,
    pub optimized: usize,
    pub after_version: i64,
}

impl BlobStorage {
    /// 1ジョブの処理量は有界。古い版から処理して独立した基準版を先に作る。
    pub async fn compact_node(
        &self,
        tenant: &TenantId,
        org: &str,
        node: Uuid,
        after_version: i64,
        options: VersionStorageOptions,
    ) -> Result<CompactionBatch, StorageError> {
        options.validate()?;
        if self.pool.options().get_max_connections() < 3 {
            return Err(StorageError::Invalid(
                "旧版最適化にはDB接続数3以上が必要です".into(),
            ));
        }
        let _memory = super::PLAN_LIMIT
            .acquire()
            .await
            .map_err(|e| StorageError::Integrity(e.to_string()))?;
        let _lifecycle = self.maintenance_guard(tenant).await?;
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        let candidates: Vec<Candidate> = sqlx::query_as(
            "WITH version_window AS MATERIALIZED ( \
               SELECT blob_sha256, version, is_proposal FROM node_version \
               WHERE tenant_id = $1 AND org = $2 AND node_id = $3 AND version > $5 \
               ORDER BY version LIMIT 16) \
             SELECT v.blob_sha256, v.version, (NOT v.is_proposal \
               AND b.storage_manifest IS NULL AND b.size_bytes <= $4 \
               AND NOT EXISTS (SELECT 1 FROM node n WHERE n.tenant_id = $1 AND n.org = $2 AND n.blob_sha256 = b.sha256)) AS eligible \
             FROM version_window v \
             JOIN blob b ON b.tenant_id = $1 AND b.org = $2 AND b.sha256 = v.blob_sha256 \
             ORDER BY v.version")
            .bind(tenant.as_str()).bind(org).bind(node).bind(options.max_optimize_bytes.min(codec::MAX_BYTES) as i64)
            .bind(after_version)
            .fetch_all(&mut *tx).await?;
        tx.commit().await?;
        let mut batch = CompactionBatch {
            checked: candidates.len(),
            optimized: 0,
            after_version: candidates.last().map_or(after_version, |c| c.version),
        };
        let mut seen = HashSet::new();
        for c in candidates {
            if c.eligible && seen.insert(c.blob_sha256.clone()) {
                self.compact_one(tenant, org, node, c, options).await?;
                batch.optimized += 1;
            }
        }
        Ok(batch)
    }

    async fn compact_one(
        &self,
        tenant: &TenantId,
        org: &str,
        node: Uuid,
        candidate: Candidate,
        options: VersionStorageOptions,
    ) -> Result<(), StorageError> {
        let sha = &candidate.blob_sha256;
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        let bytes = self.read_on(&mut tx, tenant, org, sha).await?;
        // 基準は独立 Full のみ。版の距離を制限し、古い基準への固定も避ける。
        let anchor: Option<(sqlx::types::Json<Manifest>, i64)> = sqlx::query_as(
            "SELECT b.storage_manifest, b.size_bytes FROM node_version v \
             JOIN blob b ON b.tenant_id = v.tenant_id AND b.org = v.org AND b.sha256 = v.blob_sha256 \
             WHERE v.tenant_id = $1 AND v.org = $2 AND v.node_id = $3 AND v.version < $4 \
               AND v.version >= $4 - 31 AND NOT v.is_proposal \
               AND b.size_bytes <= $5 AND b.storage_manifest->'body'->>'codec' = 'full' ORDER BY v.version DESC LIMIT 1")
            .bind(tenant.as_str()).bind(org).bind(node).bind(candidate.version).bind(options.max_optimize_bytes as i64).fetch_optional(&mut *tx).await?;
        let next: Option<String> = sqlx::query_scalar(
            "SELECT blob_sha256 FROM node_version WHERE tenant_id = $1 AND org = $2 AND node_id = $3 \
             AND version > $4 AND size_bytes <= $5 AND NOT is_proposal ORDER BY version LIMIT 1")
            .bind(tenant.as_str()).bind(org).bind(node).bind(candidate.version).bind(options.max_optimize_bytes as i64)
            .fetch_optional(&mut *tx).await?;
        // 同じ txn で複数 blob をロックしない（復元との lock 順序の循環を避ける）。
        tx.commit().await?;
        let next_bytes = if let Some(next) = next {
            let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
            let value = self.read_on(&mut tx, tenant, org, &next).await?;
            tx.commit().await?;
            Some(value)
        } else {
            None
        };
        let anchor = if let Some((m, size)) = anchor {
            let size = usize::try_from(size)
                .map_err(|_| StorageError::Integrity("基準版サイズが不正".into()))?;
            m.validate(size)?;
            if let Body::Full { object } = m.0.body {
                let packed = self
                    .store
                    .get_object(&Self::object_key(tenant, org, &object)?)
                    .await?;
                if sha256_hex(&packed) != object {
                    return Err(StorageError::Integrity("基準版のハッシュ不一致".into()));
                }
                Some((object, codec::unpack(&packed, size)?))
            } else {
                None
            }
        } else {
            None
        };
        let size = bytes.len();
        let (plans, hints) = tokio::task::spawn_blocking(move || {
            let plans = codec::candidates(&bytes, anchor)?;
            // 既に存在する次版との共有を見て初期断片を用意する。未来の編集を仮定しない。
            let hints: HashSet<String> = if let Some(next) = next_bytes {
                codec::candidates(&next, None)?
                    .into_iter()
                    .filter(|p| matches!(p.manifest.body, Body::Chunks { .. }))
                    .flat_map(|p| p.objects.into_keys())
                    .collect()
            } else {
                HashSet::new()
            };
            Ok::<_, StorageError>((plans, hints))
        })
        .await
        .map_err(|e| StorageError::Integrity(e.to_string()))??;
        let refs: BTreeSet<String> = plans
            .iter()
            .flat_map(|p| p.objects.keys().cloned())
            .collect();
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        let known: HashSet<String> = sqlx::query_scalar::<_, String>(
            "SELECT sha256 FROM blob_storage_object WHERE tenant_id = $1 AND org = $2 AND sha256 = ANY($3) AND collected_at IS NULL")
            .bind(tenant.as_str()).bind(org).bind(refs.into_iter().collect::<Vec<_>>()).fetch_all(&mut *tx).await?.into_iter().collect();
        tx.commit().await?;
        let plan = choose(plans, &known, &hints, options.read_cost_bytes)?;
        self.register_and_write(tenant, org, &plan).await?;
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        let row = Self::lock_blob(&mut tx, tenant, org, sha, true).await?;
        if row.storage_manifest.is_none() {
            let refs: BTreeSet<String> = plan
                .manifest
                .refs()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let refs: Vec<String> = refs.into_iter().collect();
            let locked: Vec<String> = sqlx::query_scalar(
                "SELECT sha256 FROM blob_storage_object
                WHERE tenant_id = $1 AND org = $2 AND sha256 = ANY($3) ORDER BY sha256 FOR UPDATE",
            )
            .bind(tenant.as_str())
            .bind(org)
            .bind(&refs)
            .fetch_all(&mut *tx)
            .await?;
            if locked.len() != refs.len() {
                return Err(StorageError::Integrity(
                    "公開実体の台帳が不足しています".into(),
                ));
            }
            // GC と排他した状態で実体を読み戻してから公開する。
            // 保護期限を超えて PUT が遅延しても、検証〜公開間に実体を消されない。
            self.decode(tenant, org, sha, size, plan.manifest.clone())
                .await?;
            sqlx::query(
                "INSERT INTO blob_storage_ref (tenant_id, org, blob_sha256, object_sha256) \
                SELECT $1, $2, $3, unnest($4::text[]) ON CONFLICT DO NOTHING",
            )
            .bind(tenant.as_str())
            .bind(org)
            .bind(sha)
            .bind(&refs)
            .execute(&mut *tx)
            .await?;
            let reads = plan
                .manifest
                .refs()
                .into_iter()
                .collect::<BTreeSet<_>>()
                .len() as i32;
            sqlx::query("UPDATE blob SET storage_manifest = $4, storage_bytes = $5, storage_reads = $7, storage_checked_at = clock_timestamp(), \
                raw_expires_at = GREATEST(raw_expires_at, clock_timestamp() + $6 * interval '1 second') \
                WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
                .bind(tenant.as_str()).bind(org).bind(sha).bind(sqlx::types::Json(plan.manifest))
                .bind(plan.stored_bytes as i64).bind(Duration::from_secs(options.raw_grace_secs).as_secs_f64()).bind(reads).execute(&mut *tx).await?;
            sqlx::query("UPDATE blob_storage_scope SET next_check_at = LEAST(next_check_at, clock_timestamp()) WHERE tenant_id = $1 AND org = $2")
                .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn register_and_write(
        &self,
        tenant: &TenantId,
        org: &str,
        plan: &Plan,
    ) -> Result<(), StorageError> {
        let mut tx = crate::db_tenant::begin_tenant_tx(&self.pool, tenant).await?;
        let hashes: Vec<String> = plan.objects.keys().cloned().collect();
        sqlx::query("INSERT INTO blob_storage_scope (tenant_id, org) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?;
        let sizes: Vec<i64> = plan.objects.values().map(|b| b.len() as i64).collect();
        sqlx::query("INSERT INTO blob_storage_object (tenant_id, org, sha256, size_bytes, protected_until) \
            SELECT $1, $2, p.sha, p.size, clock_timestamp() + interval '1 day' \
            FROM unnest($3::text[], $4::bigint[]) AS p(sha, size) \
            ON CONFLICT (tenant_id, org, sha256) DO UPDATE SET collected_at = NULL, protected_until = \
            GREATEST(blob_storage_object.protected_until, EXCLUDED.protected_until)")
            .bind(tenant.as_str()).bind(org).bind(hashes).bind(sizes).execute(&mut *tx).await?;
        sqlx::query("UPDATE blob_storage_scope SET next_check_at = LEAST(next_check_at, now() + interval '1 day') WHERE tenant_id = $1 AND org = $2")
            .bind(tenant.as_str()).bind(org).execute(&mut *tx).await?;
        tx.commit().await?;
        let writes: Vec<(String, Vec<u8>)> = plan
            .objects
            .iter()
            .map(|(sha, bytes)| Ok((Self::object_key(tenant, org, sha)?, bytes.clone())))
            .collect::<Result<_, StorageError>>()?;
        let store = self.store.clone();
        stream::iter(writes)
            .map(move |(key, bytes)| {
                let store = store.clone();
                async move {
                    if !store.exists(&key).await? {
                        store
                            .put_object(&key, bytes, "application/octet-stream")
                            .await?;
                    }
                    Ok::<_, StorageError>(())
                }
            })
            .buffer_unordered(8)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(())
    }
}

fn choose(
    plans: Vec<Plan>,
    known: &HashSet<String>,
    hints: &HashSet<String>,
    read_cost_bytes: usize,
) -> Result<Plan, StorageError> {
    let mut best: Option<(usize, Plan)> = None;
    for plan in plans {
        let mut cost = serde_json::to_vec(&plan.manifest)
            .map_err(|e| StorageError::Integrity(e.to_string()))?
            .len();
        let reads = plan
            .manifest
            .refs()
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len();
        cost = cost.saturating_add(reads.saturating_mul(read_cost_bytes));
        for (sha, bytes) in &plan.objects {
            if !known.contains(sha) {
                let size = bytes.len() + 64;
                cost += if hints.contains(sha) {
                    size.div_ceil(2)
                } else {
                    size
                };
            }
        }
        if best.as_ref().is_none_or(|(old, _)| cost < *old) {
            best = Some((cost, plan));
        }
    }
    best.map(|(_, p)| p)
        .ok_or_else(|| StorageError::Integrity("保存表現候補がありません".into()))
}
