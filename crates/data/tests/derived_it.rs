//! リンク・ルックアップ・ロールアップ・数式（Task 15.4・data-platform §7）の結合テスト。
//!
//! 受け入れ条件:
//! - N:N リンクの両方向が引け、両方向とも 1 パーティションに枝刈りされる（EXPLAIN）
//! - `one_to_one` / `one_to_many` の一意性違反が 422
//! - `per_viewer` / `volatile` 列への `indexed` / `unique` 宣言が 422
//! - 数式の循環参照が 422
//! - 金額の四則演算・集計が `numeric` で正確
//! - 参照先レコードの金額更新で材料化ロールアップが再計算され、完了まではクエリ対象外（409）
//!
//! 閲覧者ごとの権限（PIT-20・PIT-71 の 4 階層）は `policy_threat_it.rs` 側で実 OpenFGA を使って検証する。
//! ここでも材料化（`invariant`）の再計算は実 OpenFGA（タプルの包含の証明が要る）で走らせる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    clippy::print_stderr,
    clippy::cognitive_complexity
)]

mod common;

use std::sync::Arc;

use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::AuthzClient;
use common::{
    create_table, ctx, explain_small, field, keys_of, record_int_of, relations_in_plan, schema,
    setup, store_with, unique_tenant, FixedResolver,
};
use data::{
    CondOp, Condition, DataError, DataQuery, DataStore, DebugParam, DebugStatement, FieldDef,
    FieldType, LinkCardinality, LinkDef, Materialization, NewDataTable, NumericSpec, QueryTuning,
    ResultType, RollupDef, RollupOp, SortKey, TableSchema, LINK_TRAVERSAL_SQL,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

fn link(key: &str, to: Uuid, card: LinkCardinality, sym: Option<&str>) -> FieldDef {
    let mut f = field(key, FieldType::RecordRef);
    f.ref_table = Some(to);
    f.link = Some(LinkDef {
        cardinality: card,
        symmetric_field: None,
        symmetric_key: sym.map(Into::into),
        reverse: false,
    });
    f
}

fn rollup(key: &str, link: &str, target: Option<&str>, op: RollupOp) -> FieldDef {
    let mut f = field(key, FieldType::Rollup);
    f.rollup = Some(RollupDef {
        link: link.into(),
        target: target.map(Into::into),
        op,
    });
    f
}

fn formula(key: &str, e: Value) -> FieldDef {
    let mut f = field(key, FieldType::Formula);
    f.formula = Some(serde_json::from_value(e).unwrap());
    f
}

fn ids(v: &[Uuid]) -> Value {
    Value::Array(v.iter().map(|u| json!(u.to_string())).collect())
}

async fn rec(store: &DataStore, c: &authz::AuthContext, t: Uuid, d: Value) -> Uuid {
    store.create_record(c, t, d, None).await.expect("create").id
}

async fn get(store: &DataStore, c: &authz::AuthContext, t: Uuid, id: Uuid) -> Value {
    store.get_record(c, t, id, None).await.expect("get").data
}

/// 受け入れ条件: N:N リンクの両方向が引け、両方向とも 1 パーティションに枝刈りされる。
#[tokio::test]
async fn many_to_many_link_reads_both_directions_with_partition_pruning() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let customers = create_table(
        &store,
        &c,
        "customers",
        schema(vec![field("name", FieldType::Text)]),
    )
    .await;
    let mut cust = link(
        "customers",
        customers.id,
        LinkCardinality::ManyToMany,
        Some("projects"),
    );
    cust.indexed = true;
    let projects = create_table(
        &store,
        &c,
        "projects",
        schema(vec![field("title", FieldType::Text), cust]),
    )
    .await;
    // 対称列は参照先のスキーマ上のメタデータ（読取専用・サーバが作る）。
    let customers = store.get_table(&c, customers.id, None).await.unwrap();
    let sym = customers.schema.field("projects").expect("対称列");
    let fwd = projects.schema.field("customers").unwrap();
    assert!(sym.is_reverse_link());
    assert_eq!(sym.ref_table, Some(projects.id));
    assert_eq!(sym.link.as_ref().unwrap().symmetric_field, Some(fwd.id));
    assert_eq!(fwd.link.as_ref().unwrap().symmetric_field, Some(sym.id));

    let c1 = rec(&store, &c, customers.id, json!({"name": "A 社"})).await;
    let c2 = rec(&store, &c, customers.id, json!({"name": "B 社"})).await;
    let p1 = rec(
        &store,
        &c,
        projects.id,
        json!({"title": "p1", "customers": ids(&[c1, c2])}),
    )
    .await;
    let p2 = rec(
        &store,
        &c,
        projects.id,
        json!({"title": "p2", "customers": ids(&[c1])}),
    )
    .await;
    // 順方向（本体の配列）と逆方向（対称列＝逆方向行を読む）の両方が引ける。
    assert_eq!(
        get(&store, &c, projects.id, p1).await["customers"],
        ids(&[c1, c2])
    );
    assert_eq!(
        get(&store, &c, customers.id, c1).await["projects"],
        ids(&[p1, p2])
    );
    assert_eq!(
        get(&store, &c, customers.id, c2).await["projects"],
        ids(&[p1])
    );
    // 付け替え: 逆方向行も同じ Tx で更新される（残る相手の並び順は動かない）。
    let r = store.get_record(&c, projects.id, p1, None).await.unwrap();
    store
        .update_record(
            &c,
            projects.id,
            p1,
            json!({"customers": ids(&[c2])}),
            r.rev,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        get(&store, &c, customers.id, c1).await["projects"],
        ids(&[p2])
    );
    assert_eq!(
        get(&store, &c, customers.id, c2).await["projects"],
        ids(&[p1])
    );
    // 対称列へは書けない（422）。
    let r = store
        .update_record(
            &c,
            customers.id,
            c1,
            json!({"projects": ids(&[p1])}),
            1,
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");

    // 逆方向行は参照先側の table_int に載る（二重書込ではなく同一 Tx の 2 行）。
    let (ti, cust_tb) = keys_of(&pool, &tenant, customers.id).await;
    let (_, proj_tb) = keys_of(&pool, &tenant, projects.id).await;
    let rev: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_link WHERE tenant_int = $1 AND table_int = $2 AND is_reverse",
    )
    .bind(ti)
    .bind(cust_tb)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rev, 2, "p1→c2・p2→c1 の逆方向行");

    // EXPLAIN: 両方向の走査がそれぞれ 1 パーティション（data_link・相手側の data_record）。
    for (tb, fid, dst_tb, src) in [
        (
            proj_tb,
            fwd.id,
            cust_tb,
            record_int_of(&pool, ti, proj_tb, p1).await,
        ),
        (
            cust_tb,
            sym.id,
            proj_tb,
            record_int_of(&pool, ti, cust_tb, c1).await,
        ),
    ] {
        let st = DebugStatement {
            label: "link",
            sql: LINK_TRAVERSAL_SQL.into(),
            params: vec![
                DebugParam::I32(ti),
                DebugParam::I64(tb),
                DebugParam::I16(i16::try_from(fid).unwrap()),
                DebugParam::I64(dst_tb),
                DebugParam::I64Array(vec![src]),
            ],
        };
        let plan = explain_small(&pool, &st).await;
        assert_eq!(relations_in_plan(&plan, "data_link").len(), 1, "{plan}");
        assert_eq!(relations_in_plan(&plan, "data_record").len(), 1, "{plan}");
    }
    // 順方向のフィルタ（クエリコンパイラ）も 1 パーティション。
    let q = DataQuery {
        filter: Some(Condition::cmp(
            "customers",
            CondOp::Eq,
            Some(json!(c2.to_string())),
        )),
        ..DataQuery::default()
    };
    let (r, dbg) = store
        .run_query_debug(&c, projects.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert_eq!(r.items.unwrap().len(), 1);
    for st in &dbg.statements {
        let plan = explain_small(&pool, st).await;
        assert!(relations_in_plan(&plan, "data_link").len() <= 1, "{plan}");
    }

    // 対称列越しのロールアップ・lookup（逆方向から見た集計）も引ける（閲覧者の権限で計算・per_viewer）。
    let mut cs = customers.schema.clone();
    let mut titles = field("project_titles", FieldType::Lookup);
    titles.lookup = Some(data::LookupDef {
        via_field: "projects".into(),
        target_field: "title".into(),
    });
    cs.fields.push(titles);
    cs.fields
        .push(rollup("project_count", "projects", None, RollupOp::Count));
    cs.aggregate_min_rows = Some(1);
    let customers2 = store
        .update_table_schema(&c, customers.id, cs, None, None)
        .await
        .unwrap();
    assert_eq!(
        customers2
            .schema
            .field("project_count")
            .unwrap()
            .materialization,
        Some(Materialization::PerViewer),
        "対称列越しは参照元の行集合が閲覧者ごとに変わる"
    );
    let d = get(&store, &c, customers.id, c2).await;
    assert_eq!(d["project_titles"], json!(["p1"]));
    // 参照元（projects）の K は既定 5 なので、件数 1 は抑制される（参照先＝projects の宣言を継承）。
    assert_eq!(d["project_count"], Value::Null);

    // 参照元の削除で逆方向行も消える。
    let r = store.get_record(&c, projects.id, p2, None).await.unwrap();
    store
        .delete_record(&c, projects.id, p2, r.rev, None)
        .await
        .unwrap();
    assert_eq!(
        get(&store, &c, customers.id, c1).await["projects"],
        json!([])
    );
    // parity（本体の配列と順方向行）が一致する。
    let report = store
        .verify_index_parity(&tenant, projects.id)
        .await
        .unwrap();
    assert!(report.is_consistent(), "{report:?}");
}

