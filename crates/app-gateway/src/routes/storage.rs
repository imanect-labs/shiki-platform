//! storage.read / storage.write 能力アダプタ（Task 9.8）。
//!
//! `storage::StorageService`（単一チョークポイント・個人 ReBAC・監査込み）へ委譲する。
//! アプリ固有のリソース束縛は持たない——ファイル/フォルダの可視範囲は**呼出ユーザーの
//! ReBAC そのもの**（アプリはユーザーが見える範囲しか見えない・広がらない）。
//! アップロード（presign PUT→finalize の 2 段）は B2 関数のユースケースが立つ PR11 以降。

use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use storage::{ChildSort, Node, NodeKind};
use uuid::Uuid;

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

/// アプリへ見せるノードの最小 DTO。
#[derive(Debug, Serialize)]
pub(crate) struct GwNode {
    pub id: Uuid,
    pub kind: NodeKind,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub size_bytes: Option<i64>,
    pub content_type: Option<String>,
    pub version: i64,
}

fn to_gw_node(n: Node) -> GwNode {
    GwNode {
        id: n.id,
        kind: n.kind,
        name: n.name,
        parent_id: n.parent_id,
        size_bytes: n.size_bytes,
        content_type: n.content_type,
        version: n.version,
    }
}

pub(crate) async fn get_metadata(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(node_id): Path<Uuid>,
) -> Result<Json<GwNode>, GatewayError> {
    let node = state
        .caps
        .storage
        .get_metadata(&ctx.auth, node_id, None)
        .await?;
    Ok(Json(to_gw_node(node)))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListChildrenQuery {
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GwChildPage {
    pub items: Vec<GwNode>,
    pub next_cursor: Option<String>,
}

/// フォルダ直下の一覧（権限フィルタ済み・keyset カーソル）。
///
/// 1 ページが `limit` より少ないか空のまま `next_cursor` だけが付くことがある（OpenFGA に問う数の上限・BUG-187）。
/// `next_cursor` はテナント・フォルダー・並びに束縛した不透明な文字列（見えないものの名前を含む位置を外へ出さない）。
pub(crate) async fn list_children(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(node_id): Path<Uuid>,
    Query(q): Query<ListChildrenQuery>,
) -> Result<Json<GwChildPage>, GatewayError> {
    let sort = ChildSort::default();
    let kind = data::ListCursorKind::DriveFolder;
    let scope = sort.cursor_scope(&node_id.to_string());
    let tenant = &ctx.auth.tenant_id;
    let cursor = q
        .cursor
        .as_deref()
        .filter(|c| !c.is_empty())
        .map(|c| state.caps.data.open_list_cursor(kind, tenant, &scope, c))
        .transpose()?;
    let page = state
        .caps
        .storage
        .list_children(
            &ctx.auth,
            Some(node_id),
            sort,
            cursor.as_deref(),
            q.limit.unwrap_or(50).min(200),
            // ミニアプリへもシステム領域は見せない（#392）。
            false,
            None,
        )
        .await?;
    let next_cursor = page
        .next_cursor
        .map(|c| state.caps.data.seal_list_cursor(kind, tenant, &scope, &c))
        .transpose()?;
    Ok(Json(GwChildPage {
        items: page.items.into_iter().map(to_gw_node).collect(),
        next_cursor,
    }))
}

#[derive(Debug, Serialize)]
pub(crate) struct GwDownloadUrl {
    pub url: String,
    pub expires_in_secs: u64,
}

/// ダウンロード presigned URL（短命・発行自体を監査するのは StorageService 側）。
pub(crate) async fn download_url(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(node_id): Path<Uuid>,
) -> Result<Json<GwDownloadUrl>, GatewayError> {
    let ticket = state
        .caps
        .storage
        .issue_download_url(&ctx.auth, node_id, None)
        .await?;
    Ok(Json(GwDownloadUrl {
        url: ticket.url,
        expires_in_secs: ticket.expires_in_secs,
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateFolderRequest {
    /// `null` はルート直下（org メンバーであれば作成可）。
    pub parent_id: Option<Uuid>,
    pub name: String,
}

pub(crate) async fn create_folder(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Json(req): Json<CreateFolderRequest>,
) -> Result<Json<GwNode>, GatewayError> {
    let node = state
        .caps
        .storage
        .create_folder(&ctx.auth, req.parent_id, &req.name, None)
        .await?;
    Ok(Json(to_gw_node(node)))
}
