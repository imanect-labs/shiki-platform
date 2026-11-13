//! ドライブの「共有されたもの」を ListObjects の上限で切らない（BUG-162）の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! - viewer の実効集合が ListObjects の上限（1,000 件）を超えても、共有されたフォルダが一覧に出る（上限を超える
//!   タプルは OpenFGA へ直接書く。DB に無いノードのタプル＝自分の持ち物や消したノードの残りと同じ形）
//! - 共有していないノードは出ない。続きの位置で読み進めると、見えるノードを重ならず漏れなく新しい順に一巡する
//! - 見えないノードばかりが続くと、OpenFGA に問う数の上限で止まって続きの位置を返す（ページが空でも続きがある）。
//!   読み進めると残りが出る
//!
//! - 上限に届いた経路で候補を引く文は、並びの索引を逆にたどって打ち切る（引くたびに組織のノードを並べ直さない）
//!
//! 各テストは自分のテナント（`t-<uuid>`）と OpenFGA のストアに閉じ、最後に行とストアを消す（落ちても消す）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::visible::LIST_OBJECTS_MAX_RESULTS;
use authz::{AuthContext, AuthzClient, Principal, Relation};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::{
    object_store::S3Config, ObjectStore, S3ObjectStore, ShareRole, ShareTarget, SharedPosition,
    StorageService, SHARED_SCAN_MAX_CHECKS,
};
use uuid::Uuid;

/// 上限を超えるタプルを直接書くための OpenFGA の口（Write API は 1 回 100 件まで）と、ストアの片付け。
struct Fga {
    http: reqwest::Client,
    base_url: String,
    store_id: String,
    model_id: String,
}

impl Fga {
    async fn write(&self, tuples: &[(String, &str, String)]) {
        for chunk in tuples.chunks(100) {
            let keys: Vec<serde_json::Value> = chunk
                .iter()
                .map(|(u, r, o)| serde_json::json!({ "user": u, "relation": r, "object": o }))
                .collect();
            let resp = self
                .http
                .post(format!("{}/stores/{}/write", self.base_url, self.store_id))
                .json(&serde_json::json!({
                    "writes": { "tuple_keys": keys },
                    "authorization_model_id": self.model_id,
                }))
                .send()
                .await
                .expect("OpenFGA write");
            assert!(
                resp.status().is_success(),
                "write: {}",
                resp.text().await.unwrap_or_default()
            );
        }
    }

    async fn delete_store(&self) {
        let resp = self
            .http
            .delete(format!("{}/stores/{}", self.base_url, self.store_id))
            .send()
            .await
            .expect("OpenFGA delete store");
        assert!(
            resp.status().is_success(),
            "delete store: {}",
            resp.status()
        );
    }
}

struct Env {
    service: StorageService,
    pool: PgPool,
    fga: Fga,
    tenant: String,
    alice: AuthContext,
    bob: AuthContext,
}

const ORG: &str = "acme";

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
        ORG.into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(base_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
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
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-storage-shared-cap-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA 接続");
    let fga = Fga {
        http: reqwest::Client::new(),
        base_url,
        store_id: client.store_id().to_string(),
        model_id: client.model_id().to_string(),
    };
    let authz: Arc<dyn AuthzClient> = Arc::new(client);
    // フォルダしか作らない（テナントの撤去がオブジェクトのキーを列挙するので、結合テストの共用のバケットを使う）。
    let endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&S3Config {
        internal_endpoint: endpoint.clone(),
        public_endpoint: endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    }));
    store.ensure_bucket().await.expect("バケット準備");
    let service = StorageService::new(
        pool.clone(),
        store,
        Arc::clone(&authz),
        Duration::from_secs(300),
        Duration::from_secs(900),
        1024,
    );
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    // alice はドライブのトップにフォルダを作れる（組織の member）。
    authz
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().organization(ORG),
        )
        .await
        .unwrap();
    Some(Env {
        service,
        pool,
        fga,
        tenant,
        alice,
        bob,
    })
}

