//! 予算付き実行と件数探索の契約を直接確かめる結合テスト（query-optimizer.md §4.3・§6.2・監査 B1-4）。
//!
//! - **同じ戦略の継続（倍々）では途中結果を保持する**: 走査は先へ進むだけで、読み直さない
//! - **別戦略への切替では途中結果を捨てる**: 切替先は、その戦略を最初から選んだときと同じ文で引く
//! - **件数探索は上限 1,000・1 計画 3 本まで**
//! - **行述語の選言の件数探索はリクエストを跨いでキャッシュしない**（Q の述語はキャッシュする）
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）。統計表（O2）を持たないテーブルで O1 の経路を通す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::cast_possible_truncation
)]

mod common;

use common::bulk::{seed, DONE, PENDING};
use common::{bind_param, ctx, setup, store_with, unique_tenant};
use data::{
    CondOp, Condition, DataQuery, DebugParam, DebugStatement, ForcedStrategy, QueryDebug,
    QueryTuning, SortKey,
};
use serde_json::{json, Value};
use uuid::Uuid;

fn ids(r: &data::QueryResult) -> Vec<Uuid> {
    r.items
        .as_ref()
        .unwrap()
        .iter()
        .map(|x| x.id)
        .collect::<Vec<_>>()
}

/// 文の形（プレースホルダの番号を消す。`drop_cap` なら候補集合の上限 `LIMIT $n` を 1 つ除く）。
fn shape(sql: &str, drop_cap: bool) -> String {
    let mut s = sql.to_string();
    if drop_cap {
        // 候補集合の上限は CTE の中の最初の `LIMIT $n`（値索引の並び `ORDER BY …` の後に付くこともある）。
        let start = s.find(" LIMIT $").expect("候補の上限がある");
        let end = start + s[start..].find("::int8)").unwrap() + "::int8".len();
        s.replace_range(start..end, "");
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '$' {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        }
    }
    out
}

fn labeled<'a>(d: &'a QueryDebug, label: &str) -> Vec<&'a DebugStatement> {
    d.statements.iter().filter(|s| s.label == label).collect()
}

