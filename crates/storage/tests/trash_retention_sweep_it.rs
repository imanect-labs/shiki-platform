//! ゴミ箱の保持期間の掃除（GAP-108c・D-202）の、回収タイマの 1 周・上限・根ごとの失敗・設定の変更との
//! 並行の結合テスト（`trash_retention_it` の続き）。実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 掃除はテナントを横断するので、`trash_retention_it` と同じテスト用の advisory lock（別の接続のセッション
//! ロック）で 1 本ずつ流し、同じ接頭辞のテナントを作る（前の回の残りは、始めに保持期間を外す）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Consistency, FgaObject, Principal, Relation};
use chrono::Utc;
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use storage::object_store::S3Config;
use storage::trash_retention::{TrashRetentionSweep, SWEEP_MAX_ROOTS};
use storage::upload_gc_timer::run_upload_gc_cycle;
use storage::{Node, ObjectStore, S3ObjectStore, StorageService};
use uuid::Uuid;

/// `trash_retention_it` と同じ接頭辞（前の回の残りを、どちらの setup でも外す）。
const TENANT_PREFIX: &str = "trit";

/// 回収タイマの 1 周に渡す中断アップロードの TTL（ほかのテストの宣言を回収しない長さ）。
const GC_TTL: Duration = Duration::from_secs(24 * 60 * 60);

struct Ctx {
    service: Arc<StorageService>,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    store: Arc<dyn ObjectStore>,
    /// テストを 1 本ずつ流すためのセッションロックを持つ接続（テストの終わりに閉じる）。
    _serial: PgConnection,
}

/// 1 つのテナントと、その組織のメンバー alice。
struct Tenant {
    id: String,
    alice: AuthContext,
}

async fn setup() -> Option<Ctx> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(openfga_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    let mut serial = PgConnection::connect(&db_url).await.expect("専用の接続");
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('trash_retention_it', 0))")
        .execute(&mut serial)
        .await
        .expect("テストのロック");
    sqlx::query(
        "UPDATE tenant SET trash_retention_days = NULL WHERE starts_with(tenant_id, $1) \
         AND trash_retention_days IS NOT NULL",
    )
    .bind(TENANT_PREFIX)
    .execute(&pool)
    .await
    .unwrap();
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: openfga_url,
            store_name: format!("shiki-trash-sweep-it-{}", Uuid::new_v4().simple()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let s3 = S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    store.ensure_bucket().await.expect("バケット準備");
    let service = Arc::new(new_service(&pool, &store, &authz));
    Some(Ctx {
        service,
        pool,
        authz,
        store,
        _serial: serial,
    })
}

fn new_service(
    pool: &PgPool,
    store: &Arc<dyn ObjectStore>,
    authz: &Arc<dyn AuthzClient>,
) -> StorageService {
    StorageService::new(
        pool.clone(),
        store.clone(),
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    )
}

/// テナント台帳に active なテナントを作り、組織のメンバー alice を置く。保持期間は既定（null）のまま。
async fn new_tenant(c: &Ctx) -> Tenant {
    let id = format!("{TENANT_PREFIX}{}", Uuid::new_v4().simple());
    let org = format!("trorg{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, $2, 'trash sweep it', 'active')",
    )
    .bind(&id)
    .bind(&org)
    .execute(&c.pool)
    .await
    .unwrap();
    let alice = AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: format!("alice{}", Uuid::new_v4().simple()),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.clone(),
        authz::TenantId::parse(&id).unwrap(),
    );
    let org_obj = alice.ns().organization(&org);
    c.authz
        .write_tuple(&alice.subject(), Relation::Member, &org_obj)
        .await
        .expect("member");
    wait_check(c, &alice, Relation::Member, &org_obj).await;
    Tenant { id, alice }
}

async fn wait_check(c: &Ctx, ctx: &AuthContext, relation: Relation, obj: &FgaObject) {
    for _ in 0..200 {
        if c.authz
            .check(
                &ctx.subject(),
                relation,
                obj,
                Consistency::HigherConsistency,
            )
            .await
            .expect("check")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("seed したタプルが見えませんでした");
}

/// 内容 `bytes` のファイルを書き、ゴミ箱に入れて、削除した時刻を `days_ago` 日前にする（タプルは待たない）。
async fn trashed_file(c: &Ctx, t: &Tenant, name: &str, bytes: &[u8], days_ago: i32) -> Node {
    let node = c
        .service
        .write_file_internal(&t.alice, None, name, bytes, "text/plain", None)
        .await
        .expect("書き込み");
    c.service
        .soft_delete_file(&t.alice, node.id, None)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE node SET deleted_at = deleted_at - make_interval(days => $3) \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&t.id)
    .bind(node.id)
    .bind(days_ago)
    .execute(&c.pool)
    .await
    .unwrap();
    node
}

