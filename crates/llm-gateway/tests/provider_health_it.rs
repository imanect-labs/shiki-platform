//! 稼働状況の書き出し（加算 upsert）と読み取りの結合テスト（実 Postgres）。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行（未設定はスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::time::Duration;

use chrono::Utc;
use llm_gateway::health::{self, HealthRecorder, HealthState, HealthThresholds, Outcome};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

async fn pool() -> Option<PgPool> {
    let Ok(db_url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn id() -> String {
    format!("health-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
async fn flushes_from_two_recorders_add_up() {
    let Some(db) = pool().await else { return };
    let provider = id();
    // 2 インスタンスぶんの記録先が同じ分へ書き出しても足し合わせになる。
    let first = HealthRecorder::new();
    let second = HealthRecorder::new();
    for _ in 0..18 {
        first.record(&provider, Outcome::Ok, Some(Duration::from_millis(400)));
    }
    second.record(&provider, Outcome::Ok, Some(Duration::from_millis(400)));
    second.record(&provider, Outcome::Failed, None);
    second.record(&provider, Outcome::Failed, None);
    first.flush(&db).await.unwrap();
    second.flush(&db).await.unwrap();
    // 2 回目の書き出しは空振り（二重に足さない）。
    first.flush(&db).await.unwrap();

    let thresholds = HealthThresholds::default();
    let got = health::status::read(
        &db,
        std::slice::from_ref(&provider),
        Utc::now(),
        &thresholds,
    )
    .await
    .unwrap();
    let h = &got[0];
    // 19 成功・2 障害 → 障害率 2/21 ≒ 9.5%（不安定・停止の手前）。
    assert_eq!(h.state, HealthState::Degraded);
    assert!((h.failure_rate.unwrap() - 2.0 / 21.0).abs() < 1e-9);
    assert_eq!(h.ttft_p95_ms, Some(500));
    // 最後の成功は 15 分単位に丸める。
    let last = h.last_ok_at.unwrap();
    assert_eq!(last.timestamp() % (15 * 60), 0);
    assert_eq!(h.hours.len(), 24);
    assert_eq!(h.hours[23].state, HealthState::Degraded);
    assert!(h.hours[..23].iter().all(|b| b.state == HealthState::Idle));
}

#[tokio::test]
async fn unknown_provider_reads_as_idle() {
    let Some(db) = pool().await else { return };
    let p = id();
    let got = health::status::read(
        &db,
        std::slice::from_ref(&p),
        Utc::now(),
        &HealthThresholds::default(),
    )
    .await
    .unwrap();
    assert_eq!(got[0].state, HealthState::Idle);
    assert_eq!(got[0].availability_24h, None);
    assert_eq!(got[0].last_ok_at, None);
}

#[tokio::test]
async fn small_samples_hide_rates_but_keep_the_state() {
    let Some(db) = pool().await else { return };
    let provider = id();
    let rec = HealthRecorder::new();
    rec.record(&provider, Outcome::Ok, Some(Duration::from_millis(400)));
    rec.record(&provider, Outcome::Failed, None);
    rec.record(&provider, Outcome::Failed, None);
    rec.flush(&db).await.unwrap();
    let got = health::status::read(
        &db,
        std::slice::from_ref(&provider),
        Utc::now(),
        &HealthThresholds::default(),
    )
    .await
    .unwrap();
    let h = &got[0];
    // 3 件では停止にせず、率・時間も返さない（他テナントの件数が透けるため）。
    assert_eq!(h.state, HealthState::Degraded);
    assert_eq!(h.failure_rate, None);
    assert_eq!(h.ttft_p95_ms, None);
    assert_eq!(h.availability_24h, None);
    assert_eq!(h.hours[23].availability, None);
    assert_eq!(h.hours[23].state, HealthState::Degraded);
}
