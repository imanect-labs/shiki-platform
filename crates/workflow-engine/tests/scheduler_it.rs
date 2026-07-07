//! スケジューラ／イベントトリガの結合テスト（Task 10.3 受け入れ条件・実 Postgres）。
//!
//! - 複数インスタンス競走でもスケジュールは 1 回だけ発火（occurrence UNIQUE）
//! - enqueue 直後クラッシュ→再 tick でも同一 occurrence の run は 1 つ（冪等）
//! - storage 書込イベントでワークフローが起動する
//! - 無効化済みワークフローのトリガは発火しない
//! - リーダーリースは同時に 1 つだけが保持する

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;
use workflow_engine::scheduler::LeaderLease;
use workflow_engine::{RunLauncher, SchedulerStore};

/// 起動回数を数え、疑似 run_id を返す launcher。
struct CountingLauncher {
    launches: AtomicUsize,
}

#[async_trait]
impl RunLauncher for CountingLauncher {
    async fn launch(&self, _t: &str, _w: Uuid, _k: &str, _tid: &str) -> Option<Uuid> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        Some(Uuid::new_v4())
    }
}

async fn setup() -> Option<PgPool> {
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

/// registration（enabled/disabled）＋トリガを 1 本用意する。
#[allow(clippy::too_many_arguments)]
async fn register(
    pool: &PgPool,
    tenant: &str,
    workflow_id: Uuid,
    status: &str,
    trigger_id: &str,
    kind: &str,
    source: Option<&str>,
    spec: serde_json::Value,
) {
    sqlx::query(
        "INSERT INTO workflow_registration (tenant_id, workflow_id, org, status, enabled_version, consented_scopes) \
         VALUES ($1, $2, 'acme', $3, 1, '{}')",
    )
    .bind(tenant)
    .bind(workflow_id)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_trigger (tenant_id, trigger_id, workflow_id, version, kind, source, spec) \
         VALUES ($1, $2, $3, 1, $4, $5, $6)",
    )
    .bind(tenant)
    .bind(trigger_id)
    .bind(workflow_id)
    .bind(kind)
    .bind(source)
    .bind(sqlx::types::Json(spec))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn schedule_fires_once_even_with_concurrent_ticks() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    // 毎分 cron。watermark を 2 分前に置き、now までの 2 occurrence のうち catchup=skip で直近 1 発火。
    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-1",
        "schedule",
        None,
        json!({ "cron": "* * * * *", "tz": "UTC", "catchup": "skip" }),
    )
    .await;
    let after = Utc.with_ymd_and_hms(2026, 7, 7, 0, 0, 0).unwrap();
    sqlx::query("UPDATE workflow_trigger SET last_planned_at = $2 WHERE tenant_id = $1")
        .bind(&tenant)
        .bind(after)
        .execute(&pool)
        .await
        .unwrap();
    let now = Utc.with_ymd_and_hms(2026, 7, 7, 0, 3, 0).unwrap();

    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });

    // 2 つの tick を同時に走らせる（2 インスタンス競走の模擬）。
    let (a, b) = tokio::join!(
        store.tick_schedules(now, Some(&tenant), launcher.as_ref()),
        store.tick_schedules(now, Some(&tenant), launcher.as_ref()),
    );
    let fired = a.unwrap() + b.unwrap();
    assert_eq!(fired, 1, "競走しても発火は 1 回（occurrence UNIQUE）");
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

    // 再 tick（クラッシュ後再起動の模擬）でも二重投入しない。
    let again = store
        .tick_schedules(now, Some(&tenant), launcher.as_ref())
        .await
        .unwrap();
    assert_eq!(again, 0, "同一 occurrence は再発火しない（冪等）");
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disabled_workflow_does_not_fire() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(
        &pool,
        &tenant,
        wf,
        "disabled",
        "trg-d",
        "schedule",
        None,
        json!({ "cron": "* * * * *", "tz": "UTC" }),
    )
    .await;
    sqlx::query("UPDATE workflow_trigger SET last_planned_at = $2 WHERE tenant_id = $1")
        .bind(&tenant)
        .bind(Utc.with_ymd_and_hms(2026, 7, 7, 0, 0, 0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });
    let fired = store
        .tick_schedules(
            Utc.with_ymd_and_hms(2026, 7, 7, 0, 3, 0).unwrap(),
            Some(&tenant),
            launcher.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(fired, 0, "無効化済みワークフローは発火しない");
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn storage_write_event_triggers_run_once() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    // folder スコープにマッチする event トリガ。
    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-e",
        "event",
        Some("storage.write"),
        json!({ "scope": { "folder": "reports" } }),
    )
    .await;
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });

    let scope = json!({ "folder": "reports" });
    let fired = store
        .match_event(&tenant, "storage.write", 42, &scope, launcher.as_ref())
        .await
        .unwrap();
    assert_eq!(fired, 1, "storage 書込でワークフローが起動する");

    // 同一 event_id は再発火しない（outbox 1 件 1 run）。
    let again = store
        .match_event(&tenant, "storage.write", 42, &scope, launcher.as_ref())
        .await
        .unwrap();
    assert_eq!(again, 0);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

    // スコープが合わないイベントはマッチしない。
    let other = store
        .match_event(
            &tenant,
            "storage.write",
            43,
            &json!({ "folder": "other" }),
            launcher.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(other, 0);
}

#[tokio::test]
async fn leader_lease_is_mutually_exclusive() {
    let Some(pool) = setup().await else { return };
    // 単一行のグローバルリース。まず既存を消してクリーンに。
    sqlx::query("DELETE FROM scheduler_lease WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let a = LeaderLease::new(pool.clone(), "inst-a", 60);
    let b = LeaderLease::new(pool.clone(), "inst-b", 60);

    assert!(
        a.acquire_or_renew().await.unwrap(),
        "a が最初にリーダーを取る"
    );
    assert!(
        !b.acquire_or_renew().await.unwrap(),
        "b は取れない（a が保持中）"
    );
    assert!(a.acquire_or_renew().await.unwrap(), "a は更新できる");

    // a が明け渡すと b が取れる。
    a.release().await.unwrap();
    assert!(b.acquire_or_renew().await.unwrap(), "解放後は b が取れる");
    // 後片付け。
    b.release().await.unwrap();
}
