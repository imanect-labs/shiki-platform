//! フォルダ操作＋階層 API（Task 1.5）。
//!
//! フォルダの作成/リネーム/移動/削除、子一覧（権限フィルタ済みページング・PIT-13）、
//! パンくず（祖先列）を提供する。全操作は StorageService 経由で認可・監査・closure 整合を担う
//! （単一チョークポイント）。move は closure をサブツリーごと張り替え、循環移動を 400 で拒否する。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    routes::files::FileResponse,
    state::AppState,
};

/// フォルダ作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFolderRequest {
    /// 配置先フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    pub name: String,
}

/// フォルダのリネーム/移動リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateFolderRequest {
    /// 新しい名前（指定時にリネーム）。
    pub name: Option<String>,
    /// 移動先フォルダ。`null` 明示は「ルートへ移動」、省略は「移動しない」。
    #[serde(default, deserialize_with = "crate::routes::files::double_option")]
    pub parent_id: Option<Option<Uuid>>,
}

/// 子一覧のクエリ（親・カーソル・件数）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ChildrenQuery {
    /// 親フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=100。既定 50）。
    pub limit: Option<usize>,
}

/// 子一覧の 1 ページ（権限フィルタ済み）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChildrenResponse {
    pub items: Vec<FileResponse>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `null`）。
    pub next_cursor: Option<String>,
}

/// パンくず 1 要素（祖先ノード）。
#[derive(Debug, Serialize, ToSchema)]
pub struct CrumbResponse {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
}

/// フォルダを作成する。
#[utoipa::path(
    post,
    path = "/folders",
    request_body = CreateFolderRequest,
    responses(
        (status = 200, description = "作成したフォルダ", body = FileResponse),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "親フォルダが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn create_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateFolderRequest>,
) -> Result<Json<FileResponse>, ApiError> {
    let node = state
        .storage
        .create_folder(&ctx, req.parent_id, &req.name, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// フォルダ/ルートの子を権限フィルタ済みで 1 ページ返す。
#[utoipa::path(
    get,
    path = "/nodes",
    params(ChildrenQuery),
    responses(
        (status = 200, description = "子一覧（読めるもののみ）", body = ChildrenResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "親フォルダが無い"),
    ),
    security(("session" = [])),
)]
pub async fn list_children(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<ChildrenQuery>,
) -> Result<Json<ChildrenResponse>, ApiError> {
    let page = state
        .storage
        .list_children(
            &ctx,
            q.parent_id,
            q.cursor.as_deref(),
            q.limit.unwrap_or(50),
            trace.as_deref(),
        )
        .await?;
    Ok(Json(ChildrenResponse {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    }))
}

/// ノードのパンくず（root→自身。読める接頭辞のみ）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/breadcrumb",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "祖先列（root が先頭）", body = [CrumbResponse]),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn breadcrumb(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<CrumbResponse>>, ApiError> {
    let crumbs = state.storage.breadcrumb(&ctx, id, trace.as_deref()).await?;
    Ok(Json(
        crumbs
            .into_iter()
            .map(|c| CrumbResponse {
                id: c.id,
                name: c.name,
                kind: c.kind.as_str().to_string(),
            })
            .collect(),
    ))
}

/// フォルダのリネーム・移動。
#[utoipa::path(
    patch,
    path = "/folders/{id}",
    params(("id" = Uuid, Path, description = "フォルダ ID")),
    request_body = UpdateFolderRequest,
    responses(
        (status = 200, description = "更新後のフォルダ", body = FileResponse),
        (status = 400, description = "不正なリクエスト（循環移動など）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "フォルダが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn update_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateFolderRequest>,
) -> Result<Json<FileResponse>, ApiError> {
    let node = state
        .storage
        .update_folder(
            &ctx,
            id,
            req.name.as_deref(),
            req.parent_id,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(node.into()))
}

/// フォルダをサブツリーごと論理削除する（ゴミ箱へ）。
#[utoipa::path(
    delete,
    path = "/folders/{id}",
    params(("id" = Uuid, Path, description = "フォルダ ID")),
    responses(
        (status = 204, description = "削除済み"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "フォルダが無い"),
    ),
    security(("session" = [])),
)]
pub async fn delete_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .soft_delete_folder(&ctx, id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
