//! skill API（Task 6.7）。
//!
//! 保存時検証（gui::validate_skill_body）を必ず通す。バージョン管理・ReBAC 共有・監査は
//! artifact 層が担い、共有 API は既存の `/artifacts/{id}/shares` を流用する（追加実装なし）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::ui_specs::{map_gui_err, GuiValidationErrorResponse};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSkillRequest {
    /// 参照名（tenant 内で kind ごとに一意）。
    pub name: String,
    /// skill body（SKILL.md 指示文＋知識スコープ＋許可ツール＋モデル既定＋few-shot＋script）。
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
}

/// 更新（新バージョン追記）リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSkillRequest {
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// skill 本文レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillResponse {
    pub id: Uuid,
    pub version: i64,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
}

/// skill を作成する（検証 → version 1）。
#[utoipa::path(
    post,
    path = "/skills",
    request_body = CreateSkillRequest,
    responses(
        (status = 201, description = "作成した", body = SkillResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_skill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateSkillRequest>,
) -> Result<(StatusCode, Json<SkillResponse>), ApiError> {
    let (id, _body) = state
        .skills
        .create(&ctx, &req.name, &req.body, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok((
        StatusCode::CREATED,
        Json(SkillResponse {
            id,
            version: 1,
            body: req.body,
        }),
    ))
}

/// skill に新バージョンを追記する。
#[utoipa::path(
    put,
    path = "/skills/{id}",
    params(("id" = Uuid, Path, description = "skill ID")),
    request_body = UpdateSkillRequest,
    responses(
        (status = 200, description = "追記した", body = SkillResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "バージョン競合"),
    ),
    security(("session" = [])),
)]
pub async fn update_skill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSkillRequest>,
) -> Result<Json<SkillResponse>, ApiError> {
    let (version, _body) = state
        .skills
        .update(&ctx, id, &req.body, req.expected_version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(SkillResponse {
        id,
        version,
        body: req.body,
    }))
}

/// 最新バージョンを取得する。
#[utoipa::path(
    get,
    path = "/skills/{id}",
    params(("id" = Uuid, Path, description = "skill ID")),
    responses(
        (status = 200, description = "最新 skill", body = SkillResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_skill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<SkillResponse>, ApiError> {
    let (version, _body, raw) = state
        .skills
        .get_latest(&ctx, id, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(SkillResponse {
        id,
        version,
        body: raw,
    }))
}

/// 指定バージョンを不変で取得する。
#[utoipa::path(
    get,
    path = "/skills/{id}/versions/{version}",
    params(
        ("id" = Uuid, Path, description = "skill ID"),
        ("version" = i64, Path, description = "バージョン番号"),
    ),
    responses(
        (status = 200, description = "指定バージョン（不変）", body = SkillResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_skill_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<SkillResponse>, ApiError> {
    let (version, _body, raw) = state
        .skills
        .get_version(&ctx, id, version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(SkillResponse {
        id,
        version,
        body: raw,
    }))
}