/// テストの本体を走らせ、落ちても後片付け（直接入れた行・テナント・ストア）をしてから結果を返す。
async fn run<F, Fut>(body: F)
where
    F: FnOnce(Arc<Env>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(env) = setup().await else { return };
    let env = Arc::new(env);
    let outcome = tokio::spawn(body(Arc::clone(&env))).await;
    // 直接入れた行はタプルも閉包も持たないので、テナントの撤去の前に消す。
    sqlx::query("DELETE FROM node WHERE tenant_id = $1 AND name LIKE '見えない-%'")
        .bind(&env.tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    let purged = env
        .service
        .purge_tenant(&env.tenant, ORG, "provisioner:test")
        .await;
    env.fga.delete_store().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
    purged.expect("テナントの撤去");
}

/// bob を viewer にした、DB に無いフォルダのタプル（実効集合を上限の外へ押し出す詰め物）。
fn fillers(env: &Env, n: usize) -> Vec<(String, &'static str, String)> {
    let ns = env.bob.ns();
    (0..n)
        .map(|_| {
            (
                env.bob.subject().to_string(),
                "viewer",
                ns.folder(&Uuid::new_v4().to_string()).as_str().to_string(),
            )
        })
        .collect()
}

/// alice がトップにフォルダを作り、`share` なら bob に閲覧で共有する。
async fn folder(env: &Env, name: &str, share: bool) -> Uuid {
    let f = env
        .service
        .create_folder(&env.alice, None, name, None)
        .await
        .unwrap();
    if share {
        env.service
            .share_node(
                &env.alice,
                f.id,
                &ShareTarget::User { id: "bob".into() },
                ShareRole::Viewer,
                None,
            )
            .await
            .unwrap();
    }
    f.id
}

/// bob の「共有されたもの」を続きがなくなるまで読む（ページごとの件数も返す）。
async fn read_all(env: &Env, limit: usize) -> (Vec<Uuid>, Vec<usize>) {
    let mut ids = Vec::new();
    let mut sizes = Vec::new();
    let mut after: Option<SharedPosition> = None;
    for _ in 0..50 {
        let page = env
            .service
            .list_shared_with_me(&env.bob, after, limit, None)
            .await
            .unwrap();
        sizes.push(page.items.len());
        ids.extend(page.items.iter().map(|n| n.id));
        match page.next {
            Some(p) => after = Some(p),
            None => return (ids, sizes),
        }
    }
    panic!("続きの位置が終わらない");
}

#[tokio::test]
async fn shared_folders_beyond_the_list_objects_cap_are_listed() {
    run(|env| async move {
        // 詰め物を先に積み、本物の共有は後から張る（ListObjects が返す 1,000 件に入らない位置）。
        env.fga
            .write(&fillers(&env, LIST_OBJECTS_MAX_RESULTS + 100))
            .await;
        let mut shared = Vec::new();
        for i in 0..5 {
            shared.push(folder(&env, &format!("共有-{i}"), true).await);
        }
        let private = folder(&env, "共有しない", false).await;

        let (ids, _) = read_all(&env, 2).await;
        let got: HashSet<Uuid> = ids.iter().copied().collect();
        assert_eq!(got.len(), ids.len(), "ページが重ならない");
        assert_eq!(
            got,
            shared.iter().copied().collect(),
            "共有したものだけ漏れなく"
        );
        assert!(!got.contains(&private));
        // 新しい順（あとで作ったものが先）。
        let mut newest_first = shared.clone();
        newest_first.reverse();
        assert_eq!(ids, newest_first);
        // 作った本人の「共有されたもの」には出ない（作成者を除く）。
        let mine = env
            .service
            .list_shared_with_me(&env.alice, None, 50, None)
            .await
            .unwrap();
        assert!(mine.items.is_empty());
    })
    .await;
}

#[tokio::test]
async fn invisible_nodes_stop_at_the_check_budget_and_the_rest_follows() {
    run(|env| async move {
        env.fga.write(&fillers(&env, LIST_OBJECTS_MAX_RESULTS + 10)).await;
        let shared = folder(&env, "共有-古い", true).await;
        // 共有したものより新しい、bob に見えないノードを問う数の上限より多く入れる（ほかの人の非公開のフォルダと
        // 同じ形）。並びは更新の新しい順なので、共有したものに着く前に問う数の上限に届く。
        sqlx::query(
            "INSERT INTO node (org, tenant_id, kind, name, created_by, updated_by, updated_at) \
             SELECT $1, $2, 'folder', '見えない-' || g, 'carol', 'carol', now() + interval '1 hour' \
             FROM generate_series(1, $3) AS g",
        )
        .bind(ORG)
        .bind(&env.tenant)
        .bind(i32::try_from(SHARED_SCAN_MAX_CHECKS + 300).unwrap())
        .execute(&env.pool)
        .await
        .unwrap();

        let first = env
            .service
            .list_shared_with_me(&env.bob, None, 50, None)
            .await
            .unwrap();
        assert!(first.items.is_empty(), "見えないものばかりのページ");
        assert!(first.next.is_some(), "空でも続きがある");
        let (ids, sizes) = read_all(&env, 50).await;
        assert_eq!(ids, vec![shared], "読み進めると残りが出る");
        assert_eq!(sizes[0], 0);
    })
    .await;
}

/// 上限に届いた経路で候補を引く文（`storage::shared_scan_sql`）は、並びの索引（migration 0140 の
/// `node_shared_scan_idx`）を逆にたどって件数で打ち切る。索引が無いと、引くたびに組織の生きているノードをすべて
/// 読んで並べ直す計画になる（1 回の頼みで最大 5 回）。小さな表でも索引を選べるかだけを見るため、走査と並べ直しを
/// 抑えて計画を立てさせる（索引で並びを満たせないと Sort が残る）。続きの位置の有無の両方を見る。行は作らない。
#[tokio::test]
async fn the_candidate_scan_walks_the_order_index_without_sorting() {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    let explain = format!("EXPLAIN {}", storage::shared_scan_sql());
    for after in [None, Some((chrono::Utc::now(), Uuid::new_v4()))] {
        let mut tx = pool.begin().await.unwrap();
        for set in [
            "SET LOCAL enable_seqscan = off",
            "SET LOCAL enable_sort = off",
        ] {
            sqlx::query(set).execute(&mut *tx).await.unwrap();
        }
        let plan: Vec<String> = sqlx::query_scalar(&explain)
            .bind(ORG)
            .bind("t-plan")
            .bind("alice")
            .bind(after.map(|a| a.0))
            .bind(after.map(|a| a.1))
            .bind(storage::SHARED_SCAN_CHUNK)
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains("node_shared_scan_idx"), "{plan}");
        assert!(!plan.contains("Sort"), "{plan}");
    }
}
