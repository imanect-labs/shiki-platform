//! アプリ利用量の集計（Task 9.15・`GET /apps/{id}/usage`）。
//!
//! 能力呼び出し（`app_capability_usage`・app-gateway）と AI（`llm_usage`・llm-gateway）を
//! (ユーザー×アプリ) で束ねて返す。請求/クォータの供給元。呼び出し元 API は artifact owner を
//! 要求する（インストール管理と同じ owner ReBAC）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use llm_gateway::accounting::{AppLlmUsage, UsageRecorder};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::AppPlatformError;

/// アプリ利用量の応答（capability＋llm）。
#[derive(Debug, Serialize)]
pub struct AppUsage {
    pub app_id: Uuid,
    /// 能力呼び出し（user×capability×day）。
    pub capabilities: Vec<CapabilityUsageRow>,
    /// AI 利用（user×model・calls/tokens/cost）。
    pub llm: Vec<LlmUsageRow>,
}

#[derive(Debug, Serialize)]
pub struct CapabilityUsageRow {
    pub user_sub: String,
    pub capability: String,
    pub day: String,
    pub calls: i64,
}

#[derive(Debug, Serialize)]
pub struct LlmUsageRow {
    pub user_sub: String,
    pub model: String,
    pub calls: i64,
    pub tokens: i64,
    pub cost_usd_micros: i64,
}

impl From<AppLlmUsage> for LlmUsageRow {
    fn from(u: AppLlmUsage) -> Self {
        LlmUsageRow {
            user_sub: u.user_sub,
            model: u.model,
            calls: u.calls,
            tokens: u.tokens,
            cost_usd_micros: u.cost_usd_micros,
        }
    }
}

/// 利用量集計の単一チョークポイント。
#[derive(Clone)]
pub struct AppUsageStore {
    db: PgPool,
    authz: Arc<dyn AuthzClient>,
    llm: UsageRecorder,
}

impl AppUsageStore {
    pub fn new(db: PgPool, authz: Arc<dyn AuthzClient>) -> Self {
        let llm = UsageRecorder::new(db.clone());
        AppUsageStore { db, authz, llm }
    }

    /// アプリの利用量を集計する（artifact owner のみ）。
    pub async fn app_usage(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<AppUsage, AppPlatformError> {
        let obj = ctx.ns().artifact(&app_id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Owner,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| AppPlatformError::Internal(format!("authz: {e}")))?;
        if !ok {
            return Err(AppPlatformError::Forbidden);
        }
        let capabilities = app_gateway::fetch_usage(&self.db, &ctx.tenant_id, app_id, 1000)
            .await
            .map_err(|e| AppPlatformError::Internal(format!("capability usage: {e}")))?
            .into_iter()
            .map(|u| CapabilityUsageRow {
                user_sub: u.user_sub,
                capability: u.capability,
                day: u.day.to_string(),
                calls: u.calls,
            })
            .collect();
        let llm = self
            .llm
            .app_llm_usage(ctx, app_id)
            .await
            .map_err(|e| AppPlatformError::Internal(format!("llm usage: {e}")))?
            .into_iter()
            .map(Into::into)
            .collect();
        Ok(AppUsage {
            app_id,
            capabilities,
            llm,
        })
    }
}
