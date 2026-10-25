//! 最適化層の結合テスト（Task 15.10・監査 B2-1）: 述語の並べ替え・射影刈り込み・プランキャッシュの命中で
//! 降下を省くこと。どれも結果を変えない（速さだけに効く）ことを同時に確かめる。
//!
//! - **述語の並べ替え**（query-optimizer §5.2）: 統計があると、最上位の積の検査を「(1 − 選択率) / 単価」の
//!   大きい順に並べる（葉の種類の静的順位と逆になる組で確かめる）。行述語は最後のまま。
//! - **射影刈り込み**（data-platform §16.2）: 射影があれば本体の JSONB から射影の列と事後検査が読む列だけを
//!   DB 側で取る。行述語の列も取るので事後検査は 1 行も落とさない。
//! - **プランキャッシュ**（§16.3）: 同一形状の 2 回目は降下でテキストを組まない（バインドだけを集める）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

mod common;

use common::bulk::{seed, DONE};
use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{
    CmpOp, CondOp, Condition, DataQuery, DebugParam, FieldType, PolicyExpr, PolicyOperand,
    QueryTuning, RowPolicy, SortKey,
};
use serde_json::json;

/// 文のパラメータの中で、列 `field` の `field_id`（int2）が最初に現れる位置。
fn first_field(params: &[DebugParam], field: i16) -> Option<usize> {
    params.iter().position(|p| *p == DebugParam::I16(field))
}

/// 統計があると、検査を選択率と単価で並べる（静的順位なら先に来る `status = 完了`（95%）が最後になる）。
#[tokio::test]
async fn checks_follow_selectivity_when_statistics_exist() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &c, "order", 20_000, false, false).await;
    store.analyze_table(&tenant, table.id).await.unwrap();
    let id = |k: &str| i16::try_from(table.schema.field(k).unwrap().id).unwrap();
    let (fs, fa, fu) = (id("status"), id("amount"), id("assignee"));
    let users: Vec<serde_json::Value> = (0..1_000).map(|i| json!(format!("u{i}"))).collect();
    let q = DataQuery {
        filter: Some(Condition::All(vec![
            Condition::cmp("status", CondOp::Eq, Some(json!(DONE))),
            Condition::cmp("assignee", CondOp::In, Some(json!(users))),
            Condition::cmp("amount", CondOp::Gte, Some(json!(500_000))),
        ])),
        sort: vec![SortKey {
            field: "_updated_at".into(),
            descending: true,
        }],
        limit: Some(20),
        ..Default::default()
    };
    let auto = QueryTuning::default();
    let (r, d) = store
        .run_query_debug(&c, table.id, &q, &auto)
        .await
        .unwrap();
    assert!(d.stats.planned_with_stats, "{:?}", d.stats);
    let main = d
        .statements
        .iter()
        .find(|s| matches!(s.label, "sort_drive" | "candidates"))
        .expect("駆動の文");
    let (ps, pa) = (
        first_field(&main.params, fs).expect("status の検査"),
        first_field(&main.params, fa).expect("amount の検査"),
    );
    assert!(
        pa < ps,
        "{}: よく落ちる amount（約 55% 落ちる）を、ほとんど落ちない status（約 5%）より先に検査する",
        main.label
    );
    if let Some(pu) = first_field(&main.params, fu) {
        assert!(pu < ps, "assignee（約 90% 落ちる）も status より先");
    }
    // 結果は静的順位（強制戦略は統計を読まない）と一致する。
    for force in [data::ForcedStrategy::Sort, data::ForcedStrategy::Filter] {
        let (r2, d2) = store
            .run_query_debug(
                &c,
                table.id,
                &q,
                &QueryTuning {
                    force: Some(force),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!d2.stats.planned_with_stats);
        let ids = |r: &data::QueryResult| -> Vec<uuid::Uuid> {
            r.items.as_ref().unwrap().iter().map(|x| x.id).collect()
        };
        assert_eq!(ids(&r), ids(&r2), "{force:?}: 検査の順序は結果を変えない");
        assert_eq!(ids(&r).len(), 20);
    }
}

