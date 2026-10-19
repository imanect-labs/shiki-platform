//! AI 活用分析の定期実行（#751）。
//!
//! 一定間隔で、全組織のロールアップを進め（[`super::rollup`]）、分類がオンの組織の会話を
//! 分類する（[`super::classify`]）。複数インスタンスで同時に走らないよう、実行全体を
//! セッション advisory lock で囲む（取れなければその回は何もしない）。
//! 失敗はログに残して次の回に任せる（各日の集計は作り直しなので取りこぼしは次回で埋まる）。

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use llm_gateway::LlmGateway;
use sqlx::PgPool;

use super::{classify, rollup, settings, InsightsConfig};

/// advisory lock の鍵（このジョブ専用の固定値）。
const LOCK_KEY: i64 = 751_000_751;
/// 実行間隔の下限。
const MIN_INTERVAL: Duration = Duration::from_mins(1);

/// 今日（日本時間）。
pub fn today_jst() -> chrono::NaiveDate {
    (Utc::now() + chrono::Duration::hours(9)).date_naive()
}

/// 1 回分を実行する（ロックが取れなければ `Ok(false)`）。
pub async fn run_once(
    db: &PgPool,
    llm: Option<&LlmGateway>,
    config: &InsightsConfig,
) -> Result<bool, sqlx::Error> {
    // ロックとアンロックを同じ接続で行う（セッションロックは接続に紐づく）。
    let mut conn = db.acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(false);
    }
    let result = run_locked(db, llm, config).await;
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *conn)
        .await;
    result.map(|()| true)
}

async fn run_locked(
    db: &PgPool,
    llm: Option<&LlmGateway>,
    config: &InsightsConfig,
) -> Result<(), sqlx::Error> {
    let today = today_jst();
    for (tenant_id, org) in rollup::orgs(db).await? {
        // 分類を先に（その日の用途ラベルが集計に間に合うように）。
        if let Some(gw) = llm {
            let s = settings::load(db, &tenant_id, &org).await?;
            if s.classify_enabled {
                match classify::classify_org(
                    db,
                    gw,
                    &tenant_id,
                    &org,
                    config.classify_batch,
                    config.classifier_model.as_deref(),
                )
                .await
                {
                    Ok(n) if n > 0 => tracing::info!(org, n, "insights: 会話を分類"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(org, error = %e, "insights: 分類に失敗"),
                }
            }
        }
        if let Err(e) = rollup::rollup_org(db, &tenant_id, &org, today).await {
            tracing::warn!(org, error = %e, "insights: ロールアップに失敗");
        }
    }
    Ok(())
}

/// 定期実行を起動する（`enabled=false` なら何もしない）。
pub fn spawn(db: PgPool, llm: Option<Arc<LlmGateway>>, config: InsightsConfig) {
    if !config.enabled {
        tracing::info!("insights.enabled=false: AI 活用分析の定期集計は動きません");
        return;
    }
    let interval = Duration::from_secs(config.interval_secs).max(MIN_INTERVAL);
    tokio::spawn(async move {
        // 起動直後の負荷を避けて少し待つ。
        tokio::time::sleep(Duration::from_secs(15)).await;
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match run_once(&db, llm.as_deref(), &config).await {
                Ok(true) => tracing::debug!("insights: 定期集計を実行"),
                Ok(false) => tracing::debug!("insights: 他のインスタンスが実行中"),
                Err(e) => tracing::warn!(error = %e, "insights: 定期集計に失敗"),
            }
        }
    });
}
