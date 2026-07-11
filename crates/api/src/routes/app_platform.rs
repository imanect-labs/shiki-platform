//! ミニアプリ／業務アプリ API — マニフェスト＋レジストリ publish（Task 9.1 / 9.13a）。
//!
//! コードベース・ミニアプリ（mini_app_code）のマニフェスト CRUD と、レジストリへの不変
//! publish。A（宣言的 mini_app）と同一の artifact 共通枠・ReBAC・監査経路に乗る。

use app_platform::{MiniAppManifest, RegistryEntry};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// マニフェスト作成/更新リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveManifestRequest {
    pub manifest: MiniAppManifest,
    /// 更新時の楽観ロック（作成時は無視）。
    #[serde(default)]
    pub expected_version: Option<i64>,
}

/// マニフェストのメタ＋本文。
#[derive(Debug, Serialize, ToSchema)]
pub struct ManifestResponse {
    pub id: Uuid,
    pub version: i64,
    pub manifest: MiniAppManifest,
}

/// publish リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishRequest {
    /// 対象マニフェストの artifact バージョン（省略時は最新）。
    #[serde(default)]
    pub artifact_version: Option<i64>,
}

/// バージョン指定クエリ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ManifestVersionQuery {
    pub version: Option<i64>,
}

/// マニフェストを作成する（語彙照合検証・201）。
#[utoipa::path(
    post,
    path = "/apps/manifests",
    request_body = SaveManifestRequest,
    responses(
        (status = 201, description = "作成した", body = ManifestResponse),
        (status = 400, description = "マニフェストが不正（未知スコープ/ツール・スキーマ不正）"),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名マニフェストが既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_manifest(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<SaveManifestRequest>,
) -> Result<(StatusCode, Json<ManifestResponse>), ApiError> {
    let id = state
        .mini_app_code
        .create(&ctx, &req.manifest, trace.as_deref())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ManifestResponse {
            id,
            version: 1,
            manifest: req.manifest,
        }),
    ))
}

/// マニフェストに新バージョンを追記する（editor・不変追記）。
#[utoipa::path(
    put,
    path = "/apps/manifests/{id}",
    params(("id" = Uuid, Path, description = "マニフェスト（artifact）ID")),
    request_body = SaveManifestRequest,
    responses(
        (status = 200, description = "追記後のマニフェスト", body = ManifestResponse),
        (status = 400, description = "マニフェストが不正"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "expected_version 不一致"),
    ),
    security(("session" = [])),
)]
pub async fn update_manifest(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SaveManifestRequest>,
) -> Result<Json<ManifestResponse>, ApiError> {
    let version = state
        .mini_app_code
        .update(
            &ctx,
            id,
            &req.manifest,
            req.expected_version,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(ManifestResponse {
        id,
        version,
        manifest: req.manifest,
    }))
}

/// マニフェストを取得する（viewer・バージョン指定可）。
#[utoipa::path(
    get,
    path = "/apps/manifests/{id}",
    params(
        ("id" = Uuid, Path, description = "マニフェスト（artifact）ID"),
        ManifestVersionQuery,
    ),
    responses(
        (status = 200, description = "マニフェスト", body = ManifestResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_manifest(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<ManifestVersionQuery>,
) -> Result<Json<ManifestResponse>, ApiError> {
    let (version, manifest) = state
        .mini_app_code
        .get(&ctx, id, q.version, trace.as_deref())
        .await?;
    Ok(Json(ManifestResponse {
        id,
        version,
        manifest,
    }))
}

/// マニフェストをレジストリへ不変 publish する（owner・同名+version は 409）。
#[utoipa::path(
    post,
    path = "/apps/manifests/{id}/publish",
    params(("id" = Uuid, Path, description = "マニフェスト（artifact）ID")),
    request_body = PublishRequest,
    responses(
        (status = 201, description = "publish した", body = RegistryEntry),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "同名+version が既に publish 済み（不変）"),
    ),
    security(("session" = [])),
)]
pub async fn publish_manifest(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PublishRequest>,
) -> Result<(StatusCode, Json<RegistryEntry>), ApiError> {
    let entry = state
        .mini_app_code
        .publish(&ctx, id, req.artifact_version, None, trace.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

/// バンドル upload 応答（content address）。
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleUploadResponse {
    /// sha256 hex（マニフェスト frontend.bundle_key / sha256 に設定する値）。
    pub sha256: String,
}

/// B1 フロントバンドルをアップロードする（owner・単一 self-contained HTML・content-addressed）。
#[utoipa::path(
    post,
    path = "/apps/manifests/{id}/bundle",
    params(("id" = Uuid, Path, description = "マニフェスト（artifact）ID")),
    request_body(content = String, content_type = "text/html"),
    responses(
        (status = 200, description = "保存済み（sha256 を返す）", body = BundleUploadResponse),
        (status = 400, description = "空/サイズ超過"),
        (status = 403, description = "owner でない"),
    ),
    security(("session" = [])),
)]
pub async fn upload_bundle(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path(id): Path<Uuid>,
    body: axum::body::Bytes,
) -> Result<Json<BundleUploadResponse>, ApiError> {
    let sha256 = state.bundles.put(&ctx, id, &body).await?;
    Ok(Json(BundleUploadResponse { sha256 }))
}

/// ミニアプリ／レジストリ（Task 9.1/9.13a）のルート宣言。
pub(crate) fn app_platform_route_decls() -> Vec<crate::server::RouteDecl> {
    use crate::server::AccessPolicy::Session;
    use axum::routing::{get, post};
    let r = crate::server::RouteDecl::new;
    vec![
        r("/apps/manifests", &["POST"], Session, || {
            post(create_manifest)
        }),
        r("/apps/manifests/{id}", &["GET", "PUT"], Session, || {
            get(get_manifest).put(update_manifest)
        }),
        r("/apps/manifests/{id}/publish", &["POST"], Session, || {
            post(publish_manifest)
        }),
        r("/apps/manifests/{id}/bundle", &["POST"], Session, || {
            // 単一 HTML バンドル（≤5MiB）。axum 既定 2MB を上限まで引き上げる。
            post(upload_bundle).layer(axum::extract::DefaultBodyLimit::max(
                app_platform::MAX_BUNDLE_BYTES + 1024,
            ))
        }),
    ]
}
