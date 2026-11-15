use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use storage::ObjectStoreError;
use tokio::sync::Semaphore;

struct SlowPutStore {
    inner: Arc<dyn ObjectStore>,
    entered: AtomicUsize,
    release: Semaphore,
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
