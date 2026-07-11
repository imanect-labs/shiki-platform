//! ノート共同編集 WebSocket（Phase 11-pre Task 11P.1・design §4.8.1）。
//!
//! 認可（ノード実在・viewer/editor）は**アップグレード前**に判定し、HTTP 403/404 で
//! 返す。アップグレード後はセッションループ側が 30 秒ごとに relation を再チェックし、
//! 剥奪で切断する（PIT-37②・collab::session）。

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::routes::files::NodeResponse;
use crate::server::RouteDecl;
use crate::state::AppState;

/// collab のルート宣言（route_table から分離・同じ宣言的マップの一部）。
pub(crate) fn collab_route_decls() -> Vec<RouteDecl> {
    // 編集セッションの WS が長時間開くためタイムアウト無しの streaming ポリシ。
    use crate::server::AccessPolicy::{Session, SessionStreaming};
    let r = RouteDecl::new;
    vec![
        r(
            "/collab/docs/{node_id}/ws",
            &["GET"],
            SessionStreaming,
            || get(collab_ws),
        ),
        r("/notes", &["POST"], Session, || post(create_note)),
        r("/collab/docs/{node_id}/access", &["GET"], Session, || {
            get(get_access)
        }),
    ]
}

/// 共同編集アクセスモード（UI の編集可否切替に使う表示用ヒント）。
#[derive(Debug, Serialize, ToSchema)]
pub struct CollabAccessResponse {
    /// "editor"（読み書き）または "viewer"（読み取りのみ）。
    pub mode: String,
    /// ノード名（エディタ種別の判定・タイトル表示用）。
    pub name: String,
    /// 現在の node.version（外部書込検出の参考値）。
    pub version: i64,
}

/// 実行主体の共同編集アクセスモードを返す。
///
/// UI はこれで読み取り専用表示に切り替えるが、**書込の強制は WS セッション側**
/// （viewer の update 不受理・定期再チェック）が行う。
#[utoipa::path(
    get, path = "/collab/docs/{node_id}/access",
    params(("node_id" = Uuid, Path, description = "ノード（ファイル）ID")),
    responses(
        (status = 200, description = "アクセスモード", body = CollabAccessResponse),
        (status = 401, description = "未認証"),
        (status = 404, description = "ノードが存在しない・読めない（存在秘匿）"),
    ),
    security(("session" = [])),
)]
pub async fn get_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(node_id): Path<Uuid>,
) -> Result<Json<CollabAccessResponse>, ApiError> {
    let node = state
        .collab
        .require_file(&ctx, node_id, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    let mode = state
        .collab
        .authorize(&ctx, node_id)
        .await
        .map_err(to_api_error)?;
    let mode = match mode {
        collab::AccessMode::Editor => "editor",
        collab::AccessMode::Viewer => "viewer",
    };
    Ok(Json(CollabAccessResponse {
        mode: mode.to_string(),
        name: node.name,
        version: node.version,
    }))
}

/// ノート作成リクエスト（Task 11P.2・「新規作成 > ノート」/ note_ref 保存の共通経路）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateNoteRequest {
    /// 配置先フォルダ（None は org ルート直下）。
    pub parent_id: Option<Uuid>,
    /// ファイル名（`.md` は自動付与）。
    pub name: String,
    /// 初期内容の md（省略時は空ノート）。保存前に正規形へ正規化される。
    #[serde(default)]
    pub markdown: Option<String>,
}

/// ノート（.md ファイル）を作成する。
///
/// 実体は通常のドライブファイル（真実は Yjs・md はシリアライズ形式）。作成は
/// StorageService の内部書込（認可・監査・書込イベント→RAG 再索引つき）を通る。
#[utoipa::path(
    post, path = "/notes", request_body = CreateNoteRequest,
    responses(
        (status = 200, description = "作成したノートのノードメタ", body = NodeResponse),
        (status = 400, description = "名前が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "配置先への作成権限が無い"),
    ),
    security(("session" = [])),
)]
pub async fn create_note(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateNoteRequest>,
) -> Result<Json<NodeResponse>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("ノート名を指定してください".into()));
    }
    let file_name = if collab::note::is_note_file(name) {
        name.to_string()
    } else {
        format!("{name}.md")
    };
    // 初期 md は正規形へ正規化して保存する（往復契約の起点を正規形に揃える。
    // 生 HTML はこの時点でコードブロックへ縮退する＝note_ref 流入経路の XSS 遮断）。
    let markdown = req
        .markdown
        .as_deref()
        .map(collab::note::normalize_markdown)
        .unwrap_or_default();
    let node = state
        .storage
        .write_file_internal(
            &ctx,
            req.parent_id,
            &file_name,
            markdown.as_bytes(),
            "text/markdown",
            trace.0.as_deref(),
        )
        .await?;
    Ok(Json(NodeResponse::from(node)))
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
        match hub.join(&ctx, &node).await {
            Ok(doc) => collab::run_session(socket, hub, ctx, doc, mode).await,
            Err(e) => {
                tracing::warn!(%node_id, error = %e, "collab ドキュメントのロードに失敗");
            }
        }
    }))
}
