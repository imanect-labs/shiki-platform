//! ミニアプリ基盤の結合テスト（Task 9.1 / 9.13a 受け入れ条件）。
//!
//! マニフェスト保存（語彙照合）・不変バージョン・publish 不変性・A/B 同一経路を検証する。
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。authz はモック（AllowAll）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use app_platform::{MiniAppCodeStore, MiniAppManifest, Registry, TrustTier};
use artifact::{ArtifactKind, ArtifactStore};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

struct AllowAll;
#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

async fn setup() -> Option<PgPool> {
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn manifest(name: &str, version: &str) -> MiniAppManifest {
    MiniAppManifest {
        name: name.into(),
        version: version.into(),
        description: "経費申請アプリ".into(),
        requested_scopes: vec![
            "data.read".into(),
            "data.write".into(),
            "agent.invoke".into(),
        ],
        tools: vec!["doc_search".into()],
        tables: vec![],
        workflows: vec![],
        budget: app_platform::Budget::default(),
        frontend: None,
        server: None,
        trust_tier: TrustTier::InHouse,
    }
}

fn store(pool: PgPool) -> MiniAppCodeStore {
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::new(AllowAll)));
    MiniAppCodeStore::new(artifacts, Registry::new(pool))
}

#[tokio::test]
async fn manifest_versions_immutable_and_vocab_checked() {
    let Some(pool) = setup().await else { return };
    let s = store(pool);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant, "alice");

    // 語彙照合: 未知スコープ/ツールは拒否。
    let mut bad = manifest("bad-app", "1.0.0");
    bad.requested_scopes.push("storage.delete".into());
    assert!(s.create(&c, &bad, None).await.is_err());

    // 正常保存＋新バージョン追記＋過去バージョン不変。
    let id = s
        .create(&c, &manifest("expense", "1.0.0"), None)
        .await
        .expect("create");
    let v2 = s
        .update(&c, id, &manifest("expense", "1.1.0"), Some(1), None)
        .await
        .expect("v2");
    assert_eq!(v2, 2);
    let (_, m1) = s.get(&c, id, Some(1), None).await.expect("v1");
    assert_eq!(m1.version, "1.0.0");
    let (_, latest) = s.get(&c, id, None, None).await.expect("latest");
    assert_eq!(latest.version, "1.1.0");
}

#[tokio::test]
async fn publish_is_immutable() {
    let Some(pool) = setup().await else { return };
    let s = store(pool);
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant, "alice");
    let id = s
        .create(&c, &manifest("payroll", "1.0.0"), None)
        .await
        .expect("create");

    // publish 成功 → digest とバージョンが記録される。
    let entry = s
        .publish(&c, id, Some(1), None, None)
        .await
        .expect("publish");
    assert_eq!(entry.name, "payroll");
    assert_eq!(entry.version, "1.0.0");
    assert_eq!(entry.artifact_kind, ArtifactKind::MiniAppCode.as_str());
    assert!(!entry.manifest_digest.is_empty());

    // 同一 name+version の再 publish は 409（不変）。
    let dup = s.publish(&c, id, Some(1), None, None).await;
    assert!(
        matches!(dup, Err(app_platform::AppPlatformError::Conflict(_))),
        "{dup:?}"
    );

    // 新バージョンを追記して publish すると別エントリになる。
    s.update(&c, id, &manifest("payroll", "1.1.0"), Some(1), None)
        .await
        .expect("v2");
    let e2 = s
        .publish(&c, id, Some(2), None, None)
        .await
        .expect("publish v2");
    assert_eq!(e2.version, "1.1.0");
    assert_ne!(e2.id, entry.id);
}

/// レジストリの解決系（latest/get）と yank（不変性を保ちつつ新規解決を止める）を検証する。
#[tokio::test]
async fn registry_resolve_and_yank() {
    let Some(pool) = setup().await else { return };
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::new(AllowAll)));
    let registry = Registry::new(pool.clone());
    let s = MiniAppCodeStore::new(Arc::clone(&artifacts), registry.clone());
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant, "alice");
    let kind = ArtifactKind::MiniAppCode.as_str();

    let id = s
        .create(&c, &manifest("resolver", "1.0.0"), None)
        .await
        .expect("create");
    let e1 = s.publish(&c, id, Some(1), None, None).await.expect("v1");
    s.update(&c, id, &manifest("resolver", "2.0.0"), Some(1), None)
        .await
        .expect("v2");
    let e2 = s.publish(&c, id, Some(2), None, None).await.expect("v2");

    // latest は最新（未 yank）＝ 2.0.0 を返す。
    let latest = registry
        .latest(&c, kind, "resolver")
        .await
        .expect("latest")
        .expect("some");
    assert_eq!(latest.version, "2.0.0");
    assert_eq!(latest.id, e2.id);

    // 特定バージョンの get。
    let got = registry
        .get(&c, kind, "resolver", "1.0.0")
        .await
        .expect("get")
        .expect("some");
    assert_eq!(got.id, e1.id);
    assert!(registry
        .get(&c, kind, "resolver", "9.9.9")
        .await
        .expect("get")
        .is_none());

    // yank は行を残しつつ latest から外す（不変性は保つ）。
    registry.yank(&c, e2.id).await.expect("yank");
    let after = registry
        .latest(&c, kind, "resolver")
        .await
        .expect("latest")
        .expect("some");
    assert_eq!(
        after.version, "1.0.0",
        "yank 後は前バージョンへフォールバック"
    );
    // yanked 行自体は get で依然引ける（不変台帳）。
    let yanked = registry
        .get(&c, kind, "resolver", "2.0.0")
        .await
        .expect("get")
        .expect("some");
    assert!(yanked.yanked);

    // 存在しない id の yank は NotFound。
    assert!(matches!(
        registry.yank(&c, uuid::Uuid::new_v4()).await,
        Err(app_platform::AppPlatformError::NotFound)
    ));
}

/// A（宣言的 mini_app）と B（mini_app_code）が同じ artifact テーブル・list/共有経路に乗る。
#[tokio::test]
async fn a_and_b_share_artifact_plane() {
    let Some(pool) = setup().await else { return };
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), Arc::new(AllowAll)));
    let s = MiniAppCodeStore::new(Arc::clone(&artifacts), Registry::new(pool));
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant, "alice");

    // A: 宣言的 mini_app を直接 artifact として作る（gui MiniAppStore と同経路）。
    let a = artifacts
        .create(
            &c,
            artifact::NewArtifact {
                kind: ArtifactKind::MiniApp,
                name: "decl-app".into(),
                body: serde_json::json!({"description": "宣言的"}),
            },
            None,
        )
        .await
        .expect("A");
    // B: mini_app_code をマニフェストで作る。
    let b_id = s
        .create(&c, &manifest("code-app", "1.0.0"), None)
        .await
        .expect("B");

    // 両者が同じ list_mine（artifact 共通枠）に現れる。
    let mine = artifacts.list_mine(&c, None, None, 50).await.expect("list");
    assert!(mine
        .iter()
        .any(|x| x.id == a.id && x.kind == ArtifactKind::MiniApp));
    assert!(mine
        .iter()
        .any(|x| x.id == b_id && x.kind == ArtifactKind::MiniAppCode));
}
