//! 実DBを使い、permit/poolの逆順と撤去後のジョブ再投入を再現する。
use super::*;

#[tokio::test]
async fn skipped_backfill_rows_do_not_count_as_progress() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("skip-{}", uuid::Uuid::new_v4().simple())).unwrap();
    sqlx::query("INSERT INTO blob (tenant_id, org, sha256, size_bytes, object_key, created_at) \
        SELECT $1, 'org', lpad(to_hex(i),64,'0'), 0, 'unused', '1900-01-01T00:00:00Z' FROM generate_series(1,64) i")
        .bind(tenant.as_str()).execute(&storage.pool).await.unwrap();
    let target = TenantId::parse(format!("skip-target-{}", uuid::Uuid::new_v4().simple())).unwrap();
    let guard = storage
        .begin_namespace_move(tenant.clone(), target)
        .await
        .unwrap();
    assert_eq!(storage.backfill(codec::MAX_BYTES).await.unwrap(), 0);
    drop(guard);
    let mut guard = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    sqlx::query("SELECT sha256 FROM blob WHERE tenant_id = $1 FOR UPDATE")
        .bind(tenant.as_str())
        .fetch_all(&mut *guard)
        .await
        .unwrap();
    assert_eq!(storage.backfill(codec::MAX_BYTES).await.unwrap(), 0);
    guard.rollback().await.unwrap();
    sqlx::query("DELETE FROM blob WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .execute(&storage.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn held_org_cannot_starve_other_org_gc_candidates() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("gc-fair-{}", uuid::Uuid::new_v4().simple())).unwrap();
    sqlx::query("INSERT INTO blob (tenant_id, org, sha256, size_bytes, object_key, storage_manifest, raw_expires_at) \
        SELECT $1, 'held', lpad(to_hex(i),64,'0'), 0, 'unused', '{}'::jsonb, '1900-01-01T00:00:00Z' FROM generate_series(1,16) i")
        .bind(tenant.as_str()).execute(&storage.pool).await.unwrap();
    sqlx::query("INSERT INTO blob (tenant_id, org, sha256, size_bytes, object_key, storage_manifest, raw_expires_at) \
        VALUES ($1, 'free', $2, 0, 'unused', '{}'::jsonb, '1901-01-01T00:00:00Z')")
        .bind(tenant.as_str()).bind("b".repeat(64)).execute(&storage.pool).await.unwrap();
    let hold = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO blob_storage_scope (tenant_id, org, next_check_at) VALUES ($1, 'held', '1900-01-01'), ($1, 'free', '1901-01-01')")
        .bind(tenant.as_str()).execute(&storage.pool).await.unwrap();
    storage
        .hold_backup(&tenant, "held", hold, Duration::from_secs(60))
        .await
        .unwrap();
    let scopes = storage.gc_scopes_due().await.unwrap();
    assert!(scopes.contains(&(tenant.as_str().to_owned(), "free".to_owned())));
    assert!(!scopes.contains(&(tenant.as_str().to_owned(), "held".to_owned())));
    storage.release_backup(&tenant, "held", hold).await.unwrap();
    sqlx::query("DELETE FROM blob_storage_scope WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .execute(&storage.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM blob WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .execute(&storage.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn concurrent_purges_leave_a_connection_for_cleanup() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("purge-pool-{}", uuid::Uuid::new_v4().simple())).unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let storage = storage.clone();
        let tenant = tenant.clone();
        tasks.spawn(async move {
            let _guard = storage.begin_tenant_purge(&tenant).await.unwrap();
            sqlx::query("SELECT 1")
                .execute(&storage.pool)
                .await
                .unwrap();
        });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
    let Some(tiny) = self::storage(1).await else {
        return;
    };
    assert!(matches!(
        tiny.begin_tenant_purge(&tenant).await,
        Err(StorageError::Invalid(_))
    ));
}

#[tokio::test]
async fn deleting_tenant_does_not_block_other_gc_scopes() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("gc-delete-{}", uuid::Uuid::new_v4().simple())).unwrap();
    sqlx::query("INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, 'org', 'test', 'deleting')")
        .bind(tenant.as_str())
        .execute(&storage.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO blob_storage_scope (tenant_id, org, next_check_at) VALUES ($1, 'org', '1900-01-01'), ($2, 'org', '1901-01-01')")
        .bind(tenant.as_str()).bind(format!("{tenant}-active")).execute(&storage.pool).await.unwrap();
    let scopes = storage.gc_scopes_due().await.unwrap();
    assert!(!scopes.iter().any(|(t, _)| t == tenant.as_str()));
    assert!(scopes.contains(&(format!("{tenant}-active"), "org".into())));
    sqlx::query("DELETE FROM blob_storage_scope WHERE tenant_id IN ($1, $2)")
        .bind(tenant.as_str())
        .bind(format!("{tenant}-active"))
        .execute(&storage.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM tenant WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .execute(&storage.pool)
        .await
        .unwrap();
}

async fn storage(max_connections: u32) -> Option<BlobStorage> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return None;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    // このテストはストア操作へ進まず、DB接続/状態検査で止まる。
    let config: crate::S3Config = serde_json::from_value(serde_json::json!({
        "internal_endpoint": "http://127.0.0.1:1", "public_endpoint": "http://127.0.0.1:1",
        "bucket": "unused", "access_key": "unused", "secret_key": "unused"
    }))
    .unwrap();
    Some(BlobStorage::new(
        pool,
        Arc::new(crate::S3ObjectStore::new(&config)),
    ))
}

