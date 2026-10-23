//! 構造化データ API — 索引の状態と昇格（Task 15.6・data-platform §10・PIT-72）。
//!
//! - `GET /data/tables/{id}/indexes`: 列ごとの索引の状態（`absent` / `preparing` / `active`）と
//!   バックフィルの進捗（0〜1）。グリッド・管理画面の「準備中」表示に使う（件数・内部キーは返さない）。
//! - `POST /data/tables/{id}/indexes`: 列の索引を昇格する（2 万行未満は同期で有効化・それ以上は
//!   バックフィル）。テーブルあたりの索引列数の上限（既定 20）を超えるときは `approve: true` で
//!   owner が明示に承認する（承認なしは 409 `index_approval_required`）。
//!
//! グリッドの並べ替え・絞り込み（`POST /data/tables/{id}/query` に `promote_indexes: true`）とビューの
//! 保存は、未索引の列を自動で昇格する（上限内）。それ以外の経路（チャット・ワークフロー・ミニアプリ）は
//! 昇格しない。権限・監査は `DataStore` が担う。

use axum::{
    extract::{Path, State},
    routing::get,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn data_index_route_decls() -> Vec<RouteDecl> {
    vec![RouteDecl::new(
        "/data/tables/{id}/indexes",
        &["GET", "POST"],
        AccessPolicy::Session,
        || get(list_indexes).post(promote_index),
    )]
}

/// 列の索引の昇格。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PromoteIndexRequest {
    /// 列の参照名。
    pub field: String,
    /// 索引列数の上限を超えることの明示の承認（owner のみ）。
    #[serde(default)]
    pub approve: bool,
}

/// テーブルの索引の状態（viewer）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/indexes",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "索引の状態と進捗", body = data::TableIndexOverview),
        (status = 401, description = "未認証"),
        (status = 403, description = "viewer でない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_indexes(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<data::TableIndexOverview>, ApiError> {
    Ok(Json(
        state.data.table_indexes(&ctx, id, trace.as_deref()).await?,
    ))
}

/// 列の索引を昇格する（viewer・上限超過の承認は owner）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/indexes",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = PromoteIndexRequest,
    responses(
        (status = 200, description = "昇格の結果（active・または preparing と進捗）", body = data::IndexPromotion),
        (status = 400, description = "存在しない列・索引にできない列（閲覧者ごとの派生列・対称列）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "viewer でない・マスク列・承認は owner のみ"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "上限超過（code=index_approval_required）・前回の降格の後片付け中（code=index_preparing）"),
    ),
    security(("session" = [])),
)]
pub async fn promote_index(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PromoteIndexRequest>,
) -> Result<Json<data::IndexPromotion>, ApiError> {
    Ok(Json(
        state
            .data
            .promote_index(&ctx, id, &req.field, req.approve, trace.as_deref())
            .await?,
    ))
}
