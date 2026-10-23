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

/// テナント束縛つきの引き当て結果（#810）。
pub enum Lookup<T> {
    /// 登録済みかつテナント一致。
    Found(T),
    /// 未登録（破棄済み・TTL 掃除済みを含む）。
    Missing,
    /// 登録済みだが別テナントの所有（越境の試行。監査ログに残す）。
    Foreign,
}

impl<T> Lookup<T> {
    /// 一致したときだけ値を返す（Missing / Foreign は区別せず None）。
    pub fn found(self) -> Option<T> {
        match self {
            Lookup::Found(v) => Some(v),
            Lookup::Missing | Lookup::Foreign => None,
        }
    }
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

    /// インスタンスを ID で登録する。
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

    /// ID とテナントの組でインスタンスを引く（#810）。
    ///
    /// 未登録は [`Lookup::Missing`]、テナント不一致は [`Lookup::Foreign`] として返すが、
    /// 呼び出し側（gRPC 層）はクライアントへ**区別せず** not_found を返すこと（存在を漏らさない）。
    /// 区別は監査ログのためだけにある。ID だけで引く経路は意図的に公開しない。
    pub async fn get(&self, id: &str, tenant_id: &str) -> Lookup<Arc<dyn Instance>> {
        match self.inner.lock().await.get(id) {
            None => Lookup::Missing,
            Some(e) if e.tenant_id != tenant_id => Lookup::Foreign,
            Some(e) => Lookup::Found(Arc::clone(&e.instance)),
        }
    }

    /// ID とテナントの組が一致したときだけ登録解除しインスタンスを返す（destroy RPC 用・#810）。
    /// 不一致なら何も消さない（他テナントのサンドボックスを破棄させない）。
    pub async fn remove_owned(&self, id: &str, tenant_id: &str) -> Lookup<Arc<dyn Instance>> {
        let mut guard = self.inner.lock().await;
        match guard.get(id) {
            None => Lookup::Missing,
            Some(e) if e.tenant_id != tenant_id => Lookup::Foreign,
            Some(_) => guard
                .remove(id)
                .map_or(Lookup::Missing, |e| Lookup::Found(e.instance)),
        }
    }

    /// ID を登録解除しインスタンスを返す（orchestrator 内部の破棄用。テナント照合済みの exec
    /// ストリームの上限超過破棄など、外部入力の ID をそのまま渡さない経路に限る）。
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
            // 破棄が hang しても掃除ループを止めない（子プロセスは kill_on_drop で最終的に回収される）。
            match tokio::time::timeout(Duration::from_secs(10), instance.destroy()).await {
                Ok(Ok(())) => tracing::debug!(sandbox_id = %id, "TTL destroyed sandbox"),
                Ok(Err(e)) => tracing::warn!(sandbox_id = %id, error = %e, "TTL destroy failed"),
                Err(_) => tracing::warn!(sandbox_id = %id, "TTL destroy timed out"),
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
                sandbox_client::SandboxBackend::Wasm,
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
        assert!(reg.get("s1", "t").await.found().is_some());
        assert!(reg.remove("s1").await.is_some());
        assert_eq!(reg.len().await, 0);
    }

    #[tokio::test]
    async fn get_is_bound_to_tenant() {
        let reg = Registry::new();
        reg.insert(
            "s1".into(),
            instance().await,
            Duration::from_mins(1),
            "tenant-a".into(),
        )
        .await;
        assert!(matches!(reg.get("s1", "tenant-a").await, Lookup::Found(_)));
        // 別テナント・空テナント・未登録 ID は引けない。
        assert!(matches!(reg.get("s1", "tenant-b").await, Lookup::Foreign));
        assert!(matches!(reg.get("s1", "").await, Lookup::Foreign));
        assert!(matches!(reg.get("nope", "tenant-a").await, Lookup::Missing));
        assert!(reg.get("s1", "tenant-b").await.found().is_none());
    }

    #[tokio::test]
    async fn remove_owned_keeps_foreign_entry() {
        let reg = Registry::new();
        reg.insert(
            "s1".into(),
            instance().await,
            Duration::from_mins(1),
            "tenant-a".into(),
        )
        .await;
        // 別テナントからの破棄は何も消さない。
        assert!(matches!(
            reg.remove_owned("s1", "tenant-b").await,
            Lookup::Foreign
        ));
        assert_eq!(reg.len().await, 1);
        assert!(matches!(
            reg.remove_owned("nope", "tenant-a").await,
            Lookup::Missing
        ));
        // 所有テナントなら消える。
        assert!(reg.remove_owned("s1", "tenant-a").await.found().is_some());
        assert!(reg.is_empty().await);
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
        assert!(reg.get("live", "t").await.found().is_some());
    }
}
