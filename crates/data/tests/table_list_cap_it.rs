//! テーブルの一覧と名前の検索を ListObjects の上限で切らない（BUG-149）の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! - 見られるテーブルが ListObjects の上限（1,000 件）を超えても、新しいテーブルが一覧と検索に出る
//!   （上限を超えるタプルは OpenFGA へ直接書く。DB に無いテーブルのタプル＝消したテーブルの残りと同じ形）
//! - 見えないテーブルは出ない。続きの位置で読み進めると、見えるテーブルを重ならず漏れなく新しい順に一巡する
//! - 追加だけの共有も、上限を超えた人の一覧に印つきで出る
//! - 見えないテーブルばかりが続くと、OpenFGA に問う数の上限で止まって続きの位置を返す（位置は暗号化され、見えない
//!   テーブルの id を含まない）。読み進めると残りが出る。問う数は名前の検索の語によらない（見えないテーブルの名前を
//!   応答の時間から推し量らせない）。ページを持たない一覧は、切れたら続きがあると言う
//!
//! 各テストは自分のテナント（`t-<uuid>`）と OpenFGA のストアに閉じ、最後に書いたタプルとテナントを撤去する。

#![allow(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, ObjectType, Principal, Relation};
use base64::Engine as _;
use data::{
    DataStore, FieldDef, FieldType, NewDataTable, RefResolver, TableListQuery, TableSchema,
    TABLE_SCAN_MAX_CHECKS,
};
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

/// 上限を超えるタプルを直接書く・消すための OpenFGA の口（Write API は 1 回 100 件まで）。
struct Fga {
    http: reqwest::Client,
    base_url: String,
    store_id: String,
    model_id: String,
}

impl Fga {
    async fn write(&self, key: &str, tuples: &[(String, &str, String)]) {
        for chunk in tuples.chunks(100) {
            let keys: Vec<serde_json::Value> = chunk
                .iter()
                .map(|(u, r, o)| serde_json::json!({ "user": u, "relation": r, "object": o }))
                .collect();
            let resp = self
                .http
                .post(format!("{}/stores/{}/write", self.base_url, self.store_id))
                .json(&serde_json::json!({
                    key: { "tuple_keys": keys },
                    "authorization_model_id": self.model_id,
                }))
                .send()
                .await
                .expect("OpenFGA write");
            assert!(
                resp.status().is_success(),
                "{key}: {}",
                resp.text().await.unwrap_or_default()
            );
        }
    }

    /// テストのストアを消す（タプルは先に消してある）。
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
    store: DataStore,
    authz: Arc<dyn AuthzClient>,
    fga: Fga,
    pool: PgPool,
}

/// テストの本体を走らせ、落ちても後片付け（直接書いたタプル・写した行・テナント・ストア）をしてから結果を返す。
async fn run<F, Fut>(
    env: Env,
    tenant: String,
    written: Vec<(String, &'static str, String)>,
    body: F,
) where
    F: FnOnce(Arc<Env>, String) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let env = Arc::new(env);
    env.fga.write("writes", &written).await;
    let outcome = tokio::spawn(body(Arc::clone(&env), tenant.clone())).await;
    env.fga.write("deletes", &written).await;
    // 写した行はタプルを持たないので、テナントの撤去（テーブルごとに OpenFGA を読む）の前に消す。
    sqlx::query("DELETE FROM data_table WHERE tenant_id = $1 AND name LIKE '見えない-%'")
        .bind(&tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    env.store.purge_tenant(&tenant).await.unwrap();
    env.fga.delete_store().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
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
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-data-table-list-cap-{}", Uuid::new_v4()),
        },
        &model,
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
    Some(Env {
        store: DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs)),
        authz,
        fga,
        pool,
    })
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

