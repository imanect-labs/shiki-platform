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
use workflow_engine::{Catalog, RunStatus, StepStatus, ValidationError, WorkflowStoreError};

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

/// 検証のみのリクエスト（保存しない・dnd のライブ検証用）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ValidateWorkflowRequest {
    /// ワークフロー IR（JSON DAG）。
    #[schema(value_type = Object)]
    pub ir: serde_json::Value,
}

/// 検証のみのレスポンス（errors 空 = 保存可能）。
#[derive(Debug, Serialize, ToSchema)]
pub struct ValidateWorkflowResponse {
    pub errors: Vec<ValidationError>,
}

/// IR を保存せず検証する（dnd エディタのライブ検証・Task 10.12）。
///
/// 保存 API と**同一のカタログ・同一の V1〜V7 パイプライン**を通す（結果の乖離をなくす）。
/// 検証エラーは失敗ではなく 200 の本文で返す（エディタが逐次表示するため）。
#[utoipa::path(
    post,
    path = "/workflows/validate",
    request_body = ValidateWorkflowRequest,
    responses(
        (status = 200, description = "検証結果（errors 空 = 保存可能）", body = ValidateWorkflowResponse),
        (status = 401, description = "未認証"),
    ),
    tag = "workflows"
)]
pub async fn validate_workflow(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Json(req): Json<ValidateWorkflowRequest>,
) -> Result<Json<ValidateWorkflowResponse>, ApiError> {
    let catalog = build_catalog(&state, &ctx).await?;
    let errors = match workflow_engine::validate(&req.ir, &catalog) {
        Ok(_) => Vec::new(),
        Err(errors) => errors,
    };
    Ok(Json(ValidateWorkflowResponse { errors }))
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
    // workflow 種の artifact のみ返す（このエンドポイントで他種 artifact を漏らさない）。
    let meta = state.artifacts.get(&ctx, id, trace.as_deref()).await?;
    if meta.kind != artifact::ArtifactKind::Workflow {
        return Err(ApiError::NotFound);
    }
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

/// 対話トリガの run 起動リクエスト（起動入力）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct StartRunRequest {
    /// run 起動入力（`$from input` の源）。
    #[serde(default)]
    #[schema(value_type = Object)]
    pub input: serde_json::Value,
}

/// run 起動レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct StartRunResponse {
    /// 起動した run の id（起動されなかった場合は null）。
    pub run_id: Option<Uuid>,
}

/// step 状態の 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct StepStatusItem {
    pub step_path: String,
    pub status: String,
}

/// run 状態レスポンス（実行履歴の要約）。
#[derive(Debug, Serialize, ToSchema)]
pub struct RunStatusResponse {
    pub run_id: Uuid,
    pub status: String,
    pub steps: Vec<StepStatusItem>,
}

/// ワークフローを対話トリガで起動する（実行主体＝起動ユーザーの権限）。
#[utoipa::path(
    post,
    path = "/workflows/{id}/runs",
    params(("id" = Uuid, Path, description = "ワークフロー ID")),
    request_body = StartRunRequest,
    responses(
        (status = 202, description = "起動した", body = StartRunResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    security(("session" = [])),
)]
pub async fn start_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    Json(req): Json<StartRunRequest>,
) -> Result<(StatusCode, Json<StartRunResponse>), ApiError> {
    let launcher = state
        .workflow_launcher
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))?;
    // start_interactive 内で workflows.get_latest の OpenFGA 認可を通る。IR 取得失敗（権限なし/不在）は
    // 存在秘匿のため 404 に写す（500 にしない）。
    let run_id = launcher
        .start_interactive(&ctx, id, &req.input)
        .await
        .map_err(|e| match e {
            workflow_engine::run::LauncherError::Ir(_) => ApiError::NotFound,
            other => ApiError::Internal(format!("run 起動: {other}")),
        })?;
    Ok((StatusCode::ACCEPTED, Json(StartRunResponse { run_id })))
}

/// run/step の状態を取得する（実行履歴・テナントスコープ）。
#[utoipa::path(
    get,
    path = "/workflows/{id}/runs/{run_id}",
    params(
        ("id" = Uuid, Path, description = "ワークフロー ID"),
        ("run_id" = Uuid, Path, description = "run ID"),
    ),
    responses(
        (status = 200, description = "run 状態", body = RunStatusResponse),
        (status = 401, description = "未認証"),
        (status = 404, description = "存在しない"),
        (status = 503, description = "workflow 実行時が無効"),
    ),
    security(("session" = [])),
)]
pub async fn get_workflow_run(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, run_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<RunStatusResponse>, ApiError> {
    let runs = state
        .workflow_runs
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("workflow 実行時が無効です".into()))?;
    // ① ワークフロー {id} への閲覧権限を OpenFGA で検証（他人の run 履歴を覗けない）。
    let meta = state.artifacts.get(&ctx, id, trace.as_deref()).await?;
    if meta.kind != artifact::ArtifactKind::Workflow {
        return Err(ApiError::NotFound);
    }
    // ② run を {id} 配下 ＋ テナントスコープで引く（別ワークフローの run_id を渡しても存在秘匿）。
    let status: RunStatus = runs
        .run_status_for_workflow(&ctx.tenant_id, id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("run 状態: {e}")))?
        .ok_or(ApiError::NotFound)?;
    let steps = runs
        .step_statuses(&ctx.tenant_id, run_id)
        .await
        .map_err(|e| ApiError::Internal(format!("step 状態: {e}")))?;
    Ok(Json(RunStatusResponse {
        run_id,
        status: status.as_str().to_string(),
        steps: steps
            .into_iter()
            .map(|(step_path, s): (String, StepStatus)| StepStatusItem {
                step_path,
                status: s.as_str().to_string(),
            })
            .collect(),
    }))
}
