//! CSV クエリ/パッチ API（Phase 11-pre Task 11P.7・design §4.8.2）。
//!
//! すべて `TabularService`（単一チョークポイント・AuthContext 必須）を通す。DuckDB 実行は
//! 非特権別プロセスに隔離される（api プロセスは CSV を解釈しない・PIT-39）。

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::RouteDecl;
use crate::state::AppState;

/// tabular のルート宣言（route_table から分離）。
pub(crate) fn tabular_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    let r = RouteDecl::new;
    vec![
        r("/files/{id}/tabular/schema", &["GET"], Session, || {
            get(get_schema)
        }),
        r("/files/{id}/tabular/rows", &["GET"], Session, || {
            get(get_rows)
        }),
        r("/files/{id}/tabular/query", &["POST"], Session, || {
            post(post_query)
        }),
        r("/files/{id}/tabular/patch", &["POST"], Session, || {
            post(post_patch)
        }),
        r("/tabular/save", &["POST"], Session, || post(post_save)),
    ]
}

/// tabular のエラーを HTTP へ写す（fail-closed・存在秘匿）。
fn to_api_error(err: tabular::TabularError) -> ApiError {
    use tabular::TabularError as TE;
    match err {
        TE::Forbidden | TE::Authz(_) => ApiError::Forbidden,
        TE::NotFound(_) => ApiError::NotFound,
        TE::Storage(e) => ApiError::from(e),
        TE::SqlRejected(m) | TE::InvalidPatch(m) => ApiError::BadRequest(m),
        TE::QuotaExceeded(m) => ApiError::BadRequest(format!("クォータ超過: {m}")),
        TE::RevConflict { base, current } => ApiError::ConflictJson(serde_json::json!({
            "status": 409,
            "title": "競合しています（他の編集で更新されました）",
            "base_rev": base,
            "current_rev": current,
        })),
        TE::Runner(m) => ApiError::Internal(format!("tabular runner: {m}")),
        TE::Internal(m) => ApiError::Internal(m),
    }
}

/// 結果テーブル（クエリ/行取得の共通レスポンス）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TableResponse {
    pub columns: Vec<String>,
    #[schema(value_type = Vec<Vec<Option<String>>>)]
    pub rows: Vec<Vec<Option<String>>>,
    #[schema(value_type = Option<u64>)]
    pub total_rows: Option<u64>,
    pub truncated: bool,
    /// 次ページの offset（rows 取得時・None なら末尾）。
    #[schema(value_type = Option<u64>)]
    pub next_offset: Option<u64>,
}

impl TableResponse {
    fn from_runner(resp: tabular::RunnerResponse, next_offset: Option<u64>) -> Self {
        TableResponse {
            columns: resp.columns,
            rows: resp.rows,
            total_rows: resp.total_rows,
            truncated: resp.truncated,
            next_offset,
        }
    }
}

/// スキーマ（列名・総行数）。
#[derive(Debug, Serialize, ToSchema)]
pub struct SchemaResponse {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    #[schema(value_type = Option<u64>)]
    pub total_rows: Option<u64>,
}

