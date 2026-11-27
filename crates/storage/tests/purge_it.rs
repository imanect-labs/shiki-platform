//! ゴミ箱の完全削除（`purge_node`・`empty_trash`・GAP-108b・D-201）の結合テスト。
//! 実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! 片付け（タプル・本体）は commit の後にその場で行うが、掃除（`sweep_purge_pending`・テナント横断）を回す別の
//! テストが同じ行を先に取ることがある（そのときは掃除が片付ける）。タプル・本体の確かめは、片付くまで待つ。

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
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::audit::AuditRecorder;
use storage::content_address::{blob_object_key, sha256_hex};
use storage::object_store::S3Config;
use storage::service::{PurgeOutcome, EMPTY_TRASH_MAX_ROOTS};
use storage::{
    Node, NodeKind, ObjectStore, S3ObjectStore, ShareRole, ShareTarget, StorageError,
    StorageService,
};
use uuid::Uuid;

const TENANT: &str = "default";

struct Ctx {
    service: Arc<StorageService>,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    store: Arc<dyn ObjectStore>,
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
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: openfga_url,
            store_name: format!("shiki-purge-it-{}", Uuid::new_v4().simple()),
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
    let service = Arc::new(StorageService::new(
        pool.clone(),
        store.clone(),
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    Some(Ctx {
        service,
        pool,
        authz,
        store,
    })
}

fn make_ctx(org: &str, kind: authz::PrincipalKind, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        authz::TenantId::parse(TENANT).unwrap(),
    )
}

/// 1 つの組織の人たち（alice・bob はメンバー、admin は管理者でメンバー）。
struct Org {
    alice: AuthContext,
    bob: AuthContext,
    admin: AuthContext,
}

async fn new_org(c: &Ctx) -> Org {
    let org = format!("puorg{}", Uuid::new_v4().simple());
    let user = |p: &str| {
        make_ctx(
            &org,
            authz::PrincipalKind::User,
            &format!("{p}{}", Uuid::new_v4().simple()),
        )
    };
    let (alice, bob, admin) = (user("alice"), user("bob"), user("admin"));
    let org_obj = alice.ns().organization(&org);
    for u in [&alice, &bob, &admin] {
        c.authz
            .write_tuple(&u.subject(), Relation::Member, &org_obj)
            .await
            .expect("member");
        wait_check(c, u, Relation::Member, &org_obj).await;
    }
    c.authz
        .write_tuple(&admin.subject(), Relation::Admin, &org_obj)
        .await
        .expect("admin");
    wait_check(c, &admin, Relation::Admin, &org_obj).await;
    Org { alice, bob, admin }
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

/// 条件が成り立つまで待つ（片付けは掃除が先に取ることがある）。
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
    format!("purge-it {tag} {}", Uuid::new_v4()).into_bytes()
}

async fn write(c: &Ctx, ctx: &AuthContext, parent: Option<Uuid>, name: &str, bytes: &[u8]) -> Node {
    let node = c
        .service
        .write_file_internal(ctx, parent, name, bytes, "text/plain", None)
        .await
        .expect("書き込み");
    wait_check(
        c,
        ctx,
        Relation::Editor,
        &ctx.ns().file(&node.id.to_string()),
    )
    .await;
    node
}

async fn folder(c: &Ctx, ctx: &AuthContext, parent: Option<Uuid>, name: &str) -> Node {
    let node = c
        .service
        .create_folder(ctx, parent, name, None)
        .await
        .expect("フォルダ");
    wait_check(
        c,
        ctx,
        Relation::Editor,
        &ctx.ns().folder(&node.id.to_string()),
    )
    .await;
    node
}

async fn node_exists(c: &Ctx, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node WHERE tenant_id = $1 AND id = $2)")
        .bind(TENANT)
        .bind(id)
        .fetch_one(&c.pool)
        .await
        .unwrap()
}

