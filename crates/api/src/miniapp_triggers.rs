//! B2 関数の event/cron トリガ（Task 9.12）。
//!
//! - **event**: outbox 配送台帳（consumer=`miniapp-functions`・at-least-once）から
//!   アプリ向けドメインイベントを取り出し、`app_event_subscription`（インストール時ピン）と
//!   突合して関数を起動する。配送 ack はディスパッチ後（起動失敗はログ・再配送しない＝
//!   関数は at-most-once 起動。厳密リトライは関数側の冪等設計に委ねる・アルファ）。
//! - **cron**: `app_function_schedule` の due を advisory lock リーダーが拾い、
//!   `(schedule_id, scheduled_at)` 一意の実行台帳で二重起動を防ぐ。
//!
//! いずれも **service identity（B2 client_credentials）** で実行する（ユーザー文脈なし・
//! 所有テーブルの owner@miniapp ReBAC で能力が絞られる）。

use std::sync::Arc;

use app_platform::{FunctionActor, FunctionInvocation, FunctionRunner};
use authz::{AuthContext, Principal, PrincipalKind};
use sqlx::PgPool;
use uuid::Uuid;

use crate::gateway_functions::GatewayFunctionPort;

/// cron リーダー選出の advisory lock キー（プロセス横断で一意）。
const CRON_LOCK_KEY: i64 = 0x5348_494B_4930_3132; // "SHIKI-912"

pub(crate) struct TriggerDeps {
    pub db: PgPool,
    pub runner: Arc<FunctionRunner>,
    pub port: Arc<GatewayFunctionPort>,
}

