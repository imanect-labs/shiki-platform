//! 版の物理表現: 実 Postgres・OpenFGA・RustFS で読取/復元/GC の競合も検証する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines
)]

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Principal, Relation, TenantId,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{sync::Arc, time::Duration};
use storage::{
    blob_storage::{BlobStorage, VersionStorageOptions},
    content_address::{blob_object_key, sha256_hex},
    IndexerStorage, ObjectStore, S3Config, S3ObjectStore, StorageError, StorageService,
};
use uuid::Uuid;

struct Env {
    pool: PgPool,
    service: StorageService,
    archive: BlobStorage,
    store: Arc<dyn ObjectStore>,
    ctx: AuthContext,
    options: VersionStorageOptions,
}

async fn setup() -> Option<Env> {
    let (Ok(db), Ok(fga)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let authz: Arc<dyn AuthzClient> = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url: fga,
                store_name: format!("version-storage-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .unwrap(),
    );
    let endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&S3Config {
        internal_endpoint: endpoint.clone(),
        public_endpoint: endpoint,
        bucket: "shiki-version-storage-it".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    }));
    store.ensure_bucket().await.unwrap();
    let org = format!("vs-{}", Uuid::new_v4().simple());
    let ctx = AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.clone(),
        TenantId::parse(org).unwrap(),
    );
    authz
        .write_tuple(
            &ctx.subject(),
            Relation::Member,
            &ctx.ns().organization(ctx.org()),
        )
        .await
        .unwrap();
    let options = VersionStorageOptions {
        enabled: true,
        ..VersionStorageOptions::default()
    };
    let service = StorageService::new(
        pool.clone(),
        store.clone(),
        authz,
        Duration::from_secs(300),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    )
    .with_version_storage(options);
    let archive = BlobStorage::new(pool.clone(), store.clone());
    Some(Env {
        pool,
        service,
        archive,
        store,
        ctx,
        options,
    })
}

async fn history(env: &Env, count: usize) -> (Uuid, Vec<Vec<u8>>) {
    let tag = Uuid::new_v4();
    let mut versions = Vec::new();
    let mut body: Vec<u8> = (0u64..1500)
        .flat_map(|i| {
            format!(
                "{tag} paragraph {i}: テスト内容の復元 {:016x}\n",
                i.wrapping_mul(6364136223846793005)
            )
            .into_bytes()
        })
        .collect();
    versions.push(body.clone());
    let file = env
        .service
        .write_file_internal(
            &env.ctx,
            None,
            &format!("{tag}.txt"),
            &body,
            "text/plain",
            None,
        )
        .await
        .unwrap();
    for v in 1..count {
        body.extend_from_slice(format!("revision {v}\n").as_bytes());
        versions.push(body.clone());
        env.service
            .update_file_content_internal(&env.ctx, file.id, &body, "text/plain", None)
            .await
            .unwrap();
    }
    (file.id, versions)
}

async fn compact(env: &Env, node: Uuid) {
    while env
        .archive
        .compact_node(env.ctx.tenant(), env.ctx.org(), node, env.options)
        .await
        .unwrap()
        == 16
    {}
}

