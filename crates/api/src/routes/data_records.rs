//! 構造化データ API — レコード面（Task 9.2/9.3/9.5）。
//!
//! レコード CRUD・一覧/件数（行述語適用）・リビジョン履歴・個別共有。
//! 権限・検証・監査・行レベル述語は `DataStore`（単一チョークポイント）が担う。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use data::{
    DataRecord, ListRecordsOptions, RecordFilter, RecordRevision, RecordShareRole, RecordSort,
};
use serde::{Deserialize, Serialize};
use storage::ShareTarget;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

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
    /// 個別共有集合が上限（PIT-18）で切り詰められ、共有経由の一部行が
    /// 表示されていない可能性がある（fail-closed・可視減方向）。
    pub shares_truncated: bool,
}

/// レコード個別共有リクエスト（共有語彙は viewer/editor のみ・Task 9.3）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ShareRecordRequest {
    pub target: ShareTarget,
    pub role: RecordShareRole,
}

/// 件数レスポンス（行述語適用済み・Task 9.3）。
#[derive(Debug, Serialize, ToSchema)]
pub struct RecordCountResponse {
    pub count: i64,
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
    Ok(Json(RecordListResponse {
        items: page.items,
        shares_truncated: page.shares_truncated,
    }))
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

/// 可視行の件数（行述語適用済み・不可視行は混入しない）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/records/count",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("filter_field" = Option<String>, Query, description = "フィルタ対象（indexed 宣言済み）"),
        ("filter_value" = Option<String>, Query, description = "フィルタ値（text 系）"),
        ("filter_number" = Option<f64>, Query, description = "フィルタ値（number）"),
    ),
    responses(
        (status = 200, description = "件数", body = RecordCountResponse),
        (status = 400, description = "フィルタ指定が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "テーブルが存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn count_records(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ListRecordsQuery>,
) -> Result<Json<RecordCountResponse>, ApiError> {
    let filter = match (&q.filter_field, &q.filter_value, q.filter_number) {
        (Some(field), Some(value), None) => Some(RecordFilter {
            field: field.clone(),
            value: serde_json::Value::String(value.clone()),
        }),
        (Some(field), None, Some(n)) => Some(RecordFilter {
            field: field.clone(),
            value: serde_json::json!(n),
        }),
        (Some(_), _, _) => {
            return Err(ApiError::BadRequest(
                "filter_field には filter_value か filter_number のどちらか一方が必要です".into(),
            ))
        }
        (None, _, _) => None,
    };
    let count = state
        .data
        .count_records(&ctx, id, filter.as_ref(), trace.as_deref())
        .await?;
    Ok(Json(RecordCountResponse { count }))
}

/// レコードを個別共有する（テーブル owner またはレコード作成者・冪等）。
#[utoipa::path(
    put,
    path = "/data/tables/{id}/records/{record_id}/shares",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
    ),
    request_body = ShareRecordRequest,
    responses(
        (status = 204, description = "共有を付与した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner/作成者でない）"),
        (status = 404, description = "存在しない（不可視を含む）"),
    ),
    security(("session" = [])),
)]
pub async fn share_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ShareRecordRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .data
        .share_record(&ctx, id, record_id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// レコードの個別共有を解除する（冪等・即時反映）。
#[utoipa::path(
    delete,
    path = "/data/tables/{id}/records/{record_id}/shares",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
    ),
    request_body = ShareRecordRequest,
    responses(
        (status = 204, description = "共有を解除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner/作成者でない）"),
        (status = 404, description = "存在しない（不可視を含む）"),
    ),
    security(("session" = [])),
)]
pub async fn unshare_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<ShareRecordRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .data
        .unshare_record(&ctx, id, record_id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