/// CSV スキーマを返す（viewer 認可）。
#[utoipa::path(
    get, path = "/files/{id}/tabular/schema",
    params(("id" = Uuid, Path, description = "CSV ファイル ID")),
    responses((status = 200, body = SchemaResponse), (status = 403), (status = 404)),
    security(("session" = [])),
)]
pub async fn get_schema(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<SchemaResponse>, ApiError> {
    let resp = state
        .tabular
        .schema(&ctx, id, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    Ok(Json(SchemaResponse {
        columns: resp.columns,
        column_types: resp.column_types,
        total_rows: resp.total_rows,
    }))
}

/// 行ページ取得のクエリパラメータ。
#[derive(Debug, Deserialize)]
pub struct RowsParams {
    #[serde(default)]
    pub offset: u64,
}

/// CSV の 1 ページを返す（viewer 認可・無限スクロール）。
#[utoipa::path(
    get, path = "/files/{id}/tabular/rows",
    params(
        ("id" = Uuid, Path, description = "CSV ファイル ID"),
        ("offset" = Option<u64>, Query, description = "取得開始行（0 始まり）"),
    ),
    responses((status = 200, body = TableResponse), (status = 403), (status = 404)),
    security(("session" = [])),
)]
pub async fn get_rows(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(params): Query<RowsParams>,
) -> Result<Json<TableResponse>, ApiError> {
    let resp = state
        .tabular
        .rows(&ctx, id, params.offset, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    // 次ページ offset: このページが埋まっていれば続きがある。
    let page = u64::from(state.tabular.page_size());
    let got = resp.rows.len() as u64;
    let next = (got >= page).then_some(params.offset + got);
    Ok(Json(TableResponse::from_runner(resp, next)))
}

/// RO SQL クエリ。
#[derive(Debug, Deserialize, ToSchema)]
pub struct QueryRequest {
    pub sql: String,
}

/// 読み取り専用 SQL を実行する（viewer 認可・SQL 検証必須）。
#[utoipa::path(
    post, path = "/files/{id}/tabular/query", request_body = QueryRequest,
    params(("id" = Uuid, Path, description = "CSV ファイル ID")),
    responses((status = 200, body = TableResponse), (status = 400), (status = 403), (status = 404)),
    security(("session" = [])),
)]
pub async fn post_query(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<TableResponse>, ApiError> {
    let resp = state
        .tabular
        .query(&ctx, id, &req.sql, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    Ok(Json(TableResponse::from_runner(resp, None)))
}

/// パッチ適用リクエスト（rev 楽観ロック）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchRequest {
    /// 編集開始時の版（node.version）。不一致は 409。
    pub base_rev: i64,
    /// パッチ操作列（cell_update / row_insert / row_delete / column_add / column_delete / column_rename）。
    #[schema(value_type = Vec<serde_json::Value>)]
    pub ops: Vec<tabular::PatchOp>,
}

/// パッチ適用結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct PatchResponse {
    pub node_id: Uuid,
    pub version: i64,
    pub rows: usize,
    pub cols: usize,
}

/// セル/行/列パッチを適用して新バージョン保存する（editor 認可）。
#[utoipa::path(
    post, path = "/files/{id}/tabular/patch", request_body = PatchRequest,
    params(("id" = Uuid, Path, description = "CSV ファイル ID")),
    responses((status = 200, body = PatchResponse), (status = 400), (status = 403), (status = 404), (status = 409)),
    security(("session" = [])),
)]
pub async fn post_patch(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchRequest>,
) -> Result<Json<PatchResponse>, ApiError> {
    let applied = state
        .tabular
        .patch(&ctx, id, req.base_rev, &req.ops, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    Ok(Json(PatchResponse {
        node_id: applied.node_id,
        version: applied.version,
        rows: applied.rows,
        cols: applied.cols,
    }))
}

/// 新規 CSV 保存リクエスト（SQL 結果の「新規 CSV として保存」等）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveRequest {
    pub parent_id: Option<Uuid>,
    pub name: String,
    /// CSV 本文（そのまま保存）。
    pub csv: String,
}

/// 保存結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct SaveResponse {
    pub node_id: Uuid,
    pub version: i64,
    pub name: String,
}

/// 新規 CSV を保存する（保存先フォルダの作成権限）。
#[utoipa::path(
    post, path = "/tabular/save", request_body = SaveRequest,
    responses((status = 200, body = SaveResponse), (status = 400), (status = 403)),
    security(("session" = [])),
)]
pub async fn post_save(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("ファイル名を指定してください".into()));
    }
    let saved = state
        .tabular
        .save_new(
            &ctx,
            req.parent_id,
            name,
            req.csv.as_bytes(),
            trace.0.as_deref(),
        )
        .await
        .map_err(to_api_error)?;
    Ok(Json(SaveResponse {
        node_id: saved.node_id,
        version: saved.version,
        name: saved.name,
    }))
}
