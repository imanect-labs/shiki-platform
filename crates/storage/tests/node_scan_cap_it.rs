//! ドライブのフォルダーの中身の一覧と名前の検索で、候補を 1 件ずつ単発の check で確かめない（BUG-187）の結合テスト
//! （実 Postgres ＋実 OpenFGA）。
//!
//! - 候補は BatchCheck でまとめて確かめ、1 回の頼みで問う数は `NODE_SCAN_MAX_CHECKS` まで。見えない候補ばかりが続くと
//!   空のページに続きの位置を付けて返し、読み進めると残りが出る（黙って欠かない）
//! - 名前の検索は、ListObjects が上限より少なく返したら OpenFGA に問わずに集合の中だけを名前に当てる
//! - ListObjects が上限に届いたら、候補を語で選ばない。問う数は語によらず、見えないノードの名前に語が含まれるかが
//!   問う数に出ない
//! - 上限に届いた名前の検索で候補を引く文は、名前の順の索引をたどって打ち切る
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{CheckTuple, OpenFgaClient, OpenFgaConfig, ReadTupleKey};
use authz::visible::LIST_OBJECTS_MAX_RESULTS;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal, Relation,
    Subject,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::{
    object_store::S3Config, ChildSort, ChildSortKey, ObjectStore, S3ObjectStore, StorageService,
    NODE_SCAN_MAX_CHECKS,
};
use uuid::Uuid;

/// 本物の OpenFGA へ渡しつつ、単発の check と BatchCheck の問いの数を数える。
struct Counting {
    inner: Arc<dyn AuthzClient>,
    checks: AtomicUsize,
    batched: AtomicUsize,
}

impl Counting {
    fn take(&self) -> (usize, usize) {
        (
            self.checks.swap(0, Ordering::SeqCst),
            self.batched.swap(0, Ordering::SeqCst),
        )
    }
}

#[async_trait]
impl AuthzClient for Counting {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        c: Consistency,
    ) -> Result<bool, AuthzError> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        self.inner.check(s, r, o, c).await
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        self.batched.fetch_add(items.len(), Ordering::SeqCst);
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
    counting: Arc<Counting>,
    tenant: String,
    alice: AuthContext,
    bob: AuthContext,
}

