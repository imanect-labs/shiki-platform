//! 性能の計測（Task 15.3 受け入れ条件・query-optimizer.md §1.2・§1.3・§9.3 の形）。
//!
//! 100 万行のテーブル（＋プール統計を汚す別テナントの「承認待ち」20 万行）で、
//! - フィルタなしの初期グリッド 1 ページ（100 行）と深いページの p50 / p95 / p99（目標 p95 < 100ms）
//! - §1.2 の 4 ケース（選択的フィルタ／非選択的フィルタ／見える行 0.01% の利用者／相関で外れる）を
//!   自前プランナ（O1）と **Postgres 任せの単発 SQL**（素朴な `EXISTS`）で比べた p50 / p95 / p99
//! - 別の列どうしの OR（各項が数百件）で OR 駆動とソート駆動を比べる
//!
//! を測って表で出す。重いので `DATA_BENCH=1` のときだけ走る（CI の既定では走らない）。
//! 数値はローカルの 1 回の計測で、環境によって変わる（目標判定は p95 だけ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]

mod common;

use std::time::{Duration, Instant};

use authz::AuthContext;
use common::bulk::{seed, DONE, PENDING, TODO};
use common::{ctx, keys_of, setup, store_with, unique_tenant};
use data::{CondOp, Condition, DataQuery, DataStore, ForcedStrategy, QueryTuning, SortKey};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const N: i64 = 1_000_000;
const RUNS: usize = 40;

fn pct(mut v: Vec<Duration>) -> (f64, f64, f64) {
    v.sort();
    let at = |p: f64| {
        let i = ((v.len() as f64) * p).ceil() as usize;
        v[i.clamp(1, v.len()) - 1].as_secs_f64() * 1000.0
    };
    (at(0.50), at(0.95), at(0.99))
}

async fn time_store(
    store: &DataStore,
    c: &AuthContext,
    table: Uuid,
    q: &DataQuery,
    t: &QueryTuning,
) -> ((f64, f64, f64), usize, u64) {
    let mut out = Vec::new();
    let mut n = 0;
    let mut scanned = 0;
    for i in 0..RUNS + 3 {
        let s = Instant::now();
        let (r, d) = store.run_query_debug(c, table, q, t).await.unwrap();
        let e = s.elapsed();
        if i >= 3 {
            out.push(e);
        }
        n = r.items.map_or(0, |v| v.len());
        scanned = d.stats.scanned;
    }
    (pct(out), n, scanned)
}

/// Postgres 任せの単発 SQL（駆動と検査の選択をプランナに任せる・素朴な EXISTS・本体も同じ文で取る）。
async fn time_naive(
    pool: &PgPool,
    sql: &str,
    binds: &[serde_json::Value],
) -> ((f64, f64, f64), usize) {
    let mut out = Vec::new();
    let mut n = 0;
    for i in 0..RUNS + 3 {
        let mut q = sqlx::query(sql);
        for b in binds {
            q = match b {
                serde_json::Value::Number(x) if x.is_i64() => {
                    let v = x.as_i64().unwrap();
                    if v < 40_000 {
                        q.bind(v as i16)
                    } else {
                        q.bind(v)
                    }
                }
                serde_json::Value::String(s) => q.bind(s.clone()),
                other => panic!("{other}"),
            };
        }
        let s = Instant::now();
        let rows = q.fetch_all(pool).await.unwrap();
        let e = s.elapsed();
        if i >= 3 {
            out.push(e);
        }
        n = rows.len();
    }
    (pct(out), n)
}

fn row(name: &str, ours: (f64, f64, f64), naive: Option<(f64, f64, f64)>, extra: &str) {
    let n = naive.map_or_else(
        || "—".to_string(),
        |(a, b, c)| format!("{a:.1} / {b:.1} / {c:.1}"),
    );
    eprintln!(
        "| {name} | {:.1} / {:.1} / {:.1} | {n} | {extra} |",
        ours.0, ours.1, ours.2
    );
}

