//! 内部整数キー（`record_int`）の計測（Phase 15 Task 15.1 受け入れ条件・結果は
//! docs/data/internal-keys-benchmark.md）。
//!
//! 1. 索引テーブルの大きさ: レコード ID を `uuid` で持つ試作表と `int8`（record_int）で持つ試作表に
//!    同じ行を入れ、ヒープ・主キー・値索引のサイズを比べる（data-platform §3.3 の `data_index_text`
//!    と同じ列構成。索引テーブル本体は Task 15.2 の担当なので一時表で試作する）。
//! 2. 集合演算の速さ: 「見える行の集合」「OR の和集合」「交差」を Rust 側で計算する場合に、
//!    uuid の集合（`HashSet` / 整列済み `Vec` のマージ）と Roaring（64 ビット版 `RoaringTreemap`）を比べる。
//!
//! 重いので `DATA_BENCH=1` のときだけ走る（加えて 1. は `STORAGE_TEST_DATABASE_URL` が要る）。
//! 計測値はリリースビルドで取ること:
//!
//! ```text
//! DATA_BENCH=1 STORAGE_TEST_DATABASE_URL=postgres://... \
//!   cargo test -p shiki-data --release --test internal_key_bench_it -- --nocapture --test-threads=1
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::collections::HashSet;
use std::hint::black_box;
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use roaring::RoaringTreemap;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

fn enabled() -> bool {
    if std::env::var("DATA_BENCH").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("DATA_BENCH=1 未設定のため計測をスキップ");
    false
}

/// `runs` 回計測した中央値。
fn median<F: FnMut()>(runs: usize, mut f: F) -> Duration {
    let mut v: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    v.sort();
    v[v.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.2}", d.as_secs_f64() * 1e3)
}

