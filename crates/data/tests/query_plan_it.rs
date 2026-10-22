//! 計画の形の回帰（EXPLAIN・PIT-69・query-optimizer.md §9.1）。
//!
//! 代表パターン（選択的フィルタ×非選択的ソート／その逆／多条件 AND／OR 木／keyset 継続／任意列ソート
//! （区間 2）／第 1 キーが偏った複数ソート／認可駆動）について、オプティマイザが選んだ戦略と、実行した文を
//! 同じバインドで `EXPLAIN ANALYZE` し直した計画の形をアサートする:
//! - 駆動索引（どの索引表の値索引／主キーから入るか）と実測行数（予算・候補数で頭打ち）
//! - 検査が行ごとの **SubPlan**（点検索）であること、ハッシュ結合・ハッシュ化 SubPlan が無いこと
//! - 個別プラン（`force_custom_plan`）で**計画時に 1 パーティションへ枝刈り**されていること
//! - 汎用プランの形: `EXPLAIN (GENERIC_PLAN)` と `force_generic_plan` 下の `EXPLAIN ANALYZE EXECUTE`
//!   （実行時の枝刈り `Subplans Removed`）
//!
//! Postgres の版を上げるときはこのテストで `OFFSET 0` の振る舞い（半結合化の抑止）を確かめ直す。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

mod common;

use std::collections::BTreeSet;

use common::bulk::{seed, DONE, PENDING, TODO};
use common::{count_nodes, ctx, explain, relations_in_plan, setup, store_with, unique_tenant};
use data::{
    CondOp, Condition, DataQuery, DebugParam, DebugStatement, ForcedStrategy, QueryTuning, SortKey,
};
use serde_json::{json, Value};
use sqlx::PgPool;

const N: i64 = 20_000;

fn main_stmt<'a>(stmts: &'a [DebugStatement], label: &str) -> &'a DebugStatement {
    stmts
        .iter()
        .rev()
        .find(|s| s.label == label)
        .unwrap_or_else(|| {
            panic!(
                "{label} の文が無い: {:?}",
                stmts.iter().map(|s| s.label).collect::<Vec<_>>()
            )
        })
}

/// 計画時に 1 パーティションへ枝刈りされている（索引表ごとに 1 つのパーティションだけが現れる）。
fn assert_single_partition(plan: &Value) {
    for fam in [
        "data_index_text",
        "data_index_num",
        "data_index_search",
        "data_record",
    ] {
        let rels: BTreeSet<String> = relations_in_plan(plan, fam)
            .into_iter()
            .filter(|r| r.starts_with(&format!("{fam}_p")))
            .collect();
        assert!(rels.len() <= 1, "{fam} が複数パーティション: {rels:?}");
    }
    assert_eq!(
        runtime_pruned(plan),
        0,
        "個別プランは計画時に枝刈りされる（実行時の枝刈りではない）"
    );
}

/// 実行時に枝刈りしたパーティション数の合計（`Subplans Removed`）。
fn runtime_pruned(plan: &Value) -> i64 {
    match plan {
        Value::Object(m) => {
            m.get("Subplans Removed")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + m.values().map(runtime_pruned).sum::<i64>()
        }
        Value::Array(a) => a.iter().map(runtime_pruned).sum(),
        _ => 0,
    }
}

/// 検査が行ごとの点検索（SubPlan）で、ハッシュ結合・ハッシュ化 SubPlan が無い。
fn assert_point_probes(plan: &Value, min_subplans: usize) {
    let text = plan.to_string();
    let subplans = text.matches("\"Parent Relationship\":\"SubPlan\"").count();
    assert!(subplans >= min_subplans, "SubPlan が {subplans} 個: {plan}");
    assert_eq!(
        count_nodes(plan, "Hash Join"),
        0,
        "ハッシュ結合がある: {plan}"
    );
    assert!(
        !text.contains("hashed SubPlan"),
        "ハッシュ化 SubPlan がある: {plan}"
    );
}

