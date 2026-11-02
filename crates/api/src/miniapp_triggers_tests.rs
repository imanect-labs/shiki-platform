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
        installations: app_gateway::AppInstallationStore::new(pool.clone()),
    });
    TriggerDeps {
        db: pool,
        runner,
        port,
        claim_hints: storage::event::ClaimHints::default(),
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

    // 期限の来た、式に当たる時刻（5 分ごとの区切り）。
    let due = app_platform::next_cron_run_after(
        "*/5 * * * *",
        chrono::Utc::now() - chrono::Duration::minutes(10),
    )
    .unwrap();
    let (sched_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO app_function_schedule \
                 (tenant_id, org, app_id, function, expr, next_run_at) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(&tenant)
    .bind("acme")
    .bind(app_id)
    .bind("nightly")
    .bind("*/5 * * * *")
    .bind(due)
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

/// 曜日を揃える前の読み方（1 を日曜）で計算した、まだ来ていない実行の時刻は、起動時に今の読み方（1 が月曜）で
/// 計算し直す。合っている時刻・期限の来た時刻は触らない（BUG-129）。
#[tokio::test]
async fn realign_moves_stale_weekday_schedules_to_the_standard_reading() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let now = chrono::Utc::now();
    // 「毎週月曜 9:00（UTC）」を、cron クレートの読み方で計算した時刻（日曜 9:00）のまま保存した行。
    let sunday = app_platform::next_cron_run_after("0 9 * * 0", now).unwrap();
    let monday = app_platform::next_cron_run_after("0 9 * * 1", now).unwrap();
    let insert = |expr: &'static str, at: chrono::DateTime<chrono::Utc>| {
        let (pool, tenant) = (pool.clone(), tenant.clone());
        async move {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO app_function_schedule (tenant_id, org, app_id, function, expr, next_run_at) \
                 VALUES ($1, 'acme', $2, 'weekly', $3, $4) RETURNING id",
            )
            .bind(&tenant)
            .bind(Uuid::new_v4())
            .bind(expr)
            .bind(at)
            .fetch_one(&pool)
            .await
            .unwrap();
            id
        }
    };
    let stale = insert("0 9 * * 1", sunday).await;
    let right = insert("0 9 * * 1", monday).await;
    let due = insert("0 9 * * 1", now - chrono::Duration::minutes(1)).await;

    assert!(realign_schedules(&pool, now).await.unwrap() >= 1);
    let at = |id: Uuid| {
        let pool = pool.clone();
        async move {
            let (t,): (chrono::DateTime<chrono::Utc>,) =
                sqlx::query_as("SELECT next_run_at FROM app_function_schedule WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            t
        }
    };
    assert_eq!(
        at(stale).await,
        monday,
        "日曜に送っていた月曜の予定は月曜になる"
    );
    assert_eq!(at(right).await, monday, "合っている時刻は変わらない");
    assert!(at(due).await <= now, "期限の来た行は次の tick に任せる");
    // 何度流しても同じ（起動のたびに流してよい）。
    realign_schedules(&pool, now).await.unwrap();
    assert_eq!(at(stale).await, monday);

    sqlx::query("DELETE FROM app_function_schedule WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}

/// 期限の来た時刻が今の読み方で式に当たらない（古い読み方で計算した日曜の「毎週月曜」・ローリング更新の間に古い版
/// が書き戻した時刻）なら、実行せず、次の時刻を今の読み方で計算し直す（BUG-129）。
#[tokio::test]
async fn cron_tick_does_not_run_a_time_the_expression_does_not_hit() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let now = chrono::Utc::now();
    // 過ぎた日曜 9:00（UTC）。「0 9 * * 1」を古い読み方（1 が日曜）で計算した時刻。
    let sunday =
        app_platform::next_cron_run_after("0 9 * * 0", now - chrono::Duration::days(8)).unwrap();
    assert!(sunday < now);
    let (sched_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO app_function_schedule (tenant_id, org, app_id, function, expr, next_run_at) \
         VALUES ($1, 'acme', $2, 'weekly', '0 9 * * 1', $3) RETURNING id",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .bind(sunday)
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
    assert_eq!(runs, 0, "日曜には実行しない");
    let (next,): (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("SELECT next_run_at FROM app_function_schedule WHERE id = $1")
            .bind(sched_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        next,
        app_platform::next_cron_run_after("0 9 * * 1", chrono::Utc::now()).unwrap(),
        "次の月曜 9:00 に計算し直す"
    );

    sqlx::query("DELETE FROM app_function_schedule WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}

/// 読めない式（標準の読み方で写せない曜日）は実行せず、毎時に実行しないよう 1 日後へ送る（BUG-129）。
#[tokio::test]
async fn cron_tick_skips_unreadable_expressions() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let (sched_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO app_function_schedule (tenant_id, org, app_id, function, expr, next_run_at) \
         VALUES ($1, 'acme', $2, 'nightly', '0 9 * * 5-1', now() - interval '1 minute') RETURNING id",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
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
    assert_eq!(runs, 0, "読めない式では実行しない");
    let (next,): (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("SELECT next_run_at FROM app_function_schedule WHERE id = $1")
            .bind(sched_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        next > chrono::Utc::now() + chrono::Duration::hours(23),
        "確かめ直すのは 1 日後: {next}"
    );

    sqlx::query("DELETE FROM app_function_schedule WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}

/// event_targets_app: table_id は所有アプリのみ・app_id は一致必須・無しは汎用配送（越境防止）。
#[tokio::test]
async fn event_targets_app_scopes_to_owned_resources() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let other_app = Uuid::new_v4();
    let mut conn = pool.acquire().await.unwrap();
    // 内部整数キーはレジストリの採番サービスから取る（Task 15.1・列 default は持たない）。
    let placement = storage::tenant::ensure_placement(&mut conn, &tenant)
        .await
        .unwrap();
    let table_int = storage::tenant::allocate_table_int(&mut conn)
        .await
        .unwrap();
    let (table_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO data_table (tenant_id, org, app_id, name, schema, created_by, \
               tenant_int, table_int) \
             VALUES ($1, 'acme', $2, $3, '{\"fields\": []}'::jsonb, 'installer', $4, $5) RETURNING id",
    )
    .bind(&tenant)
    .bind(app_id)
    .bind(format!("tbl-{}", Uuid::new_v4()))
    .bind(placement.tenant_int)
    .bind(table_int)
    .fetch_one(&mut *conn)
    .await
    .unwrap();

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

/// 自動実行の失敗は、作成者に見せてよい文で関数ごとに 1 行残し、続けて失敗した回数を足す。成功したら消す。
/// インストールが無いアプリは記録しない（GAP-60 の残り）。
#[tokio::test]
async fn run_service_keeps_the_latest_failure_per_function() {
    let Some(pool) = pool().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let app_id = Uuid::new_v4();
    let deps = deps(pool.clone());
    let sub = Subscription {
        tenant_id: tenant.clone(),
        org: "acme".into(),
        app_id,
        function: "remind".into(),
    };
    let rows = || async {
        sqlx::query_as::<_, (String, String, String, i32)>(
            "SELECT trigger, code, message, failures FROM app_function_failure \
             WHERE tenant_id = $1 AND app_id = $2",
        )
        .bind(&tenant)
        .bind(app_id)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let body = async {
        // インストールが無い: 記録しない。
        run_service(&deps, &sub, serde_json::json!({}), FunctionTrigger::Event).await;
        assert!(rows().await.is_empty());

        // B2 の登録が無いインストール: 理由を残し、2 回目は回数を足す（内部の言葉を出さない）。
        app_gateway::AppInstallationStore::new(pool.clone())
            .upsert(
                &ctx(&tenant),
                app_gateway::NewAppInstallation {
                    app_id,
                    app_name: "経費",
                    installed_version: "1.0.0",
                    granted_scopes: &["data.read".to_string()],
                    client_id_b1: None,
                    client_id_b2: None,
                    ai: app_gateway::AiPin::default(),
                    frontend_bundle: None,
                    server_bundle: None,
                    server_spec: None,
                },
            )
            .await
            .expect("install");
        run_service(&deps, &sub, serde_json::json!({}), FunctionTrigger::Event).await;
        run_service(&deps, &sub, serde_json::json!({}), FunctionTrigger::Cron).await;
        let got = rows().await;
        assert_eq!(got.len(), 1, "{got:?}");
        let (trigger, code, message, failures) = &got[0];
        assert_eq!(
            (trigger.as_str(), code.as_str(), *failures),
            ("cron", "not_configured", 2)
        );
        assert!(message.contains("入れ直して"), "{message}");
        for word in ["B2", "secret", "server_bundle", "client"] {
            assert!(!message.contains(word), "{message}");
        }

        // 成功したら消す。
        app_platform::FunctionFailureStore::new(pool.clone())
            .record_success(&tenant, app_id, "remind")
            .await
            .expect("success");
        assert!(rows().await.is_empty());
    };
    let outcome = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(body)).await;
    for sql in [
        "DELETE FROM app_function_failure WHERE tenant_id = $1",
        "DELETE FROM app_installation WHERE tenant_id = $1",
    ] {
        sqlx::query(sql)
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect(sql);
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
