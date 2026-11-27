//! 関数の結合テストが共有するメモリの ObjectStore（コード置き場）。

use std::sync::Mutex;

use storage::{ObjectStore, ObjectStoreError};

/// メモリ ObjectStore（コード置き場）。
#[derive(Default)]
pub(crate) struct MemStore(pub(crate) Mutex<std::collections::HashMap<String, Vec<u8>>>);

#[async_trait::async_trait]
impl ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: std::time::Duration,
        _: i64,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: std::time::Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: std::time::Duration,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn read_and_hash(&self, k: &str) -> Result<(String, u64), ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn put_object(&self, k: &str, b: Vec<u8>, _: &str) -> Result<(), ObjectStoreError> {
        self.0.lock().unwrap().insert(k.into(), b);
        Ok(())
    }
    async fn get_object(&self, k: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.0
            .lock()
            .unwrap()
            .get(k)
            .cloned()
            .ok_or_else(|| ObjectStoreError::NotFound(k.into()))
    }
    async fn exists(&self, _: &str) -> Result<bool, ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}
