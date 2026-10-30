//! 消したテーブルの OpenFGA のタプルの撤去（BUG-149）の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! - テーブルを消すと、`data_table` のタプル（持ち主・閲覧・追加だけ）が消え、撤去の印が付く
//! - 削除の後の撤去に失敗したもの・撤去を始める前に消したもの（印が空のまま）は、掃除が撤去して印を付ける
//!
//! 各テストは自分のテナント（`t-<uuid>`）と OpenFGA のストアに閉じ、最後にテナントとストアを撤去する。

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, CheckTuple, Consistency, FgaObject, ObjectType,
    Principal, ReadTupleKey, Relation, Subject,
};
use data::{DataStore, FieldDef, FieldType, NewDataTable, RefResolver, TableSchema};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

struct AllowRefs;

#[async_trait]
impl RefResolver for AllowRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(true)
    }
}

/// 実 OpenFGA に委ね、`fail_purge` が立っている間だけタプルの一括撤去を失敗させる（撤去の失敗の再現）。
struct Flaky {
    inner: OpenFgaClient,
    fail_purge: AtomicBool,
}

#[async_trait]
impl AuthzClient for Flaky {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        c: Consistency,
    ) -> Result<bool, AuthzError> {
        self.inner.check(s, r, o, c).await
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        self.inner.batch_check(items, c).await
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.inner.write_tuple(s, r, o).await
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.inner.delete_tuple(s, r, o).await
    }
    async fn read_tuples(
        &self,
        o: &FgaObject,
        r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        self.inner.read_tuples(o, r).await
    }
    async fn list_objects(
        &self,
        s: &Subject,
        r: Relation,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.inner.list_objects(s, r, t).await
    }
    async fn delete_object_tuples(&self, o: &FgaObject) -> Result<u32, AuthzError> {
        if self.fail_purge.load(Ordering::SeqCst) {
            return Err(AuthzError::Unexpected {
                status: 503,
                body: "テストで撤去を失敗させた".into(),
            });
        }
        self.inner.delete_object_tuples(o).await
    }
    async fn read_subject_objects(
        &self,
        s: &Subject,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.inner.read_subject_objects(s, t).await
    }
}

struct Env {
    store: DataStore,
    authz: Arc<Flaky>,
    pool: PgPool,
    base_url: String,
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(base_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
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
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
            .expect("model json");
    let inner = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-data-table-purge-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("OpenFGA 接続");
    let authz = Arc::new(Flaky {
        inner,
        fail_purge: AtomicBool::new(false),
    });
    let dyn_authz: Arc<dyn AuthzClient> = authz.clone();
    Some(Env {
        store: DataStore::new(pool.clone(), dyn_authz, Arc::new(AllowRefs)),
        authz,
        pool,
        base_url,
    })
}

/// テナントとストアを撤去する（テーブルのタプルはテナントの撤去が消す）。
async fn teardown(env: &Env, tenant: &str) {
    env.store.purge_tenant(tenant).await.unwrap();
    let resp = reqwest::Client::new()
        .delete(format!(
            "{}/stores/{}",
            env.base_url,
            env.authz.inner.store_id()
        ))
        .send()
        .await
        .expect("OpenFGA delete store");
    assert!(
        resp.status().is_success(),
        "delete store: {}",
        resp.status()
    );
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

/// 持ち主の alice が作り、bob に閲覧・carol に追加だけで共有したテーブル。
async fn shared_table(env: &Env, tenant: &str, name: &str) -> Uuid {
    let alice = ctx(tenant, "alice");
    let id = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: name.into(),
                schema: TableSchema {
                    fields: vec![FieldDef::new("name", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                    field_policy: vec![],
                    aggregate_min_rows: None,
                    fsm_ref: None,
                    rag: None,
                },
            },
            None,
        )
        .await
        .expect("create table")
        .id;
    let obj = alice.ns().data_table(&id.to_string());
    for (who, relation) in [("bob", Relation::Viewer), ("carol", Relation::Submitter)] {
        env.authz
            .write_tuple(&ctx(tenant, who).subject(), relation, &obj)
            .await
            .unwrap();
    }
    assert_eq!(tuples(env, tenant, id).await, 3, "持ち主・閲覧・追加だけ");
    id
}

async fn tuples(env: &Env, tenant: &str, id: Uuid) -> usize {
    env.authz
        .read_tuples(&ctx(tenant, "alice").ns().data_table(&id.to_string()), None)
        .await
        .unwrap()
        .len()
}

async fn purged_mark(env: &Env, tenant: &str, id: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT authz_purged_at IS NOT NULL FROM data_table WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant)
    .bind(id)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

/// テーブルを消すと、`data_table` のタプルが消えて撤去の印が付く（見えるテーブルの数が減る）。
#[tokio::test]
async fn deleting_a_table_removes_its_tuples() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let id = shared_table(&env, &tenant, "消すテーブル").await;
    let bob = ctx(&tenant, "bob");
    let visible = env
        .authz
        .list_objects(&bob.subject(), Relation::Viewer, ObjectType::DataTable)
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);

    env.store
        .delete_table(&ctx(&tenant, "alice"), id, None)
        .await
        .unwrap();
    assert_eq!(
        tuples(&env, &tenant, id).await,
        0,
        "持ち主・共有のタプルが残らない"
    );
    assert!(purged_mark(&env, &tenant, id).await);
    let visible = env
        .authz
        .list_objects(&bob.subject(), Relation::Viewer, ObjectType::DataTable)
        .await
        .unwrap();
    assert!(visible.is_empty(), "{visible:?}");

    teardown(&env, &tenant).await;
}

/// 削除の後の撤去に失敗したもの・撤去を始める前に消したものは、掃除が撤去して印を付ける。
#[tokio::test]
async fn sweeper_purges_what_deletion_left_behind() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    // この版より前に消したテーブル（論理削除だけでタプルが残る）。
    let before = shared_table(&env, &tenant, "前に消した").await;
    sqlx::query(
        "UPDATE data_table SET deleted_at = now(), updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant)
    .bind(before)
    .execute(&env.pool)
    .await
    .unwrap();
    // 削除の後の撤去に失敗したテーブル（削除そのものは通る）。
    let failed = shared_table(&env, &tenant, "撤去に失敗").await;
    env.authz.fail_purge.store(true, Ordering::SeqCst);
    env.store
        .delete_table(&ctx(&tenant, "alice"), failed, None)
        .await
        .expect("撤去に失敗しても削除は通る");
    env.authz.fail_purge.store(false, Ordering::SeqCst);
    for id in [before, failed] {
        assert_eq!(tuples(&env, &tenant, id).await, 3);
        assert!(!purged_mark(&env, &tenant, id).await);
    }

    // 掃除を、撤去するものが無くなるまで回す（撤去を終えたものは引かないので止まる。ほかのテストが消した
    // テーブルも同じ DB にありうる）。
    let mut rounds = 0;
    loop {
        let p = env.store.purge_deleted_table_tuples().await.unwrap();
        assert_eq!(p.failed, 0, "{p:?}");
        if p.purged == 0 {
            break;
        }
        rounds += 1;
        assert!(rounds < 1_000, "掃除が終わらない");
    }
    for id in [before, failed] {
        assert_eq!(tuples(&env, &tenant, id).await, 0);
        assert!(purged_mark(&env, &tenant, id).await);
    }

    teardown(&env, &tenant).await;
}