/// 予算切れで同じ戦略を倍々で続けた回は、途中結果を保持して先へ進む（読み直さない）。
#[tokio::test]
async fn doubling_keeps_partial_results_and_never_rescans() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let c = ctx(&unique_tenant(), "alice");
    let table = seed(&pool, &store, &c, "doubling", 2_000, false, false).await;
    let limit = 30;
    // 条件なし（候補集合の戦略が無い）: 予算切れのたびに同じソート駆動を倍の予算で続けるしかない。
    let q = DataQuery {
        sort: vec![SortKey {
            field: "amount".into(),
            descending: false,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    let (r, d) = store
        .run_query_debug(
            &c,
            table.id,
            &q,
            &QueryTuning {
                sort_budget: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!d.stats.planned_with_stats, "O1 の経路: {:?}", d.stats);
    assert!(d.stats.doublings >= 3, "{:?}", d.stats);
    assert!(!d.stats.switched, "{:?}", d.stats);
    assert_eq!(d.stats.strategy, "sort");
    let steps = labeled(&d, "sort_drive");
    assert_eq!(
        steps.len() as u32,
        d.stats.doublings + 1,
        "倍々 1 回ごとに走査 1 文"
    );
    // 途中結果を捨てて先頭から読み直していれば 2+4+8+16+32 = 62 件を超えて走査する。保持して先へ
    // 進むので、1 ページ分（L+1 件）＋最後の予算の余りまでしか走査しない。
    assert!(
        d.stats.scanned <= (u64::from(limit) + 1) + 2,
        "読み直している: {:?}",
        d.stats
    );
    // 結果は予算なしのソート駆動と一致する（途中結果が欠けていない）。
    let (full, fd) = store
        .run_query_debug(
            &c,
            table.id,
            &q,
            &QueryTuning {
                force: Some(ForcedStrategy::Sort),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(fd.stats.doublings, 0);
    assert_eq!(ids(&r), ids(&full));
    assert!(r.next_cursor.is_some() && full.next_cursor.is_some());
    assert_eq!(ids(&r).len(), limit as usize);
}

/// 予算切れで別戦略（フィルタ駆動）へ切り替えた回は、ソート駆動の途中結果を捨て、フィルタ駆動を
/// 最初から選んだときと**同じ文**（同じ開始位置）で引き直す（混ぜない・PIT-76）。
#[tokio::test]
async fn switching_discards_partial_results() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let c = ctx(&unique_tenant(), "alice");
    // 承認待ちは g % 3333 == 0 の 3 件だけ（金額順に並べるとソート駆動はまず見つけられない）。
    let table = seed(&pool, &store, &c, "switch", 10_000, false, false).await;
    let q = DataQuery {
        filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!(PENDING)))),
        sort: vec![SortKey {
            field: "amount".into(),
            descending: true,
        }],
        limit: Some(5),
        ..Default::default()
    };
    let (r, d) = store
        .run_query_debug(
            &c,
            table.id,
            &q,
            &QueryTuning {
                sort_budget: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!d.stats.planned_with_stats, "O1 の経路: {:?}", d.stats);
    assert!(d.stats.switched, "{:?}", d.stats);
    assert_eq!(d.stats.strategy, "filter", "{:?}", d.stats);
    assert!(
        !labeled(&d, "sort_drive").is_empty(),
        "切替の前にソート駆動を走らせている"
    );
    let switched = labeled(&d, "candidates");
    assert_eq!(switched.len(), 1, "切替は 1 回");

    let (fresh, fd) = store
        .run_query_debug(
            &c,
            table.id,
            &q,
            &QueryTuning {
                force: Some(ForcedStrategy::Filter),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(labeled(&fd, "sort_drive").is_empty());
    let forced = labeled(&fd, "candidates");
    assert_eq!(forced.len(), 1);
    // 切替先の文は、途中の位置や途中結果を引き継がず、要求の位置から引く文そのもの
    // （違いは切替の判断で付く候補の上限 `LIMIT $n` だけ）。
    assert_eq!(shape(&switched[0].sql, true), shape(&forced[0].sql, false));
    // 候補の上限（cap）だけは切替の判断で決まるので、それ以外のバインドが同じことを確かめる。
    let strip_i64 = |s: &DebugStatement| -> Vec<DebugParam> {
        s.params
            .iter()
            .filter(|p| !matches!(p, DebugParam::I64(_)))
            .cloned()
            .collect()
    };
    assert_eq!(strip_i64(switched[0]), strip_i64(forced[0]));
    assert_eq!(ids(&r), ids(&fresh));
    assert_eq!(ids(&r).len(), 3, "承認待ちの 3 件");
}

/// 計画時の件数探索は 1 計画 3 本まで・各探索は上限 1,000 で打ち切る。
#[tokio::test]
async fn probes_are_capped_at_1000_and_three_per_plan() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &alice, "probes", 5_000, true, false).await;
    // 探索したい述語は 5 本: Q のフィルタ駆動 1・OR 駆動の各項 2・行述語の選言 2（担当者・作成者）。
    let q = DataQuery {
        filter: Some(Condition::All(vec![
            Condition::cmp("status", CondOp::Eq, Some(json!(DONE))),
            Condition::Any(vec![
                Condition::cmp("assignee", CondOp::Eq, Some(json!("u1"))),
                Condition::cmp("amount", CondOp::Eq, Some(json!(77))),
            ]),
        ])),
        sort: vec![SortKey {
            field: "amount".into(),
            descending: false,
        }],
        limit: Some(10),
        ..Default::default()
    };
    let u7 = ctx(&tenant, "u7");
    let (_, d) = store
        .run_query_debug(&u7, table.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert!(!d.stats.planned_with_stats, "O1 の経路: {:?}", d.stats);
    let probes = labeled(&d, "probe");
    assert_eq!(d.stats.probes, 3, "1 計画 3 本まで: {:?}", d.stats);
    assert_eq!(probes.len(), 3);
    for p in &probes {
        assert!(
            p.params.contains(&DebugParam::I64(1_000)),
            "上限 1,000 で打ち切る: {:?}",
            p.params
        );
    }
    // 完了（約 4,500 件）の探索は 1,000 で止まる（それ以上数えない）。
    let mut counts = Vec::new();
    for p in &probes {
        let text = format!("SELECT to_jsonb(t.n) FROM ({}) AS t(n)", p.sql);
        let mut q = sqlx::query_scalar::<_, Value>(&text);
        for b in &p.params {
            q = bind_param(q, b);
        }
        let v: Value = q.fetch_one(&pool).await.unwrap();
        counts.push(v.as_i64().unwrap());
    }
    assert!(counts.iter().all(|n| *n <= 1_000), "{counts:?}");
    assert!(
        counts.contains(&1_000),
        "完了の探索は上限で打ち切られる: {counts:?}"
    );
}

/// 行述語の選言の件数探索（材料由来）はリクエストを跨いでキャッシュしない。Q の述語の探索は
/// 全員共通なのでキャッシュする（2 回目は探索しない）。
#[tokio::test]
async fn policy_driver_probes_are_not_cached_across_requests() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &alice, "policy_probe", 3_000, true, false).await;
    let q = DataQuery {
        sort: vec![SortKey {
            field: "amount".into(),
            descending: false,
        }],
        limit: Some(5),
        ..Default::default()
    };
    let u7 = ctx(&tenant, "u7");
    for round in 0..3 {
        let (_, d) = store
            .run_query_debug(&u7, table.id, &q, &QueryTuning::default())
            .await
            .unwrap();
        assert_eq!(
            d.stats.probes, 2,
            "{round} 回目も担当者・作成者の 2 本を探索し直す: {:?}",
            d.stats
        );
    }

    // 対照: 行述語の無いテーブルでの Q の述語の探索は 2 回目からキャッシュに当たる。
    let plain = seed(&pool, &store, &alice, "plain_probe", 3_000, false, false).await;
    let q = DataQuery {
        filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!(PENDING)))),
        ..q
    };
    let (_, d1) = store
        .run_query_debug(&alice, plain.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    let (_, d2) = store
        .run_query_debug(&alice, plain.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert_eq!(d1.stats.probes, 1, "{:?}", d1.stats);
    assert_eq!(d2.stats.probes, 0, "Q の述語はキャッシュ: {:?}", d2.stats);
}
