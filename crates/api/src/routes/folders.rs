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
    routes::files::NodeResponse,
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

/// 子一覧の並び替えキー（クエリ値）。keyset カーソルへサーバ側で織り込む。
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    #[default]
    Name,
    Updated,
    Size,
}

impl SortField {
    fn to_key(&self) -> storage::ChildSortKey {
        match self {
            SortField::Name => storage::ChildSortKey::Name,
            SortField::Updated => storage::ChildSortKey::Updated,
            SortField::Size => storage::ChildSortKey::Size,
        }
    }
}

/// 子一覧のクエリ（親・並び順・カーソル・件数）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ChildrenQuery {
    /// 親フォルダ。未指定は org ルート直下。
    pub parent_id: Option<Uuid>,
    /// 並び替えキー（name|updated|size。既定 name）。
    #[serde(default)]
    pub sort: SortField,
    /// 降順にするか（既定 false=昇順）。
    #[serde(default)]
    pub desc: bool,
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=100。既定 50）。
    pub limit: Option<usize>,
    /// 名前検索（フォルダ横断・空白区切り語の AND 部分一致）。指定時は `parent_id` を無視する。
    pub q: Option<String>,
}

/// ゴミ箱一覧のクエリ（カーソル・件数）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct PageQuery {
    /// 前回応答の `next_cursor`。続きから取得する（省略で先頭）。
    pub cursor: Option<String>,
    /// 1 ページの最大件数（1..=100。既定 50）。
    pub limit: Option<usize>,
}

/// 子一覧の 1 ページ（権限フィルタ済み）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChildrenResponse {
    pub items: Vec<NodeResponse>,
    /// 続きがあれば次回 `cursor` に渡す値（末尾なら `null`）。
    pub next_cursor: Option<String>,
}

/// NodeResponse 群の `updated_by` を表示名で補完する（Task 11P.10）。
/// ディレクトリ（テナント＋org スコープ）で一括解決し、未登録 subject（AI 等）は null のまま。
/// 解決失敗は非致命（クライアントがフォールバック表示する）。
async fn resolve_updated_by_names(
    state: &AppState,
    ctx: &authz::AuthContext,
    items: &mut [NodeResponse],
) {
    let ids: Vec<String> = items.iter().map(|n| n.updated_by.clone()).collect();
    if let Ok(names) = state.directory.resolve_display_names(ctx, &ids).await {
        for n in &mut *items {
            n.updated_by_name = names.get(&n.updated_by).cloned();
        }
    }
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
        (status = 200, description = "作成したフォルダ", body = NodeResponse),
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
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .create_folder(&ctx, req.parent_id, &req.name, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// フォルダ/ルートの子を権限フィルタ済みで 1 ページ返す。
/// `q` 指定時はフォルダ横断の名前検索（同じく権限フィルタ済み）になる。
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
    let sort = storage::ChildSort {
        key: q.sort.to_key(),
        desc: q.desc,
    };
    let limit = q.limit.unwrap_or(50);
    let page = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(name_query) => {
            state
                .storage
                .search_nodes_by_name(
                    &ctx,
                    name_query,
                    sort,
                    q.cursor.as_deref(),
                    limit,
                    trace.as_deref(),
                )
                .await?
        }
        None => {
            state
                .storage
                .list_children(
                    &ctx,
                    q.parent_id,
                    sort,
                    q.cursor.as_deref(),
                    limit,
                    trace.as_deref(),
                )
                .await?
        }
    };
    let mut items: Vec<NodeResponse> = page.items.into_iter().map(Into::into).collect();
    resolve_updated_by_names(&state, &ctx, &mut items).await;
    Ok(Json(ChildrenResponse {
        items,
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
        (status = 200, description = "更新後のフォルダ", body = NodeResponse),
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
) -> Result<Json<NodeResponse>, ApiError> {
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

/// ゴミ箱からのフォルダ復元（サブツリーを同時復元）。
#[utoipa::path(
    post,
    path = "/folders/{id}/restore",
    params(("id" = Uuid, Path, description = "フォルダ ID")),
    responses(
        (status = 200, description = "復元したフォルダ", body = NodeResponse),
        (status = 400, description = "祖先が削除済みで復元不可"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "フォルダが無い"),
        (status = 409, description = "同名衝突"),
    ),
    security(("session" = [])),
)]
pub async fn restore_folder(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<NodeResponse>, ApiError> {
    let node = state
        .storage
        .restore_folder(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(node.into()))
}

/// ゴミ箱の中身（削除の根ノード）を新しい順に 1 ページ返す。
#[utoipa::path(
    get,
    path = "/trash",
    params(PageQuery),
    responses(
        (status = 200, description = "ゴミ箱の根ノード（復元できるもののみ）", body = ChildrenResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
    ),
    security(("session" = [])),
)]
pub async fn list_trash(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<PageQuery>,
) -> Result<Json<ChildrenResponse>, ApiError> {
    let page = state
        .storage
        .list_trash(
            &ctx,
            q.cursor.as_deref(),
            q.limit.unwrap_or(50),
            trace.as_deref(),
        )
        .await?;
    let mut items: Vec<NodeResponse> = page.items.into_iter().map(Into::into).collect();
    resolve_updated_by_names(&state, &ctx, &mut items).await;
    Ok(Json(ChildrenResponse {
        items,
        next_cursor: page.next_cursor,
    }))
}
