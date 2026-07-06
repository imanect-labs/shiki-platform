//! 生成済みサンドボックスのレジストリ（SandboxId→インスタンス・TTL 掃除）。
//!
//! per-sandbox の分離を担保する単一の所有点。TTL 超過分は掃除タスクが destroy する（残留無し）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::backend::Instance;

struct Entry {
    instance: Arc<dyn Instance>,
    deadline: Instant,
    tenant_id: String,
}

/// SandboxId → インスタンスの対応表。`Mutex<HashMap>` で保護。
#[derive(Default)]
pub struct Registry {
    inner: Mutex<HashMap<String, Entry>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// インスタンスを登録し ID を返す。
    pub async fn insert(
        &self,
        id: String,
        instance: Arc<dyn Instance>,
        ttl: Duration,
        tenant_id: String,
    ) {
        let deadline = Instant::now() + ttl;
        self.inner.lock().await.insert(
            id,
            Entry {
                instance,
                deadline,
                tenant_id,
            },
        );
    }

    /// ID からインスタンスを引く（見つからなければ None）。
    pub async fn get(&self, id: &str) -> Option<Arc<dyn Instance>> {
        self.inner
            .lock()
            .await
            .get(id)
            .map(|e| Arc::clone(&e.instance))
    }

    /// テナントを取得（監査用）。
    pub async fn tenant_of(&self, id: &str) -> Option<String> {
        self.inner.lock().await.get(id).map(|e| e.tenant_id.clone())
    }

    /// ID を登録解除しインスタンスを返す（destroy 呼び出し用）。
    pub async fn remove(&self, id: &str) -> Option<Arc<dyn Instance>> {
        self.inner.lock().await.remove(id).map(|e| e.instance)
    }

    /// 現在の登録数（テスト・メトリクス用）。
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    /// 登録が空か。
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// 期限切れのインスタンスを取り出す（掃除タスクが destroy する）。
    pub async fn drain_expired(&self, now: Instant) -> Vec<(String, Arc<dyn Instance>)> {
        let mut guard = self.inner.lock().await;
        let expired: Vec<String> = guard
            .iter()
            .filter(|(_, e)| e.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|id| guard.remove(&id).map(|e| (id, e.instance)))
            .collect()
    }
}

/// TTL 掃除ループ（超過サンドボックスを destroy して残留を防ぐ）。
pub async fn sweep_loop(registry: Arc<Registry>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let expired = registry.drain_expired(Instant::now()).await;
        for (id, instance) in expired {
            if let Err(e) = instance.destroy().await {
                tracing::warn!(sandbox_id = %id, error = %e, "TTL destroy failed");
            } else {
                tracing::debug!(sandbox_id = %id, "TTL destroyed sandbox");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::{FakeBackend, FakeExec};
    use crate::backend::Backend;
    use sandbox_client::{ExecEvent, SandboxSpec};

    async fn instance() -> Arc<dyn Instance> {
        let backend = FakeBackend::new().with_exec(FakeExec {
            events: vec![ExecEvent::Exited { code: 0 }],
            artifacts: Vec::new(),
        });
        backend
            .create(SandboxSpec::code_interpreter(
                "t".into(),
                "o".into(),
                "u:1".into(),
            ))
            .await
            .expect("create")
    }

    #[tokio::test]
    async fn insert_get_remove() {
        let reg = Registry::new();
        reg.insert(
            "s1".into(),
            instance().await,
            Duration::from_mins(1),
            "t".into(),
        )
        .await;
        assert_eq!(reg.len().await, 1);
        assert!(reg.get("s1").await.is_some());
        assert_eq!(reg.tenant_of("s1").await.as_deref(), Some("t"));
        assert!(reg.remove("s1").await.is_some());
        assert_eq!(reg.len().await, 0);
    }

    #[tokio::test]
    async fn drains_expired_only() {
        let reg = Registry::new();
        reg.insert(
            "live".into(),
            instance().await,
            Duration::from_mins(10),
            "t".into(),
        )
        .await;
        reg.insert("dead".into(), instance().await, Duration::ZERO, "t".into())
            .await;
        // 0ms TTL は即期限切れ。
        let expired = reg.drain_expired(Instant::now()).await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, "dead");
        assert_eq!(reg.len().await, 1);
        assert!(reg.get("live").await.is_some());
    }
}
