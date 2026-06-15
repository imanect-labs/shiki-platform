//! JWKS（JSON Web Key Set）の取得とキャッシュ（docs/roadmap phase-0 Task 0.4）。
//!
//! - TTL でキャッシュし、失効したら再取得する。
//! - 未知 kid を受けたら 1 回だけ強制再取得する（鍵ローテーション追従）。
//! - ただし直近の取得から `negative_throttle` 以内は再取得しない（JWKS への
//!   DoS 増幅を防ぐ）。
//! - 取得は single-flight 化し、起動直後の thundering herd を避ける。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use jsonwebtoken::{
    jwk::{Jwk, JwkSet},
    DecodingKey,
};
use tokio::sync::{Mutex, RwLock};

use super::claims::AuthError;

struct CachedJwks {
    keys: HashMap<String, Jwk>,
    fetched_at: Option<Instant>,
}

pub struct JwksCache {
    http: reqwest::Client,
    jwks_uri: String,
    ttl: Duration,
    negative_throttle: Duration,
    inner: RwLock<CachedJwks>,
    refresh_lock: Mutex<()>,
}

impl JwksCache {
    pub fn new(http: reqwest::Client, jwks_uri: String, ttl: Duration) -> Self {
        JwksCache {
            http,
            jwks_uri,
            ttl,
            // 未知 kid の連打でも JWKS を叩きすぎない下限間隔。
            negative_throttle: Duration::from_secs(10),
            inner: RwLock::new(CachedJwks {
                keys: HashMap::new(),
                fetched_at: None,
            }),
            refresh_lock: Mutex::new(()),
        }
    }

    /// kid に対応する検証鍵を返す。必要なら JWKS を取得し直す。
    pub async fn key_for_kid(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        // 1. 高速パス: キャッシュが有効かつ kid が存在。
        {
            let cache = self.inner.read().await;
            if let (Some(jwk), false) = (cache.keys.get(kid), self.is_expired(&cache)) {
                return decode_key(jwk);
            }
        }

        // 2. 再取得（single-flight）。
        let _guard = self.refresh_lock.lock().await;
        // ロック獲得までの間に他タスクが更新済みかもしれないので再確認。
        {
            let cache = self.inner.read().await;
            if let Some(jwk) = cache.keys.get(kid) {
                if !self.is_expired(&cache) {
                    return decode_key(jwk);
                }
            }
            // throttle: 直近取得済みで kid 不在なら、再取得せず即エラー。
            if let Some(fetched_at) = cache.fetched_at {
                if fetched_at.elapsed() < self.negative_throttle && !cache.keys.contains_key(kid) {
                    return Err(AuthError::UnknownKid);
                }
            }
        }

        let jwks = self.fetch().await?;
        let mut keys = HashMap::new();
        for jwk in jwks.keys {
            if let Some(k) = jwk.common.key_id.clone() {
                keys.insert(k, jwk);
            }
        }
        let key = keys.get(kid).map(decode_key);
        {
            let mut cache = self.inner.write().await;
            cache.keys = keys;
            cache.fetched_at = Some(Instant::now());
        }
        key.unwrap_or(Err(AuthError::UnknownKid))
    }

    fn is_expired(&self, cache: &CachedJwks) -> bool {
        match cache.fetched_at {
            None => true,
            Some(at) => at.elapsed() >= self.ttl,
        }
    }

    async fn fetch(&self) -> Result<JwkSet, AuthError> {
        let resp = self
            .http
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| AuthError::JwksFetch(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(AuthError::JwksFetch(format!("status {}", resp.status())));
        }
        resp.json::<JwkSet>()
            .await
            .map_err(|e| AuthError::JwksFetch(e.to_string()))
    }
}

fn decode_key(jwk: &Jwk) -> Result<DecodingKey, AuthError> {
    DecodingKey::from_jwk(jwk).map_err(|e| AuthError::JwksFetch(format!("from_jwk: {e}")))
}