async fn expire_raw(env: &Env) {
    sqlx::query("UPDATE blob SET raw_expires_at = clock_timestamp() - interval '1 second', raw_audit_at = clock_timestamp() - interval '1 second' WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).execute(&env.pool).await.unwrap();
}

async fn download(url: &str) -> Vec<u8> {
    reqwest::get(url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn expired_raw_is_rebuilt_for_urls_indexing_restore_and_proposal() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 8).await;
    let ticket = env
        .service
        .issue_version_download_url(&env.ctx, id, 2, None)
        .await
        .unwrap();
    compact(&env, id).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    assert_eq!(download(&ticket.url).await, versions[1]);
    expire_raw(&env).await;
    assert!(
        env.archive
            .collect(env.ctx.tenant(), env.ctx.org())
            .await
            .unwrap()
            >= 7
    );
    let key = blob_object_key(
        env.ctx.tenant_id(),
        env.ctx.org(),
        &sha256_hex(&versions[1]),
    );
    assert!(!env.store.exists(&key).await.unwrap());
    let url = env
        .service
        .issue_version_download_url(&env.ctx, id, 2, None)
        .await
        .unwrap();
    assert_eq!(download(&url.url).await, versions[1]);
    let bounded_cache: bool = sqlx::query_scalar("SELECT raw_expires_at < clock_timestamp() + interval '10 minutes' FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).bind(sha256_hex(&versions[1])).fetch_one(&env.pool).await.unwrap();
    assert!(
        bounded_cache,
        "raw の監査予定時刻でダウンロードキャッシュの TTL を引き延ばさない"
    );
    let internal = IndexerStorage::new(env.pool.clone(), env.store.clone())
        .presign_internal_get(
            env.ctx.tenant_id(),
            env.ctx.org(),
            &sha256_hex(&versions[2]),
        )
        .await
        .unwrap();
    assert_eq!(download(&internal).await, versions[2]);
    let restored = env
        .service
        .restore_version(&env.ctx, id, 4, None)
        .await
        .unwrap();
    assert_eq!(restored.version, 9);
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    assert!(env
        .store
        .exists(&blob_object_key(
            env.ctx.tenant_id(),
            env.ctx.org(),
            &sha256_hex(&versions[3])
        ))
        .await
        .unwrap());
    assert_eq!(
        env.service
            .read_file_internal(&env.ctx, id, None)
            .await
            .unwrap()
            .1,
        versions[3]
    );
    let proposal = env
        .service
        .propose_file_content_internal(&env.ctx, id, &versions[4], "text/plain", None)
        .await
        .unwrap();
    env.service
        .adopt_proposal_version(&env.ctx, id, proposal.version, None)
        .await
        .unwrap();
    assert_eq!(
        env.service
            .read_file_internal(&env.ctx, id, None)
            .await
            .unwrap()
            .1,
        versions[4]
    );
    let (list, _) = env
        .service
        .list_versions(&env.ctx, id, None, 100, None)
        .await
        .unwrap();
    assert_eq!(list.len(), 11);
}

#[tokio::test]
async fn backup_hold_current_reference_and_indirect_objects_survive_gc() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 4).await;
    compact(&env, id).await;
    expire_raw(&env).await;
    let lease = Uuid::new_v4();
    env.archive
        .hold_backup(
            env.ctx.tenant(),
            env.ctx.org(),
            lease,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(
        env.archive
            .collect(env.ctx.tenant(), env.ctx.org())
            .await
            .unwrap(),
        0
    );
    let old_key = blob_object_key(
        env.ctx.tenant_id(),
        env.ctx.org(),
        &sha256_hex(&versions[0]),
    );
    assert!(env.store.exists(&old_key).await.unwrap());
    env.archive
        .release_backup(env.ctx.tenant(), env.ctx.org(), lease)
        .await
        .unwrap();
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    let exported = env
        .archive
        .materialize_for_export(
            env.ctx.tenant(),
            env.ctx.org(),
            &sha256_hex(&versions[0]),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(env.store.get_object(&exported).await.unwrap(), versions[0]);
    let current_key = blob_object_key(
        env.ctx.tenant_id(),
        env.ctx.org(),
        &sha256_hex(&versions[3]),
    );
    assert!(env.store.exists(&current_key).await.unwrap());
    sqlx::query("UPDATE blob_storage_object SET protected_until = clock_timestamp() - interval '1 day' WHERE tenant_id = $1 AND org = $2")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).execute(&env.pool).await.unwrap();
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    for v in 1..4 {
        let ticket = env
            .service
            .issue_version_download_url(&env.ctx, id, v, None)
            .await
            .unwrap();
        assert_eq!(download(&ticket.url).await, versions[v as usize - 1]);
    }
    let refs: Vec<String> = sqlx::query_scalar(
        "SELECT object_sha256 FROM blob_storage_ref WHERE tenant_id = $1 AND org = $2",
    )
    .bind(env.ctx.tenant_id())
    .bind(env.ctx.org())
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert!(!refs.is_empty());
    for sha in refs {
        assert!(env
            .store
            .exists(&format!(
                "{}/{}/storage-v1/{sha}",
                env.ctx.tenant_id(),
                env.ctx.org()
            ))
            .await
            .unwrap());
    }
}

#[tokio::test]
async fn concurrent_compaction_gc_and_restore_preserve_bytes() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 6).await;
    let (a, b) = tokio::join!(compact(&env, id), compact(&env, id));
    let _ = (a, b);
    expire_raw(&env).await;
    let (gc, url) = tokio::join!(
        env.archive.collect(env.ctx.tenant(), env.ctx.org()),
        env.service
            .issue_version_download_url(&env.ctx, id, 3, None)
    );
    gc.unwrap();
    assert_eq!(download(&url.unwrap().url).await, versions[2]);
    expire_raw(&env).await;
    let (gc, restored) = tokio::join!(
        env.archive.collect(env.ctx.tenant(), env.ctx.org()),
        env.service.restore_version(&env.ctx, id, 2, None)
    );
    gc.unwrap();
    restored.unwrap();
    assert_eq!(
        env.service
            .read_file_internal(&env.ctx, id, None)
            .await
            .unwrap()
            .1,
        versions[1]
    );
}

