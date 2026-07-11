#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use std::time::Duration;

use script_runtime::ScriptEngine;
use storage::event::{emit_on, WriteEvent, WriteOp};
use storage::{ObjectStore, ObjectStoreError};

/// 何も保持しないダミー ObjectStore（トリガのDB経路テストでは runner.run は呼ばれない）。
struct NoStore;

#[async_trait::async_trait]
impl ObjectStore for NoStore {
    async fn ensure_bucket(&self) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn presign_put(&self, _: &str, _: Duration, _: i64) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn presign_get_internal(&self, _: &str, _: Duration) -> Result<String, ObjectStoreError> {
        Ok(String::new())
    }
    async fn read_and_hash(&self, k: &str) -> Result<(String, u64), ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn put_object(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn get_object(&self, k: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound(k.into()))
    }
    async fn exists(&self, _: &str) -> Result<bool, ObjectStoreError> {
        Ok(false)
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

async fn pool() -> Option<PgPool> {
    let url = std::env::var("STORAGE_TEST_DATABASE_URL").ok()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

fn deps(pool: PgPool) -> TriggerDeps {
    let engine = Arc::new(ScriptEngine::new().expect("engine"));
    let runner = Arc::new(
        FunctionRunner::new(engine, Arc::new(NoStore), "http://127.0.0.1:1".into())
            .expect("runner"),
    );
    let port = Arc::new(GatewayFunctionPort {
        runner: Arc::clone(&runner),
        http: reqwest::Client::new(),
        token_endpoint: "http://127.0.0.1:1/token".into(),
        secrets: None,
        gateway_audience: "shiki-gateway".into(),
        installations: app_gateway::AppInstallationStore::new(pool.clone()),
    });
    TriggerDeps {
        db: pool,
        runner,
        port,
    }
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "installer".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

/// event_tick: outbox の event_type を購読と突合し、配送台帳に ack する（起動は失効で bail）。
#[tokio::test]
async fn event_tick_matches_subscription_and_acks() {
    let Some(pool) = pool().await else { return };
    let consumer = "miniapp-functions";
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let event_type = "data.record.transitioned";

    // consumer 登録＋既存バックログを配送済みにして本テストのイベントを分離する。
    {
        let mut conn = pool.acquire().await.unwrap();
        storage::event::register_consumer(&mut conn, consumer)
            .await
            .unwrap();
    }
    let mut tx = pool.begin().await.unwrap();
    let pending = storage::event::claim_undelivered(&mut tx, consumer, 100_000)
        .await
        .unwrap();
    let ids: Vec<i64> = pending.iter().map(|e| e.id).collect();
    storage::event::mark_delivered(&mut tx, consumer, &ids)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // 購読を登録し、event_type 付きイベントを 1 件発行。
    sqlx::query(
        "INSERT INTO app_event_subscription (tenant_id, org, app_id, event_type, function) \
             VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&tenant)
    .bind("acme")
    .bind(app_id)
    .bind(event_type)
    .bind("on_approved")
    .execute(&pool)
    .await
    .unwrap();
    {
        let mut conn = pool.acquire().await.unwrap();
        emit_on(
            &mut conn,
            &ctx(&tenant),
            WriteEvent {
                node_id: app_id,
                version: 1,
                op: WriteOp::Update,
                payload: serde_json::json!({ "event_type": event_type, "record": "r1" }),
            },
            None,
        )
        .await
        .unwrap();
    }

    // 起動は installation 失効で bail するが、claim→突合→ack の DB 経路は完走する。
    event_tick(&deps(pool.clone())).await.unwrap();

    // 発行イベントは配送済み（再 claim に現れない）。
    let mut tx = pool.begin().await.unwrap();
    let remaining = storage::event::claim_undelivered(&mut tx, consumer, 100_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        remaining.iter().all(|e| e.node_id != app_id),
        "本テストのイベントは ack 済みであるべき"
    );
}

/// cron_tick: due スケジュールを実行台帳へ一意記録し、next_run_at を前進させる。
#[tokio::test]
async fn cron_tick_records_run_and_advances_next() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();

    let (sched_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO app_function_schedule \
                 (tenant_id, org, app_id, function, expr, next_run_at) \
             VALUES ($1, $2, $3, $4, $5, now() - interval '1 minute') RETURNING id",
    )
    .bind(&tenant)
    .bind("acme")
    .bind(app_id)
    .bind("nightly")
    .bind("*/5 * * * *")
    .fetch_one(&pool)
    .await
    .unwrap();

    cron_tick(&deps(pool.clone())).await.unwrap();

    let (runs,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM app_function_run WHERE schedule_id = $1")
            .bind(sched_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(runs, 1, "due スケジュールは実行台帳へ 1 度だけ記録される");

    let (next,): (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("SELECT next_run_at FROM app_function_schedule WHERE id = $1")
            .bind(sched_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(next > chrono::Utc::now(), "next_run_at は将来へ前進する");
}

/// event_targets_app: table_id は所有アプリのみ・app_id は一致必須・無しは汎用配送（越境防止）。
#[tokio::test]
async fn event_targets_app_scopes_to_owned_resources() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let other_app = Uuid::new_v4();
    let (table_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO data_table (tenant_id, org, app_id, name, schema, created_by) \
             VALUES ($1, 'acme', $2, $3, '{}'::jsonb, 'installer') RETURNING id",
    )
    .bind(&tenant)
    .bind(app_id)
    .bind(format!("tbl-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();

    // 自アプリ所有テーブルは配送対象・他アプリからは同 table_id を配送しない。
    assert!(event_targets_app(
        &mut conn,
        &tenant,
        app_id,
        &serde_json::json!({ "table_id": table_id })
    )
    .await
    .unwrap());
    assert!(!event_targets_app(
        &mut conn,
        &tenant,
        other_app,
        &serde_json::json!({ "table_id": table_id })
    )
    .await
    .unwrap());
    // app_id 一致は配送・不一致は非配送。
    assert!(event_targets_app(
        &mut conn,
        &tenant,
        app_id,
        &serde_json::json!({ "app_id": app_id })
    )
    .await
    .unwrap());
    assert!(!event_targets_app(
        &mut conn,
        &tenant,
        app_id,
        &serde_json::json!({ "app_id": other_app })
    )
    .await
    .unwrap());
    // table_id/app_id なしの汎用イベントは購読どおり配送。
    assert!(event_targets_app(
        &mut conn,
        &tenant,
        app_id,
        &serde_json::json!({ "kind": "x" })
    )
    .await
    .unwrap());
}
