//! インメモリ [`SessionStore`]（テスト用）。TTL は無視する。
//!
//! Redis を立てずに session ミドルウェア/BFF エンドポイントの挙動を検証するためのもの。

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};

use async_trait::async_trait;

use super::store::{SessionError, SessionRecord, SessionStore};

#[derive(Default)]
pub struct MemorySessionStore {
    inner: Mutex<HashMap<String, SessionRecord>>,
    /// 消費済み jti（TTL は無視・テスト用のリプレイ検知）。
    seen_jti: Mutex<HashSet<String>>,
}

impl MemorySessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(tenant_id: &str, session_id: &str) -> String {
        format!("{tenant_id}:{session_id}")
    }
}

#[async_trait]
impl SessionStore for MemorySessionStore {
    async fn put(
        &self,
        tenant_id: &str,
        session_id: &str,
        record: &SessionRecord,
        _ttl: Duration,
    ) -> Result<(), SessionError> {
        self.inner
            .lock()
            .unwrap()
            .insert(Self::key(tenant_id, session_id), record.clone());
        Ok(())
    }

    async fn update_if_present(
        &self,
        tenant_id: &str,
        session_id: &str,
        record: &SessionRecord,
        _ttl: Duration,
    ) -> Result<bool, SessionError> {
        use std::collections::hash_map::Entry;
        let mut guard = self.inner.lock().unwrap();
        match guard.entry(Self::key(tenant_id, session_id)) {
            Entry::Occupied(mut e) => {
                e.insert(record.clone());
                Ok(true)
            }
            Entry::Vacant(_) => Ok(false),
        }
    }

    async fn get(
        &self,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>, SessionError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&Self::key(tenant_id, session_id))
            .cloned())
    }

    async fn delete(&self, tenant_id: &str, session_id: &str) -> Result<(), SessionError> {
        self.inner
            .lock()
            .unwrap()
            .remove(&Self::key(tenant_id, session_id));
        Ok(())
    }

    async fn delete_tenant(&self, tenant_id: &str) -> Result<u64, SessionError> {
        let prefix = format!("{tenant_id}:");
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|k, _| !k.starts_with(&prefix));
        Ok((before - guard.len()) as u64)
    }

    async fn delete_by_subject(&self, sub: &str) -> Result<u64, SessionError> {
        // テスト実装はレコードを走査して sub 一致を削除する（Redis 実装は逆引きインデックス）。
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|_, r| r.principal.id != sub);
        Ok((before - guard.len()) as u64)
    }

    async fn delete_by_sid(&self, sid: &str) -> Result<u64, SessionError> {
        let mut guard = self.inner.lock().unwrap();
        let before = guard.len();
        guard.retain(|_, r| r.keycloak_sid.as_deref() != Some(sid));
        Ok((before - guard.len()) as u64)
    }

    async fn register_jti(&self, jti: &str, _ttl: Duration) -> Result<bool, SessionError> {
        // TTL は無視し、プロセス生存中の重複のみ検知する（テスト用）。
        Ok(self.seen_jti.lock().unwrap().insert(jti.to_string()))
    }
}