#[tokio::test]
async fn bench_one_million_rows() {
    if std::env::var("DATA_BENCH").as_deref() != Ok("1") {
        eprintln!("DATA_BENCH=1 でないためスキップ");
        return;
    }
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t0 = Instant::now();
    let noise = ctx(&unique_tenant(), "alice");
    seed(&pool, &store, &noise, "noise", 200_000, false, true).await;
    // 1 テーブルで全ケースを測る（行述語 = 担当者本人 or 作成者。alice は作成者なので全行が見える）。
    let table = seed(&pool, &store, &c, "bench", N, true, false).await;
    let ptable = table.clone();
    eprintln!("seed: {:.1}s", t0.elapsed().as_secs_f64());
    let (ti, tb) = keys_of(&pool, &tenant, table.id).await;
    let (pti, ptb) = keys_of(&pool, &tenant, ptable.id).await;
    let fid = |t: &data::DataTable, k: &str| i64::from(t.schema.field(k).unwrap().id);
    let auto = QueryTuning::default();
    let key = |f: &str, d: bool| SortKey {
        field: f.into(),
        descending: d,
    };
    let eq = |f: &str, v: &str| Condition::cmp(f.to_string(), CondOp::Eq, Some(json!(v)));

    eprintln!("| ケース | O1 p50/p95/p99 (ms) | Postgres 任せ p50/p95/p99 (ms) | 備考 |");
    eprintln!("|---|---|---|---|");

    // 初期グリッド（フィルタなし 100 行）。
    let grid = DataQuery {
        limit: Some(100),
        ..Default::default()
    };
    let (g, n, sc) = time_store(&store, &c, table.id, &grid, &auto).await;
    let naive_grid =
        "SELECT r.id, r.data FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 \
                      ORDER BY r.updated_at DESC, r.record_int DESC LIMIT 100";
    let (ng, _) = time_naive(&pool, naive_grid, &[json!(ti), json!(tb)]).await;
    row(
        "初期グリッド 100 行（フィルタなし）",
        g,
        Some(ng),
        &format!("{n} 行・走査 {sc}"),
    );
    // 内訳（文ごとの計画時間・実行時間）。
    let (_, dbg) = store
        .run_query_debug(&c, table.id, &grid, &auto)
        .await
        .unwrap();
    for st in &dbg.statements {
        let plan = common::explain(&pool, st, true).await;
        eprintln!(
            "  {}: planning {} ms / execution {} ms",
            st.label, plan[0]["Planning Time"], plan[0]["Execution Time"]
        );
    }
    let s = Instant::now();
    for _ in 0..10 {
        store.get_table(&c, table.id, None).await.unwrap();
    }
    eprintln!("  get_table x10: {:?}", s.elapsed());
    assert!(g.1 < 100.0, "初期グリッドの p95 < 100ms: {g:?}");

    // 深いページ（50 ページ目以降も同じコスト）。
    let mut q = grid.clone();
    let mut deep = Vec::new();
    for page in 0..60 {
        let s = Instant::now();
        let r = store.run_query(&c, table.id, &q, None).await.unwrap();
        if page >= 10 {
            deep.push(s.elapsed());
        }
        q.cursor = r.next_cursor;
    }
    let d = pct(deep);
    row("深いページ（11〜60 ページ目）", d, None, "keyset");
    assert!(d.1 < 100.0, "深いページの p95 < 100ms: {d:?}");

    // 金額順のグリッド（任意列・値なし 10%）。
    let by_amount = DataQuery {
        sort: vec![key("amount", true)],
        limit: Some(100),
        ..Default::default()
    };
    let (a, n, sc) = time_store(&store, &c, table.id, &by_amount, &auto).await;
    row(
        "任意列ソート 100 行（金額降順）",
        a,
        None,
        &format!("{n} 行・走査 {sc}"),
    );
    assert!(a.1 < 100.0, "{a:?}");

    let naive_sort = |status_f: i64, amount_f: i64| {
        let _ = (status_f, amount_f);
        "SELECT r.id, r.data FROM data_record r JOIN ( \
           SELECT i.record_int, i.val FROM data_index_num i \
           WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = $3 \
             AND EXISTS (SELECT 1 FROM data_index_text f WHERE f.tenant_int = $1 AND f.table_int = $2 \
                         AND f.field_id = $4 AND f.val = $5 AND f.record_int = i.record_int) \
           ORDER BY i.val DESC, i.record_int DESC LIMIT 20) p \
         ON r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = p.record_int ORDER BY p.val DESC"
    };
    // ① 承認待ち（0.03%）を金額順 20 件。
    let q1 = DataQuery {
        filter: Some(eq("status", PENDING)),
        sort: vec![key("amount", true)],
        limit: Some(20),
        ..Default::default()
    };
    let (o, n, sc) = time_store(&store, &c, table.id, &q1, &auto).await;
    let (nv, nn) = time_naive(
        &pool,
        naive_sort(0, 0),
        &[
            json!(ti),
            json!(tb),
            json!(fid(&table, "amount")),
            json!(fid(&table, "status")),
            json!(PENDING),
        ],
    )
    .await;
    row(
        "① 承認待ち 0.03% を金額順 20 件",
        o,
        Some(nv),
        &format!("{n}/{nn} 行・走査 {sc}"),
    );

    // ② 完了（≈95%）を金額順 20 件。
    let q2 = DataQuery {
        filter: Some(eq("status", DONE)),
        ..q1.clone()
    };
    let (o, n, sc) = time_store(&store, &c, table.id, &q2, &auto).await;
    let (nv, _) = time_naive(
        &pool,
        naive_sort(0, 0),
        &[
            json!(ti),
            json!(tb),
            json!(fid(&table, "amount")),
            json!(fid(&table, "status")),
            json!(DONE),
        ],
    )
    .await;
    row(
        "② 完了 ≈95% を金額順 20 件",
        o,
        Some(nv),
        &format!("{n} 行・走査 {sc}"),
    );
    let fq2 = QueryTuning {
        force: Some(ForcedStrategy::Filter),
        ..Default::default()
    };
    let (f, _, _) = time_store(&store, &c, table.id, &q2, &fq2).await;
    row(
        "② をフィルタ駆動に決め打ち",
        f,
        None,
        "決め打ちが破綻する側",
    );

    // ③ 見える行が 0.01%（100 行）の利用者がフィルタなしで更新日順 20 件。
    let u5 = ctx(&tenant, "u5");
    let q3 = DataQuery {
        limit: Some(20),
        ..Default::default()
    };
    let (o, n, sc) = time_store(&store, &u5, ptable.id, &q3, &auto).await;
    let naive_policy = "SELECT r.id, r.data FROM data_record r JOIN ( \
           SELECT u.record_int, u.val FROM data_index_text u \
           WHERE u.tenant_int = $1 AND u.table_int = $2 AND u.field_id = $3 \
             AND EXISTS (SELECT 1 FROM data_index_text p WHERE p.tenant_int = $1 AND p.table_int = $2 \
                         AND p.field_id = $4 AND p.val = $5 AND p.record_int = u.record_int) \
           ORDER BY u.val DESC, u.record_int DESC LIMIT 20) p \
         ON r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = p.record_int ORDER BY p.val DESC";
    let (nv, _) = time_naive(
        &pool,
        naive_policy,
        &[
            json!(pti),
            json!(ptb),
            json!(2),
            json!(fid(&ptable, "assignee")),
            json!("u5"),
        ],
    )
    .await;
    row(
        "③ 見える行 0.01% の利用者・更新日順 20 件",
        o,
        Some(nv),
        &format!("{n} 行・走査 {sc}"),
    );
    let sq3 = QueryTuning {
        force: Some(ForcedStrategy::Sort),
        ..Default::default()
    };
    let (s, _, sc) = time_store(&store, &u5, ptable.id, &q3, &sq3).await;
    row("③ をソート駆動に決め打ち", s, None, &format!("走査 {sc}"));

    // ④ 未着手（5%・新しい行に偏る）を作成日の古い順 20 件。
    let q4 = DataQuery {
        filter: Some(eq("status", TODO)),
        sort: vec![key("_created_at", false)],
        limit: Some(20),
        ..Default::default()
    };
    let (o, n, sc) = time_store(&store, &c, table.id, &q4, &auto).await;
    let naive_corr = "SELECT r.id, r.data FROM data_record r JOIN ( \
           SELECT u.record_int, u.val FROM data_index_text u \
           WHERE u.tenant_int = $1 AND u.table_int = $2 AND u.field_id = $3 \
             AND EXISTS (SELECT 1 FROM data_index_text f WHERE f.tenant_int = $1 AND f.table_int = $2 \
                         AND f.field_id = $4 AND f.val = $5 AND f.record_int = u.record_int) \
           ORDER BY u.val ASC, u.record_int ASC LIMIT 20) p \
         ON r.tenant_int = $1 AND r.table_int = $2 AND r.record_int = p.record_int ORDER BY p.val";
    let (nv, _) = time_naive(
        &pool,
        naive_corr,
        &[
            json!(ti),
            json!(tb),
            json!(1),
            json!(fid(&table, "status")),
            json!(TODO),
        ],
    )
    .await;
    row(
        "④ 未着手 5%（新しい行に偏る）を作成日の古い順",
        o,
        Some(nv),
        &format!("{n} 行・走査 {sc}"),
    );

    // OR 駆動 vs ソート駆動（別の列どうしの OR・各項が数百件）。
    let qor = DataQuery {
        filter: Some(Condition::Any(vec![
            eq("status", PENDING),
            eq("assignee", "u7"),
        ])),
        sort: vec![key("_created_at", false)],
        limit: Some(20),
        ..Default::default()
    };
    let (_, d) = store
        .run_query_debug(&c, table.id, &qor, &auto)
        .await
        .unwrap();
    assert_eq!(d.stats.strategy, "union", "{:?}", d.stats);
    let (u, n, sc) = time_store(&store, &c, table.id, &qor, &auto).await;
    let (s, _, ssc) = time_store(&store, &c, table.id, &qor, &sq3).await;
    row(
        "OR 駆動（承認待ち OR 担当=u7）",
        u,
        None,
        &format!("{n} 行・走査 {sc}"),
    );
    row(
        "同じクエリをソート駆動に決め打ち",
        s,
        None,
        &format!("走査 {ssc}"),
    );
    assert!(u.1 < s.1, "OR 駆動がソート駆動より速い: {u:?} vs {s:?}");
}