/// 受け入れ条件: one_to_one / one_to_many の一意性違反が 422。
#[tokio::test]
async fn link_cardinality_violations_are_422() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let target = create_table(
        &store,
        &c,
        "target",
        schema(vec![field("n", FieldType::Text)]),
    )
    .await;
    let t = target.id;
    let cs: Vec<Uuid> = {
        let mut v = Vec::new();
        for i in 0..4 {
            v.push(rec(&store, &c, t, json!({"n": format!("c{i}")})).await);
        }
        v
    };
    // dst 側の一意は対称列（逆方向行）で強制するので、対称列なしの one_to_one は 422。
    let bad = store
        .create_table(
            &c,
            NewDataTable {
                name: "bad".into(),
                schema: schema(vec![link("l", t, LinkCardinality::OneToOne, None)]),
            },
            None,
        )
        .await;
    assert!(matches!(bad, Err(DataError::Invalid(_))), "{bad:?}");

    let invalid = |r: Result<data::DataRecord, DataError>| {
        assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    };
    // one_to_one。
    let oo = create_table(
        &store,
        &c,
        "oo",
        schema(vec![link(
            "l",
            t,
            LinkCardinality::OneToOne,
            Some("oo_back"),
        )]),
    )
    .await;
    rec(&store, &c, oo.id, json!({"l": ids(&[cs[0]])})).await;
    invalid(
        store
            .create_record(&c, oo.id, json!({"l": ids(&[cs[0]])}), None)
            .await,
    );
    invalid(
        store
            .create_record(&c, oo.id, json!({"l": ids(&[cs[1], cs[2]])}), None)
            .await,
    );
    // one_to_many（src 1 : dst 複数・各 dst は 1 つの src からだけ）。
    let om = create_table(
        &store,
        &c,
        "om",
        schema(vec![link(
            "l",
            t,
            LinkCardinality::OneToMany,
            Some("om_back"),
        )]),
    )
    .await;
    rec(&store, &c, om.id, json!({"l": ids(&[cs[0], cs[1]])})).await;
    invalid(
        store
            .create_record(&c, om.id, json!({"l": ids(&[cs[1]])}), None)
            .await,
    );
    rec(&store, &c, om.id, json!({"l": ids(&[cs[2]])})).await;
    // many_to_one（v1 の record_ref と同じ向き）: src 側は 1 件まで、dst は共有できる。
    let mo = create_table(
        &store,
        &c,
        "mo",
        schema(vec![link("l", t, LinkCardinality::ManyToOne, None)]),
    )
    .await;
    rec(&store, &c, mo.id, json!({"l": ids(&[cs[3]])})).await;
    rec(&store, &c, mo.id, json!({"l": ids(&[cs[3]])})).await;
    invalid(
        store
            .create_record(&c, mo.id, json!({"l": ids(&[cs[0], cs[1]])}), None)
            .await,
    );
    // 値は常に配列（単一 UUID 文字列は v1 の record_ref だけ）。
    invalid(
        store
            .create_record(&c, mo.id, json!({"l": cs[0].to_string()}), None)
            .await,
    );

    // リンク定義は改訂で変えられない・対称列はサーバだけが作る。
    let mut next = mo.schema.clone();
    next.fields[0].link.as_mut().unwrap().cardinality = LinkCardinality::ManyToMany;
    let r = store.update_table_schema(&c, mo.id, next, None, None).await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    let mut forged = link("x", t, LinkCardinality::ManyToMany, None);
    forged.link.as_mut().unwrap().reverse = true;
    forged.link.as_mut().unwrap().symmetric_field = Some(16);
    let r = store
        .create_table(
            &c,
            NewDataTable {
                name: "forged".into(),
                schema: schema(vec![forged]),
            },
            None,
        )
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
}

