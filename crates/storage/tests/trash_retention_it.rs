//! ゴミ箱の保持期間（`storage::trash_retention`・GAP-108c・D-202）の結合テスト。
//! 実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 掃除はテナントを横断し、1 台だけが進める（advisory lock）。テストどうしが互いの掃除で消し合ったり、ロックを
//! 取り合って何もしない回が混ざったりしないよう、各テストは専用のテナントを作り、テスト用の advisory lock
//! （別の接続のセッションロック）で 1 本ずつ流す。前の回に残ったこのファイルのテナントは、始めに保持期間を外す。

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
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgPoolOptions, Connection, PgConnection, PgPool};
use storage::audit::AuditRecorder;
use storage::object_store::S3Config;
use storage::trash_retention::{TrashRetentionSweep, SWEEP_ACTOR, SWEEP_LOCK_KEY};
use storage::{Node, ObjectStore, S3ObjectStore, StorageService};
use uuid::Uuid;

/// このファイルが作るテナントの接頭辞（前の回の残りを見分ける）。
const TENANT_PREFIX: &str = "trit";

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
    // ほかのテスト（このファイル）の掃除と重ならないよう、1 本ずつ流す。
    let mut serial = PgConnection::connect(&db_url).await.expect("専用の接続");
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('trash_retention_it', 0))")
        .execute(&mut serial)
        .await
        .expect("テストのロック");
    // 前の回に落ちたテストが保持期間を入れたまま残したテナントは、外しておく（掃除の上限を埋めない）。
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
            store_name: format!("shiki-trash-retention-it-{}", Uuid::new_v4().simple()),
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
        "INSERT INTO tenant (tenant_id, org, display_name, status) VALUES ($1, $2, 'trash it', 'active')",
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

/// 条件が成り立つまで待つ（片付けは、ほかのテストの掃除が先に取ることがある）。
async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what} になりませんでした");
}

fn fresh(tag: &str) -> Vec<u8> {
    format!("trash-retention-it {tag} {}", Uuid::new_v4()).into_bytes()
}

async fn file(c: &Ctx, t: &Tenant, parent: Option<Uuid>, name: &str) -> Node {
    let node = c
        .service
        .write_file_internal(&t.alice, parent, name, &fresh(name), "text/plain", None)
        .await
        .expect("書き込み");
    wait_check(
        c,
        &t.alice,
        Relation::Editor,
        &t.alice.ns().file(&node.id.to_string()),
    )
    .await;
    node
}

async fn folder(c: &Ctx, t: &Tenant, name: &str) -> Node {
    let node = c
        .service
        .create_folder(&t.alice, None, name, None)
        .await
        .expect("フォルダ");
    wait_check(
        c,
        &t.alice,
        Relation::Editor,
        &t.alice.ns().folder(&node.id.to_string()),
    )
    .await;
    node
}

/// ファイルをゴミ箱に入れ、削除した時刻を `days_ago` 日前にする。
async fn trash_file(c: &Ctx, t: &Tenant, id: Uuid, days_ago: i32) {
    c.service
        .soft_delete_file(&t.alice, id, None)
        .await
        .unwrap();
    backdate_deleted(c, t, id, days_ago).await;
}

/// フォルダを配下ごとゴミ箱に入れ、削除した時刻を `days_ago` 日前にする。
async fn trash_folder(c: &Ctx, t: &Tenant, id: Uuid, days_ago: i32) {
    c.service
        .soft_delete_folder(&t.alice, id, None)
        .await
        .unwrap();
    backdate_deleted(c, t, id, days_ago).await;
}

/// 根と配下の、削除した時刻を `days_ago` 日前にずらす（配下は根と同じ時刻のまま・復元と同じ見方）。
async fn backdate_deleted(c: &Ctx, t: &Tenant, root: Uuid, days_ago: i32) {
    sqlx::query(
        "UPDATE node SET deleted_at = deleted_at - make_interval(days => $3) \
         WHERE tenant_id = $1 AND deleted_at IS NOT NULL \
           AND id IN (SELECT descendant FROM node_closure WHERE tenant_id = $1 AND ancestor = $2)",
    )
    .bind(&t.id)
    .bind(root)
    .bind(days_ago)
    .execute(&c.pool)
    .await
    .unwrap();
}

