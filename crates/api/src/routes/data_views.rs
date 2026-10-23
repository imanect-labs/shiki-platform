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
use data::{DataQuery, DataViewBody, DataViewSummary, QueryResult, SqlQueryResult, SqlRequest};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// クエリ要求（クエリ IR ＋ 索引の自動昇格の明示の要求）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct DataQueryRequest {
    #[serde(flatten)]
    pub query: DataQuery,
    /// 未索引の列での並べ替え・絞り込みを**索引の自動昇格の契機にする**（data-platform §10）。
    /// /data のグリッドの UI が利用者の並べ替え・絞り込みの操作でだけ送る。既定 `false` で、未索引の列は
    /// 従来どおり 400。チャット・ワークフロー・ミニアプリの経路はこの要求を持たない（昇格しない）。
    #[serde(default)]
    pub promote_indexes: bool,
}

/// 宣言的クエリを実行する（行述語＋フィールドマスク＋集計抑制と合成・Task 9.4）。
///
/// `promote_indexes: true` のときだけ、未索引の列を自動で昇格する（上限内・小さなテーブルは同期で
/// 有効化・大きなテーブルは準備中 409・上限超過は承認待ち 409）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/query",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = DataQueryRequest,
    responses(
        (status = 200, description = "クエリ結果（rows または groups）", body = QueryResult),
        (status = 400, description = "クエリ指定が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（マスク列の参照を含む）"),
        (status = 404, description = "テーブルが存在しない"),
        (status = 409, description = "列の索引を準備中（code=index_preparing・field・progress）・索引の承認が要る（code=index_approval_required）・材料化した派生列が参照先の変更を受けて再計算中（code=derived_recomputing・field）"),
    ),
    security(("session" = [])),
)]
pub async fn run_query(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<DataQueryRequest>,
) -> Result<Json<QueryResult>, ApiError> {
    let r = if req.promote_indexes {
        state
            .data
            .run_query_promoting(&ctx, id, &req.query, trace.as_deref())
            .await?
    } else {
        state
            .data
            .run_query(&ctx, id, &req.query, trace.as_deref())
            .await?
    };
    Ok(Json(r))
}

/// SQL（閉じた部分集合）で読む（Task 15.10・data-platform §16.1）。
///
/// SQL は前段言語で、**データベースへは渡らない**（パース → allowlist 検証 → 名前の束縛 → クエリ IR へ変換し、
/// 以降は `/data/tables/{id}/query` と同じ経路で行述語・フィールドマスク・集計抑制と合成する）。
/// 受け付けるのは `SELECT 列|集計 FROM 表 [JOIN 表 ON リンク列 = 別名._id] [WHERE] [GROUP BY] [HAVING]
/// [ORDER BY] [LIMIT]`。拒否は 422 と構造化した理由（`code` / `message` / `hint` / `location` / `limit` /
/// `expected`）で、存在しない列と見えない列・存在しないテーブルと見えないテーブルは同一の本文になる。
#[utoipa::path(
    post,
    path = "/data/sql",
    request_body = SqlRequest,
    responses(
        (status = 200, description = "表形式の結果（columns / rows）", body = SqlQueryResult),
        (status = 400, description = "入力が不正（索引の無い列での絞り込みなど）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 422, description = "部分集合外・名前が見つからない・資源上限（構造化した理由）", body = data::QueryRejection),
    ),
    security(("session" = [])),
)]
pub async fn run_sql(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<SqlRequest>,
) -> Result<Json<SqlQueryResult>, ApiError> {
    Ok(Json(
        state.data.run_sql(&ctx, &req, trace.as_deref()).await?,
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

/// テーブルの保存ビュー一覧（ビュー切替用）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ViewListResponse {
    pub items: Vec<DataViewSummary>,
}

/// テーブルの保存ビューのうち、自分が使えるもの（本人の ＋ 共有されたもの・更新の新しい順・最大 100 件）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/views",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "保存ビューの一覧（本文は含まない）", body = ViewListResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "テーブルの viewer でない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_table_views(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<ViewListResponse>, ApiError> {
    let items = state
        .data_views
        .list_for_table(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(ViewListResponse { items }))
}

/// 保存ビューを作成する（table viewer＋表示設定とクエリ整合を検証・201）。
///
/// 表示設定（`display`・型付き `ViewSpec`）はサーバが検証する: 存在しない列・閲覧者ごと／時刻依存の
/// 派生列での並べ替え・グループ化は 422（`code = invalid_query`）、1 ビューのルックアップ・ロールアップ列の
/// 上限（既定 12・テナント設定）超過は 422（`code = limit_exceeded`・`limit.name = max_view_lookup_columns`）。
/// 理由の文言（`message`）をそのまま画面に出せる。保存者に読めない（マスク）列の表示・並べ替え・条件は 403。
#[utoipa::path(
    post,
    path = "/data/views",
    request_body = CreateViewRequest,
    responses(
        (status = 201, description = "作成した", body = ViewResponse),
        (status = 400, description = "クエリ/スキーマが不正"),
        (status = 422, description = "表示設定が不正・ルックアップ列の上限超過（構造化した理由）", body = data::QueryRejection),
        (status = 401, description = "未認証"),
        (status = 403, description = "対象テーブルの権限がない・マスク列を指定した"),
        (status = 409, description = "同名ビューが既に存在する・索引の承認が要る"),
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
        (status = 422, description = "表示設定が不正・ルックアップ列の上限超過（構造化した理由）", body = data::QueryRejection),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない・マスク列を指定した"),
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
        r("/data/sql", &["POST"], Session, || post(run_sql)),
        r("/data/views", &["POST"], Session, || post(create_view)),
        r("/data/tables/{id}/views", &["GET"], Session, || {
            get(list_table_views)
        }),
        r("/data/views/{id}", &["GET", "PUT"], Session, || {
            get(get_view).put(update_view)
        }),
        r("/data/views/{id}/run", &["POST"], Session, || {
            post(run_view)
        }),
    ]
}