/// ノードを指す行（closure・版・共有リンク）の数。
async fn rows_referring(c: &Ctx, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM node_closure WHERE tenant_id = $1 AND (ancestor = $2 OR descendant = $2)) \
              + (SELECT count(*) FROM node_version WHERE tenant_id = $1 AND node_id = $2) \
              + (SELECT count(*) FROM node_share_link WHERE tenant_id = $1 AND node_id = $2)",
    )
    .bind(TENANT)
    .bind(id)
    .fetch_one(&c.pool)
    .await
    .unwrap()
}

async fn refcount(c: &Ctx, ctx: &AuthContext, sha: &str) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT refcount FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(sha)
    .fetch_optional(&c.pool)
    .await
    .unwrap()
}

async fn tuples_on(c: &Ctx, obj: &FgaObject) -> usize {
    c.authz.read_tuples(obj, None).await.unwrap().len()
}

async fn body_exists(c: &Ctx, ctx: &AuthContext, sha: &str) -> bool {
    c.store
        .exists(&blob_object_key(ctx.tenant_id(), ctx.org(), sha))
        .await
        .unwrap()
}

async fn purged(c: &Ctx, ctx: &AuthContext, id: Uuid) -> storage::service::PurgeReport {
    match c.service.purge_node(ctx, id, None).await {
        Ok(PurgeOutcome::Purged(r)) => r,
        other => panic!("完全に削除できること: {other:?}"),
    }
}

/// 完全に削除すると、行・タプルが残らず、ほかと共有していない本体だけが消える。同じ内容のほかのファイルは
/// 読める。delete のイベントは消した版 + 1 で出て、監査はチェーンにつながる。
#[tokio::test]
async fn purge_removes_rows_tuples_and_only_unshared_bodies() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let shared = fresh("shared");
    let own = fresh("own");
    let (shared_sha, own_sha) = (sha256_hex(&shared), sha256_hex(&own));
    let dir = folder(&c, &o.alice, None, "消すフォルダ").await;
    let a = write(&c, &o.alice, Some(dir.id), "a.txt", &shared).await;
    let b = write(&c, &o.alice, Some(dir.id), "b.txt", &own).await;
    let keep = write(&c, &o.alice, None, "keep.txt", &shared).await;
    // 共有リンクの台帳の行（purge で消える）。
    sqlx::query(
        "INSERT INTO node_share_link (link_id, node_id, tenant_id, org, kind, audience, role, token, created_by, updated_by) \
         VALUES ($1, $2, $3, $4, 'file', 'restricted', 'viewer', $5, 'x', 'x')",
    )
    .bind(Uuid::new_v4())
    .bind(a.id)
    .bind(TENANT)
    .bind(o.alice.org())
    .bind(Uuid::new_v4().to_string())
    .execute(&c.pool)
    .await
    .unwrap();
    assert_eq!(refcount(&c, &o.alice, &shared_sha).await, Some(2));
    c.service
        .soft_delete_folder(&o.alice, dir.id, None)
        .await
        .unwrap();
    let a_version: i64 =
        sqlx::query_scalar("SELECT version FROM node WHERE tenant_id = $1 AND id = $2")
            .bind(TENANT)
            .bind(a.id)
            .fetch_one(&c.pool)
            .await
            .unwrap();

    let report = purged(&c, &o.alice, dir.id).await;
    assert_eq!((report.nodes, report.files, report.versions), (3, 2, 2));
    assert_eq!(report.kind, NodeKind::Folder);
    assert_eq!(report.freed_bytes, own.len() as i64, "共有の本体は数えない");

    for id in [dir.id, a.id, b.id] {
        assert!(!node_exists(&c, id).await, "ノードの行が残らない");
        assert_eq!(
            rows_referring(&c, id).await,
            0,
            "closure・版・共有リンクが残らない"
        );
    }
    assert_eq!(
        refcount(&c, &o.alice, &shared_sha).await,
        Some(1),
        "共有の blob は 1 つ減る"
    );
    assert_eq!(
        refcount(&c, &o.alice, &own_sha).await,
        None,
        "参照 0 の blob の行は消える"
    );
    for obj in [
        o.alice.ns().folder(&dir.id.to_string()),
        o.alice.ns().file(&a.id.to_string()),
        o.alice.ns().file(&b.id.to_string()),
    ] {
        let (cr, objr) = (&c, &obj);
        eventually("タプルが外れる", move || async move {
            tuples_on(cr, objr).await == 0
        })
        .await;
    }
    let (cr, ar, sr) = (&c, &o.alice, &own_sha);
    eventually("参照 0 の本体が消える", move || async move {
        !body_exists(cr, ar, sr).await
    })
    .await;
    assert!(
        body_exists(&c, &o.alice, &shared_sha).await,
        "共有の本体は残る"
    );
    let (_, read) = c
        .service
        .read_file_internal(&o.alice, keep.id, None)
        .await
        .expect("同じ内容のほかのファイルは読める");
    assert_eq!(read, shared);

    // ファイルごとの delete（消した版 + 1・purge の印）。フォルダのイベントは出さない。
    let ev: Vec<(i64, serde_json::Value)> = sqlx::query_as(
        "SELECT version, payload FROM storage_event_outbox \
         WHERE tenant_id = $1 AND node_id = $2 AND op = 'delete' ORDER BY id",
    )
    .bind(TENANT)
    .bind(a.id)
    .fetch_all(&c.pool)
    .await
    .unwrap();
    assert_eq!(
        ev.len(),
        1,
        "論理削除はフォルダのイベントだけで、ファイルの delete は完全削除の 1 件"
    );
    assert_eq!(ev[0].0, a_version + 1);
    assert_eq!(ev[0].1["purge"], serde_json::json!(true));

    // 監査: folder.purge がチェーンにつながり、名前は入らない。チェーンの検証が通る。
    let (meta, chained): (serde_json::Value, bool) = sqlx::query_as(
        "SELECT metadata, chained FROM audit_log \
         WHERE tenant_id = $1 AND org = $2 AND action = 'folder.purge' AND object_id = $3",
    )
    .bind(TENANT)
    .bind(o.alice.org())
    .bind(dir.id.to_string())
    .fetch_one(&c.pool)
    .await
    .unwrap();
    assert!(chained);
    assert_eq!(meta["subtree_count"], 3);
    assert_eq!(meta["version_count"], 2);
    assert!(!meta.to_string().contains("消すフォルダ"), "名前を入れない");
    let report = AuditRecorder::new(c.pool.clone())
        .chain_verifier()
        .verify(&o.alice, None, 1000)
        .await
        .unwrap();
    assert!(report.broken.is_none(), "チェーンが崩れない: {report:?}");
    assert!(report.complete);
}

