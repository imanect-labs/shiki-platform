//! アーティファクト共通基盤の結合テスト（Task 6.1 受け入れ条件）。
//!
//! - 実 Postgres（`STORAGE_TEST_DATABASE_URL`）: 作成・不変バージョン追記・過去バージョン
//!   不変取得・名前解決・楽観ロック・論理削除。authz はモック（AllowAll）。
//! - 実 OpenFGA（`OPENFGA_TEST_URL` 併設時のみ）: 共有/解除と「権限のないユーザーが
//!   参照できない」の実経路検証。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// 全許可モック（DB 面のテストで OpenFGA を不要にする）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
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
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
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

fn unique_tenant() -> String {
    format!("t-{}", uuid::Uuid::new_v4())
}

fn new_wf(name: &str, body: serde_json::Value) -> NewArtifact {
    NewArtifact {
        kind: ArtifactKind::Workflow,
        name: name.into(),
        body,
    }
}

#[tokio::test]
async fn create_append_and_immutable_versions() {
    let Some(pool) = setup().await else { return };
    let store = ArtifactStore::new(pool, Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    // 作成（version 1）。
    let a = store
        .create(&c, new_wf("wf-1", json!({ "rev": 1 })), None)
        .await
        .expect("create");
    assert_eq!(a.current_version, 1);
    assert_eq!(a.owner, "alice");

    // 新バージョン追記（version 2）。
    let v2 = store
        .append_version(&c, a.id, json!({ "rev": 2 }), Some(1), None)
        .await
        .expect("append v2");
    assert_eq!(v2.version, 2);

    // 過去バージョンが不変で取得できる（受け入れ条件）。
    let v1 = store.get_version(&c, a.id, 1, None).await.expect("get v1");
    assert_eq!(v1.body, json!({ "rev": 1 }));
    let latest = store.get_version(&c, a.id, 2, None).await.expect("get v2");
    assert_eq!(latest.body, json!({ "rev": 2 }));

    // メタは最新バージョンを指す。履歴は新しい順。
    let meta = store.get(&c, a.id, None).await.expect("get meta");
    assert_eq!(meta.current_version, 2);
    let versions = store.list_versions(&c, a.id, None).await.expect("list");
    assert_eq!(
        versions.iter().map(|v| v.version).collect::<Vec<_>>(),
        vec![2, 1]
    );

    // 名前解決。
    let by_name = store
        .get_by_name(&c, ArtifactKind::Workflow, "wf-1", None)
        .await
        .expect("by name");
    assert_eq!(by_name.id, a.id);
}

#[tokio::test]
async fn optimistic_lock_and_name_conflicts() {
    let Some(pool) = setup().await else { return };
    let store = ArtifactStore::new(pool, Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    let a = store
        .create(&c, new_wf("wf-lock", json!({})), None)
        .await
        .expect("create");

    // expected_version 不一致は Conflict（lost-update 防止）。
    let stale = store
        .append_version(&c, a.id, json!({ "rev": 2 }), Some(99), None)
        .await;
    assert!(
        matches!(stale, Err(ArtifactError::Conflict(_))),
        "{stale:?}"
    );

    // 同名 kind の再作成は Conflict。
    let dup = store.create(&c, new_wf("wf-lock", json!({})), None).await;
    assert!(matches!(dup, Err(ArtifactError::Conflict(_))), "{dup:?}");

    // 別テナントでは同名を作成できる（tenant スコープの一意性）。
    let other = ctx(&unique_tenant(), "bob");
    store
        .create(&other, new_wf("wf-lock", json!({})), None)
        .await
        .expect("other tenant same name");
}

#[tokio::test]
async fn soft_delete_frees_name_and_hides_artifact() {
    let Some(pool) = setup().await else { return };
    let store = ArtifactStore::new(pool, Arc::new(AllowAll));
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");

    let a = store
        .create(&c, new_wf("wf-del", json!({})), None)
        .await
        .expect("create");
    store.delete(&c, a.id, None).await.expect("delete");

    // 取得・名前解決・一覧から消える。
    assert!(matches!(
        store.get(&c, a.id, None).await,
        Err(ArtifactError::NotFound)
    ));
    assert!(matches!(
        store
            .get_by_name(&c, ArtifactKind::Workflow, "wf-del", None)
            .await,
        Err(ArtifactError::NotFound)
    ));
    let mine = store
        .list_mine(&c, Some(ArtifactKind::Workflow), None, 50)
        .await
        .expect("list");
    assert!(mine.iter().all(|x| x.id != a.id));

    // 論理削除後はバージョン本文も読めない（FGA tuple 残存でも body を返さない・Codex P1）。
    assert!(
        matches!(
            store.get_version(&c, a.id, 1, None).await,
            Err(ArtifactError::NotFound)
        ),
        "削除済みアーティファクトの過去バージョン本文は取得できない"
    );
    assert!(
        matches!(store.list_versions(&c, a.id, None).await, Ok(v) if v.is_empty())
            || matches!(
                store.list_versions(&c, a.id, None).await,
                Err(ArtifactError::NotFound)
            ),
        "削除済みアーティファクトのバージョン一覧は空"
    );

    // 名前は再利用できる。
    store
        .create(&c, new_wf("wf-del", json!({ "gen": 2 })), None)
        .await
        .expect("recreate after delete");
}

#[tokio::test]
async fn list_mine_filters_by_kind_and_owner() {
    let Some(pool) = setup().await else { return };
    let store = ArtifactStore::new(pool, Arc::new(AllowAll));
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    store
        .create(&alice, new_wf("wf-a", json!({})), None)
        .await
        .expect("alice wf");
    store
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::PromptTemplate,
                name: "tpl-a".into(),
                body: json!({}),
            },
            None,
        )
        .await
        .expect("alice tpl");
    store
        .create(&bob, new_wf("wf-b", json!({})), None)
        .await
        .expect("bob wf");

    let wfs = store
        .list_mine(&alice, Some(ArtifactKind::Workflow), None, 50)
        .await
        .expect("list wf");
    assert_eq!(wfs.len(), 1);
    assert_eq!(wfs[0].name, "wf-a");
    let all = store.list_mine(&alice, None, None, 50).await.expect("all");
    assert_eq!(all.len(), 2, "kind 未指定は自分の全 kind");
}

