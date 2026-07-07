//! Redis pub/sub（best-effort の起床通知）。DB=真実のソースの補助であり、
//! publish 失敗・購読失敗はいずれも致命ではない（呼び出し側は DB replay で補填する）。

use redis::aio::{ConnectionManager, PubSubStream};

/// Redis pub/sub（publish 用の多重接続＋subscribe 用のクライアント）。
///
/// Clone は安価（`ConnectionManager` / `Client` は共有ハンドル）。
#[derive(Clone)]
pub struct RedisPubSub {
    manager: ConnectionManager,
    client: redis::Client,
}

impl RedisPubSub {
    /// URL から接続する。失敗はそのまま返す（有効化するかはドメイン側の判断）。
    pub async fn connect(url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(url)?;
        let manager = client.get_connection_manager().await?;
        Ok(RedisPubSub { manager, client })
    }

    /// チャネルへ publish する（best-effort・失敗は警告のみ）。
    pub async fn publish_best_effort(&self, channel: &str, payload: &str) {
        let mut conn = self.manager.clone();
        if let Err(e) = redis::AsyncCommands::publish::<_, _, ()>(&mut conn, channel, payload).await
        {
            tracing::warn!(error = %e, channel, "redis publish failed (best-effort)");
        }
    }

    /// チャネルを購読しメッセージストリームを返す（best-effort・失敗は `None`）。
    ///
    /// `None` でも呼び出し側は DB replay の短周期ポーリングで動作する。
    pub async fn subscribe(&self, channel: &str) -> Option<PubSubStream> {
        match self.client.get_async_pubsub().await {
            Ok(mut ps) => {
                if ps.subscribe(channel).await.is_ok() {
                    Some(ps.into_on_message())
                } else {
                    None
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "redis subscribe failed; DB polling only");
                None
            }
        }
    }
}
