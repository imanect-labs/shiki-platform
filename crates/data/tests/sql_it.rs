//! SQL 互換インターフェース（Task 15.10・data-platform §16.1〜§16.2）の結合テスト。
//!
//! - 部分集合外の SQL が理由つきの 422（`DataError::Rejected`）になる
//! - 存在しない列・見えない列（マスク）・存在しないテーブル・見えないテーブルの応答がバイト単位で同一
//! - 入口の資源上限が上限ごとに 422（SQL 面と IR 直投入の両方）
//! - JOIN は宣言済みリンク列の等値 1 個だけ・参照先の行述語が必ず掛かる
//! - 半結合化の前後で件数と集計値が変わらない（1:N リンクを含むデータ・参照実装とも比べる）
//! - TLP（`p` / `NOT p` / `p IS NULL` の 3 区分）で SQL（3 値論理）と参照実装が一致する
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）で実行する（無ければスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, ReadTupleKey,
    Relation, Subject,
};
use common::{create_table, ctx, field, schema, setup, unique_tenant, FixedResolver};
use data::{
    CmpOp, DataError, DataQuery, DataStore, FieldPolicy, FieldType, LinkCardinality, LinkDef,
    PolicyExpr, PolicyOperand, QueryLimits, QueryTuning, RejectCode, RowPolicy, SqlRequest,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// 全許可（ただし指定したテーブルの viewer だけ拒否する）。
struct DenyTables(std::sync::Mutex<Vec<Uuid>>);

#[async_trait]
impl AuthzClient for DenyTables {
    async fn check(
        &self,
        _s: &Subject,
        r: Relation,
        o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        let s = o.to_string();
        let denied = self
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|id| s.contains(&id.to_string()));
        Ok(!(denied && matches!(r, Relation::Viewer | Relation::Editor | Relation::Owner)))
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

/// 参照実装の注文（title, amount, status, 顧客 id）。
type Order = (String, i64, String, Uuid);

fn link(key: &str, to: Uuid, card: LinkCardinality, sym: Option<&str>) -> data::FieldDef {
    let mut f = field(key, FieldType::RecordRef);
    f.ref_table = Some(to);
    f.indexed = true;
    f.link = Some(LinkDef {
        cardinality: card,
        symmetric_field: None,
        symmetric_key: sym.map(Into::into),
        reverse: false,
    });
    f
}

fn indexed(key: &str, ty: FieldType) -> data::FieldDef {
    let mut f = field(key, ty);
    f.indexed = true;
    f
}

/// 顧客（行述語: 自分が作った顧客か東京の顧客だけ見える・`note` は誰にも見えない）／注文（顧客へ多対一）／
/// 明細（注文へ多対一・注文側に対称列 `lines` = 1:N）。
struct Fx {
    store: DataStore,
    authz: Arc<DenyTables>,
    alice: AuthContext,
    bob: AuthContext,
    orders: Uuid,
    lines: Uuid,
    /// 参照実装用: 顧客 id → (都市, 作成者)
    cust: HashMap<Uuid, (String, String)>,
    /// 注文 id → (title, amount, status, 顧客 id)
    ord: HashMap<Uuid, Order>,
    /// 明細 id → (注文 id, qty)
    lin: HashMap<Uuid, (Uuid, i64)>,
}

async fn fixture(n_orders: usize) -> Option<Fx> {
    let pool = setup().await?;
    let authz = Arc::new(DenyTables(std::sync::Mutex::new(Vec::new())));
    let store = DataStore::new(pool, authz.clone(), Arc::new(FixedResolver));
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let mut city = indexed("city", FieldType::Select);
    city.options = vec!["Tokyo".into(), "Osaka".into(), "Nagoya".into()];
    let mut cs = schema(vec![
        indexed("name", FieldType::Text),
        city,
        indexed("tier", FieldType::Number),
        field("note", FieldType::Text),
    ]);
    cs.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::IsOwner,
            PolicyExpr::FieldCmp {
                field: "city".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::Lit(json!("Tokyo")),
            },
        ]),
        write: None,
    });
    cs.field_policy = vec![FieldPolicy {
        field: "note".into(),
        readable_by: PolicyExpr::HasRole {
            role: "nobody".into(),
            subtree: false,
        },
    }];
    cs.aggregate_min_rows = Some(1);
    let customers = create_table(&store, &alice, "customers", cs).await;
    let mut status = indexed("status", FieldType::Select);
    status.options = vec!["open".into(), "done".into()];
    let mut title = indexed("title", FieldType::Text);
    title.searchable = true;
    let mut os = schema(vec![
        title,
        indexed("amount", FieldType::Number),
        status,
        link(
            "customer",
            customers.id,
            LinkCardinality::ManyToOne,
            Some("orders"),
        ),
        field("memo", FieldType::Text),
    ]);
    os.aggregate_min_rows = Some(1);
    let orders = create_table(&store, &alice, "orders", os).await;
    let mut ls = schema(vec![
        link(
            "order",
            orders.id,
            LinkCardinality::ManyToOne,
            Some("lines"),
        ),
        indexed("qty", FieldType::Number),
        indexed("product", FieldType::Text),
    ]);
    ls.aggregate_min_rows = Some(1);
    let lines = create_table(&store, &alice, "lines", ls).await;

    let mut cust = HashMap::new();
    let cities = ["Tokyo", "Osaka", "Nagoya"];
    let mut cids = Vec::new();
    for i in 0..6 {
        // 2 件は bob が作る（bob は自分の顧客と東京の顧客が見える）。
        let who = if i % 3 == 2 { &bob } else { &alice };
        let c = cities[i % 3];
        let r = store
            .create_record(
                who,
                customers.id,
                json!({"name": format!("顧客{i}"), "city": c, "tier": i % 2, "note": "機密"}),
                None,
            )
            .await
            .unwrap();
        cust.insert(r.id, (c.to_string(), who.principal.id.clone()));
        cids.push(r.id);
    }
    let mut ord = HashMap::new();
    let mut lin = HashMap::new();
    for i in 0..n_orders {
        let cid = cids[i % cids.len()];
        let status = if i % 3 == 0 { "done" } else { "open" };
        let amount = (i as i64 * 37) % 100;
        let title = format!("注文{i:03}");
        // 参照先（顧客）が見える利用者が作る（見えない顧客は参照できない・書込時の検証）。
        let author = if cust[&cid].1 == "bob" { &bob } else { &alice };
        let r = store
            .create_record(
                author,
                orders.id,
                json!({"title": title, "amount": amount, "status": status, "customer": [cid.to_string()]}),
                None,
            )
            .await
            .unwrap();
        ord.insert(r.id, (title, amount, status.to_string(), cid));
        // 明細は 0〜3 件（1:N・明細の無い注文も作る）。
        for k in 0..(i % 4) {
            let qty = (k as i64 + i as i64) % 5;
            let l = store
                .create_record(
                    &alice,
                    lines.id,
                    json!({"order": [r.id.to_string()], "qty": qty, "product": format!("品{k}")}),
                    None,
                )
                .await
                .unwrap();
            lin.insert(l.id, (r.id, qty));
        }
    }
    Some(Fx {
        store,
        authz,
        alice,
        bob,
        orders: orders.id,
        lines: lines.id,
        cust,
        ord,
        lin,
    })
}