/// 保持期間を `days` 日にして、入れてから 1 年たった状態にする（猶予は過ぎている）。
async fn enable(c: &Ctx, t: &Tenant, days: i32) {
    set_days(c, t, Some(days)).await;
    sqlx::query(
        "UPDATE tenant SET trash_retention_since = now() - make_interval(days => 365) WHERE tenant_id = $1",
    )
    .bind(&t.id)
    .execute(&c.pool)
    .await
    .unwrap();
}

async fn set_days(c: &Ctx, t: &Tenant, days: Option<i32>) {
    c.service
        .trash_retention()
        .set_retention_days(&t.id, days, "it", serde_json::json!({ "via": "it" }))
        .await
        .unwrap()
        .expect("active なテナント");
}

/// 上限に届いたら続けて回す（ほかのテナントの残りに埋もれないように）。
async fn sweep_all(c: &Ctx) -> TrashRetentionSweep {
    let mut total = TrashRetentionSweep::default();
    for _ in 0..50 {
        let r = c
            .service
            .sweep_trash_retention(Utc::now())
            .await
            .expect("掃除");
        assert!(r.leader, "テストは 1 本ずつ流すので、ロックは取れる");
        total.leader = true;
        total.purged += r.purged;
        total.nodes += r.nodes;
        total.live_descendants += r.live_descendants;
        total.failed += r.failed;
        if !r.more {
            return total;
        }
    }
    panic!("掃除が終わりませんでした");
}

async fn node_exists(c: &Ctx, t: &Tenant, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node WHERE tenant_id = $1 AND id = $2)")
        .bind(&t.id)
        .bind(id)
        .fetch_one(&c.pool)
        .await
        .unwrap()
}

async fn purge_audits(c: &Ctx, t: &Tenant, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE tenant_id = $1 AND action = 'storage.trash_retention.purge' AND object_id = $2",
    )
    .bind(&t.id)
    .bind(id.to_string())
    .fetch_one(&c.pool)
    .await
    .unwrap()
}

async fn pending_rows(c: &Ctx, t: &Tenant) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM storage_purge_pending WHERE tenant_id = $1")
        .bind(&t.id)
        .fetch_one(&c.pool)
        .await
        .unwrap()
}

/// 回収タイマの 1 周（`run_upload_gc_cycle`）が、保持期間の掃除と完全削除の片付けを回す（タイマから外すと、
/// 本番で保持期間の掃除を起こす道が無くなる）。ほかの回の残りが先に並ぶことがあるので、片付くまで周を繰り返す。
#[tokio::test]
async fn gc_cycle_runs_trash_retention_and_purge_cleanup() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let bytes = format!("trash-sweep-it cycle {}", Uuid::new_v4()).into_bytes();
    let old = trashed_file(&c, &t, "old.txt", &bytes, 40).await;
    let obj = t.alice.ns().file(&old.id.to_string());
    wait_check(&c, &t.alice, Relation::Editor, &obj).await;
    enable(&c, &t, 30).await;

    let mut done = false;
    for _ in 0..50 {
        let failed = run_upload_gc_cycle(&c.service, GC_TTL).await;
        assert!(!failed, "DB に届く周は失敗にしない");
        if !node_exists(&c, &t, old.id).await
            && pending_rows(&c, &t).await == 0
            && c.authz.read_tuples(&obj, None).await.unwrap().is_empty()
        {
            done = true;
            break;
        }
    }
    assert!(
        done,
        "回収タイマの周で、期限を過ぎた根が消えて片付け待ちとタプルが片付く"
    );
    assert_eq!(purge_audits(&c, &t, old.id).await, 1);
    set_days(&c, &t, None).await;
}

/// DB に届かない周は `true` を返す（タイマが待ちを延ばす）。
#[tokio::test]
async fn gc_cycle_reports_failure_when_db_is_unreachable() {
    let Some(c) = setup().await else { return };
    let unreachable = PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/none")
        .unwrap();
    let service = new_service(&unreachable, &c.store, &c.authz);
    assert!(run_upload_gc_cycle(&service, GC_TTL).await);
}

