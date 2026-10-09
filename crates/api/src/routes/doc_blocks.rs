//! `GET /files/{id}/versions/{version}/blocks` — 版のブロック列（出典パネル・#508）。
//!
//! 引用は「版ごとの正規化ブロック列の中の範囲」で位置を指す。出典パネルはこの API で
//! 引用箇所の前後を窓で取り、描画してハイライトする（大きな文書でも全件は返さない）。
//!
//! 認可はファイルのメタデータ取得と同じく StorageService を通す（viewer・監査つき）。
//! ハンドラに独自の権限判定は書かない。読み出しは認可済みの `Node` を受け取る形にしてある。

use axum::{
    extract::{Path, Query, State},
    Json,
};
use rag::DocBlocksPage;
use serde::Deserialize;
use storage::NodeKind;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 窓の指定。
#[derive(Debug, Deserialize, IntoParams)]
pub struct BlocksQuery {
    /// 先頭の ordinal（既定 0）。
    pub from: Option<i32>,
    /// 件数（既定 60・上限 200）。
    pub limit: Option<u32>,
}

/// 版のブロック列を ordinal 順に返す。
#[utoipa::path(
    get,
    path = "/files/{id}/versions/{version}/blocks",
    params(
        ("id" = Uuid, Path, description = "ファイル ID"),
        ("version" = i64, Path, description = "版"),
        BlocksQuery,
    ),
    responses(
        (status = 200, description = "ブロック列の窓（解析前・非対応形式は空）", body = DocBlocksPage),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない"),
        (status = 404, description = "ファイルが無い"),
        (status = 503, description = "RAG が無効設定"),
    ),
    security(("session" = [])),
)]
pub async fn list_blocks(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
    Query(q): Query<BlocksQuery>,
) -> Result<Json<DocBlocksPage>, ApiError> {
    let Some(search) = state.search.as_ref() else {
        return Err(ApiError::ServiceUnavailable("rag.enabled=false".into()));
    };
    // viewer 判定（共有リンクの遅延失効・監査を含む）。ここを通った Node だけが読み出しに渡る。
    let node = state
        .storage
        .get_metadata(&ctx, id, trace.as_deref())
        .await?;
    if node.kind != NodeKind::File || version < 1 || version > node.version {
        return Ok(Json(DocBlocksPage {
            blocks: Vec::new(),
            next_from: None,
        }));
    }
    let page = search
        .blocks(&ctx, &node, version, q.from.unwrap_or(0), q.limit.unwrap_or(60))
        .await?;
    Ok(Json(page))
}
