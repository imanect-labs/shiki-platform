//! ノート共同編集 WebSocket（Phase 11-pre Task 11P.1・design §4.8.1）。
//!
//! 認可（ノード実在・viewer/editor）は**アップグレード前**に判定し、HTTP 403/404 で
//! 返す。アップグレード後はセッションループ側が 30 秒ごとに relation を再チェックし、
//! 剥奪で切断する（PIT-37②・collab::session）。

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::RouteDecl;
use crate::state::AppState;

/// collab のルート宣言（route_table から分離・同じ宣言的マップの一部）。
pub(crate) fn collab_route_decls() -> Vec<RouteDecl> {
    // 編集セッションの WS が長時間開くためタイムアウト無しの streaming ポリシ。
    use crate::server::AccessPolicy::SessionStreaming;
    let r = RouteDecl::new;
    vec![r(
        "/collab/docs/{node_id}/ws",
        &["GET"],
        SessionStreaming,
        || get(collab_ws),
    )]
}

/// collab のエラーを HTTP エラーへ写す（fail-closed: 判定不能は 403 に倒す）。
fn to_api_error(err: collab::CollabError) -> ApiError {
    use collab::CollabError as CE;
    match err {
        CE::Forbidden(_) | CE::Authz(_) => ApiError::Forbidden,
        CE::NotFound(_) => ApiError::NotFound,
        CE::Storage(e) => ApiError::from(e),
        CE::Db(e) => ApiError::Internal(format!("collab db: {e}")),
        CE::InvalidUpdate(e) => ApiError::BadRequest(e),
    }
}

/// ノートの共同編集セッションへ接続する（y-websocket 互換ワイヤ）。
#[utoipa::path(
    get, path = "/collab/docs/{node_id}/ws",
    params(("node_id" = Uuid, Path, description = "ノード（ファイル）ID")),
    responses(
        (status = 101, description = "WebSocket へアップグレード（y-websocket 互換の sync/awareness ワイヤ）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "viewer/editor いずれの relation も無い"),
        (status = 404, description = "ノードが存在しない・ファイルでない"),
    ),
    security(("session" = [])),
)]
pub async fn collab_ws(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(node_id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let hub = state.collab.clone();
    // アップグレード前に実在＋認可を判定する（監査は get_metadata 経由で記録される）。
    let node = hub
        .require_file(&ctx, node_id, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    let mode = hub.authorize(&ctx, node_id).await.map_err(to_api_error)?;
    Ok(ws.on_upgrade(move |socket| async move {
        match hub.join(node_id, &node.org, &node.tenant_id).await {
            Ok(doc) => collab::run_session(socket, hub, ctx, doc, mode).await,
            Err(e) => {
                tracing::warn!(%node_id, error = %e, "collab ドキュメントのロードに失敗");
            }
        }
    }))
}
