//! `GET /llm/models` — チャットで選べるモデルの一覧（一般ユーザー向け・#553）。
//!
//! テナント管理者が絞った範囲（モデルカタログ ∩ テナントで有効）だけを返す。接続先や単価、
//! プロバイダの実モデル ID は返さない（選ぶのに要らない情報を利用者へ出さない）。

use std::sync::Arc;

use axum::{extract::State, routing::get, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    error::ApiError,
    extract::AuthContextExt,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn llm_model_route_decls() -> Vec<RouteDecl> {
    vec![RouteDecl::new(
        "/llm/models",
        &["GET"],
        AccessPolicy::Session,
        || get(list_models),
    )]
}

/// 思考強度（3 段階）。プロバイダ固有のノブは出さない（design §4.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
}

impl From<llm_gateway::Effort> for EffortLevel {
    fn from(e: llm_gateway::Effort) -> Self {
        match e {
            llm_gateway::Effort::Low => EffortLevel::Low,
            llm_gateway::Effort::Medium => EffortLevel::Medium,
            llm_gateway::Effort::High => EffortLevel::High,
        }
    }
}

impl From<EffortLevel> for llm_gateway::Effort {
    fn from(e: EffortLevel) -> Self {
        match e {
            EffortLevel::Low => llm_gateway::Effort::Low,
            EffortLevel::Medium => llm_gateway::Effort::Medium,
            EffortLevel::High => llm_gateway::Effort::High,
        }
    }
}

/// 選べるモデル 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct LlmModelOption {
    /// チャットへ送るモデル ID（論理名）。
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// プロバイダの表示名（例「Anthropic」「社内 GPU」）。
    pub provider_label: String,
    /// 日本国外で処理される（「国外処理」バッジ）。
    pub processed_abroad: bool,
    /// 思考の深さを選べるか。
    pub supports_effort: bool,
}

/// Auto の振り先（「Auto は何を使うか」の表示用）。
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoTarget {
    pub model: String,
    pub label: String,
    pub effort: EffortLevel,
}

/// `GET /llm/models` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct LlmModelsResponse {
    /// 新しいチャットの既定（`auto` かモデル ID）。
    pub default_model: String,
    /// モデルを選べるか（false なら選択 UI を出さない）。
    pub allow_user_choice: bool,
    /// モデルを選んだときの思考の深さの初期値。
    pub default_effort: EffortLevel,
    /// Auto の振り先（使えるモデルが無い構成では `null`）。
    pub auto: Option<AutoTarget>,
    pub models: Vec<LlmModelOption>,
}

/// gateway が無い（LLM 未構成）なら 503。
pub(crate) fn gateway(state: &AppState) -> Result<&Arc<llm_gateway::LlmGateway>, ApiError> {
    state
        .llm
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("llm gateway is not configured".into()))
}

/// プロバイダ ID → 表示名。
pub(crate) fn provider_label(gw: &llm_gateway::LlmGateway, provider_id: &str) -> String {
    gw.providers()
        .into_iter()
        .find(|p| p.id == provider_id)
        .map_or_else(|| provider_id.to_string(), |p| p.label)
}

/// チャットで選べるモデルの一覧。
#[utoipa::path(
    get,
    path = "/llm/models",
    responses(
        (status = 200, description = "選べるモデル", body = LlmModelsResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn list_models(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<LlmModelsResponse>, ApiError> {
    let gw = gateway(&state)?;
    let settings = gw
        .tenant_settings(&ctx)
        .await
        .map_err(|e| ApiError::Internal(format!("tenant llm settings: {e}")))?;
    let catalog = gw.catalog();
    let models = settings
        .enabled_models(catalog)
        .into_iter()
        .map(|m| LlmModelOption {
            id: m.id.clone(),
            label: m.label().to_string(),
            description: m.description.clone(),
            provider_label: provider_label(gw, m.provider_id()),
            processed_abroad: m.processed_abroad,
            supports_effort: gw.supports_effort(m),
        })
        .collect();
    let auto = gw.auto_target(&settings).map(|(model, effort)| AutoTarget {
        label: catalog
            .get(&model)
            .map_or_else(|| model.clone(), |m| m.label().to_string()),
        model,
        effort: effort.into(),
    });
    Ok(Json(LlmModelsResponse {
        default_model: settings.default_model,
        allow_user_choice: settings.allow_user_choice,
        default_effort: settings.default_effort.into(),
        auto,
        models,
    }))
}
