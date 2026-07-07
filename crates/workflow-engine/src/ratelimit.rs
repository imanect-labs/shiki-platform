//! Redis トークンバケット（レート制限・engine.md §8.3）。
//!
//! 外部 API 呼び出し（llm.invoke / http.request 等）の秒間レートを分散環境で守る。1 本の Lua
//! スクリプトでトークン補充＋消費をアトミックに行い、複数ワーカー間で race しない。取れなければ
//! `RateLimited`（呼び出し側は attempt 非消費で再試行）。

use redis::aio::ConnectionManager;
use redis::AsyncCommands;

/// バケット設定（容量＝バースト上限・毎秒補充レート）。
#[derive(Debug, Clone, Copy)]
pub struct BucketConfig {
    /// バケット容量（バースト上限）。
    pub capacity: u32,
    /// 毎秒の補充トークン数。
    pub refill_per_sec: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    #[error("redis エラー: {0}")]
    Redis(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_redis(e: redis::RedisError) -> RateLimitError {
    RateLimitError::Redis(format!("{e}"))
}

/// トークンバケット（Redis 上・キーごとに独立）。
#[derive(Clone)]
pub struct TokenBucket {
    conn: ConnectionManager,
}

/// 補充＋消費をアトミックに行う Lua（`now` はサーバ TIME を使い時計ずれを避ける）。
///
/// KEYS[1]=バケットキー / ARGV: capacity, refill_per_sec, cost。
/// 返り値: `{allowed(1|0), remaining_tokens}`。
const BUCKET_LUA: &str = r"
local key = KEYS[1]
local capacity = tonumber(ARGV[1])
local refill = tonumber(ARGV[2])
local cost = tonumber(ARGV[3])
local t = redis.call('TIME')
local now = tonumber(t[1]) + tonumber(t[2]) / 1000000
local data = redis.call('HMGET', key, 'tokens', 'ts')
local tokens = tonumber(data[1])
local ts = tonumber(data[2])
if tokens == nil then
  tokens = capacity
  ts = now
end
local elapsed = math.max(0, now - ts)
tokens = math.min(capacity, tokens + elapsed * refill)
local allowed = 0
if tokens >= cost then
  tokens = tokens - cost
  allowed = 1
end
redis.call('HSET', key, 'tokens', tokens, 'ts', now)
-- TTL は満充填にかかる時間の 2 倍（放置キーを掃除）。
local ttl = math.ceil(capacity / math.max(refill, 0.001)) * 2 + 1
redis.call('EXPIRE', key, ttl)
return {allowed, math.floor(tokens)}
";

impl TokenBucket {
    pub fn new(conn: ConnectionManager) -> Self {
        TokenBucket { conn }
    }

    /// `cost` トークンを消費しようと試みる。true=許可（消費済み）、false=レート超過。
    pub async fn try_acquire(
        &self,
        key: &str,
        cfg: BucketConfig,
        cost: u32,
    ) -> Result<bool, RateLimitError> {
        let mut conn = self.conn.clone();
        let redis_key = format!("wf:ratelimit:{key}");
        let res: Vec<i64> = redis::Script::new(BUCKET_LUA)
            .key(&redis_key)
            .arg(cfg.capacity)
            .arg(cfg.refill_per_sec)
            .arg(cost)
            .invoke_async(&mut conn)
            .await
            .map_err(map_redis)?;
        Ok(res.first().copied() == Some(1))
    }

    /// 現在の残トークン（監視・テスト補助）。
    pub async fn remaining(&self, key: &str) -> Result<Option<f64>, RateLimitError> {
        let mut conn = self.conn.clone();
        let redis_key = format!("wf:ratelimit:{key}");
        let tokens: Option<String> = conn.hget(&redis_key, "tokens").await.map_err(map_redis)?;
        Ok(tokens.and_then(|s| s.parse().ok()))
    }
}
