//! 構造化データ API — FSM 定義＋status 遷移（Task 9.10）。
//!
//! FSM 定義は artifact(kind=fsm) として保存・共有・バージョン管理する。遷移は record の
//! status を進める唯一の経路で、行述語ロック→from 検証→actor 述語→status 更新→outbox→
//! 監査を単一トランザクションで行う（副作用ゼロ・Phase 10 workflow-engine へ委譲）。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use data::{DataRecord, FsmBody};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// FSM 作成リクエスト（対象テーブル＋定義）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFsmRequest {
    pub name: String,
    pub table_id: Uuid,
    pub body: FsmBody,
}

/// FSM 更新リクエスト（不変追記・楽観ロック）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateFsmRequest {
    pub table_id: Uuid,
    pub body: FsmBody,
    pub expected_version: Option<i64>,
}

/// FSM のメタ＋本文。
#[derive(Debug, Serialize, ToSchema)]
pub struct FsmResponse {
    pub id: Uuid,
    pub version: i64,
    pub body: FsmBody,
}

/// バージョン指定クエリ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct FsmVersionQuery {
    pub version: Option<i64>,
}

/// 遷移リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct TransitionRequest {
    /// 遷移先の状態。
    pub to: String,
    /// 楽観ロック（現在の rev）。
    pub expected_rev: i64,
}

/// FSM を作成する（対象テーブルのスキーマと照合検証・201）。
#[utoipa::path(
    post,
    path = "/data/fsms",
    request_body = CreateFsmRequest,
    responses(
        (status = 201, description = "作成した", body = FsmResponse),
        (status = 400, description = "FSM 定義が不正（状態/遷移/actor/options 不一致）"),
        (status = 401, description = "未認証"),
        (status = 403, description = "対象テーブルの権限がない"),
        (status = 409, description = "同名 FSM が既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_fsm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateFsmRequest>,
) -> Result<(StatusCode, Json<FsmResponse>), ApiError> {
    let id = state
        .fsms
        .create(&ctx, &req.name, req.table_id, &req.body, trace.as_deref())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(FsmResponse {
            id,
            version: 1,
            body: req.body,
        }),
    ))
}

/// FSM に新バージョンを追記する（editor）。
#[utoipa::path(
    put,
    path = "/data/fsms/{id}",
    params(("id" = Uuid, Path, description = "FSM（artifact）ID")),
    request_body = UpdateFsmRequest,
    responses(
        (status = 200, description = "追記後の FSM", body = FsmResponse),
        (status = 400, description = "FSM 定義が不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "expected_version 不一致"),
    ),
    security(("session" = [])),
)]
pub async fn update_fsm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateFsmRequest>,
) -> Result<Json<FsmResponse>, ApiError> {
    let version = state
        .fsms
        .update(
            &ctx,
            id,
            req.table_id,
            &req.body,
            req.expected_version,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(FsmResponse {
        id,
        version,
        body: req.body,
    }))
}

/// FSM を取得する（viewer・バージョン指定可）。
#[utoipa::path(
    get,
    path = "/data/fsms/{id}",
    params(
        ("id" = Uuid, Path, description = "FSM（artifact）ID"),
        FsmVersionQuery,
    ),
    responses(
        (status = 200, description = "FSM", body = FsmResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_fsm(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<FsmVersionQuery>,
) -> Result<Json<FsmResponse>, ApiError> {
    let (version, body) = state
        .fsms
        .get(&ctx, id, q.version, trace.as_deref())
        .await?;
    Ok(Json(FsmResponse { id, version, body }))
}

/// レコードの status を遷移させる（editor＋actor 述語・原子的・outbox 発行・Task 9.10）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/records/{record_id}/transition",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("record_id" = Uuid, Path, description = "レコード ID"),
    ),
    request_body = TransitionRequest,
    responses(
        (status = 200, description = "遷移後のレコード", body = DataRecord),
        (status = 400, description = "定義外の遷移・FSM 未設定・status 欠落"),
        (status = 401, description = "未認証"),
        (status = 403, description = "actor 述語を満たさない"),
        (status = 404, description = "テーブル/レコードが存在しない（不可視を含む）"),
        (status = 409, description = "rev 不一致（同時更新）"),
    ),
    security(("session" = [])),
)]
pub async fn transition_record(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, record_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<TransitionRequest>,
) -> Result<Json<DataRecord>, ApiError> {
    // テーブルの fsm_ref を解決し、ピンされたバージョンの FSM 定義をチョークポイント経由で読む。
    let table = state.data.get_table(&ctx, id, trace.as_deref()).await?;
    let fsm_ref = table.schema.fsm_ref.clone().ok_or_else(|| {
        ApiError::BadRequest("このテーブルは FSM 管理されていません（fsm_ref 未設定）".into())
    })?;
    let status_field =
        table.schema.status_field.clone().ok_or_else(|| {
            ApiError::BadRequest("このテーブルに status_field がありません".into())
        })?;
    let (_, fsm) = state
        .fsms
        .get(
            &ctx,
            fsm_ref.artifact_id,
            Some(fsm_ref.version),
            trace.as_deref(),
        )
        .await?;
    let updated = state
        .data
        .transition_record(
            &ctx,
            id,
            record_id,
            &req.to,
            req.expected_rev,
            &fsm,
            &status_field,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(updated))
}

/// FSM/遷移（Task 9.10）のルート宣言。
pub(crate) fn data_fsm_route_decls() -> Vec<crate::server::RouteDecl> {
    use crate::server::AccessPolicy::Session;
    use axum::routing::{get, post};
    let r = crate::server::RouteDecl::new;
    vec![
        r("/data/fsms", &["POST"], Session, || post(create_fsm)),
        r("/data/fsms/{id}", &["GET", "PUT"], Session, || {
            get(get_fsm).put(update_fsm)
        }),
        r(
            "/data/tables/{id}/records/{record_id}/transition",
            &["POST"],
            Session,
            || post(transition_record),
        ),
    ]
}