fn lines_schema(k: Option<i64>) -> TableSchema {
    let mut amount = field("amount", FieldType::Number);
    amount.numeric_spec = Some(NumericSpec {
        scale: 2,
        on_excess_scale: data::ExcessScale::Reject,
        min: None,
        max: None,
    });
    let mut s = schema(vec![amount, field("name", FieldType::Text)]);
    s.aggregate_min_rows = k;
    s
}

/// 受け入れ条件: per_viewer / volatile 列への indexed / unique が 422。数式の循環が 422。
#[tokio::test]
async fn viewer_dependent_or_volatile_columns_cannot_be_indexed_and_cycles_are_rejected() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let lines = create_table(&store, &c, "lines", lines_schema(Some(1))).await;
    let base = || {
        vec![
            link("items", lines.id, LinkCardinality::ManyToMany, None),
            field("price", FieldType::Number),
        ]
    };
    let create = |fields: Vec<FieldDef>, name: &str| {
        let store = store.clone();
        let c = c.clone();
        let name = name.to_string();
        async move {
            store
                .create_table(
                    &c,
                    NewDataTable {
                        name,
                        schema: schema(fields),
                    },
                    None,
                )
                .await
        }
    };
    // 参照先テーブルの viewer 集合の包含を証明できない（既定）→ per_viewer → indexed は 422。
    let mut total = rollup("total", "items", Some("amount"), RollupOp::Sum);
    total.indexed = true;
    let mut f = base();
    f.push(total.clone());
    let r = create(f, "o1").await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // now を含む数式（volatile）も 422。
    let mut age = formula("age", json!({"date_diff": ["now", {"lit": "2026-01-01"}]}));
    age.indexed = true;
    let mut f = base();
    f.push(age.clone());
    let r = create(f, "o2").await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // unique は派生フィールドには常に 422。
    let mut uq = formula("double", json!({"mul": [{"field": "price"}, {"lit": 2}]}));
    uq.unique = true;
    let mut f = base();
    f.push(uq);
    let r = create(f, "o3").await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    // indexed を外せば保存でき、サーバが材料化可否と型を決める（入力の宣言は信用しない）。
    total.indexed = false;
    total.materialization = Some(Materialization::Invariant);
    age.indexed = false;
    let mut f = base();
    f.extend([total, age]);
    let t = create(f, "o4").await.unwrap();
    let total = t.schema.field("total").unwrap();
    assert_eq!(total.materialization, Some(Materialization::PerViewer));
    assert_eq!(total.result_type, Some(ResultType::Number));
    assert_eq!(
        t.schema.field("age").unwrap().materialization,
        Some(Materialization::Volatile)
    );
    // per_viewer 列でのソート・フィルタは 422。
    let q = DataQuery {
        sort: vec![SortKey {
            field: "total".into(),
            descending: true,
        }],
        ..DataQuery::default()
    };
    let r = store.run_query(&c, t.id, &q, None).await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");

    // 数式の循環（自テーブル内）は 422。
    let mut f = base();
    f.push(formula("x", json!({"add": [{"field": "y"}, {"lit": 1}]})));
    f.push(formula("y", json!({"add": [{"field": "x"}, {"lit": 1}]})));
    let r = create(f, "cyc").await;
    assert!(
        matches!(&r, Err(DataError::Invalid(m)) if m.contains("循環")),
        "{r:?}"
    );
    // テーブルを跨ぐ循環: lines.back（orders の total を集計）→ orders.total（lines.back を集計）。
    let orders = create(
        vec![
            link(
                "items",
                lines.id,
                LinkCardinality::ManyToMany,
                Some("orders"),
            ),
            rollup("total", "items", Some("amount"), RollupOp::Sum),
        ],
        "orders",
    )
    .await
    .unwrap();
    let mut ls = store.get_table(&c, lines.id, None).await.unwrap().schema;
    ls.fields
        .push(rollup("back", "orders", Some("total"), RollupOp::Sum));
    store
        .update_table_schema(&c, lines.id, ls, None, None)
        .await
        .expect("lines.back は orders.total に依存（まだ循環なし）");
    let mut os = orders.schema.clone();
    os.fields
        .push(rollup("loop_total", "items", Some("back"), RollupOp::Sum));
    os.fields.push(formula(
        "cyc",
        json!({"add": [{"field": "loop_total"}, {"field": "cyc"}]}),
    ));
    let r = store
        .update_table_schema(&c, orders.id, os, None, None)
        .await;
    assert!(
        matches!(&r, Err(DataError::Invalid(m)) if m.contains("循環")),
        "{r:?}"
    );
}

