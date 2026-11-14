//! ゴミ箱の完全削除（GAP-108b・D-201）の、大きさと並行の結合テスト（`purge_it` の続き）。
//! 実 Postgres・RustFS・OpenFGA が必要。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行する（未設定ならスキップ・
//! `SHIKI_REQUIRE_IT=1` なら失敗）。
//!
//! - 異なる本体が共有ロック表の大きさより多い根も、1 回で消せる（blob を行ロックで押さえる）。
//! - 「ゴミ箱を空にする」の続きの呼び出しは、最初の呼び出しの後にゴミ箱に入れたものを消さない。
//! - 共有リンクの解錠（redeem）は、完全削除と直列になる（消したノードに台帳の行と via_link のタプルを残さない）。

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
use storage::content_address::sha256_hex;
use storage::object_store::S3Config;
use storage::service::{PurgeOutcome, EMPTY_TRASH_MAX_ROOTS};
use storage::{
    GeneralAccessLevel, Node, ObjectStore, S3ObjectStore, ShareRole, StorageError, StorageService,
};
use uuid::Uuid;

const TENANT: &str = "default";

/// 共有ロック表より多い、異なる本体の数。表は PG16 の既定で `64 × (max_connections + 補助のプロセス)` ≒ 7,800 項目に
/// 共有メモリの余りを足した大きさで、手元の compose（既定の設定）では 1 つの Tx の advisory lock が 12,000〜13,000 で
/// `out of shared memory` になる。
const MANY_BLOBS: i64 = 20_000;

struct Ctx {
    service: Arc<StorageService>,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
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
            store_name: format!("shiki-purge-conc-it-{}", Uuid::new_v4().simple()),
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
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    Some(Ctx {
        service,
        pool,
        authz,
    })
}

fn user(org: &str, prefix: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: format!("{prefix}{}", Uuid::new_v4().simple()),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        authz::TenantId::parse(TENANT).unwrap(),
    )
}

/// 1 つの組織のメンバー alice・bob。
async fn new_org(c: &Ctx) -> (AuthContext, AuthContext) {
    let org = format!("pcorg{}", Uuid::new_v4().simple());
    let (alice, bob) = (user(&org, "alice"), user(&org, "bob"));
    let org_obj = alice.ns().organization(&org);
    for u in [&alice, &bob] {
        c.authz
            .write_tuple(&u.subject(), Relation::Member, &org_obj)
            .await
            .expect("member");
        wait_check(c, u, Relation::Member, &org_obj).await;
    }
    (alice, bob)
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

async fn write(c: &Ctx, ctx: &AuthContext, name: &str, bytes: &[u8]) -> Node {
    let node = c
        .service
        .write_file_internal(ctx, None, name, bytes, "text/plain", None)
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

async fn node_exists(c: &Ctx, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node WHERE tenant_id = $1 AND id = $2)")
        .bind(TENANT)
        .bind(id)
        .fetch_one(&c.pool)
        .await
        .unwrap()
}

/// 同じ DB で、`needle` を含む問い合わせが行ロックを待っているか（ほかのテストの advisory lock の待ちは数えない）。
async fn waiting_on_row(c: &Ctx, needle: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity \
         WHERE datname = current_database() AND pid <> pg_backend_pid() \
           AND wait_event_type = 'Lock' AND wait_event IN ('transactionid', 'tuple') \
           AND strpos(query, $1) > 0)",
    )
    .bind(needle)
    .fetch_one(&c.pool)
    .await
    .unwrap()
}