const ORG: &str = "acme";
/// 直接入れる「見えない」ノードの名前の頭（名前の順で、bob が作るものより前に並ぶ・片付けもこれで引く）。
const HIDDEN: &str = "aaa-見えない-";

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
            store_name: format!("shiki-storage-node-scan-{}", Uuid::new_v4()),
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
    let counting = Arc::new(Counting {
        inner: Arc::new(client),
        checks: AtomicUsize::new(0),
        batched: AtomicUsize::new(0),
    });
    let authz: Arc<dyn AuthzClient> = counting.clone();
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
    // 2 人とも組織の member（ドライブのトップを一覧でき、フォルダを作れる）。
    for who in [&alice, &bob] {
        authz
            .write_tuple(
                &who.subject(),
                Relation::Member,
                &who.ns().organization(ORG),
            )
            .await
            .unwrap();
    }
    counting.take();
    Some(Env {
        service,
        pool,
        fga,
        counting,
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
    sqlx::query("DELETE FROM node WHERE tenant_id = $1 AND name LIKE $2")
        .bind(&env.tenant)
        .bind(format!("{HIDDEN}%"))
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

/// carol（ほかの人）の非公開のフォルダーと同じ形の、bob に見えないノードを `n` 件、`parent` の直下に入れる
/// （タプルを書かない）。名前は `{HIDDEN}{word}-{番号}`。
async fn hidden(env: &Env, parent: Option<Uuid>, word: &str, n: usize) {
    sqlx::query(
        "INSERT INTO node (org, tenant_id, kind, name, parent_id, created_by, updated_by) \
         SELECT $1, $2, 'folder', $3 || lpad(g::text, 5, '0'), $4, 'carol', 'carol' \
         FROM generate_series(1, $5) AS g",
    )
    .bind(ORG)
    .bind(&env.tenant)
    .bind(format!("{HIDDEN}{word}-"))
    .bind(parent)
    .bind(i32::try_from(n).unwrap())
    .execute(&env.pool)
    .await
    .unwrap();
}

/// bob を viewer にした、DB に無いファイルのタプル（file の ListObjects を上限に届かせる詰め物）。folder の
/// ListObjects は上限の内のまま（bob のフォルダーは必ず返る）なので、問う数は見えないノードの数だけで決まる。
fn file_fillers(env: &Env, n: usize) -> Vec<(String, &'static str, String)> {
    let ns = env.bob.ns();
    (0..n)
        .map(|_| {
            (
                env.bob.subject().to_string(),
                "viewer",
                ns.file(&Uuid::new_v4().to_string()).as_str().to_string(),
            )
        })
        .collect()
}

/// 一覧を続きがなくなるまで読む（ページごとの件数も返す）。`list` は続きの位置から 1 ページを読む。
async fn read_all<F, Fut>(mut list: F) -> (Vec<Uuid>, Vec<usize>)
where
    F: FnMut(Option<String>) -> Fut,
    Fut: std::future::Future<Output = storage::ChildPage>,
{
    let mut ids = Vec::new();
    let mut sizes = Vec::new();
    let mut cursor = None;
    for _ in 0..50 {
        let page = list(cursor.clone()).await;
        sizes.push(page.items.len());
        ids.extend(page.items.iter().map(|n| n.id));
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return (ids, sizes),
        }
    }
    panic!("続きの位置が終わらない");
}

#[tokio::test]
async fn the_top_level_full_of_hidden_folders_stops_at_the_budget_and_the_rest_follows() {
    run(|env| async move {
        // bob に見えないトップのフォルダーを、問う数の上限より多く入れ、bob のフォルダーを名前の順でその後ろに置く。
        hidden(&env, None, "x", NODE_SCAN_MAX_CHECKS + 150).await;
        let mine = env
            .service
            .create_folder(&env.bob, None, "zzz-ボブの資料", None)
            .await
            .unwrap();
        env.counting.take();

        let first = env
            .service
            .list_children(&env.bob, None, ChildSort::default(), None, 50, false, None)
            .await
            .unwrap();
        let (checks, batched) = env.counting.take();
        assert!(first.items.is_empty(), "見えないものばかりのページ");
        assert!(first.next_cursor.is_some(), "空でも続きがある");
        // 単発の check は親（組織の member）の 1 回だけ。候補は BatchCheck で、上限まで。
        assert_eq!(checks, 1, "候補を単発の check で問わない");
        assert_eq!(batched, NODE_SCAN_MAX_CHECKS, "問う数は上限で止まる");
        let raw = first.next_cursor.clone().unwrap();
        assert!(!raw.is_empty());

        let (ids, sizes) = read_all(|c| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .list_children(
                        &env.bob,
                        None,
                        ChildSort::default(),
                        c.as_deref(),
                        50,
                        false,
                        None,
                    )
                    .await
                    .unwrap()
            }
        })
        .await;
        assert_eq!(ids, vec![mine.id], "読み進めると残りが出る（重ならない）");
        assert_eq!(sizes[0], 0);
        // alice には自分のものが無く、トップは空（見えないフォルダーは alice のものでもない）。
        let (ids, _) = read_all(|c| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .list_children(
                        &env.alice,
                        None,
                        ChildSort::default(),
                        c.as_deref(),
                        50,
                        false,
                        None,
                    )
                    .await
                    .unwrap()
            }
        })
        .await;
        assert!(ids.is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_folder_lists_its_children_with_batch_checks() {
    run(|env| async move {
        let top = env
            .service
            .create_folder(&env.bob, None, "zzz-受付", None)
            .await
            .unwrap();
        let mut made = Vec::new();
        for i in 0..7 {
            let f = env
                .service
                .create_folder(&env.bob, Some(top.id), &format!("申請-{i}"), None)
                .await
                .unwrap();
            made.push(f.id);
        }
        env.counting.take();
        let (ids, sizes) = read_all(|c| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .list_children(
                        &env.bob,
                        Some(top.id),
                        ChildSort::default(),
                        c.as_deref(),
                        3,
                        false,
                        None,
                    )
                    .await
                    .unwrap()
            }
        })
        .await;
        assert_eq!(ids, made, "名前の順に漏れなく重ならず");
        assert_eq!(&sizes[..2], &[3, 3]);
        let (checks, _) = env.counting.take();
        // 単発の check は親の閲覧（ページごとに 1 回）だけ。
        assert_eq!(checks, sizes.len(), "子を単発の check で問わない");
        // 降順は逆の順に並ぶ。
        let desc = ChildSort {
            key: ChildSortKey::Name,
            desc: true,
        };
        let page = env
            .service
            .list_children(&env.bob, Some(top.id), desc, None, 10, false, None)
            .await
            .unwrap();
        let got: Vec<Uuid> = page.items.iter().map(|n| n.id).collect();
        let mut rev = made.clone();
        rev.reverse();
        assert_eq!(got, rev);
        // 見えない人（alice）は親を読めない。
        assert!(env
            .service
            .list_children(
                &env.alice,
                Some(top.id),
                ChildSort::default(),
                None,
                10,
                false,
                None
            )
            .await
            .is_err());
    })
    .await;
}