/// 受け入れ条件: 金額の四則演算・集計が numeric で正確（浮動小数点誤差が出ない）。
#[tokio::test]
async fn money_arithmetic_and_rollups_are_exact() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let lines = create_table(&store, &c, "lines", lines_schema(Some(1))).await;
    let mut qty = field("qty", FieldType::Number);
    qty.required = false;
    let orders = create_table(
        &store,
        &c,
        "orders",
        schema(vec![
            link("items", lines.id, LinkCardinality::ManyToMany, None),
            field("unit", FieldType::Number),
            qty,
            rollup("total", "items", Some("amount"), RollupOp::Sum),
            rollup("avg", "items", Some("amount"), RollupOp::Avg),
            rollup("n", "items", None, RollupOp::Count),
            rollup("names", "items", Some("name"), RollupOp::Concat),
            formula(
                "with_tax",
                json!({"round": [{"mul": [{"field": "total"}, {"lit": 1.1}]}, 2]}),
            ),
            formula("subtotal", json!({"mul": [{"field": "unit"}, {"field": "qty"}]})),
            formula(
                "net",
                json!({"sub": [{"add": [{"field": "total"}, {"field": "subtotal"}]}, {"lit": 0.3}]}),
            ),
        ]),
    )
    .await;
    let l1 = rec(&store, &c, lines.id, json!({"amount": 0.1, "name": "a"})).await;
    let l2 = rec(&store, &c, lines.id, json!({"amount": 0.2, "name": "b"})).await;
    let l3 = rec(&store, &c, lines.id, json!({"amount": 19.99, "name": "c"})).await;
    let o = rec(
        &store,
        &c,
        orders.id,
        json!({"items": ids(&[l1, l2, l3]), "unit": 0.1, "qty": 3}),
    )
    .await;
    let d = get(&store, &c, orders.id, o).await;
    // f64 なら 0.1 + 0.2 + 19.99 = 20.290000000000003。
    assert_eq!(d["total"], json!(20.29));
    assert_eq!(
        d["avg"],
        json!(6.76333333),
        "除算は小数桁（2）+ 6 桁へ偶数丸め"
    );
    assert_eq!(d["n"], json!(3));
    assert_eq!(d["names"], json!("a, b, c"));
    assert_eq!(d["with_tax"], json!(22.32), "20.29 × 1.1 = 22.319 → 22.32");
    // f64 なら 0.1 × 3 = 0.30000000000000004。
    assert_eq!(d["subtotal"], json!(0.3));
    assert_eq!(d["net"], json!(20.29));
    // 一覧（クエリ経路）でも同じ値。
    let page = store
        .run_query(&c, orders.id, &DataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(page.items.unwrap()[0].data["total"], json!(20.29));
    // 派生フィールドへは書けない。
    let r = store
        .create_record(&c, orders.id, json!({"total": 1}), None)
        .await;
    assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
}

