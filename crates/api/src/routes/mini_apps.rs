//! ミニアプリ API（Task 6.10）。
//!
//! 作成/更新は**作成者の権限**で全ピンを検証し、解決（実行）はミニアプリ本体の viewer のみを
//! 要求する（部品はバンドル権限チョークポイント経由・部品の個別共有は不要）。
//! UI アクションは解決済みスペックの宣言束縛に照合して実行する（アンビエント権限なし）。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use gui::ActionSource;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::ui_actions::{map_action_err, UiActionResponse};
use super::ui_specs::{map_gui_err, GuiValidationErrorResponse};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMiniAppRequest {
    pub name: String,
    /// mini_app body（ui_spec/skill/workflows のバージョンピン）。
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
}

/// 更新（新バージョン追記）リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMiniAppRequest {
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// ミニアプリ本文レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct MiniAppResponse {
    pub id: Uuid,
    pub version: i64,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
}

/// 解決済みミニアプリ（実行画面が使う一式・全て検証済み）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedMiniAppResponse {
    pub id: Uuid,
    pub version: i64,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    /// 検証済み UI スペック（描画の正）。
    #[schema(value_type = Object)]
    pub ui_spec: serde_json::Value,
}

/// resolve クエリ（version 省略時は current）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ResolveQuery {
    pub version: Option<i64>,
}

/// ミニアプリの UI アクション実行リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct MiniAppActionRequest {
    /// 実行対象のミニアプリ版（**必須**・解決済み画面の版と一致させる。current 暗黙参照だと
    /// 画面を開いたまま新版が保存されたとき、別の束縛へ同じ action_id で到達し得る）。
    pub version: i64,
    pub action_id: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

/// ミニアプリを作成する（ピン検証 → version 1）。
#[utoipa::path(
    post,
    path = "/mini-apps",
    request_body = CreateMiniAppRequest,
    responses(
        (status = 201, description = "作成した", body = MiniAppResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateMiniAppRequest>,
) -> Result<(StatusCode, Json<MiniAppResponse>), ApiError> {
    let (id, _body) = state
        .mini_apps
        .create(&ctx, &req.name, &req.body, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok((
        StatusCode::CREATED,
        Json(MiniAppResponse {
            id,
            version: 1,
            body: req.body,
        }),
    ))
}

/// ミニアプリに新バージョンを追記する。
#[utoipa::path(
    put,
    path = "/mini-apps/{id}",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = UpdateMiniAppRequest,
    responses(
        (status = 200, description = "追記した", body = MiniAppResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "バージョン競合"),
    ),
    security(("session" = [])),
)]
pub async fn update_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMiniAppRequest>,
) -> Result<Json<MiniAppResponse>, ApiError> {
    let (version, _body) = state
        .mini_apps
        .update(&ctx, id, &req.body, req.expected_version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(MiniAppResponse {
        id,
        version,
        body: req.body,
    }))
}

/// ミニアプリを解決する（実行画面用・部品はバンドル権限で読む）。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/resolved",
    params(
        ("id" = Uuid, Path, description = "ミニアプリ ID"),
        ResolveQuery,
    ),
    responses(
        (status = 200, description = "解決済みミニアプリ", body = ResolvedMiniAppResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn resolve_mini_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ResolveQuery>,
) -> Result<Json<ResolvedMiniAppResponse>, ApiError> {
    let resolved = state
        .mini_apps
        .resolve(&ctx, id, q.version, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    let body = serde_json::to_value(&resolved.body)
        .map_err(|e| ApiError::Internal(format!("mini_app body: {e}")))?;
    Ok(Json(ResolvedMiniAppResponse {
        id: resolved.id,
        version: resolved.version,
        body,
        ui_spec: resolved.ui_spec_json,
    }))
}

/// ミニアプリの UI アクションを実行する（宣言済み束縛のみ・実行者権限・監査つき）。
#[utoipa::path(
    post,
    path = "/mini-apps/{id}/ui-actions",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = MiniAppActionRequest,
    responses(
        (status = 200, description = "実行した", body = UiActionResponse),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "対象が見つからない（未宣言アクション含む）"),
        (status = 503, description = "束縛先が無効"),
    ),
    security(("session" = [])),
)]
pub async fn invoke_mini_app_action(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<MiniAppActionRequest>,
) -> Result<Json<UiActionResponse>, ApiError> {
    // 解決（本体 viewer 認可＋部品のバンドル読み＋再検証）を通った束縛のみが照合対象。
    let resolved = state
        .mini_apps
        .resolve(&ctx, id, Some(req.version), trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    let source = ActionSource::MiniApp {
        artifact_id: resolved.id,
        version: resolved.version,
    };
    let result = state
        .ui_actions
        .dispatch(
            &ctx,
            &source,
            &resolved.ui_spec,
            &req.action_id,
            req.params,
            trace.as_deref(),
        )
        .await
        .map_err(map_action_err)?;
    Ok(Json(UiActionResponse { result }))
}
