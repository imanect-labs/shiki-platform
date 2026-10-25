//! ミニアプリ API（Task 6.10・#604）。
//!
//! 作成/更新は**作成者の権限**で全ピンを検証し、解決（実行）はミニアプリ本体の viewer のみを
//! 要求する（部品はバンドル権限チョークポイント経由・部品の個別共有は不要）。
//! 画面（Yui の ```ui を含む Markdown）のボタンは、保存済みの版の画面から読み直し、束ねた
//! ワークフローだけを押した本人の権限で起動する（アンビエント権限なし）。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use gui::{MiniAppActionRequest, MiniAppRun};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::gui_errors::{map_gui_err, GuiValidationErrorResponse};
use super::ui_actions::{map_yui_action_err, UiActionResponse};
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMiniAppRequest {
    pub name: String,
    /// mini_app body（description・screen・skill/workflows のバージョンピン）。
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
}

/// resolve クエリ（version 省略時は current）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ResolveQuery {
    pub version: Option<i64>,
}

/// 本人が画面のボタンで起動した記録。
#[derive(Debug, Serialize, ToSchema)]
pub struct MiniAppRunsResponse {
    pub runs: Vec<MiniAppRun>,
}

/// 回答の本文の ```ui app を「アプリとして保存」するリクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveYuiAppRequest {
    /// 保存する図のブロックの識別子（ボタンの id の `:` より前・16 桁）。
    pub block: String,
    /// アプリの名前（一覧の名前・テナント内で一意）。
    pub name: String,
    /// 説明（画面の見出し）。空なら図の見出しを使う。
    #[serde(default)]
    pub description: Option<String>,
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
    }))
}

/// ミニアプリの画面のボタンを実行する（画面から読み直し・値の検証・冪等キー・実行者権限・監査つき）。
///
/// 画面の表（```data_grid）も同じ入口で描く: `action_id` は `data_grid:{別名}`、`params` は
/// `DataViewPage`（`cursor`・`limit` だけ）、冪等キーは不要。結果は `result.kind = "data_view"` と
/// `result.result`（`DataViewRun`）で、行・列は**閲覧者本人の権限**で毎回評価したもの（Task 15.7）。
#[utoipa::path(
    post,
    path = "/mini-apps/{id}/ui-actions",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    request_body = MiniAppActionRequest,
    responses(
        (status = 200, description = "実行した（同じ冪等キーの再送は前の結果）・表は DataViewRun", body = UiActionResponse),
        (status = 400, description = "値の検証エラー・冪等キーが無い"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "ボタンが見つからない"),
        (status = 409, description = "同じ送信が起動の途中・表の列の索引を準備中（code=index_preparing・field・progress）"),
        (status = 503, description = "実行先が無効"),
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
    let result = state
        .mini_apps
        .invoke_yui_action(&ctx, &state.ui_actions, id, req, trace.as_deref())
        .await
        .map_err(map_yui_action_err)?;
    Ok(Json(UiActionResponse { result }))
}

/// 本人がミニアプリの画面のボタンで起動した記録（新しい 20 件を古い順に）。
#[utoipa::path(
    get,
    path = "/mini-apps/{id}/runs",
    params(("id" = Uuid, Path, description = "ミニアプリ ID")),
    responses(
        (status = 200, description = "起動の記録", body = MiniAppRunsResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_mini_app_runs(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<MiniAppRunsResponse>, ApiError> {
    let runs = state
        .mini_apps
        .my_runs(&ctx, id, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok(Json(MiniAppRunsResponse { runs }))
}

/// 回答の本文の ```ui app を、そのボタンの実行先ごとミニアプリとして保存する。
///
/// 会話を読める人が使える。実行先は回答の確定時に解決した束縛で、保存する人の権限で
/// ピンを検証し直す（見えないワークフローは束ねられない）。
#[utoipa::path(
    post,
    path = "/threads/{thread_id}/messages/{message_id}/save-as-app",
    params(
        ("thread_id" = Uuid, Path, description = "スレッド ID"),
        ("message_id" = Uuid, Path, description = "メッセージ ID"),
    ),
    request_body = SaveYuiAppRequest,
    responses(
        (status = 201, description = "保存した", body = MiniAppResponse),
        (status = 400, description = "検証エラー（全件）", body = GuiValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "会話を読めない"),
        (status = 404, description = "図が見つからない"),
        (status = 409, description = "同名が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn save_yui_app(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((thread_id, message_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SaveYuiAppRequest>,
) -> Result<(StatusCode, Json<MiniAppResponse>), ApiError> {
    let chat = state
        .chat
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("チャットが無効です".into()))?;
    let draft = chat
        .yui_app_for_saving(&ctx, thread_id, message_id, &req.block, trace.as_deref())
        .await
        .map_err(ApiError::from)?
        .ok_or(ApiError::NotFound)?;
    let description = req
        .description
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| draft.title.clone());
    let body = serde_json::json!({
        "description": description,
        // 会話の図は ` ```ui:app ` と見出しだけで書かれることもあるので、種類をフェンスに残す。
        "screen": format!("```ui:app\n{}\n```\n", draft.source.trim_end()),
        "workflows": draft.workflows.iter().map(|(alias, id, version)| serde_json::json!({
            "alias": alias,
            "artifact_id": id,
            "version": version,
        })).collect::<Vec<_>>(),
    });
    let (id, _) = state
        .mini_apps
        .create(&ctx, req.name.trim(), &body, trace.as_deref())
        .await
        .map_err(map_gui_err)?;
    Ok((
        StatusCode::CREATED,
        Json(MiniAppResponse {
            id,
            version: 1,
            body,
        }),
    ))
}