/// 消せるのは根の owner と組織の管理者。editor だけの人は 403（deny を監査に残す）。workflow が owner の
/// ファイルは管理者だけが消せる。
#[tokio::test]
async fn only_owner_or_admin_can_purge() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let dir = folder(&c, &o.alice, None, "共有").await;
    c.service
        .share_node(
            &o.alice,
            dir.id,
            &ShareTarget::User {
                id: o.bob.principal().id.clone(),
            },
            ShareRole::Editor,
            None,
        )
        .await
        .unwrap();
    wait_check(
        &c,
        &o.bob,
        Relation::Editor,
        &o.bob.ns().folder(&dir.id.to_string()),
    )
    .await;
    let bobs = write(&c, &o.bob, Some(dir.id), "bob.txt", &fresh("bob")).await;
    c.service
        .soft_delete_folder(&o.bob, dir.id, None)
        .await
        .unwrap();

    let err = c
        .service
        .purge_node(&o.bob, dir.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Forbidden), "{err:?}");
    let deny: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND org = $2 \
         AND action = 'folder.purge' AND decision = 'deny' AND object_id = $3",
    )
    .bind(TENANT)
    .bind(o.bob.org())
    .bind(dir.id.to_string())
    .fetch_one(&c.pool)
    .await
    .unwrap();
    assert_eq!(deny, 1);
    assert!(node_exists(&c, dir.id).await && node_exists(&c, bobs.id).await);

    // フォルダの持ち主は、その下のほかの人のファイルもまとめて消せる。
    let r = purged(&c, &o.alice, dir.id).await;
    assert_eq!(r.files, 1);
    assert!(!node_exists(&c, bobs.id).await);

    // 管理者は持ち主でなくても消せる（editor でなくてよい）。
    let alices = write(&c, &o.alice, None, "alice.txt", &fresh("alice")).await;
    c.service
        .soft_delete_file(&o.alice, alices.id, None)
        .await
        .unwrap();
    purged(&c, &o.admin, alices.id).await;

    // workflow が owner のファイル: 置き場所のフォルダの持ち主でも、ファイルの根は消せない。管理者は消せる。
    let wf_dir = folder(&c, &o.alice, None, "wf").await;
    let wf = make_ctx(
        o.alice.org(),
        authz::PrincipalKind::Workflow,
        &format!("wf{}", Uuid::new_v4().simple()),
    );
    let wf_dir_obj = o.alice.ns().folder(&wf_dir.id.to_string());
    c.authz
        .write_tuple(&wf.subject(), Relation::Editor, &wf_dir_obj)
        .await
        .unwrap();
    wait_check(&c, &wf, Relation::Editor, &wf_dir_obj).await;
    let made = c
        .service
        .write_file_internal(
            &wf,
            Some(wf_dir.id),
            "wf.txt",
            &fresh("wf"),
            "text/plain",
            None,
        )
        .await
        .unwrap();
    wait_check(
        &c,
        &o.alice,
        Relation::Editor,
        &o.alice.ns().file(&made.id.to_string()),
    )
    .await;
    c.service
        .soft_delete_file(&o.alice, made.id, None)
        .await
        .unwrap();
    let err = c
        .service
        .purge_node(&o.alice, made.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Forbidden), "{err:?}");
    purged(&c, &o.admin, made.id).await;
    assert!(!node_exists(&c, made.id).await);
}

