//! Redis を backend とする [`SessionStore`] 実装（プール型・`tenant_id` キースコープ）。
//!
//! キーは `shiki:sess:{tenant_id}:{session_id}`。全テナント共用の単一 Redis でも、
//! キーのテナントスコープで論理分離する（docs/auth/browser-token-strategy.md §7.2）。

use std::time::Duration;

use async_trait::async_trait;
use redis::{aio::ConnectionManager, AsyncCommands};

use super::store::{SessionError, SessionRecord, SessionStore};

/// セッションキーの接頭辞。
const KEY_PREFIX: &str = "shiki:sess";

pub struct RedisSessionStore {
    conn: ConnectionManager,
}

impl RedisSessionStore {
    /// Redis に接続し、自動再接続する [`ConnectionManager`] を用意する。
    pub async fn connect(redis_url: &str) -> Result<Self, SessionError> {
        let client =
            redis::Client::open(redis_url).map_err(|e| SessionError::Backend(e.to_string()))?;
        let conn = client
            .get_connection_manager()
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        Ok(RedisSessionStore { conn })
    }

    /// `tenant_id` スコープのキーを組み立てる。
    fn key(tenant_id: &str, session_id: &str) -> String {
        format!("{KEY_PREFIX}:{tenant_id}:{session_id}")
    }
}

#[async_trait]
impl SessionStore for RedisSessionStore {
    async fn put(
        &self,
        tenant_id: &str,
        session_id: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<(), SessionError> {
        let json = serde_json::to_string(record).map_err(|e| SessionError::Serde(e.to_string()))?;
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(Self::key(tenant_id, session_id), json, ttl.as_secs())
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn update_if_present(
        &self,
        tenant_id: &str,
        session_id: &str,
        record: &SessionRecord,
        ttl: Duration,
    ) -> Result<bool, SessionError> {
        let json = serde_json::to_string(record).map_err(|e| SessionError::Serde(e.to_string()))?;
        let mut conn = self.conn.clone();
        // SET key val EX <ttl> XX: キーが既に存在する時のみ書き込む（無ければ nil を返す）。
        let result: Option<String> = redis::cmd("SET")
            .arg(Self::key(tenant_id, session_id))
            .arg(json)
            .arg("EX")
            .arg(ttl.as_secs())
            .arg("XX")
            .query_async(&mut conn)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        Ok(result.is_some())
    }

    async fn get(
        &self,
        tenant_id: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>, SessionError> {
        let mut conn = self.conn.clone();
        let value: Option<String> = conn
            .get(Self::key(tenant_id, session_id))
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        match value {
            Some(json) => {
                let record =
                    serde_json::from_str(&json).map_err(|e| SessionError::Serde(e.to_string()))?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    async fn delete(&self, tenant_id: &str, session_id: &str) -> Result<(), SessionError> {
        let mut conn = self.conn.clone();
        let _: () = conn
            .del(Self::key(tenant_id, session_id))
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        Ok(())
    }
}
