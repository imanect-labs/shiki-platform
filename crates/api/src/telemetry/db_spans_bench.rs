//! DB span のオーバーヘッドの計測（`STORAGE_TEST_DATABASE_URL` があるとき。`DATA_BENCH=1` で本計測、
//! 無ければ回数を絞った煙試験として流れる）。煙試験でも、実際の sqlx（Postgres）が出すイベントから
//! DB span が作られること（sqlx のフィールド名への追従）を確かめる。
//!
//! 実 sqlx の確認をここに寄せているのは、`cargo test` が同じプロセスで別スレッドに別の subscriber を
//! 立てると、per-layer フィルタの callsite の interest が別スレッドで引き直されて不安定になるため
//! （本番は subscriber が 1 つ・CI の nextest はテストごとにプロセスが分かれる）。
//!
//! ```bash
//! DATA_BENCH=1 STORAGE_TEST_DATABASE_URL=postgres://… \
//!   cargo test -p shiki-api --all-features --lib db_span_overhead -- --nocapture
//! ```
//!
//! 同じクエリを「変更前の OTLP 有効時の構成（全体に EnvFilter＋OTel レイヤ）」と「変更後（`otel_layers`）」で
//! 交互に流し、1 クエリの所要時間の分布を比べる。エクスポータは送らずに捨てる（バッチ処理は本番と同じ）。

use std::time::{Duration, Instant};

use opentelemetry_sdk::{error::OTelSdkResult, trace::SpanExporter};
use tracing::Instrument as _;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData};
use tracing_subscriber::layer::SubscriberExt as _;

/// 送らずに捨てるエクスポータ。計測が本当に DB span を作ったかを確かめるため、期待どおりの名前の
/// client span だけ数える。
#[derive(Debug, Clone, Default)]
struct DiscardExporter {
    db_spans: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl SpanExporter for DiscardExporter {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let n = batch
            .iter()
            .filter(|s| {
                s.span_kind == opentelemetry::trace::SpanKind::Client
                    && s.name == "SELECT generate_series"
            })
            .count();
        self.db_spans
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

async fn run_round(pool: &sqlx::PgPool, per_round: usize, out: &mut Vec<Duration>) -> Duration {
    let started = Instant::now();
    for i in 0..per_round {
        let t = Instant::now();
        let rows: Vec<i32> = sqlx::query_scalar("SELECT g FROM generate_series(1, $1) AS g")
            .bind(20 + (i % 3) as i32)
            .fetch_all(pool)
            .instrument(tracing::info_span!("request"))
            .await
            .unwrap();
        assert!(rows.len() >= 20);
        out.push(t.elapsed());
    }
    started.elapsed() / per_round as u32
}

fn report(name: &str, mut all: Vec<Duration>, round_means: &[Duration]) -> f64 {
    all.sort_unstable();
    let mean = all.iter().sum::<Duration>() / all.len() as u32;
    let rm: Vec<f64> = round_means.iter().map(|d| us(*d)).collect();
    let avg = rm.iter().sum::<f64>() / rm.len() as f64;
    let sd = (rm.iter().map(|x| (x - avg).powi(2)).sum::<f64>() / (rm.len() as f64 - 1.0)).sqrt();
    println!(
        "{name:<14} n={} mean={:.1}µs p50={:.1}µs p95={:.1}µs p99={:.1}µs | 回ごとの平均の標準偏差={sd:.1}µs",
        all.len(),
        us(mean),
        us(percentile(&all, 0.50)),
        us(percentile(&all, 0.95)),
        us(percentile(&all, 0.99)),
    );
    us(mean)
}

#[tokio::test]
async fn db_span_overhead() {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let (rounds, per_round) = if std::env::var("DATA_BENCH").as_deref() == Ok("1") {
        (20, 500)
    } else {
        (2, 3)
    };
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let exporter = DiscardExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("bench");
    // 変更前の構成（全体に EnvFilter・OTel レイヤ）と、変更後の構成（per-layer ＋ DB span）。
    let before = || {
        tracing_subscriber::registry()
            .with(crate::telemetry::env_filter())
            .with(tracing_opentelemetry::layer().with_tracer(tracer.clone()))
    };
    let after =
        || tracing_subscriber::registry().with(crate::telemetry::otel_layers(tracer.clone()));

    // 温め（接続・準備済み文のキャッシュ）。
    let mut warm = Vec::new();
    run_round(&pool, per_round, &mut warm).await;

    let (mut b_all, mut a_all) = (Vec::new(), Vec::new());
    let (mut b_rounds, mut a_rounds) = (Vec::new(), Vec::new());
    for r in 0..rounds {
        // 順番の偏り（温まり・他の負荷）を打ち消すため、回ごとに先後を入れ替える。
        for first_before in [r % 2 == 0, r % 2 != 0] {
            if first_before {
                let _g = tracing::subscriber::set_default(before());
                b_rounds.push(run_round(&pool, per_round, &mut b_all).await);
            } else {
                let _g = tracing::subscriber::set_default(after());
                a_rounds.push(run_round(&pool, per_round, &mut a_all).await);
            }
        }
    }
    println!("DB span のオーバーヘッド（{rounds} 回 × {per_round} クエリ・各回で先後を交互）");
    let b = report("変更前(OTel)", b_all, &b_rounds);
    let a = report("変更後(+DB)", a_all, &a_rounds);
    println!(
        "差（平均）= {:+.1}µs/クエリ（{:+.2}%）",
        a - b,
        (a - b) / b * 100.0
    );
    provider.shutdown().unwrap();
    // 「変更後」の回のクエリだけが DB span になっている（計測が空振りしていない）こと。
    assert_eq!(
        exporter.db_spans.load(std::sync::atomic::Ordering::Relaxed),
        rounds * per_round
    );
}
