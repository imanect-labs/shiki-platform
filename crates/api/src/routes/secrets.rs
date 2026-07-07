//! シークレット API（Task 10.9・write-only / use-only）。
//!
//! 登録・ローテーション・宛先束縛更新・削除・**参照名一覧**のみ。**平文を読み返すルートは無い**
//! （解決＝利用は実行時にエンジン側でのみ行う）。権限・暗号・監査は `SecretStore` が担う。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use secrets::{NewSecret, SecretMeta};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 登録リクエスト（平文はここでのみ受け取り、保存後は二度と読めない）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSecretRequest {
    pub name: String,
    /// 平文（トークン等）。base64 ではなくそのまま受ける（TLS＋write-only 前提）。
    pub value: String,
    /// 添付を許可する宛先ホスト（完全一致 or `*.suffix`）。
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

/// ローテーションリクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct RotateSecretRequest {
    pub value: String,
}

/// 宛先束縛更新リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateBindingRequest {
    pub allowed_hosts: Vec<String>,
}

/// 一覧レスポンス（参照名のみ・平文なし）。
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct SecretListResponse {
    pub items: Vec<SecretMeta>,
}

fn store(state: &AppState) -> Result<&secrets::SecretStore, ApiError> {
    state
        .secrets
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("secrets: 未設定（マスターキー無し）".into()))
}

/// シークレットを登録する（201・平文は暗号化して保存）。
#[utoipa::path(
    post,
    path = "/secrets",
    request_body = CreateSecretRequest,
    responses(
        (status = 201, description = "登録した", body = SecretMeta),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名シークレットが既に存在する"),
        (status = 503, description = "secrets 未設定"),
    ),
    security(("session" = [])),
)]
pub async fn create_secret(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateSecretRequest>,
) -> Result<(StatusCode, Json<SecretMeta>), ApiError> {
    let meta = store(&state)?
        .create(
            &ctx,
            NewSecret {
                name: req.name,
                plaintext: req.value.into_bytes(),
                allowed_hosts: req.allowed_hosts,
            },
            trace.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(meta)))
}

/// 参照名一覧（平文なし）。
#[utoipa::path(
    get,
    path = "/secrets",
    responses(
        (status = 200, description = "参照名一覧", body = SecretListResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "secrets 未設定"),
    ),
    security(("session" = [])),
)]
pub async fn list_secrets(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<SecretListResponse>, ApiError> {
    let items = store(&state)?.list_mine(&ctx).await?;
    Ok(Json(SecretListResponse { items }))
}

/// メタデータ取得（can_use・平文なし）。
#[utoipa::path(
    get,
    path = "/secrets/{id}",
    params(("id" = Uuid, Path, description = "シークレット ID")),
    responses(
        (status = 200, description = "メタデータ（平文なし）", body = SecretMeta),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_secret(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretMeta>, ApiError> {
    Ok(Json(
        store(&state)?.get_meta(&ctx, id, trace.as_deref()).await?,
    ))
}

/// ローテーション（owner・新しい平文で再暗号化）。
#[utoipa::path(
    put,
    path = "/secrets/{id}",
    params(("id" = Uuid, Path, description = "シークレット ID")),
    request_body = RotateSecretRequest,
    responses(
        (status = 200, description = "ローテーションした", body = SecretMeta),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn rotate_secret(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<RotateSecretRequest>,
) -> Result<Json<SecretMeta>, ApiError> {
    Ok(Json(
        store(&state)?
            .rotate(&ctx, id, req.value.into_bytes(), trace.as_deref())
            .await?,
    ))
}

/// 宛先束縛の更新（owner）。
#[utoipa::path(
    patch,
    path = "/secrets/{id}/binding",
    params(("id" = Uuid, Path, description = "シークレット ID")),
    request_body = UpdateBindingRequest,
    responses(
        (status = 200, description = "更新した", body = SecretMeta),
        (status = 400, description = "不正なホスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn update_binding(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateBindingRequest>,
) -> Result<Json<SecretMeta>, ApiError> {
    Ok(Json(
        store(&state)?
            .update_binding(&ctx, id, req.allowed_hosts, trace.as_deref())
            .await?,
    ))
}

/// 削除（owner）。
#[utoipa::path(
    delete,
    path = "/secrets/{id}",
    params(("id" = Uuid, Path, description = "シークレット ID")),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn delete_secret(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    store(&state)?.delete(&ctx, id, trace.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}
