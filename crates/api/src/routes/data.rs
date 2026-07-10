//! 構造化データ API（Task 9.2 / 9.5）。
//!
//! テーブル（スキーマレジストリ）とレコード CRUD・リビジョン履歴。
//! 権限・検証・監査は `DataStore`（単一チョークポイント）が担い、ハンドラは薄い変換のみ。
//! 宣言的クエリ（filter/sort/page/aggregate の合成・保存ビュー）は Task 9.4 で拡張する。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use data::{
    DataRecord, DataTable, ListRecordsOptions, NewDataTable, RecordFilter, RecordRevision,
    RecordSort, TableSchema,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// テーブル作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTableRequest {
    pub name: String,
    pub schema: TableSchema,
}

/// スキーマ改訂リクエスト（additive のみ・楽観ロック）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSchemaRequest {
    pub schema: TableSchema,
    pub expected_schema_version: Option<i64>,
}

/// テーブル一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct TableListResponse {
    pub items: Vec<DataTable>,
}

/// レコード作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateRecordRequest {
    pub data: serde_json::Value,
}

/// レコード更新リクエスト（merge patch・`null` はフィールド除去・楽観ロック必須）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRecordRequest {
    pub patch: serde_json::Value,
    pub expected_rev: i64,
}

/// レコード一覧クエリ（宣言フィールドの等値フィルタ＋ソート・Task 9.4 で宣言的クエリへ拡張）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListRecordsQuery {
    /// フィルタ対象フィールド（`indexed`/`unique` 宣言済みのみ）。
    pub filter_field: Option<String>,
    /// フィルタ値（text 系は完全一致・multi_select は包含・number は等値）。
    pub filter_value: Option<String>,
    /// number フィールドのフィルタ値（filter_value と排他）。
    pub filter_number: Option<f64>,
    /// ソート対象フィールド（`indexed`/`unique` 宣言済みのみ）。
    pub sort_field: Option<String>,
    /// 降順ソート（既定 false）。
    pub sort_desc: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// レコード一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct RecordListResponse {
    pub items: Vec<DataRecord>,
}

/// 削除クエリ（楽観ロック必須）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct DeleteRecordQuery {
    pub expected_rev: i64,
}

/// リビジョン一覧クエリ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListRevisionsQuery {
    pub before_rev: Option<i64>,
    pub limit: Option<i64>,
}

/// リビジョン一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct RevisionListResponse {
    pub items: Vec<RecordRevision>,
}