fn mib(bytes: i64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

// ---------------------------------------------------------------------------
// 1. 索引テーブルの大きさ（uuid vs record_int）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn index_table_size_uuid_vs_record_int() {
    if !enabled() {
        return;
    }
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();

    // 20 万レコード × 5 フィールド = 100 万索引行（値は select 相当の 1,000 種の文字列）。
    let records: i64 = std::env::var("DATA_BENCH_RECORDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let fields: i64 = 5;
    eprintln!("\n## 索引テーブルの大きさ（{records} レコード × {fields} フィールド）\n");
    eprintln!("| キー | 行数 | ヒープ MiB | 主キー MiB | 値索引 MiB | 合計 MiB | 1 行あたり B |");
    eprintln!("|---|---:|---:|---:|---:|---:|---:|");
    let mut totals = Vec::new();
    // §3.3 の列順（tenant_int int4, table_int int8, field_id int2, rec, ord int2, ...）と、
    // 固定長列を幅の降順に並べ替えた列順（アラインメントのパディングを消す）の両方を測る。
    let layout_doc =
        "tenant_int int not null, table_int bigint not null, field_id smallint not null, \
                      rec {K} not null, ord smallint not null default 0, \
                      val text not null collate \"C\", truncated boolean not null default false";
    let layout_packed = "table_int bigint not null, rec {K} not null, tenant_int int not null, \
                         field_id smallint not null, ord smallint not null default 0, \
                         truncated boolean not null default false, val text not null collate \"C\"";
    for (n, (label, key_type, key_expr, layout)) in [
        (
            "uuid（§3.3 の列順）",
            "uuid",
            "gen_random_uuid()",
            layout_doc,
        ),
        ("record_int（§3.3 の列順）", "bigint", "r.n", layout_doc),
        (
            "uuid（幅の降順に整列）",
            "uuid",
            "gen_random_uuid()",
            layout_packed,
        ),
        (
            "record_int（幅の降順に整列）",
            "bigint",
            "r.n",
            layout_packed,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let t = format!("bench_idx_{n}");
        let columns = layout.replace("{K}", key_type);
        sqlx::query(&format!("DROP TABLE IF EXISTS {t}"))
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query(&format!(
            "CREATE TEMP TABLE {t} ({columns}, \
               primary key (tenant_int, table_int, field_id, rec, ord))"
        ))
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE INDEX {t}_val ON {t} (tenant_int, table_int, field_id, val, rec)"
        ))
        .execute(&mut *conn)
        .await
        .unwrap();
        // レコードごとに 1 つのキーを作り、全フィールドで共有する（同じレコードの索引行）。
        sqlx::query(&format!(
            "INSERT INTO {t} (tenant_int, table_int, field_id, rec, val) \
             SELECT 7, 12345, f.id::smallint, r.k, 'opt-' || ((r.n * 7919 + f.id) % 1000) \
             FROM (SELECT n, {key_expr} AS k FROM generate_series(1, $1) AS r(n)) r \
             CROSS JOIN generate_series(16, 15 + $2) AS f(id)"
        ))
        .bind(records)
        .bind(fields)
        .execute(&mut *conn)
        .await
        .unwrap();
        sqlx::query(&format!("VACUUM ANALYZE {t}"))
            .execute(&mut *conn)
            .await
            .unwrap();
        let (rows, heap, pk, val): (i64, i64, i64, i64) = sqlx::query_as(&format!(
            "SELECT (SELECT count(*) FROM {t}), pg_relation_size('{t}'), \
                    pg_relation_size('{t}_pkey'), pg_relation_size('{t}_val')"
        ))
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        let total = heap + pk + val;
        totals.push(total);
        eprintln!(
            "| {label} | {rows} | {} | {} | {} | {} | {:.1} |",
            mib(heap),
            mib(pk),
            mib(val),
            mib(total),
            total as f64 / rows as f64
        );
        sqlx::query(&format!("DROP TABLE {t}"))
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    let saved = 1.0 - totals[1] as f64 / totals[0] as f64;
    let saved_packed = 1.0 - totals[3] as f64 / totals[2] as f64;
    let saved_best = 1.0 - totals[3] as f64 / totals[0] as f64;
    eprintln!(
        "\nrecord_int による削減率（合計・§3.3 の列順）: {:.1}%",
        saved * 100.0
    );
    eprintln!(
        "record_int による削減率（合計・整列した列順）: {:.1}%",
        saved_packed * 100.0
    );
    eprintln!(
        "uuid・§3.3 の列順 → record_int・整列した列順: {:.1}%",
        saved_best * 100.0
    );
    assert!(totals[1] < totals[0], "int8 キーの方が小さいはず");
}

// ---------------------------------------------------------------------------
// 2. 和集合・交差の速さ（uuid 集合 vs Roaring）
// ---------------------------------------------------------------------------

/// テーブル内の record_int 集合を作る。`stride` は他テーブルの挿入に挟まれる間隔
/// （プール共通シーケンスなのでテーブル内では疎になる・data-platform §3.2）。
fn record_ints(rng: &mut rand::rngs::StdRng, n: usize, stride: u64, keep: f64) -> Vec<u64> {
    let mut v = Vec::with_capacity(n);
    let mut x: u64 = 1_000_000;
    while v.len() < n {
        x += rng.gen_range(1..=stride.max(1) * 2 - 1);
        if rng.gen_bool(keep) {
            v.push(x);
        }
    }
    v
}

#[test]
fn set_ops_uuid_vs_roaring() {
    if !enabled() {
        return;
    }
    let mut rng = rand::rngs::StdRng::seed_from_u64(151);
    eprintln!("\n## 和集合・交差（中央値 ms・各 7 回）\n");
    eprintln!(
        "| ケース | 要素数 A/B | HashSet<Uuid> ∪ | HashSet<Uuid> ∩ | 整列 Vec<Uuid> ∪ | 整列 Vec<Uuid> ∩ | Roaring ∪ | Roaring ∩ | Roaring サイズ KiB | uuid 配列 KiB |"
    );
    eprintln!("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (label, n, stride) in [
        ("密（単一テーブルが連続挿入）", 1_000_000usize, 1u64),
        ("疎（100 テーブルが交互に挿入）", 1_000_000, 100),
        ("小（可視行 1 万）", 10_000, 100),
    ] {
        // A = 条件 1 の該当集合、B = 条件 2（A と半分重なる）。
        let a = record_ints(&mut rng, n, stride, 0.7);
        let mut b: Vec<u64> = a.iter().copied().step_by(2).collect();
        // 残り半分は A と交わらない値域から（交差の要素数を uuid 側と一致させる）。
        b.extend(
            record_ints(&mut rng, n / 2, stride, 0.7)
                .iter()
                .map(|x| x + 1_000_000_000_000),
        );
        b.sort_unstable();
        b.dedup();
        // uuid 側は同じ要素数・同じ重なり方を uuid で再現する。
        let mut ua: Vec<Uuid> = (0..a.len()).map(|_| Uuid::new_v4()).collect();
        let mut ub: Vec<Uuid> = ua.iter().copied().step_by(2).collect();
        ub.extend((0..(b.len() - ub.len())).map(|_| Uuid::new_v4()));
        ua.shuffle(&mut rng);
        ub.shuffle(&mut rng);
        let ha: HashSet<Uuid> = ua.iter().copied().collect();
        let hb: HashSet<Uuid> = ub.iter().copied().collect();
        let mut sa = ua.clone();
        sa.sort_unstable();
        let mut sb = ub.clone();
        sb.sort_unstable();
        let ra: RoaringTreemap = a.iter().copied().collect();
        let rb: RoaringTreemap = b.iter().copied().collect();

        let runs = 7;
        let h_union = median(runs, || {
            let u: HashSet<&Uuid> = ha.union(&hb).collect();
            black_box(u.len());
        });
        let h_inter = median(runs, || {
            let i: HashSet<&Uuid> = ha.intersection(&hb).collect();
            black_box(i.len());
        });
        let v_union = median(runs, || {
            black_box(merge_union(&sa, &sb).len());
        });
        let v_inter = median(runs, || {
            black_box(merge_inter(&sa, &sb).len());
        });
        let r_union = median(runs, || {
            black_box((&ra | &rb).len());
        });
        let r_inter = median(runs, || {
            black_box((&ra & &rb).len());
        });
        // 結果の要素数は同じ重なり方なので一致する（計測の妥当性）。
        assert_eq!((&ra & &rb).len() as usize, ha.intersection(&hb).count());

        eprintln!(
            "| {label} | {}/{} | {} | {} | {} | {} | {} | {} | {:.0} | {:.0} |",
            a.len(),
            b.len(),
            ms(h_union),
            ms(h_inter),
            ms(v_union),
            ms(v_inter),
            ms(r_union),
            ms(r_inter),
            ra.serialized_size() as f64 / 1024.0,
            (ua.len() * 16) as f64 / 1024.0
        );
    }
}

fn merge_union(a: &[Uuid], b: &[Uuid]) -> Vec<Uuid> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn merge_inter(a: &[Uuid], b: &[Uuid]) -> Vec<Uuid> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}
