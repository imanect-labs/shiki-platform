//! 一般アクセス API（共有リンクの公開範囲・#338）。
//!
//! Google Drive の「一般アクセス」に相当。owner がノードに `organization`（組織内）/
//! `anyone`（すべての認証済みユーザー）の公開範囲・有効期限・パスワードを設定できる。認可の
//! 正本は OpenFGA タプルで、set/clear は owner ゲート（confused-deputy 防御）を通す。redeem は
//! パスワード解錠で、**authenticated であれば誰でも**呼べる（失敗は一律 403＝オラクル防止）。
//!
//! 語彙（`GeneralAccessLevel`/`ShareRole`/`GeneralAccess`）は storage 側の単一定義を使う
//! （手書きミラーを作らない・codegen が正）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use storage::{GeneralAccess, GeneralAccessLevel, ShareRole};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// 一般アクセスの設定リクエスト。`level = restricted` は解除（clear）と同義。
/// `expires_at`/`password` は organization/anyone にのみ意味を持つ（restricted では無視）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetGeneralAccessRequest {
    pub level: GeneralAccessLevel,
    pub role: ShareRole,
    /// 有効期限（NULL/未指定 = 無期限）。
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// パスワード（未指定/空 = 無し）。設定すると解錠（redeem）を要求する。
    #[serde(default)]
    pub password: Option<String>,
    /// 既存パスワードを引き継ぐ（level/期限だけ変更する編集で再入力を強いない）。
    /// `password` が指定されていればそちらが優先。
    #[serde(default)]
    pub keep_password: bool,
}

/// パスワード解錠（redeem）リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct RedeemGeneralAccessRequest {
    pub password: String,
}

/// 一般アクセスの現在設定を取得する（owner 権限）。
#[utoipa::path(
    get,
    path = "/nodes/{id}/general-access",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 200, description = "一般アクセス設定", body = GeneralAccess),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn get_general_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<GeneralAccess>, ApiError> {
    let ga = state
        .storage
        .get_general_access(&ctx, id, trace.as_deref())
        .await?;
    Ok(Json(ga))
}

/// 一般アクセスを設定する（owner 権限）。`level = restricted` は解除。
#[utoipa::path(
    put,
    path = "/nodes/{id}/general-access",
    params(("id" = Uuid, Path, description = "ノード ID")),
    request_body = SetGeneralAccessRequest,
    responses(
        (status = 204, description = "設定した"),
        (status = 400, description = "不正なリクエスト"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn set_general_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<SetGeneralAccessRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .set_general_access(
            &ctx,
            id,
            req.level,
            req.role,
            req.expires_at,
            req.password.as_deref(),
            req.keep_password,
            trace.as_deref(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 一般アクセスを解除して restricted へ戻す（owner 権限）。
#[utoipa::path(
    delete,
    path = "/nodes/{id}/general-access",
    params(("id" = Uuid, Path, description = "ノード ID")),
    responses(
        (status = 204, description = "解除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "認可されていない（owner でない）"),
        (status = 404, description = "ノードが無い"),
    ),
    security(("session" = [])),
)]
pub async fn clear_general_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .clear_general_access(&ctx, id, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// パスワード付き一般アクセスを解錠する（authenticated なら誰でも・失敗は一律 403）。
#[utoipa::path(
    post,
    path = "/nodes/{id}/general-access/redeem",
    params(("id" = Uuid, Path, description = "ノード ID")),
    request_body = RedeemGeneralAccessRequest,
    responses(
        (status = 204, description = "解錠して権限を得た"),
        (status = 401, description = "未認証"),
        (status = 403, description = "解錠できない（パスワード不一致/期限切れ/対象外など・理由は秘匿）"),
    ),
    security(("session" = [])),
)]
pub async fn redeem_general_access(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<RedeemGeneralAccessRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .storage
        .redeem_general_access(&ctx, id, &req.password, trace.as_deref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
