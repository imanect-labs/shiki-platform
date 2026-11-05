//! スケジューラ／イベントトリガの結合テスト（Task 10.3 受け入れ条件・実 Postgres）。
//!
//! - 複数インスタンス競走でもスケジュールは 1 回だけ発火（occurrence UNIQUE）
//! - enqueue 直後クラッシュ→再 tick でも同一 occurrence の run は 1 つ（冪等）
//! - storage 書込イベントでワークフローが起動する（候補の祖先束縛・scope の形・filter）
//! - 無効化済みワークフローのトリガは発火しない
//! - filter の評価エラー（大小比較の型不一致）では発火しない（fail-closed）
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
    async fn launch(
        &self,
        _t: &str,
        _w: Uuid,
        _k: &str,
        _tid: &str,
        _payload: &serde_json::Value,
        _event_id: Option<i64>,
    ) -> workflow_engine::LaunchOutcome {
        self.launches.fetch_add(1, Ordering::SeqCst);
        workflow_engine::LaunchOutcome::Started(Uuid::new_v4())
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

/// ファイル保存トリガを relay と同じ手順で照合・発火する（候補 → filter → 発火）。発火した数を返す。
///
/// workflow プリンシパルの閲覧の確認（D-50）と新しい版のまとめ（D-38）は api の relay が行う
/// （`workflow_files_it`）。ここでは候補の祖先束縛・scope の形・filter だけを確かめる。
async fn match_file_event(
    store: &SchedulerStore,
    tenant: &str,
    event_id: i64,
    folder: Option<Uuid>,
    payload: &serde_json::Value,
    launcher: &dyn RunLauncher,
) -> usize {
    let mut fired = 0;
    for c in store.file_trigger_candidates(tenant, folder).await.unwrap() {
        if !c.admits(payload) {
            continue;
        }
        let out = store
            .fire_event_trigger(&c.trigger, event_id, payload, None, launcher)
            .await
            .unwrap();
        if let workflow_engine::FireOutcome::Fired(_) = out {
            fired += 1;
        }
    }
    fired
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

/// 受け取った payload と、起動の間に読める予定の時刻を記録する launcher。
#[derive(Default)]
struct RecordingLauncher {
    seen: std::sync::Mutex<Vec<(serde_json::Value, Option<chrono::DateTime<Utc>>)>>,
}

#[async_trait]
impl RunLauncher for RecordingLauncher {
    async fn launch(
        &self,
        _t: &str,
        _w: Uuid,
        _k: &str,
        _tid: &str,
        payload: &serde_json::Value,
        _event_id: Option<i64>,
    ) -> workflow_engine::LaunchOutcome {
        let at = workflow_engine::scheduler::current_scheduled_at();
        self.seen.lock().unwrap().push((payload.clone(), at));
        workflow_engine::LaunchOutcome::Started(Uuid::new_v4())
    }
}

/// スケジュールの発火は、起動口の payload を今までどおり null で渡し（トレイトの約束を変えない）、予定の時刻は
/// 起動の間だけ読める（実行情報の予定の日時・GAP-15）。起動の外では読めない。
#[tokio::test]
async fn schedule_launch_keeps_a_null_payload_and_exposes_the_planned_time() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-at",
        "schedule",
        None,
        json!({ "cron": "* * * * *", "tz": "UTC", "catchup": "skip" }),
    )
    .await;
    sqlx::query("UPDATE workflow_trigger SET last_planned_at = $2 WHERE tenant_id = $1")
        .bind(&tenant)
        .bind(Utc.with_ymd_and_hms(2026, 7, 7, 0, 0, 0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let launcher = RecordingLauncher::default();
    let fired = SchedulerStore::new(pool.clone())
        .tick_schedules(
            Utc.with_ymd_and_hms(2026, 7, 7, 0, 3, 30).unwrap(),
            Some(&tenant),
            &launcher,
        )
        .await;
    for table in [
        "schedule_occurrence",
        "workflow_trigger",
        "workflow_registration",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(fired.unwrap(), 1);
    let seen = launcher.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![(
            serde_json::Value::Null,
            Some(Utc.with_ymd_and_hms(2026, 7, 7, 0, 3, 0).unwrap())
        )]
    );
    assert_eq!(workflow_engine::scheduler::current_scheduled_at(), None);
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

/// catchup=none（#904）: 稼働中の時刻どおりの occurrence は発火し、スケジューラ停止中に時刻を過ぎた
/// occurrence（猶予を超えて遅れたもの）は捨てる（watermark だけ前進）。
#[tokio::test]
async fn catchup_none_fires_on_time_and_drops_misfires() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-none",
        "schedule",
        None,
        json!({ "cron": "0 * * * *", "tz": "UTC", "catchup": "none" }),
    )
    .await;
    let set_watermark = |at| {
        let pool = pool.clone();
        let tenant = tenant.clone();
        async move {
            sqlx::query("UPDATE workflow_trigger SET last_planned_at = $2 WHERE tenant_id = $1")
                .bind(&tenant)
                .bind(at)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });

    // ① 稼働中: 前回の tick（00:59:55）から 01:00:03 の tick までに 01:00 が到来 → 時刻どおりに発火する。
    set_watermark(Utc.with_ymd_and_hms(2026, 7, 7, 0, 59, 55).unwrap()).await;
    let fired = store
        .tick_schedules(
            Utc.with_ymd_and_hms(2026, 7, 7, 1, 0, 3).unwrap(),
            Some(&tenant),
            launcher.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(
        fired, 1,
        "時刻どおりの occurrence は catchup=none でも発火する"
    );

    // ② 停止明け: 01:00 の後に止まり 03:30 に再開 → 02:00・03:00 は時刻を大きく過ぎているので捨てる。
    set_watermark(Utc.with_ymd_and_hms(2026, 7, 7, 1, 0, 3).unwrap()).await;
    let fired = store
        .tick_schedules(
            Utc.with_ymd_and_hms(2026, 7, 7, 3, 30, 0).unwrap(),
            Some(&tenant),
            launcher.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(fired, 0, "停止中に過ぎた occurrence は捨てる（misfire）");
    let watermark: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT last_planned_at FROM workflow_trigger WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        watermark,
        Utc.with_ymd_and_hms(2026, 7, 7, 3, 30, 0).unwrap(),
        "捨てた分も watermark で消化する"
    );

    // ③ 再開後の次の時刻（04:00）は再び時刻どおりに発火する。
    let fired = store
        .tick_schedules(
            Utc.with_ymd_and_hms(2026, 7, 7, 4, 0, 2).unwrap(),
            Some(&tenant),
            launcher.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(fired, 1);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);

    // ④ 稼働中だが tick ループが重く間が空いた（04:59:58 → 05:01:05・05:00 から 65 秒遅れ）: 停止ではないので発火する。
    set_watermark(Utc.with_ymd_and_hms(2026, 7, 7, 4, 59, 58).unwrap()).await;
    let fired = store
        .tick_schedules(
            Utc.with_ymd_and_hms(2026, 7, 7, 5, 1, 5).unwrap(),
            Some(&tenant),
            launcher.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(
        fired, 1,
        "遅い tick でも動き続けていたなら時刻どおりとして発火する"
    );
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 3);
}

/// 発火は曜日を標準の読み方（0 と 7 が日曜、1 が月曜）で数える。画面の「毎週・月曜日」（`MON`）と保存済みの
/// `1` は月曜に、`0` は日曜に発火する（cron クレートの番号で読むと 1 が日曜だった・BUG-95）。
#[tokio::test]
async fn schedule_fires_on_the_standard_weekday() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });
    // 2026-10-25 は日曜。土曜 12:00 から月曜 12:00 までに、どの曜日の 9:00 で発火したか。
    let saturday = Utc.with_ymd_and_hms(2026, 10, 24, 12, 0, 0).unwrap();
    let sunday_noon = Utc.with_ymd_and_hms(2026, 10, 25, 12, 0, 0).unwrap();
    let monday_noon = Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
    let cases = [
        ("1", "trg-mon-1"),
        ("MON", "trg-mon-name"),
        ("0", "trg-sun-0"),
        ("7", "trg-sun-7"),
    ];
    for (dow, trigger) in cases {
        register(
            &pool,
            &tenant,
            Uuid::new_v4(),
            "enabled",
            trigger,
            "schedule",
            None,
            json!({ "cron": format!("0 9 * * {dow}"), "tz": "UTC", "catchup": "all" }),
        )
        .await;
    }
    sqlx::query("UPDATE workflow_trigger SET last_planned_at = $2 WHERE tenant_id = $1")
        .bind(&tenant)
        .bind(saturday)
        .execute(&pool)
        .await
        .unwrap();
    let on_sunday = store
        .tick_schedules(sunday_noon, Some(&tenant), launcher.as_ref())
        .await
        .unwrap();
    let on_monday = store
        .tick_schedules(monday_noon, Some(&tenant), launcher.as_ref())
        .await
        .unwrap();
    let fired: Vec<(String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT trigger_id, scheduled_at FROM schedule_occurrence WHERE tenant_id = $1 \
         ORDER BY scheduled_at, trigger_id",
    )
    .bind(&tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    for table in [
        "schedule_occurrence",
        "workflow_trigger",
        "workflow_registration",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .unwrap();
    }
    let sunday_9 = Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap();
    let monday_9 = Utc.with_ymd_and_hms(2026, 10, 26, 9, 0, 0).unwrap();
    assert_eq!((on_sunday, on_monday), (2, 2));
    assert_eq!(
        fired,
        [
            ("trg-sun-0".to_string(), sunday_9),
            ("trg-sun-7".to_string(), sunday_9),
            ("trg-mon-1".to_string(), monday_9),
            ("trg-mon-name".to_string(), monday_9),
        ]
    );
}