fn schema() -> TableSchema {
    TableSchema {
        fields: vec![FieldDef::new("name", FieldType::Text)],
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

async fn table(env: &Env, owner: &AuthContext, name: &str) -> Uuid {
    env.store
        .create_table(
            owner,
            NewDataTable {
                name: name.into(),
                schema: schema(),
            },
            None,
        )
        .await
        .expect("create table")
        .id
}

async fn grant(env: &Env, who: &AuthContext, relation: Relation, table: Uuid) {
    env.authz
        .write_tuple(
            &who.subject(),
            relation,
            &who.ns().data_table(&table.to_string()),
        )
        .await
        .expect("grant");
}

/// DB に無いテーブルへの `relation` のタプルを `n` 件（消したテーブルの残りと同じ形）。id は小さい順に並ぶ
/// ようにする（OpenFGA が先に読む側を、DB に無いテーブルで埋める）。
fn leftovers(
    who: &AuthContext,
    relation: &'static str,
    n: usize,
) -> Vec<(String, &'static str, String)> {
    (0..n)
        .map(|i| {
            let id = format!("00000000-0000-4000-8000-{i:012}");
            (
                who.subject().as_str().to_string(),
                relation,
                who.ns().data_table(&id).as_str().to_string(),
            )
        })
        .collect()
}

/// 上限を超える件数（ListObjects がどの 1,000 件を返しても、上限で切られる）。
const OVER_CAP: usize = authz::visible::LIST_OBJECTS_MAX_RESULTS + 50;

fn page(q: Option<&str>, cursor: Option<String>, limit: i64) -> TableListQuery {
    TableListQuery {
        q: q.map(str::to_string),
        cursor,
        limit,
    }
}

fn names(items: &[data::DataTable]) -> Vec<String> {
    items.iter().map(|t| t.name.clone()).collect()
}

/// 続きの位置で最後まで読む（読んだテーブルの id を順に・頼んだ回数）。
async fn read_all(env: &Env, who: &AuthContext, q: Option<&str>, limit: i64) -> (Vec<Uuid>, usize) {
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let p = env
            .store
            .list_tables_page(who, &page(q, cursor.clone(), limit))
            .await
            .unwrap();
        seen.extend(p.items.iter().map(|t| t.id));
        pages += 1;
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => return (seen, pages),
        }
        assert!(pages < 50, "続きが終わらない");
    }
}

/// 見られるテーブルが上限を超えても、新しいテーブルが一覧と検索に出る。見えないテーブルは出ない。
#[tokio::test]
async fn new_tables_show_up_past_the_list_objects_cap() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    // bob と carol は、消したテーブルの残りのタプルで ListObjects の上限を超えている。
    let mut written = leftovers(&ctx(&tenant, "bob"), "viewer", OVER_CAP);
    written.extend(leftovers(&ctx(&tenant, "carol"), "viewer", OVER_CAP));
    written.extend(leftovers(&ctx(&tenant, "carol"), "submitter", OVER_CAP));
    run(env, tenant, written, past_the_cap).await;
}