/// 対象の決まり: ゴミ箱に無い → NotInTrash・システム領域 → 404・根でない削除済み → InsideDeletedFolder・
/// ほかの組織 → 404。
#[tokio::test]
async fn purge_refuses_what_is_not_a_trash_root() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let live = write(&c, &o.alice, None, "live.txt", &fresh("live")).await;
    assert_eq!(
        c.service.purge_node(&o.alice, live.id, None).await.unwrap(),
        PurgeOutcome::NotInTrash
    );
    assert!(node_exists(&c, live.id).await);

    let sys = c
        .service
        .create_system_folder(&o.alice, None, "ws", None)
        .await
        .unwrap();
    wait_check(
        &c,
        &o.alice,
        Relation::Editor,
        &o.alice.ns().folder(&sys.id.to_string()),
    )
    .await;
    c.service
        .soft_delete_folder(&o.alice, sys.id, None)
        .await
        .unwrap();
    let err = c
        .service
        .purge_node(&o.alice, sys.id, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound), "{err:?}");

    let dir = folder(&c, &o.alice, None, "d").await;
    let inner = write(&c, &o.alice, Some(dir.id), "inner.txt", &fresh("inner")).await;
    c.service
        .soft_delete_folder(&o.alice, dir.id, None)
        .await
        .unwrap();
    assert_eq!(
        c.service
            .purge_node(&o.alice, inner.id, None)
            .await
            .unwrap(),
        PurgeOutcome::InsideDeletedFolder
    );
    assert!(node_exists(&c, inner.id).await);

    let other = new_org(&c).await;
    let err = c
        .service
        .purge_node(&other.admin, dir.id, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StorageError::NotFound),
        "ほかの組織からは見えない: {err:?}"
    );
    let err = c
        .service
        .purge_node(&o.alice, Uuid::new_v4(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound), "{err:?}");
}