/// ページの後ろに見えるものがあるときだけ続きの位置を返す（ちょうど `limit` 件で尽きたら返さない・BUG-180）。
/// フォルダーの中身と名前の検索（見える集合の経路）の両方。
#[tokio::test]
async fn a_full_page_returns_a_cursor_only_when_more_follow() {
    run(|env| async move {
        let top = env
            .service
            .create_folder(&env.bob, None, "zzz-ちょうど", None)
            .await
            .unwrap();
        for i in 0..3 {
            env.service
                .create_folder(&env.bob, Some(top.id), &format!("控え-{i}"), None)
                .await
                .unwrap();
        }
        let list = |limit| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .list_children(
                        &env.bob,
                        Some(top.id),
                        ChildSort::default(),
                        None,
                        limit,
                        false,
                        None,
                    )
                    .await
                    .unwrap()
            }
        };
        let exact = list(3).await;
        assert_eq!(exact.items.len(), 3);
        assert!(exact.next_cursor.is_none(), "ちょうどで尽きたら続きは無い");
        let short = list(2).await;
        assert_eq!(short.items.len(), 2);
        let rest = env
            .service
            .list_children(
                &env.bob,
                Some(top.id),
                ChildSort::default(),
                short.next_cursor.as_deref(),
                2,
                false,
                None,
            )
            .await
            .unwrap();
        assert_eq!(rest.items.len(), 1, "後ろに 1 件ある");
        assert!(rest.next_cursor.is_none());

        let search = |limit| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .search_nodes_by_name(&env.bob, "控え", ChildSort::default(), None, limit, None)
                    .await
                    .unwrap()
            }
        };
        let exact = search(3).await;
        assert_eq!(exact.items.len(), 3);
        assert!(
            exact.next_cursor.is_none(),
            "検索もちょうどで尽きたら続きは無い"
        );
        let short = search(2).await;
        assert_eq!(short.items.len(), 2);
        assert!(short.next_cursor.is_some());
    })
    .await;
}

#[tokio::test]
async fn name_search_within_the_visible_set_asks_nothing() {
    run(|env| async move {
        let mine = env
            .service
            .create_folder(&env.bob, None, "zzz-出張報告", None)
            .await
            .unwrap();
        hidden(&env, None, "出張報告", 30).await;
        env.counting.take();
        let page = env
            .service
            .search_nodes_by_name(&env.bob, "出張 報告", ChildSort::default(), None, 50, None)
            .await
            .unwrap();
        let (checks, batched) = env.counting.take();
        assert_eq!(
            page.items.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![mine.id]
        );
        assert!(page.next_cursor.is_none());
        assert_eq!(
            (checks, batched),
            (1, 0),
            "組織の member の確認だけで、候補を問わない"
        );
    })
    .await;
}

