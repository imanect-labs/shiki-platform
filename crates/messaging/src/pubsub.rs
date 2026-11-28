//! 配信の起床通知（SSE ＋ Redis pub/sub・design §4.14 配信）。
//!
//! - **トピック名は [`topic`] だけが組む**（`{tenant_id}:channel:{id}`・PIT-66）。文字列連結を散らさない。
//! - 通知は「変わった」という合図だけで本文を運ばない。受け手は必ず DB を自分の `AuthContext` で
//!   読み直す（[`crate::store_message::fetch_messages`]）ので、合図が漏れても本文は漏れない。
//! - プロセス内は 1 本の broadcast で配り、レプリカ間は Redis の 1 本の pattern 購読で中継する
//!   （1,000 接続で Redis の接続を 1,000 本張らない）。Redis が無い・落ちても DB のポーリングで届く。

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

/// 配信の合図。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Signal {
    /// 発言が増えた・変わった（本文は運ばない）。
    Changed,
    /// 参加者が外れた（購読中の接続は権限を引き直し、外れていれば閉じる・PIT-64）。
    Leave { user_id: String },
    /// チャンネルの設定（名前・アーカイブ）が変わった。
    Channel,
    /// その利用者だけに見せるエージェントの回答が増えた（受け手が本人なら読み直す）。
    AgentReply { user_id: String },
}

/// チャンネルのトピック名（`{tenant_id}:channel:{id}`）。**唯一の組み立て関数**（PIT-66）。
#[must_use]
pub fn topic(tenant_id: &str, channel: Uuid) -> String {
    format!("{tenant_id}:channel:{channel}")
}

#[derive(Serialize, Deserialize)]
struct Wire {
    origin: Uuid,
    signal: Signal,
}

/// プロセス内の配信ハブ（Redis はレプリカ間の中継だけ）。
#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<(String, Signal)>,
    redis: Option<redis::aio::ConnectionManager>,
    origin: Uuid,
}

impl Default for Hub {
    fn default() -> Self {
        Self::local()
    }
}

impl Hub {
    /// Redis 無し（単一プロセス・テスト）。
    #[must_use]
    pub fn local() -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self {
            tx,
            redis: None,
            origin: Uuid::new_v4(),
        }
    }

    /// Redis でレプリカ間を中継する。接続できなければ警告してプロセス内だけで動く。
    pub async fn connect(url: &str) -> Self {
        let mut hub = Self::local();
        let client = match redis::Client::open(url) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "messaging: redis URL が不正です（プロセス内配信のみ）");
                return hub;
            }
        };
        match client.get_connection_manager().await {
            Ok(m) => hub.redis = Some(m),
            Err(e) => {
                tracing::warn!(error = %e, "messaging: redis に接続できません（プロセス内配信のみ）");
                return hub;
            }
        }
        let tx = hub.tx.clone();
        let origin = hub.origin;
        tokio::spawn(async move {
            loop {
                if let Err(e) = relay(&client, &tx, origin).await {
                    tracing::warn!(error = %e, "messaging: redis の購読が切れました（再接続します）");
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
        hub
    }

    /// 合図を送る（プロセス内へ即時・Redis へは best-effort）。
    pub async fn publish(&self, tenant_id: &str, channel: Uuid, signal: Signal) {
        let topic = topic(tenant_id, channel);
        let _ = self.tx.send((topic.clone(), signal.clone()));
        if let Some(mut conn) = self.redis.clone() {
            let payload = serde_json::to_string(&Wire {
                origin: self.origin,
                signal,
            })
            .unwrap_or_default();
            if let Err(e) =
                redis::AsyncCommands::publish::<_, _, ()>(&mut conn, &topic, payload).await
            {
                tracing::warn!(error = %e, "messaging: redis publish に失敗（DB ポーリングで届く）");
            }
        }
    }

    /// 購読する（受け手は自分のトピックだけを見る・[`Subscription::recv`]）。
    #[must_use]
    pub fn subscribe(&self, tenant_id: &str, channel: Uuid) -> Subscription {
        Subscription {
            topic: topic(tenant_id, channel),
            rx: self.tx.subscribe(),
        }
    }
}

async fn relay(
    client: &redis::Client,
    tx: &broadcast::Sender<(String, Signal)>,
    origin: Uuid,
) -> Result<(), redis::RedisError> {
    let mut ps = client.get_async_pubsub().await?;
    ps.psubscribe("*:channel:*").await?;
    let mut stream = ps.into_on_message();
    while let Some(msg) = stream.next().await {
        let topic = msg.get_channel_name().to_string();
        let Ok(payload) = msg.get_payload::<String>() else {
            continue;
        };
        let Ok(wire) = serde_json::from_str::<Wire>(&payload) else {
            continue;
        };
        if wire.origin != origin {
            let _ = tx.send((topic, wire.signal));
        }
    }
    Ok(())
}

/// 1 チャンネルの購読。
pub struct Subscription {
    topic: String,
    rx: broadcast::Receiver<(String, Signal)>,
}

impl Subscription {
    /// 自分のトピックの次の合図。取りこぼし（遅延）は `Changed` として返す（DB を読み直させる）。
    /// ハブが閉じたら `None`。
    pub async fn recv(&mut self) -> Option<Signal> {
        loop {
            match self.rx.recv().await {
                Ok((topic, signal)) if topic == self.topic => return Some(signal),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return Some(Signal::Changed),
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_always_carries_the_tenant() {
        let id = Uuid::nil();
        assert_eq!(
            topic("t1", id),
            "t1:channel:00000000-0000-0000-0000-000000000000"
        );
        assert_ne!(topic("t1", id), topic("t2", id));
    }

    #[tokio::test]
    async fn same_channel_id_in_another_tenant_does_not_cross() {
        let hub = Hub::local();
        let id = Uuid::new_v4();
        let mut a = hub.subscribe("tenant-a", id);
        let mut b = hub.subscribe("tenant-b", id);
        hub.publish("tenant-b", id, Signal::Changed).await;
        hub.publish("tenant-a", id, Signal::Channel).await;
        // a は自テナントの合図だけを受け、b の合図は飛ばす。
        assert_eq!(a.recv().await, Some(Signal::Channel));
        assert_eq!(b.recv().await, Some(Signal::Changed));
    }
}
