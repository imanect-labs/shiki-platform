//! 認可材料のリクエストスコープ memo ＋ BatchCheck ＋ スキーマキャッシュ（Task 15.9）の結合テスト。
//!
//! - グリッド 1 画面（ページ＋件数＋lookup / rollup の解決）の OpenFGA 往復が 2 回以下（計測）
//! - リクエストを跨いだ材料の再利用が無い（権限剥奪が次のリクエストで即反映・PIT-18）
//! - 1 リクエスト内で別テーブル（別の式集合）の材料が混ざらない
//! - スキーマ改訂後の最初のクエリが新スキーマで動き、削除・テナント purge の後に古い世代が読まれない
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で実行する。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, CheckTuple, Consistency, FgaObject, ObjectType,
    Principal, ReadTupleKey, Relation, Subject,
};
use data::{
    CmpOp, CountResult, DataError, DataQuery, DataStore, FieldDef, FieldPolicy, FieldType,
    LinkCardinality, LinkDef, ListRecordsOptions, LookupDef, NewDataTable, PolicyExpr,
    PolicyOperand, RefResolver, RollupDef, RollupOp, RowPolicy, SortKey, TableSchema,
};
use serde_json::json;
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

/// 実 OpenFGA へ委譲しつつ、読取の往復（HTTP リクエスト）を数える。
#[derive(Default)]
struct Counts {
    check: AtomicUsize,
    batch: AtomicUsize,
    list_objects: AtomicUsize,
    read_subject: AtomicUsize,
    read_tuples: AtomicUsize,
}

impl Counts {
    fn total(&self) -> usize {
        self.check.load(Ordering::SeqCst)
            + self.batch.load(Ordering::SeqCst)
            + self.list_objects.load(Ordering::SeqCst)
            + self.read_subject.load(Ordering::SeqCst)
            + self.read_tuples.load(Ordering::SeqCst)
    }
    fn reset(&self) {
        for c in [
            &self.check,
            &self.batch,
            &self.list_objects,
            &self.read_subject,
            &self.read_tuples,
        ] {
            c.store(0, Ordering::SeqCst);
        }
    }
    fn describe(&self) -> String {
        format!(
            "check={} batch_check={} list_objects={} read_subject_objects={} read_tuples={}",
            self.check.load(Ordering::SeqCst),
            self.batch.load(Ordering::SeqCst),
            self.list_objects.load(Ordering::SeqCst),
            self.read_subject.load(Ordering::SeqCst),
            self.read_tuples.load(Ordering::SeqCst),
        )
    }
}

struct Counting {
    inner: OpenFgaClient,
    counts: Arc<Counts>,
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
        self.counts.check.fetch_add(1, Ordering::SeqCst);
        self.inner.check(s, r, o, c).await
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        // 50 問ごとに 1 往復（OpenFGA の BatchCheck の上限）。
        self.counts
            .batch
            .fetch_add(items.len().div_ceil(50), Ordering::SeqCst);
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
        self.counts.read_tuples.fetch_add(1, Ordering::SeqCst);
        self.inner.read_tuples(o, r).await
    }
    async fn list_objects(
        &self,
        s: &Subject,
        r: Relation,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.counts.list_objects.fetch_add(1, Ordering::SeqCst);
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
        self.counts.read_subject.fetch_add(1, Ordering::SeqCst);
        self.inner.read_subject_objects(s, t).await
    }
}

async fn setup() -> Option<(PgPool, Arc<dyn AuthzClient>, Arc<Counts>)> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
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
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-memo-test-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
    let counts = Arc::new(Counts::default());
    let authz: Arc<dyn AuthzClient> = Arc::new(Counting {
        inner: fga,
        counts: Arc::clone(&counts),
    });
    Some((pool, authz, counts))
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

fn field(name: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(name, ty)
}

fn has_role(role: &str) -> PolicyExpr {
    PolicyExpr::HasRole {
        role: role.into(),
        subtree: true,
    }
}

