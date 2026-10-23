//! 自前オプティマイザ O2 / O3 の性能ベンチ（Task 15.12 受け入れ条件・query-optimizer.md §9.3）。
//!
//! 1. **顧客データを使わない**: 業務的な値を持つ「実テナントのテーブル」（顧客データの代わり）から
//!    統計収集で形（行数・型・種類数・頻度の形・値なし率・作成順との相関）を**値を含まない数値**として
//!    取り出し、その形だけからベンチのテーブルを合成する（値は記号）。形に元の値が無いことを確かめる。
//! 2. 偏り（Zipf）・相関・値なし・多値・多テナントの雑音（別テナントで人気の値を対象テーブルの希少な値に
//!    重ねる＝プール統計を汚す）を含むデータで、代表クエリごとに**後悔比**（自動選択の所要時間 ÷
//!    全戦略を強制した中で最短の所要時間）を測り、p95 が 2 倍以内であることを確かめる。
//! 3. 較正ベンチの前後で単価が置き換わり、予測コストと実測の相関（対数のピアソン）を記録する。
//! 4. 統計収集ジョブを回しながら、別テナントの読取の p95 が悪化しないことを確かめる。
//!
//! 重いので `DATA_BENCH=1` のときだけ走る（CI の既定では走らない）。数値はローカルの計測で環境で変わる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

mod common;

use std::time::{Duration, Instant};

use authz::AuthContext;
use common::synth::{generate, symbolic};
use common::{ctx, setup, store_with, unique_tenant};
use data::{
    ColumnShape, CondOp, Condition, DataQuery, DataStore, ForcedStrategy, QueryTuning, ShapeType,
    SortKey, TableShape,
};
use serde_json::json;
use uuid::Uuid;

const RUNS: usize = 5;

fn col(ty: ShapeType, distinct: u64, zipf_s: f64, null_rate: f64, corr: f64) -> ColumnShape {
    ColumnShape {
        ordinal: 0,
        ty,
        null_rate,
        distinct,
        top_freqs: Vec::new(),
        zipf_s,
        avg_elems: if ty == ShapeType::MultiCategory { 1.7 } else { 1.0 },
        truncated_rate: 0.0,
        creation_correlation: corr,
    }
}

/// 「実テナント」の値（顧客データの代わり）。ベンチの形にこれらが現れてはならない。
const STATUS_JA: [&str; 6] = ["完了", "承認待ち", "差戻し", "下書き", "未着手", "保留"];

fn customer_like(c: usize, r: usize) -> String {
    match c {
        0 => STATUS_JA[r % STATUS_JA.len()].to_string(),
        1 => format!("経費区分-{r:03}"),
        2 => format!("tanaka.{r}@customer.example.jp"),
        _ => format!("タグ{r}"),
    }
}

fn pct(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    let i = ((v.len() as f64) * p).ceil() as usize;
    v[i.clamp(1, v.len()) - 1]
}

async fn median_ms(
    store: &DataStore,
    c: &AuthContext,
    table: Uuid,
    q: &DataQuery,
    t: &QueryTuning,
) -> (f64, data::QueryExecStats) {
    let mut out = Vec::new();
    let mut last = data::QueryExecStats::default();
    for i in 0..=RUNS {
        let s = Instant::now();
        let (_, d) = store.run_query_debug(c, table, q, t).await.unwrap();
        if i > 0 {
            out.push(s.elapsed().as_secs_f64() * 1e3);
        }
        last = d.stats;
    }
    (pct(out, 0.5), last)
}

fn key(f: &str, desc: bool) -> SortKey {
    SortKey {
        field: f.into(),
        descending: desc,
    }
}

fn eq(f: &str, v: &str) -> Condition {
    Condition::cmp(f.to_string(), CondOp::Eq, Some(json!(v)))
}

const STRATEGIES: [(&str, ForcedStrategy); 7] = [
    ("sort", ForcedStrategy::Sort),
    ("filter", ForcedStrategy::Filter),
    ("union", ForcedStrategy::Union),
    ("policy", ForcedStrategy::Policy),
    ("intersect", ForcedStrategy::Intersect),
    ("batch", ForcedStrategy::Batch),
    ("safe", ForcedStrategy::Safe),
];