/// 運用者が日数を設定する（監査の記録者は `it`）。
async fn set_days(
    c: &Ctx,
    t: &Tenant,
    days: Option<i32>,
) -> storage::trash_retention::TrashRetentionChange {
    c.service
        .trash_retention()
        .set_retention_days(&t.id, days, "it", serde_json::json!({ "via": "it" }))
        .await
        .unwrap()
        .expect("active なテナント")
}

/// 猶予の起点を `days_ago` 日前にずらす（日数を入れてから時間が経った状態にする）。
async fn backdate_since(c: &Ctx, t: &Tenant, days_ago: i32) {
    sqlx::query(
        "UPDATE tenant SET trash_retention_since = now() - make_interval(days => $2) WHERE tenant_id = $1",
    )
    .bind(&t.id)
    .bind(days_ago)
    .execute(&c.pool)
    .await
    .unwrap();
}

async fn since(c: &Ctx, t: &Tenant) -> Option<DateTime<Utc>> {
    c.service
        .trash_retention()
        .setting(&t.id)
        .await
        .unwrap()
        .expect("テナント")
        .since
}

/// 上限に届いたら続けて回し、`now` で期限を過ぎたものを消しきる（ほかのテナントの残りに埋もれないように）。
async fn sweep_all(c: &Ctx, now: DateTime<Utc>) -> TrashRetentionSweep {
    let mut total = TrashRetentionSweep::default();
    for _ in 0..50 {
        let r = c.service.sweep_trash_retention(now).await.expect("掃除");
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

/// 監査の 1 行（actor・decision・chained・object_type・metadata）。
type AuditRow = (String, String, bool, String, serde_json::Value);

async fn audit_rows(c: &Ctx, t: &Tenant, action: &str, object_id: &str) -> Vec<AuditRow> {
    sqlx::query_as(
        "SELECT actor, decision, chained, object_type, metadata FROM audit_log \
         WHERE tenant_id = $1 AND org = $2 AND action = $3 AND object_id = $4 ORDER BY id",
    )
    .bind(&t.id)
    .bind(t.alice.org())
    .bind(action)
    .bind(object_id)
    .fetch_all(&c.pool)
    .await
    .unwrap()
}

async fn count_action(c: &Ctx, t: &Tenant, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = $2")
        .bind(&t.id)
        .bind(action)
        .fetch_one(&c.pool)
        .await
        .unwrap()
}

/// 終わりに保持期間を外す（このテナントの残りを、あとの回の掃除が拾わないように）。
async fn finish(c: &Ctx, t: &Tenant) {
    set_days(c, t, None).await;
}

/// 期限を過ぎた削除の根だけを、システムの主体で消す。期限の前のもの・ゴミ箱に無いものは残す。監査は
/// `storage.trash_retention.purge` をチェーンで残し、利用者の完全削除の記録（file.purge）は残さない。タプルも外れる。
#[tokio::test]
async fn sweep_purges_only_expired_roots_as_system() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let old = file(&c, &t, None, "old.txt").await;
    let dir = folder(&c, &t, "old-dir").await;
    let child = file(&c, &t, Some(dir.id), "child.txt").await;
    let recent = file(&c, &t, None, "recent.txt").await;
    let alive = file(&c, &t, None, "alive.txt").await;
    trash_file(&c, &t, old.id, 40).await;
    trash_folder(&c, &t, dir.id, 31).await;
    trash_file(&c, &t, recent.id, 29).await;
    let change = set_days(&c, &t, Some(30)).await;
    assert_eq!(
        (change.previous, change.days, change.grace_reset),
        (None, Some(30), true)
    );
    // 入れてから 1 年たった状態にする（猶予は過ぎている）。
    backdate_since(&c, &t, 365).await;

    let r = sweep_all(&c, Utc::now()).await;
    assert_eq!(r.failed, 0, "{r:?}");
    assert!(r.purged >= 2 && r.nodes >= 3, "{r:?}");
    for id in [old.id, dir.id, child.id] {
        assert!(!node_exists(&c, &t, id).await, "期限を過ぎたものは消える");
    }
    assert!(node_exists(&c, &t, recent.id).await, "期限の前のものは残す");
    assert!(
        node_exists(&c, &t, alive.id).await,
        "ゴミ箱に無いものは残す"
    );

    // 監査: 主体は system:trash-retention・チェーンにつなぐ・日数と数を残す。
    let rows = audit_rows(&c, &t, "storage.trash_retention.purge", &old.id.to_string()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, decision, chained, object_type, meta) = &rows[0];
    assert_eq!(
        (
            actor.as_str(),
            decision.as_str(),
            *chained,
            object_type.as_str()
        ),
        (SWEEP_ACTOR, "allow", true, "file")
    );
    assert_eq!(meta["retention_days"], 30);
    assert_eq!(meta["subtree_count"], 1);
    assert!(meta["deleted_at"].is_string(), "{meta}");
    let rows = audit_rows(&c, &t, "storage.trash_retention.purge", &dir.id.to_string()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].3, "folder");
    assert_eq!(
        (
            rows[0].4["subtree_count"].clone(),
            rows[0].4["file_count"].clone()
        ),
        (2.into(), 1.into())
    );
    assert_eq!(count_action(&c, &t, "file.purge").await, 0);
    assert_eq!(count_action(&c, &t, "folder.purge").await, 0);
    // システムの主体で残した行も、チェーンの検証を通る。
    let report = AuditRecorder::new(c.pool.clone())
        .chain_verifier()
        .verify(&t.alice, None, 10_000)
        .await
        .unwrap();
    assert!(report.broken.is_none() && report.complete, "{report:?}");
    // 消したノードのタプルと本体の片付けは残らない（その場の片付けか、片付け待ちの掃除・ほかのテストの掃除が
    // 先に取ることもあるので、片付くまで待つ）。
    let (cr, tr) = (&c, &t);
    eventually("片付け待ちが空", move || async move {
        cr.service.sweep_purge_pending().await.unwrap();
        let pending: i64 =
            sqlx::query_scalar("SELECT count(*) FROM storage_purge_pending WHERE tenant_id = $1")
                .bind(&tr.id)
                .fetch_one(&cr.pool)
                .await
                .unwrap();
        pending == 0
    })
    .await;
    for obj in [
        t.alice.ns().file(&old.id.to_string()),
        t.alice.ns().folder(&dir.id.to_string()),
        t.alice.ns().file(&child.id.to_string()),
    ] {
        assert!(
            c.authz.read_tuples(&obj, None).await.unwrap().is_empty(),
            "{obj:?}"
        );
    }
    finish(&c, &t).await;
}

