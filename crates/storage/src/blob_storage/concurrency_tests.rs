//! 実DBを使い、permit/poolの逆順と撤去後のジョブ再投入を再現する。
use super::*;

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