/// 実 OpenFGA での ReBAC 共有・解除・拒否の実経路検証（受け入れ条件）。
#[tokio::test]
async fn share_and_deny_with_live_openfga() {
    let Some(pool) = setup().await else { return };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
            .expect("model json");
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-artifact-test-{}", uuid::Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
    let store = ArtifactStore::new(pool, Arc::new(fga));

    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    let a = store
        .create(&alice, new_wf("wf-share", json!({ "secret": false })), None)
        .await
        .expect("create");

    // 共有前: bob は参照できない（Forbidden・受け入れ条件）。
    assert!(matches!(
        store.get(&bob, a.id, None).await,
        Err(ArtifactError::Forbidden)
    ));

    // viewer 共有 → bob が読める。ただし編集はできない。
    store
        .share(
            &alice,
            a.id,
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .expect("share viewer");
    store.get(&bob, a.id, None).await.expect("bob reads");
    store
        .get_version(&bob, a.id, 1, None)
        .await
        .expect("bob reads v1");
    assert!(matches!(
        store
            .append_version(&bob, a.id, json!({}), None, None)
            .await,
        Err(ArtifactError::Forbidden)
    ));

    // 共有相手一覧（owner のみ・bob には見えない）。
    let shares = store.list_shares(&alice, a.id, None).await.expect("shares");
    assert_eq!(shares.len(), 1);
    assert!(matches!(
        store.list_shares(&bob, a.id, None).await,
        Err(ArtifactError::Forbidden)
    ));

    // 解除 → 即時に読めなくなる（剥奪即時反映）。
    store
        .unshare(
            &alice,
            a.id,
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Viewer,
            None,
        )
        .await
        .expect("unshare");
    assert!(matches!(
        store.get(&bob, a.id, None).await,
        Err(ArtifactError::Forbidden)
    ));

    // editor 共有はバージョン追記可・削除（owner）は不可。
    store
        .share(
            &alice,
            a.id,
            &storage::ShareTarget::User { id: "bob".into() },
            ArtifactRole::Editor,
            None,
        )
        .await
        .expect("share editor");
    store
        .append_version(&bob, a.id, json!({ "by": "bob" }), None, None)
        .await
        .expect("bob edits");
    assert!(matches!(
        store.delete(&bob, a.id, None).await,
        Err(ArtifactError::Forbidden)
    ));
}