/// 射影があれば本体から必要なキーだけを取る。行述語の列も取るので事後検査は落とさず、結果は刈り込みなしと同じ。
#[tokio::test]
async fn projection_is_pruned_in_sql_and_keeps_policy_columns() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    let mut dept = field("assignee", FieldType::UserRef);
    dept.indexed = true;
    let memo = field("memo", FieldType::Text);
    let mut s = schema(vec![title, dept, memo]);
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::Any(vec![
            PolicyExpr::IsOwner,
            PolicyExpr::FieldCmp {
                field: "assignee".into(),
                op: CmpOp::Eq,
                value: PolicyOperand::UserId,
            },
        ]),
        write: None,
    });
    let table = create_table(&store, &alice, "prune", s).await;
    for i in 0..12 {
        let who = if i % 3 == 0 { "bob" } else { "alice" };
        store
            .create_record(
                &alice,
                table.id,
                json!({ "title": format!("t{i}"), "assignee": who, "memo": "x".repeat(4_000) }),
                None,
            )
            .await
            .unwrap();
    }
    let full = DataQuery {
        sort: vec![SortKey {
            field: "title".into(),
            descending: false,
        }],
        limit: Some(50),
        ..Default::default()
    };
    let mut pruned = full.clone();
    pruned.select = Some(vec!["title".into()]);
    let auto = QueryTuning::default();
    let (rf, _) = store
        .run_query_debug(&bob, table.id, &full, &auto)
        .await
        .unwrap();
    let (rp, dp) = store
        .run_query_debug(&bob, table.id, &pruned, &auto)
        .await
        .unwrap();
    let fetch = dp
        .statements
        .iter()
        .find(|s| s.label == "fetch")
        .expect("本体の取得");
    assert!(fetch.sql.contains("jsonb_each(r.data)"), "{}", fetch.sql);
    let key = |k: &str| format!("f{}", table.schema.field(k).unwrap().id);
    assert!(
        fetch
            .params
            .contains(&DebugParam::TextArray(vec![key("title"), key("assignee")])),
        "射影の列＋事後検査が読む行述語の列だけ: {:?}",
        fetch.params
    );
    assert_eq!(dp.stats.verify_dropped, 0, "事後検査は落とさない");
    let items_f = rf.items.unwrap();
    let items_p = rp.items.unwrap();
    assert_eq!(items_p.len(), 4, "bob に見えるのは担当の 4 行");
    assert_eq!(
        items_f.iter().map(|r| r.id).collect::<Vec<_>>(),
        items_p.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    for (a, b) in items_f.iter().zip(&items_p) {
        assert_eq!(b.data, json!({ "title": a.data["title"] }), "射影の列だけ");
    }
    // 射影なしは本体を丸ごと取る（テキストは刈り込みの有無だけで決まる）。
    let (_, df) = store
        .run_query_debug(&bob, table.id, &full, &auto)
        .await
        .unwrap();
    let f = df.statements.iter().find(|s| s.label == "fetch").unwrap();
    assert!(!f.sql.contains("jsonb_each"), "{}", f.sql);
}

/// 同一形状の 2 回目以降は降下でテキストを組まない（値・閲覧者が違っても）。計測では検証の組み直しを切る。
#[tokio::test]
async fn plan_cache_hits_skip_lowering() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    store.set_plan_cache_verify(false);
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &c, "cache", 3_000, false, false).await;
    let q = |min: i64| DataQuery {
        filter: Some(Condition::cmp("amount", CondOp::Gte, Some(json!(min)))),
        sort: vec![SortKey {
            field: "amount".into(),
            descending: true,
        }],
        limit: Some(20),
        with_count: true,
        ..Default::default()
    };
    let sort = QueryTuning {
        force: Some(data::ForcedStrategy::Sort),
        ..Default::default()
    };
    store
        .run_query_debug(&c, table.id, &q(0), &sort)
        .await
        .unwrap();
    let before = store.plan_cache_stats();
    // 計画（戦略・件数の入口）が同じになる値の範囲で、値と閲覧者を変える。
    for (who, min) in [("alice", 10), ("bob", 20), ("carol", 30)] {
        store
            .run_query_debug(&ctx(&tenant, who), table.id, &q(min), &sort)
            .await
            .unwrap();
    }
    let after = store.plan_cache_stats();
    eprintln!(
        "コンパイル {} → {}・命中で降下を省いた {} → {}",
        before.compiled, after.compiled, before.reused, after.reused
    );
    assert_eq!(after.compiled, before.compiled, "2 回目以降は組まない");
    assert!(
        after.reused >= before.reused + 3 * 2,
        "ソート駆動・件数・本体取得が命中する"
    );
    assert_eq!(after.verified, before.verified, "検証を切ると組み直さない");
    assert_eq!((after.mismatches, after.verify_failures), (0, 0));
}