/// フォルダノード＋自己 closure（depth 0）を作る。`parent` を渡すと祖先 closure も張る。
async fn mk_folder(pool: &PgPool, tenant: &str, id: Uuid, parent: Option<Uuid>) {
    sqlx::query(
        "INSERT INTO node (id, org, tenant_id, kind, name, parent_id, created_by, updated_by) \
         VALUES ($1, 'acme', $2, 'folder', $3, $4, 'alice', 'alice')",
    )
    .bind(id)
    .bind(tenant)
    .bind(id.to_string())
    .bind(parent)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO node_closure (org, tenant_id, ancestor, descendant, depth) \
         VALUES ('acme', $1, $2, $2, 0)",
    )
    .bind(tenant)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    if let Some(p) = parent {
        // p の全祖先 → id への closure を depth+1 で張る（自己含む）。
        sqlx::query(
            "INSERT INTO node_closure (org, tenant_id, ancestor, descendant, depth) \
             SELECT 'acme', $1, c.ancestor, $2, c.depth + 1 FROM node_closure c \
             WHERE c.tenant_id = $1 AND c.descendant = $3",
        )
        .bind(tenant)
        .bind(id)
        .bind(p)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn storage_write_event_triggers_run_once() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    // フォルダ階層: parent > child。トリガは parent に束縛（祖先束縛で child のイベントも拾う）。
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let unrelated = Uuid::new_v4();
    mk_folder(&pool, &tenant, parent, None).await;
    mk_folder(&pool, &tenant, child, Some(parent)).await;
    mk_folder(&pool, &tenant, unrelated, None).await;

    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-e",
        "event",
        Some("storage.write"),
        json!({ "scope": { "folder": parent.to_string() } }),
    )
    .await;
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });
    let payload = json!({ "parent_id": child.to_string() });

    // 祖先束縛: サブフォルダ（child）配下のイベントが parent 束縛トリガで発火する。
    let fired = match_file_event(
        &store,
        &tenant,
        42,
        Some(child),
        &payload,
        launcher.as_ref(),
    )
    .await;
    assert_eq!(fired, 1, "サブフォルダ配下の書込が祖先束縛トリガで起動する");

    // 同一 event_id は再発火しない（outbox 1 件 1 run）。
    let again = match_file_event(
        &store,
        &tenant,
        42,
        Some(child),
        &payload,
        launcher.as_ref(),
    )
    .await;
    assert_eq!(again, 0);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

    // 祖先関係にないフォルダのイベントはマッチしない。
    let other = match_file_event(
        &store,
        &tenant,
        43,
        Some(unrelated),
        &json!({ "parent_id": unrelated.to_string() }),
        launcher.as_ref(),
    )
    .await;
    assert_eq!(other, 0);
}

