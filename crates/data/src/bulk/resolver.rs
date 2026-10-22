//! 検証 1 チャンクの間だけ答えを覚える参照整合の解決器（ユーザー・ロール・ファイル）。
//!
//! 一括取込の検証は通常書込と**同じ検証器**（[`crate::validate::validate_record_data`]）を通すが、
//! 担当者列のように同じ値が何百行にも現れるので、そのまま呼ぶと同じ問いを行数ぶん投げる。
//! ここでは 1 チャンク（同じ主体・数百行・数秒）の中でだけ答えを覚える。チャンクの外へは
//! 持ち出さない（メモはチャンクごとに作り直す・PIT-18 のリクエストスコープと同じ扱い）。
//! 失敗（`Err`）は覚えない。

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use authz::AuthContext;
use uuid::Uuid;

use crate::validate::RefResolver;

pub(super) struct ChunkResolver<'a> {
    inner: &'a dyn RefResolver,
    users: Mutex<HashMap<String, bool>>,
    roles: Mutex<HashMap<String, bool>>,
    files: Mutex<HashMap<Uuid, bool>>,
}

impl<'a> ChunkResolver<'a> {
    pub(super) fn new(inner: &'a dyn RefResolver) -> Self {
        ChunkResolver {
            inner,
            users: Mutex::new(HashMap::new()),
            roles: Mutex::new(HashMap::new()),
            files: Mutex::new(HashMap::new()),
        }
    }
}

fn cached<K: std::hash::Hash + Eq>(m: &Mutex<HashMap<K, bool>>, k: &K) -> Option<bool> {
    m.lock().ok().and_then(|g| g.get(k).copied())
}

fn remember<K: std::hash::Hash + Eq>(m: &Mutex<HashMap<K, bool>>, k: K, v: bool) {
    if let Ok(mut g) = m.lock() {
        g.insert(k, v);
    }
}

#[async_trait]
impl RefResolver for ChunkResolver<'_> {
    async fn user_exists(&self, ctx: &AuthContext, user_id: &str) -> Result<bool, String> {
        let key = user_id.to_string();
        if let Some(v) = cached(&self.users, &key) {
            return Ok(v);
        }
        let v = self.inner.user_exists(ctx, user_id).await?;
        remember(&self.users, key, v);
        Ok(v)
    }

    async fn role_exists(&self, ctx: &AuthContext, role_id: &str) -> Result<bool, String> {
        let key = role_id.to_string();
        if let Some(v) = cached(&self.roles, &key) {
            return Ok(v);
        }
        let v = self.inner.role_exists(ctx, role_id).await?;
        remember(&self.roles, key, v);
        Ok(v)
    }

    async fn file_readable(&self, ctx: &AuthContext, file_id: Uuid) -> Result<bool, String> {
        if let Some(v) = cached(&self.files, &file_id) {
            return Ok(v);
        }
        let v = self.inner.file_readable(ctx, file_id).await?;
        remember(&self.files, file_id, v);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct Counting(AtomicUsize);

    #[async_trait]
    impl RefResolver for Counting {
        async fn user_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if id == "boom" {
                return Err("down".into());
            }
            Ok(id == "alice")
        }
        async fn role_exists(&self, _: &AuthContext, id: &str) -> Result<bool, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(id == "sales")
        }
        async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(false)
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: None,
            },
            "acme".into(),
            "t".into(),
        )
    }

    #[tokio::test]
    async fn answers_are_remembered_within_the_chunk_but_errors_are_not() {
        let inner = Counting(AtomicUsize::new(0));
        let r = ChunkResolver::new(&inner);
        let c = ctx();
        for _ in 0..3 {
            assert_eq!(r.user_exists(&c, "alice").await, Ok(true));
            assert_eq!(r.user_exists(&c, "bob").await, Ok(false));
            assert_eq!(r.role_exists(&c, "sales").await, Ok(true));
            assert_eq!(r.file_readable(&c, Uuid::nil()).await, Ok(false));
        }
        assert_eq!(inner.0.load(Ordering::SeqCst), 4, "同じ問いは 1 回だけ");
        assert!(r.user_exists(&c, "boom").await.is_err());
        assert!(r.user_exists(&c, "boom").await.is_err());
        assert_eq!(inner.0.load(Ordering::SeqCst), 6, "失敗は覚えない");
    }
}
