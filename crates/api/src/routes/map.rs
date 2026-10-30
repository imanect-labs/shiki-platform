//! `GET /map/config` — 地図の背景タイルの設定（回答の ```ui の map を描く web 向け）。
//!
//! 背景タイルはブラウザが配信元から直接読む（外部接続必須機能・requirements NFR-2）。
//! 返すのはスタイル JSON の URL だけ。地図が無効な構成（エアギャップ・未設定）では `null` を返し、
//! web は静止画を描く。

use axum::{extract::State, routing::get, Json};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{
    error::ApiError,
    extract::AuthContextExt,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn map_route_decls() -> Vec<RouteDecl> {
    vec![RouteDecl::new(
        "/map/config",
        &["GET"],
        AccessPolicy::Session,
        || get(map_config),
    )]
}

/// `GET /map/config` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct MapConfigResponse {
    /// ライト用のスタイル JSON の URL（`null` なら地図の背景を出さない）。
    pub style_url_light: Option<String>,
    /// ダーク用のスタイル JSON の URL。
    pub style_url_dark: Option<String>,
}

/// 地図の背景タイルの設定。
#[utoipa::path(
    get,
    path = "/map/config",
    responses(
        (status = 200, description = "地図の設定（無効ならスタイルの URL が null）", body = MapConfigResponse),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn map_config(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
) -> Result<Json<MapConfigResponse>, ApiError> {
    // 起動時に検証済み（AppConfig::validate）なので、ここで失敗するのは設定の取り違えだけ。
    let client = state
        .config
        .map
        .client()
        .map_err(|e| ApiError::Internal(format!("map config: {e}")))?;
    Ok(Json(MapConfigResponse {
        style_url_light: client.as_ref().map(|c| c.style_url_light.clone()),
        style_url_dark: client.map(|c| c.style_url_dark),
    }))
}
