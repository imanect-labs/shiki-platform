//! アーティファクト API（Task 6.1）。
//!
//! バージョン付き共有本文の共通枠。作成・不変バージョン追記・取得・履歴・共有/解除・削除。
//! 権限・監査は `ArtifactStore`（単一チョークポイント）が担い、ハンドラは薄い変換のみ。

use artifact::{Artifact, ArtifactKind, ArtifactRole, ArtifactVersion, NewArtifact, VersionMeta};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use storage::ShareTarget;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 作成リクエスト（body が version 1 になる）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateArtifactRequest {
    pub kind: ArtifactKind,
    pub name: String,
    pub body: serde_json::Value,
}

/// バージョン追記リクエスト。`expected_version` は楽観ロック（不一致は 409）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct AppendVersionRequest {
    pub body: serde_json::Value,
    pub expected_version: Option<i64>,
}

/// 共有/解除リクエスト（共有語彙は viewer/editor のみ）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ShareArtifactRequest {
    pub target: ShareTarget,
    pub role: ArtifactRole,
}

/// 共有相手 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtifactShareEntry {
    pub target: ShareTarget,
    pub role: ArtifactRole,
}

/// 一覧クエリ（kind 絞り込み・keyset ページング）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ListArtifactsQuery {
    pub kind: Option<ArtifactKind>,
    /// 前ページ末尾の updated_at（RFC3339）。
    pub before_updated_at: Option<DateTime<Utc>>,
    /// 前ページ末尾の id（updated_at と対で使う）。
    pub before_id: Option<Uuid>,
    pub limit: Option<i64>,
}

/// 一覧レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct ArtifactListResponse {
    pub items: Vec<Artifact>,
}

/// バージョン履歴レスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct VersionListResponse {
    pub items: Vec<VersionMeta>,
}

/// 汎用 /artifacts の書込を許す kind か検査する（Phase 6 ゲート）。
///
/// 専用の保存時検証を持つ kind（workflow=V1〜V7 / ui_spec=カタログ検証 / skill・mini_app=body
/// 検証）は、汎用 API 経由の**無検証保存バイパス**を塞ぐため 400 で拒否し専用エンドポイントへ
/// 誘導する（skill/mini_app の専用エンドポイントは Phase 6 後続 PR）。
fn require_generic_kind(kind: ArtifactKind) -> Result<(), ApiError> {
    match kind {
        ArtifactKind::Workflow => Err(ApiError::BadRequest(
            "kind=workflow は /workflows（保存時検証つき）で作成・更新してください".into(),
        )),
        ArtifactKind::UiSpec => Err(ApiError::BadRequest(
            "kind=ui_spec は /ui-specs（保存時検証つき）で作成・更新してください".into(),
        )),
        ArtifactKind::Skill | ArtifactKind::MiniApp => Err(ApiError::BadRequest(format!(
            "kind={} は専用エンドポイント（保存時検証つき）で作成・更新してください",
            kind.as_str()
        ))),
        ArtifactKind::Script => Ok(()),
    }
}