#[tokio::test]
async fn name_search_beyond_the_list_objects_cap_asks_the_same_whatever_the_word() {
    run(|env| async move {
        env.fga
            .write(&file_fillers(&env, LIST_OBJECTS_MAX_RESULTS + 50))
            .await;
        // 見えないノードの半分だけが「極秘」を名前に含む。bob のものは名前の順でその後ろ。
        hidden(&env, None, "極秘", 120).await;
        hidden(&env, None, "雑記", 120).await;
        let mine = env
            .service
            .create_folder(&env.bob, None, "zzz-極秘ではないメモ", None)
            .await
            .unwrap();
        env.counting.take();

        let mut asked = Vec::new();
        for word in ["極秘", "雑記", "どこにも無い語"] {
            let (ids, _) = read_all(|c| {
                let env = Arc::clone(&env);
                async move {
                    env.service
                        .search_nodes_by_name(
                            &env.bob,
                            word,
                            ChildSort::default(),
                            c.as_deref(),
                            50,
                            None,
                        )
                        .await
                        .unwrap()
                }
            })
            .await;
            asked.push(env.counting.take());
            match word {
                "極秘" => assert_eq!(ids, vec![mine.id], "見えるものだけが当たる"),
                _ => assert!(ids.is_empty(), "{word}"),
            }
        }
        // 問う数は、見えないノードの名前に語が含まれるかによらない（候補を語で選ばない）。
        assert!(asked[0].1 > 0, "上限に届いた経路で問う");
        assert_eq!(asked[0], asked[1], "{asked:?}");
        assert_eq!(asked[1], asked[2], "{asked:?}");
    })
    .await;
}

#[tokio::test]
async fn name_search_full_of_hidden_hits_stops_at_the_budget_and_the_rest_follows() {
    run(|env| async move {
        env.fga
            .write(&file_fillers(&env, LIST_OBJECTS_MAX_RESULTS + 10))
            .await;
        hidden(&env, None, "見積", NODE_SCAN_MAX_CHECKS + 100).await;
        let mine = env
            .service
            .create_folder(&env.bob, None, "zzz-見積の控え", None)
            .await
            .unwrap();
        env.counting.take();
        let first = env
            .service
            .search_nodes_by_name(&env.bob, "見積", ChildSort::default(), None, 50, None)
            .await
            .unwrap();
        let (_, batched) = env.counting.take();
        assert!(first.items.is_empty());
        assert!(first.next_cursor.is_some(), "空でも続きがある");
        assert!(batched <= NODE_SCAN_MAX_CHECKS, "{batched}");
        let (ids, _) = read_all(|c| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .search_nodes_by_name(
                        &env.bob,
                        "見積",
                        ChildSort::default(),
                        c.as_deref(),
                        50,
                        None,
                    )
                    .await
                    .unwrap()
            }
        })
        .await;
        assert_eq!(ids, vec![mine.id]);
        let got: HashSet<Uuid> = ids.into_iter().collect();
        assert_eq!(got.len(), 1);
    })
    .await;
}

