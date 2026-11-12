//! 作成の冪等の引き当て（`find_own_created_since`・R1）と、作成の owner と監査を commit の前に書く順（7p-10）の結合テスト。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。認可は owner のタプルを覚えるだけの偽物（owner なら読める）で、
//! 書き込みを止める・失敗させることができる（呼ぶ側を捨てる区間の試験）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::sync::{Notify, Semaphore};

/// owner のタプルだけを覚える認可（owner なら viewer 以上を持つとみなす）。
struct OwnerTuples {
    owners: Mutex<HashSet<(Subject, FgaObject)>>,
    /// 立っていれば、書き込みに入ったことを `entered` で知らせ、`gate` の許可が来るまで返さない（書いたことにするのは
    /// 許可が来た後）。
    hold_writes: AtomicBool,
    entered: Notify,
    gate: Semaphore,
    /// 立っていれば書き込みを失敗させる。
    fail_writes: AtomicBool,
}

impl Default for OwnerTuples {
    fn default() -> Self {
        OwnerTuples {
            owners: Mutex::default(),
            hold_writes: AtomicBool::new(false),
            entered: Notify::new(),
            gate: Semaphore::new(0),
            fail_writes: AtomicBool::new(false),
        }
    }
}

impl OwnerTuples {
    fn has_owner(&self, s: &Subject, o: &FgaObject) -> bool {
        self.owners
            .lock()
            .unwrap()
            .contains(&(s.clone(), o.clone()))
    }
}

