//! `GET /org-admin/ai/providers/status` — プロバイダの稼働状況（組織管理者のみ）。
//!
//! 集計（`llm_provider_health`）は全テナントで共有するプロバイダの数字なので、件数は返さず
//! 率と時間だけを返す（件数は他テナントの利用規模を映す）。試行の少ない窓では率と時間も
//! `null` にし、最後の成功は 15 分単位に丸める（llm-gateway の `health::status`）。

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use llm_gateway::health::{HealthReason, HealthState, ProviderHealth};
use serde::Serialize;
use utoipa::ToSchema;

use super::llm_models::gateway;
use crate::{error::ApiError, state::AppState};

/// プロバイダの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthState {
    /// 正常。
    Operational,
    /// 不安定（障害・レート制限・遅延のいずれか）。
    Degraded,
    /// 停止（障害率が高い）。
    Down,
    /// 直近の利用が無く判定できない。
    Idle,
    /// 数えていない（連携者個人のプラン枠）。
    Unmonitored,
}

impl From<HealthState> for ProviderHealthState {
    fn from(s: HealthState) -> Self {
        match s {
            HealthState::Operational => Self::Operational,
            HealthState::Degraded => Self::Degraded,
            HealthState::Down => Self::Down,
            HealthState::Idle => Self::Idle,
        }
    }
}

/// 不安定・停止の理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthReason {
    /// 障害率が高い。
    Errors,
    /// レート制限が続いている。
    RateLimited,
    /// 最初の応答が遅い。
    Slow,
}

impl From<HealthReason> for ProviderHealthReason {
    fn from(r: HealthReason) -> Self {
        match r {
            HealthReason::Errors => Self::Errors,
            HealthReason::RateLimited => Self::RateLimited,
            HealthReason::Slow => Self::Slow,
        }
    }
}

/// 1 時間ぶんの稼働。
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderHealthHour {
    /// 時間の頭（UTC）。
    pub start: DateTime<Utc>,
    pub state: ProviderHealthState,
    /// 稼働率（0〜1・レート制限は除く）。利用が無ければ `null`。
    pub availability: Option<f64>,
}

/// プロバイダ 1 件の稼働状況。
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminProviderHealth {
    pub id: String,
    pub label: String,
    pub state: ProviderHealthState,
    pub reasons: Vec<ProviderHealthReason>,
    /// 判定窓の中の障害率（0〜1）。
    pub failure_rate: Option<f64>,
    /// 判定窓の中の試行に占めるレート制限の割合（0〜1）。
    pub rate_limited_rate: Option<f64>,
    /// 判定窓の中の最初の応答までの時間の p95（ミリ秒・目安）。
    pub ttft_p95_ms: Option<u64>,
    /// 最後に成功した時刻（分単位・直近 7 日の中で）。
    pub last_ok_at: Option<DateTime<Utc>>,
    /// 直近 24 時間の稼働率（0〜1）。
    pub availability_24h: Option<f64>,
    /// 直近 24 時間（古い順・最後が今の時間）。数えていないプロバイダは空。
    pub hours: Vec<ProviderHealthHour>,
}

/// `GET /org-admin/ai/providers/status` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderStatusResponse {
    /// 判定した時刻。
    pub generated_at: DateTime<Utc>,
    /// 今の状態を判定する窓（分）。
    pub window_minutes: u32,
    /// 「不安定」とみなす最初の応答の p95（ミリ秒）。
    pub slow_ttft_p95_ms: u64,
    pub providers: Vec<AdminProviderHealth>,
}

fn to_dto(id: String, label: String, h: Option<ProviderHealth>) -> AdminProviderHealth {
    let Some(h) = h else {
        return AdminProviderHealth {
            id,
            label,
            state: ProviderHealthState::Unmonitored,
            reasons: Vec::new(),
            failure_rate: None,
            rate_limited_rate: None,
            ttft_p95_ms: None,
            last_ok_at: None,
            availability_24h: None,
            hours: Vec::new(),
        };
    };
    AdminProviderHealth {
        id,
        label,
        state: h.state.into(),
        reasons: h.reasons.into_iter().map(Into::into).collect(),
        failure_rate: h.failure_rate,
        rate_limited_rate: h.rate_limited_rate,
        ttft_p95_ms: h.ttft_p95_ms,
        last_ok_at: h.last_ok_at,
        availability_24h: h.availability_24h,
        hours: h
            .hours
            .into_iter()
            .map(|b| ProviderHealthHour {
                start: b.start,
                state: b.state.into(),
                availability: b.availability,
            })
            .collect(),
    }
}

/// 接続済みプロバイダの稼働状況（並びはプロバイダ一覧と同じ）。
#[utoipa::path(
    get,
    path = "/org-admin/ai/providers/status",
    responses(
        (status = 200, description = "稼働状況", body = ProviderStatusResponse),
        (status = 403, description = "組織管理者ではない"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn provider_status(
    State(state): State<AppState>,
) -> Result<Json<ProviderStatusResponse>, ApiError> {
    let gw = gateway(&state)?;
    let now = Utc::now();
    let rows = gw
        .provider_health(now)
        .await
        .map_err(|e| ApiError::Internal(format!("provider health: {e}")))?;
    let t = gw.health_thresholds();
    Ok(Json(ProviderStatusResponse {
        generated_at: now,
        window_minutes: t.window_minutes,
        slow_ttft_p95_ms: t.slow_ttft_p95_ms,
        providers: rows
            .into_iter()
            .map(|(p, h)| to_dto(p.id, p.label, h))
            .collect(),
    }))
}