/// ロールアップのスモールセル抑制は参照先の aggregate_min_rows を継承する（PIT-17）。
#[tokio::test]
async fn rollup_small_cell_suppression_inherits_reference_threshold() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let lines = create_table(&store, &c, "lines", lines_schema(None)).await;
    let orders = create_table(
        &store,
        &c,
        "orders",
        schema(vec![
            link("items", lines.id, LinkCardinality::ManyToMany, None),
            rollup("total", "items", Some("amount"), RollupOp::Sum),
            rollup("n", "items", None, RollupOp::Count),
        ]),
    )
    .await;
    let l1 = rec(&store, &c, lines.id, json!({"amount": 5})).await;
    let l2 = rec(&store, &c, lines.id, json!({"amount": 7})).await;
    let o = rec(&store, &c, orders.id, json!({"items": ids(&[l1, l2])})).await;
    // 既定 K = 5 に対して 2 件 → 抑制。
    let d = get(&store, &c, orders.id, o).await;
    assert_eq!(d["total"], Value::Null);
    assert_eq!(d["n"], Value::Null);
    // 参照先の K を 2 に下げれば出る（参照先の宣言を継承する）。
    let mut s = store.get_table(&c, lines.id, None).await.unwrap().schema;
    s.aggregate_min_rows = Some(2);
    store
        .update_table_schema(&c, lines.id, s, None, None)
        .await
        .unwrap();
    let d = get(&store, &c, orders.id, o).await;
    assert_eq!(d["total"], json!(12));
    assert_eq!(d["n"], json!(2));
}