/// アップロードの前の同じ名前の確かめ（名前の完全一致の引き当て・BUG-199）は、名前の検索の先頭のページが空でも、
/// 同じフォルダーの同じ名前を見つける。OpenFGA に問うのは、親の確認と当たったノードの確かめの 1 回ずつ。見えない
/// 同じ名前・ほかのフォルダーの同じ名前・大文字と小文字の違う名前は返さない。
#[tokio::test]
async fn exact_name_lookup_finds_the_sibling_a_search_page_misses() {
    run(|env| async move {
        env.fga
            .write(&file_fillers(&env, LIST_OBJECTS_MAX_RESULTS + 10))
            .await;
        hidden(&env, None, "報告書", NODE_SCAN_MAX_CHECKS + 100).await;
        let name = "zzz-報告書.pdf";
        let mine = env
            .service
            .create_folder(&env.bob, None, name, None)
            .await
            .unwrap();
        // 画面がこれまで使っていた確かめ（名前の検索の先頭のページ）は、0 件で続きの位置だけを返す。
        let first = env
            .service
            .search_nodes_by_name(&env.bob, name, ChildSort::default(), None, 100, None)
            .await
            .unwrap();
        assert!(first.items.is_empty(), "先頭のページには出ない");
        assert!(first.next_cursor.is_some());
        env.counting.take();

        let find = |who: AuthContext, parent: Option<Uuid>, name: &'static str| {
            let env = Arc::clone(&env);
            async move {
                env.service
                    .find_child_by_name(&who, parent, name, None)
                    .await
            }
        };
        let found = find(env.bob.clone(), None, name).await.unwrap();
        assert_eq!(found.map(|n| n.id), Some(mine.id), "同じ名前を見つける");
        assert_eq!(
            env.counting.take(),
            (2, 0),
            "親（組織の member）と当たったノードの 1 回ずつ"
        );
        assert!(find(env.bob.clone(), None, "ZZZ-報告書.PDF")
            .await
            .unwrap()
            .is_none());
        assert!(find(env.bob.clone(), None, "どこにも無い名前")
            .await
            .unwrap()
            .is_none());
        assert_eq!(env.counting.take(), (2, 0), "当たらなければ親の確認だけ");
        // 見えない同じ名前（ほかの人の非公開のもの）は返さない。
        assert!(find(env.bob.clone(), None, "aaa-見えない-報告書-00001")
            .await
            .unwrap()
            .is_none());
        // フォルダーの中: ほかのフォルダーの同じ名前とは分ける。親を読めない人には存在を明かさない。
        let top = env
            .service
            .create_folder(&env.bob, None, "zzz-受付", None)
            .await
            .unwrap();
        assert!(find(env.bob.clone(), Some(top.id), name)
            .await
            .unwrap()
            .is_none());
        let inner = env
            .service
            .create_folder(&env.bob, Some(top.id), name, None)
            .await
            .unwrap();
        assert_eq!(
            find(env.bob.clone(), Some(top.id), name)
                .await
                .unwrap()
                .map(|n| n.id),
            Some(inner.id)
        );
        assert!(matches!(
            find(env.alice.clone(), Some(top.id), name).await,
            Err(storage::StorageError::NotFound)
        ));
    })
    .await;
}

/// 上限に届いた名前の検索で候補を引く文（`storage::named_scan_sql`）は、名前の順の索引（migration 0139 の
/// `node_name_scan_idx`・降順は `node_name_scan_desc_idx`）をたどって件数で打ち切る。並びはフォルダー優先 → 名前の
/// 自然順 → id（#1040）。索引が無いと、引くたびに組織の生きているノードをすべて読んで並べ直す計画になる。小さな表でも
/// 索引を選べるかだけを見るため、走査と並べ直しを抑えて計画を立てさせる（索引で並びを満たせないと Sort が残る）。
/// 昇順・降順と、続きの位置の有無と、種別の絞り込みの有無を見る。行は作らない。
#[tokio::test]
async fn the_named_candidate_scan_walks_the_name_index_without_sorting() {
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
    for (desc, index) in [
        (false, "node_name_scan_idx"),
        (true, "node_name_scan_desc_idx"),
    ] {
        let sort = ChildSort {
            key: ChildSortKey::Name,
            desc,
        };
        let explain = format!("EXPLAIN {}", storage::named_scan_sql(sort));
        // 続きの位置は（並びの値・id・種別・名前）。名前の順では並びの値も名前。
        for after in [None, Some(("m".to_string(), Uuid::new_v4(), "file"))] {
            for kind in [None, Some("file")] {
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
                    .bind(vec!["%a%".to_string()])
                    .bind(after.as_ref().map(|a| a.0.clone()))
                    .bind(after.as_ref().map(|a| a.1))
                    .bind(200_i64)
                    .bind(kind)
                    .bind(after.as_ref().map(|a| a.2))
                    .bind(after.as_ref().map(|a| a.0.clone()))
                    .fetch_all(&mut *tx)
                    .await
                    .unwrap();
                tx.rollback().await.unwrap();
                let plan = plan.join("\n");
                assert!(plan.contains(&format!(" {index} ")), "{plan}");
                assert!(!plan.contains("Sort"), "{plan}");
            }
        }
    }
}