/// event/cron トリガのループを spawn する（gateway 有効時のみ呼ばれる）。
pub(crate) fn spawn_miniapp_triggers(deps: TriggerDeps) {
    let deps = Arc::new(deps);
    let event_deps = Arc::clone(&deps);
    tokio::spawn(async move {
        // 台帳コンシューマ登録（初回のみ効果・冪等）。
        if let Ok(mut conn) = event_deps.db.acquire().await {
            let _ = storage::event::register_consumer(&mut conn, "miniapp-functions").await;
        }
        loop {
            if let Err(e) = event_tick(&event_deps).await {
                tracing::warn!(error = %e, "miniapp event トリガの処理に失敗");
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    tokio::spawn(async move {
        loop {
            if let Err(e) = cron_tick(&deps).await {
                tracing::warn!(error = %e, "miniapp cron トリガの処理に失敗");
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    tracing::info!("miniapp event/cron トリガを起動しました");
}

#[derive(sqlx::FromRow)]
struct Subscription {
    tenant_id: String,
    org: String,
    app_id: Uuid,
    function: String,
}

async fn event_tick(deps: &TriggerDeps) -> anyhow::Result<()> {
    let mut tx = deps.db.begin().await?;
    let events = storage::event::claim_undelivered(&mut tx, "miniapp-functions", 50).await?;
    if events.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    // 突合対象（event_type 付きのみ）を集めてから ack（at-least-once の台帳→起動は at-most-once）。
    let mut dispatches: Vec<(Subscription, serde_json::Value, String)> = Vec::new();
    for event in &events {
        let Some(event_type) = event.payload.get("event_type").and_then(|v| v.as_str()) else {
            continue;
        };
        let subs: Vec<Subscription> = sqlx::query_as(
            "SELECT tenant_id, org, app_id, function FROM app_event_subscription \
             WHERE tenant_id = $1 AND event_type = $2",
        )
        .bind(&event.tenant_id)
        .bind(event_type)
        .fetch_all(&mut *tx)
        .await?;
        for sub in subs {
            dispatches.push((sub, event.payload.clone(), event_type.to_string()));
        }
    }
    storage::event::mark_delivered(&mut tx, "miniapp-functions", &ids).await?;
    tx.commit().await?;

    for (sub, payload, event_type) in dispatches {
        if let Err(e) = invoke_service(deps, &sub, payload).await {
            tracing::warn!(error = %e, app_id = %sub.app_id, function = %sub.function,
                event_type, "event 起動の関数実行に失敗");
        }
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct DueSchedule {
    id: Uuid,
    tenant_id: String,
    org: String,
    app_id: Uuid,
    function: String,
    expr: String,
    next_run_at: chrono::DateTime<chrono::Utc>,
}

async fn cron_tick(deps: &TriggerDeps) -> anyhow::Result<()> {
    let mut tx = deps.db.begin().await?;
    // リーダー選出（tx スコープの advisory lock・多重起動しても 1 プロセスだけが拾う）。
    let (leader,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock($1)")
        .bind(CRON_LOCK_KEY)
        .fetch_one(&mut *tx)
        .await?;
    if !leader {
        return Ok(());
    }
    let due: Vec<DueSchedule> = sqlx::query_as(
        "SELECT id, tenant_id, org, app_id, function, expr, next_run_at \
         FROM app_function_schedule WHERE next_run_at <= now() \
         ORDER BY next_run_at LIMIT 20 FOR UPDATE SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut to_run = Vec::new();
    for s in due {
        // (schedule_id, scheduled_at) 一意＝リーダー交代/再起動でも同一時刻の二重起動なし。
        let inserted = sqlx::query(
            "INSERT INTO app_function_run (schedule_id, scheduled_at) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(s.id)
        .bind(s.next_run_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let next = app_platform::next_cron_run_after(&s.expr, chrono::Utc::now())
            .unwrap_or(s.next_run_at + chrono::Duration::hours(1));
        sqlx::query("UPDATE app_function_schedule SET next_run_at = $2 WHERE id = $1")
            .bind(s.id)
            .bind(next)
            .execute(&mut *tx)
            .await?;
        if inserted > 0 {
            to_run.push(s);
        }
    }
    tx.commit().await?;

    for s in to_run {
        let sub = Subscription {
            tenant_id: s.tenant_id,
            org: s.org,
            app_id: s.app_id,
            function: s.function,
        };
        if let Err(e) = invoke_service(deps, &sub, serde_json::json!({ "trigger": "cron" })).await {
            tracing::warn!(error = %e, app_id = %sub.app_id, function = %sub.function,
                "cron 起動の関数実行に失敗");
        }
    }
    Ok(())
}

/// service identity（B2 client_credentials）で関数を実行する。
async fn invoke_service(
    deps: &TriggerDeps,
    sub: &Subscription,
    payload: serde_json::Value,
) -> anyhow::Result<()> {
    // インストール時ピン（server_bundle/spec/client）を解決する（失効済みは起動しない）。
    let installation = deps
        .port
        .runner_installation(&sub.tenant_id, sub.app_id)
        .await?;
    let Some(installation) = installation else {
        anyhow::bail!("インストールが失効しています");
    };
    let (Some(server_bundle), Some(client_id_b2)) = (
        installation.server_bundle.clone(),
        installation.client_id_b2.clone(),
    ) else {
        anyhow::bail!("B2 構成（server_bundle/client）がありません");
    };
    // service ctx（監査 actor 用・org/tenant は購読行から）。
    let ctx = AuthContext::new(
        Principal {
            kind: PrincipalKind::MiniApp,
            id: sub.app_id.to_string(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(sub.tenant_id.clone()),
        },
        sub.org.clone(),
        sub.tenant_id.clone(),
    );
    let secret = deps
        .port
        .resolve_b2_secret(&ctx, sub.app_id)
        .await
        .map_err(|e| anyhow::anyhow!("secret: {e}"))?;
    let token = deps
        .port
        .client_credentials_token(&client_id_b2, &secret)
        .await
        .map_err(|e| anyhow::anyhow!("token: {e}"))?;
    let egress = installation
        .server_spec
        .as_ref()
        .and_then(|s| s.get("egress_allowlist"))
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    let outcome = deps
        .runner
        .run(
            &server_bundle,
            FunctionInvocation {
                tenant_id: sub.tenant_id.clone(),
                app_id: sub.app_id,
                function: sub.function.clone(),
                payload,
                bearer: token,
                actor: FunctionActor::Service,
                egress_allowlist: egress,
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("run: {e}"))?;
    if !outcome.ok {
        tracing::warn!(app_id = %sub.app_id, function = %sub.function,
            value = %outcome.value, "関数がエラー終了");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        async fn presign_put(
            &self,
            _: &str,
            _: Duration,
            _: i64,
        ) -> Result<String, ObjectStoreError> {
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
        async fn presign_get_internal(
            &self,
            _: &str,
            _: Duration,
        ) -> Result<String, ObjectStoreError> {
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
}