fn plain(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: Some(1),
        fsm_ref: None,
        rag: None,
    }
}

async fn grant(authz: &Arc<dyn AuthzClient>, who: &AuthContext, rel: Relation, table: Uuid) {
    authz
        .write_tuple(
            &who.subject(),
            rel,
            &who.ns().data_table(&table.to_string()),
        )
        .await
        .expect("grant");
}

async fn join_role(authz: &Arc<dyn AuthzClient>, who: &AuthContext, role: &str) {
    authz
        .write_tuple(&who.subject(), Relation::Member, &who.ns().role(role))
        .await
        .expect("role");
}

async fn leave_role(authz: &Arc<dyn AuthzClient>, who: &AuthContext, role: &str) {
    authz
        .delete_tuple(&who.subject(), Relation::Member, &who.ns().role(role))
        .await
        .expect("role");
}

/// グリッドの題材: 明細（lines）と、明細へのリンク・lookup・rollup を持つ注文（orders）。
///
/// - lines の行述語: 本人の行 or `hr` ロール（配下込み）
/// - orders の行述語: 作成者本人 or `mgr` ロール（配下込み）・`salary` は `hr` だけが読める
///
/// 2 テーブルの式集合が違うので、1 リクエストの中で材料が混ざれば lookup / rollup の値が変わる。
struct Grid {
    store: DataStore,
    authz: Arc<dyn AuthzClient>,
    counts: Arc<Counts>,
    alice: AuthContext,
    bob: AuthContext,
    lines: Uuid,
    orders: Uuid,
}

async fn grid() -> Option<Grid> {
    let (pool, authz, counts) = setup().await?;
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let store = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));

    let mut person = field("person", FieldType::UserRef);
    person.indexed = true;
    let mut lines_schema = plain(vec![
        person,
        field("name", FieldType::Text),
        field("amount", FieldType::Number),
    ]);
    lines_schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::FieldCmp {
                field: "person".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
            has_role("hr"),
        ]),
        write: None,
    });
    let lines = store
        .create_table(
            &alice,
            NewDataTable {
                name: "lines".into(),
                schema: lines_schema,
            },
            None,
        )
        .await
        .unwrap()
        .id;

    let mut items = field("items", FieldType::RecordRef);
    items.ref_table = Some(lines);
    items.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToMany,
        symmetric_field: None,
        symmetric_key: None,
        reverse: false,
    });
    let mut names = field("names", FieldType::Lookup);
    names.lookup = Some(LookupDef {
        via_field: "items".into(),
        target_field: "name".into(),
    });
    let mut total = field("total", FieldType::Rollup);
    total.rollup = Some(RollupDef {
        link: "items".into(),
        target: Some("amount".into()),
        op: RollupOp::Sum,
    });
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let mut orders_schema = plain(vec![
        title,
        field("salary", FieldType::Number),
        items,
        names,
        total,
    ]);
    orders_schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![PolicyExpr::IsOwner, has_role("mgr")]),
        write: None,
    });
    orders_schema.field_policy = vec![FieldPolicy {
        field: "salary".into(),
        readable_by: has_role("hr"),
    }];
    let orders = store
        .create_table(
            &alice,
            NewDataTable {
                name: "orders".into(),
                schema: orders_schema,
            },
            None,
        )
        .await
        .unwrap()
        .id;

    // 明細は alice の行が 2 件（bob の分は無い）。注文は 3 件、それぞれ 2 明細を指す。
    let mut line_ids = Vec::new();
    for (n, a) in [("l1", 100), ("l2", 250)] {
        line_ids.push(
            store
                .create_record(
                    &alice,
                    lines,
                    json!({"person": "alice", "name": n, "amount": a}),
                    None,
                )
                .await
                .unwrap()
                .id,
        );
    }
    let link: Vec<String> = line_ids.iter().map(ToString::to_string).collect();
    // salary は hr だけが読める（＝書ける）列なので、値を入れる alice は hr（読めない列への書込は 403）。
    join_role(&authz, &alice, "hr").await;
    for t in ["o1", "o2", "o3"] {
        store
            .create_record(
                &alice,
                orders,
                json!({"title": t, "salary": 500, "items": link}),
                None,
            )
            .await
            .unwrap();
    }
    grant(&authz, &bob, Relation::Viewer, orders).await;
    grant(&authz, &bob, Relation::Viewer, lines).await;
    join_role(&authz, &bob, "mgr").await;
    join_role(&authz, &bob, "hr").await;
    Some(Grid {
        store,
        authz,
        counts,
        alice,
        bob,
        lines,
        orders,
    })
}

