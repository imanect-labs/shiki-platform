//! UI スペック API（Task 6.3 保存路）。
//!
//! 保存時検証（gui::SpecValidator）を必ず通し、検証エラーは全件を 400 で返す。
//! 権限・監査・バージョニングは artifact 層（`UiSpecStore` → `ArtifactStore`）が担う。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use gui::{GuiError, GuiValidationError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateUiSpecRequest {
    /// 参照名（tenant 内で kind ごとに一意）。
    pub name: String,
    /// UI スペック本文（検証・解決される）。
    #[schema(value_type = Object)]
    pub spec: serde_json::Value,
}

/// 更新（新バージョン追記）リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateUiSpecRequest {
    #[schema(value_type = Object)]
    pub spec: serde_json::Value,
    /// 楽観ロック（省略時は無条件追記）。
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// スペック本文レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct UiSpecResponse {
    pub id: Uuid,
    pub version: i64,
    /// 検証・解決済みスペック（workflow 束縛はピン済み）。
    #[schema(value_type = Object)]
    pub spec: serde_json::Value,
}

/// 検証エラーレスポンス（全件・LLM/エディタが位置つきで表示）。
#[derive(Debug, Serialize, ToSchema)]
pub struct GuiValidationErrorResponse {
    pub errors: Vec<GuiValidationError>,
}

/// gui 層のエラーを HTTP へ写す（検証エラーは構造化 400）。
pub(crate) fn map_gui_err(err: GuiError) -> ApiError {
    match err {
        GuiError::Validation(errors) => {
            let payload = serde_json::to_value(GuiValidationErrorResponse { errors })
                .unwrap_or_else(|_| serde_json::json!({ "errors": [] }));
            ApiError::UnprocessableJson(payload)
        }
        GuiError::Artifact(e) => ApiError::from(e),
    }
}

/// UI スペックを作成する（検証・解決 → version 1）。
#[utoipa::path(
    post,
    path = "/ui-specs",
    request_body = CreateUiSpecRequest,
    responses(
        (status = 201, description = "作成した", body = UiSpecResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_ui_spec(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateUiSpecRequest>,
) -> Result<(StatusCode, Json<UiSpecResponse>), ApiError> {
    let (id, resolved) = state
        .ui_specs
        .create(&ctx, &req.name, &req.spec, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok((
        StatusCode::CREATED,
        Json(UiSpecResponse {
            id,
            version: 1,
            spec: resolved.json,
        }),
    ))
}

/// UI スペックに新バージョンを追記する（検証・解決 → 不変追記）。
#[utoipa::path(
    put,
    path = "/ui-specs/{id}",
    params(("id" = Uuid, Path, description = "UI スペック ID")),
    request_body = UpdateUiSpecRequest,
    responses(
        (status = 200, description = "追記した", body = UiSpecResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "バージョン競合"),
    ),
    security(("session" = [])),
)]
pub async fn update_ui_spec(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateUiSpecRequest>,
) -> Result<Json<UiSpecResponse>, ApiError> {
    let (version, resolved) = state
        .ui_specs
        .update(&ctx, id, &req.spec, req.expected_version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(UiSpecResponse {
        id,
        version,
        spec: resolved.json,
    }))
}

/// 最新バージョンの本文を取得する。
#[utoipa::path(
    get,
    path = "/ui-specs/{id}",
    params(("id" = Uuid, Path, description = "UI スペック ID")),
    responses(
        (status = 200, description = "最新スペック", body = UiSpecResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_ui_spec(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<UiSpecResponse>, ApiError> {
    let (version, spec) = state
        .ui_specs
        .get_latest(&ctx, id, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(UiSpecResponse { id, version, spec }))
}

/// 指定バージョンの本文を不変で取得する。
#[utoipa::path(
    get,
    path = "/ui-specs/{id}/versions/{version}",
    params(
        ("id" = Uuid, Path, description = "UI スペック ID"),
        ("version" = i64, Path, description = "バージョン番号"),
    ),
    responses(
        (status = 200, description = "指定バージョン（不変）", body = UiSpecResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_ui_spec_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<UiSpecResponse>, ApiError> {
    let (version, spec) = state
        .ui_specs
        .get_version(&ctx, id, version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(UiSpecResponse { id, version, spec }))
}