/// テーブルを作成する（スキーマ検証＋式インデックス生成・201）。
#[utoipa::path(
    post,
    path = "/data/tables",
    request_body = CreateTableRequest,
    responses(
        (status = 201, description = "作成した", body = DataTable),
        (status = 400, description = "スキーマが不正"),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名テーブルが既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_table(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateTableRequest>,
) -> Result<(StatusCode, Json<DataTable>), ApiError> {
    let created = state
        .data
        .create_table(
            &ctx,
            NewDataTable {
                name: req.name,
                schema: req.schema,
            },
            trace.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 自分が使えるテーブル一覧（ReBAC viewer 実効集合）。
#[utoipa::path(
    get,
    path = "/data/tables",
    responses(
        (status = 200, description = "一覧", body = TableListResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn list_tables(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<TableListResponse>, ApiError> {
    let items = state.data.list_tables(&ctx, 200).await?;
    Ok(Json(TableListResponse { items }))
}

/// テーブルのメタ＋スキーマを取得する（viewer）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "テーブル", body = DataTable),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_table(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<DataTable>, ApiError> {
    Ok(Json(
        state.data.get_table(&ctx, id, trace.as_deref()).await?,
    ))
}

/// スキーマを改訂する（owner・additive のみ・式インデックス差分適用）。
#[utoipa::path(
    put,
    path = "/data/tables/{id}/schema",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = UpdateSchemaRequest,
    responses(
        (status = 200, description = "改訂後のテーブル", body = DataTable),
        (status = 400, description = "スキーマが不正（型変更・削除を含む）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "expected_schema_version 不一致"),
    ),
    security(("session" = [])),
)]
pub async fn update_table_schema(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSchemaRequest>,
) -> Result<Json<DataTable>, ApiError> {
    let updated = state
        .data
        .update_table_schema(
            &ctx,
            id,
            req.schema,
            req.expected_schema_version,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(updated))
}

/// テーブルを論理削除する（owner）。
#[utoipa::path(
    delete,
    path = "/data/tables/{id}",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn delete_table(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.data.delete_table(&ctx, id, trace.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// レコードを作成する（editor・サーバ検証・201）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/records",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = CreateRecordRequest,
    responses(
        (status = 201, description = "作成した", body = DataRecord),
        (status = 400, description = "検証エラー（型・必須・選択肢・参照整合）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "テーブルが存在しない"),
        (status = 409, description = "unique 制約違反"),
    ),
    security(("session" = [])),
)]
pub async fn create_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateRecordRequest>,
) -> Result<(StatusCode, Json<DataRecord>), ApiError> {
    let created = state
        .data
        .create_record(&ctx, id, req.data, trace.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// レコード一覧（viewer・宣言フィールドの等値フィルタ＋ソート）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/records",
    params(("id" = Uuid, Path, description = "テーブル ID"), ListRecordsQuery),
    responses(
        (status = 200, description = "一覧", body = RecordListResponse),
        (status = 400, description = "フィルタ/ソート指定が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "テーブルが存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_records(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ListRecordsQuery>,
) -> Result<Json<RecordListResponse>, ApiError> {
    let filter = match (&q.filter_field, &q.filter_value, q.filter_number) {
        (Some(field), Some(value), None) => Some(RecordFilter {
            field: field.clone(),
            value: serde_json::Value::String(value.clone()),
        }),
        (Some(field), None, Some(n)) => Some(RecordFilter {
            field: field.clone(),
            value: serde_json::json!(n),
        }),
        (Some(_), Some(_), Some(_)) => {
            return Err(ApiError::BadRequest(
                "filter_value と filter_number は同時に指定できません".into(),
            ))
        }
        (Some(_), None, None) => {
            return Err(ApiError::BadRequest(
                "filter_field には filter_value か filter_number が必要です".into(),
            ))
        }
        (None, _, _) => None,
    };
    let sort = q.sort_field.as_ref().map(|field| RecordSort {
        field: field.clone(),
        descending: q.sort_desc.unwrap_or(false),
    });
    let page = state
        .data
        .list_records(
            &ctx,
            id,
            &ListRecordsOptions {
                filter,
                sort,
                limit: q.limit.unwrap_or(50),
                offset: q.offset.unwrap_or(0),
            },
            trace.as_deref(),
        )
        .await?;
    Ok(Json(RecordListResponse { items: page.items }))
}

/// レコードを取得する（viewer）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/records/{record_id}",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
    ),
    responses(
        (status = 200, description = "レコード", body = DataRecord),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<DataRecord>, ApiError> {
    Ok(Json(
        state
            .data
            .get_record(&ctx, id, record_id, trace.as_deref())
            .await?,
    ))
}

/// レコードを更新する（editor・merge patch・楽観ロック）。
#[utoipa::path(
    patch,
    path = "/data/tables/{id}/records/{record_id}",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
    ),
    request_body = UpdateRecordRequest,
    responses(
        (status = 200, description = "更新後のレコード", body = DataRecord),
        (status = 400, description = "検証エラー"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "rev 不一致（同時更新）または unique 制約違反"),
    ),
    security(("session" = [])),
)]
pub async fn update_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateRecordRequest>,
) -> Result<Json<DataRecord>, ApiError> {
    let updated = state
        .data
        .update_record(
            &ctx,
            id,
            record_id,
            req.patch,
            req.expected_rev,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(updated))
}

/// レコードを削除する（editor・楽観ロック・削除リビジョン記録）。
#[utoipa::path(
    delete,
    path = "/data/tables/{id}/records/{record_id}",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
        DeleteRecordQuery,
    ),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "rev 不一致（同時更新）"),
    ),
    security(("session" = [])),
)]
pub async fn delete_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<DeleteRecordQuery>,
) -> Result<StatusCode, ApiError> {
    state
        .data
        .delete_record(&ctx, id, record_id, q.expected_rev, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// リビジョン履歴（フィールド単位差分・rev 降順・Task 9.5）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/records/{record_id}/revisions",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
        ListRevisionsQuery,
    ),
    responses(
        (status = 200, description = "履歴", body = RevisionListResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_revisions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<ListRevisionsQuery>,
) -> Result<Json<RevisionListResponse>, ApiError> {
    let items = state
        .data
        .list_revisions(
            &ctx,
            id,
            record_id,
            q.before_rev,
            q.limit.unwrap_or(50),
            trace.as_deref(),
        )
        .await?;
    Ok(Json(RevisionListResponse { items }))
}