#[tokio::test]
async fn corrupt_payload_fails_closed_and_cross_tenant_lookup_is_absent() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 4).await;
    compact(&env, id).await;
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    let index = IndexerStorage::new(env.pool.clone(), env.store.clone());
    assert!(matches!(
        index
            .presign_internal_get("another-tenant", env.ctx.org(), &sha256_hex(&versions[1]))
            .await,
        Err(StorageError::NotFound)
    ));
    let object: String = sqlx::query_scalar("SELECT object_sha256 FROM blob_storage_ref WHERE tenant_id = $1 AND org = $2 AND blob_sha256 = $3 ORDER BY object_sha256 LIMIT 1")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).bind(sha256_hex(&versions[1])).fetch_one(&env.pool).await.unwrap();
    let key = format!(
        "{}/{}/storage-v1/{object}",
        env.ctx.tenant_id(),
        env.ctx.org()
    );
    env.store
        .put_object(&key, b"broken".to_vec(), "application/octet-stream")
        .await
        .unwrap();
    assert!(matches!(
        env.service
            .issue_version_download_url(&env.ctx, id, 2, None)
            .await,
        Err(StorageError::Integrity(_))
    ));
    assert_eq!(
        env.service
            .read_file_internal(&env.ctx, id, None)
            .await
            .unwrap()
            .1,
        versions[3]
    );
}

#[tokio::test]
async fn registered_orphans_and_rolled_back_raw_cache_are_collected_idempotently() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 3).await;
    compact(&env, id).await;
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    let raw_key = blob_object_key(
        env.ctx.tenant_id(),
        env.ctx.org(),
        &sha256_hex(&versions[0]),
    );
    env.store
        .put_object(&raw_key, versions[0].clone(), "text/plain")
        .await
        .unwrap();
    let orphan = sha256_hex(b"orphan");
    let object_key = format!(
        "{}/{}/storage-v1/{orphan}",
        env.ctx.tenant_id(),
        env.ctx.org()
    );
    sqlx::query("INSERT INTO blob_storage_object (tenant_id, org, sha256, size_bytes, protected_until) VALUES ($1, $2, $3, 6, clock_timestamp() - interval '1 day')")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).bind(&orphan).execute(&env.pool).await.unwrap();
    env.store
        .put_object(&object_key, b"orphan".to_vec(), "application/octet-stream")
        .await
        .unwrap();
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    assert!(!env.store.exists(&raw_key).await.unwrap());
    assert!(!env.store.exists(&object_key).await.unwrap());
    assert_eq!(
        env.archive
            .collect(env.ctx.tenant(), env.ctx.org())
            .await
            .unwrap(),
        0
    );
    // GC より遅れて PUT が完了した状態。tombstone から再回収できる。
    env.store
        .put_object(&object_key, b"orphan".to_vec(), "application/octet-stream")
        .await
        .unwrap();
    sqlx::query("UPDATE blob_storage_object SET protected_until = clock_timestamp() - interval '1 second' WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).bind(&orphan).execute(&env.pool).await.unwrap();
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    assert!(!env.store.exists(&object_key).await.unwrap());
}