/// 既定（null）のテナントは、どれだけ古いゴミ箱も消さない。active でない（撤去中の）テナントも消さない。
#[tokio::test]
async fn null_or_inactive_tenant_never_purges() {
    let Some(c) = setup().await else { return };
    let off = new_tenant(&c).await;
    let ancient = file(&c, &off, None, "ancient.txt").await;
    trash_file(&c, &off, ancient.id, 3000).await;
    assert_eq!(
        c.service.trash_retention().setting(&off.id).await.unwrap(),
        Some(storage::trash_retention::TrashRetentionSetting {
            days: None,
            since: None
        }),
        "既定は無効"
    );
    let deleting = new_tenant(&c).await;
    let old = file(&c, &deleting, None, "old.txt").await;
    trash_file(&c, &deleting, old.id, 100).await;
    set_days(&c, &deleting, Some(1)).await;
    backdate_since(&c, &deleting, 365).await;
    sqlx::query("UPDATE tenant SET status = 'deleting' WHERE tenant_id = $1")
        .bind(&deleting.id)
        .execute(&c.pool)
        .await
        .unwrap();

    // 遠い先の時刻で回しても消えない。
    sweep_all(&c, Utc::now() + chrono::Duration::days(10_000)).await;
    assert!(
        node_exists(&c, &off, ancient.id).await,
        "null のテナントは消さない"
    );
    assert!(
        node_exists(&c, &deleting, old.id).await,
        "active でないテナントは消さない"
    );
    assert_eq!(
        count_action(&c, &off, "storage.trash_retention.purge").await,
        0
    );
    assert_eq!(
        count_action(&c, &deleting, "storage.trash_retention.purge").await,
        0
    );
    // 撤去中のテナントは設定も変えられない（active のみ）。
    assert!(c
        .service
        .trash_retention()
        .set_retention_days(&deleting.id, None, "it", serde_json::json!({}))
        .await
        .unwrap()
        .is_none());
    sqlx::query(
        "UPDATE tenant SET status = 'active', trash_retention_days = NULL WHERE tenant_id = $1",
    )
    .bind(&deleting.id)
    .execute(&c.pool)
    .await
    .unwrap();
}

