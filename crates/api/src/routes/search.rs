//! `POST /search` — permission-aware 引用付き検索（Task 2.10）。
//!
//! ハンドラは薄く、実体は `rag::SearchService`（二段 authz・RRF・rerank・引用監査）。
//! DTO は rag 側の単一定義（`rag::SearchRequest` / `rag::SearchResponse`）をそのまま
//! OpenAPI へ流す（手書きミラー禁止・codegen が正）。

use axum::{extract::State, Json};
use rag::{SearchMode, SearchRequest, SearchResponse};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 権限を守った引用付き検索。
///
/// 実効結果 = pre-filter（可読 authz_tags）∩ post-filter（OpenFGA file check）。
/// 閲覧不可のファイルは結果に**絶対に**混入しない（docs/design.md §4.3 二段 authz）。
#[utoipa::path(
    post,
    path = "/search",
    request_body = SearchRequest,
    responses(
        (status = 200, description = "引用チャンク付きの検索結果", body = SearchResponse),
        (status = 400, description = "不正なリクエスト（空クエリ等）"),
        (status = 401, description = "未認証"),
        (status = 503, description = "RAG が無効設定、または依存サービスへ到達不能"),
    ),
    security(("session" = [])),
)]
pub async fn search(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    TraceIdExt(trace_id): TraceIdExt,
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    let Some(search) = state.search.as_ref() else {
        return Err(ApiError::ServiceUnavailable("rag.enabled=false".into()));
    };
    let query = req.query.trim();
    if query.is_empty() {
        return Err(ApiError::BadRequest("query が空です".into()));
    }

    let output = search
        .search(
            &ctx,
            query,
            req.top_k,
            req.mode.unwrap_or(SearchMode::Hybrid),
            trace_id.as_deref(),
        )
        .await?;
    Ok(Json(SearchResponse {
        results: output.results,
        debug: req.debug.then_some(output.debug),
    }))
}