#[tokio::test]
async fn waiting_for_pool_does_not_consume_io_permits() {
    let Some(storage) = storage(1).await else {
        return;
    };
    let _connection = storage.pool.acquire().await.unwrap();
    let tenant = TenantId::parse("permit-pool-test").unwrap();
    let sha = "a".repeat(64);
    let mut readers = Vec::new();
    for _ in 0..4 {
        readers.push(Box::pin(storage.presign(
            &tenant,
            "org",
            &sha,
            DownloadSpec {
                ttl: Duration::from_secs(60),
                filename: None,
                content_type: None,
                internal: true,
            },
        )));
    }
    // 全readerを接続待ちまで進め、future内のguardを保持する。
    for reader in &mut readers {
        assert!(futures::poll!(reader.as_mut()).is_pending());
    }
    let permits = tokio::time::timeout(Duration::from_secs(1), IO_LIMIT.acquire_many(4)).await;
    assert!(
        permits.is_ok(),
        "DB接続待ちで全IO permitを占有すると内部読取が停止する"
    );
}

#[tokio::test]
async fn stale_backfill_cannot_enqueue_during_or_after_purge() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("guard-{}", uuid::Uuid::new_v4().simple())).unwrap();
    let mut guard = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 118488))")
        .bind(tenant.as_str())
        .execute(&mut *guard)
        .await
        .unwrap();
    let mut tx = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    assert!(matches!(
        BlobStorage::enqueue(&mut tx, &tenant, "org", uuid::Uuid::new_v4()).await,
        Err(StorageError::NotFound)
    ));
    tx.rollback().await.unwrap();
    guard.rollback().await.unwrap();
    let mut tx = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, 'org', 'deleted test', 'deleted')")
        .bind(tenant.as_str()).execute(&mut *tx).await.unwrap();
    assert!(matches!(
        BlobStorage::enqueue(&mut tx, &tenant, "org", uuid::Uuid::new_v4()).await,
        Err(StorageError::NotFound)
    ));
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM job_queue WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn pending_jobs_keep_the_earliest_cursor_and_active_jobs_allow_followups() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("cursor-{}", uuid::Uuid::new_v4().simple())).unwrap();
    let node = uuid::Uuid::new_v4();
    let mut tx = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    for after in [100, 50, 200] {
        BlobStorage::enqueue_after(&mut tx, &tenant, "org", node, after)
            .await
            .unwrap();
    }
    let cursors: Vec<i64> = sqlx::query_scalar("SELECT (payload->>'after_version')::bigint FROM job_queue WHERE tenant_id = $1 ORDER BY id")
        .bind(tenant.as_str()).fetch_all(&mut *tx).await.unwrap();
    assert_eq!(cursors, [50]);
    sqlx::query("UPDATE job_queue SET attempts = 1 WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    for after in [300, 150] {
        BlobStorage::enqueue_after(&mut tx, &tenant, "org", node, after)
            .await
            .unwrap();
    }
    let cursors: Vec<i64> = sqlx::query_scalar("SELECT (payload->>'after_version')::bigint FROM job_queue WHERE tenant_id = $1 ORDER BY id")
        .bind(tenant.as_str()).fetch_all(&mut *tx).await.unwrap();
    assert_eq!(cursors, [50, 150]);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn readers_share_a_blob_lock_and_tiny_pools_fail_without_waiting() {
    let Some(storage) = storage(2).await else {
        return;
    };
    let tenant = TenantId::parse(format!("share-{}", uuid::Uuid::new_v4().simple())).unwrap();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            storage.compact_node(
                &tenant,
                "org",
                uuid::Uuid::new_v4(),
                0,
                VersionStorageOptions::default()
            )
        )
        .await
        .unwrap(),
        Err(StorageError::Invalid(_))
    ));
    let sha = "a".repeat(64);
    let mut a = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    sqlx::query("INSERT INTO blob (tenant_id, org, sha256, size_bytes, object_key) VALUES ($1, 'org', $2, 1, 'unused')")
        .bind(tenant.as_str()).bind(&sha).execute(&mut *a).await.unwrap();
    a.commit().await.unwrap();
    let mut a = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    let mut b = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    BlobStorage::lock_blob(&mut a, &tenant, "org", &sha, false)
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        BlobStorage::lock_blob(&mut b, &tenant, "org", &sha, false),
    )
    .await
    .unwrap()
    .unwrap();
    b.rollback().await.unwrap();
    a.rollback().await.unwrap();
    let mut tx = crate::db_tenant::begin_tenant_tx(&storage.pool, &tenant)
        .await
        .unwrap();
    sqlx::query("DELETE FROM blob WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
