//! /data の SQL と chat の `data_query` を、素の PostgreSQL と突き合わせる生成テスト（#1004）。
//!
//! 仕組みは [`sql_diff`]。実 Postgres（`STORAGE_TEST_DATABASE_URL`）で実行する（無ければスキップ）。
//!
//! - `SQL_DIFF_SEED`: 種（既定は固定値。`random` で毎回変える）
//! - `SQL_DIFF_TABLES` / `SQL_DIFF_QUERIES`: 作る表の数・表ごとの問い合わせの数
//! - `SQL_DIFF_OUT`: 縮めた再現の保存先（既定 `target/sql-diff`）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

mod common;
mod sql_diff;

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::SeedableRng;
use sql_diff::exec::{check, check_loaded, load_shiki, variants, Verdict};
use sql_diff::model::Case;
use sql_diff::{gen, save, shrink::shrink};

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// 種（`random` なら毎回変える・読めない値は既定に戻さず止める）。最初に表示する。
fn seed() -> u64 {
    let seed = match std::env::var("SQL_DIFF_SEED").as_deref() {
        Ok("random") => rand::random(),
        Ok(s) => s
            .parse()
            .unwrap_or_else(|_| panic!("SQL_DIFF_SEED は整数か random: {s}")),
        Err(_) => 1004,
    };
    eprintln!("SQL_DIFF_SEED={seed}");
    seed
}

/// 探索の結果。
#[derive(Default)]
struct Tally {
    same: u64,
    skipped: BTreeMap<String, u64>,
    /// 縮めた再現（保存先・食い違い）。
    found: Vec<(String, String)>,
}

/// 種から表と問い合わせを作って比べる。食い違いは縮めて保存する（`stop_after` 件で打ち切る）。
async fn explore(
    pool: &sqlx::PgPool,
    seed: u64,
    tables: u64,
    queries: u64,
    stop_after: usize,
    mutate: Option<&dyn Fn(String) -> String>,
) -> Tally {
    let vs = variants(pool);
    let mut rng = StdRng::seed_from_u64(seed);
    let mut tally = Tally::default();
    for _ in 0..tables {
        let base = gen::case(&mut rng);
        let tenant = if mutate.is_none() {
            match load_shiki(&vs[0].store, &base).await {
                Ok((t, _)) => Some(t),
                Err(e) => panic!(
                    "表を作れない（SQL_DIFF_SEED={seed}）: {e}\n{}",
                    base.describe()
                ),
            }
        } else {
            None
        };
        for qi in 0..queries {
            let case = if qi == 0 {
                base.clone()
            } else {
                Case {
                    query: gen::query(&mut rng, &base),
                    ..base.clone()
                }
            };
            let v = match &tenant {
                Some(t) => check_loaded(pool, &vs, t, &case).await,
                None => check(pool, &vs, &case, mutate).await,
            };
            match v {
                Verdict::Same => tally.same += 1,
                Verdict::Skipped(why) => *tally.skipped.entry(why).or_default() += 1,
                d @ Verdict::Differ { .. } => {
                    let (min, dv) = shrink(pool, &vs, case, d, mutate).await;
                    let detail = format!("{dv:?}");
                    let path = save(&min, &detail);
                    eprintln!(
                        "食い違い（縮めた再現: {}）\n{detail}\n{}",
                        path.display(),
                        min.describe()
                    );
                    tally.found.push((path.display().to_string(), detail));
                    if tally.found.len() >= stop_after {
                        return tally;
                    }
                }
            }
        }
    }
    tally
}

#[tokio::test]
async fn shiki_sql_matches_postgres_on_random_cases() {
    let Some(pool) = common::setup().await else {
        return;
    };
    let seed = seed();
    let tables = env_num("SQL_DIFF_TABLES", 30);
    let queries = env_num("SQL_DIFF_QUERIES", 10);
    let t = explore(&pool, seed, tables, queries, 5, None).await;
    eprintln!(
        "SQL_DIFF_SEED={seed}: 一致 {} 件・許容リストで飛ばした {:?}・食い違い {} 件",
        t.same,
        t.skipped,
        t.found.len()
    );
    assert!(
        t.found.is_empty(),
        "Shiki と PostgreSQL の結果が食い違った（SQL_DIFF_SEED={seed} で再現・縮めた再現は保存先を参照）:\n{}",
        t.found
            .iter()
            .map(|(p, d)| format!("- {p}\n  {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // 許容リストで飛ばすばかりで比べていない、にならないこと。
    assert!(
        t.same * 2 > tables * queries,
        "比べられた件数が少なすぎる（一致 {} 件 / {} 件）",
        t.same,
        tables * queries
    );
}

/// 自己検査: 参照側の SQL をわざと壊す（`<` を `<=` にする）と、食い違いとして見つけ、小さく縮める。
/// これが落ちない（見つけない・縮まない）なら、上の比較も信用できない。
#[tokio::test]
async fn harness_finds_and_shrinks_a_planted_difference() {
    let Some(pool) = common::setup().await else {
        return;
    };
    let mutate = |s: String| s.replace(" < ", " <= ");
    // 最初の 1 件が見つかるまで（生成を変えても見つかる程度の件数・見つけたらそこで止まる）。
    let t = explore(&pool, 7, 200, 1, 1, Some(&mutate)).await;
    let (path, detail) = t.found.first().expect("壊した参照との食い違いを見つける");
    let body = std::fs::read_to_string(path).unwrap();
    let min: Case = serde_json::from_str(&body).unwrap();
    assert!(
        min.rows.len() <= 2,
        "行を 2 行以下まで縮める: {} 行\n{detail}\n{}",
        min.rows.len(),
        min.describe()
    );
    let sql = min.sql("t", sql_diff::model::Side::Shiki);
    assert!(
        sql.contains(" < ") && sql.len() < 160,
        "条件を `<` を含む小さな式まで縮める: {sql}"
    );
}

/// 保存済みの再現（直した食い違い）が、全構成で一致し続ける。
#[tokio::test]
async fn saved_repros_stay_fixed() {
    let Some(pool) = common::setup().await else {
        return;
    };
    let vs = variants(&pool);
    let saved = sql_diff::corpus();
    assert!(!saved.is_empty(), "保存済みの再現が読めない");
    for (name, s) in saved {
        let v = check(&pool, &vs, &s.case, None).await;
        let got = match &v {
            Verdict::Same => "same".to_string(),
            Verdict::Skipped(id) => id.clone(),
            Verdict::Differ { .. } => format!("{v:?}"),
        };
        assert_eq!(got, s.expect, "{name}（{}）\n{}", s.note, s.case.describe());
    }
}