fn screen() -> DataQuery {
    DataQuery {
        sort: vec![SortKey {
            field: "title".into(),
            descending: false,
        }],
        limit: Some(50),
        with_count: true,
        ..DataQuery::default()
    }
}

/// グリッド 1 画面（ページ＋件数＋lookup / rollup の解決＋マスク）の OpenFGA 往復が 2 回以下。
///
/// 往復の内訳: テーブル viewer・参照先テーブル viewer・名指しロール（mgr / hr）の check を束ねた
/// BatchCheck 1 回 ＋ 個別共有集合の list_objects 1 回。
#[tokio::test]
async fn grid_screen_reads_with_at_most_two_fga_round_trips() {
    let Some(g) = grid().await else {
        return;
    };
    g.counts.reset();
    let r = g
        .store
        .run_query(&g.bob, g.orders, &screen(), None)
        .await
        .unwrap();
    let trips = g.counts.total();
    eprintln!(
        "グリッド 1 画面（run_query）: OpenFGA 往復 {trips}（{}）",
        g.counts.describe()
    );
    assert!(
        trips <= 2,
        "OpenFGA 往復が 2 回以下: {}",
        g.counts.describe()
    );
    assert_eq!(
        g.counts.batch.load(Ordering::SeqCst),
        1,
        "check は 1 回の BatchCheck"
    );
    // 値が正しい（mgr で全注文・hr で全明細と salary が見える）。
    assert_eq!(r.count, Some(CountResult::Exact(3)));
    let items = r.items.unwrap();
    assert_eq!(items.len(), 3);
    for it in &items {
        assert_eq!(it.data["total"], json!(350), "{}", it.data);
        assert_eq!(it.data["names"], json!(["l1", "l2"]));
        assert_eq!(it.data["salary"], json!(500));
    }

    // 旧 API のグリッド（一覧＋件数）も同じ予算に収まる。
    g.counts.reset();
    let page = g
        .store
        .list_records(&g.bob, g.orders, &ListRecordsOptions::default(), None)
        .await
        .unwrap();
    let trips = g.counts.total();
    eprintln!(
        "グリッド 1 画面（list_records）: OpenFGA 往復 {trips}（{}）",
        g.counts.describe()
    );
    assert!(trips <= 2, "{}", g.counts.describe());
    assert_eq!(page.items.len(), 3);

    // 1 件の取得（詳細画面）も同じ。
    g.counts.reset();
    let one = g
        .store
        .get_record(&g.bob, g.orders, items[0].id, None)
        .await
        .unwrap();
    assert!(g.counts.total() <= 2, "{}", g.counts.describe());
    assert_eq!(one.data["total"], json!(350));
}