/// 異なる本体が共有ロック表の大きさより多い根も、1 つの Tx で消しきる（本体ごとの advisory lock を取らない）。
/// 版の行と blob の行は SQL で足す（本体は置かない・片付けは存在しない鍵の削除で冪等に済む）。
#[tokio::test]
async fn purge_root_with_more_blobs_than_the_lock_table() {
    let Some(c) = setup().await else { return };
    let (alice, _) = new_org(&c).await;
    let file = write(&c, &alice, "many-versions.txt", b"many versions").await;
    let mut tx = c.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO blob (tenant_id, org, sha256, size_bytes, content_type, object_key, refcount) \
         SELECT $1, $2, md5($3 || g::text) || md5(g::text || $3), 1, 'text/plain', \
                'purge-conc-it/' || g::text, 1 \
         FROM generate_series(1, $4::bigint) AS g",
    )
    .bind(TENANT)
    .bind(alice.org())
    .bind(file.id.to_string())
    .bind(MANY_BLOBS)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_version \
           (node_id, version, org, tenant_id, blob_sha256, size_bytes, content_type, author) \
         SELECT $1, 1000 + g, $2, $3, md5($4 || g::text) || md5(g::text || $4), 1, 'text/plain', 'it' \
         FROM generate_series(1, $5::bigint) AS g",
    )
    .bind(file.id)
    .bind(alice.org())
    .bind(TENANT)
    .bind(file.id.to_string())
    .bind(MANY_BLOBS)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    c.service
        .soft_delete_file(&alice, file.id, None)
        .await
        .unwrap();

    let out = c.service.purge_node(&alice, file.id, None).await;
    // 片付け待ちに残った本体の行は、存在しない鍵なので、ほかのテストの掃除に回さずに消しておく。
    let cleanup = || async {
        sqlx::query(
            "DELETE FROM storage_purge_pending WHERE tenant_id = $1 AND org = $2 AND kind = 'object'",
        )
        .bind(TENANT)
        .bind(alice.org())
        .execute(&c.pool)
        .await
        .unwrap();
    };
    cleanup().await;
    let report = match out {
        Ok(PurgeOutcome::Purged(r)) => r,
        other => panic!("1 回で消しきれること: {other:?}"),
    };
    assert_eq!(report.versions as i64, MANY_BLOBS + 1);
    assert!(!node_exists(&c, file.id).await);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM blob WHERE tenant_id = $1 AND org = $2")
            .bind(TENANT)
            .bind(alice.org())
            .fetch_one(&c.pool)
            .await
            .unwrap();
    assert_eq!(left, 0, "参照が 0 になった blob の行は残らない");
}

