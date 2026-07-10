//! 構造化データ API — 宣言的クエリ＋保存ビュー（Task 9.4）。
//!
//! クエリ（filter/sort/page/aggregate）は生 SQL 非公開で、行述語・フィールドマスク・
//! 集計抑制と必ず合成される。保存ビューは artifact(kind=data_view) として ReBAC 共有・
//! バージョン管理し、実行は毎回**閲覧者本人**の権限で再評価する。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use data::{DataQuery, DataViewBody, QueryResult};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 宣言的クエリを実行する（行述語＋フィールドマスク＋集計抑制と合成・Task 9.4）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/query",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = DataQuery,
    responses(
        (status = 200, description = "クエリ結果（rows または groups）", body = QueryResult),
        (status = 400, description = "クエリ指定が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（マスク列の参照を含む）"),
        (status = 404, description = "テーブルが存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn run_query(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(query): Json<DataQuery>,
) -> Result<Json<QueryResult>, ApiError> {
    Ok(Json(
        state
            .data
            .run_query(&ctx, id, &query, trace.as_deref())
            .await?,
    ))
}

/// 保存ビュー作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateViewRequest {
    pub name: String,
    #[serde(flatten)]
    pub body: DataViewBody,
}

/// 保存ビュー更新リクエスト（不変追記・楽観ロック）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateViewRequest {
    #[serde(flatten)]
    pub body: DataViewBody,
    pub expected_version: Option<i64>,
}

/// 保存ビューのメタ＋本文。
#[derive(Debug, Serialize, ToSchema)]
pub struct ViewResponse {
    pub id: Uuid,
    pub version: i64,
    #[serde(flatten)]
    pub body: DataViewBody,
}

/// バージョン指定クエリ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ViewVersionQuery {
    pub version: Option<i64>,
}

/// 保存ビューを作成する（table viewer＋クエリ整合を検証・201）。
#[utoipa::path(
    post,
    path = "/data/views",
    request_body = CreateViewRequest,
    responses(
        (status = 201, description = "作成した", body = ViewResponse),
        (status = 400, description = "クエリ/スキーマが不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "対象テーブルの権限がない"),
        (status = 409, description = "同名ビューが既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_view(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateViewRequest>,
) -> Result<(StatusCode, Json<ViewResponse>), ApiError> {
    let id = state
        .data_views
        .create(&ctx, &req.name, &req.body, trace.as_deref())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ViewResponse {
            id,
            version: 1,
            body: req.body,
        }),
    ))
}

/// 保存ビューに新バージョンを追記する（editor・不変追記）。
#[utoipa::path(
    put,
    path = "/data/views/{id}",
    params(("id" = Uuid, Path, description = "ビュー（artifact）ID")),
    request_body = UpdateViewRequest,
    responses(
        (status = 200, description = "追記後のビュー", body = ViewResponse),
        (status = 400, description = "クエリ/スキーマが不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "expected_version 不一致"),
    ),
    security(("session" = [])),
)]
pub async fn update_view(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateViewRequest>,
) -> Result<Json<ViewResponse>, ApiError> {
    let version = state
        .data_views
        .update(&ctx, id, &req.body, req.expected_version, trace.as_deref())
        .await?;
    Ok(Json(ViewResponse {
        id,
        version,
        body: req.body,
    }))
}

/// 保存ビューを取得する（viewer・バージョン指定可）。
#[utoipa::path(
    get,
    path = "/data/views/{id}",
    params(
        ("id" = Uuid, Path, description = "ビュー（artifact）ID"),
        ViewVersionQuery,
    ),
    responses(
        (status = 200, description = "ビュー", body = ViewResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_view(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ViewVersionQuery>,
) -> Result<Json<ViewResponse>, ApiError> {
    let (version, body) = state
        .data_views
        .get(&ctx, id, q.version, trace.as_deref())
        .await?;
    Ok(Json(ViewResponse { id, version, body }))
}

/// 保存ビューを実行する（**閲覧者本人**の行述語・マスク・集計抑制で再評価・viewer）。
#[utoipa::path(
    post,
    path = "/data/views/{id}/run",
    params(
        ("id" = Uuid, Path, description = "ビュー（artifact）ID"),
        ViewVersionQuery,
    ),
    responses(
        (status = 200, description = "クエリ結果", body = QueryResult),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（マスク列の参照を含む）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn run_view(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ViewVersionQuery>,
) -> Result<Json<QueryResult>, ApiError> {
    Ok(Json(
        state
            .data_views
            .run(&ctx, id, q.version, trace.as_deref())
            .await?,
    ))
}

/// 構造化データのクエリ/ビュー（Task 9.4）のルート宣言。
pub(crate) fn data_view_route_decls() -> Vec<crate::server::RouteDecl> {
    use crate::server::AccessPolicy::Session;
    use axum::routing::{get, post};
    let r = crate::server::RouteDecl::new;
    vec![
        r("/data/tables/{id}/query", &["POST"], Session, || {
            post(run_query)
        }),
        r("/data/views", &["POST"], Session, || post(create_view)),
        r("/data/views/{id}", &["GET", "PUT"], Session, || {
            get(get_view).put(update_view)
        }),
        r("/data/views/{id}/run", &["POST"], Session, || {
            post(run_view)
        }),
    ]
}