#[tokio::test]
async fn large_insertions_share_chunks_and_restore_exactly() {
    let Some(env) = setup().await else { return };
    let mut state = 73u64;
    let mut data: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let mut versions = vec![data.clone()];
    let file = env
        .service
        .write_file_internal(
            &env.ctx,
            None,
            "binary.dat",
            &data,
            "application/octet-stream",
            None,
        )
        .await
        .unwrap();
    for i in 0..3 {
        data.splice(0..0, format!("insert-{i}").into_bytes());
        versions.push(data.clone());
        env.service
            .update_file_content_internal(
                &env.ctx,
                file.id,
                &data,
                "application/octet-stream",
                None,
            )
            .await
            .unwrap();
    }
    compact(&env, file.id).await;
    let shared: i64 = sqlx::query_scalar("SELECT count(*) FROM blob_storage_object WHERE tenant_id = $1 AND org = $2 AND refcount > 1")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).fetch_one(&env.pool).await.unwrap();
    assert!(shared > 10);
    let physical: i64 = sqlx::query_scalar("SELECT coalesce(sum(size_bytes), 0)::bigint FROM blob_storage_object WHERE tenant_id = $1 AND org = $2 AND refcount > 0")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).fetch_one(&env.pool).await.unwrap();
    assert!(
        physical < 3 * 1024 * 1024,
        "旧版3つで2 MiB台を共有する: {physical}"
    );
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    for v in 1..=3 {
        let ticket = env
            .service
            .issue_version_download_url(&env.ctx, file.id, v, None)
            .await
            .unwrap();
        assert_eq!(download(&ticket.url).await, versions[v as usize - 1]);
    }
    let counts: Vec<(i64, i64)> = sqlx::query_as("SELECT o.refcount, count(r.object_sha256) FROM blob_storage_object o LEFT JOIN blob_storage_ref r \
        ON r.tenant_id = o.tenant_id AND r.org = o.org AND r.object_sha256 = o.sha256 \
        WHERE o.tenant_id = $1 AND o.org = $2 GROUP BY o.sha256, o.refcount")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).fetch_all(&env.pool).await.unwrap();
    assert!(counts.iter().all(|(stored, actual)| stored == actual));
}

#[tokio::test]
async fn long_history_uses_bounded_direct_anchors_and_preserves_every_version() {
    let Some(mut env) = setup().await else { return };
    env.options.read_cost_bytes = 8192;
    let (id, versions) = history(&env, 129).await;
    assert_eq!(
        env.archive
            .compact_node(env.ctx.tenant(), env.ctx.org(), id, env.options)
            .await
            .unwrap(),
        16
    );
    let first_batch: Vec<i64> = sqlx::query_scalar("SELECT v.version FROM node_version v JOIN blob b ON b.tenant_id = v.tenant_id AND b.org = v.org AND b.sha256 = v.blob_sha256 WHERE v.tenant_id = $1 AND v.org = $2 AND v.node_id = $3 AND b.storage_manifest IS NOT NULL ORDER BY v.version")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).bind(id).fetch_all(&env.pool).await.unwrap();
    assert_eq!(
        first_batch,
        (1..=16).collect::<Vec<_>>(),
        "古い版から基準実体を用意する"
    );
    compact(&env, id).await;
    let reads: Vec<i32> = sqlx::query_scalar("SELECT storage_reads FROM blob WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).fetch_all(&env.pool).await.unwrap();
    assert_eq!(reads.len(), 128);
    assert!(reads.iter().all(|r| *r <= 2));
    assert!(reads.contains(&2));
    // 猶予が過ぎた raw を複数バッチで取り除く。
    expire_raw(&env).await;
    for _ in 0..3 {
        env.archive
            .collect(env.ctx.tenant(), env.ctx.org())
            .await
            .unwrap();
    }
    for v in 1..=129 {
        let url = env
            .service
            .issue_version_download_url(&env.ctx, id, v, None)
            .await
            .unwrap();
        assert_eq!(download(&url.url).await, versions[v as usize - 1]);
    }
}

#[tokio::test]
async fn deleting_tenant_cannot_start_compaction_or_backup() {
    let Some(env) = setup().await else { return };
    let (id, _) = history(&env, 2).await;
    sqlx::query("INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, $2, 'deleting test', 'deleting')")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).execute(&env.pool).await.unwrap();
    assert!(matches!(
        env.archive
            .compact_node(env.ctx.tenant(), env.ctx.org(), id, env.options)
            .await,
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        env.archive
            .hold_backup(
                env.ctx.tenant(),
                env.ctx.org(),
                Uuid::new_v4(),
                Duration::from_secs(60)
            )
            .await,
        Err(StorageError::NotFound)
    ));
}