/// 「空にする」の続きの呼び出しは、最初の呼び出しの後にゴミ箱に入れたものを消さない（確認の後に入れたものを、
/// 元に戻す機会なしに消さない）。続きの位置なしで呼び直せば消せる。
#[tokio::test]
async fn empty_trash_continuation_keeps_what_was_trashed_after_the_first_call() {
    let Some(c) = setup().await else { return };
    let (alice, _) = new_org(&c).await;
    let total = EMPTY_TRASH_MAX_ROOTS + 1;
    let bytes = format!("purge-conc-it upper {}", Uuid::new_v4()).into_bytes();
    let mut ids = Vec::with_capacity(total);
    for i in 0..total {
        let n = c
            .service
            .write_file_internal(
                &alice,
                None,
                &format!("{i}.txt"),
                &bytes,
                "text/plain",
                None,
            )
            .await
            .unwrap();
        ids.push(n.id);
    }
    wait_check(
        &c,
        &alice,
        Relation::Editor,
        &alice.ns().file(&ids[total - 1].to_string()),
    )
    .await;
    for id in &ids {
        c.service.soft_delete_file(&alice, *id, None).await.unwrap();
    }
    let first = c.service.empty_trash(&alice, None, None).await.unwrap();
    assert_eq!(first.purged, EMPTY_TRASH_MAX_ROOTS);
    let mut cursor = first.next_cursor.expect("上限で区切って続きを返す");

    // 最初の呼び出しの後に、ゴミ箱に入れる。
    let late = write(&c, &alice, "late.txt", b"late").await;
    c.service
        .soft_delete_file(&alice, late.id, None)
        .await
        .unwrap();

    let mut purged = first.purged;
    for _ in 0..10 {
        let out = c
            .service
            .empty_trash(&alice, Some(&cursor), None)
            .await
            .unwrap();
        purged += out.purged;
        match out.next_cursor {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(purged, total, "最初の呼び出しの時点のものは消しきる");
    for id in &ids {
        assert!(!node_exists(&c, *id).await);
    }
    assert!(
        node_exists(&c, late.id).await,
        "最初の呼び出しの後に入れたものは残る"
    );

    let again = c.service.empty_trash(&alice, None, None).await.unwrap();
    assert_eq!((again.purged, again.next_cursor), (1, None));
    assert!(!node_exists(&c, late.id).await, "呼び直せば消せる");
}

/// 「空にする」が候補を選んでから根を押さえるまでの間に、元に戻してゴミ箱へ入れ直したものは消さない（押した時刻
/// より後に入れたものになる・D-221）。その隙間は、根の行を別の Tx で押さえて作り、待っている間に削除の時刻を
/// 押した後へ置き直す（元に戻して入れ直すのと同じ行になる）。
#[tokio::test]
async fn empty_trash_keeps_what_was_trashed_again_after_it_started() {
    let Some(c) = setup().await else { return };
    let (alice, _) = new_org(&c).await;
    let file = write(&c, &alice, "again.txt", b"purge-conc-it trashed again").await;
    wait_check(
        &c,
        &alice,
        Relation::Editor,
        &alice.ns().file(&file.id.to_string()),
    )
    .await;
    c.service
        .soft_delete_file(&alice, file.id, None)
        .await
        .unwrap();

    let mut hold = c.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM node WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(TENANT)
        .bind(file.id)
        .execute(&mut *hold)
        .await
        .unwrap();
    let empty = {
        let (service, alice) = (c.service.clone(), alice.clone());
        tokio::spawn(async move { service.empty_trash(&alice, None, None).await })
    };
    // 「空にする」が候補を選び、根の行を待つまで。
    for _ in 0..200 {
        if empty.is_finished() || waiting_on_row(&c, "FOR UPDATE OF n").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    sqlx::query("UPDATE node SET deleted_at = clock_timestamp() WHERE tenant_id = $1 AND id = $2")
        .bind(TENANT)
        .bind(file.id)
        .execute(&mut *hold)
        .await
        .unwrap();
    hold.commit().await.unwrap();
    let out = tokio::time::timeout(Duration::from_secs(60), empty)
        .await
        .expect("止まらない")
        .unwrap()
        .unwrap();
    assert_eq!(out.purged, 0, "押した後に入れ直したものは消さない: {out:?}");
    assert!(node_exists(&c, file.id).await);

    let again = c.service.empty_trash(&alice, None, None).await.unwrap();
    assert_eq!(again.purged, 1, "呼び直せば消せる: {again:?}");
    assert!(!node_exists(&c, file.id).await);
}

/// 完全削除がノードの行を押さえて消している間の redeem は、完全削除の commit を待ってから断られる（消した
/// ノードに台帳の行と via_link のタプルを残さない）。完全削除の DB の側は、`purge` と同じ順に行を押さえて
/// 消す Tx で模す（commit の前に redeem を走らせるため）。
#[tokio::test]
async fn redeem_waits_for_purge_and_leaves_no_grant() {
    let Some(c) = setup().await else { return };
    let (alice, bob) = new_org(&c).await;
    let bytes = format!("purge-conc-it redeem {}", Uuid::new_v4()).into_bytes();
    let sha = sha256_hex(&bytes);
    let file = write(&c, &alice, "鍵付き.txt", &bytes).await;
    let link = c
        .service
        .create_share_link(
            &alice,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-purge-conc"),
            None,
            None,
        )
        .await
        .unwrap();
    c.service
        .soft_delete_file(&alice, file.id, None)
        .await
        .unwrap();

    // 完全削除の途中（ノードの行を FOR UPDATE で押さえ、台帳・リンク・版・closure・ノード・blob の行を消した、
    // commit の前）。
    let mut purge = c.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM node WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(TENANT)
        .bind(file.id)
        .execute(&mut *purge)
        .await
        .unwrap();
    for sql in [
        "DELETE FROM node_share_link_grant WHERE tenant_id = $1 AND node_id = $2",
        "DELETE FROM node_share_link WHERE tenant_id = $1 AND node_id = $2",
        "DELETE FROM node_version WHERE tenant_id = $1 AND node_id = $2",
        "DELETE FROM node_closure WHERE tenant_id = $1 AND (descendant = $2 OR ancestor = $2)",
        "DELETE FROM node WHERE tenant_id = $1 AND id = $2",
    ] {
        sqlx::query(sql)
            .bind(TENANT)
            .bind(file.id)
            .execute(&mut *purge)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM blob WHERE tenant_id = $1 AND org = $2 AND sha256 = $3")
        .bind(TENANT)
        .bind(alice.org())
        .bind(&sha)
        .execute(&mut *purge)
        .await
        .unwrap();

    let redeem = {
        let (service, bob, token) = (c.service.clone(), bob.clone(), link.token.clone());
        tokio::spawn(async move {
            service
                .redeem_share_link(&bob, &token, Some("pw-purge-conc"), None)
                .await
        })
    };
    // redeem がノードの行を待つまで（直す前の形では待たずに終わる）。
    for _ in 0..200 {
        if redeem.is_finished() || waiting_on_row(&c, "FOR KEY SHARE").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    purge.commit().await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(60), redeem)
        .await
        .expect("止まらない")
        .unwrap();
    assert!(matches!(r, Err(StorageError::Forbidden)), "{r:?}");
    let grants: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node_share_link_grant WHERE tenant_id = $1 AND node_id = $2",
    )
    .bind(TENANT)
    .bind(file.id)
    .fetch_one(&c.pool)
    .await
    .unwrap();
    assert_eq!(grants, 0, "消したノードに台帳の行を残さない");
    let via_link = c
        .authz
        .read_tuples(
            &alice.ns().file(&file.id.to_string()),
            Some(Relation::ViewerViaLink),
        )
        .await
        .unwrap();
    assert!(via_link.is_empty(), "{via_link:?}");
}
