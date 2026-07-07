//! ワークフロー IR API（Task 10.1a）。
//!
//! IR を保存（V1〜V7 検証）・バージョン取得する。検証エラーは全件を 400 で返す（dnd 表示用）。
//! 実行主体の権限・監査・バージョニングは artifact 層が担う。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use workflow_engine::{Catalog, ValidationError, WorkflowStoreError};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 保存リクエスト（IR 本文）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveWorkflowRequest {
    /// ワークフロー IR（JSON DAG）。
    #[schema(value_type = Object)]
    pub ir: serde_json::Value,
    /// 更新時の楽観ロック（省略時は無条件追記）。
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// 保存レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct SaveWorkflowResponse {
    pub id: Uuid,
    pub version: i64,
    pub name: String,
}

/// バージョン取得レスポンス（IR 本文付き）。
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowVersionResponse {
    pub id: Uuid,
    pub version: i64,
    #[schema(value_type = Object)]
    pub ir: serde_json::Value,
}

/// 検証エラーレスポンス（全件・dnd がノード/エッジに表示）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ValidationErrorResponse {
    pub errors: Vec<ValidationError>,
}

/// API 層でカタログを組む（Stage A: 登録済み secret の名前→許可ホスト・モデルカタログ）。
async fn build_catalog(state: &AppState, ctx: &authz::AuthContext) -> Result<Catalog, ApiError> {
    let mut catalog = Catalog::default();
    // secret の参照名→許可ホスト（V4 の宛先束縛事前照合に使う）。
    if let Some(secrets) = state.secrets.as_deref() {
        for meta in secrets.list_mine(ctx).await? {
            catalog.secrets.insert(meta.name, meta.allowed_hosts);
        }
    }
    // モデルカタログ（llm.invoke の model 照合）。設定済みモデルを使う。
    catalog.models = state
        .config
        .llm
        .models
        .iter()
        .map(|m| m.id.clone())
        .collect();
    Ok(catalog)
}

/// 検証エラーを 400 として返す（全件を JSON body へ）。
fn map_store_err(err: WorkflowStoreError) -> ApiError {
    match err {
        WorkflowStoreError::Validation(errors) => {
            // 全件を構造化 JSON body で 400 に載せる（dnd クライアントが per-node/per-edge を描画できる）。
            let payload = serde_json::to_value(ValidationErrorResponse { errors })
                .unwrap_or_else(|_| serde_json::json!({ "errors": [] }));
            ApiError::UnprocessableJson(payload)
        }
        WorkflowStoreError::Artifact(e) => ApiError::from(e),
    }
}

/// ワークフローを保存する（新規・検証 → version 1）。
#[utoipa::path(
    post,
    path = "/workflows",
    request_body = SaveWorkflowRequest,
    responses(
        (status = 201, description = "保存した", body = SaveWorkflowResponse),
        (status = 400, description = "IR 検証エラー（全件）", body = ValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名ワークフローが既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<SaveWorkflowRequest>,
) -> Result<(StatusCode, Json<SaveWorkflowResponse>), ApiError> {
    let catalog = build_catalog(&state, &ctx).await?;
    let (id, ir) = state
        .workflows
        .create(&ctx, &req.ir, &catalog, trace.as_deref())
        .await
        .map_err(map_store_err)?;
    Ok((
        StatusCode::CREATED,
        Json(SaveWorkflowResponse {
            id,
            version: 1,
            name: ir.name,
        }),
    ))
}

/// ワークフローに新バージョンを追記する（検証 → 不変追記）。
#[utoipa::path(
    put,
    path = "/workflows/{id}",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    request_body = SaveWorkflowRequest,
    responses(
        (status = 200, description = "追記した", body = SaveWorkflowResponse),
        (status = 400, description = "IR 検証エラー（全件）", body = ValidationErrorResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "バージョン競合"),
    ),
    security(("session" = [])),
)]
pub async fn update_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SaveWorkflowRequest>,
) -> Result<Json<SaveWorkflowResponse>, ApiError> {
    let catalog = build_catalog(&state, &ctx).await?;
    let (version, ir) = state
        .workflows
        .update(
            &ctx,
            id,
            &req.ir,
            &catalog,
            req.expected_version,
            trace.as_deref(),
        )
        .await
        .map_err(map_store_err)?;
    Ok(Json(SaveWorkflowResponse {
        id,
        version,
        name: ir.name,
    }))
}

/// 最新バージョンの IR を取得する。
#[utoipa::path(
    get,
    path = "/workflows/{id}",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    responses(
        (status = 200, description = "最新 IR", body = WorkflowVersionResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowVersionResponse>, ApiError> {
    let (version, _ir) = state
        .workflows
        .get_latest(&ctx, id, trace.as_deref())
        .await
        .map_err(map_store_err)?;
    // 本文は artifact のバージョン本文をそのまま返す（正本 JSON）。
    let body = state
        .artifacts
        .get_version(&ctx, id, version, trace.as_deref())
        .await?;
    Ok(Json(WorkflowVersionResponse {
        id,
        version,
        ir: body.body,
    }))
}

/// 指定バージョンの IR を不変で取得する。
#[utoipa::path(
    get,
    path = "/workflows/{id}/versions/{version}",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("version" = i64, Path, description = "バージョン番号"),
    ),
    responses(
        (status = 200, description = "指定 IR（不変）", body = WorkflowVersionResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_workflow_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<WorkflowVersionResponse>, ApiError> {
    let body = state
        .artifacts
        .get_version(&ctx, id, version, trace.as_deref())
        .await?;
    Ok(Json(WorkflowVersionResponse {
        id,
        version,
        ir: body.body,
    }))
}