async fn past_the_cap(env: Arc<Env>, tenant: String) {
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let carol = ctx(&tenant, "carol");
    let cut = env
        .authz
        .list_objects(&bob.subject(), Relation::Viewer, ObjectType::DataTable)
        .await
        .unwrap();
    assert!(
        authz::visible::list_may_be_cut(cut.len()),
        "前提: bob の ListObjects が上限に届いている（{} 件）",
        cut.len()
    );

    let mut shown = Vec::new();
    for name in ["備品", "経費-新しい-A", "経費-新しい-B", "経費-新しい-C"] {
        let id = table(&env, &alice, name).await;
        grant(&env, &bob, Relation::Viewer, id).await;
        shown.push(id);
    }
    let hidden = table(&env, &alice, "経費-秘密").await;
    let add_only = table(&env, &alice, "経費-申請").await;
    grant(&env, &carol, Relation::Submitter, add_only).await;

    // 一覧の先頭のページに、作ったばかりのテーブルが新しい順に出る。
    let first = env
        .store
        .list_tables_page(&bob, &page(None, None, 50))
        .await
        .unwrap();
    let mut want = shown.clone();
    want.reverse();
    assert_eq!(
        first.items.iter().map(|t| t.id).collect::<Vec<_>>(),
        want,
        "{:?}",
        names(&first.items)
    );
    assert!(first.next_cursor.is_none());
    // 名前の検索も（見えない「経費-秘密」「経費-申請」には当たらない）。
    let hit = env
        .store
        .list_tables_page(&bob, &page(Some("経費"), None, 50))
        .await
        .unwrap();
    assert_eq!(
        names(&hit.items),
        vec!["経費-新しい-C", "経費-新しい-B", "経費-新しい-A"]
    );
    // ページの外の一覧（チャット・ワークフロー・SQL の名前の候補）も同じ。読み切ったので続きは無い。
    let listed = env.store.list_tables_listing(&bob, 200).await.unwrap();
    assert_eq!(listed.items.iter().map(|t| t.id).collect::<Vec<_>>(), want);
    assert!(listed
        .items
        .iter()
        .all(|t| t.id != hidden && t.id != add_only));
    assert!(!listed.more);
    // 上限の件数で切れたら続きがあると言う。
    let short = env.store.list_tables_listing(&bob, 2).await.unwrap();
    assert_eq!(short.items.len(), 2);
    assert!(short.more);

    // アプリ所有の一覧（app-gateway）も、上限を超えた人に所有テーブルを返す。
    let app = Uuid::new_v4();
    sqlx::query("UPDATE data_table SET app_id = $3 WHERE tenant_id = $1 AND id = ANY($2)")
        .bind(&tenant)
        .bind(vec![shown[1], hidden])
        .bind(app)
        .execute(&env.pool)
        .await
        .unwrap();
    let owned = env.store.list_app_tables(&bob, app, 200).await.unwrap();
    assert_eq!(names(&owned.items), vec!["経費-新しい-A"]);
    assert!(!owned.more);

    // 2 件ずつ読み進めても、重ならず漏れなく新しい順に一巡する（続きの位置は暗号化されている）。
    let (seen, pages) = read_all(&env, &bob, None, 2).await;
    assert_eq!(seen, want);
    assert_eq!(pages, 2);
    let (seen, _) = read_all(&env, &bob, Some("経費"), 2).await;
    assert_eq!(seen, want[..3].to_vec());

    // 追加だけの共有も、上限を超えた人の画面の一覧と検索に印つきで出る（閲覧の一覧には出ない）。
    let p = env
        .store
        .list_tables_for_viewer_page(&carol, &page(Some("経費"), None, 50))
        .await
        .unwrap();
    assert_eq!(names(&p.items), vec!["経費-申請"]);
    assert!(p.items[0].add_only);
    assert!(env.store.list_tables(&carol, 50).await.unwrap().is_empty());
}

/// 見えないテーブルばかりが続くと、OpenFGA に問う数の上限で止まって続きの位置を返す。位置は見えないテーブルの
/// id を含まず、読み進めると残りの見えるテーブルが出る。
#[tokio::test]
async fn scan_budget_stops_with_an_opaque_cursor_and_reading_on_finds_the_rest() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let written = leftovers(&ctx(&tenant, "bob"), "viewer", OVER_CAP);
    run(env, tenant, written, scan_budget).await;
}