/// 日数を null から入れた直後と短くした直後は、その時点から数え直す（古いゴミ箱が一斉に消えない）。長くした・null に
/// 戻したときは数え直さない。設定の変更は tenant.trash_retention.set として監査に残る。
#[tokio::test]
async fn enabling_and_shortening_restart_the_clock() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let a = file(&c, &t, None, "a.txt").await;
    trash_file(&c, &t, a.id, 100).await;

    // null → 30: 猶予を置き直す。100 日前に消したものも、入れてから 30 日は残す。
    let change = set_days(&c, &t, Some(30)).await;
    assert!(change.grace_reset);
    let start = change.since.expect("起点");
    let now = Utc::now();
    sweep_all(&c, now).await;
    assert!(node_exists(&c, &t, a.id).await, "入れた直後は消えない");
    sweep_all(&c, start + chrono::Duration::days(29)).await;
    assert!(
        node_exists(&c, &t, a.id).await,
        "入れてから 29 日目も消えない"
    );
    sweep_all(
        &c,
        start + chrono::Duration::days(30) + chrono::Duration::minutes(1),
    )
    .await;
    assert!(
        !node_exists(&c, &t, a.id).await,
        "入れてから 30 日を過ぎたら消える"
    );

    // 30 → 60（長くする）: 起点は変えない。
    let change = set_days(&c, &t, Some(60)).await;
    assert_eq!((change.previous, change.grace_reset), (Some(30), false));
    assert_eq!(since(&c, &t).await, Some(start), "長くしても起点は変えない");

    // 入れてから 1 年たった後に、40 日前に消したものは 60 日では残る。
    backdate_since(&c, &t, 365).await;
    let long_ago = since(&c, &t).await;
    let b = file(&c, &t, None, "b.txt").await;
    trash_file(&c, &t, b.id, 40).await;
    sweep_all(&c, Utc::now()).await;
    assert!(node_exists(&c, &t, b.id).await, "60 日の期限の前");

    // 60 → 10（短くする）: 起点を今にする。40 日前に消したものも、縮めてから 10 日は残す。
    let change = set_days(&c, &t, Some(10)).await;
    assert_eq!((change.previous, change.grace_reset), (Some(60), true));
    let shortened = change.since.expect("起点");
    assert!(Some(shortened) > long_ago, "起点を今にする");
    sweep_all(&c, Utc::now()).await;
    assert!(node_exists(&c, &t, b.id).await, "縮めた直後は消えない");
    sweep_all(&c, shortened + chrono::Duration::days(9)).await;
    assert!(
        node_exists(&c, &t, b.id).await,
        "縮めてから 9 日目も消えない"
    );
    sweep_all(
        &c,
        shortened + chrono::Duration::days(10) + chrono::Duration::minutes(1),
    )
    .await;
    assert!(
        !node_exists(&c, &t, b.id).await,
        "縮めてから 10 日を過ぎたら消える"
    );

    // 10 → null（止める）: 起点は変えず、遠い先でも消さない。
    let d = file(&c, &t, None, "d.txt").await;
    trash_file(&c, &t, d.id, 1000).await;
    let change = set_days(&c, &t, None).await;
    assert_eq!(
        (change.previous, change.days, change.grace_reset),
        (Some(10), None, false)
    );
    assert_eq!(
        since(&c, &t).await,
        Some(shortened),
        "止めても起点は変えない"
    );
    sweep_all(&c, Utc::now() + chrono::Duration::days(10_000)).await;
    assert!(node_exists(&c, &t, d.id).await, "止めたら消さない");

    // 変更はすべて監査のチェーンに残る（記録者・前後の日数・猶予を置き直したか）。
    let rows: Vec<(String, bool, serde_json::Value)> = sqlx::query_as(
        "SELECT actor, chained, metadata FROM audit_log \
         WHERE tenant_id = $1 AND org = $2 AND action = 'tenant.trash_retention.set' ORDER BY id",
    )
    .bind(&t.id)
    .bind(t.alice.org())
    .fetch_all(&c.pool)
    .await
    .unwrap();
    let seen: Vec<(serde_json::Value, serde_json::Value, serde_json::Value)> = rows
        .iter()
        .map(|(actor, chained, m)| {
            assert_eq!((actor.as_str(), *chained), ("it", true));
            assert_eq!(m["via"], "it", "呼び手の metadata も残す");
            (
                m["previous"].clone(),
                m["retention_days"].clone(),
                m["grace_reset"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (serde_json::Value::Null, 30.into(), true.into()),
            (30.into(), 60.into(), false.into()),
            (60.into(), 10.into(), true.into()),
            (10.into(), serde_json::Value::Null, false.into()),
        ]
    );
}

/// 削除したフォルダの中に生きているノードがある根（7p-4 より前に崩れた行）は、消さずに飛ばして数え、ほかの期限を
/// 過ぎた根は続けて消す。
#[tokio::test]
async fn live_descendant_root_is_skipped_and_counted() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let broken = folder(&c, &t, "broken").await;
    let alive = file(&c, &t, Some(broken.id), "alive.txt").await;
    trash_folder(&c, &t, broken.id, 50).await;
    sqlx::query("UPDATE node SET deleted_at = NULL WHERE tenant_id = $1 AND id = $2")
        .bind(&t.id)
        .bind(alive.id)
        .execute(&c.pool)
        .await
        .unwrap();
    // 崩れた根より新しい、期限を過ぎた根。
    let next = file(&c, &t, None, "next.txt").await;
    trash_file(&c, &t, next.id, 40).await;
    set_days(&c, &t, Some(30)).await;
    backdate_since(&c, &t, 365).await;

    let r = sweep_all(&c, Utc::now()).await;
    assert!(r.live_descendants >= 1, "{r:?}");
    assert!(node_exists(&c, &t, broken.id).await && node_exists(&c, &t, alive.id).await);
    assert_eq!(
        audit_rows(
            &c,
            &t,
            "storage.trash_retention.purge",
            &broken.id.to_string()
        )
        .await
        .len(),
        0
    );
    assert!(
        !node_exists(&c, &t, next.id).await,
        "飛ばした後も続けて消す"
    );
    finish(&c, &t).await;
}