/// 削除したフォルダの下に生きたファイルがある（7p-4 より前に崩れた行）と 409 にし、行も本体もタプルも残す。
#[tokio::test]
async fn purge_refuses_folder_with_live_descendant() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let bytes = fresh("alive");
    let sha = sha256_hex(&bytes);
    let dir = folder(&c, &o.alice, None, "崩れた").await;
    let alive = write(&c, &o.alice, Some(dir.id), "alive.txt", &bytes).await;
    c.service
        .soft_delete_folder(&o.alice, dir.id, None)
        .await
        .unwrap();
    sqlx::query("UPDATE node SET deleted_at = NULL WHERE tenant_id = $1 AND id = $2")
        .bind(TENANT)
        .bind(alive.id)
        .execute(&c.pool)
        .await
        .unwrap();

    assert_eq!(
        c.service.purge_node(&o.alice, dir.id, None).await.unwrap(),
        PurgeOutcome::HasLiveDescendants
    );
    assert!(node_exists(&c, dir.id).await && node_exists(&c, alive.id).await);
    assert!(rows_referring(&c, alive.id).await > 0);
    assert_eq!(refcount(&c, &o.alice, &sha).await, Some(1));
    assert!(body_exists(&c, &o.alice, &sha).await);
    assert!(tuples_on(&c, &o.alice.ns().file(&alive.id.to_string())).await > 0);
    assert!(tuples_on(&c, &o.alice.ns().folder(&dir.id.to_string())).await > 0);
}

/// 復元と完全削除を並べると、片方だけが勝つ（ファイル・フォルダ）。
#[tokio::test]
async fn restore_and_purge_race_has_one_winner() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    for round in 0..4 {
        let f = write(&c, &o.alice, None, &format!("r{round}.txt"), &fresh("race")).await;
        c.service
            .soft_delete_file(&o.alice, f.id, None)
            .await
            .unwrap();
        let (restored, purge) = tokio::join!(
            c.service.restore_file(&o.alice, f.id, None),
            c.service.purge_node(&o.alice, f.id, None),
        );
        match (&restored, &purge) {
            (Ok(_), Ok(PurgeOutcome::NotInTrash)) => assert!(node_exists(&c, f.id).await),
            (Err(StorageError::NotFound), Ok(PurgeOutcome::Purged(_))) => {
                assert!(!node_exists(&c, f.id).await)
            }
            other => panic!("片方だけが勝つこと: {other:?}"),
        }
    }
    for round in 0..3 {
        let dir = folder(&c, &o.alice, None, &format!("rd{round}")).await;
        let mut children = Vec::new();
        for i in 0..3 {
            children.push(
                write(
                    &c,
                    &o.alice,
                    Some(dir.id),
                    &format!("{i}.txt"),
                    &fresh("rd"),
                )
                .await
                .id,
            );
        }
        c.service
            .soft_delete_folder(&o.alice, dir.id, None)
            .await
            .unwrap();
        let (restored, purge) = tokio::join!(
            c.service.restore_folder(&o.alice, dir.id, None),
            c.service.purge_node(&o.alice, dir.id, None),
        );
        let alive = node_exists(&c, dir.id).await;
        for id in &children {
            assert_eq!(
                node_exists(&c, *id).await,
                alive,
                "配下はフォルダと一緒に残るか消える"
            );
        }
        match (&restored, &purge) {
            (Ok(_), Ok(PurgeOutcome::NotInTrash)) => assert!(alive),
            (Err(StorageError::NotFound), Ok(PurgeOutcome::Purged(_))) => assert!(!alive),
            // 復元の UPDATE は配下を id の順に押さえないので、デッドロックで片方が落ちることがある（勝つのは残る方）。
            (Err(StorageError::Db(_)), Ok(PurgeOutcome::Purged(_))) => assert!(!alive),
            (Ok(_), Err(StorageError::Db(_))) => assert!(alive),
            other => panic!("片方だけが勝つこと: {other:?}"),
        }
    }
}