/// 計画中の索引走査（対象表の接頭辞 → 実測行数の合計）。
fn index_scans(plan: &Value, rel_prefix: &str) -> Vec<(String, String, i64)> {
    let mut out = Vec::new();
    fn walk(v: &Value, p: &str, out: &mut Vec<(String, String, i64)>) {
        match v {
            Value::Object(m) => {
                let nt = m.get("Node Type").and_then(Value::as_str).unwrap_or("");
                if nt.contains("Index") {
                    if let Some(rel) = m.get("Relation Name").and_then(Value::as_str) {
                        if rel.starts_with(p) {
                            out.push((
                                nt.to_string(),
                                m.get("Index Name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                m.get("Actual Rows").and_then(Value::as_i64).unwrap_or(-1),
                            ));
                        }
                    }
                }
                m.values().for_each(|x| walk(x, p, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, p, out)),
            _ => {}
        }
    }
    walk(plan, rel_prefix, &mut out);
    out
}

/// SQL リテラル（PREPARE/EXECUTE 用・テスト専用）。
fn literal(p: &DebugParam) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let arr = |v: Vec<String>, ty: &str| format!("ARRAY[{}]::{ty}[]", v.join(","));
    match p {
        DebugParam::Text(s) => format!("{}::text", q(s)),
        DebugParam::TextArray(v) => {
            if v.is_empty() {
                "'{}'::text[]".into()
            } else {
                arr(v.iter().map(|s| q(s)).collect(), "text")
            }
        }
        DebugParam::F64(f) => format!("{f}::float8"),
        DebugParam::UuidArray(v) => {
            format!(
                "'{{{}}}'::uuid[]",
                v.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        DebugParam::I16(n) => format!("{n}::int2"),
        DebugParam::I32(n) => format!("{n}::int4"),
        DebugParam::I64(n) => format!("{n}::int8"),
        DebugParam::I64Array(v) => format!(
            "'{{{}}}'::int8[]",
            v.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ),
        DebugParam::Numeric(s) => format!("{}::numeric", q(s)),
        DebugParam::NumericArray(v) => arr(v.iter().map(|s| q(s)).collect(), "numeric"),
    }
}

/// 汎用プランの形: `EXPLAIN (GENERIC_PLAN)` と、`force_generic_plan` 下の `EXPLAIN ANALYZE EXECUTE`
/// （実行時に 1 パーティションへ枝刈りされること）。
async fn assert_generic_plan(pool: &PgPool, st: &DebugStatement) {
    let mut conn = pool.acquire().await.unwrap();
    // パラメータを束縛しない計画は単純問い合わせプロトコルで投げる（拡張プロトコルは $n に値を要求する）。
    let sql = format!("EXPLAIN (GENERIC_PLAN, FORMAT JSON) {}", st.sql);
    let row = sqlx::raw_sql(&sql).fetch_one(&mut *conn).await.unwrap();
    let plan: Value = sqlx::Row::try_get(&row, 0).unwrap();
    assert_point_probes(&plan, 1);
    sqlx::query("SET plan_cache_mode = force_generic_plan")
        .execute(&mut *conn)
        .await
        .unwrap();
    let name = format!("p{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(&format!("PREPARE {name} AS {}", st.sql))
        .execute(&mut *conn)
        .await
        .unwrap();
    let args: Vec<String> = st.params.iter().map(literal).collect();
    let sql = format!(
        "EXPLAIN (ANALYZE, TIMING OFF, FORMAT JSON) EXECUTE {name}({})",
        args.join(", ")
    );
    let row = sqlx::raw_sql(&sql).fetch_one(&mut *conn).await.unwrap();
    let plan: Value = sqlx::Row::try_get(&row, 0).unwrap();
    assert!(
        runtime_pruned(&plan) > 0,
        "汎用プランでも実行時に枝刈りされる: {plan}"
    );
    assert_point_probes(&plan, 1);
    sqlx::query(&format!("DEALLOCATE {name}"))
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("RESET plan_cache_mode")
        .execute(&mut *conn)
        .await
        .unwrap();
}

fn sort_key(f: &str, desc: bool) -> SortKey {
    SortKey {
        field: f.into(),
        descending: desc,
    }
}

fn eq(f: &str, v: Value) -> Condition {
    Condition::cmp(f.to_string(), CondOp::Eq, Some(v))
}

#[tokio::test]
async fn plan_shapes_are_fixed_per_strategy() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    // プール統計を汚す雑音（別テナントに承認待ちを大量に）。
    let noise = ctx(&unique_tenant(), "alice");
    seed(&pool, &store, &noise, "noise", 30_000, false, true).await;
    let table = seed(&pool, &store, &c, "plan", N, false, false).await;
    let auto = QueryTuning::default();
    let run = |q: DataQuery, t: QueryTuning| {
        let store = store.clone();
        let c = c.clone();
        async move { store.run_query_debug(&c, table.id, &q, &t).await.unwrap() }
    };

    // ① 選択的フィルタ（承認待ち ≈ 0.03%）× 非選択的ソート（金額）→ フィルタ駆動。
    let q1 = DataQuery {
        filter: Some(eq("status", json!(PENDING))),
        sort: vec![sort_key("amount", true)],
        limit: Some(20),
        ..Default::default()
    };
    let (r, d) = run(q1.clone(), auto.clone()).await;
    assert_eq!(d.stats.strategy, "filter", "{:?}", d.stats);
    let n_pending = r.items.unwrap().len();
    assert!(n_pending > 0);
    let st = main_stmt(&d.statements, "candidates");
    let plan = explain(&pool, st, true).await;
    let scans = index_scans(&plan, "data_index_text_p");
    assert!(
        scans
            .iter()
            .any(|(_, idx, rows)| idx.contains("val") && *rows >= 1),
        "候補は status の値索引から入る: {scans:?}"
    );
    assert!(plan.to_string().contains("\"CTE Name\":\"cand\""));
    assert_point_probes(&plan, 1);
    assert_single_partition(&plan);
    assert!(d.stats.scanned < 100, "候補件数だけを読む: {:?}", d.stats);
    assert_generic_plan(&pool, st).await;
    // 本体の取得は `(tenant_int, table_int, record_int)` の一意索引の点検索（seq scan にしない）。
    let fetch = explain(&pool, main_stmt(&d.statements, "fetch"), true).await;
    let scans = index_scans(&fetch, "data_record_p");
    assert!(
        scans.iter().any(|(_, idx, rows)| idx.contains("record_int")
            && usize::try_from(*rows).ok() == Some(n_pending)),
        "本体は一意索引で引く: {fetch}"
    );
    assert_eq!(count_nodes(&fetch, "Seq Scan"), 0);
    assert_single_partition(&fetch);

    // ② 非選択的フィルタ（完了 ≈ 90%）× ソート → ソート駆動（予算付き・早期終了）。
    let q2 = DataQuery {
        filter: Some(eq("status", json!(DONE))),
        sort: vec![sort_key("amount", true)],
        limit: Some(20),
        ..Default::default()
    };
    let (r2, d) = run(q2.clone(), auto.clone()).await;
    assert_eq!(d.stats.strategy, "sort", "{:?}", d.stats);
    let st = main_stmt(&d.statements, "sort_drive");
    let plan = explain(&pool, st, true).await;
    let scans = index_scans(&plan, "data_index_num_p");
    let drive = scans
        .iter()
        .find(|(_, idx, _)| idx.contains("val"))
        .unwrap_or_else(|| panic!("金額の値索引から入る: {scans:?}"));
    assert!(drive.2 <= 60, "早期終了（読んだ行数）: {drive:?}");
    assert_point_probes(&plan, 1);
    assert_single_partition(&plan);
    assert_generic_plan(&pool, st).await;

    // ⑤ keyset 継続（深いページでも同じ形・同じ件数）。
    let mut q5 = q2.clone();
    q5.cursor = r2.next_cursor.clone();
    for _ in 0..30 {
        let (r, _) = run(q5.clone(), auto.clone()).await;
        q5.cursor = r.next_cursor;
    }
    let (_, d) = run(q5.clone(), auto.clone()).await;
    let st = main_stmt(&d.statements, "sort_drive");
    assert!(st.sql.contains("(i.val, i.record_int) <"), "{}", st.sql);
    let plan = explain(&pool, st, true).await;
    let scans = index_scans(&plan, "data_index_num_p");
    let drive = scans
        .iter()
        .find(|(_, idx, _)| idx.contains("val"))
        .unwrap();
    assert!(drive.2 <= 60, "深いページでも走査は一定: {drive:?}");
    assert_single_partition(&plan);

    // ③ 多条件 AND（3 項）→ 検査が 2 個以上の SubPlan。
    let q3 = DataQuery {
        filter: Some(Condition::All(vec![
            eq("status", json!(DONE)),
            Condition::cmp("amount", CondOp::Gte, Some(json!(100_000))),
            Condition::Not(Box::new(eq("assignee", json!("u1")))),
        ])),
        sort: vec![sort_key("_created_at", true)],
        limit: Some(20),
        ..Default::default()
    };
    let (_, d) = run(q3, auto.clone()).await;
    let label = if d.stats.strategy == "sort" {
        "sort_drive"
    } else {
        "candidates"
    };
    let plan = explain(&pool, main_stmt(&d.statements, label), true).await;
    assert_point_probes(&plan, 2);
    assert_single_partition(&plan);

    // ④ OR 木（別の列どうし・各項が絞れている）→ OR 駆動。
    let q4 = DataQuery {
        filter: Some(Condition::Any(vec![
            eq("status", json!(PENDING)),
            eq("assignee", json!("u7")),
        ])),
        sort: vec![sort_key("_created_at", false)],
        limit: Some(20),
        ..Default::default()
    };
    let (_, d) = run(q4.clone(), auto.clone()).await;
    assert_eq!(d.stats.strategy, "union", "{:?}", d.stats);
    let st = main_stmt(&d.statements, "candidates");
    let plan = explain(&pool, st, true).await;
    let scans = index_scans(&plan, "data_index_text_p");
    assert!(
        scans
            .iter()
            .filter(|(_, idx, _)| idx.contains("val"))
            .count()
            >= 2,
        "各項の値索引から入る: {scans:?}"
    );
    assert_point_probes(&plan, 1);
    assert_single_partition(&plan);
    assert_generic_plan(&pool, st).await;

    // ⑥ 任意列（値なし 10%）のソートで区間 2（擬似列 created_at の主キー順）まで進む。
    let q6 = DataQuery {
        filter: Some(eq("status", json!(TODO))),
        sort: vec![sort_key("amount", false)],
        limit: Some(200),
        ..Default::default()
    };
    let mut q = q6.clone();
    let mut seen_missing = None;
    let mut total = 0;
    for _ in 0..20 {
        let (r, d) = run(
            q.clone(),
            QueryTuning {
                force: Some(ForcedStrategy::Sort),
                ..Default::default()
            },
        )
        .await;
        total += r.items.unwrap().len();
        if let Some(st) = d.statements.iter().find(|s| s.label == "missing_drive") {
            seen_missing = Some(st.clone());
        }
        match r.next_cursor {
            Some(cur) => q.cursor = Some(cur),
            None => break,
        }
    }
    let st = seen_missing.expect("区間 2 の走査が起きる");
    let todo_total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM data_record WHERE tenant_id = $1 AND table_id = $2 AND data ->> $3 = $4",
    )
    .bind(&tenant)
    .bind(table.id)
    .bind(format!("f{}", table.schema.field("status").unwrap().id))
    .bind(TODO)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(total as i64, todo_total, "値なしの行も欠落しない");
    let plan = explain(&pool, &st, true).await;
    let scans = index_scans(&plan, "data_index_text_p");
    assert!(
        scans.iter().any(|(_, idx, _)| idx.contains("pkey")),
        "区間 2 は擬似列の主キー順: {scans:?}"
    );
    assert_point_probes(&plan, 2);
    assert_single_partition(&plan);

    // ⑦ 第 1 キーが偏った複数ソート（status は 90% が完了）: 窓は第 1 キーの値索引の範囲で、
    //    深いページでも結果は全順序どおり。
    let q7 = DataQuery {
        sort: vec![sort_key("status", false), sort_key("amount", true)],
        limit: Some(50),
        ..Default::default()
    };
    let (r, d) = run(
        q7.clone(),
        QueryTuning {
            force: Some(ForcedStrategy::Sort),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(r.items.unwrap().len(), 50);
    let st = main_stmt(&d.statements, "candidates");
    let plan = explain(&pool, st, true).await;
    // 第 1 キーの同値群（完了 ≈ 90%）は窓に丸ごと入る＝群の大きさに比例する最悪ケース（data-platform §5.3。
    // 複合索引の材料化は Task 15.12）。窓の読取は自テーブルの 1 パーティションに閉じる。
    assert!(
        st.sql.contains("i.val <=") || st.sql.contains("i.val >="),
        "窓は第 1 キーの値の範囲: {}",
        st.sql
    );
    assert_single_partition(&plan);
    eprintln!(
        "skewed multi-sort: scanned={} statements={}",
        d.stats.scanned, d.stats.statements
    );
}

#[tokio::test]
async fn policy_drive_plan_for_users_who_see_few_rows() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let owner = ctx(&tenant, "alice");
    let table = seed(&pool, &store, &owner, "policy-plan", N, true, false).await;
    // u5 だけが見える行（0.01%）: フィルタなし・更新日順でも認可駆動。
    let u5 = ctx(&tenant, "u5");
    let q = DataQuery {
        limit: Some(20),
        ..Default::default()
    };
    let (r, d) = store
        .run_query_debug(&u5, table.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert_eq!(d.stats.strategy, "policy", "{:?}", d.stats);
    assert_eq!(r.items.unwrap().len(), 2, "u5 の行は 20,000 行中 2 行");
    let st = main_stmt(&d.statements, "candidates");
    let plan = explain(&pool, st, true).await;
    // 認可駆動でも行述語の検査は残る（Guard を省かない・§7.1）。
    assert_point_probes(&plan, 1);
    assert_single_partition(&plan);
    assert_generic_plan(&pool, st).await;
}