/// 1 回の掃除は根の上限（`SWEEP_MAX_ROOTS`）で止めて `more` を返し、次の回が残りを消す。候補は 50 件ずつ引くので、
/// 上限までに 2 つ目のバッチも引く。ファイルは同じ内容にして本体を共有させる。
#[tokio::test]
async fn sweep_stops_at_root_limit_and_next_sweep_finishes() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let bytes = format!("trash-sweep-it many {}", Uuid::new_v4()).into_bytes();
    let mut ids = Vec::new();
    for i in 0..=SWEEP_MAX_ROOTS {
        ids.push(
            trashed_file(&c, &t, &format!("f{i}.txt"), &bytes, 40)
                .await
                .id,
        );
    }
    enable(&c, &t, 30).await;

    let now = Utc::now();
    let first = c.service.sweep_trash_retention(now).await.unwrap();
    assert!(first.leader);
    assert_eq!(
        (first.purged, first.more, first.failed),
        (SWEEP_MAX_ROOTS, true, 0),
        "{first:?}"
    );
    let second = c.service.sweep_trash_retention(now).await.unwrap();
    assert_eq!(
        (second.purged, second.more, second.failed),
        (1, false, 0),
        "{second:?}"
    );
    for id in &ids {
        assert!(!node_exists(&c, &t, *id).await);
    }
    set_days(&c, &t, None).await;
}

/// 消せずに失敗した根（参照数の決まりが崩れた blob）は数えて残し、ほかの期限を過ぎた根は続けて消す。
#[tokio::test]
async fn failing_root_is_counted_and_others_continue() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let broken_bytes = format!("trash-sweep-it broken {}", Uuid::new_v4()).into_bytes();
    let broken = trashed_file(&c, &t, "broken.txt", &broken_bytes, 50).await;
    sqlx::query(
        "UPDATE blob SET refcount = 0 WHERE tenant_id = $1 AND org = $2 \
         AND sha256 = (SELECT blob_sha256 FROM node WHERE tenant_id = $1 AND id = $3)",
    )
    .bind(&t.id)
    .bind(t.alice.org())
    .bind(broken.id)
    .execute(&c.pool)
    .await
    .unwrap();
    let next_bytes = format!("trash-sweep-it next {}", Uuid::new_v4()).into_bytes();
    let next = trashed_file(&c, &t, "next.txt", &next_bytes, 40).await;
    enable(&c, &t, 30).await;

    let r = sweep_all(&c).await;
    assert!(r.failed >= 1, "{r:?}");
    assert!(node_exists(&c, &t, broken.id).await, "失敗した根は残す");
    assert_eq!(purge_audits(&c, &t, broken.id).await, 0);
    assert!(!node_exists(&c, &t, next.id).await, "失敗の後も続けて消す");
    // 失敗した根を、あとの回の掃除が拾い続けないようにする。
    set_days(&c, &t, None).await;
}

/// 保持期間を止める変更（tenant の行を FOR UPDATE で押さえて書き換えた、commit の前）と重なった掃除は、変更の
/// commit を待ち、新しい値で確かめ直して消さない（止めた後に、古い日数での完全削除が確定しない）。
#[tokio::test]
async fn sweep_waits_for_retention_change_and_does_not_purge() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let bytes = format!("trash-sweep-it change {}", Uuid::new_v4()).into_bytes();
    let old = trashed_file(&c, &t, "old.txt", &bytes, 40).await;
    enable(&c, &t, 30).await;

    let mut change = c.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM tenant WHERE tenant_id = $1 FOR UPDATE")
        .bind(&t.id)
        .execute(&mut *change)
        .await
        .unwrap();
    sqlx::query("UPDATE tenant SET trash_retention_days = NULL WHERE tenant_id = $1")
        .bind(&t.id)
        .execute(&mut *change)
        .await
        .unwrap();
    let now = Utc::now();
    let sweep = {
        let service = c.service.clone();
        tokio::spawn(async move { service.sweep_trash_retention(now).await })
    };
    // 掃除が tenant の行を待つまで（直す前の形では待たずに消す）。
    for _ in 0..200 {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity \
             WHERE datname = current_database() AND pid <> pg_backend_pid() \
               AND wait_event_type = 'Lock' AND wait_event IN ('transactionid', 'tuple') \
               AND strpos(query, 'FOR KEY SHARE OF t') > 0)",
        )
        .fetch_one(&c.pool)
        .await
        .unwrap();
        if sweep.is_finished() || waiting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    change.commit().await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(60), sweep)
        .await
        .expect("止まらない")
        .unwrap()
        .unwrap();
    assert!(r.leader);
    assert!(
        node_exists(&c, &t, old.id).await,
        "止めた後に消さない: {r:?}"
    );
    assert_eq!(purge_audits(&c, &t, old.id).await, 0);
}