async fn fga_store(pool: PgPool) -> Option<(DataStore, Arc<dyn AuthzClient>)> {
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let model: Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url,
            store_name: format!("shiki-data-derived-test-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("OpenFGA 接続");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    Some((
        DataStore::new(pool, Arc::clone(&authz), Arc::new(FixedResolver)),
        authz,
    ))
}

/// 材料化した値（索引行）をレコードごとに引く（`orders` の並び順で返す・テスト専用）。
async fn materialized(
    pool: &PgPool,
    tenant: &str,
    table: Uuid,
    field_id: u16,
    orders: &[Uuid],
) -> Vec<Option<String>> {
    let (ti, tb) = keys_of(pool, tenant, table).await;
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT r.id, i.val::text FROM data_index_num i \
         JOIN data_record r ON r.tenant_int = i.tenant_int AND r.table_int = i.table_int \
          AND r.record_int = i.record_int \
         WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = $3",
    )
    .bind(ti)
    .bind(tb)
    .bind(i16::try_from(field_id).unwrap())
    .fetch_all(pool)
    .await
    .unwrap();
    orders
        .iter()
        .map(|o| rows.iter().find(|(id, _)| id == o).map(|(_, v)| v.clone()))
        .collect()
}

/// 受け入れ条件: 参照先レコードの金額を更新すると参照元の材料化ロールアップが再計算され、
/// 再計算が終わるまでは当該列がクエリ対象外（409）になる。
#[tokio::test]
async fn materialized_rollup_is_recomputed_and_excluded_until_done() {
    recompute_case(None).await;
}

/// 同上（対称列つきのリンク: 影響する参照元を参照先の逆方向行から主キーで引く経路）。
#[tokio::test]
async fn materialized_rollup_over_symmetric_link_is_recomputed() {
    recompute_case(Some("orders")).await;
}