async fn scan_budget(env: Arc<Env>, tenant: String) {
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let older = table(&env, &alice, "見える-古い").await;
    let newer = table(&env, &alice, "見える-新しい").await;
    for id in [older, newer] {
        grant(&env, &bob, Relation::Viewer, id).await;
    }
    // bob に見えないテーブルを、OpenFGA に問う数の上限より 50 件多く、見えるテーブルより新しく置く
    // （作成の入口を通すと遅いので、作ったテーブルの行を写す。内部の連番は採番と重ならない負の値）。
    let extra = i64::try_from(TABLE_SCAN_MAX_CHECKS).unwrap() + 50;
    let base: i64 = -1_000_000_000 - i64::from(rand_u16()) * 100_000;
    let inserted = sqlx::query(
        "INSERT INTO data_table (tenant_id, id, org, name, schema, schema_version, created_by, \
             created_at, updated_at, tenant_int, table_int, key_format, next_field_id) \
         SELECT tenant_id, gen_random_uuid(), org, '見えない-' || gs, schema, schema_version, created_by, \
             created_at, updated_at + interval '1 hour' + gs * interval '1 millisecond', tenant_int, $3 - gs, \
             key_format, next_field_id \
         FROM data_table, generate_series(1, $2::bigint) gs \
         WHERE tenant_id = $1 AND id = $4",
    )
    .bind(&tenant)
    .bind(extra)
    .bind(base)
    .bind(newer)
    .execute(&env.pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(inserted, u64::try_from(extra).unwrap());
    let hidden: HashSet<Uuid> = sqlx::query_scalar(
        "SELECT id FROM data_table WHERE tenant_id = $1 AND name LIKE '見えない-%'",
    )
    .bind(&tenant)
    .fetch_all(&env.pool)
    .await
    .unwrap()
    .into_iter()
    .collect();

    // 1 ページ目: 見えない候補を問う数の上限まで確かめて、何も返さずに続きの位置を返す。
    let first = env
        .store
        .list_tables_page(&bob, &page(None, None, 50))
        .await
        .unwrap();
    assert!(first.items.is_empty(), "{:?}", names(&first.items));
    let cursor = first.next_cursor.clone().expect("続きの位置");
    // 位置は不透明（見えないテーブルの id も、その平文の base64 も含まない）。
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&cursor)
        .unwrap();
    let raw = String::from_utf8_lossy(&raw);
    for id in &hidden {
        assert!(!cursor.contains(&id.to_string()) && !raw.contains(&id.to_string()));
    }
    // 読み進めると、残りの見えない候補の先に、見えるテーブルが新しい順に出る。
    let second = env
        .store
        .list_tables_page(&bob, &page(None, Some(cursor), 50))
        .await
        .unwrap();
    assert_eq!(names(&second.items), vec!["見える-新しい", "見える-古い"]);
    assert!(second.next_cursor.is_none());
    // 見えないテーブルの名前で探しても、どのページにも出ない（頼む回数は問う数の上限で決まる）。
    let (seen, pages) = read_all(&env, &bob, Some("見えない"), 50).await;
    assert!(seen.is_empty());
    assert_eq!(pages, 2);
    // 頼む回数（と OpenFGA に問う数）は、見えないテーブルの名前に当たる語でも当たらない語でも同じ（語で候補を
    // 選ばない）。以前は、当たらない語なら OpenFGA を呼ばずに 1 回で終わり、名前があるかが分かった。
    let first = env
        .store
        .list_tables_page(&bob, &page(Some("どこにもない名前"), None, 50))
        .await
        .unwrap();
    assert!(first.items.is_empty());
    assert!(
        first.next_cursor.is_some(),
        "当たらない語でも、問う数の上限で止まる"
    );
    let (seen, nowhere_pages) = read_all(&env, &bob, Some("どこにもない名前"), 50).await;
    assert!(seen.is_empty());
    assert_eq!(nowhere_pages, pages);
    // 見えるテーブルの名前で探すと、見えない候補の先で見つかる。
    let (seen, _) = read_all(&env, &bob, Some("見える"), 50).await;
    assert_eq!(seen, vec![newer, older]);

    // ページを持たない一覧（チャットの AI・ワークフロー）は、問う数の上限で 0 件に切れても続きがあると言う
    // （チャットはデータのツールを出し、「他にもテーブルがあります」と添える）。
    let listing = env.store.list_tables_listing(&bob, 31).await.unwrap();
    assert!(listing.items.is_empty(), "{:?}", names(&listing.items));
    assert!(listing.more);
}

fn rand_u16() -> u16 {
    let b = Uuid::new_v4().into_bytes();
    u16::from_be_bytes([b[0], b[1]])
}