#[tokio::test]
async fn purge_is_blocked_by_backup_and_cleans_all_representation_sidecars() {
    let Some(env) = setup().await else { return };
    let (id, _) = history(&env, 4).await;
    compact(&env, id).await;
    let hold = Uuid::new_v4();
    env.archive
        .hold_backup(
            env.ctx.tenant(),
            env.ctx.org(),
            hold,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert!(matches!(
        env.service
            .purge_tenant(env.ctx.tenant_id(), env.ctx.org(), "alice")
            .await,
        Err(StorageError::Invalid(_))
    ));
    env.archive
        .release_backup(env.ctx.tenant(), env.ctx.org(), hold)
        .await
        .unwrap();
    env.service
        .purge_tenant(env.ctx.tenant_id(), env.ctx.org(), "alice")
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM blob_storage_ref WHERE tenant_id = $1) + \
        (SELECT count(*) FROM blob_storage_object WHERE tenant_id = $1) + (SELECT count(*) FROM blob_storage_hold WHERE tenant_id = $1)")
        .bind(env.ctx.tenant_id()).fetch_one(&env.pool).await.unwrap();
    assert_eq!(remaining, 0);
    assert!(env
        .store
        .list_prefix(&format!("{}/", env.ctx.tenant_id()), None)
        .await
        .unwrap()
        .0
        .is_empty());
}

#[tokio::test]
async fn purge_lock_rejects_new_materialization_without_waiting_for_pool() {
    let Some(env) = setup().await else { return };
    let (id, _) = history(&env, 2).await;
    compact(&env, id).await;
    let mut guard = storage::db_tenant::begin_tenant_tx(&env.pool, env.ctx.tenant())
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 118488))")
        .bind(env.ctx.tenant_id())
        .execute(&mut *guard)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        env.service
            .issue_version_download_url(&env.ctx, id, 1, None),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(StorageError::NotFound)));
    guard.rollback().await.unwrap();
}

#[tokio::test]
async fn worker_backfills_legacy_versions_without_new_writes() {
    let Some(mut env) = setup().await else { return };
    env.service = env
        .service
        .with_version_storage(VersionStorageOptions::default());
    let (id, versions) = history(&env, 3).await;
    // 既存の大量履歴に混ぜても、この fixture は移行対象の古い履歴として先に発見される。
    sqlx::query(
        "UPDATE blob SET created_at = '2000-01-01T00:00:00Z' WHERE tenant_id = $1 AND org = $2",
    )
    .bind(env.ctx.tenant_id())
    .bind(env.ctx.org())
    .execute(&env.pool)
    .await
    .unwrap();

    let handle =
        storage::blob_storage::spawn_version_storage_worker(env.archive.clone(), env.options);
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM blob WHERE tenant_id = $1 AND org = $2 AND storage_manifest IS NOT NULL")
                .bind(env.ctx.tenant_id()).bind(env.ctx.org()).fetch_one(&env.pool).await.unwrap();
            if count == 2 { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await;
    handle.abort();
    let _ = handle.await;
    outcome.expect("新たな保存がなくても既存履歴が処理される");
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    let url = env
        .service
        .issue_version_download_url(&env.ctx, id, 1, None)
        .await
        .unwrap();
    assert_eq!(download(&url.url).await, versions[0]);
}

#[tokio::test]
async fn migration_protects_legacy_urls_without_extending_new_raw_leases() {
    use sqlx::{postgres::PgConnectOptions, Connection, PgConnection};
    use std::{borrow::Cow, str::FromStr};
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let database = format!("version_legacy_{}", Uuid::new_v4().simple());
    let base = PgConnectOptions::from_str(&url).unwrap();
    let mut admin = PgConnection::connect_with(&base.clone().database("postgres"))
        .await
        .unwrap();
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(base.database(&database))
        .await
        .unwrap();
    let mut before = sqlx::migrate!("../../migrations");
    before.migrations = Cow::Owned(before.iter().filter(|m| m.version < 118).cloned().collect());
    before.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key) VALUES ('legacy', 'org', $1, 4, 'text/plain', 'legacy-object')")
        .bind("a".repeat(64)).execute(&pool).await.unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let protected: bool = sqlx::query_scalar("SELECT raw_expires_at > now() + interval '6 days' FROM blob WHERE tenant_id = 'legacy' AND org = 'org' AND sha256 = $1")
        .bind("a".repeat(64)).fetch_one(&pool).await.unwrap();
    assert!(protected);
    sqlx::query("INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key) VALUES ('legacy', 'org', $1, 4, 'text/plain', 'new-object')")
        .bind("b".repeat(64)).execute(&pool).await.unwrap();
    let short: bool = sqlx::query_scalar("SELECT raw_expires_at < now() + interval '1 minute' FROM blob WHERE tenant_id = 'legacy' AND org = 'org' AND sha256 = $1")
        .bind("b".repeat(64)).fetch_one(&pool).await.unwrap();
    assert!(short);
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&mut admin)
        .await
        .unwrap();
}