#[async_trait]
impl AuthzClient for OwnerTuples {
    async fn check(
        &self,
        s: &Subject,
        _r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(self.has_owner(s, o))
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(AuthzError::Unexpected {
                status: 503,
                body: "書き込みを失敗させた".into(),
            });
        }
        if self.hold_writes.load(Ordering::SeqCst) {
            self.entered.notify_one();
            self.gate.acquire().await.unwrap().forget();
        }
        if r == Relation::Owner {
            return Ok(self.owners.lock().unwrap().insert((s.clone(), o.clone())));
        }
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        if r == Relation::Owner {
            return Ok(self.owners.lock().unwrap().remove(&(s.clone(), o.clone())));
        }
        Ok(false)
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
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
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
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

fn unique_tenant() -> String {
    format!("t-{}", uuid::Uuid::new_v4())
}

fn new_wf(name: &str) -> NewArtifact {
    NewArtifact {
        kind: ArtifactKind::Workflow,
        name: name.into(),
        body: json!({ "rev": 1 }),
    }
}

/// DB の時計の今（作成時刻 `created_at` と同じ時計で比べる）。
async fn db_now(pool: &PgPool) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 生きている同名の行の数。
async fn live_rows(pool: &PgPool, tenant: &str, name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM artifact WHERE tenant_id = $1 AND name = $2 AND deleted_at IS NULL",
    )
    .bind(tenant)
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// その id の作成の監査の行の数。
async fn create_audits(pool: &PgPool, tenant: &str, id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'artifact.create' AND object_id = $2",
    )
    .bind(tenant)
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// テナントの作成の監査の行の数（id の分からない巻き戻しの確かめに使う）。
async fn tenant_create_audits(pool: &PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'artifact.create'",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 本人が since 以降に作り、消していない、読めるものだけが見つかる。ほかの人のもの・since より前・別の種類・
/// 消したもの・ReBAC で読めないもの・ほかのテナントは `None`。
#[tokio::test]
async fn finds_only_own_live_readable_creations_since() {
    let Some(pool) = setup().await else { return };
    let authz = Arc::new(OwnerTuples::default());
    let store = ArtifactStore::new(pool.clone(), authz.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");

    let before = db_now(&pool).await;
    let created = store.create(&alice, new_wf("wf-a"), None).await.unwrap();
    let after = db_now(&pool).await;

    let find = |c: &AuthContext, kind: ArtifactKind, name: &str, since: DateTime<Utc>| {
        let (store, c, name) = (store.clone(), c.clone(), name.to_string());
        async move {
            store
                .find_own_created_since(&c, kind, &name, since, None)
                .await
                .unwrap()
        }
    };
    let found = find(&alice, ArtifactKind::Workflow, "wf-a", before).await;
    assert_eq!(found.map(|a| a.id), Some(created.id));
    // 名前は作成と同じく前後の空白を落として引く。
    let padded = find(&alice, ArtifactKind::Workflow, "  wf-a ", before).await;
    assert_eq!(padded.map(|a| a.id), Some(created.id));

    // since より前に作ったもの・ほかの人・別の種類・別の名前は見つからない。
    assert!(find(&alice, ArtifactKind::Workflow, "wf-a", after)
        .await
        .is_none());
    assert!(find(&bob, ArtifactKind::Workflow, "wf-a", before)
        .await
        .is_none());
    assert!(find(&alice, ArtifactKind::MiniApp, "wf-a", before)
        .await
        .is_none());
    assert!(find(&alice, ArtifactKind::Workflow, "wf-b", before)
        .await
        .is_none());
    // ほかのテナントの同じ人・同じ名前も見つからない。
    let other = ctx(&unique_tenant(), "alice");
    assert!(find(&other, ArtifactKind::Workflow, "wf-a", before)
        .await
        .is_none());

    // ReBAC で読めなくなったもの（owner のタプルが外れた）は、行があっても見つからない。
    let obj = alice.ns().artifact(&created.id.to_string());
    authz
        .delete_tuple(&alice.subject(), Relation::Owner, &obj)
        .await
        .unwrap();
    assert!(find(&alice, ArtifactKind::Workflow, "wf-a", before)
        .await
        .is_none());

    // 消したものは見つからない。
    let gone = store.create(&alice, new_wf("wf-gone"), None).await.unwrap();
    assert!(find(&alice, ArtifactKind::Workflow, "wf-gone", before)
        .await
        .is_some());
    store.delete(&alice, gone.id, None).await.unwrap();
    assert!(find(&alice, ArtifactKind::Workflow, "wf-gone", before)
        .await
        .is_none());
}

/// 作成の区間（owner を書いて commit する）は、呼ぶ側を owner の書き込みの途中で捨てても最後まで走る。owner のタプルは
/// commit の前に書くので、書き込みの途中では行はまだ見えず、終わると行と owner と作成の監査がそろう（owner の無い行が
/// 名前を塞がない・監査の無い作成が残らない・7p-10）。
#[tokio::test]
async fn dropped_create_leaves_the_row_and_its_owner_together() {
    let Some(pool) = setup().await else { return };
    let authz = Arc::new(OwnerTuples::default());
    authz.hold_writes.store(true, Ordering::SeqCst);
    let store = ArtifactStore::new(pool.clone(), authz.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");

    let entered = authz.entered.notified();
    let task = {
        let (store, alice) = (store.clone(), alice.clone());
        tokio::spawn(async move { store.create(&alice, new_wf("wf-drop"), None).await })
    };
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .expect("owner の書き込みに入る");
    // owner を書いている間は、まだ commit していない（行は見えない）。
    assert_eq!(live_rows(&pool, &tenant, "wf-drop").await, 0);
    // 呼ぶ側を捨ててから書き込みを通す。
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    authz.gate.add_permits(1);

    let mut rows = 0;
    for _ in 0..100 {
        rows = live_rows(&pool, &tenant, "wf-drop").await;
        if rows == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rows, 1, "捨てた後も区間は commit まで走る");
    let id: uuid::Uuid = sqlx::query_scalar(
        "SELECT id FROM artifact WHERE tenant_id = $1 AND name = $2 AND deleted_at IS NULL",
    )
    .bind(&tenant)
    .bind("wf-drop")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        authz.has_owner(&alice.subject(), &alice.ns().artifact(&id.to_string())),
        "行には owner のタプルがある"
    );
    assert_eq!(
        create_audits(&pool, &tenant, &id.to_string()).await,
        1,
        "捨てた作成にも作成の監査が残る"
    );
    authz.hold_writes.store(false, Ordering::SeqCst);
    assert_eq!(store.get(&alice, id, None).await.unwrap().id, id);
}

/// owner の書き込みに失敗したら commit せずに巻き戻す（行も残らず、同じ名前ですぐ作り直せる）。
#[tokio::test]
async fn failed_owner_write_leaves_no_row() {
    let Some(pool) = setup().await else { return };
    let authz = Arc::new(OwnerTuples::default());
    authz.fail_writes.store(true, Ordering::SeqCst);
    let store = ArtifactStore::new(pool.clone(), authz.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");

    let err = store
        .create(&alice, new_wf("wf-fail"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ArtifactError::Internal(m) if m.starts_with("owner tuple:")),
        "{err:?}"
    );
    assert_eq!(live_rows(&pool, &tenant, "wf-fail").await, 0);
    assert_eq!(
        tenant_create_audits(&pool, &tenant).await,
        0,
        "巻き戻した作成の監査は残らない"
    );

    authz.fail_writes.store(false, Ordering::SeqCst);
    let made = store.create(&alice, new_wf("wf-fail"), None).await.unwrap();
    assert_eq!(store.get(&alice, made.id, None).await.unwrap().id, made.id);
    assert_eq!(create_audits(&pool, &tenant, &made.id.to_string()).await, 1);
}
