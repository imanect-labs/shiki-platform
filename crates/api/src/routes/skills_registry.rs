//! skill のレジストリ publish / 同意インストール API（#344 Task 10.11・500 行規約で分離）。
//!
//! 実体は `app_platform::SkillInstallService`（Phase 9 レジストリの流用・信頼ティア検証・
//! ユーザー単位インストール・監査）。認可は publish=artifact owner、install=本人の明示行為
//! （first-party は署名検証・in-house は viewer 必須・fail-closed）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use base64::Engine as _;
use serde::Deserialize;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use app_platform::{RegistryEntry, SkillInstallation};

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// publish リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishSkillRequest {
    /// レジストリの公開バージョン（IR の `skill:<name>@<version>` 語彙・未指定は
    /// artifact の current_version 文字列）。
    #[serde(default)]
    pub version: Option<String>,
    /// 信頼ティア（`in_house` | `first_party`）。既定は in_house。
    #[serde(default)]
    pub trust_tier: Option<String>,
    /// body digest への ed25519 署名（base64・first-party のみ・検証はインストール時）。
    #[serde(default)]
    pub signature_base64: Option<String>,
}

/// インストールリクエスト（version 未指定は最新の未 yank）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct InstallSkillRequest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

/// レジストリ一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillRegistryResponse {
    pub entries: Vec<RegistryEntry>,
}

/// インストール済み一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillInstallationsResponse {
    pub installations: Vec<SkillInstallation>,
}

/// AppState からサービスを取り出す。
fn service(state: &AppState) -> &app_platform::SkillInstallService {
    &state.skill_installs
}

/// skill をレジストリへ publish する（artifact owner のみ・不変・同一 name+version は 409）。
#[utoipa::path(
    post, path = "/skills/{id}/publish", request_body = PublishSkillRequest,
    params(("id" = Uuid, Path, description = "skill artifact ID")),
    responses(
        (status = 200, description = "publish 済みエントリ", body = RegistryEntry),
        (status = 403, description = "owner ではない"),
        (status = 409, description = "同一バージョンが publish 済み"),
    ),
    security(("session" = [])),
)]
pub async fn publish_skill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PublishSkillRequest>,
) -> Result<Json<RegistryEntry>, ApiError> {
    let signature = match &req.signature_base64 {
        Some(b64) => Some(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| ApiError::BadRequest("signature_base64 が不正です".into()))?,
        ),
        None => None,
    };
    let entry = service(&state)
        .publish(
            &ctx,
            id,
            req.version.as_deref(),
            req.trust_tier.as_deref().unwrap_or("in_house"),
            signature.as_deref(),
            trace.as_deref(),
        )
        .await?;
    Ok(Json(entry))
}

/// テナントの skill レジストリ一覧（インストール UI 用）。
#[utoipa::path(
    get, path = "/skills/registry",
    responses((status = 200, description = "エントリ一覧", body = SkillRegistryResponse)),
    security(("session" = [])),
)]
pub async fn list_skill_registry(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<SkillRegistryResponse>, ApiError> {
    let entries = service(&state).registry().list(&ctx, "skill", 200).await?;
    Ok(Json(SkillRegistryResponse { entries }))
}

/// skill を本人のカタログへインストールする（ユーザー単位・同意＝明示行為）。
#[utoipa::path(
    post, path = "/skills/installations", request_body = InstallSkillRequest,
    responses(
        (status = 200, description = "インストール結果", body = SkillInstallation),
        (status = 403, description = "署名不一致 / 読取権限なし（fail-closed）"),
        (status = 404, description = "レジストリに存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn install_skill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<InstallSkillRequest>,
) -> Result<Json<SkillInstallation>, ApiError> {
    let row = service(&state)
        .install(&ctx, &req.name, req.version.as_deref(), trace.as_deref())
        .await?;
    Ok(Json(row))
}

/// 本人のインストール済み skill 一覧。
#[utoipa::path(
    get, path = "/skills/installations",
    responses((status = 200, description = "一覧", body = SkillInstallationsResponse)),
    security(("session" = [])),
)]
pub async fn list_skill_installations(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
) -> Result<Json<SkillInstallationsResponse>, ApiError> {
    let installations = service(&state).list_installed(&ctx).await?;
    Ok(Json(SkillInstallationsResponse { installations }))
}

/// インストールを解除する（本人のカタログから外す）。
#[utoipa::path(
    delete, path = "/skills/installations/{name}",
    params(("name" = String, Path, description = "skill 名")),
    responses(
        (status = 204, description = "解除した"),
        (status = 404, description = "インストールされていない"),
    ),
    security(("session" = [])),
)]
pub async fn uninstall_skill(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    service(&state)
        .uninstall(&ctx, &name, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