#[tokio::test]
async fn namespace_move_preserves_archived_versions_and_rejects_early_cleanup() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 3).await;
    compact(&env, id).await;
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    let target = TenantId::parse(format!("vs-{}", Uuid::new_v4().simple())).unwrap();
    let hold = Uuid::new_v4();
    env.archive
        .hold_backup(
            env.ctx.tenant(),
            env.ctx.org(),
            hold,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert!(matches!(
        env.archive
            .begin_namespace_move(env.ctx.tenant().clone(), target.clone())
            .await,
        Err(StorageError::Invalid(_))
    ));
    env.archive
        .release_backup(env.ctx.tenant(), env.ctx.org(), hold)
        .await
        .unwrap();
    let movement = env
        .archive
        .begin_namespace_move(env.ctx.tenant().clone(), target.clone())
        .await
        .unwrap();
    assert!(matches!(
        env.archive.collect(env.ctx.tenant(), env.ctx.org()).await,
        Err(StorageError::NotFound)
    ));
    assert!(matches!(
        movement.cleanup_old_objects().await,
        Err(StorageError::Invalid(_))
    ));
    assert!(movement.copy_objects().await.unwrap() > 0);
    // 再実行でも同じ実体を使用し、コピー済みの内容を検証する。
    assert!(movement.copy_objects().await.unwrap() > 0);
    let raw: Vec<(String, String)> = sqlx::query_as(
        "SELECT sha256, object_key FROM blob WHERE tenant_id = $1 AND org = $2 AND raw_available",
    )
    .bind(env.ctx.tenant_id())
    .bind(env.ctx.org())
    .fetch_all(&env.pool)
    .await
    .unwrap();
    for (sha, old) in &raw {
        env.store
            .copy(old, &blob_object_key(target.as_str(), env.ctx.org(), sha))
            .await
            .unwrap();
    }
    let tables = storage::tenant_scope::tenant_scoped_tables(&env.pool)
        .await
        .unwrap();
    let mut tx = env.pool.begin().await.unwrap();
    sqlx::query("UPDATE blob SET object_key = $2 || substring(object_key FROM length($1) + 1) WHERE tenant_id = $1")
        .bind(env.ctx.tenant_id()).bind(target.as_str()).execute(&mut *tx).await.unwrap();
    storage::tenant_scope::rename_tenant_rows(
        &mut tx,
        &tables,
        env.ctx.tenant_id(),
        target.as_str(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(movement.cleanup_old_objects().await.unwrap() > 0);
    for (_, old) in &raw {
        env.store.delete(old).await.unwrap();
    }
    drop(movement);
    let index = IndexerStorage::new(env.pool.clone(), env.store.clone());
    for body in &versions {
        let url = index
            .presign_internal_get(target.as_str(), env.ctx.org(), &sha256_hex(body))
            .await
            .unwrap();
        assert_eq!(download(&url).await, *body);
    }
    assert!(matches!(
        index
            .presign_internal_get(
                env.ctx.tenant_id(),
                env.ctx.org(),
                &sha256_hex(&versions[0])
            )
            .await,
        Err(StorageError::NotFound)
    ));
    let mismatches: i64 = sqlx::query_scalar("SELECT count(*) FROM blob_storage_object o WHERE o.tenant_id = $1 AND o.org = $2 AND o.refcount <> (SELECT count(*) FROM blob_storage_ref r WHERE r.tenant_id = o.tenant_id AND r.org = o.org AND r.object_sha256 = o.sha256)")
        .bind(target.as_str()).bind(env.ctx.org()).fetch_one(&env.pool).await.unwrap();
    assert_eq!(mismatches, 0);
    assert!(env
        .store
        .list_prefix(&format!("{}/", env.ctx.tenant_id()), None)
        .await
        .unwrap()
        .0
        .is_empty());
}