/// 同じ内容（sha）を共有する 2 つのフォルダを並べて完全に削除しても、デッドロックにならず両方消える。
#[tokio::test]
async fn concurrent_purges_sharing_blobs_do_not_deadlock() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let (x, y) = (fresh("x"), fresh("y"));
    let (one, two) = (
        folder(&c, &o.alice, None, "one").await,
        folder(&c, &o.alice, None, "two").await,
    );
    // 作る順を逆にして、行の並びと sha の並びを食い違わせる。
    write(&c, &o.alice, Some(one.id), "x.txt", &x).await;
    write(&c, &o.alice, Some(one.id), "y.txt", &y).await;
    write(&c, &o.alice, Some(two.id), "y.txt", &y).await;
    write(&c, &o.alice, Some(two.id), "x.txt", &x).await;
    for d in [&one, &two] {
        c.service
            .soft_delete_folder(&o.alice, d.id, None)
            .await
            .unwrap();
    }
    let (a, b) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(
            c.service.purge_node(&o.alice, one.id, None),
            c.service.purge_node(&o.alice, two.id, None),
        )
    })
    .await
    .expect("止まらない");
    assert!(matches!(a, Ok(PurgeOutcome::Purged(_))), "{a:?}");
    assert!(matches!(b, Ok(PurgeOutcome::Purged(_))), "{b:?}");
    for bytes in [&x, &y] {
        let sha = sha256_hex(bytes);
        assert_eq!(refcount(&c, &o.alice, &sha).await, None);
        let (cr, ar, sr) = (&c, &o.alice, &sha);
        eventually("本体が消える", move || async move {
            !body_exists(cr, ar, sr).await
        })
        .await;
    }
}

/// ゴミ箱の一覧に、消せるかと、フォルダの配下のファイルの数（ほかの人が作った数）が添う。
#[tokio::test]
async fn trash_entries_carry_can_purge_and_counts() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let dir = folder(&c, &o.alice, None, "数える").await;
    c.service
        .share_node(
            &o.alice,
            dir.id,
            &ShareTarget::User {
                id: o.bob.principal().id.clone(),
            },
            ShareRole::Editor,
            None,
        )
        .await
        .unwrap();
    wait_check(
        &c,
        &o.bob,
        Relation::Editor,
        &o.bob.ns().folder(&dir.id.to_string()),
    )
    .await;
    write(&c, &o.alice, Some(dir.id), "a.txt", &fresh("a")).await;
    write(&c, &o.bob, Some(dir.id), "b.txt", &fresh("b")).await;
    let file = write(&c, &o.alice, None, "f.txt", &fresh("f")).await;
    c.service
        .soft_delete_folder(&o.alice, dir.id, None)
        .await
        .unwrap();
    c.service
        .soft_delete_file(&o.alice, file.id, None)
        .await
        .unwrap();

    let alice = c
        .service
        .list_trash_entries(&o.alice, None, 50, None)
        .await
        .unwrap();
    let d = alice.items.iter().find(|e| e.node.id == dir.id).unwrap();
    assert!(d.can_purge);
    assert_eq!((d.file_count, d.others_file_count), (Some(2), Some(1)));
    let f = alice.items.iter().find(|e| e.node.id == file.id).unwrap();
    assert!(f.can_purge);
    assert_eq!((f.file_count, f.others_file_count), (None, None));

    let bob = c
        .service
        .list_trash_entries(&o.bob, None, 50, None)
        .await
        .unwrap();
    assert_eq!(bob.items.len(), 1, "bob の一覧は共有されたフォルダだけ");
    let d = &bob.items[0];
    assert!(!d.can_purge, "editor だけでは消せない");
    assert_eq!((d.file_count, d.others_file_count), (Some(2), Some(1)));
}