#[tokio::test]
async fn event_trigger_with_non_folder_scope_never_fires() {
    // fail-closed: folder キーを持たない scope のトリガは全購読に縮退せず一切発火しない（Codex P1）。
    // （保存時 V3 は { "folder": "<uuid>" } を強制するが、DB 直書き等の不正形状も実行時に閉じる。）
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let folder = Uuid::new_v4();
    mk_folder(&pool, &tenant, folder, None).await;
    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-badscope",
        "event",
        Some("storage.write"),
        json!({ "scope": { "table": "expense" } }),
    )
    .await;
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });
    let fired = match_file_event(
        &store,
        &tenant,
        77,
        Some(folder),
        &json!({ "parent_id": folder.to_string() }),
        launcher.as_ref(),
    )
    .await;
    assert_eq!(fired, 0, "非 folder scope は発火しない（fail-closed）");
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
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

/// engine.md §5.6: filter の評価エラー（数値と文字列の大小比較・BUG-14）は発火しない（fail-closed）。
/// 型が合えば同じ filter で発火する。
#[tokio::test]
async fn event_trigger_filter_type_error_does_not_fire() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4());
    let wf = Uuid::new_v4();
    let folder = Uuid::new_v4();
    mk_folder(&pool, &tenant, folder, None).await;
    register(
        &pool,
        &tenant,
        wf,
        "enabled",
        "trg-filter",
        "event",
        Some("storage.write"),
        json!({ "scope": { "folder": folder.to_string() },
                "filter": { "cmp": { "left": { "$from": "trigger", "path": "/size" },
                                     "op": "gt", "right": "10" } } }),
    )
    .await;
    let store = SchedulerStore::new(pool.clone());
    let launcher = Arc::new(CountingLauncher {
        launches: AtomicUsize::new(0),
    });
    let fired = match_file_event(
        &store,
        &tenant,
        91,
        Some(folder),
        &json!({ "size": 12 }),
        launcher.as_ref(),
    )
    .await;
    assert_eq!(fired, 0, "数値と文字列の大小比較は発火しない");
    // 文字列どうしなら比べられる（"12" > "10"）。
    let fired = match_file_event(
        &store,
        &tenant,
        92,
        Some(folder),
        &json!({ "size": "12" }),
        launcher.as_ref(),
    )
    .await;
    assert_eq!(fired, 1);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
}