fn req(sql: &str) -> SqlRequest {
    SqlRequest {
        sql: sql.into(),
        ..SqlRequest::default()
    }
}

fn rejection(r: Result<data::SqlQueryResult, DataError>) -> data::QueryRejection {
    match r {
        Err(DataError::Rejected(x)) => *x,
        other => panic!("422 を期待: {other:?}"),
    }
}

/// 全ページを読む（`limit` ごと）。
async fn all_rows(store: &DataStore, who: &AuthContext, sql: &str) -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    let mut cursor = None;
    for _ in 0..200 {
        let r = store
            .run_sql(
                who,
                &SqlRequest {
                    sql: sql.into(),
                    cursor: cursor.clone(),
                    with_count: false,
                },
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
        out.extend(r.rows);
        match r.next_cursor {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
    panic!("ページングが終わらない: {sql}");
}

fn visible_to(fx: &Fx, who: &str, cust: Uuid) -> bool {
    let (city, owner) = &fx.cust[&cust];
    city == "Tokyo" || owner == who
}

#[tokio::test]
async fn subset_violations_are_422_with_structured_reasons() {
    let Some(fx) = fixture(4).await else { return };
    for (sql, want) in [
        ("DELETE FROM orders", RejectCode::Unsupported),
        ("WITH x AS (SELECT title FROM orders) SELECT title FROM x", RejectCode::Unsupported),
        ("SELECT title, row_number() OVER () FROM orders", RejectCode::Unsupported),
        (
            "SELECT title FROM orders o WHERE EXISTS (SELECT 1 FROM customers c WHERE c._id = o.customer)",
            RejectCode::Unsupported,
        ),
        ("SELECT title FROM orders UNION SELECT name FROM customers", RejectCode::Unsupported),
        ("SELECT upper(title) FROM orders", RejectCode::Unsupported),
        ("SELECT title FROM pg_catalog.pg_class", RejectCode::Unsupported),
        ("SELECT * FROM orders", RejectCode::Unsupported),
        ("SELECT title FROM orders LIMIT 5 OFFSET 5", RejectCode::Unsupported),
        ("SELECT title FROM orders WHERE amount::text = '1'", RejectCode::Unsupported),
        ("SELEC title FROM orders", RejectCode::SyntaxError),
    ] {
        let e = rejection(fx.store.run_sql(&fx.alice, &req(sql), None).await);
        assert_eq!(e.code, want, "{sql}: {e:?}");
        assert!(!e.message.is_empty());
        // 構造化した理由は JSON にできる（API の 422 本文）。
        let v = serde_json::to_value(&e).unwrap();
        assert!(v["code"].is_string() && v["message"].is_string(), "{v}");
    }
    // 受け付ける形は実行できる。
    let r = fx
        .store
        .run_sql(
            &fx.alice,
            &req("SELECT title, amount FROM orders WHERE amount >= 0 ORDER BY amount DESC LIMIT 2"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.columns, vec!["title", "amount"]);
    assert_eq!(r.rows.len(), 2);
}

#[tokio::test]
async fn unknown_and_invisible_names_get_byte_identical_responses() {
    let Some(fx) = fixture(4).await else { return };
    let body = |r: Result<data::SqlQueryResult, DataError>| -> String {
        let e = rejection(r);
        assert_eq!(e.code, RejectCode::UnknownName);
        serde_json::to_string(&e).unwrap()
    };
    // 列: 存在しない列と、マスクされた列（note）を同じ名前の位置で比べる。
    for (pos, tmpl) in [
        ("select", "SELECT {} FROM customers"),
        ("where", "SELECT name FROM customers WHERE {} = 'x'"),
        ("group", "SELECT {}, COUNT(*) FROM customers GROUP BY {}"),
        ("order", "SELECT name FROM customers ORDER BY {}"),
        ("qualified", "SELECT c.{} FROM customers c"),
    ] {
        // 同じ長さ・同じ位置の名前にして、位置情報まで一致させる。
        let masked = tmpl.replace("{}", "note");
        let missing = tmpl.replace("{}", "nope");
        let a = body(fx.store.run_sql(&fx.alice, &req(&masked), None).await);
        let b = body(fx.store.run_sql(&fx.alice, &req(&missing), None).await);
        assert_eq!(
            a.replace("note", "X"),
            b.replace("nope", "X"),
            "{pos}: マスク列と存在しない列の応答が違う"
        );
        // 候補（expected）にマスク列が出ない。
        assert!(!a.contains("\"note\""), "{a}");
    }
    // テーブル: 見えないテーブル（viewer でない）と存在しないテーブル。
    fx.authz.0.lock().unwrap().push(fx.lines);
    let a = body(
        fx.store
            .run_sql(&fx.alice, &req("SELECT qty FROM lines"), None)
            .await,
    );
    let b = body(
        fx.store
            .run_sql(&fx.alice, &req("SELECT qty FROM nopes"), None)
            .await,
    );
    assert_eq!(a.replace("lines", "X"), b.replace("nopes", "X"));
    // JOIN 先の見えないテーブルも同じ。
    let a = body(
        fx.store
            .run_sql(
                &fx.alice,
                &req("SELECT o.title FROM orders o JOIN lines l ON l.order = o._id"),
                None,
            )
            .await,
    );
    let b = body(
        fx.store
            .run_sql(
                &fx.alice,
                &req("SELECT o.title FROM orders o JOIN nopes l ON l.order = o._id"),
                None,
            )
            .await,
    );
    assert_eq!(a.replace("lines", "X"), b.replace("nopes", "X"));
}

#[tokio::test]
async fn each_resource_limit_is_422_on_sql_and_ir_paths() {
    let Some(fx) = fixture(2).await else { return };
    let name = |r: Result<data::SqlQueryResult, DataError>| rejection(r).limit.unwrap().name;
    let big = format!(
        "SELECT title FROM orders WHERE title = '{}'",
        "x".repeat(70_000)
    );
    assert_eq!(
        name(fx.store.run_sql(&fx.alice, &req(&big), None).await),
        "max_sql_bytes"
    );
    let deep = format!(
        "SELECT title FROM orders WHERE {}amount = 1{}",
        "(".repeat(40),
        ")".repeat(40)
    );
    assert_eq!(
        name(fx.store.run_sql(&fx.alice, &req(&deep), None).await),
        "max_ast_depth"
    );
    let wide = format!(
        "SELECT title FROM orders WHERE {}",
        vec!["title IS NULL"; 1_200].join(" OR ")
    );
    assert_eq!(
        name(fx.store.run_sql(&fx.alice, &req(&wide), None).await),
        "max_ast_nodes"
    );
    let lits: Vec<String> = (0..1_100).map(|i| i.to_string()).collect();
    let many = format!(
        "SELECT title FROM orders WHERE amount IN ({})",
        lits.join(", ")
    );
    assert_eq!(
        name(fx.store.run_sql(&fx.alice, &req(&many), None).await),
        "max_literals"
    );
    // 仕事量（決定的: 予算 1 単位ではどのクエリも超える）
    let no_work = fx.store.clone().with_query_limits(QueryLimits {
        max_compile_work: 1,
        ..QueryLimits::default()
    });
    assert_eq!(
        name(
            no_work
                .run_sql(&fx.alice, &req("SELECT title FROM orders"), None)
                .await
        ),
        "max_compile_work"
    );
    // 経過時間の外側の安全網（予算 0 では必ず超える）
    let tight = fx.store.clone().with_query_limits(QueryLimits {
        max_compile: std::time::Duration::ZERO,
        ..QueryLimits::default()
    });
    assert_eq!(
        name(
            tight
                .run_sql(&fx.alice, &req("SELECT title FROM orders"), None)
                .await
        ),
        "max_compile_ms"
    );
    let joins = "SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id \
                 JOIN orders o2 ON o2.customer = c._id JOIN customers c2 ON o2.customer = c2._id \
                 JOIN orders o3 ON o3.customer = c2._id";
    assert_eq!(
        name(fx.store.run_sql(&fx.alice, &req(joins), None).await),
        "max_join_depth"
    );

    // IR 直投入にも同じ上限（DataError::Rejected）。
    let ir_name = |r: Result<data::QueryResult, DataError>| match r {
        Err(DataError::Rejected(x)) => x.limit.unwrap().name,
        other => panic!("{other:?}"),
    };
    let q: DataQuery = serde_json::from_value(json!({
        "filter": {"cmp": {"field": "title", "op": "eq", "value": "x".repeat(70_000)}}
    }))
    .unwrap();
    assert_eq!(
        ir_name(fx.store.run_query(&fx.alice, fx.orders, &q, None).await),
        "max_sql_bytes"
    );
    let mut c = json!({"cmp": {"field": "title", "op": "exists"}});
    for _ in 0..40 {
        c = json!({"not": c});
    }
    let q: DataQuery = serde_json::from_value(json!({ "filter": c })).unwrap();
    assert_eq!(
        ir_name(fx.store.run_query(&fx.alice, fx.orders, &q, None).await),
        "max_ast_depth"
    );
    let q = DataQuery {
        select: Some(vec!["title".to_string(); 4_200]),
        ..DataQuery::default()
    };
    assert_eq!(
        ir_name(fx.store.run_query(&fx.alice, fx.orders, &q, None).await),
        "max_ast_nodes"
    );
    let q: DataQuery = serde_json::from_value(json!({
        "filter": {"all": [
            {"cmp": {"field": "amount", "op": "in", "value": (0..600).collect::<Vec<_>>()}},
            {"cmp": {"field": "amount", "op": "in", "value": (0..600).collect::<Vec<_>>()}}
        ]}
    }))
    .unwrap();
    assert_eq!(
        ir_name(fx.store.run_query(&fx.alice, fx.orders, &q, None).await),
        "max_literals"
    );
    assert_eq!(
        ir_name(
            no_work
                .run_query(&fx.alice, fx.orders, &DataQuery::default(), None)
                .await
        ),
        "max_compile_work"
    );
    assert_eq!(
        ir_name(
            tight
                .run_query(&fx.alice, fx.orders, &DataQuery::default(), None)
                .await
        ),
        "max_compile_ms"
    );
}

/// コンパイルの仕事量の予算（`MAX_COMPILE_WORK`）は決定的で、静的な上限内の最悪形（同じ列の `IN` を
/// `OR` で束ねて約 1,000 値を重複除去する＝約 55 万単位）は通し、静的な上限を緩めた運用でも
/// 正規化の二乗の費用を予算で打ち切る（早期に重複除去を省いて 422）。
#[tokio::test]
async fn compile_work_budget_passes_the_static_worst_case_and_cuts_superlinear_cost() {
    let Some(fx) = fixture(4).await else { return };
    let ins = |a: usize, b: usize| (a..b).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    let or_of_ins = |lists: usize, per: usize| {
        (0..lists)
            .map(|k| format!("amount IN ({})", ins(k * per, (k + 1) * per)))
            .collect::<Vec<_>>()
            .join(" OR ")
    };
    // 静的な上限内の最悪形（16 × 63 = 1,008 リテラル）と、二つの大きな集合の交差。
    for sql in [
        format!("SELECT title FROM orders WHERE {}", or_of_ins(16, 63)),
        format!(
            "SELECT title FROM orders WHERE amount IN ({}) AND amount IN ({})",
            ins(0, 510),
            ins(0, 510)
        ),
    ] {
        fx.store
            .run_sql(&fx.alice, &req(&sql), None)
            .await
            .expect("静的な上限内の入力は仕事量の予算で弾かない");
    }
    // 静的な上限を緩めた運用（リテラル・ノード・長さ）でも、仕事量の予算は既定のまま効く:
    // 8 × 1,000 = 8,000 値の重複除去は二乗で約 3,200 万単位になり、予算（200 万）で打ち切る。
    let loose = fx.store.clone().with_query_limits(QueryLimits {
        max_sql_bytes: 1 << 20,
        max_ast_nodes: 1 << 16,
        max_literals: 1 << 16,
        ..QueryLimits::default()
    });
    let sql = format!("SELECT title FROM orders WHERE {}", or_of_ins(8, 1_000));
    let e = rejection(loose.run_sql(&fx.alice, &req(&sql), None).await);
    assert_eq!(e.limit.unwrap().name, "max_compile_work");
    let q: DataQuery = serde_json::from_value(json!({
        "filter": {"any": (0..8)
            .map(|k| json!({"cmp": {"field": "amount", "op": "in",
                "value": (k * 1_000..(k + 1) * 1_000).collect::<Vec<_>>()}}))
            .collect::<Vec<_>>()}
    }))
    .unwrap();
    match loose.run_query(&fx.alice, fx.orders, &q, None).await {
        Err(DataError::Rejected(x)) => {
            assert_eq!(x.limit.unwrap().name, "max_compile_work");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn joins_bind_only_declared_link_columns_and_apply_reference_row_policy() {
    let Some(fx) = fixture(12).await else { return };
    let code = |r: Result<data::SqlQueryResult, DataError>| rejection(r).code;
    for sql in [
        // 任意の ON 式（リンク列でない列どうし）
        "SELECT o.title FROM orders o JOIN customers c ON o.title = c._id",
        "SELECT o.title FROM orders o JOIN customers c ON o.title = c.name",
        // 追加条件・OR・関数
        "SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id AND c.city = 'Tokyo'",
        "SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id OR o.amount > 1",
        // 別名経由の未宣言結合（リンク列の参照先は customers なのに lines へ結ぶ）
        "SELECT o.title FROM orders o JOIN lines c ON o.customer = c._id",
        // 両辺 _id・同じ表どうし・まだ結合していない表
        "SELECT o.title FROM orders o JOIN customers c ON o._id = c._id",
        "SELECT o.title FROM orders o JOIN customers c ON o.customer = o._id",
    ] {
        let c = code(fx.store.run_sql(&fx.alice, &req(sql), None).await);
        assert!(
            matches!(c, RejectCode::InvalidJoin | RejectCode::Unsupported),
            "{sql}: {c:?}"
        );
    }
    // 参照先の行述語: bob は自分の顧客と東京の顧客だけ見える。JOIN は見えない顧客の注文を落とす
    // （半結合化しても・結合を残しても同じ）。
    let expect: Vec<String> = {
        let mut v: Vec<(i64, String)> = fx
            .ord
            .values()
            .filter(|(_, _, _, c)| visible_to(&fx, "bob", *c))
            .map(|(t, a, _, _)| (*a, t.clone()))
            .collect();
        v.sort();
        v.into_iter().map(|(_, t)| t).collect()
    };
    for keep in [false, true] {
        let store = fx.store.clone().with_query_tuning(QueryTuning {
            keep_joins: keep,
            ..QueryTuning::default()
        });
        let rows = all_rows(
            &store,
            &fx.bob,
            "SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id ORDER BY o.amount, o.title LIMIT 3",
        )
        .await;
        let mut got: Vec<String> = rows
            .iter()
            .map(|r| r[0].as_str().unwrap().to_string())
            .collect();
        let mut want = expect.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want, "keep_joins={keep}");
        // 結合先の列を射影する（結合を残す）: 見えない顧客の名前は出ない。
        let rows = all_rows(
            &store,
            &fx.bob,
            "SELECT o.title, c.name, c.city FROM orders o JOIN customers c ON o.customer = c._id ORDER BY o.title LIMIT 4",
        )
        .await;
        assert_eq!(rows.len(), expect.len());
        for r in &rows {
            let city = r[2].as_str().unwrap();
            assert!(city == "Tokyo" || city == "Nagoya", "{r:?}");
        }
    }
    // 逆方向（対称列）でも同じ規則（1:N・顧客ごとの注文）。
    let rows = all_rows(
        &fx.store,
        &fx.bob,
        "SELECT c.name, o.title FROM customers c JOIN orders o ON o.customer = c._id ORDER BY c.name LIMIT 5",
    )
    .await;
    assert_eq!(rows.len(), expect.len());
}

/// 参照実装（見える注文 × 見える明細の組）での件数と集計。
#[tokio::test]
async fn semijoin_rewrite_keeps_counts_and_aggregates_on_one_to_many_links() {
    let Some(fx) = fixture(30).await else { return };
    let who = &fx.bob;
    let vis_orders: Vec<(&Uuid, &Order)> = fx
        .ord
        .iter()
        .filter(|(_, o)| visible_to(&fx, "bob", o.3))
        .collect();
    // 明細の無い注文は 1:N の INNER JOIN で消え、明細の数だけ重複する（注文・明細に行述語は無い）。
    let pairs: Vec<(&Order, i64)> = fx
        .lin
        .values()
        .filter_map(|(o, q)| fx.ord.get(o).map(|x| (x, *q)))
        .collect();
    let cases: Vec<(&str, Vec<Vec<Value>>)> = vec![
        (
            // 多対一（高々 1 件）: 半結合へ書き換えてよい。
            "SELECT COUNT(*), SUM(o.amount) FROM orders o JOIN customers c ON o.customer = c._id WHERE c.city = 'Tokyo'",
            {
                let v: Vec<_> = vis_orders
                    .iter()
                    .filter(|(_, o)| fx.cust[&o.3].0 == "Tokyo")
                    .collect();
                vec![vec![json!(v.len()), json!(v.iter().map(|(_, o)| o.1).sum::<i64>() as f64)]]
            },
        ),
        (
            // 1:N（逆方向）: 書き換えてはならない（件数は組の数）。
            "SELECT COUNT(*), SUM(o.amount), SUM(l.qty) FROM orders o JOIN lines l ON l.order = o._id",
            vec![vec![
                json!(pairs.len()),
                json!(pairs.iter().map(|(o, _)| o.1).sum::<i64>() as f64),
                json!(pairs.iter().map(|(_, q)| q).sum::<i64>() as f64),
            ]],
        ),
        (
            // 1:N の結合先の列を使わない集計も、件数が変わるので書き換えない。
            "SELECT o.status, COUNT(*) FROM orders o JOIN lines l ON l.order = o._id GROUP BY o.status ORDER BY o.status",
            {
                let mut m: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
                for (o, _) in &pairs {
                    *m.entry(o.2.clone()).or_default() += 1;
                }
                m.into_iter().map(|(k, v)| vec![json!(k), json!(v)]).collect()
            },
        ),
    ];
    for (sql, want) in &cases {
        for keep in [false, true] {
            let store = fx.store.clone().with_query_tuning(QueryTuning {
                keep_joins: keep,
                ..QueryTuning::default()
            });
            let r = store
                .run_sql(who, &req(sql), None)
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
            let norm = |rows: &Vec<Vec<Value>>| -> Vec<Vec<String>> {
                rows.iter()
                    .map(|r| {
                        r.iter()
                            .map(|v| match v {
                                Value::Number(n) => format!("{:.6}", n.as_f64().unwrap()),
                                other => other.to_string(),
                            })
                            .collect()
                    })
                    .collect()
            };
            assert_eq!(norm(&r.rows), norm(want), "{sql} keep_joins={keep}");
        }
    }
    // 行の一覧（1:N で重複する）も、ページを跨いで参照実装の組の数と一致する。
    let rows = all_rows(
        &fx.store,
        who,
        "SELECT o.title, l.qty FROM orders o JOIN lines l ON l.order = o._id WHERE l.qty >= 1 ORDER BY o.title LIMIT 4",
    )
    .await;
    let mut want: Vec<(String, i64)> = pairs
        .iter()
        .filter(|(_, q)| *q >= 1)
        .map(|(o, q)| (o.0.clone(), *q))
        .collect();
    let mut got: Vec<(String, i64)> = rows
        .iter()
        .map(|r| (r[0].as_str().unwrap().to_string(), r[1].as_i64().unwrap()))
        .collect();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    // 多対一の結合を射影しない一覧は半結合（行は重複しない）。
    let rows = all_rows(
        &fx.store,
        who,
        "SELECT o.title FROM orders o JOIN customers c ON o.customer = c._id WHERE c.tier = 1 ORDER BY o.title LIMIT 7",
    )
    .await;
    let titles: HashSet<String> = rows
        .iter()
        .map(|r| r[0].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles.len(), rows.len(), "多対一の結合で行が重複しない");
}

/// 3 値論理の参照実装（`None` = UNKNOWN）。
fn eval3(p: &P, o: &Order) -> Option<bool> {
    match p {
        P::AmountLt(n) => Some(o.1 < *n),
        P::StatusEq(s) => Some(o.2 == *s),
        P::StatusNe(s) => Some(o.2 != *s),
        P::Not(x) => eval3(x, o).map(|b| !b),
        P::And(a, b) => match (eval3(a, o), eval3(b, o)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        P::Or(a, b) => match (eval3(a, o), eval3(b, o)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
    }
}

#[derive(Clone, Debug)]
enum P {
    AmountLt(i64),
    StatusEq(String),
    StatusNe(String),
    Not(Box<P>),
    And(Box<P>, Box<P>),
    Or(Box<P>, Box<P>),
}

fn sql_of(p: &P) -> String {
    match p {
        P::AmountLt(n) => format!("amount < {n}"),
        P::StatusEq(s) => format!("status = '{s}'"),
        P::StatusNe(s) => format!("status <> '{s}'"),
        P::Not(x) => format!("NOT ({})", sql_of(x)),
        P::And(a, b) => format!("({}) AND ({})", sql_of(a), sql_of(b)),
        P::Or(a, b) => format!("({}) OR ({})", sql_of(a), sql_of(b)),
    }
}

#[tokio::test]
async fn tlp_three_partitions_match_the_three_valued_reference() {
    let Some(fx) = fixture(18).await else { return };
    // 値なしを作る（status の無い注文）。
    let blank = fx
        .store
        .create_record(
            &fx.alice,
            fx.orders,
            json!({"title": "空", "amount": 50}),
            None,
        )
        .await
        .unwrap();
    let mut ord = fx.ord.clone();
    ord.insert(blank.id, ("空".into(), 50, String::new(), Uuid::nil()));
    let leaves = vec![
        P::AmountLt(40),
        P::StatusEq("open".into()),
        P::StatusNe("done".into()),
    ];
    let mut preds = leaves.clone();
    for a in &leaves {
        preds.push(P::Not(Box::new(a.clone())));
        for b in &leaves {
            preds.push(P::And(Box::new(a.clone()), Box::new(b.clone())));
            preds.push(P::Or(
                Box::new(a.clone()),
                Box::new(P::Not(Box::new(b.clone()))),
            ));
        }
    }
    let all: HashSet<String> = ord.values().map(|o| o.0.clone()).collect();
    for p in &preds {
        let titles = |rows: Vec<Vec<Value>>| -> HashSet<String> {
            rows.into_iter()
                .map(|r| r[0].as_str().unwrap().to_string())
                .collect()
        };
        let cond_sql = sql_of(p);
        let part_t = titles(
            all_rows(
                &fx.store,
                &fx.alice,
                &format!("SELECT title FROM orders WHERE {cond_sql} ORDER BY title LIMIT 50"),
            )
            .await,
        );
        let part_f = titles(
            all_rows(
                &fx.store,
                &fx.alice,
                &format!("SELECT title FROM orders WHERE NOT ({cond_sql}) ORDER BY title LIMIT 50"),
            )
            .await,
        );
        let part_u = titles(
            all_rows(
                &fx.store,
                &fx.alice,
                &format!(
                    "SELECT title FROM orders WHERE ({cond_sql}) IS NULL ORDER BY title LIMIT 50"
                ),
            )
            .await,
        );
        // 3 区分は交わらず、和が全体（TLP）。
        assert!(
            part_t.is_disjoint(&part_f)
                && part_t.is_disjoint(&part_u)
                && part_f.is_disjoint(&part_u),
            "{cond_sql}"
        );
        let union: HashSet<String> = part_t
            .iter()
            .chain(&part_f)
            .chain(&part_u)
            .cloned()
            .collect();
        assert_eq!(union, all, "{cond_sql}");
        // 3 値論理の参照実装と一致（値なしの status は UNKNOWN）。
        for o in ord.values() {
            let r = if o.2.is_empty() {
                eval3_blank(p, o)
            } else {
                eval3(p, o)
            };
            let got = if part_t.contains(&o.0) {
                Some(true)
            } else if part_f.contains(&o.0) {
                Some(false)
            } else {
                None
            };
            assert_eq!(got, r, "{cond_sql}: {}", o.0);
        }
    }
}

/// status が値なし（NULL）の行の 3 値論理。
fn eval3_blank(p: &P, o: &Order) -> Option<bool> {
    match p {
        P::StatusEq(_) | P::StatusNe(_) => None,
        P::AmountLt(n) => Some(o.1 < *n),
        P::Not(x) => eval3_blank(x, o).map(|b| !b),
        P::And(a, b) => match (eval3_blank(a, o), eval3_blank(b, o)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        P::Or(a, b) => match (eval3_blank(a, o), eval3_blank(b, o)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
    }
}
