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
//!
//! 自動実行の失敗（シークレット・トークン・実行の誤り・関数の失敗）は、作成者に見せてよい文で関数ごとに残し、
//! 次に成功したら消す（[`app_platform::FunctionFailureStore`]・GAP-60 の残り）。内部の文はサーバーのログにだけ書く。

use std::sync::Arc;

use app_platform::{
    FunctionActor, FunctionFailure, FunctionFailureStore, FunctionInvocation, FunctionRunner,
    FunctionTrigger,
};
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
    /// outbox の claim の走査の下限（アイドルの間は走査しない・3 回目の検証 3-D-1）。
    pub claim_hints: storage::event::ClaimHints,
}

/// event/cron トリガのループを spawn する（gateway 有効時のみ呼ばれる）。
pub(crate) fn spawn_miniapp_triggers(deps: TriggerDeps) {
    let deps = Arc::new(deps);
    let event_deps = Arc::clone(&deps);
    tokio::spawn(async move {
        // 台帳コンシューマ登録（初回のみ効果・冪等）。登録と fast-forward の原子性が要るので
        // pool 版（自前で txn を張る）を使う。
        if let Err(e) =
            storage::event::register_consumer_on_pool(&event_deps.db, "miniapp-functions").await
        {
            tracing::error!(error = %e, "miniapp-functions consumer 登録に失敗");
        }
        loop {
            match event_tick(&event_deps).await {
                // 溜まっていれば間を空けずに続ける（構造化データのレコード変更も outbox に乗る・
                // 一括取込の直後に 5 秒 50 件では GC と他 consumer を長く止める・Task 15.6）。
                Ok(n) if n >= EVENT_BATCH => continue,
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "miniapp event トリガの処理に失敗"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    tokio::spawn(async move {
        // 曜日の読み方を揃える前に計算した次の実行の時刻を、今の読み方で計算し直す（BUG-129）。
        match realign_schedules(&deps.db, chrono::Utc::now()).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                realigned = n,
                "miniapp cron の次の実行の時刻を計算し直しました"
            ),
            Err(e) => tracing::warn!(error = %e, "miniapp cron の次の実行の時刻の計算し直しに失敗"),
        }
        loop {
            if let Err(e) = cron_tick(&deps).await {
                tracing::warn!(error = %e, "miniapp cron トリガの処理に失敗");
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
    tracing::info!("miniapp event/cron トリガを起動しました");
}

#[derive(sqlx::FromRow, Clone)]
struct Subscription {
    tenant_id: String,
    org: String,
    app_id: Uuid,
    function: String,
}

/// 1 回に claim する outbox の件数（claim は配送済みの行を走査するので、1 回の件数を大きくして回数を減らす・
/// 一括取込の 10 万件で 50 件ずつだと追いつくまで数十分かかった・検証 D-14）。
const EVENT_BATCH: usize = 500;

async fn event_tick(deps: &TriggerDeps) -> anyhow::Result<usize> {
    let mut tx = deps.db.begin().await?;
    let events = storage::event::claim_undelivered_hinted(
        &mut tx,
        &deps.claim_hints,
        "miniapp-functions",
        "miniapp-functions",
        EVENT_BATCH as i64,
    )
    .await?;
    if events.is_empty() {
        return Ok(0);
    }
    let claimed = events.len();
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    // 突合対象（event_type 付きのみ）を集めてから ack（at-least-once の台帳→起動は at-most-once）。
    let mut dispatches: Vec<(Subscription, serde_json::Value, String)> = Vec::new();
    // 購読の照会はバッチ内で (テナント, event_type) ごとに 1 回（一括取込の直後は同じ組のイベントが
    // 数万件並ぶ・検証 D-14）。
    let mut subs_of: std::collections::HashMap<(String, String), Vec<Subscription>> =
        std::collections::HashMap::new();
    for event in &events {
        let Some(event_type) = event.payload.get("event_type").and_then(|v| v.as_str()) else {
            continue;
        };
        let key = (event.tenant_id.clone(), event_type.to_string());
        if !subs_of.contains_key(&key) {
            let found: Vec<Subscription> = sqlx::query_as(
                "SELECT tenant_id, org, app_id, function FROM app_event_subscription \
                 WHERE tenant_id = $1 AND event_type = $2",
            )
            .bind(&event.tenant_id)
            .bind(event_type)
            .fetch_all(&mut *tx)
            .await?;
            subs_of.insert(key.clone(), found);
        }
        let subs = subs_of.get(&key).cloned().unwrap_or_default();
        for sub in subs {
            // 越境防止: payload が他アプリのリソース（table_id/app_id）を指す場合は配送しない
            // （event_type だけでの購読が他アプリのイベント payload を受け取るのを防ぐ・SSE 経路と同等）。
            if !event_targets_app(&mut tx, &event.tenant_id, sub.app_id, &event.payload).await? {
                continue;
            }
            dispatches.push((sub, event.payload.clone(), event_type.to_string()));
        }
    }
    storage::event::mark_delivered(&mut tx, "miniapp-functions", &ids).await?;
    tx.commit().await?;

    for (sub, payload, event_type) in dispatches {
        tracing::debug!(app_id = %sub.app_id, function = %sub.function, event_type, "event で関数を起動");
        run_service(deps, &sub, payload, FunctionTrigger::Event).await;
    }
    Ok(claimed)
}

/// イベント payload が購読アプリ自身のリソースを指すか（越境配送の防止）。
///
/// `table_id` があれば当該アプリ所有テーブル（`data_table.app_id`）に限定し、`app_id` があれば
/// 一致必須。いずれも無い汎用イベントは購読どおり配送する。
async fn event_targets_app(
    tx: &mut sqlx::PgConnection,
    tenant_id: &str,
    app_id: Uuid,
    payload: &serde_json::Value,
) -> anyhow::Result<bool> {
    let as_uuid =
        |k: &str| -> Option<Uuid> { payload.get(k).and_then(|v| v.as_str())?.parse().ok() };
    if let Some(table_id) = as_uuid("table_id") {
        let owned: Option<(bool,)> = sqlx::query_as(
            "SELECT true FROM data_table WHERE tenant_id = $1 AND app_id = $2 AND id = $3 LIMIT 1",
        )
        .bind(tenant_id)
        .bind(app_id)
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await?;
        return Ok(owned.is_some());
    }
    if let Some(payload_app) = as_uuid("app_id") {
        return Ok(payload_app == app_id);
    }
    Ok(true)
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
        // 読めない式（標準の読み方で写せない曜日など・BUG-129）は実行しない。以前は 1 時間後へ送り、毎時に実行して
        // いた。式は再インストールまで変わらないので、確かめ直すのは 1 日後にする。
        let next = match app_platform::next_cron_run_after(&s.expr, chrono::Utc::now()) {
            Ok(next) => next,
            Err(e) => {
                tracing::warn!(error = %e, schedule = %s.id, app_id = %s.app_id,
                    "cron の式を読めないため、関数を実行しません");
                sqlx::query("UPDATE app_function_schedule SET next_run_at = $2 WHERE id = $1")
                    .bind(s.id)
                    .bind(chrono::Utc::now() + chrono::Duration::days(1))
                    .execute(&mut *tx)
                    .await?;
                continue;
            }
        };
        // 期限の来た時刻が、今の読み方で式に当たる時刻かを確かめる（BUG-129）。曜日を標準の読み方にする前に計算した
        // 時刻や、ローリング更新の間に古い版のリーダーが書き戻した時刻は当たらない（曜日が 1 日ずれる）ので、実行せず
        // 次の時刻を計算し直すだけにする。
        let hits = app_platform::next_cron_run_after(
            &s.expr,
            s.next_run_at - chrono::Duration::seconds(1),
        )
        .is_ok_and(|at| at == s.next_run_at);
        if !hits {
            tracing::info!(schedule = %s.id, app_id = %s.app_id, at = %s.next_run_at, %next,
                "式に当たらない時刻のため実行せず、次の時刻を計算し直します");
            sqlx::query("UPDATE app_function_schedule SET next_run_at = $2 WHERE id = $1")
                .bind(s.id)
                .bind(next)
                .execute(&mut *tx)
                .await?;
            continue;
        }
        // (schedule_id, scheduled_at) 一意＝リーダー交代/再起動でも同一時刻の二重起動なし。
        let inserted = sqlx::query(
            "INSERT INTO app_function_run (tenant_id, schedule_id, scheduled_at) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(&s.tenant_id)
        .bind(s.id)
        .bind(s.next_run_at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        // tenant-sql: allow(全テナント横断の cron 次回時刻更新（id は直前の横断走査から）)
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
        run_service(
            deps,
            &sub,
            serde_json::json!({ "trigger": "cron" }),
            FunctionTrigger::Cron,
        )
        .await;
    }
    Ok(())
}

/// まだ来ていない実行の時刻（`next_run_at > now`）を、今の読み方で計算し直す（起動時・BUG-129）。
///
/// 曜日を標準の読み方（1 が月曜・0 と 7 が日曜）で読むようにする前に保存した時刻は、cron クレートの読み方
/// （1 が日曜）で計算してあり、そのままだと次の 1 回だけ 1 日ずれた曜日に動く。期限の来た行は次の tick が、式に
/// 当たる時刻かを確かめてから実行し（当たらなければ実行せずに計算し直す）、その後の時刻を今の読み方で求めるので
/// 触らない。読めない式は tick に任せる（実行せずに送る）。同じ読み方なら
/// 値は変わらないので、起動のたびに流してよい。tick と取り違えないよう、読んだ時刻のままの行だけを書き換える。
/// 戻り値 = 書き換えた行の数。
async fn realign_schedules(db: &PgPool, now: chrono::DateTime<chrono::Utc>) -> anyhow::Result<u64> {
    let rows: Vec<(Uuid, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, expr, next_run_at FROM app_function_schedule WHERE next_run_at > $1",
    )
    .bind(now)
    .fetch_all(db)
    .await?;
    let mut changed = 0;
    for (id, expr, at) in rows {
        let Ok(next) = app_platform::next_cron_run_after(&expr, now) else {
            continue;
        };
        if next == at {
            continue;
        }
        changed += sqlx::query(
            "UPDATE app_function_schedule SET next_run_at = $3 \
             WHERE id = $1 AND next_run_at = $2 AND next_run_at > now()",
        )
        .bind(id)
        .bind(at)
        .bind(next)
        .execute(db)
        .await?
        .rows_affected();
    }
    Ok(changed)
}

/// 自動実行 1 回の結果（[`invoke_service`]）。
#[derive(Debug)]
enum ServiceRun {
    /// インストールが無い・アンインストールした（記録しない）。
    NotInstalled,
    /// 関数が成功した。
    Succeeded,
    /// 失敗した（作成者に見せてよい理由と文。内部の文はログにだけ書いた）。
    Failed(FunctionFailure),
}

/// 自動実行 1 回を動かし、結果を残す（失敗は関数ごとに直近の 1 件・成功したら消す・GAP-60）。
///
/// 記録の失敗は自動実行を止めない（ログに残す）。
async fn run_service(
    deps: &TriggerDeps,
    sub: &Subscription,
    payload: serde_json::Value,
    trigger: FunctionTrigger,
) {
    let store = FunctionFailureStore::new(deps.db.clone());
    let recorded = match invoke_service(deps, sub, payload).await {
        Ok(ServiceRun::Succeeded) => {
            store
                .record_success(&sub.tenant_id, sub.app_id, &sub.function)
                .await
        }
        Ok(ServiceRun::Failed(failure)) => {
            tracing::warn!(app_id = %sub.app_id, function = %sub.function, trigger = trigger.as_str(),
                code = %failure.code, message = %failure.message, "自動実行の関数が失敗");
            store
                .record_failure(
                    &sub.tenant_id,
                    &sub.org,
                    sub.app_id,
                    &sub.function,
                    trigger,
                    &failure,
                )
                .await
        }
        Ok(ServiceRun::NotInstalled) => {
            tracing::info!(app_id = %sub.app_id, function = %sub.function,
                "インストールが無いため、自動実行しません");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, app_id = %sub.app_id, function = %sub.function,
                trigger = trigger.as_str(), "自動実行の準備に失敗");
            Ok(())
        }
    };
    if let Err(e) = recorded {
        tracing::warn!(error = %e, app_id = %sub.app_id, function = %sub.function,
            "自動実行の結果を残せません");
    }
}

/// 自動実行の失敗（作成者に見せてよい文・やり直しても通らないものは `retryable` を立てない）。
fn failed(code: &str, message: &str, retryable: bool) -> ServiceRun {
    ServiceRun::Failed(FunctionFailure {
        code: code.into(),
        message: message.into(),
        retryable,
    })
}

/// service identity（B2 client_credentials）で関数を実行する。
///
/// `Err` はインストールを引けないとき（DB の失敗）だけ。そのほかの失敗は、作成者に見せてよい文の
/// [`ServiceRun::Failed`] にし、内部の文はここでログに書く。
async fn invoke_service(
    deps: &TriggerDeps,
    sub: &Subscription,
    payload: serde_json::Value,
) -> anyhow::Result<ServiceRun> {
    // インストール時ピン（server_bundle/spec/client）を解決する（失効済みは起動しない）。
    let installation = deps
        .port
        .runner_installation(&sub.tenant_id, sub.app_id)
        .await?;
    let Some(installation) = installation else {
        return Ok(ServiceRun::NotInstalled);
    };
    let (Some(server_bundle), Some(client_id_b2)) = (
        installation.server_bundle.clone(),
        installation.client_id_b2.clone(),
    ) else {
        return Ok(failed(
            "not_configured",
            "このアプリには、サーバーで動く処理の登録がないため、自動実行できませんでした。アプリを入れ直してください。",
            false,
        ));
    };
    if deps.port.secrets.is_none() {
        return Ok(failed(
            "not_configured",
            "この環境では、アプリを自動で動かすためのシークレットを保管していません。管理者に確かめてください。",
            false,
        ));
    }
    // service ctx（監査 actor 用・org/tenant は購読行から。購読行の tenant_id も文字ポリシーを通す）。
    let tenant = authz::TenantId::parse(sub.tenant_id.as_str())
        .map_err(|e| anyhow::anyhow!("購読の tenant_id が不正です: {e}"))?;
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
        tenant,
    );
    let secret = match deps.port.resolve_b2_secret(&ctx, sub.app_id).await {
        Ok(secret) => secret,
        Err(e) => {
            tracing::warn!(error = %e, app_id = %sub.app_id, "自動実行の B2 secret を読めない");
            return Ok(failed(
                "secret_unavailable",
                "アプリが自動で動くためのシークレット（API キーなどの秘密の値）を読めませんでした。アプリを入れ直してください。",
                false,
            ));
        }
    };
    // 同意付与スコープを service token に明示要求する（optional client scope のため）。
    let token = match deps
        .port
        .client_credentials_token(&client_id_b2, &secret, &installation.granted_scopes)
        .await
    {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!(error = %e, app_id = %sub.app_id, "自動実行のトークンを取れない");
            return Ok(match e {
                app_gateway::GatewayError::Forbidden(_) => failed(
                    "token_rejected",
                    "アプリの登録が止められているか見つからないため、自動実行できませんでした。管理者に確かめてください。",
                    false,
                ),
                _ => failed(
                    "token_unavailable",
                    "ログインを確かめる仕組みにつながらないため、自動実行できませんでした。続くときは管理者に確かめてください。",
                    true,
                ),
            });
        }
    };
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
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!(error = %e, app_id = %sub.app_id, function = %sub.function,
                "自動実行の関数を動かせない");
            return Ok(match e {
                app_platform::AppPlatformError::NotFound => failed(
                    "code_missing",
                    "アプリのサーバーで動く処理のコードが見つからないため、自動実行できませんでした。アプリを入れ直してください。",
                    false,
                ),
                app_platform::AppPlatformError::Invalid(_) => failed(
                    "code_invalid",
                    "アプリのサーバーで動く処理のコードを読み込めないため、自動実行できませんでした。直した版を出して、入れ直してください。",
                    false,
                ),
                _ => failed(
                    "internal",
                    "サーバーで問題が起きたため、自動実行できませんでした。続くときは管理者に確かめてください。",
                    true,
                ),
            });
        }
    };
    Ok(match outcome.error {
        Some(error) if !outcome.ok => ServiceRun::Failed(error),
        _ if !outcome.ok => failed("internal", "関数の実行中に問題が起きました。", false),
        _ => ServiceRun::Succeeded,
    })
}

#[cfg(test)]
#[path = "miniapp_triggers_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "miniapp_b2_tests.rs"]
mod b2_tests;
