//! rag.query 能力アダプタ（Task 9.8）。
//!
//! [`crate::RagPort`]（実体は api 配線の `rag::SearchService` ラッパ）へ委譲する。
//! 検索は permission-aware（pre-filter＋OpenFGA post-filter の二段 authz）で、アプリ経由でも
//! **呼出ユーザーが読めない文書は結果に混入しない**（port 実装側が構造的に保証）。

use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError, RagHit,
};

#[derive(Debug, Deserialize)]
pub(crate) struct RagQueryRequest {
    pub query: String,
    pub top_k: Option<u32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RagQueryResponse {
    pub hits: Vec<RagHit>,
}

pub(crate) async fn query(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Json(req): Json<RagQueryRequest>,
) -> Result<Json<RagQueryResponse>, GatewayError> {
    let q = req.query.trim();
    if q.is_empty() {
        return Err(GatewayError::Invalid("query が空です".into()));
    }
    let hits = state.caps.rag.query(&ctx.auth, q, req.top_k, None).await?;
    Ok(Json(RagQueryResponse { hits }))
}