/// ほかの台が掃除のロックを持っている間は、何もせずに `leader: false` を返す。2 台で同時に回しても、それぞれの根を
/// 1 度だけ消す（監査の行が根ごとに 1 行）。
#[tokio::test]
async fn concurrent_sweeps_purge_each_root_once() {
    let Some(c) = setup().await else { return };
    let t = new_tenant(&c).await;
    let mut ids = Vec::new();
    for i in 0..12 {
        let f = file(&c, &t, None, &format!("f{i}.txt")).await;
        trash_file(&c, &t, f.id, 40).await;
        ids.push(f.id);
    }
    set_days(&c, &t, Some(30)).await;
    backdate_since(&c, &t, 365).await;
    let other = Arc::new(new_service(&c.pool, &c.store, &c.authz));
    let now = Utc::now();

    // ほかの台がロックを持っている間は進めない。
    let mut held = c.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(SWEEP_LOCK_KEY)
        .execute(&mut *held)
        .await
        .unwrap();
    let r = c.service.sweep_trash_retention(now).await.unwrap();
    assert_eq!(
        r,
        TrashRetentionSweep::default(),
        "ロックが取れなければ何もしない"
    );
    for id in &ids {
        assert!(node_exists(&c, &t, *id).await, "ロックの外では消さない");
    }
    held.rollback().await.unwrap();

    let (a, b) = tokio::join!(
        c.service.sweep_trash_retention(now),
        other.sweep_trash_retention(now)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a.leader || b.leader, "{a:?} {b:?}");
    if a.leader && b.leader {
        // 片方が終えてからもう片方が始まった（後の側は消すものが無い）。
        assert_eq!(a.purged.min(b.purged), 0, "{a:?} {b:?}");
    }
    // 上限に届いた残りがあれば消しきる（ほかの回の残りが先に並ぶことがある）。
    sweep_all(&c, now).await;
    for id in &ids {
        assert!(!node_exists(&c, &t, *id).await);
        assert_eq!(
            audit_rows(&c, &t, "storage.trash_retention.purge", &id.to_string())
                .await
                .len(),
            1,
            "根ごとに 1 度だけ消す"
        );
    }
    assert_eq!(
        count_action(&c, &t, "storage.trash_retention.purge").await,
        12
    );
    finish(&c, &t).await;
}