async fn recompute_case(symmetric: Option<&str>) {
    let Some(pool) = setup().await else { return };
    let Some((store, _authz)) = fga_store(pool.clone()).await else {
        return;
    };
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let app = Uuid::new_v4();
    let lines = store
        .create_table_for_app(
            &c,
            NewDataTable {
                name: "lines".into(),
                schema: lines_schema(Some(1)),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let mut total = rollup("total", "items", Some("amount"), RollupOp::Sum);
    total.indexed = true;
    let orders = store
        .create_table_for_app(
            &c,
            NewDataTable {
                name: "orders".into(),
                schema: schema(vec![
                    link("items", lines.id, LinkCardinality::ManyToMany, symmetric),
                    total,
                ]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let total = orders.schema.field("total").unwrap().clone();
    assert_eq!(
        total.materialization,
        Some(Materialization::Invariant),
        "同一ミニアプリ・viewer 集合の包含・行述語/マスク/個別共有なし"
    );
    let l1 = rec(&store, &c, lines.id, json!({"amount": 100})).await;
    let l2 = rec(&store, &c, lines.id, json!({"amount": 250})).await;
    let l3 = rec(&store, &c, lines.id, json!({"amount": 300})).await;
    let o1 = rec(&store, &c, orders.id, json!({"items": ids(&[l1, l2])})).await;
    let o2 = rec(&store, &c, orders.id, json!({"items": ids(&[l3])})).await;
    // 参照元の書込は同じ Tx で材料化する（直後からソートに使える）。
    assert_eq!(
        materialized(&pool, &tenant, orders.id, total.id, &[o1, o2]).await,
        vec![Some("350".to_string()), Some("300".to_string())]
    );
    let by_total = DataQuery {
        sort: vec![SortKey {
            field: "total".into(),
            descending: true,
        }],
        ..DataQuery::default()
    };
    let order = |r: data::QueryResult| -> Vec<Uuid> {
        r.items.unwrap().into_iter().map(|x| x.id).collect()
    };
    assert_eq!(
        order(
            store
                .run_query(&c, orders.id, &by_total, None)
                .await
                .unwrap()
        ),
        vec![o1, o2]
    );

    // 参照先の金額を更新 → 参照元は再計算待ち（クエリ対象外＝409）。表示は読取時計算なので正しい。
    let r = store.get_record(&c, lines.id, l3, None).await.unwrap();
    store
        .update_record(&c, lines.id, l3, json!({"amount": 1000}), r.rev, None)
        .await
        .unwrap();
    let r = store.run_query(&c, orders.id, &by_total, None).await;
    assert!(matches!(r, Err(DataError::Conflict(_))), "{r:?}");
    let filter = DataQuery {
        filter: Some(Condition::cmp("total", CondOp::Gt, Some(json!(500)))),
        ..DataQuery::default()
    };
    assert!(matches!(
        store.run_query(&c, orders.id, &filter, None).await,
        Err(DataError::Conflict(_))
    ));
    assert_eq!(get(&store, &c, orders.id, o2).await["total"], json!(1000));
    // 世代付きジョブが積まれている。
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_queue WHERE queue = 'data_index_backfill' AND tenant_id = $1 \
         AND payload ->> 'op' = 'derived' AND payload ->> 'table_id' = $2",
    )
    .bind(&tenant)
    .bind(orders.id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(jobs >= 1, "再計算ジョブ");
    // 再計算が終われば新しい値で並ぶ。
    store.recompute_derived(&tenant, orders.id).await.unwrap();
    assert_eq!(
        materialized(&pool, &tenant, orders.id, total.id, &[o1, o2]).await,
        vec![Some("350".to_string()), Some("1000".to_string())]
    );
    assert_eq!(
        order(
            store
                .run_query(&c, orders.id, &by_total, None)
                .await
                .unwrap()
        ),
        vec![o2, o1]
    );
    assert_eq!(
        order(store.run_query(&c, orders.id, &filter, None).await.unwrap()),
        vec![o2]
    );
    // 参照先の削除も再計算待ちになり、終われば反映される。
    let r = store.get_record(&c, lines.id, l2, None).await.unwrap();
    store
        .delete_record(&c, lines.id, l2, r.rev, None)
        .await
        .unwrap();
    assert!(matches!(
        store.run_query(&c, orders.id, &by_total, None).await,
        Err(DataError::Conflict(_))
    ));
    store.recompute_derived(&tenant, orders.id).await.unwrap();
    assert_eq!(
        materialized(&pool, &tenant, orders.id, total.id, &[o1, o2]).await,
        vec![Some("100".to_string()), Some("1000".to_string())]
    );
    // 参照元自身のリンク付け替えは同じ Tx で再計算される（待ちを作らない）。
    let r = store.get_record(&c, orders.id, o1, None).await.unwrap();
    store
        .update_record(
            &c,
            orders.id,
            o1,
            json!({"items": ids(&[l1, l3])}),
            r.rev,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        materialized(&pool, &tenant, orders.id, total.id, &[o1, o2]).await,
        vec![Some("1100".to_string()), Some("1000".to_string())]
    );
    assert_eq!(
        order(
            store
                .run_query(&c, orders.id, &by_total, None)
                .await
                .unwrap()
        ),
        vec![o1, o2]
    );
}