#[tokio::test]
async fn bench_regret_calibration_and_fairness() {
    if std::env::var("DATA_BENCH").as_deref() != Ok("1") {
        eprintln!("DATA_BENCH=1 でないためスキップ");
        return;
    }
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let t0 = Instant::now();

    // ① 実テナントのテーブル（業務的な値・行述語あり）から形を取り出す。
    let real_tenant = unique_tenant();
    let rc = ctx(&real_tenant, "alice");
    let hand = TableShape {
        n_rows: 30_000,
        columns: vec![
            col(ShapeType::Category, 6, 1.6, 0.0, 0.6),
            col(ShapeType::Category, 40, 1.0, 0.05, 0.0),
            col(ShapeType::Category, 800, 0.6, 0.2, 0.1),
            col(ShapeType::Number, 2_000, 0.2, 0.3, 0.4),
            col(ShapeType::MultiCategory, 8, 0.8, 0.4, 0.0),
        ],
        owner: Some(col(ShapeType::Category, 300, 1.2, 0.0, 0.0)),
        links: vec![],
    };
    let real = generate(&pool, &store, &rc, "経費", &hand, 30_000, true, 0xC0FFEE, &customer_like).await;
    store.analyze_table(&real_tenant, real.id).await.unwrap();
    let shape = store
        .extract_table_shape(&real_tenant, real.id)
        .await
        .unwrap()
        .expect("統計から形を取り出せる");
    let text = serde_json::to_string(&shape).unwrap();
    for v in STATUS_JA {
        assert!(!text.contains(v), "形に値が含まれる: {v}");
    }
    assert!(!text.contains("customer.example") && !text.contains("経費"), "{text}");
    for k in ["c0", "c1", "c2", "status"] {
        assert!(!text.contains(&format!("\"{k}\"")), "列の参照名を含めない: {text}");
    }
    eprintln!(
        "形（値なし）: 行数 {} / 列 {:?}",
        shape.n_rows,
        shape
            .columns
            .iter()
            .map(|c| format!(
                "{:?} 種類 {} zipf {:.2} 値なし {:.2} 相関 {:.2}",
                c.ty, c.distinct, c.zipf_s, c.null_rate, c.creation_correlation
            ))
            .collect::<Vec<_>>()
    );

    // ② 形から合成: 対象テナント（行述語なし 200k・作成者のみ 100k）と雑音テナント（人気の値を反転）。
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let t = generate(&pool, &store, &c, "bench", &shape, 200_000, false, 7, &symbolic).await;
    let p = generate(&pool, &store, &c, "bench_p", &shape, 100_000, true, 11, &symbolic).await;
    let d0 = shape.columns[0].distinct.max(1) as usize;
    let noise_namer = move |col: usize, r: usize| {
        if col == 0 {
            format!("c0v{}", d0 - 1 - r.min(d0 - 1))
        } else {
            symbolic(col, r)
        }
    };
    let noise = ctx(&unique_tenant(), "alice");
    generate(&pool, &store, &noise, "noise", &shape, 200_000, false, 13, &noise_namer).await;
    store.analyze_table(&tenant, t.id).await.unwrap();
    store.analyze_table(&tenant, p.id).await.unwrap();
    eprintln!("合成と統計収集: {:.1}s", t0.elapsed().as_secs_f64());

    let rare = format!("c0v{}", d0 - 1);
    let mid = format!("c0v{}", (d0 / 2).max(1));
    let top = "c0v0".to_string();
    let rare_owner = format!("o{}", shape.owner.as_ref().map_or(1, |o| o.distinct.saturating_sub(5)));
    let qs: Vec<(&str, Uuid, AuthContext, DataQuery)> = vec![
        ("①選択的フィルタ×非選択的ソート", t.id, c.clone(), DataQuery {
            filter: Some(eq("c0", &rare)),
            sort: vec![key("c3", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("②非選択的フィルタ×ソート", t.id, c.clone(), DataQuery {
            filter: Some(eq("c0", &top)),
            sort: vec![key("c3", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("③相関で外れる（新しい行に偏る値を古い順）", t.id, c.clone(), DataQuery {
            filter: Some(eq("c0", &mid)),
            sort: vec![key("_created_at", false)],
            limit: Some(50),
            ..Default::default()
        }),
        ("④多条件 AND", t.id, c.clone(), DataQuery {
            filter: Some(Condition::All(vec![
                eq("c0", &top),
                eq("c1", "c1v3"),
                Condition::cmp("c3", CondOp::Gte, Some(json!(1_000))),
            ])),
            sort: vec![key("_updated_at", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑤別の列どうしの OR", t.id, c.clone(), DataQuery {
            filter: Some(Condition::Any(vec![eq("c0", &rare), eq("c1", "c1v35")])),
            sort: vec![key("_created_at", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑥中程度の等値 2 つの交わり", t.id, c.clone(), DataQuery {
            filter: Some(Condition::All(vec![eq("c1", "c1v6"), eq("c2", "c2v40")])),
            sort: vec![key("c3", false)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑦範囲", t.id, c.clone(), DataQuery {
            filter: Some(Condition::All(vec![
                Condition::cmp("c3", CondOp::Gte, Some(json!(2_000))),
                Condition::cmp("c3", CondOp::Lt, Some(json!(3_000))),
            ])),
            sort: vec![key("_created_at", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑧値なしの区間（任意列ソートの末尾）", t.id, c.clone(), DataQuery {
            filter: Some(Condition::All(vec![
                Condition::cmp("c3", CondOp::IsNull, None),
                eq("c1", "c1v2"),
            ])),
            sort: vec![key("c3", false)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑨フィルタなしの初期グリッド", t.id, c.clone(), DataQuery {
            sort: vec![key("_updated_at", true)],
            limit: Some(100),
            ..Default::default()
        }),
        ("⑩多値の等値", t.id, c.clone(), DataQuery {
            filter: Some(eq("c4", "c4v6")),
            sort: vec![key("_created_at", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑪否定 ∧ 等値", t.id, c.clone(), DataQuery {
            filter: Some(Condition::All(vec![
                Condition::Not(Box::new(eq("c0", &top))),
                eq("c1", "c1v0"),
            ])),
            sort: vec![key("c3", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑫見える行が少ない利用者（認可駆動）", p.id, ctx(&tenant, &rare_owner), DataQuery {
            sort: vec![key("_created_at", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑬多くが見える利用者×希少な値", p.id, ctx(&tenant, "o0"), DataQuery {
            filter: Some(eq("c0", &rare)),
            sort: vec![key("c3", true)],
            limit: Some(50),
            ..Default::default()
        }),
        ("⑭見える行が少ない利用者×人気の値", p.id, ctx(&tenant, &rare_owner), DataQuery {
            filter: Some(eq("c0", &top)),
            sort: vec![key("c3", true)],
            limit: Some(50),
            ..Default::default()
        }),
    ];
    let auto = QueryTuning::default();

    // ③ 較正の前後: 予測コストと実測の相関（対数のピアソン）。
    for (_, tb, who, q) in &qs {
        store.run_query_debug(who, *tb, q, &auto).await.unwrap();
    }
    store.clear_optimizer_samples();
    for _ in 0..3 {
        for (_, tb, who, q) in &qs {
            store.run_query_debug(who, *tb, q, &auto).await.unwrap();
        }
    }
    let (corr_before, n_before) = store.optimizer_correlation();
    let cal = store.calibrate_costs().await.unwrap();
    store.clear_optimizer_samples();
    for _ in 0..3 {
        for (_, tb, who, q) in &qs {
            store.run_query_debug(who, *tb, q, &auto).await.unwrap();
        }
    }
    let (corr_after, n_after) = store.optimizer_correlation();
    eprintln!("較正: 前 {:?}", cal.before);
    eprintln!("較正: 後 {:?}", cal.after);
    eprintln!(
        "予測コストと実測の相関（対数）: 較正前 {corr_before:?}（{n_before} 件）→ 較正後 {corr_after:?}（{n_after} 件）"
    );
    assert_ne!(cal.after, cal.before);
    assert!(corr_after.is_some());

    // ④ 後悔比（自動 ÷ 全戦略の最短）。
    eprintln!("| クエリ | 自動（ms・初手→最終） | 最短の戦略（ms） | 後悔比 |");
    eprintln!("|---|---|---|---|");
    let mut regrets = Vec::new();
    for (name, tb, who, q) in &qs {
        let (a, sa) = median_ms(&store, who, *tb, q, &auto).await;
        let mut best = (a, "auto");
        for (sname, f) in STRATEGIES {
            let t = QueryTuning {
                force: Some(f),
                ..Default::default()
            };
            let (ms, _) = median_ms(&store, who, *tb, q, &t).await;
            if ms < best.0 {
                best = (ms, sname);
            }
        }
        let regret = a / best.0;
        regrets.push(regret);
        eprintln!(
            "| {name} | {a:.1}（{}→{}） | {:.1}（{}） | {regret:.2} |",
            sa.first_strategy, sa.strategy, best.0, best.1
        );
    }
    let p50 = pct(regrets.clone(), 0.5);
    let p95 = pct(regrets.clone(), 0.95);
    eprintln!("後悔比: p50 {p50:.2} / p95 {p95:.2} / 最大 {:.2}", pct(regrets, 1.0));
    assert!(p95 <= 2.0, "後悔比の p95 が 2 倍を超えた: {p95:.2}");

    // ⑤ 統計収集ジョブを回しながら、別テナントの読取 p95 が悪化しない。
    let other = unique_tenant();
    let oc = ctx(&other, "alice");
    let small = common::bulk::seed(&pool, &store, &oc, "other", 5_000, false, false).await;
    let oq = [
        DataQuery {
            filter: Some(eq("status", common::bulk::PENDING)),
            sort: vec![key("amount", true)],
            limit: Some(20),
            ..Default::default()
        },
        DataQuery {
            filter: Some(eq("status", common::bulk::DONE)),
            sort: vec![key("amount", true)],
            limit: Some(20),
            ..Default::default()
        },
    ];
    let measure = |n: usize| {
        let store = store.clone();
        let oc = oc.clone();
        let oq = oq.clone();
        async move {
            let mut v = Vec::with_capacity(n);
            for i in 0..n {
                let s = Instant::now();
                store
                    .run_query(&oc, small.id, &oq[i % 2], None)
                    .await
                    .unwrap();
                v.push(s.elapsed().as_secs_f64() * 1e3);
            }
            v
        }
    };
    measure(20).await;
    let base = measure(300).await;
    let bg = {
        let store = store.clone();
        let tenant = tenant.clone();
        let tid = t.id;
        tokio::spawn(async move {
            let mut n = 0;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(600) {
                store.analyze_table(&tenant, tid).await.unwrap();
                n += 1;
                if n >= 3 {
                    break;
                }
            }
            n
        })
    };
    let mut during = Vec::new();
    while !bg.is_finished() {
        during.extend(measure(20).await);
    }
    let analyzed = bg.await.unwrap();
    let (b95, d95) = (pct(base.clone(), 0.95), pct(during.clone(), 0.95));
    eprintln!(
        "別テナントの読取 p95: 平常 {b95:.2}ms → 統計収集中 {d95:.2}ms（{} 件・統計収集 {analyzed} 回）・p50 {:.2} → {:.2}",
        during.len(),
        pct(base, 0.5),
        pct(during, 0.5)
    );
    assert!(
        d95 <= (b95 * 1.5).max(b95 + 5.0),
        "統計収集で別テナントの p95 が悪化: {b95:.2} → {d95:.2}"
    );
    eprintln!("合計: {:.1}s", t0.elapsed().as_secs_f64());
}
