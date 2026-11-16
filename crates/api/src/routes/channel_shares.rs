//! 資料のチャンネル共有 API（design §4.14）。
//!
//! ファイル・フォルダをチャンネルの参加者へ閲覧で共有する（`channel#member` の viewer タプル）。
//! 付与・解除・一覧は StorageService の単一チョークポイントを通り、owner 認可と監査はそこで掛かる。
//! チャンネルの名前は、呼び出した人が読めるチャンネルだけ messaging から引く（読めなければ名前を出さない）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// ノードを共有しているチャンネル 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct ChannelShareEntry {
    pub channel_id: Uuid,
    /// チャンネル名（呼び出した人が読めないチャンネルは `None`）。
    pub name: Option<String>,
    pub kind: Option<messaging::ChannelKind>,
}

/// ノードをチャンネルの参加者へ閲覧で共有する（owner かつチャンネルの参加者・冪等）。
#[utoipa::path(
    put,
    path = "/nodes/{id}/channel-shares/{channel_id}",
    params(("id" = Uuid, Path, description = "ノード ID"), ("channel_id" = Uuid, Path, description = "チャンネル ID")),
    responses(
        (status = 204, description = "共有した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "owner でない、またはチャンネルの参加者でない"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn share_with_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, channel_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .share_node_with_channel(&ctx, id, channel_id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// チャンネルへの共有を解除する（owner 権限・冪等・即時反映）。
#[utoipa::path(
    delete,
    path = "/nodes/{id}/channel-shares/{channel_id}",
    params(("id" = Uuid, Path, description = "ノード ID"), ("channel_id" = Uuid, Path, description = "チャンネル ID")),
    responses(
        (status = 204, description = "解除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "owner でない"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn unshare_from_channel(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, channel_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .unshare_node_from_channel(&ctx, id, channel_id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// このノードを共有しているチャンネルの一覧（owner 権限）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/channel-shares",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "共有先のチャンネル", body = [ChannelShareEntry]),
        (status = 401, description = "未認証"),
        (status = 403, description = "owner でない"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn list_channel_shares(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ChannelShareEntry>>, ApiError> {
    let ids = state
        .storage
        .list_channel_shares(&ctx, id, trace.as_deref())
        .await?;
    let mut out = Vec::with_capacity(ids.len());
    for channel_id in ids {
        let view = match &state.messaging {
            Some(svc) => svc.channel_view(&ctx, channel_id).await.ok(),
            None => None,
        };
        out.push(ChannelShareEntry {
            channel_id,
            name: view.as_ref().map(|v| v.name.clone()),
            kind: view.map(|v| v.kind),
        });
    }
    Ok(Json(out))
}
