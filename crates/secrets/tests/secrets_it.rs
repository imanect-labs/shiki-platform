//! シークレット管理の結合テスト（Task 10.9 受け入れ条件）。
//!
//! - 実 Postgres: 登録・ローテ・削除・一覧・**平文が DB/一覧/メタに現れない**（write-only）。
//! - 実 OpenFGA（併設時）: can_use を持たない実行主体の解決拒否・剥奪即時反映。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use secrets::{LocalKeyFileProvider, NewSecret, SecretError, SecretStore};
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
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
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

fn store(pool: PgPool, authz: Arc<dyn AuthzClient>) -> SecretStore {
    let provider = Arc::new(LocalKeyFileProvider::from_bytes([42u8; 32]));
    SecretStore::new(pool, authz, provider)
}

fn tenant() -> String {
    format!("t-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
async fn create_resolve_and_plaintext_never_read_back() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");

    let meta = s
        .create(
            &c,
            NewSecret {
                name: "slack-token".into(),
                plaintext: b"xoxb-super-secret".to_vec(),
                allowed_hosts: vec!["api.slack.com".into()],
            },
            None,
        )
        .await
        .expect("create");
    assert_eq!(meta.name, "slack-token");
    assert_eq!(meta.allowed_hosts, vec!["api.slack.com".to_string()]);

    // メタ・一覧に平文が現れない（write-only）。
    let got = s.get_meta(&c, meta.id, None).await.expect("meta");
    let meta_json = serde_json::to_string(&got).unwrap();
    assert!(!meta_json.contains("xoxb"), "メタに平文が現れてはならない");
    let list = s.list_mine(&c).await.expect("list");
    assert!(!serde_json::to_string(&list).unwrap().contains("xoxb"));

    // DB を直接見ても平文列が無い（暗号文のみ）。
    let (ct, plaintext_col_exists): (Vec<u8>, Option<String>) = sqlx::query_as(
        "SELECT ciphertext, (SELECT string_agg(column_name, ',') FROM information_schema.columns \
         WHERE table_name = 'secret' AND column_name = 'plaintext') \
         FROM secret WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&t)
    .bind(meta.id)
    .fetch_one(&pool)
    .await
    .expect("row");
    assert!(
        plaintext_col_exists.is_none(),
        "secret に plaintext 列が存在してはならない"
    );
    assert!(
        !ct.windows(4).any(|w| w == b"xoxb"),
        "暗号文に平文が漏れてはならない"
    );

    // resolve は平文を返す（能力ゲートウェイ経路）。
    let resolved = s.resolve(&c, "slack-token", None).await.expect("resolve");
    assert_eq!(resolved.plaintext, b"xoxb-super-secret");
    assert!(resolved.binding.allows("api.slack.com"));
    assert!(!resolved.binding.allows("evil.com"));
}

#[tokio::test]
async fn rotate_changes_ciphertext_keeps_resolvable() {
    let Some(pool) = setup().await else { return };
    let s = store(pool, Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");
    let meta = s
        .create(
            &c,
            NewSecret {
                name: "api-key".into(),
                plaintext: b"v1-secret".to_vec(),
                allowed_hosts: vec![],
            },
            None,
        )
        .await
        .expect("create");
    let rotated = s
        .rotate(&c, meta.id, b"v2-secret".to_vec(), None)
        .await
        .expect("rotate");
    assert_eq!(rotated.version, 2);
    let resolved = s.resolve(&c, "api-key", None).await.expect("resolve");
    assert_eq!(resolved.plaintext, b"v2-secret");
}

#[tokio::test]
async fn invalid_binding_hosts_rejected() {
    let Some(pool) = setup().await else { return };
    let s = store(pool, Arc::new(AllowAll));
    let c = ctx(&tenant(), "alice");
    let bad = s
        .create(
            &c,
            NewSecret {
                name: "bad-binding".into(),
                plaintext: b"x".to_vec(),
                allowed_hosts: vec!["https://api.slack.com/webhook".into()],
            },
            None,
        )
        .await;
    assert!(matches!(bad, Err(SecretError::Invalid(_))), "{bad:?}");
}

/// can_use を持たない実行主体の解決が拒否される（live OpenFGA）。
#[tokio::test]
async fn resolve_denied_without_can_use() {
    let Some(pool) = setup().await else { return };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-secret-test-{}", uuid::Uuid::new_v4()),
    };
    let fga = Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
            .await
            .unwrap(),
    );
    let s = store(pool, fga.clone());
    let t = tenant();
    let alice = ctx(&t, "alice");
    let bob = ctx(&t, "bob");

    let meta = s
        .create(
            &alice,
            NewSecret {
                name: "shared-token".into(),
                plaintext: b"top-secret".to_vec(),
                allowed_hosts: vec!["api.slack.com".into()],
            },
            None,
        )
        .await
        .expect("create");

    // owner(alice) は解決できる。
    s.resolve(&alice, "shared-token", None)
        .await
        .expect("alice resolves");
    // bob は can_use が無いので解決拒否（Forbidden）。
    assert!(matches!(
        s.resolve(&bob, "shared-token", None).await,
        Err(SecretError::Forbidden)
    ));
    assert!(matches!(
        s.get_meta(&bob, meta.id, None).await,
        Err(SecretError::Forbidden)
    ));

    // can_use 付与 → bob が解決できる。
    let obj = alice.ns().secret(&meta.id.to_string());
    fga.write_tuple(&bob.subject(), Relation::CanUse, &obj)
        .await
        .unwrap();
    let resolved = s
        .resolve(&bob, "shared-token", None)
        .await
        .expect("bob resolves after grant");
    assert_eq!(resolved.plaintext, b"top-secret");

    // 剥奪 → 即時に解決不可（HigherConsistency）。
    fga.delete_tuple(&bob.subject(), Relation::CanUse, &obj)
        .await
        .unwrap();
    assert!(matches!(
        s.resolve(&bob, "shared-token", None).await,
        Err(SecretError::Forbidden)
    ));
}