/// 「空にする」は呼んだ人の一覧に出るものだけを消す。管理者が空にしても、管理者の一覧に出ない他人の根は残る
/// （crit の P1）。一覧に出るが消せないものは skipped に数えて残す。
#[tokio::test]
async fn empty_trash_takes_only_the_callers_listed_roots() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let alices = write(&c, &o.alice, None, "alice.txt", &fresh("a")).await;
    let bobs = write(&c, &o.bob, None, "bob.txt", &fresh("b")).await;
    let admins = write(&c, &o.admin, None, "admin.txt", &fresh("m")).await;
    c.service
        .soft_delete_file(&o.alice, alices.id, None)
        .await
        .unwrap();
    c.service
        .soft_delete_file(&o.bob, bobs.id, None)
        .await
        .unwrap();
    c.service
        .soft_delete_file(&o.admin, admins.id, None)
        .await
        .unwrap();

    let out = c.service.empty_trash(&o.admin, None, None).await.unwrap();
    assert_eq!(
        (out.purged, out.skipped, out.next_cursor.as_deref()),
        (1, 0, None)
    );
    assert!(!node_exists(&c, admins.id).await);
    assert!(
        node_exists(&c, alices.id).await,
        "管理者の一覧に出ない他人の根は残る"
    );
    assert!(
        node_exists(&c, bobs.id).await,
        "管理者の一覧に出ない他人の根は残る"
    );

    // bob の一覧: 自分のファイルと、alice から editor で共有されたフォルダ（bob は消せない）。
    let shared = folder(&c, &o.alice, None, "共有").await;
    c.service
        .share_node(
            &o.alice,
            shared.id,
            &ShareTarget::User {
                id: o.bob.principal().id.clone(),
            },
            ShareRole::Editor,
            None,
        )
        .await
        .unwrap();
    wait_check(
        &c,
        &o.bob,
        Relation::Editor,
        &o.bob.ns().folder(&shared.id.to_string()),
    )
    .await;
    c.service
        .soft_delete_folder(&o.bob, shared.id, None)
        .await
        .unwrap();
    let out = c.service.empty_trash(&o.bob, None, None).await.unwrap();
    assert_eq!(
        (out.purged, out.skipped, out.next_cursor.as_deref()),
        (1, 1, None)
    );
    assert!(!node_exists(&c, bobs.id).await);
    assert!(
        node_exists(&c, shared.id).await,
        "一覧に出るが消せないものは残る"
    );

    let rows: Vec<(serde_json::Value, bool)> = sqlx::query_as(
        "SELECT metadata, chained FROM audit_log WHERE tenant_id = $1 AND org = $2 \
         AND action = 'trash.empty' ORDER BY id",
    )
    .bind(TENANT)
    .bind(o.bob.org())
    .fetch_all(&c.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "1 回の呼び出しで 1 行");
    assert!(
        rows.iter().all(|(_, chained)| !chained),
        "trash.empty はチェーンしない"
    );
    assert_eq!(rows[1].0["skipped"], 1);
}

/// 上限を超える数の根は、next_cursor をたどると全部消える。
#[tokio::test]
async fn empty_trash_follows_cursor_until_everything_is_gone() {
    let Some(c) = setup().await else { return };
    let o = new_org(&c).await;
    let total = EMPTY_TRASH_MAX_ROOTS + 2;
    let mut ids = Vec::with_capacity(total);
    for i in 0..total {
        let n = c
            .service
            .write_file_internal(
                &o.alice,
                None,
                &format!("{i}.txt"),
                &fresh("many"),
                "text/plain",
                None,
            )
            .await
            .unwrap();
        ids.push(n.id);
    }
    let last = ids[total - 1];
    wait_check(
        &c,
        &o.alice,
        Relation::Editor,
        &o.alice.ns().file(&last.to_string()),
    )
    .await;
    for id in &ids {
        c.service
            .soft_delete_file(&o.alice, *id, None)
            .await
            .unwrap();
    }
    let mut cursor: Option<String> = None;
    let mut purged = 0;
    let mut calls = 0;
    loop {
        let out = c
            .service
            .empty_trash(&o.alice, cursor.as_deref(), None)
            .await
            .unwrap();
        assert!(out.purged <= EMPTY_TRASH_MAX_ROOTS);
        purged += out.purged;
        calls += 1;
        cursor = out.next_cursor;
        if cursor.is_none() {
            break;
        }
        assert!(calls < 10, "続きが終わらない");
    }
    assert_eq!(purged, total);
    assert!(calls >= 2, "上限で区切って続きを返す");
    for id in ids {
        assert!(!node_exists(&c, id).await);
    }
    let err = c
        .service
        .empty_trash(&o.alice, Some("zz"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Invalid(_)), "{err:?}");
}