/// 1 リクエストの中で、参照先テーブル（別の式集合）の材料が参照元の材料と混ざらない。
///
/// bob は mgr（注文が全部見える）だが hr ではない: 明細は本人の行だけ（0 件）で、salary も見えない。
/// 注文側の材料（mgr=真）が明細側へ流用されれば明細が見えてしまう。
#[tokio::test]
async fn materials_of_different_tables_do_not_mix_within_a_request() {
    let Some(g) = grid().await else {
        return;
    };
    leave_role(&g.authz, &g.bob, "hr").await;
    let r = g
        .store
        .run_query(&g.bob, g.orders, &screen(), None)
        .await
        .unwrap();
    let items = r.items.unwrap();
    assert_eq!(items.len(), 3, "mgr で注文は全部見える");
    for it in &items {
        assert_eq!(it.data["names"], json!([]), "明細は hr でないので見えない");
        assert!(it.data.get("salary").is_none(), "salary は hr だけ");
    }
    // 逆向き: hr だけ（mgr でない）なら注文は 1 件も見えない（作成者でもない）。
    join_role(&g.authz, &g.bob, "hr").await;
    leave_role(&g.authz, &g.bob, "mgr").await;
    let r = g
        .store
        .run_query(&g.bob, g.orders, &screen(), None)
        .await
        .unwrap();
    assert_eq!(r.count, Some(CountResult::Exact(0)));
    // 明細テーブルを直接読めば hr で全部見える。
    let r = g
        .store
        .run_query(&g.bob, g.lines, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(r.items.unwrap().len(), 2);
}

/// 権限の剥奪が**次のリクエストで即座に**反映される（リクエストを跨いだ材料の再利用が無い・PIT-18）。
#[tokio::test]
async fn revocation_is_visible_on_the_very_next_request() {
    let Some(g) = grid().await else {
        return;
    };
    let q = screen();
    let before = g.store.run_query(&g.bob, g.orders, &q, None).await.unwrap();
    assert_eq!(before.count, Some(CountResult::Exact(3)));

    // ① ロール（行述語の材料）を剥奪 → 次のリクエストで注文が見えない。
    leave_role(&g.authz, &g.bob, "mgr").await;
    let after = g.store.run_query(&g.bob, g.orders, &q, None).await.unwrap();
    assert_eq!(
        after.count,
        Some(CountResult::Exact(0)),
        "ロール剥奪が即反映"
    );
    join_role(&g.authz, &g.bob, "mgr").await;

    // ② マスクの材料（hr）を剥奪 → 次のリクエストで salary も明細も消える。
    leave_role(&g.authz, &g.bob, "hr").await;
    let after = g.store.run_query(&g.bob, g.orders, &q, None).await.unwrap();
    let items = after.items.unwrap();
    assert!(items.iter().all(|r| r.data.get("salary").is_none()));
    assert!(items
        .iter()
        .all(|r| r.data["total"] == serde_json::Value::Null && r.data["names"] == json!([])));
    join_role(&g.authz, &g.bob, "hr").await;

    // ③ 参照先テーブルの viewer を剥奪 → 次のリクエストで lookup / rollup が null / 空。
    g.authz
        .delete_tuple(
            &g.bob.subject(),
            Relation::Viewer,
            &g.bob.ns().data_table(&g.lines.to_string()),
        )
        .await
        .unwrap();
    let after = g.store.run_query(&g.bob, g.orders, &q, None).await.unwrap();
    for it in after.items.unwrap() {
        assert_eq!(it.data["names"], json!([]));
        assert_eq!(it.data["total"], serde_json::Value::Null);
    }

    // ④ テーブル viewer を剥奪 → 次のリクエストは拒否（見えないテーブルは存在しないのと同じ 404）。
    g.authz
        .delete_tuple(
            &g.bob.subject(),
            Relation::Viewer,
            &g.bob.ns().data_table(&g.orders.to_string()),
        )
        .await
        .unwrap();
    let r = g.store.run_query(&g.bob, g.orders, &q, None).await;
    assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
    // alice（作成者・owner）は影響を受けない。
    let r = g
        .store
        .run_query(&g.alice, g.orders, &q, None)
        .await
        .unwrap();
    assert_eq!(r.count, Some(CountResult::Exact(3)));
}

/// スキーマ改訂後の最初のクエリが新スキーマで動く（別レプリカでの改訂を含む）。テーブル削除・
/// テナント purge の後に古い世代が読まれない。
#[tokio::test]
async fn schema_cache_switches_generation_on_revision_delete_and_purge() {
    let Some((pool, authz, _)) = setup().await else {
        return;
    };
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    // 同じ DB を見る 2 つのレプリカ（キャッシュは別々）。
    let r1 = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs));
    let r2 = DataStore::new(pool, Arc::clone(&authz), Arc::new(AllowRefs));
    let t = r1
        .create_table(
            &alice,
            NewDataTable {
                name: "notes".into(),
                schema: plain(vec![field("title", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    r1.create_record(&alice, t.id, json!({"title": "a"}), None)
        .await
        .unwrap();
    r1.run_query(&alice, t.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(r1.cached_schema_versions(&tenant), vec![(t.id, 1)]);

    // 別レプリカでの改訂（列の追加）→ r1 の最初のクエリが新しい列で絞り込める。
    let mut next = t.schema.clone();
    let mut note = field("note", FieldType::Text);
    note.indexed = true;
    next.fields.push(note);
    let revised = r2
        .update_table_schema(&alice, t.id, next, Some(1), None)
        .await
        .unwrap();
    assert_eq!(revised.schema_version, 2);
    // 既存行がある列の索引はバックフィルが終わるまで準備中（data-platform §10）。
    r2.backfill_table_index(&tenant, t.id).await.unwrap();
    r2.create_record(&alice, t.id, json!({"title": "b", "note": "x"}), None)
        .await
        .unwrap();
    let q = DataQuery {
        filter: Some(data::Condition::cmp(
            "note",
            data::CondOp::Eq,
            Some(json!("x")),
        )),
        ..DataQuery::default()
    };
    let got = r1.run_query(&alice, t.id, &q, None).await.unwrap();
    let items = got.items.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].data["note"], json!("x"));
    assert_eq!(
        r1.cached_schema_versions(&tenant),
        vec![(t.id, 2)],
        "新しい世代に切り替わる"
    );
    // 改訂したレプリカ自身も古い世代を持たない。
    assert!(r2
        .cached_schema_versions(&tenant)
        .iter()
        .all(|(_, v)| *v == 2));

    // テーブル削除（別レプリカ）→ r1 は古い世代を読まない（DB の照合で見つからない）。
    r2.delete_table(&alice, t.id, None).await.unwrap();
    let r = r1
        .run_query(&alice, t.id, &DataQuery::default(), None)
        .await;
    assert!(
        matches!(r, Err(DataError::NotFound | DataError::Forbidden)),
        "{r:?}"
    );
    let r = r1.get_table(&alice, t.id, None).await;
    assert!(
        matches!(r, Err(DataError::NotFound | DataError::Forbidden)),
        "{r:?}"
    );
    assert!(
        r2.cached_schema_versions(&tenant).is_empty(),
        "削除したレプリカは世代を落とす"
    );

    // テナント purge → そのテナントの世代は全て落ち、以後も読まれない。
    let u = r1
        .create_table(
            &alice,
            NewDataTable {
                name: "other".into(),
                schema: plain(vec![field("title", FieldType::Text)]),
            },
            None,
        )
        .await
        .unwrap();
    r1.run_query(&alice, u.id, &DataQuery::default(), None)
        .await
        .unwrap();
    r2.run_query(&alice, u.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert!(!r1.cached_schema_versions(&tenant).is_empty());
    r1.purge_tenant(&tenant).await.unwrap();
    assert!(
        r1.cached_schema_versions(&tenant).is_empty(),
        "purge で世代を落とす"
    );
    for r in [&r1, &r2] {
        let res = r.run_query(&alice, u.id, &DataQuery::default(), None).await;
        assert!(res.is_err(), "purge 後に古い世代が読まれない: {res:?}");
    }
    // 削除・purge したテーブルに残った索引ジョブ（バックフィル完了後の parity 等）は、
    // エラーにならず捨てられる（共有 DB の他のテストのジョブ消費を止めない）。
    for _ in 0..1_000 {
        if !r1.consume_index_job_once().await.expect("索引ジョブ") {
            break;
        }
    }
}