/// アーティファクトを作成する（version 1 込み・201）。
#[utoipa::path(
    post,
    path = "/artifacts",
    request_body = CreateArtifactRequest,
    responses(
        (status = 201, description = "作成した", body = Artifact),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名アーティファクトが既に存在する"),
    ),
    security(("session" = [])),
)]
pub async fn create_artifact(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateArtifactRequest>,
) -> Result<(StatusCode, Json<Artifact>), ApiError> {
    require_generic_kind(req.kind)?;
    let created = state
        .artifacts
        .create(
            &ctx,
            NewArtifact {
                kind: req.kind,
                name: req.name,
                body: req.body,
            },
            trace.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 自分が使えるアーティファクト一覧（所有＋共有された・kind 絞り込み・更新日降順）。
///
/// 共有された分（ReBAC viewer の実効集合）が無いと、共有相手が skill/ミニアプリを
/// UI から見つけて実行できない（Task 6.11「共有相手の実行」）。keyset カーソルは
/// 所有分にのみ適用し、共有分は初回ページに合流する（共有集合は小さい前提）。
#[utoipa::path(
    get,
    path = "/artifacts",
    params(ListArtifactsQuery),
    responses(
        (status = 200, description = "一覧", body = ArtifactListResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn list_artifacts(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<ListArtifactsQuery>,
) -> Result<Json<ArtifactListResponse>, ApiError> {
    let before = match (q.before_updated_at, q.before_id) {
        (Some(at), Some(id)) => Some((at, id)),
        _ => None,
    };
    let limit = q.limit.unwrap_or(50);
    let mut items = state
        .artifacts
        .list_mine(&ctx, q.kind, before, limit)
        .await?;
    // 2 ページ目以降（カーソルあり）は所有分の続きだけを返す（共有分は初回に合流済み）。
    if before.is_none() {
        let shared = state
            .artifacts
            .list_shared_with_me(&ctx, q.kind, limit)
            .await?;
        items.extend(shared);
        items.sort_by_key(|a| std::cmp::Reverse((a.updated_at, a.id)));
    }
    Ok(Json(ArtifactListResponse { items }))
}

/// メタデータを取得する（viewer）。
#[utoipa::path(
    get,
    path = "/artifacts/{id}",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    responses(
        (status = 200, description = "メタデータ", body = Artifact),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_artifact(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Artifact>, ApiError> {
    Ok(Json(state.artifacts.get(&ctx, id, trace.as_deref()).await?))
}

/// 論理削除する（owner・バージョン履歴は保持）。
#[utoipa::path(
    delete,
    path = "/artifacts/{id}",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn delete_artifact(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.artifacts.delete(&ctx, id, trace.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 新バージョンを追記する（editor・不変追記・楽観ロック）。
#[utoipa::path(
    post,
    path = "/artifacts/{id}/versions",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    request_body = AppendVersionRequest,
    responses(
        (status = 201, description = "追記した", body = ArtifactVersion),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "expected_version 不一致"),
    ),
    security(("session" = [])),
)]
pub async fn append_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<AppendVersionRequest>,
) -> Result<(StatusCode, Json<ArtifactVersion>), ApiError> {
    // 追記対象の kind を確認してからゲートする（viewer 検査込みの get）。
    let meta = state.artifacts.get(&ctx, id, trace.as_deref()).await?;
    require_generic_kind(meta.kind)?;
    let v = state
        .artifacts
        .append_version(&ctx, id, req.body, req.expected_version, trace.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(v)))
}

/// バージョン履歴（メタのみ・新しい順）。
#[utoipa::path(
    get,
    path = "/artifacts/{id}/versions",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    responses(
        (status = 200, description = "履歴", body = VersionListResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_versions(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<VersionListResponse>, ApiError> {
    let items = state
        .artifacts
        .list_versions(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(VersionListResponse { items }))
}

/// 指定バージョンの本文を取得する（viewer・過去バージョンも不変で取得できる）。
#[utoipa::path(
    get,
    path = "/artifacts/{id}/versions/{version}",
    params(
        ("id" = Uuid, Path, description = "アーティファクト ID"),
        ("version" = i64, Path, description = "バージョン番号（1 始まり）"),
    ),
    responses(
        (status = 200, description = "バージョン本文", body = ArtifactVersion),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_version(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path((id, version)): Path<(Uuid, i64)>,
) -> Result<Json<ArtifactVersion>, ApiError> {
    Ok(Json(
        state
            .artifacts
            .get_version(&ctx, id, version, trace.as_deref())
            .await?,
    ))
}

/// 共有する（owner・viewer/editor・冪等）。
#[utoipa::path(
    put,
    path = "/artifacts/{id}/shares",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    request_body = ShareArtifactRequest,
    responses(
        (status = 204, description = "共有を付与した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn share_artifact(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareArtifactRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .artifacts
        .share(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有を解除する（owner・冪等・即時反映）。
#[utoipa::path(
    delete,
    path = "/artifacts/{id}/shares",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    request_body = ShareArtifactRequest,
    responses(
        (status = 204, description = "共有を解除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn unshare_artifact(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<ShareArtifactRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .artifacts
        .unshare(&ctx, id, &req.target, req.role, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有相手一覧（owner）。
#[utoipa::path(
    get,
    path = "/artifacts/{id}/shares",
    params(("id" = Uuid, Path, description = "アーティファクト ID")),
    responses(
        (status = 200, description = "共有相手一覧", body = [ArtifactShareEntry]),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_artifact_shares(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ArtifactShareEntry>>, ApiError> {
    let entries = state
        .artifacts
        .list_shares(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(
        entries
            .into_iter()
            .map(|(target, role)| ArtifactShareEntry { target, role })
            .collect(),
    ))
}
