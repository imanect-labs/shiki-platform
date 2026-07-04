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
/// backchannel logout 逆引きインデックスの接頭辞（`sub`/`sid` → セッション集合）。
const SUB_IDX_PREFIX: &str = "shiki:sess:idx:sub";
const SID_IDX_PREFIX: &str = "shiki:sess:idx:sid";

/// Redis の glob メタ文字（`* ? [ ] \`）をエスケープする（MATCH パターンへの注入防止）。
fn escape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

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

    /// backchannel logout 逆引きインデックスのメンバー値（`{tenant}:{session_id}`）。
    /// logout_token はテナントを持たないため、メンバーにテナントを埋めてテナント横断で辿る。
    fn index_member(tenant_id: &str, session_id: &str) -> String {
        format!("{tenant_id}:{session_id}")
    }

    /// `sub`/`sid` 逆引きインデックスにセッションを登録し、TTL を張り直す。
    ///
    /// メンバーは個別 SREM せず、インデックスキー自体の TTL（セッションと同値・put 毎に更新）で
    /// 束ごと期限切れさせる。失効済みセッションのメンバーが一時的に残っても、
    /// [`delete_by_subject`]/[`delete_by_sid`] は存在しないセッション削除を no-op として扱うため無害。
    ///
    /// [`delete_by_subject`]: SessionStore::delete_by_subject
    /// [`delete_by_sid`]: SessionStore::delete_by_sid
    async fn index_session(
        &self,
        record: &SessionRecord,
        session_id: &str,
        ttl: Duration,
    ) -> Result<(), SessionError> {
        let member = Self::index_member(&record.tenant_id, session_id);
        let mut conn = self.conn.clone();
        let sub_key = format!("{SUB_IDX_PREFIX}:{}", record.principal.id);
        let _: () = conn
            .sadd(&sub_key, &member)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        let _: () = conn
            .expire(&sub_key, ttl.as_secs() as i64)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        if let Some(sid) = &record.keycloak_sid {
            let sid_key = format!("{SID_IDX_PREFIX}:{sid}");
            let _: () = conn
                .sadd(&sid_key, &member)
                .await
                .map_err(|e| SessionError::Backend(e.to_string()))?;
            let _: () = conn
                .expire(&sid_key, ttl.as_secs() as i64)
                .await
                .map_err(|e| SessionError::Backend(e.to_string()))?;
        }
        Ok(())
    }

    /// 逆引きインデックスのメンバー集合を辿り、各セッションを削除する（backchannel logout 共通処理）。
    async fn delete_by_index(&self, index_key: &str) -> Result<u64, SessionError> {
        let mut conn = self.conn.clone();
        let members: Vec<String> = conn
            .smembers(index_key)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        let mut deleted: u64 = 0;
        for member in &members {
            // メンバーは `{tenant}:{session_id}`。tenant/session_id とも `:` を含まない
            // （tenant は禁止文字検証済み、session_id は base64url 乱数）ため最初の `:` で分割する。
            let Some((tenant_id, session_id)) = member.split_once(':') else {
                continue;
            };
            let n: u64 = conn
                .del(Self::key(tenant_id, session_id))
                .await
                .map_err(|e| SessionError::Backend(e.to_string()))?;
            deleted += n;
        }
        // インデックスキーごと破棄（メンバーは処理済み・冪等）。
        let _: () = conn
            .del(index_key)
            .await
            .map_err(|e| SessionError::Backend(e.to_string()))?;
        Ok(deleted)
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
        // backchannel logout 用に sub/sid 逆引きインデックスへ登録する。
        self.index_session(record, session_id, ttl).await?;
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
        let updated = result.is_some();
        // 既存セッションを更新できた時のみ逆引きインデックスを張り直す（refresh で sid/TTL 追従）。
        if updated {
            self.index_session(record, session_id, ttl).await?;
        }
        Ok(updated)
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

    async fn delete_tenant(&self, tenant_id: &str) -> Result<u64, SessionError> {
        // KEYS はブロッキングのため使わず、SCAN でカーソル走査して UNLINK（非同期解放）。
        // tenant_id に glob メタ文字（`*` `?` `[` `]`）が含まれても他テナントのキーへ
        // 展開されないよう必ずエスケープする（`prod*` が `prod-a` に一致する越境を防ぐ）。
        let pattern = format!("{KEY_PREFIX}:{}:*", escape_glob(tenant_id));
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        let mut deleted: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut conn)
                .await
                .map_err(|e| SessionError::Backend(e.to_string()))?;
            if !keys.is_empty() {
                let n: u64 = redis::cmd("UNLINK")
                    .arg(&keys)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| SessionError::Backend(e.to_string()))?;
                deleted += n;
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(deleted)
    }

    async fn delete_by_subject(&self, sub: &str) -> Result<u64, SessionError> {
        self.delete_by_index(&format!("{SUB_IDX_PREFIX}:{sub}"))
            .await
    }

    async fn delete_by_sid(&self, sid: &str) -> Result<u64, SessionError> {
        self.delete_by_index(&format!("{SID_IDX_PREFIX}:{sid}"))
            .await
    }
}
