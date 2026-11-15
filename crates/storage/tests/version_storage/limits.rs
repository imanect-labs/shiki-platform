use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use storage::ObjectStoreError;
use tokio::sync::Semaphore;

struct SlowPutStore {
    inner: Arc<dyn ObjectStore>,
    entered: AtomicUsize,
    release: Semaphore,
    fail_put: bool,
}

#[async_trait::async_trait]
impl ObjectStore for SlowPutStore {
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), ObjectStoreError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        if self.fail_put {
            return Err(ObjectStoreError::Backend("injected PUT failure".into()));
        }
        let _permit = self
            .release
            .acquire()
            .await
            .map_err(|e| ObjectStoreError::Backend(e.to_string()))?;
        self.inner.put_object(key, bytes, content_type).await
    }
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        self.inner.ensure_bucket().await
    }
    async fn presign_put(
        &self,
        key: &str,
        ttl: Duration,
        length: i64,
    ) -> Result<String, ObjectStoreError> {
        self.inner.presign_put(key, ttl, length).await
    }
    async fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        name: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        self.inner.presign_get(key, ttl, name, content_type).await
    }
    async fn presign_get_internal(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<String, ObjectStoreError> {
        self.inner.presign_get_internal(key, ttl).await
    }
    async fn read_and_hash(&self, key: &str) -> Result<(String, u64), ObjectStoreError> {
        self.inner.read_and_hash(key).await
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.inner.get_object(key).await
    }
    async fn exists(&self, key: &str) -> Result<bool, ObjectStoreError> {
        self.inner.exists(key).await
    }
    async fn copy(&self, from: &str, to: &str) -> Result<(), ObjectStoreError> {
        self.inner.copy(from, to).await
    }
    async fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        self.inner.delete(key).await
    }
    async fn list_prefix(
        &self,
        prefix: &str,
        continuation: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        self.inner.list_prefix(prefix, continuation).await
    }
    async fn delete_batch(&self, keys: &[String]) -> Result<(), ObjectStoreError> {
        self.inner.delete_batch(keys).await
    }
}

#[tokio::test]
async fn reconstruction_holds_io_limit_until_raw_put_finishes() {
    let Some(env) = setup().await else { return };
    let (id, versions) = history(&env, 6).await;
    compact(&env, id).await;
    expire_raw(&env).await;
    env.archive
        .collect(env.ctx.tenant(), env.ctx.org())
        .await
        .unwrap();
    let store = Arc::new(SlowPutStore {
        inner: env.store.clone(),
        entered: AtomicUsize::new(0),
        release: Semaphore::new(0),
        fail_put: false,
    });
    let archive = BlobStorage::new(env.pool.clone(), store.clone());
    let mut tasks = tokio::task::JoinSet::new();
    for body in &versions[..5] {
        let (archive, tenant, org, sha) = (
            archive.clone(),
            env.ctx.tenant().clone(),
            env.ctx.org().to_owned(),
            sha256_hex(body),
        );
        tasks.spawn(async move {
            archive
                .materialize_for_export(&tenant, &org, &sha, Duration::from_secs(60))
                .await
        });
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        while store.entered.load(Ordering::SeqCst) < 4 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let concurrent = store.entered.load(Ordering::SeqCst);
    store.release.add_permits(5);
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    assert_eq!(
        concurrent, 4,
        "遅いPUTが復元済みバッファを保持する間、5件目を展開しない"
    );
    assert_eq!(store.entered.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn failed_put_keeps_orphan_gc_within_its_protection_deadline() {
    let Some(env) = setup().await else { return };
    let (id, _) = history(&env, 2).await;
    sqlx::query("INSERT INTO blob_storage_scope (tenant_id, org, next_check_at) VALUES ($1, $2, now() + interval '7 days')")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).execute(&env.pool).await.unwrap();
    let store = Arc::new(SlowPutStore {
        inner: env.store.clone(),
        entered: AtomicUsize::new(0),
        release: Semaphore::new(0),
        fail_put: true,
    });
    let archive = BlobStorage::new(env.pool.clone(), store.clone());
    assert!(matches!(
        archive
            .compact_node(env.ctx.tenant(), env.ctx.org(), id, 0, env.options)
            .await,
        Err(StorageError::ObjectStore(_))
    ));
    assert!(store.entered.load(Ordering::SeqCst) > 0);
    let (objects, scheduled): (i64, bool) = sqlx::query_as(
        "SELECT count(*), bool_and(s.next_check_at <= o.protected_until) FROM blob_storage_scope s \
         JOIN blob_storage_object o USING (tenant_id, org) WHERE s.tenant_id = $1 AND s.org = $2 AND o.refcount = 0")
        .bind(env.ctx.tenant_id()).bind(env.ctx.org()).fetch_one(&env.pool).await.unwrap();
    assert!(objects > 0);
    assert!(
        scheduled,
        "PUT失敗後も、遠いraw期限ではなく未公開実体の期限に回収を予定する"
    );
}
