//! 共有リンク API（#342）。
//!
//! 1 ノードに複数の共有リンクを発行でき、各リンクは範囲(audience)・権限・有効期限・パスワードを
//! 持ち、個別に失効/延長できる。認可の正本は OpenFGA タプルで、発行/失効/延長/一覧は owner ゲート
//! （confused-deputy 防御）を通す。redeem（パスワード解錠）は **authenticated であれば誰でも**呼べる
//! （失敗は一律 403＝オラクル防止）。
//!
//! 語彙（`GeneralAccessLevel`＝audience／`ShareRole`／`ShareLink`）は storage 側の単一定義を使う
//! （手書きミラーを作らない・codegen が正）。匿名・テナント跨ぎの公開リンク（#341）は本 PR 対象外。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use storage::{GeneralAccessLevel, ShareLink, ShareRole};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 共有リンクの発行リクエスト。`audience = restricted` は付与ゼロの純ポインタ（既存アクセス者向け）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateShareLinkRequest {
    pub audience: GeneralAccessLevel,
    pub role: ShareRole,
    /// 有効期限（NULL/未指定 = 無期限）。
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// パスワード（未指定/空 = 無し）。設定すると解錠（redeem）を要求する。
    #[serde(default)]
    pub password: Option<String>,
    /// 任意のリンク名（UX 用）。
    #[serde(default)]
    pub label: Option<String>,
}

/// 共有リンクの延長/期限変更リクエスト（`expires_at = null` で無期限化）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ExtendShareLinkRequest {
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// パスワード解錠（redeem）リクエスト。token でリンクを一意特定する。
#[derive(Debug, Deserialize, ToSchema)]
pub struct RedeemShareLinkRequest {
    pub token: String,
    #[serde(default)]
    pub password: Option<String>,
}

/// 共有リンクを発行する（owner 権限）。発行結果（token 含む）を返す。
#[utoipa::path(
    post,
    path = "/nodes/{id}/share-links",
    params(("id" = Uuid, Path, description = "ノード ID")),
    request_body = CreateShareLinkRequest,
    responses(
        (status = 200, description = "発行した共有リンク", body = ShareLink),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn create_share_link(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateShareLinkRequest>,
) -> Result<Json<ShareLink>, ApiError> {
    let link = state
        .storage
        .create_share_link(
            &ctx,
            id,
            req.audience,
            req.role,
            req.expires_at,
            req.password.as_deref(),
            req.label.as_deref(),
            trace.as_deref(),
        )
        .await?;
    Ok(Json(link))
}

/// ノードの active な共有リンク一覧を取得する（owner 権限）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/share-links",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "共有リンク一覧", body = [ShareLink]),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn list_share_links(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ShareLink>>, ApiError> {
    let links = state
        .storage
        .list_share_links(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(links))
}

/// 共有リンクを失効する（owner 権限）。
#[utoipa::path(
    delete,
    path = "/share-links/{link_id}",
    params(("link_id" = Uuid, Path, description = "共有リンク ID")),
    responses(
        (status = 204, description = "失効した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない/リンクが無い）"),
    ),
    security(("session" = [])),
)]
pub async fn revoke_share_link(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(link_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .revoke_share_link(&ctx, link_id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 共有リンクの有効期限を延長/変更する（owner 権限）。
#[utoipa::path(
    patch,
    path = "/share-links/{link_id}",
    params(("link_id" = Uuid, Path, description = "共有リンク ID")),
    request_body = ExtendShareLinkRequest,
    responses(
        (status = 204, description = "更新した"),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない/リンクが無い）"),
    ),
    security(("session" = [])),
)]
pub async fn extend_share_link(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(link_id): Path<Uuid>,
    Json(req): Json<ExtendShareLinkRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .extend_share_link(&ctx, link_id, req.expires_at, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// パスワード付き共有リンクを解錠する（authenticated なら誰でも・失敗は一律 403）。
#[utoipa::path(
    post,
    path = "/share-links/redeem",
    request_body = RedeemShareLinkRequest,
    responses(
        (status = 204, description = "解錠して権限を得た"),
        (status = 401, description = "未認証"),
        (status = 403, description = "解錠できない（パスワード不一致/期限切れ/対象外など・理由は秘匿）"),
    ),
    security(("session" = [])),
)]
pub async fn redeem_share_link(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<RedeemShareLinkRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .redeem_share_link(&ctx, &req.token, req.password.as_deref(), trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
